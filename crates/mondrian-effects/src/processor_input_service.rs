//! Invocation-borrowed source planning/materialization, never factory state.
use super::*;
use crate::{
    CompiledEffectStageBinding, EffectFrameExtent, EffectTemporalExecutionError,
    EffectTemporalExecutionOutput, PreparedEffectTemporalExecution, PreparedTemporalFrameSet,
};

#[cfg(test)]
#[path = "processor_input_service_tests.rs"]
mod tests;

/// One exact input demand in Clip author time and actual image coordinates.
/// Generation, continuity and cancellation still come from the bound scheduler.
#[derive(Debug, Clone, Copy)]
pub struct EffectProcessorInputRequest {
    /// Exact Clip-local author time, already adapted from the external format.
    pub time: mondrian_core::TimelineTime,
    /// Current complete input raster, independent of project canvas geometry.
    /// The source scheduler owns the actual shape at the requested time.
    pub frame_extent: EffectFrameExtent,
    /// Actual incoming value representation; a service must refuse unsupported
    /// associations rather than silently normalize or clamp native Float alpha.
    pub format: crate::EffectValueFormat,
    /// Actual source sample shape and X/Y density.
    pub sampling: mondrian_core::ImageSampling,
    /// This subrequest can only shorten the owning attempt's deadline.
    pub deadline: std::time::Instant,
}

/// One exact scheduler-prepared input image; its origin may differ from the
/// current image. Source backing must be admitted before constructing frames.
pub struct PreparedEffectProcessorInput {
    /// Existing bounded program for the proven entering value.
    pub execution: PreparedEffectTemporalExecution,
    /// Frozen source values, resolved by the original source scheduler.
    pub frames: PreparedTemporalFrameSet,
    /// Image pixel origin, including negative source RoD coordinates.
    pub origin: [i32; 2],
}

/// Borrowed by one execution Control. The immutable factory never owns this.
/// Preparation uses the original source scheduler and must obtain `admit` for
/// the complete simultaneous source backing before allocation/materialization.
/// It returns frozen frames only: upstream Effect DSP runs after this mutable
/// borrow is restored, so nested processors can use this same service safely.
pub trait EffectProcessorInputService {
    /// Select only the supplied compiler-proven Definition input; do not infer
    /// a prefix from EffectId, dense stage ordinal or the current pixel buffer.
    fn prepare_input(
        &mut self,
        stage: &CompiledEffectStageBinding,
        request: EffectProcessorInputRequest,
        admit: &mut dyn FnMut(usize) -> std::result::Result<(), EffectTemporalExecutionError>,
        control: &mut EffectProcessorControl<'_>,
    ) -> std::result::Result<PreparedEffectProcessorInput, EffectTemporalExecutionError>;
}

fn unavailable(reason: &str) -> EffectTemporalExecutionError {
    crate::EffectExecutionError::InputUnavailable { reason: reason.to_owned() }.into()
}

impl<'a> EffectProcessorControl<'a> {
    /// Borrow the original task's source service for this execution only.
    /// Unknown dynamic dependencies bypass scalar pixel output caching. The
    /// consumer must also forbid outer frame reuse without complete source
    /// dependency identity (the installed OFX adapter is already Uncacheable).
    pub fn with_input_service(mut self, service: &'a mut dyn EffectProcessorInputService) -> Self {
        self.input_service = Some(service);
        self
    }

    /// Only the executor's current direct-input node may request its Source.
    pub fn resolves_processor_input(&self) -> bool {
        self.input_service.is_some()
            && self.input_stage.is_some()
            && self.current_occurrence.is_some()
    }

    pub(crate) fn uses_input_service(&self) -> bool {
        self.input_service.is_some()
    }

    pub(crate) fn input_stage_for(
        &self,
        graph: &crate::CompiledEffectGraph,
        node: crate::EffectGraphNodeId,
    ) -> Option<CompiledEffectStageBinding> {
        self.input_service.as_ref()?;
        graph.processor_input_stage(node).cloned()
    }

    pub(crate) fn with_input_stage<T>(
        &mut self,
        stage: Option<CompiledEffectStageBinding>,
        run: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if self.input_service.is_none() && self.input_stage.is_none() {
            return run(self);
        }
        let previous = std::mem::replace(&mut self.input_stage, stage);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(self)));
        self.input_stage = previous;
        match result {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn with_input_deadline<T>(
        &mut self,
        deadline: std::time::Instant,
        run: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = self.observation.deadline;
        self.observation.deadline = Some(previous.map_or(deadline, |owner| owner.min(deadline)));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(self)));
        self.observation.deadline = previous;
        match result {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn prepare_processor_input(
        &mut self,
        request: EffectProcessorInputRequest,
        admit: &mut dyn FnMut(usize) -> std::result::Result<(), EffectTemporalExecutionError>,
    ) -> std::result::Result<PreparedEffectProcessorInput, EffectTemporalExecutionError> {
        input_checkpoint(self)?;
        if self.current_occurrence.is_none() {
            return Err(unavailable("input request has no active bank occurrence"));
        }
        let stage = self
            .input_stage
            .clone()
            .ok_or_else(|| unavailable("node has no direct Definition input"))?;
        let service = self
            .input_service
            .take()
            .ok_or_else(|| unavailable("no invocation input service is bound"))?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            service.prepare_input(&stage, request, admit, self)
        }));
        self.input_service = Some(service);
        // Host source callback failures belong to the input, not the waiting
        // provider. Always restore the borrow, including panic and owner stop.
        input_checkpoint(self)?;
        match result {
            Ok(result) => result,
            Err(_) => Err(unavailable("input source service panicked")),
        }
    }
}

fn input_checkpoint(
    control: &mut EffectProcessorControl<'_>,
) -> std::result::Result<(), EffectTemporalExecutionError> {
    control.check().map_err(|_| {
        crate::EffectExecutionError::CustomProcessorStopped {
            reason: control.stop_reason().unwrap_or(EffectProcessorStopReason::Canceled),
        }
        .into()
    })
}

impl EffectProcessorResources<'_> {
    /// Prepare through the original task's borrowed source service, then run
    /// upstream on this invocation's bank/Control. `held_bytes` is the complete
    /// still-live extra parent/native backing, not an increment.
    pub fn resolve_input_f32(
        &mut self,
        request: EffectProcessorInputRequest,
        held_bytes: usize,
        control: &mut EffectProcessorControl<'_>,
    ) -> std::result::Result<(EffectTemporalExecutionOutput, [i32; 2]), EffectTemporalExecutionError>
    {
        control.with_input_deadline(request.deadline, |control| {
            self.resolve_input_scoped(request, held_bytes, control)
        })
    }

    fn resolve_input_scoped(
        &mut self,
        request: EffectProcessorInputRequest,
        held_bytes: usize,
        control: &mut EffectProcessorControl<'_>,
    ) -> std::result::Result<(EffectTemporalExecutionOutput, [i32; 2]), EffectTemporalExecutionError>
    {
        input_checkpoint(control)?;
        self.acquire_transient(held_bytes).map_err(|e| {
            EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                required_bytes: e.required_bytes,
                budget_bytes: e.available_bytes,
            }
        })?;
        let mut admitted = 0;
        let mut admit = |bytes: usize| {
            let required_bytes = held_bytes
                .checked_add(bytes)
                .ok_or(EffectTemporalExecutionError::SourceCoverageSizeOverflow)?;
            self.acquire_transient(required_bytes).map_err(|e| {
                EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                    required_bytes: e.required_bytes,
                    budget_bytes: e.available_bytes,
                }
            })?;
            admitted = admitted.max(bytes);
            Ok(())
        };
        let PreparedEffectProcessorInput { execution, mut frames, origin } =
            control.prepare_processor_input(request, &mut admit)?;
        if frames.retained_coverage_bytes() > admitted {
            return Err(unavailable(
                "source service returned backing without prior admission",
            ));
        }
        // Source geometry may vary with time. The returned admitted image owns
        // its actual extent/sampling; it cannot exchange the requested instant.
        let prepared = execution.request();
        if prepared.output_time() != request.time {
            return Err(unavailable("input service returned a different request"));
        }
        let extent = execution.request().frame_extent();
        if i32::try_from(extent.width())
            .ok()
            .and_then(|w| origin[0].checked_add(w))
            .is_none()
            || i32::try_from(extent.height())
                .ok()
                .and_then(|h| origin[1].checked_add(h))
                .is_none()
        {
            return Err(unavailable("source image bounds overflow"));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.execute_prepared_input_f32(&execution, &mut frames, held_bytes, control)
        }))
        .map_err(|_| unavailable("upstream input execution panicked"))?;
        result.map(|output| (output, origin))
    }
}
