//! Invocation-owned upstream image service. Graph/decoder work stays on the
//! original owner thread; foreign callbacks exchange bounded requests only.
use super::*;

#[cfg(test)]
#[path = "source_input_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "source_bank_tests.rs"]
mod bank_tests;

pub(super) fn delivery_error(reason: impl Into<String>) -> OpenFxHostError {
    OpenFxHostError::InputFailed(Box::new(
        mondrian_effects::EffectExecutionError::InputUnavailable { reason: reason.into() },
    ))
}

pub(super) fn classify_delivery_error(error: OpenFxHostError) -> OpenFxHostError {
    match error {
        OpenFxHostError::Canceled
        | OpenFxHostError::DeadlineExceeded
        | OpenFxHostError::InputFailed(_) => error,
        OpenFxHostError::Frame(error) => OpenFxHostError::InputFailed(Box::new(
            mondrian_effects::EffectExecutionError::FrameContract(error),
        )),
        other => delivery_error(other.to_string()),
    }
}

pub(super) fn graph_error(
    error: mondrian_effects::EffectTemporalExecutionError,
) -> OpenFxHostError {
    let error = match error {
        mondrian_effects::EffectTemporalExecutionError::CustomProcessor(error) => error,
        source => {
            mondrian_effects::EffectExecutionError::TemporalInput { source: Box::new(source) }
        }
    };
    OpenFxHostError::InputFailed(Box::new(error))
}

pub(super) fn check_input_owner(
    owner: &mut dyn RenderOwner,
    deadline: Instant,
) -> Result<(), OpenFxHostError> {
    owner.observe(OpenFxRenderObservation::Checkpoint)?;
    if Instant::now() >= deadline {
        return Err(OpenFxHostError::InputFailed(Box::new(
            mondrian_effects::EffectExecutionError::CustomProcessorStopped {
                reason: mondrian_effects::EffectProcessorStopReason::DeadlineExpired,
            },
        )));
    }
    Ok(())
}

/// Complete immutable upstream value, already evaluated by the shared graph.
pub(crate) struct ResolvedInput {
    pub frame: EffectFloatFrame,
    pub x: i32,
    pub y: i32,
}

/// Adapter-bound OFX time plus already-held native backing. The graph owner
/// deducts this backing from its original grant before evaluating upstream.
pub(crate) struct InputRequest {
    pub time: f64,
    pub held_native_bytes: usize,
    pub deadline: Instant,
}

/// One mutable owner for cancellation, resource grants and upstream evaluation.
/// Sharing this borrow prevents separate callbacks from aliasing the same bank.
pub(crate) trait RenderOwner {
    fn observe(
        &mut self,
        event: OpenFxRenderObservation<'_>,
    ) -> Result<mondrian_effects::EffectProcessorQuestionState, OpenFxHostError>;
    fn acquire_memory(&mut self, bytes: usize) -> Result<bool, OpenFxHostError>;
    fn resolves_input(&self) -> bool {
        false
    }
    fn resolve_input(&mut self, _request: InputRequest) -> Result<ResolvedInput, OpenFxHostError> {
        Err(unavailable(
            "no upstream image resolver is bound to this invocation",
        ))
    }
}

pub(super) struct ClosureOwner<'a> {
    pub observe: &'a mut dyn FnMut(
        OpenFxRenderObservation<'_>,
    ) -> Result<
        mondrian_effects::EffectProcessorQuestionState,
        OpenFxHostError,
    >,
    pub memory: &'a mut dyn FnMut(usize) -> Result<bool, OpenFxHostError>,
}
impl RenderOwner for ClosureOwner<'_> {
    fn observe(
        &mut self,
        event: OpenFxRenderObservation<'_>,
    ) -> Result<mondrian_effects::EffectProcessorQuestionState, OpenFxHostError> {
        (self.observe)(event)
    }
    fn acquire_memory(&mut self, bytes: usize) -> Result<bool, OpenFxHostError> {
        (self.memory)(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InputIdentity {
    pub service: u64,
    pub time: f64,
}

/// The cache stores owned maps and scalar metadata, never borrowed pointers.
/// All callbacks join before the enclosing native call and error teardown end.
pub(super) struct SourceArena<'a> {
    shared: &'a pump::Shared,
    prepare: &'a Prepare,
    timing: OpenFxRenderTiming,
    sampling: mondrian_core::ImageSampling,
    initial_count: usize,
    initial_bytes: usize,
    images: std::sync::Mutex<Vec<source_frames::MappedInput>>,
}

pub(crate) fn storage_bytes() -> usize {
    256 * (std::mem::size_of::<source_frames::MappedInput>()
        + std::mem::size_of::<tempfile::NamedTempFile>()
        + 4096)
        + 2 * source_frames::storage_bytes()
        + std::mem::size_of::<SourceArena<'static>>()
}

impl<'a> SourceArena<'a> {
    pub fn new(
        shared: &'a pump::Shared,
        prepare: &'a Prepare,
        timing: &OpenFxRenderTiming,
        sampling: mondrian_core::ImageSampling,
        initial: Option<&source_frames::Manifest>,
    ) -> Result<Self, OpenFxHostError> {
        if prepare.context != OpenFxContext::Filter {
            return Err(invalid(
                "only a connected Source can resolve upstream images",
            ));
        }
        let mut images = Vec::new();
        images.try_reserve_exact(256).map_err(|e| unavailable(e.to_string()))?;
        if images.capacity() > 256 {
            return Err(unavailable("source map capacity exceeds policy"));
        }
        Ok(Self {
            shared,
            prepare,
            timing: timing.clone(),
            sampling,
            initial_count: initial.map_or(0, source_frames::Manifest::count),
            initial_bytes: initial.map_or(0, source_frames::Manifest::bytes),
            images: std::sync::Mutex::new(images),
        })
    }

    pub fn provider(&self) -> NativeSourceProvider {
        fn require_sync<T: Sync>() {}
        require_sync::<Self>();
        NativeSourceProvider {
            context: std::ptr::from_ref(self).cast(),
            fetch: Some(fetch),
        }
    }

    fn fetch(&self, time: f64) -> Result<(NativeSourceFrame, usize), OpenFxHostError> {
        if self.shared.is_canceled() {
            return Err(OpenFxHostError::Canceled);
        }
        let mut images =
            self.images.lock().map_err(|_| unavailable("source cache gate poisoned"))?;
        if let Some((index, image)) =
            images.iter().enumerate().find(|(_, image)| image.time == time)
        {
            return Ok((image.view(), self.initial_count + index + 1));
        }
        // Serialize misses, including duplicate requests from concurrent plugin
        // threads. The pump and original owner remain free to cancel/service.
        let manifest = self.shared.request_input(time)?;
        if self.initial_count + images.len() >= 256
            || manifest.count() != 1
            || images
                .iter()
                .try_fold(self.initial_bytes, |n, image| n.checked_add(image.bytes()))
                .and_then(|n| n.checked_add(manifest.bytes()))
                .is_none_or(|n| n > MAX_FRAME_BYTES)
        {
            return Err(invalid("resolved source exceeds the admitted image set"));
        }
        let mapped = source_frames::MappedSources::open(
            Some(&manifest),
            self.prepare,
            &self.timing,
            self.sampling,
            &mut || {
                if self.shared.is_canceled() {
                    Err(OpenFxHostError::Canceled)
                } else {
                    Ok(())
                }
            },
        )?
        .into_single()?;
        if mapped.time != time {
            return Err(invalid("resolved source changed the requested exact time"));
        }
        images.push(mapped);
        let index = images.len() - 1;
        Ok((images[index].view(), self.initial_count + index + 1))
    }
}

unsafe extern "C" fn fetch(
    context: *const c_void,
    time: f64,
    frame: *mut NativeSourceFrame,
    index: *mut usize,
) -> c_int {
    if context.is_null() || frame.is_null() || index.is_null() || !time.is_finite() {
        return 1;
    }
    // SAFETY: the host lends this immutable, Sync arena until the native action,
    // all legal callbacks and native error destruction return. Returned pixels
    // remain owned by its maps; no graph/user closure runs on this thread.
    let arena = unsafe { &*context.cast::<SourceArena<'_>>() };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| arena.fetch(time))) {
        Ok(Ok((view, ordinal))) => {
            // SAFETY: C++ lends aligned writable POD slots for this callback.
            unsafe {
                frame.write(view);
                index.write(ordinal);
            }
            0
        }
        Ok(Err(error)) => {
            if !arena.shared.is_canceled() {
                arena.shared.fail(&error);
            }
            1
        }
        Err(payload) => {
            if payload.is::<String>() || payload.is::<&'static str>() {
                drop(payload);
            } else {
                std::mem::forget(payload);
            }
            arena.shared.fail(&unavailable("host source callback panicked"));
            1
        }
    }
}
