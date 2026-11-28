//! Background analysis for the Clip Stabilizer inspector.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc::{self, Receiver, TryRecvError},
    Arc,
};

use mondrian_core::{
    mask_data::MaskTrackingSettings, ClipId, EffectId, ExecutionCancellationToken, MondrianError,
    Rational, Result, SequenceId,
};
use mondrian_editor_state::AuthoringSessionId;
use mondrian_effects::{stabilization::StabilizationRecord, EffectType};
use mondrian_media::{PreviewDecodeSessionContext, PreviewSourceColorContract};
use mondrian_renderer::stabilization::{stabilization_analysis_for_clip, ClipStabilizationCapture};

use super::AppState;

const MAX_ANALYSIS_FRAMES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClipStabilizationStatus {
    Required,
    Saved,
    Analyzing { completed: usize, total: usize },
    Canceling,
    Canceled,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    clip: ClipId,
    effect: EffectId,
}

struct AnalysisJob {
    session: AuthoringSessionId,
    sequence: SequenceId,
    frame_rate: Rational,
    source_color: PreviewSourceColorContract,
    target: Target,
    cancellation: ExecutionCancellationToken,
    progress: Arc<AtomicUsize>,
    completed: usize,
    total: usize,
    result: Receiver<std::result::Result<StabilizationRecord, String>>,
}

#[derive(Default)]
pub(super) struct ClipStabilizationService {
    active: Option<AnalysisJob>,
    last: Option<(Target, ClipStabilizationStatus)>,
}

impl ClipStabilizationService {
    pub(super) fn cancel_all(&mut self) {
        if let Some(job) = &self.active {
            job.cancellation.cancel();
        }
        self.last = None;
    }
}

impl Drop for ClipStabilizationService {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

fn failure(reason: impl ToString) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "clip_stabilization".to_owned(),
        reason: reason.to_string(),
    }
}

impl AppState {
    pub(crate) fn can_analyze_clip_stabilization(&self) -> bool {
        !self.is_playing() && self.clip_stabilization.active.is_none()
    }

    pub(crate) fn clip_stabilization_status(
        &self,
        clip: ClipId,
        effect: EffectId,
        has_analysis: bool,
    ) -> ClipStabilizationStatus {
        let target = Target { clip, effect };
        if let Some(job) =
            self.clip_stabilization.active.as_ref().filter(|job| {
                job.target == target && self.authoring_session_id() == Some(job.session)
            })
        {
            return if job.cancellation.is_canceled() {
                ClipStabilizationStatus::Canceling
            } else {
                ClipStabilizationStatus::Analyzing { completed: job.completed, total: job.total }
            };
        }
        if let Some((_, status)) =
            self.clip_stabilization.last.as_ref().filter(|(owner, _)| *owner == target)
        {
            return status.clone();
        }
        if has_analysis {
            ClipStabilizationStatus::Saved
        } else {
            ClipStabilizationStatus::Required
        }
    }

    pub(super) fn start_clip_stabilization(
        &mut self,
        clip_id: ClipId,
        effect_id: EffectId,
    ) -> Result<()> {
        if !self.can_analyze_clip_stabilization() {
            return Err(failure(
                "stop playback or wait for the current stabilization analysis",
            ));
        }
        let session = self.authoring_session_id().ok_or_else(|| failure("Project is closed"))?;
        let sequence = self.active_sequence().ok_or_else(|| failure("no active Sequence"))?;
        let clip = sequence
            .video_tracks
            .iter()
            .filter(|track| !track.is_locked)
            .flat_map(|track| &track.clips)
            .find(|clip| clip.id == clip_id)
            .ok_or_else(|| failure("video Clip is locked or no longer exists"))?;
        if !clip
            .effects
            .iter()
            .any(|effect| effect.id == effect_id && effect.effect_type == EffectType::Stabilizer)
        {
            return Err(failure("Stabilizer no longer belongs to the Clip"));
        }
        let input = sequence
            .settings
            .root_program_color_context(self.project_color_environment())
            .map_err(failure)?
            .media_input(sequence.settings.color.input.auto_tone_map_media);
        let (path, source, color) = self.tracking_media_source(clip, &input).map_err(failure)?;
        let capture = ClipStabilizationCapture::new(
            clip,
            source,
            sequence.settings.frame_rate,
            MaskTrackingSettings::default(),
            MAX_ANALYSIS_FRAMES,
        )
        .map_err(failure)?;
        let (sequence_id, frame_rate) = (sequence.id, sequence.settings.frame_rate);
        let total = capture.len();
        let cancellation = ExecutionCancellationToken::new();
        let progress = Arc::new(AtomicUsize::new(0));
        let worker_cancellation = cancellation.clone();
        let worker_progress = progress.clone();
        let (send, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("mondrian-clip-stabilization".to_owned())
            .spawn(move || {
                let analysis = capture
                    .analyze_media(
                        &path,
                        color,
                        &mut PreviewDecodeSessionContext::new(),
                        &worker_cancellation,
                        |completed, _| worker_progress.store(completed, Ordering::Relaxed),
                    )
                    .map_err(|error| error.to_string());
                let _ = send.send(analysis);
            })
            .map_err(failure)?;
        self.clip_stabilization.last = None;
        self.clip_stabilization.active = Some(AnalysisJob {
            session,
            sequence: sequence_id,
            frame_rate,
            source_color: color,
            target: Target { clip: clip_id, effect: effect_id },
            cancellation,
            progress,
            completed: 0,
            total,
            result,
        });
        Ok(())
    }

    pub(super) fn cancel_clip_stabilization(
        &mut self,
        clip: ClipId,
        effect: EffectId,
    ) -> Result<()> {
        let target = Target { clip, effect };
        let session = self.authoring_session_id();
        let job = self
            .clip_stabilization
            .active
            .as_ref()
            .filter(|job| {
                job.target == target
                    && session == Some(job.session)
                    && !job.cancellation.is_canceled()
            })
            .ok_or_else(|| failure("the Stabilizer has no active analysis"))?;
        job.cancellation.cancel();
        Ok(())
    }

    pub(crate) fn poll_clip_stabilization(&mut self) -> bool {
        let Some(job) = &mut self.clip_stabilization.active else {
            return false;
        };
        let result = match job.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => {
                Err("Stabilizer analysis worker terminated".to_owned())
            }
            Err(TryRecvError::Empty) => {
                let completed = job.progress.load(Ordering::Relaxed).min(job.total);
                return std::mem::replace(&mut job.completed, completed) != completed;
            }
        };
        let job = self.clip_stabilization.active.take().expect("polled active analysis");
        if self.authoring_session_id() != Some(job.session) {
            return true;
        }
        let status = if job.cancellation.is_canceled() {
            Some(ClipStabilizationStatus::Canceled)
        } else {
            match result.and_then(|record| {
                self.publish_clip_stabilization(&job, record).map_err(|error| error.to_string())
            }) {
                Ok(()) => None,
                Err(error) => Some(ClipStabilizationStatus::Failed(error)),
            }
        };
        self.clip_stabilization.last = status.map(|status| (job.target, status));
        true
    }

    fn publish_clip_stabilization(
        &mut self,
        job: &AnalysisJob,
        record: StabilizationRecord,
    ) -> Result<()> {
        let sequence = self
            .sequences()
            .iter()
            .find(|sequence| sequence.id == job.sequence)
            .ok_or_else(|| failure("Sequence was removed during analysis"))?;
        if sequence.settings.frame_rate != job.frame_rate {
            return Err(failure(
                "Sequence frame rate changed during analysis. Analyze the Clip again.",
            ));
        }
        let clip = sequence
            .video_tracks
            .iter()
            .filter(|track| !track.is_locked)
            .flat_map(|track| &track.clips)
            .find(|clip| clip.id == job.target.clip)
            .ok_or_else(|| failure("video Clip is locked or was removed during analysis"))?;
        let input = sequence
            .settings
            .root_program_color_context(self.project_color_environment())
            .map_err(failure)?
            .media_input(sequence.settings.color.input.auto_tone_map_media);
        let (path, source, color) = self.tracking_media_source(clip, &input).map_err(failure)?;
        if color != job.source_color {
            return Err(failure(
                "source color interpretation changed during analysis",
            ));
        }
        if mondrian_core::MediaFileFingerprint::capture(&path) != source.media {
            return Err(failure("source file changed during analysis"));
        }
        stabilization_analysis_for_clip(&record, clip, source, MaskTrackingSettings::default())
            .map_err(failure)?;
        self.commit_sequence_edit(job.sequence, "应用画面稳定分析", |sequence| {
            let effect = sequence
                .video_tracks
                .iter_mut()
                .filter(|track| !track.is_locked)
                .flat_map(|track| &mut track.clips)
                .find(|clip| clip.id == job.target.clip)
                .and_then(|clip| {
                    clip.effects.iter_mut().find(|effect| {
                        effect.id == job.target.effect
                            && effect.effect_type == EffectType::Stabilizer
                    })
                })
                .ok_or_else(|| failure("Stabilizer was removed during analysis"))?;
            record.store_in_effect(effect).map_err(failure)
        })
    }
}
