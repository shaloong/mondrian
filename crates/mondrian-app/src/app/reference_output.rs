//! App composition/lifecycle Adapter for professional Reference Output.
//!
//! Machine-local device routing never enters Project author state or Undo/Redo.
//! The service binds one physical Session to the exact active Sequence revision
//! and Project author generation; any edit, navigation change, or Project close
//! stops stale scheduled output before another callback can publish authority.

use mondrian_core::timeline_data::FieldOrder;
use mondrian_core::types::{SequenceId, SequenceRevision};
use mondrian_reference_output::{
    ReferenceOutputAdapter, ReferenceOutputBundle, ReferenceOutputDeviceDescriptor,
    ReferenceOutputDiagnostics, ReferenceOutputModule, ReferenceOutputModuleShutdownReceipt,
    ReferenceOutputModuleStopCoordinator, ReferenceOutputModuleStopOutcome,
    ReferenceOutputOpenRequest, ReferenceOutputSessionShutdownReceipt,
};
use std::time::{Duration, Instant};

use super::AppState;
use mondrian_timeline::sequence::{StaticHdrMetadataPolicy, VideoRange};

/// Exact author binding for one physical output Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceOutputBinding {
    /// Active root/nested Sequence identity.
    pub sequence_id: SequenceId,
    /// Monotonic Sequence author revision.
    pub sequence_revision: SequenceRevision,
    /// Project-wide author generation captured at open.
    pub author_generation: u64,
}

/// Product-visible state of ordinary asynchronous Reference Output teardown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AppReferenceOutputTeardownStatus {
    /// No ordinary Session teardown is in flight or latched as failed.
    #[default]
    Idle,
    /// The stop request was admitted and provider consumption is still active.
    Stopping,
    /// Resource closure was not proven; rebind remains fail closed.
    Failed,
}

const PRODUCT_REFERENCE_OUTPUT_RETIRE_BUDGET: Duration = Duration::from_millis(25);

#[derive(Default)]
pub(crate) struct AppReferenceOutputService {
    output: Option<ReferenceOutputModule<Box<dyn ReferenceOutputAdapter>>>,
    stopping: Option<ReferenceOutputModuleStopCoordinator<Box<dyn ReferenceOutputAdapter>>>,
    request_failure_pending: bool,
    terminal_failure: Option<ReferenceOutputModuleShutdownReceipt>,
    last_diagnostics: Option<ReferenceOutputDiagnostics>,
    binding: Option<ReferenceOutputBinding>,
}

impl AppReferenceOutputService {
    fn install(
        &mut self,
        adapter: Box<dyn ReferenceOutputAdapter>,
    ) -> Result<(), AppReferenceOutputError> {
        self.retire_until(Instant::now() + PRODUCT_REFERENCE_OUTPUT_RETIRE_BUDGET)?;
        self.output = Some(ReferenceOutputModule::new(adapter));
        self.last_diagnostics = None;
        Ok(())
    }

    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, AppReferenceOutputError> {
        self.prepare_ready()?;
        self.output
            .as_mut()
            .ok_or(AppReferenceOutputError::AdapterNotInstalled)?
            .discover()
            .map_err(Into::into)
    }

    fn open(
        &mut self,
        binding: ReferenceOutputBinding,
        device: &ReferenceOutputDeviceDescriptor,
        request: ReferenceOutputOpenRequest,
        first_frame_index: u64,
    ) -> Result<(), AppReferenceOutputError> {
        self.prepare_ready()?;
        self.output.as_mut().ok_or(AppReferenceOutputError::AdapterNotInstalled)?.open(
            device,
            request,
            first_frame_index,
        )?;
        self.binding = Some(binding);
        Ok(())
    }

    fn schedule(
        &mut self,
        current: ReferenceOutputBinding,
        bundle: ReferenceOutputBundle,
    ) -> Result<(), AppReferenceOutputError> {
        self.prepare_ready()?;
        self.validate_binding(current)?;
        self.output
            .as_mut()
            .ok_or(AppReferenceOutputError::AdapterNotInstalled)?
            .schedule(bundle)?;
        Ok(())
    }

    fn start(&mut self, current: ReferenceOutputBinding) -> Result<(), AppReferenceOutputError> {
        self.prepare_ready()?;
        self.validate_binding(current)?;
        self.output
            .as_mut()
            .ok_or(AppReferenceOutputError::AdapterNotInstalled)?
            .start()?;
        Ok(())
    }

    fn poll(
        &mut self,
        current: ReferenceOutputBinding,
        limit: usize,
    ) -> Result<usize, AppReferenceOutputError> {
        self.prepare_ready()?;
        self.validate_binding(current)?;
        self.output
            .as_mut()
            .ok_or(AppReferenceOutputError::AdapterNotInstalled)?
            .poll(limit)
            .map_err(Into::into)
    }

    fn stop(&mut self) -> Result<(), AppReferenceOutputError> {
        self.binding = None;
        if let Some(receipt) = self.terminal_failure.as_ref() {
            return Err(AppReferenceOutputError::TeardownIncomplete {
                receipt: Box::new(receipt.clone()),
            });
        }
        if self.stopping.is_some() {
            if self
                .stopping
                .as_ref()
                .is_some_and(ReferenceOutputModuleStopCoordinator::is_finished)
            {
                self.settle_finished_stop()?;
            }
            return if self.request_failure_pending {
                Err(AppReferenceOutputError::TeardownRequestFailed)
            } else {
                Ok(())
            };
        }
        let Some(output) = self.output.take() else {
            return Ok(());
        };
        if self.binding.is_none() && output.diagnostics().device_id.is_none() {
            self.output = Some(output);
            return Ok(());
        }
        self.last_diagnostics = Some(output.diagnostics().clone());
        let stopping = output.begin_stop();
        let admitted = stopping.shutdown_request_admitted();
        self.stopping = Some(stopping);
        if admitted {
            Ok(())
        } else {
            self.request_failure_pending = true;
            let diagnostics = self.last_diagnostics.get_or_insert_with(Default::default);
            diagnostics.state = mondrian_reference_output::ReferenceOutputState::Failed;
            diagnostics.last_error = Some(
                "provider did not admit the non-blocking Reference Output shutdown request"
                    .to_owned(),
            );
            Err(AppReferenceOutputError::TeardownRequestFailed)
        }
    }

    /// Retire Project-scoped ownership within the ordinary product budget.
    pub(super) fn retire(&mut self) -> Result<(), AppReferenceOutputError> {
        self.retire_until(Instant::now() + PRODUCT_REFERENCE_OUTPUT_RETIRE_BUDGET)
    }

    fn retire_until(&mut self, deadline: Instant) -> Result<(), AppReferenceOutputError> {
        self.binding = None;
        if let Some(receipt) = self.terminal_failure.as_ref() {
            return Err(AppReferenceOutputError::TeardownIncomplete {
                receipt: Box::new(receipt.clone()),
            });
        }
        let receipt = if let Some(stopping) = self.stopping.take() {
            self.request_failure_pending = false;
            match stopping.finish_until(deadline) {
                ReferenceOutputModuleStopOutcome::Stopped(output) => {
                    (*output).shutdown_until(deadline)
                }
                ReferenceOutputModuleStopOutcome::Terminal(receipt) => *receipt,
            }
        } else if let Some(output) = self.output.take() {
            output.shutdown_until(deadline)
        } else {
            return Ok(());
        };
        if receipt.all_resources_released() {
            self.last_diagnostics = None;
            Ok(())
        } else {
            self.last_diagnostics = Some(receipt.diagnostics.clone());
            self.terminal_failure = Some(receipt.clone());
            Err(AppReferenceOutputError::TeardownIncomplete { receipt: Box::new(receipt) })
        }
    }

    pub(super) fn begin_endurance_shutdown(&mut self) {
        self.binding = None;
        if let Some(output) = self.output.as_mut() {
            // The Module retains any provider request failure for the consuming
            // receipt; signaling every owner must continue without early exit.
            let _ = output.begin_shutdown();
        }
    }

    pub(super) fn finish_endurance_shutdown(
        &mut self,
        deadline: std::time::Instant,
    ) -> ReferenceOutputModuleShutdownReceipt {
        self.begin_endurance_shutdown();
        self.binding = None;
        if let Some(receipt) = self.terminal_failure.take() {
            return receipt;
        }
        if let Some(stopping) = self.stopping.take() {
            self.request_failure_pending = false;
            return match stopping.finish_until(deadline) {
                ReferenceOutputModuleStopOutcome::Stopped(output) => {
                    (*output).shutdown_until(deadline)
                }
                ReferenceOutputModuleStopOutcome::Terminal(receipt) => *receipt,
            };
        }
        self.output.take().map_or_else(
            || ReferenceOutputModuleShutdownReceipt {
                schema_version: 2,
                session: ReferenceOutputSessionShutdownReceipt::never_opened(),
                diagnostics: ReferenceOutputDiagnostics::default(),
                outstanding_frames_before_shutdown: 0,
                module_failure: None,
            },
            |output| output.shutdown_until(deadline),
        )
    }

    fn validate_binding(
        &mut self,
        current: ReferenceOutputBinding,
    ) -> Result<(), AppReferenceOutputError> {
        let Some(expected) = self.binding else {
            return Err(AppReferenceOutputError::NotBound);
        };
        if expected == current {
            return Ok(());
        }
        self.stop()?;
        Err(AppReferenceOutputError::StaleBinding { expected, current })
    }

    fn diagnostics(&self) -> Option<&ReferenceOutputDiagnostics> {
        self.output
            .as_ref()
            .map(ReferenceOutputModule::diagnostics)
            .or(self.last_diagnostics.as_ref())
    }

    fn teardown_status(&self) -> AppReferenceOutputTeardownStatus {
        if self.terminal_failure.is_some() || self.request_failure_pending {
            AppReferenceOutputTeardownStatus::Failed
        } else if self.stopping.is_some() {
            AppReferenceOutputTeardownStatus::Stopping
        } else {
            AppReferenceOutputTeardownStatus::Idle
        }
    }

    fn prepare_ready(&mut self) -> Result<(), AppReferenceOutputError> {
        if let Some(receipt) = self.terminal_failure.as_ref() {
            return Err(AppReferenceOutputError::TeardownIncomplete {
                receipt: Box::new(receipt.clone()),
            });
        }
        if self.request_failure_pending {
            if self
                .stopping
                .as_ref()
                .is_some_and(ReferenceOutputModuleStopCoordinator::is_finished)
            {
                return self.settle_finished_stop();
            }
            return Err(AppReferenceOutputError::TeardownRequestFailed);
        }
        let Some(stopping) = self.stopping.as_ref() else {
            return Ok(());
        };
        if !stopping.is_finished() {
            return Err(AppReferenceOutputError::TeardownInProgress);
        }
        self.settle_finished_stop()
    }

    fn settle_finished_stop(&mut self) -> Result<(), AppReferenceOutputError> {
        let Some(stopping) = self.stopping.take() else {
            return Ok(());
        };
        match stopping.finish_until(Instant::now()) {
            ReferenceOutputModuleStopOutcome::Stopped(output) => {
                self.request_failure_pending = false;
                self.last_diagnostics = Some(output.diagnostics().clone());
                self.output = Some(*output);
                Ok(())
            }
            ReferenceOutputModuleStopOutcome::Terminal(receipt) => {
                let receipt = *receipt;
                self.request_failure_pending = false;
                self.last_diagnostics = Some(receipt.diagnostics.clone());
                self.terminal_failure = Some(receipt.clone());
                Err(AppReferenceOutputError::TeardownIncomplete { receipt: Box::new(receipt) })
            }
        }
    }
}

impl Drop for AppReferenceOutputService {
    fn drop(&mut self) {
        self.binding = None;
        if let Some(stopping) = self.stopping.take() {
            drop(stopping);
        }
        if let Some(output) = self.output.take() {
            let _receipt = output.shutdown_until(Instant::now());
        }
    }
}

impl AppState {
    /// Install one machine-local physical/simulated Adapter Implementation.
    /// This is composition, not an authoring action.
    pub fn install_reference_output_adapter(
        &mut self,
        adapter: Box<dyn ReferenceOutputAdapter>,
    ) -> Result<(), AppReferenceOutputError> {
        self.reference_output.install(adapter)
    }

    /// Enumerate exact device modes through the installed Adapter.
    pub fn discover_reference_output_devices(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, AppReferenceOutputError> {
        self.reference_output.discover()
    }

    /// Open and bind a device Session to the exact current author state.
    pub fn open_reference_output(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: ReferenceOutputOpenRequest,
        first_frame_index: u64,
    ) -> Result<(), AppReferenceOutputError> {
        self.validate_reference_output_request(&request)?;
        let binding = self.current_reference_output_binding()?;
        self.reference_output.open(binding, device, request, first_frame_index)
    }

    /// Schedule one already-lowered canonical Program Output + Audio Program bundle.
    pub fn schedule_reference_output(
        &mut self,
        bundle: ReferenceOutputBundle,
    ) -> Result<(), AppReferenceOutputError> {
        let binding = self.current_reference_output_binding()?;
        self.reference_output.schedule(binding, bundle)
    }

    /// Start after complete provider preroll.
    pub fn start_reference_output(&mut self) -> Result<(), AppReferenceOutputError> {
        let binding = self.current_reference_output_binding()?;
        self.reference_output.start(binding)
    }

    /// Drain bounded provider callback/status events.
    pub fn poll_reference_output(
        &mut self,
        limit: usize,
    ) -> Result<usize, AppReferenceOutputError> {
        let binding = self.current_reference_output_binding()?;
        self.reference_output.poll(binding, limit)
    }

    /// Stop device output and release ownership.
    pub fn stop_reference_output(&mut self) -> Result<(), AppReferenceOutputError> {
        self.reference_output.stop()
    }

    /// Current machine-local output diagnostics.
    pub fn reference_output_diagnostics(&self) -> Option<&ReferenceOutputDiagnostics> {
        self.reference_output.diagnostics()
    }

    /// Ordinary product teardown status independent from provider diagnostics.
    ///
    /// `Stopping` means only that a non-blocking stop request was admitted;
    /// provider release is not proven until this returns `Idle` after a later
    /// operation reaps coordinator completion. `Failed` blocks rebinding.
    pub fn reference_output_teardown_status(&self) -> AppReferenceOutputTeardownStatus {
        self.reference_output.teardown_status()
    }

    /// Current exact author binding, when a Session is active.
    pub const fn reference_output_binding(&self) -> Option<ReferenceOutputBinding> {
        self.reference_output.binding
    }

    fn current_reference_output_binding(
        &self,
    ) -> Result<ReferenceOutputBinding, AppReferenceOutputError> {
        let sequence = self.active_sequence().ok_or(AppReferenceOutputError::NoActiveSequence)?;
        Ok(ReferenceOutputBinding {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            author_generation: self.project_author_generation(),
        })
    }

    fn validate_reference_output_request(
        &self,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<(), AppReferenceOutputError> {
        let sequence = self.active_sequence().ok_or(AppReferenceOutputError::NoActiveSequence)?;
        let settings = &sequence.settings;
        if request.signal.width != settings.resolution.width
            || request.signal.height != settings.resolution.height
        {
            return Err(AppReferenceOutputError::SequenceSignalMismatch { field: "raster" });
        }
        if request.signal.frame_rate != settings.frame_rate {
            return Err(AppReferenceOutputError::SequenceSignalMismatch { field: "cadence" });
        }
        if settings.field_order != FieldOrder::Progressive
            || request.signal.scan != mondrian_reference_output::ReferenceOutputScan::Progressive
        {
            return Err(AppReferenceOutputError::InterlacedHardwareOutputUnqualified);
        }
        if settings.audio_sample_rate != 48_000 {
            return Err(AppReferenceOutputError::SequenceSignalMismatch {
                field: "embedded audio sample rate",
            });
        }
        if request.signal.audio_layout != settings.audio_channel_layout {
            return Err(AppReferenceOutputError::SequenceSignalMismatch {
                field: "embedded audio layout",
            });
        }
        let expected_range = match settings.delivery.video_range {
            VideoRange::Full => mondrian_reference_output::ReferenceOutputRange::Full,
            VideoRange::Legal => mondrian_reference_output::ReferenceOutputRange::Legal,
        };
        if request.signal.range != expected_range {
            return Err(AppReferenceOutputError::SequenceSignalMismatch { field: "video range" });
        }
        let program = settings
            .root_program_color_context(self.project_color_environment())
            .map_err(|error| AppReferenceOutputError::ProgramColor { detail: error.to_string() })?;
        if program.output_color_space().color() != Some(request.signal.color_space) {
            return Err(AppReferenceOutputError::SequenceSignalMismatch {
                field: "Program Output color identity",
            });
        }
        let expected_hdr = match settings.delivery.static_hdr_metadata_policy {
            StaticHdrMetadataPolicy::Omit if request.signal.color_space.is_hdr() => {
                Some(mondrian_reference_output::ReferenceHdrSignal {
                    mastering_display: None,
                    content_light: None,
                })
            }
            StaticHdrMetadataPolicy::Omit => None,
            StaticHdrMetadataPolicy::WriteAuthored => {
                Some(mondrian_reference_output::ReferenceHdrSignal {
                    mastering_display: settings.delivery.hdr_mastering_display.clone(),
                    content_light: settings.delivery.hdr_content_light,
                })
            }
        };
        if request.signal.hdr != expected_hdr {
            return Err(AppReferenceOutputError::SequenceSignalMismatch {
                field: "HDR signal metadata",
            });
        }
        Ok(())
    }
}

/// App composition/lifecycle failure.
#[derive(Debug, thiserror::Error)]
pub enum AppReferenceOutputError {
    /// No vendor/simulated Adapter was installed by the composition root.
    #[error("reference output Adapter is not installed")]
    AdapterNotInstalled,
    /// Device output requires an open Project with an active Sequence.
    #[error("reference output requires an active Sequence")]
    NoActiveSequence,
    /// Physical signal must be derived from the active Sequence, not guessed by UI.
    #[error("reference output {field} differs from the active Sequence")]
    SequenceSignalMismatch { field: &'static str },
    /// ADR-0008 does not yet qualify hardware field scheduling.
    #[error("interlaced hardware reference output is not qualified")]
    InterlacedHardwareOutputUnqualified,
    /// Canonical Program Output color context could not be resolved.
    #[error("reference output Program color context is blocked: {detail}")]
    ProgramColor { detail: String },
    /// Operation requires an author binding created at open.
    #[error("reference output Session has no author binding")]
    NotBound,
    /// Author edits/navigation revoked queued output authority.
    #[error("reference output author binding changed from {expected:?} to {current:?}")]
    StaleBinding {
        expected: ReferenceOutputBinding,
        current: ReferenceOutputBinding,
    },
    /// A prior ordinary stop is still consuming provider ownership.
    #[error("reference output teardown is still in progress")]
    TeardownInProgress,
    /// Provider rejected or panicked while admitting the non-blocking request.
    #[error("reference output provider did not admit the shutdown request")]
    TeardownRequestFailed,
    /// Provider or coordinator closure was not positively proven.
    #[error("reference output teardown did not prove complete resource release")]
    TeardownIncomplete {
        /// Terminal fail-closed lifecycle evidence.
        receipt: Box<ReferenceOutputModuleShutdownReceipt>,
    },
    /// Deep Reference Output Module failure.
    #[error(transparent)]
    Output(#[from] mondrian_reference_output::ReferenceOutputError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};
    use mondrian_reference_output::{
        ReferenceOutputAdapterError, ReferenceOutputAdapterEvent, ReferenceOutputAdapterSession,
        ReferenceOutputMode, ReferenceOutputPixelFormat, ReferenceOutputProvider,
        ReferenceOutputProviderEvidence, ReferenceOutputRange, ReferenceOutputReferencePolicy,
        ReferenceOutputRuntimeAvailability, ReferenceOutputScan, ReferenceOutputSignal,
        SimulatedReferenceOutputAdapter,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct BlockingDropAdapter {
        evidence: ReferenceOutputProviderEvidence,
        dropped: Arc<AtomicBool>,
        delay: Duration,
    }

    #[derive(Clone, Copy)]
    enum BeginFailureMode {
        Error,
        Panic,
    }

    struct BeginFailureAdapter {
        inner: SimulatedReferenceOutputAdapter,
        mode: BeginFailureMode,
    }

    impl ReferenceOutputAdapter for BeginFailureAdapter {
        fn evidence(&self) -> &ReferenceOutputProviderEvidence {
            self.inner.evidence()
        }

        fn discover(
            &mut self,
        ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
            self.inner.discover()
        }

        fn open(
            &mut self,
            device: &ReferenceOutputDeviceDescriptor,
            request: &ReferenceOutputOpenRequest,
        ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
            Ok(Box::new(BeginFailureSession {
                inner: self.inner.open(device, request)?,
                mode: self.mode,
            }))
        }
    }

    struct BeginFailureSession {
        inner: Box<dyn ReferenceOutputAdapterSession>,
        mode: BeginFailureMode,
    }

    impl ReferenceOutputAdapterSession for BeginFailureSession {
        fn evidence(&self) -> &ReferenceOutputProviderEvidence {
            self.inner.evidence()
        }

        fn request(&self) -> &ReferenceOutputOpenRequest {
            self.inner.request()
        }

        fn device_generation(&self) -> u64 {
            self.inner.device_generation()
        }

        fn schedule(
            &mut self,
            bundle: ReferenceOutputBundle,
        ) -> Result<(), ReferenceOutputAdapterError> {
            self.inner.schedule(bundle)
        }

        fn start(&mut self) -> Result<(), ReferenceOutputAdapterError> {
            self.inner.start()
        }

        fn poll(
            &mut self,
        ) -> Result<Option<ReferenceOutputAdapterEvent>, ReferenceOutputAdapterError> {
            self.inner.poll()
        }

        fn begin_shutdown(&mut self) -> Result<(), ReferenceOutputAdapterError> {
            match self.mode {
                BeginFailureMode::Error => Err(ReferenceOutputAdapterError::Vendor {
                    operation: "begin_shutdown",
                    detail: "synthetic App request rejection".to_owned(),
                }),
                BeginFailureMode::Panic => panic!("synthetic App request panic"),
            }
        }

        fn stop(&mut self) -> Result<(), ReferenceOutputAdapterError> {
            self.inner.stop()
        }

        fn shutdown(self: Box<Self>) -> ReferenceOutputSessionShutdownReceipt {
            let Self { inner, .. } = *self;
            inner.shutdown()
        }
    }

    impl Drop for BlockingDropAdapter {
        fn drop(&mut self) {
            std::thread::sleep(self.delay);
            self.dropped.store(true, Ordering::Release);
        }
    }

    impl ReferenceOutputAdapter for BlockingDropAdapter {
        fn evidence(&self) -> &ReferenceOutputProviderEvidence {
            &self.evidence
        }

        fn discover(
            &mut self,
        ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
            Ok(Vec::new())
        }

        fn open(
            &mut self,
            _device: &ReferenceOutputDeviceDescriptor,
            _request: &ReferenceOutputOpenRequest,
        ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
            Err(ReferenceOutputAdapterError::NoDevices)
        }
    }

    fn request() -> ReferenceOutputOpenRequest {
        ReferenceOutputOpenRequest {
            signal: ReferenceOutputSignal {
                width: 6,
                height: 1,
                frame_rate: Rational::FPS_25,
                scan: ReferenceOutputScan::Progressive,
                pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
                color_space: ColorSpace::Rec709,
                range: ReferenceOutputRange::Legal,
                hdr: None,
                audio_layout: AudioChannelLayout::Stereo,
            },
            reference_policy: ReferenceOutputReferencePolicy::FreeRunAllowed,
            ancillary_policy: mondrian_reference_output::ReferenceOutputAncillaryPolicy::Disabled,
            preroll_frames: 1,
            max_scheduled_frames: 2,
        }
    }

    fn simulated_adapter(request: &ReferenceOutputOpenRequest) -> SimulatedReferenceOutputAdapter {
        SimulatedReferenceOutputAdapter::new(vec![ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: false,
            supports_ancillary_readback: false,
        }])
        .expect("simulated Adapter")
    }

    fn blocking_adapter(dropped: Arc<AtomicBool>) -> BlockingDropAdapter {
        BlockingDropAdapter {
            evidence: ReferenceOutputProviderEvidence {
                provider: ReferenceOutputProvider::Simulated,
                adapter_version: "test".to_owned(),
                sdk_version: None,
                driver_version: None,
                hardware_backed: false,
                availability: ReferenceOutputRuntimeAvailability::Available,
            },
            dropped,
            delay: Duration::from_millis(250),
        }
    }

    fn assert_begin_failure_status(mode: BeginFailureMode) {
        let request = request();
        let mut service = AppReferenceOutputService::default();
        service
            .install(Box::new(BeginFailureAdapter {
                inner: simulated_adapter(&request),
                mode,
            }))
            .expect("install");
        let device = service.discover().expect("discover").remove(0);
        let binding = ReferenceOutputBinding {
            sequence_id: SequenceId::new(),
            sequence_revision: SequenceRevision::INITIAL,
            author_generation: 1,
        };
        service.open(binding, &device, request, 0).expect("open");

        assert!(matches!(
            service.stop(),
            Err(AppReferenceOutputError::TeardownRequestFailed)
        ));
        assert_eq!(
            service.teardown_status(),
            AppReferenceOutputTeardownStatus::Failed
        );
        let diagnostics = service.diagnostics().expect("failure diagnostics");
        assert_eq!(
            diagnostics.state,
            mondrian_reference_output::ReferenceOutputState::Failed
        );
        assert!(diagnostics.last_error.is_some());

        let receipt = service.finish_endurance_shutdown(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            receipt.diagnostics.state,
            mondrian_reference_output::ReferenceOutputState::Failed
        );
        assert!(!receipt.all_resources_released());
    }

    fn wait_for_drop(dropped: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !dropped.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_stop_is_observable_and_reaps_clean_module_for_reuse() {
        let request = request();
        let mut service = AppReferenceOutputService::default();
        service.install(Box::new(simulated_adapter(&request))).expect("install");
        let device = service.discover().expect("discover").remove(0);
        let binding = ReferenceOutputBinding {
            sequence_id: SequenceId::new(),
            sequence_revision: SequenceRevision::INITIAL,
            author_generation: 1,
        };
        service.open(binding, &device, request, 0).expect("open");

        let started = Instant::now();
        service.stop().expect("stop admission");
        assert!(started.elapsed() < Duration::from_millis(75));
        assert_eq!(
            service.teardown_status(),
            AppReferenceOutputTeardownStatus::Stopping
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match service.discover() {
                Ok(_) => break,
                Err(AppReferenceOutputError::TeardownInProgress) if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                Err(error) => panic!("ordinary stop did not settle cleanly: {error}"),
            }
        }
        assert_eq!(
            service.teardown_status(),
            AppReferenceOutputTeardownStatus::Idle
        );
        let receipt = service.finish_endurance_shutdown(Instant::now() + Duration::from_secs(1));
        assert!(receipt.all_resources_released());
    }

    #[test]
    fn rejected_begin_shutdown_is_immediately_failed_not_stopping() {
        assert_begin_failure_status(BeginFailureMode::Error);
    }

    #[test]
    fn panicking_begin_shutdown_is_immediately_failed_not_stopping() {
        assert_begin_failure_status(BeginFailureMode::Panic);
    }

    #[test]
    fn service_retire_bounds_blocking_adapter_destruction() {
        let dropped = Arc::new(AtomicBool::new(false));
        let mut service = AppReferenceOutputService::default();
        service.output = Some(ReferenceOutputModule::new(
            Box::new(blocking_adapter(Arc::clone(&dropped))) as Box<dyn ReferenceOutputAdapter>,
        ));

        let started = Instant::now();
        let result = service.retire();
        assert!(started.elapsed() < Duration::from_millis(150));
        assert!(matches!(
            result,
            Err(AppReferenceOutputError::TeardownIncomplete { .. })
        ));
        assert!(!dropped.load(Ordering::Acquire));
        assert_eq!(
            service.teardown_status(),
            AppReferenceOutputTeardownStatus::Failed
        );
        wait_for_drop(&dropped);
    }

    #[test]
    fn service_drop_never_waits_for_blocking_adapter_destruction() {
        let dropped = Arc::new(AtomicBool::new(false));
        let mut service = AppReferenceOutputService::default();
        service.output = Some(ReferenceOutputModule::new(
            Box::new(blocking_adapter(Arc::clone(&dropped))) as Box<dyn ReferenceOutputAdapter>,
        ));

        let started = Instant::now();
        drop(service);
        assert!(started.elapsed() < Duration::from_millis(75));
        assert!(!dropped.load(Ordering::Acquire));
        wait_for_drop(&dropped);
    }
}
