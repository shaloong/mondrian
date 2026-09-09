//! Persistent canonical Reference Output producer for endurance qualification.

use std::sync::Arc;

use mondrian_core::ExecutionCancellationToken;
use mondrian_export::preset::TimelineExportRange;
use mondrian_export::queue::FrozenTimelineReferenceFrameSession;
use mondrian_media::{AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderRequest};
use mondrian_reference_output::{
    ReferenceAudioCadence, ReferenceOutputDeviceDescriptor, ReferenceOutputDiagnostics,
    ReferenceOutputOpenRequest, ReferenceOutputProvider, ReferenceOutputReferencePolicy,
    ReferenceOutputRuntimeAvailability, ReferenceOutputState,
};
use mondrian_renderer::{ReferenceOutputProgram, RenderCpuColorExecutionSession};
use thiserror::Error;

use super::audio_rendering::TimelineAudioPcmRenderer;
use super::exporting::capture_timeline_export_snapshot;
use super::{AppState, AudioPcmRenderer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenReferenceBinding {
    sequence_id: mondrian_core::SequenceId,
    sequence_revision: mondrian_core::SequenceRevision,
    author_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReferencePumpState {
    Prepared,
    Running,
    Closing,
}

/// Phase-scoped full-raster Program Output and public Audio Program pump.
///
/// The picture path owns one frozen exact-source materialization session; the
/// audio path owns one independent public Program runtime with no audition or
/// channel remapping. A frame is scheduled only after both paths and Reference
/// signal packing succeed for the same absolute frame/cadence coordinate.
#[must_use = "a persistent Reference Output pump must be explicitly closed"]
pub struct PersistentReferenceOutputPump {
    binding: FrozenReferenceBinding,
    request: ReferenceOutputOpenRequest,
    picture: FrozenTimelineReferenceFrameSession,
    audio: TimelineAudioPcmRenderer,
    audio_generation: AudioPcmRenderGeneration,
    audio_entered: bool,
    cadence: ReferenceAudioCadence,
    program: ReferenceOutputProgram,
    color_session: RenderCpuColorExecutionSession,
    cancellation: ExecutionCancellationToken,
    state: ReferencePumpState,
    scheduled_frames: u64,
    fault: Option<String>,
    wire_correlation: Option<mondrian_broadcast::AncillaryWireCorrelation>,
    ancillary_program: Option<Arc<super::endurance_ancillary::PreparedEnduranceAncillaryProgram>>,
}

/// Opaque proof that the exact physical Session opened, reported external
/// lock through its live event/readback path, and entered Running state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalReferenceStartEvidence {
    _private: (),
}

impl PersistentReferenceOutputPump {
    /// Nanosecond scheduling interval derived from the exact rational picture cadence.
    #[cfg(feature = "validation")]
    pub(crate) fn cadence_interval(&self) -> Result<std::time::Duration, String> {
        let rate = self.request.signal.frame_rate;
        let numerator = u128::try_from(rate.num)
            .map_err(|_| "Reference frame-rate numerator must be positive".to_owned())?;
        let denominator = u128::try_from(rate.den)
            .map_err(|_| "Reference frame-rate denominator must be positive".to_owned())?;
        if numerator == 0 || denominator == 0 {
            return Err("Reference frame rate must be nonzero".to_owned());
        }
        let nanoseconds = 1_000_000_000_u128
            .checked_mul(denominator)
            .and_then(|value| value.checked_div(numerator))
            .ok_or_else(|| "Reference frame interval overflowed".to_owned())?
            .max(1);
        Ok(std::time::Duration::from_nanos(
            u64::try_from(nanoseconds)
                .map_err(|_| "Reference frame interval exceeds u64".to_owned())?,
        ))
    }

    /// Freeze the active Timeline and prepare all software owners before opening hardware.
    pub fn prepare(
        app: &AppState,
        request: ReferenceOutputOpenRequest,
        first_frame_index: u64,
    ) -> Result<Self, PersistentReferenceOutputError> {
        let sequence = app.active_sequence().cloned().ok_or_else(|| {
            PersistentReferenceOutputError::InvalidPlan(
                "Reference Output requires an active Sequence".to_owned(),
            )
        })?;
        if sequence.settings.audio_sample_rate != 48_000 {
            return Err(PersistentReferenceOutputError::InvalidPlan(
                "Reference embedded Audio Program requires an exact 48 kHz Sequence".to_owned(),
            ));
        }
        if request.reference_policy != ReferenceOutputReferencePolicy::RequireExternalLock {
            return Err(PersistentReferenceOutputError::InvalidPlan(
                "commercial endurance Reference Output requires external lock".to_owned(),
            ));
        }
        let sequences = app.export_sequences_snapshot();
        let binding = FrozenReferenceBinding {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            author_generation: app.project_author_generation(),
        };
        let color_context = sequence
            .settings
            .root_program_color_context(app.project_color_environment())
            .map_err(|error| PersistentReferenceOutputError::InvalidPlan(error.to_string()))?;
        let program = ReferenceOutputProgram::prepare(&color_context, request.signal.clone())
            .map_err(|error| PersistentReferenceOutputError::InvalidPlan(error.to_string()))?;
        let cadence = ReferenceAudioCadence::new(request.signal.frame_rate, 48_000)
            .map_err(|error| PersistentReferenceOutputError::InvalidPlan(error.to_string()))?;
        let timeline = capture_timeline_export_snapshot(
            app,
            sequence.clone(),
            sequences.clone(),
            TimelineExportRange::EntireSequence,
            false,
        )
        .map_err(PersistentReferenceOutputError::InvalidPlan)?;
        let picture = FrozenTimelineReferenceFrameSession::new(timeline, first_frame_index)
            .map_err(PersistentReferenceOutputError::InvalidPlan)?;
        let library = app.asset_library_handle().ok_or_else(|| {
            PersistentReferenceOutputError::InvalidPlan(
                "Reference Audio Program requires the active Asset Library".to_owned(),
            )
        })?;
        let audio = TimelineAudioPcmRenderer::new_public_program(
            sequence,
            sequences,
            library,
            Arc::clone(&app.audio_source_cache),
            app.execution_resources.decision().audio.runtime_grant,
        )
        .map_err(|error| PersistentReferenceOutputError::InvalidPlan(error.to_string()))?;
        let audio_generation = AudioPcmRenderGeneration::new(
            binding.author_generation.checked_add(1).ok_or_else(|| {
                PersistentReferenceOutputError::InvalidPlan(
                    "Reference Audio Program generation overflow".to_owned(),
                )
            })?,
        );
        Ok(Self {
            binding,
            request,
            picture,
            audio,
            audio_generation,
            audio_entered: false,
            cadence,
            program,
            color_session: RenderCpuColorExecutionSession::default(),
            cancellation: ExecutionCancellationToken::new(),
            state: ReferencePumpState::Prepared,
            scheduled_frames: 0,
            fault: None,
            wire_correlation: None,
            ancillary_program: None,
        })
    }

    /// Bind the explicit validation-only canonical ANC owner before hardware opens.
    pub fn set_wire_correlation(
        &mut self,
        correlation: Option<mondrian_broadcast::AncillaryWireCorrelation>,
    ) -> Result<(), PersistentReferenceOutputError> {
        self.require_state(ReferencePumpState::Prepared)?;
        if self.request.ancillary_policy.requires_readback() && correlation.is_none() {
            return Err(PersistentReferenceOutputError::InvalidPlan(
                "physical ANC readback requires its canonical correlation owner".to_owned(),
            ));
        }
        self.wire_correlation = correlation;
        Ok(())
    }

    /// Retain the same prepared program before opening physical hardware.
    pub fn set_ancillary_program(
        &mut self,
        program: Option<Arc<super::endurance_ancillary::PreparedEnduranceAncillaryProgram>>,
    ) -> Result<(), PersistentReferenceOutputError> {
        self.require_state(ReferencePumpState::Prepared)?;
        if let Some(program) = &program {
            program
                .validate_rate(self.request.signal.frame_rate)
                .map_err(PersistentReferenceOutputError::InvalidPlan)?;
            if !self.request.ancillary_policy.requires_readback() || self.wire_correlation.is_none()
            {
                return Err(PersistentReferenceOutputError::InvalidPlan(
                    "campaign ANC requires independent physical wire readback".to_owned(),
                ));
            }
        }
        self.ancillary_program = program;
        Ok(())
    }
    /// Open the exact provider mode, schedule complete preroll, then start playback.
    pub fn open_preroll_and_start(
        &mut self,
        app: &mut AppState,
        device: &ReferenceOutputDeviceDescriptor,
    ) -> Result<PhysicalReferenceStartEvidence, PersistentReferenceOutputError> {
        self.require_state(ReferencePumpState::Prepared)?;
        self.validate_binding(app)?;
        app.open_reference_output(
            device,
            self.request.clone(),
            self.picture.next_frame_index(),
        )
        .map_err(|error| self.latch_fault(format!("open Reference Output: {error}")))?;
        for _ in 0..self.request.preroll_frames {
            self.render_and_schedule_next(app)?;
        }
        app.poll_reference_output(self.request.max_scheduled_frames as usize)
            .map_err(|error| self.latch_fault(format!("poll initial Reference status: {error}")))?;
        self.validate_physical_start_evidence(app, device, ReferenceOutputState::Priming)?;
        app.start_reference_output()
            .map_err(|error| self.latch_fault(format!("start Reference Output: {error}")))?;
        self.validate_physical_start_evidence(app, device, ReferenceOutputState::Running)?;
        self.state = ReferencePumpState::Running;
        Ok(PhysicalReferenceStartEvidence { _private: () })
    }

    /// Drain provider callbacks and atomically schedule one subsequent A/V bundle.
    pub fn pump_next(&mut self, app: &mut AppState) -> Result<(), PersistentReferenceOutputError> {
        self.require_state(ReferencePumpState::Running)?;
        self.validate_binding(app)?;
        app.poll_reference_output(self.request.max_scheduled_frames as usize)
            .map_err(|error| self.latch_fault(format!("poll Reference Output: {error}")))?;
        self.render_and_schedule_next(app)
    }

    /// Stop new work and hand provider teardown to the App lifecycle owner.
    pub fn begin_close(
        &mut self,
        app: &mut AppState,
    ) -> Result<(), PersistentReferenceOutputError> {
        if self.state == ReferencePumpState::Closing {
            return Ok(());
        }
        self.state = ReferencePumpState::Closing;
        self.cancellation.cancel();
        self.picture.cancel();
        app.stop_reference_output()
            .map_err(|error| self.latch_fault(format!("stop Reference Output: {error}")))
    }

    /// Number of complete picture/audio bundles admitted to the provider queue.
    pub const fn scheduled_frames(&self) -> u64 {
        self.scheduled_frames
    }

    /// Absolute frame coordinate to be rendered by the next successful pump.
    pub const fn next_frame_index(&self) -> u64 {
        self.picture.next_frame_index()
    }

    fn render_and_schedule_next(
        &mut self,
        app: &mut AppState,
    ) -> Result<(), PersistentReferenceOutputError> {
        self.validate_binding(app)?;
        let frame_index = self.picture.next_frame_index();
        let start_sample = self
            .cadence
            .sample_position(frame_index)
            .and_then(|sample| {
                i64::try_from(sample)
                    .map_err(|_| mondrian_reference_output::ReferenceAudioCadenceError::Overflow)
            })
            .map_err(|error| self.latch_fault(error.to_string()))?;
        let frame_count =
            self.cadence
                .sample_frames_for_video_frame(frame_index)
                .map_err(|error| self.latch_fault(error.to_string()))? as usize;
        let picture = self
            .picture
            .render_next()
            .map_err(|error| self.latch_fault(format!("materialize Reference picture: {error}")))?;
        let continuity = if self.audio_entered {
            AudioPcmContinuity::Continue(self.audio_generation)
        } else {
            AudioPcmContinuity::Enter(self.audio_generation)
        };
        let audio = self
            .audio
            .render(
                AudioPcmRenderRequest {
                    start_sample,
                    frame_count,
                    sample_rate: 48_000,
                    channel_layout: self.request.signal.audio_layout,
                    continuity,
                },
                &self.cancellation,
            )
            .map_err(|error| {
                self.latch_fault(format!("render Reference Audio Program: {error}"))
            })?;
        self.audio_entered = true;
        let program_frame = match &self.ancillary_program {
            Some(owner) => owner
                .frame_at(frame_index, self.request.signal.frame_rate)
                .map_err(|error| self.latch_fault(error))?,
            None => mondrian_reference_output::AncillaryFrame::empty(frame_index),
        };
        let ancillary = match &self.wire_correlation {
            Some(owner) => owner
                .frame(frame_index, program_frame.packets().to_vec())
                .map_err(|error| self.latch_fault(format!("canonical ANC marker: {error}")))?,
            None => program_frame,
        };
        let bundle = self
            .program
            .execute_cpu_with_ancillary(
                &picture.working_frame,
                frame_index,
                &audio.samples,
                ancillary,
                &mut self.color_session,
            )
            .map_err(|error| self.latch_fault(format!("lower Reference A/V bundle: {error}")))?;
        app.schedule_reference_output(bundle)
            .map_err(|error| self.latch_fault(format!("schedule Reference A/V bundle: {error}")))?;
        self.scheduled_frames = self.scheduled_frames.checked_add(1).ok_or_else(|| {
            self.latch_fault("Reference scheduled-frame counter overflow".to_owned())
        })?;
        Ok(())
    }

    fn validate_physical_start_evidence(
        &mut self,
        app: &AppState,
        device: &ReferenceOutputDeviceDescriptor,
        expected_state: ReferenceOutputState,
    ) -> Result<(), PersistentReferenceOutputError> {
        let diagnostics = app.reference_output_diagnostics().cloned().ok_or_else(|| {
            self.latch_fault("Reference Output did not publish live diagnostics".to_owned())
        })?;
        if !physical_start_evidence_is_complete(&diagnostics, device, expected_state) {
            return Err(self.latch_fault(format!(
                "Reference Output dynamic start evidence is incomplete: {diagnostics:?}"
            )));
        }
        Ok(())
    }

    fn validate_binding(&mut self, app: &AppState) -> Result<(), PersistentReferenceOutputError> {
        let current = app.active_sequence().map(|sequence| FrozenReferenceBinding {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            author_generation: app.project_author_generation(),
        });
        if current != Some(self.binding) {
            return Err(self.latch_fault(
                "Reference Output author binding changed during the persistent session".to_owned(),
            ));
        }
        Ok(())
    }

    fn require_state(
        &mut self,
        expected: ReferencePumpState,
    ) -> Result<(), PersistentReferenceOutputError> {
        if let Some(detail) = &self.fault {
            return Err(PersistentReferenceOutputError::Faulted(detail.clone()));
        }
        if self.state != expected {
            return Err(self.latch_fault(format!(
                "Reference Output pump state is {:?}, expected {:?}",
                self.state, expected
            )));
        }
        Ok(())
    }

    fn latch_fault(&mut self, detail: String) -> PersistentReferenceOutputError {
        if self.fault.is_none() {
            self.fault = Some(detail);
        }
        PersistentReferenceOutputError::Faulted(
            self.fault
                .as_ref()
                .cloned()
                .unwrap_or_else(|| "Reference Output pump entered an unknown fault".to_owned()),
        )
    }
}

fn physical_start_evidence_is_complete(
    diagnostics: &ReferenceOutputDiagnostics,
    device: &ReferenceOutputDeviceDescriptor,
    expected_state: ReferenceOutputState,
) -> bool {
    diagnostics.state == expected_state
        && diagnostics.provider.as_ref().is_some_and(|provider| {
            provider.hardware_backed
                && provider.provider != ReferenceOutputProvider::Simulated
                && provider.provider == device.provider
                && provider.availability == ReferenceOutputRuntimeAvailability::Available
        })
        && diagnostics.device_id.as_ref() == Some(&device.id)
        && diagnostics.device_generation == Some(device.generation)
        && diagnostics.reference_locked == Some(true)
        && diagnostics.reference_lock_losses == 0
}

/// Stable persistent Reference Output preparation/execution failure.
#[derive(Debug, Error)]
pub enum PersistentReferenceOutputError {
    /// Frozen Timeline, signal, media, audio, or resource admission failed before hardware open.
    #[error("invalid persistent Reference Output plan: {0}")]
    InvalidPlan(String),
    /// A started/opening generation faulted and cannot continue.
    #[error("persistent Reference Output failed: {0}")]
    Faulted(String),
}

#[cfg(test)]
mod tests {
    use mondrian_reference_output::{ReferenceOutputDeviceId, ReferenceOutputProviderEvidence};

    use super::*;

    fn physical_fixture() -> (ReferenceOutputDeviceDescriptor, ReferenceOutputDiagnostics) {
        let device = ReferenceOutputDeviceDescriptor {
            id: ReferenceOutputDeviceId::new("decklink:test:1").expect("device id"),
            provider: ReferenceOutputProvider::DeckLink,
            display_name: "DeckLink Test".to_owned(),
            generation: 7,
            modes: Vec::new(),
        };
        let diagnostics = ReferenceOutputDiagnostics {
            state: ReferenceOutputState::Priming,
            provider: Some(ReferenceOutputProviderEvidence {
                provider: ReferenceOutputProvider::DeckLink,
                adapter_version: "test".to_owned(),
                sdk_version: Some("test-sdk".to_owned()),
                driver_version: Some("test-driver".to_owned()),
                hardware_backed: true,
                availability: ReferenceOutputRuntimeAvailability::Available,
            }),
            device_id: Some(device.id.clone()),
            device_generation: Some(device.generation),
            reference_locked: Some(true),
            ..ReferenceOutputDiagnostics::default()
        };
        (device, diagnostics)
    }

    #[test]
    fn dynamic_reference_start_requires_physical_exact_locked_readback() {
        let (device, diagnostics) = physical_fixture();
        assert!(physical_start_evidence_is_complete(
            &diagnostics,
            &device,
            ReferenceOutputState::Priming
        ));

        let mut non_hardware = diagnostics.clone();
        non_hardware.provider.as_mut().expect("provider").hardware_backed = false;
        assert!(!physical_start_evidence_is_complete(
            &non_hardware,
            &device,
            ReferenceOutputState::Priming
        ));

        let mut unlocked = diagnostics.clone();
        unlocked.reference_locked = Some(false);
        assert!(!physical_start_evidence_is_complete(
            &unlocked,
            &device,
            ReferenceOutputState::Priming
        ));

        let mut stale_generation = diagnostics;
        stale_generation.device_generation = Some(device.generation + 1);
        assert!(!physical_start_evidence_is_complete(
            &stale_generation,
            &device,
            ReferenceOutputState::Priming
        ));
    }
}
