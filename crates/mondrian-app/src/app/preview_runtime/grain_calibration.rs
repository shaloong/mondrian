//! Fixed source measurements for Preview, before temporal demands are expanded.

use std::collections::VecDeque;
use std::sync::Arc;

use mondrian_core::{ExecutionCancellationToken, PictureInterpretationOverrides};
use mondrian_effects::grain_calibration::{
    calibrate_grain, GrainCalibrationObservation, GrainCalibrationRequest,
    GrainCalibrationSettings, GrainSamplePicture,
};
use mondrian_effects::CompiledEffectGraph;
use mondrian_media::{PreviewDecodeAccessMode, PreviewRepresentationQuality};
use mondrian_playback::FramePresentationQuality;

use super::{MediaPreviewKey, PreviewExecutionSnapshot, PreviewProductionRuntime};
use crate::app::exporting::export_picture_source_extent;
use crate::app::preview_access_mode::MediaPreviewRequestIntent;
use crate::app::preview_media_source::{
    resolve_preview_media_source, PreviewMediaSourceOutcome, PreviewMediaSourceRequest,
};
use crate::app::preview_timeline_execution::{
    PreviewGrainGraphBinding, PreviewTimelineAbort, PreviewTimelineMediaFrame,
    PreviewTimelinePendingDependency,
};
use crate::app::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};

const CACHE_CAPACITY: usize = 32;

#[derive(Default)]
pub(super) struct PreviewGrainCalibrationCache {
    entries: VecDeque<CalibrationEntry>,
    revision: u64,
}

#[derive(Clone, Copy)]
pub(super) enum PreviewGrainBindingUse<'a> {
    Materialize(&'a ExecutionCancellationToken),
    PrefetchPlan,
}

struct CalibrationEntry {
    source: MediaPreviewKey,
    settings: GrainCalibrationSettings,
    observation: GrainCalibrationObservation,
}

pub(super) fn grain_source_is_current(
    snapshot: &PreviewExecutionSnapshot<'_>,
    key: &MediaPreviewKey,
) -> bool {
    let source = key.decode.source();
    snapshot
        .authoring()
        .and_then(|authoring| authoring.asset_library().get_asset(key.asset_id).ok().flatten())
        .is_some_and(|asset| {
            asset.source_fingerprint() == Some(source.fingerprint())
                && asset
                    .media_probe()
                    .and_then(|probe| probe.primary_video())
                    .is_some_and(|stream| stream.index == source.video_stream_index())
                && mondrian_media::MediaFileFingerprint::capture(source.path())
                    == source.fingerprint()
        })
}

impl<O: Clone> PreviewProductionRuntime<O> {
    pub(super) fn bind_preview_grain_graph(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        binding: PreviewGrainGraphBinding<'_>,
        graph: Arc<CompiledEffectGraph>,
        binding_use: PreviewGrainBindingUse<'_>,
        mut observe_key: impl FnMut(&MediaPreviewKey, bool),
    ) -> Result<Arc<CompiledEffectGraph>, PreviewTimelineAbort> {
        let media = binding.media;
        let blocked = |detail: String| {
            PreviewUnavailability::blocked(PreviewOutputStage::MediaResolution, detail)
        };
        let authoring = snapshot.authoring().ok_or_else(|| {
            blocked("source grain calibration requires the project Asset Library".into())
        })?;
        let asset = authoring
            .asset_library()
            .get_asset(media.asset_id)
            .map_err(|error| blocked(error.to_string()))?
            .ok_or_else(|| blocked(format!("grain source Asset {} is missing", media.asset_id)))?;
        let stream =
            asset.media_probe().and_then(|probe| probe.primary_video()).ok_or_else(|| {
                blocked(format!(
                    "grain source Asset {} has no admitted picture stream",
                    media.asset_id
                ))
            })?;
        let available =
            export_picture_source_extent(&asset.kind, stream.duration, stream.total_frames)
                .ok_or_else(|| {
                    blocked(format!(
                        "grain source Asset {} has no proven extent",
                        media.asset_id
                    ))
                })?;
        let input_color = binding.color_context.media_input(media.auto_tone_map);
        let mut policy = self.media_source_work_policy(snapshot);
        if matches!(policy.intent, MediaPreviewRequestIntent::Current(_))
            && policy.access_mode == PreviewDecodeAccessMode::ScrubCursor
        {
            // Interactive calibration uses exact still decode. Playback stays
            // in its admitted residency family with the same presentation deadline.
            policy.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;
        }
        let mut interruption = None;
        let result = graph.bind_grain_calibration_from_media(
            |request: GrainCalibrationRequest| {
                let Some(source_sample) = media
                    .fixed_source_frame(request.frame_index(), stream, available)
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(None);
                };
                let source = resolve_preview_media_source(PreviewMediaSourceRequest {
                    asset: &asset,
                    color_space_override: media.color_space_override,
                    alpha_interpretation: media.alpha_interpretation,
                    picture_overrides: PictureInterpretationOverrides {
                        pixel_aspect_ratio: media.pixel_aspect_ratio_override,
                        field_order: media.field_order_override,
                    },
                    source_sample,
                    input_color: &input_color,
                    prefer_proxy: false,
                    request_missing_proxy_generation: false,
                    proxy_config: authoring.proxy().config(),
                    proxy_color: None,
                    hardware_admission: self.hardware_decode_admission.get(),
                    cpu_working_required: true,
                    representation_quality: PreviewRepresentationQuality::Full,
                });
                let key = match source {
                    PreviewMediaSourceOutcome::Ready(source) => source.key,
                    PreviewMediaSourceOutcome::Unavailable(source) => {
                        return Err(source.reason.to_string());
                    }
                    PreviewMediaSourceOutcome::ColorRejected(_) => {
                        return Err("grain source color interpretation was rejected".into());
                    }
                };
                let settings = request.settings();
                let cached = {
                    let mut cache = self.grain_calibration.borrow_mut();
                    cache.lookup(&key, settings)
                };
                if let Some(observation) = cached {
                    if matches!(binding_use, PreviewGrainBindingUse::Materialize(_)) {
                        observe_key(&key, false);
                    }
                    return Ok(Some(observation));
                }
                let PreviewGrainBindingUse::Materialize(cancellation) = binding_use else {
                    // Lookahead warms the exact media key. Only a real
                    // evaluation can authorize sample statistics or pixels.
                    observe_key(&key, false);
                    return Ok(None);
                };
                let frame = self.media_frame_for_frozen_key(key.clone(), policy, true);
                observe_key(
                    &key,
                    matches!(&frame, PreviewTimelineMediaFrame::Pending { .. }),
                );
                let frame = match frame {
                    PreviewTimelineMediaFrame::Ready(frame) => frame,
                    PreviewTimelineMediaFrame::Pending { wait } => {
                        interruption = Some(PreviewTimelineAbort::Pending {
                            dependency: PreviewTimelinePendingDependency::Media {
                                asset_id: media.asset_id,
                                wait,
                            },
                        });
                        return Err("fixed grain source is pending".into());
                    }
                    PreviewTimelineMediaFrame::Unavailable { reason } => {
                        interruption = Some(reason.into());
                        return Err("fixed grain source is unavailable".into());
                    }
                };
                if frame.presentation_quality() == FramePresentationQuality::Degraded {
                    return Err("fixed grain decode returned an approximate sample".into());
                }
                if frame.source_key() != Some(&key) {
                    return Err("fixed grain decode returned a different source identity".into());
                }
                let pixels = frame
                    .working_frame_with_session(binding.color_session)
                    .map_err(|error| error.to_string())?;
                let view = mondrian_renderer::timeline_auxiliary_frame_view(&pixels.frame);
                if view.extent().width() != stream.width || view.extent().height() != stream.height
                {
                    return Err("fixed grain decode did not retain the original source grid".into());
                }
                let profile = calibrate_grain(view, settings, cancellation)
                    .map_err(|error| error.to_string())?
                    .map(Arc::new);
                let source = key.decode.source();
                let observation = GrainCalibrationObservation {
                    picture: GrainSamplePicture {
                        target: source_sample,
                        fingerprint: source.fingerprint(),
                        video_stream_index: source.video_stream_index(),
                        extent: view.extent(),
                        sampling: view.sampling(),
                    },
                    profile,
                };
                self.grain_calibration.borrow_mut().insert(key, settings, observation.clone());
                Ok(Some(observation))
            },
            |offset| {
                let neighbor =
                    binding.clip_time.checked_add(offset).map_err(|error| error.to_string())?;
                binding
                    .program
                    .admits_media_grain_neighbor(media.placement, neighbor, available)
                    .map_err(|error| error.to_string())
            },
        );
        result.map_err(|reason| interruption.unwrap_or_else(|| blocked(reason).into()))
    }
}

impl PreviewGrainCalibrationCache {
    pub(super) const fn revision(&self) -> u64 {
        self.revision
    }

    #[cfg(test)]
    pub(super) fn only_observation(&self) -> (&MediaPreviewKey, &GrainCalibrationObservation) {
        assert_eq!(self.entries.len(), 1);
        let entry = self.entries.front().unwrap();
        (&entry.source, &entry.observation)
    }

    fn lookup(
        &mut self,
        key: &MediaPreviewKey,
        settings: GrainCalibrationSettings,
    ) -> Option<GrainCalibrationObservation> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.source == *key && entry.settings == settings)?;
        let entry = self.entries.remove(index).expect("known calibration index");
        let observation = entry.observation.clone();
        self.entries.push_back(entry);
        Some(observation)
    }

    fn insert(
        &mut self,
        source: MediaPreviewKey,
        settings: GrainCalibrationSettings,
        observation: GrainCalibrationObservation,
    ) {
        if self.entries.len() == CACHE_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(CalibrationEntry { source, settings, observation });
        self.revision = self.revision.saturating_add(1);
    }
}
