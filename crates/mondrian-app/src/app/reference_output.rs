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
    ReferenceOutputOpenRequest, ReferenceOutputSessionShutdownReceipt,
};

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

#[derive(Default)]
pub(crate) struct AppReferenceOutputService {
    output: Option<ReferenceOutputModule<Box<dyn ReferenceOutputAdapter>>>,
    binding: Option<ReferenceOutputBinding>,
}

impl AppReferenceOutputService {
    fn install(
        &mut self,
        adapter: Box<dyn ReferenceOutputAdapter>,
    ) -> Result<(), AppReferenceOutputError> {
        self.stop()?;
        self.output = Some(ReferenceOutputModule::new(adapter));
        Ok(())
    }

    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, AppReferenceOutputError> {
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
        self.validate_binding(current)?;
        self.output
            .as_mut()
            .ok_or(AppReferenceOutputError::AdapterNotInstalled)?
            .schedule(bundle)?;
        Ok(())
    }

    fn start(&mut self, current: ReferenceOutputBinding) -> Result<(), AppReferenceOutputError> {
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
        self.validate_binding(current)?;
        self.output
            .as_mut()
            .ok_or(AppReferenceOutputError::AdapterNotInstalled)?
            .poll(limit)
            .map_err(Into::into)
    }

    fn stop(&mut self) -> Result<(), AppReferenceOutputError> {
        if let Some(output) = self.output.as_mut() {
            output.stop()?;
        }
        self.binding = None;
        Ok(())
    }

    /// Retire Project-scoped ownership even when a vendor stop call fails.
    /// Dropping the Module/Session is the final Adapter release boundary.
    pub(super) fn retire(&mut self) -> Result<(), AppReferenceOutputError> {
        self.binding = None;
        let Some(mut output) = self.output.take() else {
            return Ok(());
        };
        output.stop().map_err(Into::into)
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
        self.output.as_ref().map(ReferenceOutputModule::diagnostics)
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
    /// Deep Reference Output Module failure.
    #[error(transparent)]
    Output(#[from] mondrian_reference_output::ReferenceOutputError),
}
