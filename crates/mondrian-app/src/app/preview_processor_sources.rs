//! Invocation-bound source requests to the original Preview media producer.
//!
//! A live CPU operation waits with its original Control. Runtime owns admission
//! and Frame Store leases; producer completion never restarts plugin DSP.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

use mondrian_effects::{
    EffectProcessorControl, EffectTemporalExecutionError, PreparedTemporalFrameSet,
};
use mondrian_renderer::{
    CpuColorFrame, PreparedTimelineClipInputPlan, TimelineTemporalDemandBatch,
    TimelineTemporalSource,
};

use super::preview_access_mode::{MediaPreviewKey, PreviewMediaWorkPolicy};
use super::preview_media_frame::MediaPreviewFrame;
use super::preview_processor_feedback::PreviewProcessorFeedbackScope;
use super::preview_timeline_execution::PreviewTimelineMediaFrame;
use super::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use super::preview_viewer_plan::{ResolvedPreviewElement, ResolvedPreviewTransitionInput};

const SOURCE_WAIT_POLL: Duration = Duration::from_millis(1);
const MAX_SOURCE_REQUEST_FRAMES: usize = 256;

#[derive(Debug, Clone)]
struct MediaBinding {
    plan: Arc<PreparedTimelineClipInputPlan>,
    key: MediaPreviewKey,
}

/// A job retains only the exact source keys admitted with its resolved inputs.
/// The existing media producer and Store remain responsible for pixels.
#[derive(Debug, Clone)]
pub(crate) struct PreviewProcessorSourceWork {
    policy: PreviewMediaWorkPolicy,
    bindings: Vec<MediaBinding>,
}

impl PreviewProcessorSourceWork {
    pub(crate) fn from_bindings(
        policy: PreviewMediaWorkPolicy,
        bindings: impl IntoIterator<Item = (Arc<PreparedTimelineClipInputPlan>, MediaPreviewKey)>,
    ) -> Self {
        let mut selected: Vec<MediaBinding> = Vec::new();
        for (plan, key) in bindings {
            if !selected.iter().any(|binding| binding.plan.matches_semantics(&plan)) {
                selected.push(MediaBinding { plan, key });
            }
        }
        Self { policy, bindings: selected }
    }

    pub(crate) fn from_elements(
        policy: PreviewMediaWorkPolicy,
        elements: &[ResolvedPreviewElement],
    ) -> Self {
        let mut bindings = Vec::new();
        let mut media =
            |frame: &MediaPreviewFrame,
             route: Option<&mondrian_renderer::PreparedTimelinePreviewEffectRoute>| {
                if let Some(plan) = route.and_then(|route| route.processor_input_plan())
                    && let Some(key) = frame.source_key()
                {
                    bindings.push(MediaBinding { plan: Arc::clone(plan), key: key.clone() });
                }
            };
        let mut transition = |input: &ResolvedPreviewTransitionInput| {
            if let ResolvedPreviewTransitionInput::Media { frame, prepared_effect_route, .. } =
                input
            {
                media(frame, prepared_effect_route.as_deref());
            }
        };
        // Bind exact resolved placements, retaining their already-admitted keys.
        for element in elements {
            if let ResolvedPreviewElement::CrossDissolve { left, right, .. } = element {
                transition(left);
                transition(right);
            }
        }
        drop(transition);
        for element in elements {
            if let ResolvedPreviewElement::Media { frame, prepared_effect_route, .. } = element {
                media(frame, prepared_effect_route.as_deref());
            }
        }
        Self { policy, bindings }
    }
}

type SourceReply = Result<Vec<MediaPreviewFrame>, PreviewUnavailability>;

/// Move-only reply capability for one bounded dependency batch. Partial Ready
/// inputs live only for this invocation and retain their original Store leases.
pub(crate) struct PreviewProcessorSourceRequest {
    scope: PreviewProcessorFeedbackScope,
    policy: PreviewMediaWorkPolicy,
    keys: Vec<MediaPreviewKey>,
    ready: Vec<Option<MediaPreviewFrame>>,
    reply: mpsc::SyncSender<SourceReply>,
    active: Arc<AtomicBool>,
    retry_admission: bool,
}

impl std::fmt::Debug for PreviewProcessorSourceRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewProcessorSourceRequest")
            .field("scope", &self.scope)
            .field("sources", &self.keys.len())
            .field("active", &self.is_active())
            .finish()
    }
}

impl PreviewProcessorSourceRequest {
    pub(crate) fn scope(&self) -> PreviewProcessorFeedbackScope {
        self.scope
    }
    pub(crate) fn needs_admission_retry(&self) -> bool {
        self.retry_admission
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
            && self.policy.deadline.is_none_or(|deadline| Instant::now() < deadline)
    }

    /// Poll through Runtime's ordinary admission path. Acquired frames are kept
    /// across pending/admission retries, and no completion authority is created.
    pub(crate) fn poll(
        &mut self,
        mut fetch: impl FnMut(&MediaPreviewKey, PreviewMediaWorkPolicy) -> PreviewTimelineMediaFrame,
    ) -> bool {
        if !self.is_active() {
            return true;
        }
        self.retry_admission = false;
        for (key, slot) in self.keys.iter().zip(&mut self.ready) {
            if slot.is_some() {
                continue;
            }
            match fetch(key, self.policy) {
                PreviewTimelineMediaFrame::Ready(frame) => {
                    if frame.presentation_quality()
                        != mondrian_playback::FramePresentationQuality::Ready
                    {
                        let _ = self.reply.try_send(Err(PreviewUnavailability::blocked(
                            PreviewOutputStage::MediaDecode,
                            "plugin source dependency requires an exact Ready media sample",
                        )));
                        return true;
                    }
                    *slot = Some(frame);
                }
                PreviewTimelineMediaFrame::Pending { wait } => {
                    self.retry_admission |= wait == super::preview_timeline_execution::PreviewTimelineMediaWait::RetryAdmission;
                }
                PreviewTimelineMediaFrame::Unavailable { reason } => {
                    let _ = self.reply.try_send(Err(reason));
                    return true;
                }
            }
        }
        if self.ready.iter().all(Option::is_some) {
            let frames = self
                .ready
                .iter_mut()
                .map(|slot| slot.take().expect("complete source batch"))
                .collect();
            let _ = self.reply.try_send(Ok(frames));
            true
        } else {
            false
        }
    }
}

struct SourceWaitOwner(Arc<AtomicBool>);
impl Drop for SourceWaitOwner {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) struct PreviewProcessorSourceClient<'a> {
    scope: PreviewProcessorFeedbackScope,
    work: &'a PreviewProcessorSourceWork,
    publish: &'a mut dyn FnMut(
        PreviewProcessorSourceRequest,
    )
        -> Result<(), mpsc::TrySendError<PreviewProcessorSourceRequest>>,
}

impl<'a> PreviewProcessorSourceClient<'a> {
    pub(crate) fn new(
        scope: PreviewProcessorFeedbackScope,
        work: &'a PreviewProcessorSourceWork,
        publish: &'a mut dyn FnMut(
            PreviewProcessorSourceRequest,
        )
            -> Result<(), mpsc::TrySendError<PreviewProcessorSourceRequest>>,
    ) -> Self {
        Self { scope, work, publish }
    }

    pub(crate) fn resolve(
        &mut self,
        plan: &PreparedTimelineClipInputPlan,
        batch: &TimelineTemporalDemandBatch,
        current: Option<&CpuColorFrame>,
        space: mondrian_core::WorkingColorSpace,
        admit: &mut dyn FnMut(usize) -> Result<(), EffectTemporalExecutionError>,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<PreparedTemporalFrameSet, EffectTemporalExecutionError> {
        let unavailable = |reason: String| -> EffectTemporalExecutionError {
            mondrian_effects::EffectExecutionError::InputUnavailable { reason }.into()
        };
        control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
        plan.validate_source_demands(batch)?;
        let current_source =
            plan.current_source().map_err(|error| unavailable(error.to_string()))?;
        let mut keys = Vec::new();
        for demand in batch.source_demands() {
            if matches!(demand.source, TimelineTemporalSource::SolidColor { .. })
                || (demand.source == current_source && current.is_some())
            {
                continue;
            }
            let TimelineTemporalSource::Media { asset_id, source_sample, .. } = demand.source
            else {
                return Err(unavailable(
                    "historical nested source is not yet admitted".into(),
                ));
            };
            let binding = self
                .work
                .bindings
                .iter()
                .find(|binding| binding.plan.matches_semantics(plan))
                .ok_or_else(|| {
                    unavailable("Clip has no original frozen media source key".into())
                })?;
            let mut raw_configuration = demand.source.clone();
            if let TimelineTemporalSource::Media { source_sample: at_current, .. } = &current_source
                && let TimelineTemporalSource::Media { source_sample, .. } = &mut raw_configuration
            {
                *source_sample = *at_current;
            }
            if raw_configuration != current_source {
                return Err(unavailable(
                    "historical raw-source interpretation differs from the original input".into(),
                ));
            }
            if binding.key.asset_id != asset_id {
                return Err(unavailable(
                    "source key belongs to another Clip asset".into(),
                ));
            }
            let mut key = binding.key.clone();
            key.decode = key
                .decode
                .at_source_sample(source_sample)
                .map_err(|error| unavailable(error.to_string()))?;
            if !keys.contains(&key) {
                if keys.len() == MAX_SOURCE_REQUEST_FRAMES {
                    return Err(unavailable(
                        "plugin source dependency batch exceeds the host frame limit".into(),
                    ));
                }
                keys.push(key);
            }
        }
        if keys.is_empty() {
            return plan.prepare_available_sources(batch, current, space, admit, control);
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let active = Arc::new(AtomicBool::new(true));
        let _owner = SourceWaitOwner(active.clone());
        let mut policy = self.work.policy;
        if let Some(deadline) = batch.execution_request().cancellation().deadline() {
            policy.deadline = Some(policy.deadline.map_or(deadline, |own| own.min(deadline)));
        }
        let mut request = PreviewProcessorSourceRequest {
            scope: self.scope,
            policy,
            ready: vec![None; keys.len()],
            keys: keys.clone(),
            reply,
            active,
            retry_admission: false,
        };
        loop {
            control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
            match (self.publish)(request) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(back)) => {
                    request = back;
                    std::thread::park_timeout(SOURCE_WAIT_POLL);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
                    return Err(unavailable("source request transport disconnected".into()));
                }
            }
        }
        let frames = loop {
            control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
            match receiver.recv_timeout(SOURCE_WAIT_POLL) {
                Ok(Ok(frames)) => break frames,
                Ok(Err(reason)) => return Err(unavailable(reason.detail().to_owned())),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
                    return Err(unavailable("source request owner retired".into()));
                }
            }
        };
        // The prepared source's existing OnceLock owns its already-budgeted
        // working allocation. This uncached color scope avoids reborrowing the
        // compositor's active color Session; plugin DSP stays in its one bank.
        let mut color = mondrian_renderer::RenderCpuColorExecutionSession::new(0);
        plan.prepare_sources(
            batch,
            space,
            &mut |demand, control| {
                control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
                if demand.source == current_source
                    && let Some(frame) = current
                {
                    return Ok(frame.clone());
                }
                let TimelineTemporalSource::Media { source_sample, .. } = demand.source else {
                    return Err(unavailable("unsupported source materialization".into()));
                };
                let index =
                    keys.iter().position(|key| key.source_sample() == source_sample).ok_or_else(
                        || unavailable("source reply does not cover the original demand".into()),
                    )?;
                let frame = frames[index]
                    .working_frame_with_session(&mut color)
                    .map_err(|error| unavailable(error.to_string()))?;
                control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
                Ok(frame.frame)
            },
            admit,
            control,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::preview_frame_store::{
        MediaWorkReservationAdmission, PreviewFrameStoreAdapter,
    };

    #[test]
    fn processor_source_partial_ready_retains_one_store_lease_and_releases_on_cancellation() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"source identity").unwrap();
        let path = file.path().canonicalize().unwrap();
        let key = MediaPreviewKey::test_cpu(
            path.clone(),
            mondrian_media::MediaFileFingerprint::capture(&path),
            mondrian_core::TimelineTime::ZERO,
            mondrian_core::Resolution { width: 1, height: 1 },
            mondrian_media::PreviewSourceColorContract::automatic(
                mondrian_core::ColorSpace::Rec709,
                mondrian_media::DecodedVideoRange::Full,
            ),
        );
        let mut next = key.clone();
        next.decode = key
            .decode
            .at_source_sample(mondrian_core::SourceSampleTarget::covering(
                mondrian_core::TimelineTime::new(1, 1).unwrap(),
            ))
            .unwrap();
        let frame = MediaPreviewFrame::from_working(
            CpuColorFrame::working(mondrian_core::WorkingRgbaF32Frame {
                width: 1,
                height: 1,
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                data: vec![[0.25, 0.5, 2.0, 1.0]],
                alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
                color_space: mondrian_core::WorkingColorSpace::LinearRec709,
            }),
            mondrian_core::Resolution { width: 1, height: 1 },
            crate::app::preview_execution::PreviewSemanticIdentityBuilder::new(
                b"source-partial-ready",
            )
            .finish_identity(),
            mondrian_playback::FramePresentationQuality::Ready,
            crate::app::preview_execution::PreviewDecodeExecutionSummary::from_path(
                mondrian_media::PreviewDecodeExecutionPath::SoftwareCpu,
            ),
        );
        let bytes = frame.reserved_cpu_bytes();
        let mut store = PreviewFrameStoreAdapter::default();
        let MediaWorkReservationAdmission::Reserved(lease) = store
            .reserve_media_work(
                &key,
                mondrian_playback::MediaWorkReservationIntent::Prefetch,
                bytes,
                0,
            )
            .unwrap()
        else {
            panic!("original physical Store reservation")
        };
        assert!(store.insert_media_frame(key.clone(), frame, lease).is_admitted());
        let epoch = mondrian_playback::PlaybackEngine::default().snapshot().epoch;
        let active = Arc::new(AtomicBool::new(true));
        let owner = SourceWaitOwner(active.clone());
        let (reply, receiver) = mpsc::sync_channel(1);
        let mut request = PreviewProcessorSourceRequest {
            scope: PreviewProcessorFeedbackScope { key: None, generation: 1, epoch },
            policy: PreviewMediaWorkPolicy {
                intent: super::super::preview_access_mode::MediaPreviewRequestIntent::Prefetch,
                access_mode: mondrian_media::PreviewDecodeAccessMode::RandomAccessStillFrame,
                deadline: None,
                demand_identity: None,
                direction: mondrian_media::PreviewPlaybackDirection::Forward,
            },
            keys: vec![key.clone(), next],
            ready: vec![None, None],
            reply,
            active,
            retry_admission: false,
        };
        let mut first_fetches = 0;
        assert!(!request.poll(|requested, _| {
                if requested == &key {
                    first_fetches += 1;
                    PreviewTimelineMediaFrame::Ready(store.media_frame(&key).unwrap())
                } else {
                    PreviewTimelineMediaFrame::Pending {
                        wait: super::super::preview_timeline_execution::PreviewTimelineMediaWait::Producer,
                    }
                }
            }));
        store.clear_media_frames();
        assert_eq!(store.diagnostics().media_aggregate_reserved_bytes, bytes);
        assert!(!request.needs_admission_retry());
        assert!(!request.poll(|requested, _| {
                assert_ne!(requested, &key, "Ready dependency must not be reacquired");
                PreviewTimelineMediaFrame::Pending {
                    wait: super::super::preview_timeline_execution::PreviewTimelineMediaWait::RetryAdmission,
                }
            }));
        assert!(request.needs_admission_retry());
        assert_eq!(first_fetches, 1);
        drop(owner);
        assert!(request.poll(|_, _| panic!("canceled request must not admit more work")));
        drop(request);
        assert!(receiver.try_recv().is_err());
        assert_eq!(store.diagnostics().media_aggregate_reserved_bytes, 0);
    }
}
