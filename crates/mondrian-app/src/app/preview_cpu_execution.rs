//! CPU working-space composition and final raster output adaptation.
//!
//! This Module is the sole CPU execution path from a resolved Viewer plan to
//! either a working-linear frame or an encoded UI raster. Timeline traversal,
//! decode scheduling, GPU planning, and final presentation arbitration remain
//! outside it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mondrian_core::types::{BlendMode, ColorSpace};
use mondrian_core::{OcioColorSpaceIdentity, OutputTransformIntentResolutionError};
use mondrian_effects::{
    CompiledEffectGraph, EffectExecutionError, EffectFloatExecutionError,
    EffectFloatUnsupportedReason,
};
use mondrian_renderer::{
    composite_timeline_elements_color_frame_with_diagnostics, CpuColorFrame,
    RenderColorStageDiagnostics, RenderColorTransformDiagnostics, RenderColorTransformError,
    RenderMonitorAdaptation, RenderMonitorAdaptationError, RenderOutputColorBoundary,
    TimelineAdjustmentLayer, TimelineCompositeDiagnostics, TimelineCompositeElement,
    TimelineCompositeError, TimelineCompositeOptions, TimelineCompositeScratch,
    TimelineCrossDissolveLayer, TimelineEffectColorRuntime, TimelineMediaLayer,
    TimelineSolidColorLayer, TimelineTransitionInput,
};
use mondrian_timeline::sequence::ProgramColorContext;

use super::preview_media_frame::MediaPreviewWorkingFrameError;
use super::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use super::preview_viewer_plan::{ResolvedPreviewElement, ResolvedPreviewTransitionInput};

/// Typed failure from the shared CPU Preview execution path.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum PreviewCpuExecutionError {
    #[error(transparent)]
    WorkingFrame(#[from] MediaPreviewWorkingFrameError),
    #[error("Timeline composition failed: {0}")]
    TimelineComposite(#[from] TimelineCompositeError),
    #[error("Preview Program Output {identity:?} is not an encoded color identity")]
    ProgramOutputIdentity { identity: OcioColorSpaceIdentity },
    #[error("Preview Program Output transform is blocked: {0}")]
    ProgramOutputTransform(#[from] OutputTransformIntentResolutionError),
    #[error("Preview monitor adaptation is blocked: {0}")]
    MonitorAdaptation(#[from] RenderMonitorAdaptationError),
    #[error("Preview final color execution failed: {source}")]
    FinalColorTransform {
        #[source]
        source: std::sync::Arc<RenderColorTransformError>,
    },
}

impl PreviewCpuExecutionError {
    /// Project this execution error into the shared terminal Preview contract.
    pub(crate) fn unavailability(&self) -> PreviewUnavailability {
        match self {
            Self::WorkingFrame(MediaPreviewWorkingFrameError::NativeSurfaceRequiresGpu {
                ..
            })
            | Self::WorkingFrame(MediaPreviewWorkingFrameError::CpuYuvRequiresGpu) => {
                PreviewUnavailability::blocked(
                    PreviewOutputStage::InputAdaptation,
                    self.to_string(),
                )
            }
            Self::WorkingFrame(MediaPreviewWorkingFrameError::InputColorTransform { .. }) => {
                PreviewUnavailability::failed(PreviewOutputStage::InputAdaptation, self.to_string())
            }
            Self::TimelineComposite(error) if timeline_composite_is_blocked(error) => {
                PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineComposite,
                    self.to_string(),
                )
            }
            Self::TimelineComposite(_) => PreviewUnavailability::failed(
                PreviewOutputStage::TimelineComposite,
                self.to_string(),
            ),
            Self::ProgramOutputIdentity { .. } | Self::ProgramOutputTransform(_) => {
                PreviewUnavailability::blocked(PreviewOutputStage::ProgramOutput, self.to_string())
            }
            Self::MonitorAdaptation(_) => PreviewUnavailability::blocked(
                PreviewOutputStage::MonitorAdaptation,
                self.to_string(),
            ),
            Self::FinalColorTransform { .. } => {
                PreviewUnavailability::failed(PreviewOutputStage::ProgramOutput, self.to_string())
            }
        }
    }
}

fn timeline_composite_is_blocked(error: &TimelineCompositeError) -> bool {
    match error {
        TimelineCompositeError::EffectDomainBlocked { .. } => true,
        TimelineCompositeError::LegacyRgba8WorkingCompositeForbidden { .. } => true,
        TimelineCompositeError::EncodedEffect(error) => matches!(
            error,
            EffectExecutionError::ColorDomainConversionRequired { .. }
                | EffectExecutionError::ColorDomainBlocked { .. }
                | EffectExecutionError::InvalidColorDomainPlan
                | EffectExecutionError::CustomProcessorUnavailable { .. }
        ),
        TimelineCompositeError::FloatEffect {
            reason: EffectFloatExecutionError::UnsupportedNode { reason, .. },
        } => !matches!(
            reason,
            EffectFloatUnsupportedReason::ColorDomainTransitionFailed { .. }
        ),
        TimelineCompositeError::FloatEffect {
            reason: EffectFloatExecutionError::ExecutionContract(_),
        } => true,
        TimelineCompositeError::FloatEffect {
            reason: EffectFloatExecutionError::MaskRasterFailed { .. },
        } => true,
        TimelineCompositeError::FloatEffect {
            reason:
                EffectFloatExecutionError::InputSizeMismatch { .. }
                | EffectFloatExecutionError::MissingOutput { .. },
        } => false,
        TimelineCompositeError::MediaFrameContractMismatch { .. }
        | TimelineCompositeError::MediaFrameStorageLengthMismatch { .. } => false,
        TimelineCompositeError::CpuWorkingSet(_) => false,
    }
}

/// CPU Preview execution timings independent of any diagnostics projection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewCpuExecutionDurations {
    pub(crate) working_prepare_us: u64,
    pub(crate) cpu_composite_us: u64,
    pub(crate) cpu_output_boundary_us: u64,
}

pub(crate) struct PreviewCompositeOutput {
    pub(crate) rgba: Vec<u8>,
    /// Exact post-effect/post-composite working frame before display adaptation.
    pub(crate) working_frame: CpuColorFrame,
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
    CrossDissolve {
        left: PreviewWorkingTransitionInput,
        right: PreviewWorkingTransitionInput,
        progress: f32,
    },
}

enum PreviewWorkingTransitionInput {
    Transparent,
    SolidColor(TimelineSolidColorLayer),
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
    color_context: &ProgramColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewWorkingCompositeOutput, PreviewCpuExecutionError> {
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
            ResolvedPreviewElement::HeterogeneousSolidColor { layer, .. } => {
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
                ..
            } => {
                let working = frame.working_frame_with_session(scratch.color_execution_mut())?;
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
            ResolvedPreviewElement::CrossDissolve { left, right, progress } => {
                let left = prepare_transition_input(
                    left,
                    &mut working_frames,
                    &mut input_color_diagnostics,
                    &mut input_color_stage_diagnostics,
                    scratch.color_execution_mut(),
                )?;
                let right = prepare_transition_input(
                    right,
                    &mut working_frames,
                    &mut input_color_diagnostics,
                    &mut input_color_stage_diagnostics,
                    scratch.color_execution_mut(),
                )?;
                working_elements.push(PreviewWorkingElement::CrossDissolve {
                    left,
                    right,
                    progress: *progress,
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
            PreviewWorkingElement::CrossDissolve { left, right, progress } => {
                TimelineCompositeElement::CrossDissolve(TimelineCrossDissolveLayer {
                    left: lower_transition_input(left, &working_frames),
                    right: lower_transition_input(right, &working_frames),
                    progress: *progress,
                })
            }
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
    )?;
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

fn prepare_transition_input(
    input: &ResolvedPreviewTransitionInput,
    working_frames: &mut Vec<CpuColorFrame>,
    input_color_diagnostics: &mut Vec<RenderColorTransformDiagnostics>,
    input_color_stage_diagnostics: &mut RenderColorStageDiagnostics,
    color_session: &mut mondrian_renderer::RenderCpuColorExecutionSession,
) -> Result<PreviewWorkingTransitionInput, PreviewCpuExecutionError> {
    Ok(match input {
        ResolvedPreviewTransitionInput::Transparent => PreviewWorkingTransitionInput::Transparent,
        ResolvedPreviewTransitionInput::SolidColor(solid) => {
            PreviewWorkingTransitionInput::SolidColor(solid.clone())
        }
        ResolvedPreviewTransitionInput::HeterogeneousSolidColor { layer, .. } => {
            PreviewWorkingTransitionInput::SolidColor(layer.clone())
        }
        ResolvedPreviewTransitionInput::Media {
            frame,
            opacity,
            blend_mode,
            transform,
            effect_graph,
            frame_seed,
            ..
        } => {
            let working = frame.working_frame_with_session(color_session)?;
            if let Some(diagnostics) = working.color_diagnostics {
                input_color_diagnostics.push(diagnostics);
            }
            input_color_stage_diagnostics.accumulate(working.stage_diagnostics);
            let frame_index = working_frames.len();
            working_frames.push(working.frame);
            PreviewWorkingTransitionInput::Media {
                frame_index,
                opacity: *opacity,
                blend_mode: *blend_mode,
                transform: *transform,
                effect_graph: Arc::clone(effect_graph),
                frame_seed: *frame_seed,
            }
        }
    })
}

fn lower_transition_input<'a>(
    input: &'a PreviewWorkingTransitionInput,
    frames: &'a [CpuColorFrame],
) -> TimelineTransitionInput<'a> {
    match input {
        PreviewWorkingTransitionInput::Transparent => TimelineTransitionInput::Transparent,
        PreviewWorkingTransitionInput::SolidColor(solid) => {
            TimelineTransitionInput::SolidColor(solid.clone())
        }
        PreviewWorkingTransitionInput::Media {
            frame_index,
            opacity,
            blend_mode,
            transform,
            effect_graph,
            frame_seed,
        } => TimelineTransitionInput::Media(TimelineMediaLayer {
            frame: &frames[*frame_index],
            opacity: *opacity,
            blend_mode: *blend_mode,
            transform: *transform,
            effect_graph: Arc::clone(effect_graph),
            frame_seed: *frame_seed,
        }),
    }
}

pub(crate) fn output_boundary_from_color_context(
    color_context: &ProgramColorContext,
) -> Result<RenderOutputColorBoundary, PreviewCpuExecutionError> {
    let output_color_space = color_context.output_color_space.color().ok_or({
        PreviewCpuExecutionError::ProgramOutputIdentity {
            identity: color_context.output_color_space,
        }
    })?;
    RenderOutputColorBoundary::from_intent(
        mondrian_renderer::RenderOutputColorBoundaryTarget::Display,
        output_color_space,
        &color_context.output_transform,
        color_context.output_tone_map,
        color_context.engine.clone(),
    )
    .map_err(PreviewCpuExecutionError::from)
}

pub(crate) fn composite_resolved_preview(
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    color_context: &ProgramColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewCompositeOutput, PreviewCpuExecutionError> {
    let composite =
        composite_resolved_preview_working(width, height, resolved, color_context, scratch)?;
    present_preview_working(composite, color_context, scratch)
}

/// Apply only the production Program Output/monitor boundary to an already
/// materialized working composite, including a verified Timeline cache hit.
pub(crate) fn present_preview_working(
    composite: PreviewWorkingCompositeOutput,
    color_context: &ProgramColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewCompositeOutput, PreviewCpuExecutionError> {
    let output_boundary_started_at = Instant::now();
    let mut execution_durations = composite.execution_durations;
    let boundary = output_boundary_from_color_context(color_context)?;
    let program_output = boundary.output_color_space;
    let adaptation = RenderMonitorAdaptation::new(
        program_output,
        ColorSpace::Srgb,
        color_context.engine.clone(),
    )?;
    mondrian_renderer::execute_cpu_program_monitor_presentation_rgba8_with_session(
        &composite.frame,
        &boundary,
        &adaptation,
        scratch.color_execution_mut(),
    )
    .map(|output| {
        execution_durations.cpu_output_boundary_us =
            duration_us(output_boundary_started_at.elapsed());
        PreviewCompositeOutput {
            rgba: output.rgba,
            working_frame: composite.frame,
            composite_diagnostics: composite.composite_diagnostics,
            input_color_diagnostics: composite.input_color_diagnostics,
            input_color_stage_diagnostics: composite.input_color_stage_diagnostics,
            color_diagnostics: output.program_color_diagnostics,
            monitor_color_diagnostics: output.monitor_color_diagnostics,
            color_stage_diagnostics: output.stage_diagnostics,
            execution_durations,
        }
    })
    .map_err(|source| PreviewCpuExecutionError::FinalColorTransform {
        source: std::sync::Arc::new(source),
    })
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::preview_unavailability::PreviewUnavailabilityDisposition;

    #[test]
    fn effect_domain_blocker_keeps_blocked_disposition_and_stage() {
        let error = PreviewCpuExecutionError::TimelineComposite(
            TimelineCompositeError::EffectDomainBlocked {
                media_effect: 1,
                solid_effect: 0,
                adjustment_effect: 0,
            },
        );

        let unavailable = error.unavailability();
        assert_eq!(
            unavailable.disposition(),
            PreviewUnavailabilityDisposition::Blocked
        );
        assert_eq!(unavailable.stage(), PreviewOutputStage::TimelineComposite);
        assert_eq!(unavailable.code(), "preview.blocked.timeline_composite");
        assert!(unavailable.detail().contains("effect color domain is blocked"));
    }

    #[test]
    fn forbidden_working_precision_is_a_blocked_preview_capability() {
        let error = PreviewCpuExecutionError::TimelineComposite(
            TimelineCompositeError::LegacyRgba8WorkingCompositeForbidden { effect_graphs: 1 },
        );

        let unavailable = error.unavailability();
        assert_eq!(
            unavailable.disposition(),
            PreviewUnavailabilityDisposition::Blocked
        );
        assert_eq!(unavailable.stage(), PreviewOutputStage::TimelineComposite);
        assert_eq!(unavailable.code(), "preview.blocked.timeline_composite");
        assert!(unavailable.detail().contains("Float32 working composite"));
    }

    #[test]
    fn custom_processor_failure_keeps_failed_disposition_and_stage() {
        let error = PreviewCpuExecutionError::TimelineComposite(
            TimelineCompositeError::EncodedEffect(EffectExecutionError::CustomProcessorFailed {
                key: "vendor.effect".to_owned(),
                reason: "processor panic isolated".to_owned(),
            }),
        );

        let unavailable = error.unavailability();
        assert_eq!(
            unavailable.disposition(),
            PreviewUnavailabilityDisposition::Failed
        );
        assert_eq!(unavailable.stage(), PreviewOutputStage::TimelineComposite);
        assert_eq!(unavailable.code(), "preview.failed.timeline_composite");
        assert!(unavailable.detail().contains("processor panic isolated"));
    }

    #[test]
    fn invalid_mask_raster_keeps_blocked_disposition_and_typed_detail() {
        let error =
            PreviewCpuExecutionError::TimelineComposite(TimelineCompositeError::FloatEffect {
                reason: EffectFloatExecutionError::MaskRasterFailed {
                    node_id: mondrian_effects::EffectGraphNodeId(9),
                    source: mondrian_effects::MaskRasterError::InvalidGeometry {
                        reason: "Path control points must be finite",
                    },
                },
            });

        let unavailable = error.unavailability();
        assert_eq!(
            unavailable.disposition(),
            PreviewUnavailabilityDisposition::Blocked
        );
        assert_eq!(unavailable.stage(), PreviewOutputStage::TimelineComposite);
        assert!(unavailable.detail().contains("Path control points must be finite"));
    }
}
