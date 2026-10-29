//! Source materialization borrowed by one original Export processor invocation.

use super::*;
use mondrian_effects::{CompiledEffectGraph, EffectProcessorControl, EffectTemporalExecutionError};
use mondrian_renderer::{PreparedTimelineClipInputPlan, TimelineFrameExecutionAuthority};

const MAX_SOURCE_DEMANDS: usize = 256;

pub(super) fn clip_input_plan(
    program: &Arc<PreparedVisualProgram>,
    owner: &TimelineFrameExecutionRequest,
    placement: TimelineClipExecutionRef,
    graph: &CompiledEffectGraph,
) -> Result<Option<Arc<PreparedTimelineClipInputPlan>>, String> {
    graph
        .requires_processor_input_service()
        .then(|| program.clip_processor_input_plan(placement, owner).map(Arc::new))
        .transpose()
        .map_err(|error| error.to_string())
}

/// Borrows the job's immutable dependencies and existing cache. Late decodes
/// never enter that cache; only their admitted tiles survive the source loan.
pub(super) struct ExportProcessorSources<'a> {
    pub(super) authority: TimelineFrameExecutionAuthority,
    media: &'a HashMap<AssetId, crate::preset::ExportMediaDependency>,
    color_context: &'a ProgramColorContext,
    cache: &'a HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
    input_counts: Option<&'a mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&'a mut RenderColorStageDiagnostics>,
    color: mondrian_renderer::RenderCpuColorExecutionSession,
}

impl<'a> ExportProcessorSources<'a> {
    pub(super) fn new(
        owner: &TimelineFrameExecutionRequest,
        media: &'a HashMap<AssetId, crate::preset::ExportMediaDependency>,
        color_context: &'a ProgramColorContext,
        cache: &'a HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
        input_counts: Option<&'a mut InputColorResolutionSourceCounts>,
        stage_diagnostics: Option<&'a mut RenderColorStageDiagnostics>,
    ) -> Self {
        Self {
            authority: TimelineFrameExecutionAuthority::new(
                owner.generation(),
                owner.continuity(),
                owner.cancellation().clone(),
            ),
            media,
            color_context,
            cache,
            input_counts,
            stage_diagnostics,
            // Never recursively borrow the compositor's active color Session.
            color: mondrian_renderer::RenderCpuColorExecutionSession::new(0),
        }
    }

    pub(super) fn resolve(
        &mut self,
        decode: &mut PreviewDecodeSessionContext,
        plan: &PreparedTimelineClipInputPlan,
        batch: &TimelineTemporalDemandBatch,
        current: Option<&CpuColorFrame>,
        space: WorkingColorSpace,
        admit: &mut dyn FnMut(usize) -> Result<(), EffectTemporalExecutionError>,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<PreparedTemporalFrameSet, EffectTemporalExecutionError> {
        let unavailable = |reason: String| -> EffectTemporalExecutionError {
            mondrian_effects::EffectExecutionError::InputUnavailable { reason }.into()
        };
        control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
        plan.validate_source_demands(batch)?;
        if space != self.color_context.working_color_space() {
            return Err(unavailable(
                "processor source changed the original Export working space".into(),
            ));
        }
        if batch.source_demands().len() > MAX_SOURCE_DEMANDS {
            return Err(unavailable(
                "Export processor source batch exceeds the host demand limit".into(),
            ));
        }
        let original = plan.current_source().map_err(|error| unavailable(error.to_string()))?;
        // Metadata-only preflight precedes all new decode/color allocations.
        // Retained source tiles plus one producer's peak coexist. The ceiling
        // covers decoded RGBA, alpha normalization and two float color buffers;
        // codec/SDK internals remain owned by the existing decoder context.
        let mut producer_peak = 0usize;
        for demand in batch.source_demands() {
            control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
            if matches!(demand.source, TimelineTemporalSource::SolidColor { .. })
                || (demand.source == original && current.is_some())
            {
                continue;
            }
            let (key, _) = prepare_request(self.media, self.color_context, demand, None)
                .map_err(unavailable)?;
            if self.cached(&key).map_err(unavailable)?.is_none() {
                let extent = demand.effect_request.frame_extent();
                let bytes = (extent.width() as usize)
                    .checked_mul(extent.height() as usize)
                    .and_then(|pixels| pixels.checked_mul(4 * std::mem::size_of::<[f32; 4]>()))
                    .ok_or(EffectTemporalExecutionError::SourceCoverageSizeOverflow)?;
                producer_peak = producer_peak.max(bytes);
            }
        }
        let peak = batch
            .effect_demands()
            .coverage_bytes()
            .checked_add(producer_peak)
            .ok_or(EffectTemporalExecutionError::SourceCoverageSizeOverflow)?;
        admit(peak)?;
        plan.prepare_sources(
            batch,
            space,
            &mut |demand, control| {
                control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
                if demand.source == original
                    && let Some(frame) = current
                {
                    return Ok(frame.clone());
                }
                // The second lowering uses the same immutable facts, with no
                // retained request list or duplicate pixel cache. Record actual
                // resolution counts only here, never in the preflight pass.
                let counts = self.input_counts.as_deref_mut();
                let (key, request) =
                    prepare_request(self.media, self.color_context, demand, counts)
                        .map_err(unavailable)?;
                let decoded = if let Some(decoded) = self.cached(&key).map_err(unavailable)? {
                    decoded
                } else {
                    let decoded = decode_video_layer_scaled(
                        request,
                        ExportVideoLayerDecodeExecutionContext {
                            color_session: &mut self.color,
                            decode_context: decode,
                            cancellation: batch.execution_request().cancellation(),
                            route: ExportDecodedSourceRoute::CpuWorking,
                            native_import_support: None,
                        },
                    )
                    .map_err(unavailable)?;
                    validate_decoded(&key, &decoded).map_err(unavailable)?;
                    if let Some(diagnostics) = self.stage_diagnostics.as_deref_mut() {
                        diagnostics.accumulate(decoded.stage_diagnostics);
                    }
                    decoded
                };
                control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
                decoded.cpu_frame().cloned().map_err(unavailable)
            },
            admit,
            control,
        )
    }

    fn cached(&self, key: &ExportDecodeCacheKey) -> Result<Option<Arc<DecodedVideoLayer>>, String> {
        self.cache
            .get(key)
            .map(|decoded| {
                validate_decoded(key, decoded)?;
                Ok(Arc::clone(decoded))
            })
            .transpose()
    }
}

fn validate_decoded(key: &ExportDecodeCacheKey, decoded: &DecodedVideoLayer) -> Result<(), String> {
    if decoded.route() != ExportDecodedSourceRoute::CpuWorking
        || decoded.source_fingerprint != key.source_fingerprint
        || decoded.video_stream_index != key.video_stream_index
    {
        return Err("processor source cache differs from the frozen CPU media contract".into());
    }
    Ok(())
}

fn prepare_request<'a>(
    dependencies: &'a HashMap<AssetId, crate::preset::ExportMediaDependency>,
    color: &ProgramColorContext,
    demand: &mondrian_renderer::TimelineTemporalSourceDemand,
    counts: Option<&mut InputColorResolutionSourceCounts>,
) -> Result<(ExportDecodeCacheKey, ExportVideoLayerDecodeRequest<'a>), String> {
    let TimelineTemporalSource::Media {
        asset_id,
        source_sample,
        color_space_override,
        picture_overrides,
        alpha_interpretation,
        auto_tone_map,
    } = demand.source
    else {
        return Err("dynamic nested Export source is not prepared by the original closure".into());
    };
    let identity = identity_compiled_effect_graph()
        .ok_or_else(|| "renderer could not prepare the identity Effect graph".to_owned())?;
    let media = TimelineMediaPlan {
        placement: demand.placement,
        asset_id,
        source_sample,
        color_space_override,
        pixel_aspect_ratio_override: picture_overrides.pixel_aspect_ratio,
        field_order_override: picture_overrides.field_order,
        alpha_interpretation,
        frame_rate_override: None,
        opacity: 1.0,
        blend_mode: mondrian_core::BlendMode::Normal,
        transform: [1., 0., 0., 0., 1., 0.],
        effect_graph: identity,
        frame_seed: 0,
        auto_tone_map,
    };
    let extent = demand.effect_request.frame_extent();
    prepare_export_media_decode(
        dependencies,
        &media,
        extent.width(),
        extent.height(),
        color,
        counts,
        ExportDecodedSourceRoute::CpuWorking,
    )
}
