//! Persistent canonical Reference Output producer for endurance qualification.

use std::sync::Arc;

use mondrian_core::ExecutionCancellationToken;
use mondrian_export::preset::TimelineExportRange;
use mondrian_export::queue::FrozenTimelineReferenceFrameSession;
use mondrian_media::{AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderRequest};
use mondrian_reference_output::{
    ReferenceAudioCadence, ReferenceOutputDeviceDescriptor, ReferenceOutputOpenRequest,
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
}

impl PersistentReferenceOutputPump {
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
        })
    }

    /// Open the exact provider mode, schedule complete preroll, then start playback.
    pub fn open_preroll_and_start(
        &mut self,
        app: &mut AppState,
        device: &ReferenceOutputDeviceDescriptor,
    ) -> Result<(), PersistentReferenceOutputError> {
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
        app.start_reference_output()
            .map_err(|error| self.latch_fault(format!("start Reference Output: {error}")))?;
        self.state = ReferencePumpState::Running;
        Ok(())
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
        let bundle = self
            .program
            .execute_cpu(
                &picture.working_frame,
                frame_index,
                &audio.samples,
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
