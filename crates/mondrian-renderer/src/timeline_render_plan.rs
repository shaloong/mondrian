use mondrian_core::{
    timeline_data::{
        AlphaInterpretation, ClipContent, FieldOrder, FlatActiveClip, FlatVideoTransition,
        FlatVideoTransitionDefinition, FlatVisualItem, NestedColorProcessing, PixelAspectRatio,
        RenderPlanSource,
    },
    types::{AssetId, BlendMode, Color, ColorSpace, FramePosition, Rational, SequenceId},
    ColorEncodingSpec, EvaluatedBasicTitle, FrameRounding, MondrianError, Result, TimelineTime,
    WorkingColorSpace,
};
use mondrian_effects::CompiledEffectGraph;
use std::sync::Arc;

/// Why a sequence is being evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRenderIntent {
    /// Interactive viewer preview.
    Preview,
    /// Final timeline export.
    Export,
    /// Thumbnail or low-cost still generation.
    Thumbnail,
    /// Non-presentational analysis such as diagnostics or media collection.
    Analysis,
}

/// Quality target for a sequence evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRenderQuality {
    /// Favor responsiveness; frame dropping and lower-resolution work are allowed.
    Interactive,
    /// Favor approximate visual fidelity at reduced cost.
    Draft,
    /// Favor final-quality correctness.
    Final,
}

/// Color target expected by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRenderColorTarget {
    /// Output is intended for display/viewer presentation.
    Display,
    /// Output is intended for encoded export.
    Export,
    /// Output stays in timeline working space for downstream processing.
    Working,
}

/// Render settings that alter execution without changing timeline semantics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineRenderSettings {
    /// Resolution scale relative to the sequence frame size.
    pub resolution_scale: f32,
    /// Requested quality level.
    pub quality: TimelineRenderQuality,
    /// Expected color target.
    pub color_target: TimelineRenderColorTarget,
    /// Whether a scheduler may skip late frames for this request.
    pub allow_frame_drop: bool,
}

impl TimelineRenderSettings {
    /// Settings for an interactive preview request.
    pub fn preview(resolution_scale: f32) -> Self {
        Self {
            resolution_scale: normalize_resolution_scale(resolution_scale),
            quality: TimelineRenderQuality::Interactive,
            color_target: TimelineRenderColorTarget::Display,
            allow_frame_drop: true,
        }
    }

    /// Settings for final export.
    pub fn export() -> Self {
        Self {
            resolution_scale: 1.0,
            quality: TimelineRenderQuality::Final,
            color_target: TimelineRenderColorTarget::Export,
            allow_frame_drop: false,
        }
    }

    /// Settings for timeline diagnostics and non-presentational analysis.
    pub fn analysis() -> Self {
        Self {
            resolution_scale: 1.0,
            quality: TimelineRenderQuality::Final,
            color_target: TimelineRenderColorTarget::Working,
            allow_frame_drop: false,
        }
    }
}

/// A request to evaluate one sequence frame into a render plan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineEvaluationRequest {
    /// Timeline frame in the source sequence time base.
    pub timeline_frame: i64,
    /// Caller intent.
    pub intent: TimelineRenderIntent,
    /// Execution settings for the request.
    pub settings: TimelineRenderSettings,
}

impl TimelineEvaluationRequest {
    /// Build a preview evaluation request.
    pub fn preview(timeline_frame: i64, resolution_scale: f32) -> Self {
        Self {
            timeline_frame,
            intent: TimelineRenderIntent::Preview,
            settings: TimelineRenderSettings::preview(resolution_scale),
        }
    }

    /// Build an export evaluation request.
    pub fn export(timeline_frame: i64) -> Self {
        Self {
            timeline_frame,
            intent: TimelineRenderIntent::Export,
            settings: TimelineRenderSettings::export(),
        }
    }

    /// Build an analysis evaluation request.
    pub fn analysis(timeline_frame: i64) -> Self {
        Self {
            timeline_frame,
            intent: TimelineRenderIntent::Analysis,
            settings: TimelineRenderSettings::analysis(),
        }
    }
}

/// Diagnostics captured while evaluating a timeline frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimelineEvaluationDiagnostics {
    /// Ordered visual items returned by the source at the requested time.
    pub active_visual_items: usize,
    /// Render-plan elements emitted after filtering.
    pub emitted_elements: usize,
    /// Active clips skipped because effective opacity was zero.
    pub skipped_zero_opacity: usize,
    /// Active clips skipped because required render data was incomplete.
    pub skipped_unrenderable: usize,
}

/// A complete evaluation result for one sequence frame.
#[derive(Debug, Clone)]
pub struct TimelineRenderPlan {
    /// Frame requested by the caller, clamped only where execution requires it.
    pub timeline_frame: i64,
    /// Source sequence time base.
    pub time_base: Rational,
    /// Caller intent.
    pub intent: TimelineRenderIntent,
    /// Execution settings.
    pub settings: TimelineRenderSettings,
    /// Ordered render elements from bottom to top.
    pub elements: Vec<TimelineRenderPlanElement>,
    /// Evaluation diagnostics.
    pub diagnostics: TimelineEvaluationDiagnostics,
}

impl TimelineRenderPlan {
    /// Return whether the plan contains no renderable elements.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Return the number of renderable elements.
    pub fn len(&self) -> usize {
        self.elements.len()
    }
}

#[derive(Debug, Clone)]
pub struct TimelineMediaPlan {
    pub asset_id: AssetId,
    pub color_space_override: Option<ColorSpace>,
    pub pixel_aspect_ratio_override: Option<PixelAspectRatio>,
    pub field_order_override: Option<FieldOrder>,
    pub alpha_interpretation: AlphaInterpretation,
    /// Exact source-domain decode target after any explicit interpretation grid.
    pub source_time: TimelineTime,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
    /// Whether to auto tone-map media to working color space.
    pub auto_tone_map: bool,
}

#[derive(Debug, Clone)]
pub struct TimelineAdjustmentPlan {
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineSolidColorPlan {
    pub color: Color,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

/// Evaluated sequence-local Basic Title source.
#[derive(Debug, Clone)]
pub struct TimelineBasicTitlePlan {
    /// Typed title semantics evaluated at the Clip's exact visual author time.
    pub title: EvaluatedBasicTitle,
    /// Clip opacity evaluated at the same author time.
    pub opacity: f32,
    /// Clip blend mode used by ordinary Timeline compositing.
    pub blend_mode: BlendMode,
    /// Clip source-to-Sequence affine transform.
    pub transform: [f32; 6],
    /// Compiled ordered visual Effect graph applied after title generation.
    pub effect_graph: Arc<CompiledEffectGraph>,
    /// Deterministic per-frame seed shared with other Clip content types.
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineNestedSequencePlan {
    pub sequence_id: SequenceId,
    /// Exact child-Sequence-local source time; consumers resolve the child grid.
    pub source_time: TimelineTime,
    pub color_processing: NestedColorProcessing,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

/// One renderable endpoint of a two-input visual Transition.
#[derive(Debug, Clone)]
pub enum TimelineTransitionInputPlan {
    /// A disabled endpoint contributes transparent scene-linear coverage.
    Transparent,
    /// File-backed media evaluated at the Transition's exact source demand.
    Media(TimelineMediaPlan),
    /// A generated solid evaluated through its Clip processing.
    SolidColor(TimelineSolidColorPlan),
    /// A generated Basic Title evaluated through its Clip processing.
    BasicTitle(TimelineBasicTitlePlan),
    /// A child Sequence evaluated before the parent Transition.
    NestedSequence(TimelineNestedSequencePlan),
}

/// Scene-linear, coverage-correct Cross Dissolve plan.
#[derive(Debug, Clone)]
pub struct TimelineCrossDissolvePlan {
    /// Earlier editorial endpoint after Clip-local evaluation.
    pub left: TimelineTransitionInputPlan,
    /// Later editorial endpoint after Clip-local evaluation.
    pub right: TimelineTransitionInputPlan,
    /// Normalized coefficient derived from exact author time.
    pub progress: f32,
}

#[derive(Debug, Clone)]
pub enum TimelineRenderPlanElement {
    Media(TimelineMediaPlan),
    Adjustment(TimelineAdjustmentPlan),
    SolidColor(TimelineSolidColorPlan),
    BasicTitle(TimelineBasicTitlePlan),
    NestedSequence(TimelineNestedSequencePlan),
    /// A two-input operation occupying one position in the Track stack.
    CrossDissolve(TimelineCrossDissolvePlan),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineColorDiagnostic {
    pub asset_id: AssetId,
    pub input_color_space_override: Option<ColorSpace>,
    pub input_encoding_override: Option<ColorEncodingSpec>,
    pub working_color_space: WorkingColorSpace,
    pub output_color_space: ColorSpace,
    pub output_encoding: ColorEncodingSpec,
    /// OCIO display used for presentation, when the caller is collecting display diagnostics.
    pub ocio_display: Option<String>,
    /// OCIO view used for presentation, when the caller is collecting display diagnostics.
    pub ocio_view: Option<String>,
    pub tone_map: bool,
    pub pixel_aspect_ratio_override: Option<PixelAspectRatio>,
    pub field_order_override: Option<FieldOrder>,
    pub alpha_interpretation: AlphaInterpretation,
    pub source_time: TimelineTime,
}

pub fn collect_timeline_color_diagnostics(
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
    working_color_space: WorkingColorSpace,
    output_color_space: ColorSpace,
) -> Result<Vec<TimelineColorDiagnostic>> {
    collect_timeline_color_diagnostics_with_display_view(
        source,
        timeline_frame,
        working_color_space,
        output_color_space,
        None,
        None,
    )
}

/// Collect color diagnostics and attach the caller's resolved OCIO display/view.
pub fn collect_timeline_color_diagnostics_with_display_view(
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
    working_color_space: WorkingColorSpace,
    output_color_space: ColorSpace,
    ocio_display: Option<&str>,
    ocio_view: Option<&str>,
) -> Result<Vec<TimelineColorDiagnostic>> {
    Ok(
        evaluate_timeline_render_plan(source, TimelineEvaluationRequest::analysis(timeline_frame))?
            .elements
            .into_iter()
            .flat_map(|element| {
                let mut diagnostics = Vec::with_capacity(2);
                match element {
                    TimelineRenderPlanElement::Media(media) => {
                        diagnostics.push(timeline_media_color_diagnostic(
                            &media,
                            working_color_space,
                            output_color_space,
                            ocio_display,
                            ocio_view,
                        ))
                    }
                    TimelineRenderPlanElement::CrossDissolve(transition) => {
                        for input in [transition.left, transition.right] {
                            if let TimelineTransitionInputPlan::Media(media) = input {
                                diagnostics.push(timeline_media_color_diagnostic(
                                    &media,
                                    working_color_space,
                                    output_color_space,
                                    ocio_display,
                                    ocio_view,
                                ));
                            }
                        }
                    }
                    TimelineRenderPlanElement::Adjustment(_)
                    | TimelineRenderPlanElement::SolidColor(_)
                    | TimelineRenderPlanElement::BasicTitle(_)
                    | TimelineRenderPlanElement::NestedSequence(_) => {}
                }
                diagnostics
            })
            .collect(),
    )
}

fn timeline_media_color_diagnostic(
    media: &TimelineMediaPlan,
    working_color_space: WorkingColorSpace,
    output_color_space: ColorSpace,
    ocio_display: Option<&str>,
    ocio_view: Option<&str>,
) -> TimelineColorDiagnostic {
    TimelineColorDiagnostic {
        asset_id: media.asset_id,
        input_color_space_override: media.color_space_override,
        input_encoding_override: media.color_space_override.map(ColorSpace::encoding),
        working_color_space,
        output_color_space,
        output_encoding: output_color_space.encoding(),
        ocio_display: ocio_display.map(str::to_owned),
        ocio_view: ocio_view.map(str::to_owned),
        tone_map: media.auto_tone_map,
        pixel_aspect_ratio_override: media.pixel_aspect_ratio_override,
        field_order_override: media.field_order_override,
        alpha_interpretation: media.alpha_interpretation,
        source_time: media.source_time,
    }
}

/// Evaluate one timeline frame into a typed render plan and diagnostics.
pub fn evaluate_timeline_render_plan(
    source: &dyn RenderPlanSource,
    request: TimelineEvaluationRequest,
) -> Result<TimelineRenderPlan> {
    let time_base = source.source_time_base();
    let timeline_frame = request.timeline_frame.max(0);
    let current = FramePosition::new(timeline_frame, time_base);
    let current_time = TimelineTime::from_frame_position(current)?;
    let active = source.flat_visual_items_at(current_time)?;
    let mut diagnostics = TimelineEvaluationDiagnostics {
        active_visual_items: active.len(),
        ..Default::default()
    };
    let mut elements = Vec::with_capacity(active.len());

    for item in active {
        match item {
            FlatVisualItem::Clip(clip) => {
                if let Some(element) =
                    compile_flat_clip(clip, source, timeline_frame, &mut diagnostics)?
                {
                    elements.push(element);
                }
            }
            FlatVisualItem::Transition(transition) => {
                elements.push(compile_transition(
                    *transition,
                    source,
                    timeline_frame,
                    &mut diagnostics,
                )?);
            }
        }
    }

    diagnostics.emitted_elements = elements.len();

    Ok(TimelineRenderPlan {
        timeline_frame,
        time_base,
        intent: request.intent,
        settings: request.settings,
        elements,
        diagnostics,
    })
}

fn compile_transition(
    transition: FlatVideoTransition,
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
    diagnostics: &mut TimelineEvaluationDiagnostics,
) -> Result<TimelineRenderPlanElement> {
    match transition.definition {
        FlatVideoTransitionDefinition::CrossDissolve => {
            if transition.properties.iter().next().is_some()
                || transition.params.as_object().is_none_or(|params| !params.is_empty())
            {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "compile_video_transition".to_owned(),
                    reason: format!(
                        "Cross Dissolve {} carries unsupported definition state",
                        transition.transition_id
                    ),
                });
            }
            let left =
                compile_transition_input(transition.left, source, timeline_frame, diagnostics)?;
            let right =
                compile_transition_input(transition.right, source, timeline_frame, diagnostics)?;
            let progress = transition.progress.normalized().ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "compile_video_transition".to_owned(),
                    reason: format!(
                        "video Transition {} has invalid progress coordinates",
                        transition.transition_id
                    ),
                }
            })?;
            Ok(TimelineRenderPlanElement::CrossDissolve(
                TimelineCrossDissolvePlan { left, right, progress },
            ))
        }
        FlatVideoTransitionDefinition::Plugin { definition_id } => {
            Err(MondrianError::WorkflowStepFailed {
                step_id: "compile_video_transition".to_owned(),
                reason: format!(
                    "video Transition {} requires unavailable definition `{definition_id}`",
                    transition.transition_id
                ),
            })
        }
    }
}

fn compile_transition_input(
    clip: FlatActiveClip,
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
    diagnostics: &mut TimelineEvaluationDiagnostics,
) -> Result<TimelineTransitionInputPlan> {
    if clip.is_disabled || clip.opacity.clamp(0.0, 1.0) <= 0.0 {
        return Ok(TimelineTransitionInputPlan::Transparent);
    }
    let element =
        compile_flat_clip(clip, source, timeline_frame, diagnostics)?.ok_or_else(|| {
            MondrianError::WorkflowStepFailed {
                step_id: "compile_video_transition".to_owned(),
                reason: "Transition endpoint did not produce a visual input".to_owned(),
            }
        })?;
    match element {
        TimelineRenderPlanElement::Media(media) => Ok(TimelineTransitionInputPlan::Media(media)),
        TimelineRenderPlanElement::SolidColor(solid) => {
            Ok(TimelineTransitionInputPlan::SolidColor(solid))
        }
        TimelineRenderPlanElement::BasicTitle(title) => {
            Ok(TimelineTransitionInputPlan::BasicTitle(title))
        }
        TimelineRenderPlanElement::NestedSequence(nested) => {
            Ok(TimelineTransitionInputPlan::NestedSequence(nested))
        }
        TimelineRenderPlanElement::Adjustment(_) => Err(MondrianError::WorkflowStepFailed {
            step_id: "compile_video_transition".to_owned(),
            reason: "Adjustment Layer cannot be a Transition endpoint".to_owned(),
        }),
        TimelineRenderPlanElement::CrossDissolve(_) => unreachable!("a Clip cannot lower itself"),
    }
}

fn compile_flat_clip(
    ac: FlatActiveClip,
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
    diagnostics: &mut TimelineEvaluationDiagnostics,
) -> Result<Option<TimelineRenderPlanElement>> {
    let opacity = ac.opacity.clamp(0.0, 1.0);
    if ac.is_disabled || opacity <= 0.0 {
        diagnostics.skipped_zero_opacity += 1;
        return Ok(None);
    }

    let effect_graph =
        mondrian_effects::compile_clip_effect_graph(&ac.effects, &ac.masks, ac.clip_time).map_err(
            |error| MondrianError::EffectGraphEvaluationFailed { reason: error.to_string() },
        )?;
    let frame_seed = timeline_frame.max(0);
    Ok(Some(match ac.content {
        ClipContent::NestedSequence { sequence_id, color_processing } => {
            TimelineRenderPlanElement::NestedSequence(TimelineNestedSequencePlan {
                sequence_id,
                source_time: ac.source_time,
                color_processing,
                opacity,
                blend_mode: ac.blend_mode,
                transform: ac.transform_matrix,
                effect_graph,
                frame_seed,
            })
        }
        ClipContent::AdjustmentLayer { .. } => {
            TimelineRenderPlanElement::Adjustment(TimelineAdjustmentPlan {
                effect_graph,
                opacity,
                blend_mode: ac.blend_mode,
                frame_seed,
            })
        }
        ClipContent::SolidColor { color, .. } => {
            TimelineRenderPlanElement::SolidColor(TimelineSolidColorPlan {
                color,
                opacity,
                blend_mode: ac.blend_mode,
                transform: ac.transform_matrix,
                effect_graph,
                frame_seed,
            })
        }
        ClipContent::BasicTitle { title } => {
            let title = title.evaluate(ac.clip_time)?;
            TimelineRenderPlanElement::BasicTitle(TimelineBasicTitlePlan {
                title,
                opacity,
                blend_mode: ac.blend_mode,
                transform: ac.transform_matrix,
                effect_graph,
                frame_seed,
            })
        }
        ClipContent::Media { asset_id, interpretation } => {
            let source_time = if let Some(frame_rate) = interpretation.frame_rate_override {
                TimelineTime::from_frame_position(
                    ac.source_time.to_frame_position(frame_rate, FrameRounding::Floor)?,
                )?
            } else {
                ac.source_time
            };
            let transform = apply_pixel_aspect_to_affine(
                ac.transform_matrix,
                interpretation.pixel_aspect_ratio_override,
            );
            TimelineRenderPlanElement::Media(TimelineMediaPlan {
                asset_id,
                color_space_override: interpretation.color_space_override,
                pixel_aspect_ratio_override: interpretation.pixel_aspect_ratio_override,
                field_order_override: interpretation.field_order_override,
                alpha_interpretation: interpretation.alpha,
                source_time,
                opacity,
                blend_mode: ac.blend_mode,
                transform,
                effect_graph,
                frame_seed,
                auto_tone_map: source.auto_tone_map_media(),
            })
        }
    }))
}

pub fn mat3_to_affine(cols: [f32; 9]) -> [f32; 6] {
    [cols[0], cols[3], cols[6], cols[1], cols[4], cols[7]]
}

/// Project an authoring-space affine transform into reduced-resolution render extents.
///
/// Timeline transforms map full-resolution source pixel coordinates into the
/// full-resolution sequence canvas. Preview samples and proxy frames may use
/// fewer pixels on either side of that mapping; those sampling-density changes
/// must not alter the authored spatial result.
pub fn project_affine_to_sampled_extents(
    transform: [f32; 6],
    source_authoring: mondrian_core::Resolution,
    source_sampled: mondrian_core::Resolution,
    output_authoring: mondrian_core::Resolution,
    output_sampled: mondrian_core::Resolution,
) -> Option<[f32; 6]> {
    if source_authoring.width == 0
        || source_authoring.height == 0
        || source_sampled.width == 0
        || source_sampled.height == 0
        || output_authoring.width == 0
        || output_authoring.height == 0
        || output_sampled.width == 0
        || output_sampled.height == 0
        || transform.iter().any(|value| !value.is_finite())
    {
        return None;
    }

    let source_x = source_authoring.width as f32 / source_sampled.width as f32;
    let source_y = source_authoring.height as f32 / source_sampled.height as f32;
    let output_x = output_sampled.width as f32 / output_authoring.width as f32;
    let output_y = output_sampled.height as f32 / output_authoring.height as f32;
    let projected = [
        output_x * transform[0] * source_x,
        output_x * transform[1] * source_y,
        output_x * transform[2],
        output_y * transform[3] * source_x,
        output_y * transform[4] * source_y,
        output_y * transform[5],
    ];
    projected.iter().all(|value| value.is_finite()).then_some(projected)
}

fn apply_pixel_aspect_to_affine(
    mut transform: [f32; 6],
    pixel_aspect_ratio: Option<PixelAspectRatio>,
) -> [f32; 6] {
    let Some(ratio) = pixel_aspect_ratio.and_then(PixelAspectRatio::ratio) else {
        return transform;
    };
    if (ratio - 1.0).abs() <= f32::EPSILON {
        return transform;
    }
    transform[0] *= ratio;
    transform[3] *= ratio;
    transform
}

fn normalize_resolution_scale(scale: f32) -> f32 {
    if !scale.is_finite() {
        return 1.0;
    }
    scale.clamp(0.125, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{Keyframe, PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::types::{AssetId, Resolution};
    use mondrian_timeline::clip::{Clip, Transform2D};
    use mondrian_timeline::sequence::Sequence;
    use mondrian_timeline::track::Track;
    use mondrian_timeline::VideoTransition;

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("valid test time")
    }

    fn analysis_elements(
        source: &dyn RenderPlanSource,
        timeline_frame: i64,
    ) -> Vec<TimelineRenderPlanElement> {
        evaluate_timeline_render_plan(source, TimelineEvaluationRequest::analysis(timeline_frame))
            .expect("evaluate timeline")
            .elements
    }

    #[test]
    fn evaluation_request_preview_carries_interactive_contract() {
        let request = TimelineEvaluationRequest::preview(42, 0.5);

        assert_eq!(request.timeline_frame, 42);
        assert_eq!(request.intent, TimelineRenderIntent::Preview);
        assert_eq!(request.settings.quality, TimelineRenderQuality::Interactive);
        assert_eq!(
            request.settings.color_target,
            TimelineRenderColorTarget::Display
        );
        assert!(request.settings.allow_frame_drop);
        assert_eq!(request.settings.resolution_scale, 0.5);
    }

    #[test]
    fn evaluation_request_export_disallows_frame_drop() {
        let request = TimelineEvaluationRequest::export(12);

        assert_eq!(request.intent, TimelineRenderIntent::Export);
        assert_eq!(request.settings.quality, TimelineRenderQuality::Final);
        assert_eq!(
            request.settings.color_target,
            TimelineRenderColorTarget::Export
        );
        assert!(!request.settings.allow_frame_drop);
        assert_eq!(request.settings.resolution_scale, 1.0);
    }

    #[test]
    fn sampled_extent_projection_preserves_authored_fit() {
        let projected = project_affine_to_sampled_extents(
            [0.5, 0.0, 0.0, 0.0, 0.5, 0.0],
            Resolution { width: 3840, height: 2160 },
            Resolution { width: 960, height: 540 },
            Resolution { width: 1920, height: 1080 },
            Resolution { width: 960, height: 540 },
        )
        .expect("valid sampled extents");

        assert_eq!(projected, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn sampled_extent_projection_scales_axes_and_translation_independently() {
        let projected = project_affine_to_sampled_extents(
            [0.0, -1.0, 120.0, 1.0, 0.0, 80.0],
            Resolution { width: 4000, height: 2000 },
            Resolution { width: 1000, height: 1000 },
            Resolution { width: 2000, height: 1000 },
            Resolution { width: 1000, height: 250 },
        )
        .expect("valid sampled extents");

        assert_eq!(projected, [0.0, -1.0, 60.0, 1.0, 0.0, 20.0]);
    }

    #[test]
    fn render_plan_uses_track_blend_mode_for_media_and_adjustment() {
        let mut seq = Sequence::new("render-plan-blend");
        let tb = seq.time_base();
        seq.settings.resolution = Resolution::FHD;
        seq.video_tracks[0].blend_mode = BlendMode::Screen;
        seq.video_tracks[1].blend_mode = BlendMode::Multiply;

        let media = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        let adjustment = Clip::new_adjustment_layer(AssetId::new(), tt(0, tb), tt(20, tb))
            .expect("valid adjustment clip");
        seq.video_tracks[0].add_clip(media).expect("add media");
        seq.video_tracks[1].add_clip(adjustment).expect("add adjustment");

        let plan = analysis_elements(&seq, 5);
        assert_eq!(plan.len(), 2);

        match &plan[0] {
            TimelineRenderPlanElement::Media(media) => {
                assert_eq!(media.blend_mode, BlendMode::Screen);
            }
            _ => panic!("expected media"),
        }

        match &plan[1] {
            TimelineRenderPlanElement::Adjustment(adjustment) => {
                assert_eq!(adjustment.blend_mode, BlendMode::Multiply);
            }
            _ => panic!("expected adjustment"),
        }
    }

    #[test]
    fn render_plan_prefers_clip_blend_mode_over_track() {
        let mut seq = Sequence::new("render-plan-clip-override");
        let tb = seq.time_base();
        seq.video_tracks[0].blend_mode = BlendMode::Screen;

        let mut media = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        media.blend_mode = Some(BlendMode::HardLight);
        seq.video_tracks[0].add_clip(media).expect("add media");

        let plan = analysis_elements(&seq, 5);
        assert_eq!(plan.len(), 1);
        match &plan[0] {
            TimelineRenderPlanElement::Media(media) => {
                assert_eq!(media.blend_mode, BlendMode::HardLight);
            }
            _ => panic!("expected media"),
        }
    }

    #[test]
    fn render_plan_evaluates_transform_and_title_in_one_clip_local_domain() {
        let mut seq = Sequence::new("clip-local-render-plan");
        let tb = seq.time_base();
        let mut title = Clip::new_basic_title("Mondrian", "Segoe UI", tt(10, tb), tt(20, tb))
            .expect("valid Basic Title");
        title.source_in = tt(100, tb);
        title.source_out = tt(120, tb);
        for (time, opacity, font_size) in [(tt(0, tb), 0.0, 40.0), (tt(20, tb), 1.0, 80.0)] {
            title
                .apply_property_mutation(PropertyMutation::SetKeyframe {
                    path: Transform2D::OPACITY_PATH.to_owned(),
                    keyframe: Keyframe::linear(time, PropertyValue::Float(opacity)),
                })
                .expect("opacity key");
            title
                .apply_property_mutation(PropertyMutation::SetKeyframe {
                    path: mondrian_core::BasicTitle::FONT_SIZE_PATH.to_owned(),
                    keyframe: Keyframe::linear(time, PropertyValue::Float(font_size)),
                })
                .expect("font-size key");
        }
        seq.video_tracks[0].add_clip(title).expect("add title");

        let preview =
            evaluate_timeline_render_plan(&seq, TimelineEvaluationRequest::preview(20, 0.5))
                .expect("preview plan");
        let export = evaluate_timeline_render_plan(&seq, TimelineEvaluationRequest::export(20))
            .expect("export plan");
        assert_eq!(
            preview_semantic_signature(&preview),
            preview_semantic_signature(&export)
        );

        let TimelineRenderPlanElement::BasicTitle(title) = &preview.elements[0] else {
            panic!("expected Basic Title");
        };

        assert!((title.opacity - 0.5).abs() < 1.0e-6);
        assert!((title.title.font_size - 60.0).abs() < 1.0e-5);
    }

    #[test]
    fn render_plan_carries_clip_media_interpretation() {
        let mut seq = Sequence::new("render-plan-interpretation");
        let tb = seq.time_base();
        let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        let interpretation = clip.media_interpretation_mut().expect("media interpretation");
        interpretation.color_space_override = Some(mondrian_core::types::ColorSpace::Srgb);
        interpretation.pixel_aspect_ratio_override = Some(PixelAspectRatio::Anamorphic2x);
        interpretation.field_order_override = Some(FieldOrder::UpperFirst);
        interpretation.alpha = AlphaInterpretation::Premultiplied;
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let plan = analysis_elements(&seq, 0);
        let TimelineRenderPlanElement::Media(media) = &plan[0] else {
            panic!("expected media plan");
        };
        assert_eq!(
            media.color_space_override,
            Some(mondrian_core::types::ColorSpace::Srgb)
        );
        assert_eq!(
            media.pixel_aspect_ratio_override,
            Some(PixelAspectRatio::Anamorphic2x)
        );
        assert_eq!(media.field_order_override, Some(FieldOrder::UpperFirst));
        assert_eq!(
            media.alpha_interpretation,
            AlphaInterpretation::Premultiplied
        );
        assert!((media.transform[0] - 2.0).abs() < 1.0e-6);
    }

    #[test]
    fn render_plan_resolves_exact_source_time_on_overridden_frame_grid() {
        let mut seq = Sequence::new("render-plan-frame-rate-override");
        let tb = seq.time_base();
        let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
        clip.media_interpretation_mut()
            .expect("media interpretation")
            .frame_rate_override = Some(Rational::FPS_30);
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let plan = analysis_elements(&seq, 15);
        let TimelineRenderPlanElement::Media(media) = &plan[0] else {
            panic!("expected media plan");
        };
        assert_eq!(
            media.source_time,
            TimelineTime::new(3, 5).expect("exact source time")
        );
    }

    #[test]
    fn render_plan_preserves_exact_source_time_without_an_override_grid() {
        let mut seq = Sequence::new("render-plan-native-source-time");
        let tb = seq.time_base();
        let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
        clip.source_in = TimelineTime::new(1, 7).expect("exact source offset");
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let plan = analysis_elements(&seq, 15);
        let TimelineRenderPlanElement::Media(media) = &plan[0] else {
            panic!("expected media plan");
        };
        assert_eq!(
            media.source_time,
            TimelineTime::new(26, 35).expect("exact source time")
        );
    }

    #[test]
    fn color_diagnostics_expose_media_interpretation_and_sequence_output() {
        let mut seq = Sequence::new("color-diagnostics");
        let tb = seq.time_base();
        seq.settings.color.working_color_space = mondrian_core::WorkingColorSpace::LinearRec2020;
        seq.settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        let asset_id = clip.media_asset_id().expect("media asset");
        let interpretation = clip.media_interpretation_mut().expect("media interpretation");
        interpretation.color_space_override = Some(ColorSpace::AppleLogBt2020);
        interpretation.pixel_aspect_ratio_override = Some(PixelAspectRatio::DvcproHd);
        interpretation.field_order_override = Some(FieldOrder::LowerFirst);
        interpretation.alpha = AlphaInterpretation::Ignore;
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let diagnostics = collect_timeline_color_diagnostics(
            &seq,
            4,
            seq.settings.color.working_color_space,
            seq.settings.color.program_output.color_space,
        )
        .expect("collect diagnostics");
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.asset_id, asset_id);
        assert_eq!(
            diagnostic.input_color_space_override,
            Some(ColorSpace::AppleLogBt2020)
        );
        assert_eq!(
            diagnostic.working_color_space,
            WorkingColorSpace::LinearRec2020
        );
        assert_eq!(diagnostic.output_color_space, ColorSpace::Rec2100Pq);
        assert_eq!(
            diagnostic.output_encoding.kind,
            mondrian_core::ColorEncodingKind::DisplayHdr
        );
        assert_eq!(
            diagnostic
                .input_encoding_override
                .expect("clip override should carry encoding")
                .kind,
            mondrian_core::ColorEncodingKind::SceneLog
        );
        assert_eq!(
            diagnostic.pixel_aspect_ratio_override,
            Some(PixelAspectRatio::DvcproHd)
        );
        assert_eq!(
            diagnostic.field_order_override,
            Some(FieldOrder::LowerFirst)
        );
        assert_eq!(diagnostic.alpha_interpretation, AlphaInterpretation::Ignore);
    }

    #[test]
    fn color_diagnostics_can_carry_display_view_context() {
        let mut seq = Sequence::new("display-view-diagnostics");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let diagnostics = collect_timeline_color_diagnostics_with_display_view(
            &seq,
            4,
            WorkingColorSpace::LinearRec709,
            ColorSpace::Srgb,
            Some("sRGB - Display"),
            Some("ACES 2.0 - SDR 100 nits (Rec.709)"),
        )
        .expect("collect diagnostics");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].ocio_display.as_deref(),
            Some("sRGB - Display")
        );
        assert_eq!(
            diagnostics[0].ocio_view.as_deref(),
            Some("ACES 2.0 - SDR 100 nits (Rec.709)")
        );
    }

    #[test]
    fn render_plan_carries_nested_clip_color_processing() {
        let mut seq = Sequence::new("render-plan-nested-processing");
        let tb = seq.time_base();
        let child = Sequence::new("child");
        let child_id = child.id;
        let mut nested_clip =
            Clip::new_nested_sequence(child_id, tt(0, tb), tt(20, tb), Some("child".to_string()))
                .expect("valid nested clip");
        let ClipContent::NestedSequence { color_processing, .. } = &mut nested_clip.content else {
            panic!("expected nested Clip content");
        };
        *color_processing = NestedColorProcessing::ForceParentWorkingSpace;

        seq.video_tracks[0].add_clip(nested_clip).expect("add nested sequence");

        let plan = analysis_elements(&seq, 0);
        let TimelineRenderPlanElement::NestedSequence(nested) = &plan[0] else {
            panic!("expected nested sequence plan");
        };
        assert_eq!(
            nested.color_processing,
            NestedColorProcessing::ForceParentWorkingSpace
        );
    }

    #[test]
    fn preview_and_export_requests_preserve_timeline_semantics() {
        let mut seq = Sequence::new("preview-export-contract");
        seq.video_tracks = vec![
            Track::new_video("V1"),
            Track::new_video("V2"),
            Track::new_video("V3"),
            Track::new_video("V4"),
        ];
        seq.video_tracks[1].blend_mode = BlendMode::Screen;
        seq.video_tracks[3].blend_mode = BlendMode::Multiply;
        let tb = seq.time_base();

        let media_asset = AssetId::new();
        let mut media = Clip::new(media_asset, tt(0, tb), tt(30, tb)).expect("valid clip");
        media.source_in = tt(3, tb);
        media.source_out = tt(33, tb);
        let interpretation = media.media_interpretation_mut().expect("media interpretation");
        interpretation.color_space_override = Some(ColorSpace::Srgb);
        interpretation.pixel_aspect_ratio_override = Some(PixelAspectRatio::Anamorphic2x);
        interpretation.field_order_override = Some(FieldOrder::UpperFirst);
        interpretation.alpha = AlphaInterpretation::Premultiplied;
        media.blend_mode = Some(BlendMode::HardLight);
        seq.video_tracks[0].add_clip(media).expect("add media");

        let solid = Clip::new_solid_color(
            AssetId::new(),
            Color::from_rgba8(16, 48, 128, 255),
            tt(0, tb),
            tt(30, tb),
        )
        .expect("valid solid clip");
        seq.video_tracks[1].add_clip(solid).expect("add solid");

        let child_id = SequenceId::new();
        let mut nested =
            Clip::new_nested_sequence(child_id, tt(5, tb), tt(30, tb), Some("child".to_owned()))
                .expect("valid nested clip");
        nested.source_in = tt(20, tb);
        nested.source_out = tt(50, tb);
        let ClipContent::NestedSequence { color_processing, .. } = &mut nested.content else {
            panic!("expected nested Clip content");
        };
        *color_processing = NestedColorProcessing::ForceParentWorkingSpace;
        seq.video_tracks[2].add_clip(nested).expect("add nested");

        let adjustment = Clip::new_adjustment_layer(AssetId::new(), tt(0, tb), tt(30, tb))
            .expect("valid adjustment clip");
        seq.video_tracks[3].add_clip(adjustment).expect("add adjustment");

        let preview =
            evaluate_timeline_render_plan(&seq, TimelineEvaluationRequest::preview(12, 0.25))
                .expect("preview plan");
        let export = evaluate_timeline_render_plan(&seq, TimelineEvaluationRequest::export(12))
            .expect("export plan");

        assert_eq!(preview.timeline_frame, export.timeline_frame);
        assert_eq!(preview.time_base, export.time_base);
        assert_ne!(preview.intent, export.intent);
        assert_ne!(preview.settings, export.settings);
        assert_eq!(preview.diagnostics, export.diagnostics);
        assert_eq!(preview.len(), 4);
        assert_eq!(
            preview_semantic_signature(&preview),
            preview_semantic_signature(&export)
        );
    }

    #[test]
    fn evaluate_timeline_render_plan_reports_filtering_diagnostics() {
        let mut seq = Sequence::new("render-plan-diagnostics");
        let tb = seq.time_base();
        let mut hidden = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        hidden
            .transform
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: Transform2D::OPACITY_PATH.to_string(),
                value: PropertyValue::Float(0.0),
            })
            .expect("set clip opacity");
        seq.video_tracks[0].add_clip(hidden).expect("add hidden");

        let visible = Clip::new_solid_color(
            AssetId::new(),
            Color::from_rgba8(255, 0, 0, 255),
            tt(0, tb),
            tt(20, tb),
        )
        .expect("valid solid clip");
        seq.video_tracks[1].add_clip(visible).expect("add visible");

        let plan = evaluate_timeline_render_plan(&seq, TimelineEvaluationRequest::preview(4, 0.5))
            .expect("preview plan");

        assert_eq!(plan.timeline_frame, 4);
        assert_eq!(plan.intent, TimelineRenderIntent::Preview);
        assert_eq!(plan.diagnostics.active_visual_items, 2);
        assert_eq!(plan.diagnostics.emitted_elements, 1);
        assert_eq!(plan.diagnostics.skipped_zero_opacity, 1);
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn cross_dissolve_plan_uses_unclamped_endpoint_source_times_and_exact_progress() {
        let mut sequence = Sequence::new("cross dissolve render plan");
        let time_base = sequence.time_base();
        let mut left =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("left");
        left.source_in = tt(5, time_base);
        left.source_out = tt(15, time_base);
        let mut right =
            Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right");
        right.source_in = tt(20, time_base);
        right.source_out = tt(30, time_base);
        let (left_id, right_id) = (left.id, right.id);
        sequence.video_tracks[0].add_clip(left).expect("left placement");
        sequence.video_tracks[0].add_clip(right).expect("right placement");
        sequence.video_transitions.push(VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(tt(8, time_base), tt(4, time_base))
                .expect("transition range"),
        ));
        sequence.validate_author_identities().expect("valid author graph");

        let plan = evaluate_timeline_render_plan(&sequence, TimelineEvaluationRequest::export(10))
            .expect("transition plan");
        assert_eq!(plan.diagnostics.active_visual_items, 1);
        assert_eq!(plan.len(), 1);
        let TimelineRenderPlanElement::CrossDissolve(transition) = &plan.elements[0] else {
            panic!("expected one Cross Dissolve");
        };
        assert_eq!(transition.progress, 0.5);
        let TimelineTransitionInputPlan::Media(left) = &transition.left else {
            panic!("left input must be media");
        };
        let TimelineTransitionInputPlan::Media(right) = &transition.right else {
            panic!("right input must be media");
        };
        assert_eq!(left.source_time, tt(15, time_base));
        assert_eq!(right.source_time, tt(20, time_base));

        let start = evaluate_timeline_render_plan(&sequence, TimelineEvaluationRequest::export(8))
            .expect("transition start plan");
        let TimelineRenderPlanElement::CrossDissolve(start) = &start.elements[0] else {
            panic!("expected Cross Dissolve at start");
        };
        assert_eq!(start.progress, 0.0);
        let TimelineTransitionInputPlan::Media(right) = &start.right else {
            panic!("right input must be media");
        };
        assert_eq!(right.source_time, tt(18, time_base));
    }

    #[test]
    fn unavailable_transition_definition_fails_closed() {
        let mut sequence = Sequence::new("missing transition plugin");
        let time_base = sequence.time_base();
        let left = Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("left");
        let right = Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right");
        let (left_id, right_id) = (left.id, right.id);
        sequence.video_tracks[0].add_clip(left).expect("left placement");
        sequence.video_tracks[0].add_clip(right).expect("right placement");
        let mut transition = VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(tt(8, time_base), tt(4, time_base))
                .expect("transition range"),
        );
        transition.transition_type = mondrian_timeline::VideoTransitionType::Plugin {
            definition_id: "vendor.missing.transition".to_owned(),
        };
        sequence.video_transitions.push(transition);

        let error =
            evaluate_timeline_render_plan(&sequence, TimelineEvaluationRequest::preview(10, 1.0))
                .expect_err("missing plugin must not substitute Cross Dissolve");
        assert!(error.to_string().contains("unavailable definition"));
    }

    #[derive(Debug, PartialEq)]
    enum RenderPlanSemanticElement {
        Media {
            asset_id: AssetId,
            color_space_override: Option<ColorSpace>,
            pixel_aspect_ratio_override: Option<PixelAspectRatio>,
            field_order_override: Option<FieldOrder>,
            alpha_interpretation: AlphaInterpretation,
            source_time: TimelineTime,
            opacity: f32,
            blend_mode: BlendMode,
            transform: [f32; 6],
            frame_seed: i64,
            auto_tone_map: bool,
        },
        Adjustment {
            opacity: f32,
            blend_mode: BlendMode,
            frame_seed: i64,
        },
        SolidColor {
            color: Color,
            opacity: f32,
            blend_mode: BlendMode,
            transform: [f32; 6],
            frame_seed: i64,
        },
        BasicTitle {
            title: EvaluatedBasicTitle,
            opacity: f32,
            blend_mode: BlendMode,
            transform: [f32; 6],
            frame_seed: i64,
        },
        NestedSequence {
            sequence_id: SequenceId,
            source_time: TimelineTime,
            color_processing: NestedColorProcessing,
            opacity: f32,
            blend_mode: BlendMode,
            transform: [f32; 6],
            frame_seed: i64,
        },
        CrossDissolve {
            left: RenderPlanSemanticTransitionInput,
            right: RenderPlanSemanticTransitionInput,
            progress: f32,
        },
    }

    #[derive(Debug, PartialEq)]
    enum RenderPlanSemanticTransitionInput {
        Transparent,
        Media {
            asset_id: AssetId,
            source_time: TimelineTime,
        },
        SolidColor {
            color: Color,
        },
        BasicTitle {
            title: EvaluatedBasicTitle,
        },
        NestedSequence {
            sequence_id: SequenceId,
            source_time: TimelineTime,
        },
    }

    fn preview_semantic_signature(plan: &TimelineRenderPlan) -> Vec<RenderPlanSemanticElement> {
        plan.elements
            .iter()
            .map(|element| match element {
                TimelineRenderPlanElement::Media(media) => RenderPlanSemanticElement::Media {
                    asset_id: media.asset_id,
                    color_space_override: media.color_space_override,
                    pixel_aspect_ratio_override: media.pixel_aspect_ratio_override,
                    field_order_override: media.field_order_override,
                    alpha_interpretation: media.alpha_interpretation,
                    source_time: media.source_time,
                    opacity: media.opacity,
                    blend_mode: media.blend_mode,
                    transform: media.transform,
                    frame_seed: media.frame_seed,
                    auto_tone_map: media.auto_tone_map,
                },
                TimelineRenderPlanElement::Adjustment(adjustment) => {
                    RenderPlanSemanticElement::Adjustment {
                        opacity: adjustment.opacity,
                        blend_mode: adjustment.blend_mode,
                        frame_seed: adjustment.frame_seed,
                    }
                }
                TimelineRenderPlanElement::SolidColor(solid) => {
                    RenderPlanSemanticElement::SolidColor {
                        color: solid.color,
                        opacity: solid.opacity,
                        blend_mode: solid.blend_mode,
                        transform: solid.transform,
                        frame_seed: solid.frame_seed,
                    }
                }
                TimelineRenderPlanElement::BasicTitle(title) => {
                    RenderPlanSemanticElement::BasicTitle {
                        title: title.title.clone(),
                        opacity: title.opacity,
                        blend_mode: title.blend_mode,
                        transform: title.transform,
                        frame_seed: title.frame_seed,
                    }
                }
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    RenderPlanSemanticElement::NestedSequence {
                        sequence_id: nested.sequence_id,
                        source_time: nested.source_time,
                        color_processing: nested.color_processing,
                        opacity: nested.opacity,
                        blend_mode: nested.blend_mode,
                        transform: nested.transform,
                        frame_seed: nested.frame_seed,
                    }
                }
                TimelineRenderPlanElement::CrossDissolve(transition) => {
                    RenderPlanSemanticElement::CrossDissolve {
                        left: transition_input_signature(&transition.left),
                        right: transition_input_signature(&transition.right),
                        progress: transition.progress,
                    }
                }
            })
            .collect()
    }

    fn transition_input_signature(
        input: &TimelineTransitionInputPlan,
    ) -> RenderPlanSemanticTransitionInput {
        match input {
            TimelineTransitionInputPlan::Transparent => {
                RenderPlanSemanticTransitionInput::Transparent
            }
            TimelineTransitionInputPlan::Media(media) => RenderPlanSemanticTransitionInput::Media {
                asset_id: media.asset_id,
                source_time: media.source_time,
            },
            TimelineTransitionInputPlan::SolidColor(solid) => {
                RenderPlanSemanticTransitionInput::SolidColor { color: solid.color }
            }
            TimelineTransitionInputPlan::BasicTitle(title) => {
                RenderPlanSemanticTransitionInput::BasicTitle { title: title.title.clone() }
            }
            TimelineTransitionInputPlan::NestedSequence(nested) => {
                RenderPlanSemanticTransitionInput::NestedSequence {
                    sequence_id: nested.sequence_id,
                    source_time: nested.source_time,
                }
            }
        }
    }
}
