//! Job-local fixed-media calibration. The decoded raster is transient.

use std::{collections::VecDeque, sync::Arc};

use mondrian_effects::{
    grain_calibration::{
        calibrate_grain, GrainCalibrationObservation, GrainCalibrationRequest,
        GrainCalibrationSettings, GrainSamplePicture,
    },
    CompiledEffectGraph,
};

use super::{
    decode_video_layer_scaled, prepare_export_picture_decode, AssetId, ExecutionCancellationToken,
    ExportDecodeCacheKey, ExportDecodedSourceRoute, ExportPictureDecodeSource,
    ExportVideoLayerDecodeExecutionContext, HashMap, MediaFileFingerprint,
    PreviewDecodeSessionContext, ProgramColorContext, TimelineMediaPlan, TimelineTime,
};

type Dependencies = HashMap<AssetId, crate::preset::ExportMediaDependency>;

struct Entry {
    decode: ExportDecodeCacheKey,
    settings: GrainCalibrationSettings,
    observation: GrainCalibrationObservation,
}

pub(super) struct ExportGrainCalibrationCache {
    entries: VecDeque<Entry>,
    capacity: usize,
}

pub(super) struct CalibrationSourceContext<'a> {
    pub(super) media_dependencies: &'a Dependencies,
    pub(super) color_context: &'a ProgramColorContext,
    pub(super) decode_context: &'a mut PreviewDecodeSessionContext,
    pub(super) cancellation: &'a ExecutionCancellationToken,
}

impl ExportGrainCalibrationCache {
    pub(super) fn new(effect_cache_entries: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: effect_cache_entries.min(64),
        }
    }

    pub(super) fn bind_graph(
        &mut self,
        program: &mondrian_renderer::PreparedVisualProgram,
        media: &TimelineMediaPlan,
        time: TimelineTime,
        graph: Arc<CompiledEffectGraph>,
        source: &mut CalibrationSourceContext<'_>,
        color_session: &mut mondrian_renderer::RenderCpuColorExecutionSession,
    ) -> Result<Arc<CompiledEffectGraph>, String> {
        let available = source
            .media_dependencies
            .get(&media.asset_id)
            .and_then(|dependency| dependency.picture_source_extent)
            .ok_or_else(|| format!("asset={} has no frozen grain source extent", media.asset_id))?;
        graph.bind_grain_calibration_from_media(
            |request| self.resolve(media, request, source, color_session),
            |offset| {
                let neighbor = time.checked_add(offset).map_err(|err| err.to_string())?;
                program
                    .admits_media_grain_neighbor(media.placement, neighbor, available)
                    .map_err(|err| err.to_string())
            },
        )
    }

    pub(super) fn resolve(
        &mut self,
        media: &TimelineMediaPlan,
        request: GrainCalibrationRequest,
        source: &mut CalibrationSourceContext<'_>,
        color_session: &mut mondrian_renderer::RenderCpuColorExecutionSession,
    ) -> Result<Option<GrainCalibrationObservation>, String> {
        if source.cancellation.is_canceled() {
            return Err("grain calibration canceled".to_owned());
        }
        let dependency = source.media_dependencies.get(&media.asset_id).ok_or_else(|| {
            format!(
                "asset={} has no frozen grain calibration dependency",
                media.asset_id
            )
        })?;
        let (Some(stream), Some(available), Some(resolution)) = (
            &dependency.source_video_stream,
            dependency.picture_source_extent,
            dependency.source_resolution,
        ) else {
            return Err(format!(
                "asset={} has no frozen grain source grid",
                media.asset_id
            ));
        };
        let Some(source_sample) = media
            .fixed_source_frame(request.frame_index(), stream, available)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let route = ExportDecodedSourceRoute::CpuWorking;
        let (key, decode) = prepare_export_picture_decode(
            source.media_dependencies,
            ExportPictureDecodeSource {
                asset_id: media.asset_id,
                source_sample,
                alpha_interpretation: media.alpha_interpretation,
                color_space_override: media.color_space_override,
                auto_tone_map: media.auto_tone_map,
                pixel_aspect_ratio_override: media.pixel_aspect_ratio_override,
                field_order_override: media.field_order_override,
                unit: true,
            },
            resolution,
            source.color_context,
            None,
            route,
        )?;
        let settings = request.settings();
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.decode == key && entry.settings == settings)
        {
            if MediaFileFingerprint::capture(&dependency.path) != key.source_fingerprint {
                return Err(format!(
                    "asset={} source revision changed after grain calibration",
                    media.asset_id
                ));
            }
            let entry = self.entries.remove(index).expect("known calibration index");
            let observation = entry.observation.clone();
            self.entries.push_back(entry);
            return Ok(Some(observation));
        }

        let decoded = decode_video_layer_scaled(
            decode,
            ExportVideoLayerDecodeExecutionContext {
                color_session,
                decode_context: source.decode_context,
                cancellation: source.cancellation,
                route,
                native_import_support: None,
            },
        )?;
        if decoded.source_fingerprint != key.source_fingerprint
            || decoded.video_stream_index != key.video_stream_index
        {
            return Err(format!(
                "asset={} grain decode did not prove the source revision",
                media.asset_id
            ));
        }
        let view = mondrian_renderer::timeline_auxiliary_frame_view(decoded.cpu_frame()?);
        if (view.extent().width(), view.extent().height()) != (resolution.width, resolution.height)
        {
            return Err(format!(
                "asset={} fixed grain decode changed the source grid",
                media.asset_id
            ));
        }
        let observation = GrainCalibrationObservation {
            picture: GrainSamplePicture {
                target: source_sample,
                fingerprint: decoded.source_fingerprint,
                video_stream_index: decoded.video_stream_index,
                extent: view.extent(),
                sampling: view.sampling(),
            },
            profile: calibrate_grain(view, settings, source.cancellation)
                .map_err(|error| error.to_string())?
                .map(Arc::new),
        };
        if self.capacity > 0 {
            if self.entries.len() == self.capacity {
                self.entries.pop_front();
            }
            self.entries.push_back(Entry {
                decode: key,
                settings,
                observation: observation.clone(),
            });
        }
        Ok(Some(observation))
    }
}
