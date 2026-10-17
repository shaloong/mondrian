use super::{IsolatedAudioProcessorWorkerBlock, NativeRequestCohort};
use crate::AudioProcessorHostError;

/// Disjoint borrowed roles of the one retained native object.
///
/// The control role stays on the initializing main thread. The processing role
/// may move only into a scoped DSP thread. Both borrow the same worker, preventing
/// Enter/configuration/seed/termination from replacing it until the DSP joins.
#[must_use]
pub struct IsolatedAudioProcessorWorkerSplit<'a> {
    /// Native main/controller service; no PCM or component lifecycle capability.
    pub main: Box<dyn IsolatedAudioProcessorMainRole + 'a>,
    /// Native DSP storage; no editor/controller or component lifecycle capability.
    pub processing: Box<dyn IsolatedAudioProcessorProcessingRole + Send + 'a>,
}

/// Main-thread service borrowed from the current exclusive worker.
///
/// This interface is source observation and GUI service, never an author lease,
/// root-window admission, state installation or a second control dispatcher.
pub trait IsolatedAudioProcessorMainRole {
    /// Complete concrete adapter and SDK main-role Box layouts, if supported.
    /// These requests do not issue a Runtime lease or prove a whole-root grant.
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        None
    }
    /// Service a bounded batch of actual platform/controller work on main.
    fn service_native_main_thread(&mut self) -> Result<(), AudioProcessorHostError>;

    /// Finish current main/controller work after the DSP role has joined.
    /// Temporary role completion may preserve the same owner's attached view.
    /// Lifecycle replacement separately requires explicit view/parent removal
    /// by the full native owner, with isolated retirement on unresolved failure.
    fn finish_processing_scope(&mut self) -> Result<(), AudioProcessorHostError>;

    /// Read the same native callback cohort without consuming it or configuring DSP.
    fn native_configuration_cohort(
        &self,
    ) -> Result<Option<NativeRequestCohort>, AudioProcessorHostError>;

    /// Observe pending source data; sole Runtime chooses next root-window hold.
    fn pending_reprepare(
        &self,
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError>;

    /// Observe only source facts through the same native-main role.
    /// Preserve performs no controller/component state capture or author drain;
    /// it may continue a live DSP borrow. Drain stays on the joined full owner.
    fn preserve_native_observations(
        &self,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        Ok(crate::NativeAudioObservations::default())
    }

    /// Admit this DrainAuthor command's DSP job only for the current binding
    /// and actual object when the full owner will collect new author work.
    /// A retained delivery/awaiting acceptance returns to that owner unchanged.
    /// Independent adapter idle service is not gated by this command check.
    /// Public metadata alone cannot authorize a native callback.
    fn native_author_binding_matches(
        &self,
        _binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<bool, AudioProcessorHostError> {
        Ok(false)
    }

    /// Observe a close request on the same attached parent, without removing
    /// the view or replacing the native owner before its DSP role has joined.
    fn native_editor_close_requested(&self) -> Result<bool, AudioProcessorHostError> {
        Ok(false)
    }
}

/// DSP-only borrowed state of the same native object as the main role.
///
/// Implementations must delegate to the same prepared ABI processing algorithm
/// as their inline worker, with exact parameter domains and continuity. Native
/// initialization, editor/controller, configuration and termination stay on main.
pub trait IsolatedAudioProcessorProcessingRole: Send {
    /// Complete concrete adapter and SDK processing-role Box layouts.
    /// Shared DSP/native storage remains charged to its existing unique owner.
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        None
    }
    /// Process one exact shared block through the sole native DSP interpretation.
    fn process(
        &mut self,
        block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError>;
    /// Optional bounded idle DSP service, without PCM or transport advancement.
    /// Ordinary native roles have no idle wakeup or callback by default.
    fn idle_service_interval(&self) -> Option<std::time::Duration> {
        None
    }

    /// Service the same native object's active parameters on this DSP role.
    fn service_native_idle(&mut self) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }

    /// Deliver the same source's actual native Value callbacks without an
    /// audio block or time advance. Unsupported DSP roles never claim success.
    fn flush_native_author_parameters(
        &mut self,
    ) -> Result<crate::NativeAudioAuthorParameterFlushOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioAuthorParameterFlushOutcome::Refused(
            crate::NativeAudioAuthorRefusal::UnsupportedAdapter,
        ))
    }
}
