//! CPU working-space composition and final raster output adaptation.
//!
//! This Module is the sole CPU execution path from a resolved Viewer plan to
//! either a working-linear frame or an encoded UI raster. Timeline traversal,
//! decode scheduling, GPU planning, and final presentation arbitration remain
//! outside it.

use super::*;

pub(super) struct PreviewCompositeOutput {
    pub(super) rgba: Vec<u8>,
    pub(super) composite_diagnostics: TimelineCompositeDiagnostics,
    pub(super) color_diagnostics: RenderColorTransformDiagnostics,
    pub(super) monitor_color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(super) color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(super) render_stage_durations: AppUiPreviewRenderStageDurations,
}

pub(super) struct PreviewWorkingCompositeOutput {
    pub(super) frame: CpuColorFrame,
    pub(super) composite_diagnostics: TimelineCompositeDiagnostics,
    pub(super) input_color_diagnostics: Vec<RenderColorTransformDiagnostics>,
    pub(super) input_color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(super) render_stage_durations: AppUiPreviewRenderStageDurations,
}

enum PreviewWorkingElement {
    SolidColor(TimelineSolidColorLayer),
    Adjustment(TimelineAdjustmentLayer),
    Media {
        frame_index: usize,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: Arc<CompiledEffectGraph>,
        frame_seed: i64,
    },
}

pub(super) fn composite_resolved_preview_working(
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    color_context: &ColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewWorkingCompositeOutput, String> {
    let working_prepare_started_at = Instant::now();
    let mut working_frames = Vec::new();
    let mut working_elements = Vec::with_capacity(resolved.len());
    let mut input_color_diagnostics = Vec::new();
    let mut input_color_stage_diagnostics = RenderColorStageDiagnostics::default();

    for element in resolved {
        match element {
            ResolvedPreviewElement::SolidColor(layer) => {
                working_elements.push(PreviewWorkingElement::SolidColor(layer.clone()));
            }
            ResolvedPreviewElement::Adjustment(layer) => {
                working_elements.push(PreviewWorkingElement::Adjustment(layer.clone()));
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => {
                let working = frame.working_frame()?;
                if let Some(diagnostics) = working.color_diagnostics {
                    input_color_diagnostics.push(diagnostics);
                }
                input_color_stage_diagnostics.accumulate(working.stage_diagnostics);
                let frame_index = working_frames.len();
                working_frames.push(working.frame);
                working_elements.push(PreviewWorkingElement::Media {
                    frame_index,
                    opacity: *opacity,
                    blend_mode: *blend_mode,
                    transform: *transform,
                    effect_graph: Arc::clone(effect_graph),
                    frame_seed: *frame_seed,
                });
            }
        }
    }

    let elements: Vec<_> = working_elements
        .iter()
        .map(|element| match element {
            PreviewWorkingElement::SolidColor(layer) => {
                TimelineCompositeElement::SolidColor(layer.clone())
            }
            PreviewWorkingElement::Adjustment(layer) => {
                TimelineCompositeElement::Adjustment(layer.clone())
            }
            PreviewWorkingElement::Media {
                frame_index,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &working_frames[*frame_index],
                opacity: *opacity,
                blend_mode: *blend_mode,
                transform: *transform,
                effect_graph: Arc::clone(effect_graph),
                frame_seed: *frame_seed,
            }),
        })
        .collect();
    let working_prepare_us = app_duration_us(working_prepare_started_at.elapsed());
    let cpu_composite_started_at = Instant::now();
    let composite = composite_timeline_elements_color_frame_with_diagnostics(
        width,
        height,
        &elements,
        TimelineCompositeOptions::default(),
        TimelineEffectColorRuntime::new(&color_context.engine, color_context.working_color_space),
        scratch,
    );
    let cpu_composite_us = app_duration_us(cpu_composite_started_at.elapsed());
    Ok(PreviewWorkingCompositeOutput {
        frame: composite.frame,
        composite_diagnostics: composite.diagnostics,
        input_color_diagnostics,
        input_color_stage_diagnostics,
        render_stage_durations: AppUiPreviewRenderStageDurations {
            working_prepare_us,
            cpu_composite_us,
            ..AppUiPreviewRenderStageDurations::default()
        },
    })
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CpuRasterPresentationContract {
    pub(super) raster_color_space: mondrian_ui_core::RasterImageColorSpace,
}

pub(super) fn cpu_raster_presentation_contract(
    requested: &ColorContext,
) -> Result<CpuRasterPresentationContract, String> {
    let program_output = requested.output_color_space.color().ok_or_else(|| {
        format!(
            "Program Output {:?} is not an encoded color identity",
            requested.output_color_space
        )
    })?;
    RenderMonitorAdaptation::new(program_output, ColorSpace::Srgb, requested.engine.clone())
        .map_err(|error| error.to_string())?;
    Ok(CpuRasterPresentationContract {
        raster_color_space: mondrian_ui_core::RasterImageColorSpace::Srgb,
    })
}

pub(super) fn output_boundary_from_color_context(
    color_context: &ColorContext,
) -> Result<RenderOutputColorBoundary, String> {
    let output_color_space = color_context.output_color_space.color().ok_or_else(|| {
        format!(
            "preview output identity {:?} is not an encoded color space",
            color_context.output_color_space
        )
    })?;
    RenderOutputColorBoundary::from_intent(
        mondrian_renderer::RenderOutputColorBoundaryTarget::Display,
        output_color_space,
        &color_context.output_transform,
        color_context.tone_map,
        color_context.engine.clone(),
    )
    .map_err(|error| error.to_string())
}

pub(super) fn composite_resolved_preview(
    service: &AppUiPreviewService,
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    color_context: &ColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewCompositeOutput, String> {
    let composite =
        composite_resolved_preview_working(width, height, resolved, color_context, scratch)?;
    for diagnostics in composite.input_color_diagnostics {
        service.record_color_transform(diagnostics);
    }
    if composite.input_color_stage_diagnostics != RenderColorStageDiagnostics::default() {
        service.record_color_stage(composite.input_color_stage_diagnostics);
    }
    if composite.composite_diagnostics.legacy_rgba8_composites > 0 {
        use crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker;
        service.record_preview_gpu_output_blocker(
            &PreviewGpuOutputBlocker::LegacyRgba8CompositeBoundary {
                legacy_composites: composite.composite_diagnostics.legacy_rgba8_composites,
            },
        );
    }
    let output_boundary_started_at = Instant::now();
    let mut render_stage_durations = composite.render_stage_durations;
    let boundary = output_boundary_from_color_context(color_context)
        .map_err(|error| format!("unsupported preview output transform: {error}"))?;
    let program_output = boundary.output_color_space;
    let adaptation = RenderMonitorAdaptation::new(
        program_output,
        ColorSpace::Srgb,
        color_context.engine.clone(),
    )
    .map_err(|error| format!("unsupported CPU raster monitor adaptation: {error}"))?;
    execute_cpu_program_monitor_boundary_rgba8(&composite.frame, &boundary, &adaptation)
        .map(|output| {
            render_stage_durations.cpu_output_boundary_us =
                app_duration_us(output_boundary_started_at.elapsed());
            PreviewCompositeOutput {
                rgba: output.rgba,
                composite_diagnostics: composite.composite_diagnostics,
                color_diagnostics: output.program_output.color_diagnostics,
                monitor_color_diagnostics: output.monitor_color_diagnostics,
                color_stage_diagnostics: output.stage_diagnostics,
                render_stage_durations,
            }
        })
        .map_err(|err| format!("viewer preview final color transform failed: {err}"))
}
