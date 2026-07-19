//! CPU working-space composition and final raster output adaptation.
//!
//! This Module is the sole CPU execution path from a resolved Viewer plan to
//! either a working-linear frame or an encoded UI raster. Timeline traversal,
//! decode scheduling, GPU planning, and final presentation arbitration remain
//! outside it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mondrian_core::types::{BlendMode, ColorSpace};
use mondrian_effects::CompiledEffectGraph;
use mondrian_renderer::{
    composite_timeline_elements_color_frame_with_diagnostics,
    execute_cpu_program_monitor_boundary_rgba8, CpuColorFrame, RenderColorStageDiagnostics,
    RenderColorTransformDiagnostics, RenderMonitorAdaptation, RenderOutputColorBoundary,
    TimelineAdjustmentLayer, TimelineCompositeDiagnostics, TimelineCompositeElement,
    TimelineCompositeOptions, TimelineCompositeScratch, TimelineEffectColorRuntime,
    TimelineMediaLayer, TimelineSolidColorLayer,
};
use mondrian_timeline::sequence::ColorContext;

use super::preview_viewer_plan::ResolvedPreviewElement;

/// CPU Preview execution timings independent of any diagnostics projection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewCpuExecutionDurations {
    pub(crate) working_prepare_us: u64,
    pub(crate) cpu_composite_us: u64,
    pub(crate) cpu_output_boundary_us: u64,
}

pub(crate) struct PreviewCompositeOutput {
    pub(crate) rgba: Vec<u8>,
    pub(crate) composite_diagnostics: TimelineCompositeDiagnostics,
    pub(crate) input_color_diagnostics: Vec<RenderColorTransformDiagnostics>,
    pub(crate) input_color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(crate) color_diagnostics: RenderColorTransformDiagnostics,
    pub(crate) monitor_color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(crate) color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(crate) execution_durations: PreviewCpuExecutionDurations,
}

pub(crate) struct PreviewWorkingCompositeOutput {
    pub(crate) frame: CpuColorFrame,
    pub(crate) composite_diagnostics: TimelineCompositeDiagnostics,
    pub(crate) input_color_diagnostics: Vec<RenderColorTransformDiagnostics>,
    pub(crate) input_color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(crate) execution_durations: PreviewCpuExecutionDurations,
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

pub(crate) fn composite_resolved_preview_working(
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
    let working_prepare_us = duration_us(working_prepare_started_at.elapsed());
    let cpu_composite_started_at = Instant::now();
    let composite = composite_timeline_elements_color_frame_with_diagnostics(
        width,
        height,
        &elements,
        TimelineCompositeOptions::default(),
        TimelineEffectColorRuntime::new(&color_context.engine, color_context.working_color_space),
        scratch,
    );
    let cpu_composite_us = duration_us(cpu_composite_started_at.elapsed());
    Ok(PreviewWorkingCompositeOutput {
        frame: composite.frame,
        composite_diagnostics: composite.diagnostics,
        input_color_diagnostics,
        input_color_stage_diagnostics,
        execution_durations: PreviewCpuExecutionDurations {
            working_prepare_us,
            cpu_composite_us,
            ..PreviewCpuExecutionDurations::default()
        },
    })
}

pub(crate) fn output_boundary_from_color_context(
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

pub(crate) fn composite_resolved_preview(
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    color_context: &ColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewCompositeOutput, String> {
    let composite =
        composite_resolved_preview_working(width, height, resolved, color_context, scratch)?;
    let output_boundary_started_at = Instant::now();
    let mut execution_durations = composite.execution_durations;
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
            execution_durations.cpu_output_boundary_us =
                duration_us(output_boundary_started_at.elapsed());
            PreviewCompositeOutput {
                rgba: output.rgba,
                composite_diagnostics: composite.composite_diagnostics,
                input_color_diagnostics: composite.input_color_diagnostics,
                input_color_stage_diagnostics: composite.input_color_stage_diagnostics,
                color_diagnostics: output.program_output.color_diagnostics,
                monitor_color_diagnostics: output.monitor_color_diagnostics,
                color_stage_diagnostics: output.stage_diagnostics,
                execution_durations,
            }
        })
        .map_err(|err| format!("viewer preview final color transform failed: {err}"))
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}
