use mondrian_core::{
    timeline_data::{
        AlphaInterpretation, ClipContent, FieldOrder, FlatActiveClip, FlatVideoTransition,
        FlatVideoTransitionDefinition, FlatVisualItem, NestedColorProcessing, PixelAspectRatio,
        RenderPlanSource, TimelineClipEndpointContext, TimelineClipExecutionRef,
    },
    types::{AssetId, BlendMode, Color, ColorSpace, FramePosition, Rational, SequenceId},
    ColorEncodingSpec, EvaluatedBasicTitle, MondrianError, Result, TimelineTime, WorkingColorSpace,
};
use mondrian_effects::{CompiledEffectGraph, EffectExecutionSession};
use std::sync::Arc;

use crate::prepared_visual_program::PreparedVisualProgram;

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

/// A request to evaluate one exact visual sample into a render plan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineEvaluationRequest {
    /// Exact Sequence-local progressive-frame or half-frame field position.
    ///
    /// Evaluation rejects a negative position or a time base other than the
    /// source Sequence Evaluation Grid or its exact doubled field grid. The
    /// renderer never rounds a field instant back to an authored frame.
    pub position: FramePosition,
    /// Caller intent.
    pub intent: TimelineRenderIntent,
    /// Execution settings for the request.
    pub settings: TimelineRenderSettings,
}

impl TimelineEvaluationRequest {
    /// Build a preview evaluation request.
    pub fn preview(position: FramePosition, resolution_scale: f32) -> Self {
        Self {
            position,
            intent: TimelineRenderIntent::Preview,
            settings: TimelineRenderSettings::preview(resolution_scale),
        }
    }

    /// Build an export evaluation request.
    pub fn export(position: FramePosition) -> Self {
        Self {
            position,
            intent: TimelineRenderIntent::Export,
            settings: TimelineRenderSettings::export(),
        }
    }

    /// Build an analysis evaluation request.
    pub fn analysis(position: FramePosition) -> Self {
        Self {
            position,
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
    /// Exact nonnegative Sequence-local position evaluated by this plan.
    pub position: FramePosition,
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
    /// Exact prepared placement and endpoint identity.
    pub placement: TimelineClipExecutionRef,
    pub asset_id: AssetId,
    pub color_space_override: Option<ColorSpace>,
    pub pixel_aspect_ratio_override: Option<PixelAspectRatio>,
    pub field_order_override: Option<FieldOrder>,
    pub alpha_interpretation: AlphaInterpretation,
    /// Optional authored interpretation grid used for every current or
    /// historical decode target.
    pub frame_rate_override: Option<Rational>,
    /// Exact source-domain decode target after any explicit interpretation grid.
    pub source_sample: mondrian_core::SourceSampleTarget,
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
    /// Exact prepared placement identity. Temporal Adjustment execution is
    /// currently rejected because it has no single source value.
    pub placement: TimelineClipExecutionRef,
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub frame_seed: i64,
}

/// Full-composite Sequence grade evaluated once after every visual item.
#[derive(Debug, Clone)]
pub struct TimelineGradePlan {
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineSolidColorPlan {
    /// Exact prepared placement and endpoint identity.
    pub placement: TimelineClipExecutionRef,
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
    /// Exact prepared placement and endpoint identity.
    pub placement: TimelineClipExecutionRef,
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
    /// Exact prepared parent placement and endpoint identity.
    pub placement: TimelineClipExecutionRef,
    pub sequence_id: SequenceId,
    /// Exact child-Sequence-local source time; consumers resolve the child grid.
    pub source_sample: mondrian_core::SourceSampleTarget,
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
    /// Stable author Transition identity retained for historical endpoint
    /// sampling.
    pub transition_id: mondrian_core::VideoTransitionId,
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
    CrossDissolve(Box<TimelineCrossDissolvePlan>),
    /// Explicit full-composite grade; never masquerades as a Clip placement.
    TimelineGrade(TimelineGradePlan),
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

#[cfg(test)]
pub(crate) fn collect_timeline_color_diagnostics(
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
#[cfg(test)]
pub(crate) fn collect_timeline_color_diagnostics_with_display_view(
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
    working_color_space: WorkingColorSpace,
    output_color_space: ColorSpace,
    ocio_display: Option<&str>,
    ocio_view: Option<&str>,
) -> Result<Vec<TimelineColorDiagnostic>> {
    Ok(evaluate_timeline_render_plan(
        source,
        TimelineEvaluationRequest::analysis(FramePosition::new(
            timeline_frame,
            source.source_time_base(),
        )),
    )?
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
            | TimelineRenderPlanElement::TimelineGrade(_)
            | TimelineRenderPlanElement::SolidColor(_)
            | TimelineRenderPlanElement::BasicTitle(_)
            | TimelineRenderPlanElement::NestedSequence(_) => {}
        }
        diagnostics
    })
    .collect())
}

#[cfg(test)]
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
        source_time: media.source_sample.time(),
    }
}

/// Evaluate one timeline frame into a typed render plan and diagnostics.
#[cfg(test)]
pub(crate) fn evaluate_timeline_render_plan(
    source: &dyn RenderPlanSource,
    request: TimelineEvaluationRequest,
) -> Result<TimelineRenderPlan> {
    let mut effects = DirectClipEffectResolver {
        working_color_space: source.source_working_color_space(),
    };
    evaluate_timeline_render_plan_with_effects(source, request, &mut effects)
}

/// Evaluate one frame through a revision-bound visual program without retaining
/// dynamic topology.
///
/// This remains a scalar/reference path for tests and diagnostics. Production
/// Preview and Export use [`evaluate_prepared_visual_program_with_session`] so
/// every reusable execution resource remains inside an explicit owner Session.
pub fn evaluate_prepared_visual_program(
    program: &PreparedVisualProgram,
    request: TimelineEvaluationRequest,
) -> Result<TimelineRenderPlan> {
    let mut effects = PreparedClipEffectResolver { program };
    evaluate_timeline_render_plan_with_effects(program.schedule(), request, &mut effects)
}

/// Evaluate one prepared visual frame while retaining dynamic Effect topology
/// only in the caller's explicit execution Session.
///
/// Production Preview and Export must use this entry point through their
/// owner-scoped compositor scratch. [`evaluate_prepared_visual_program`] is the
/// uncached scalar/reference entry point.
pub fn evaluate_prepared_visual_program_with_session(
    program: &PreparedVisualProgram,
    request: TimelineEvaluationRequest,
    session: &mut EffectExecutionSession,
) -> Result<TimelineRenderPlan> {
    let mut effects = SessionPreparedClipEffectResolver { program, session };
    evaluate_timeline_render_plan_with_effects(program.schedule(), request, &mut effects)
}

trait ClipEffectResolver {
    fn evaluate(&mut self, clip: &FlatActiveClip) -> Result<Arc<CompiledEffectGraph>>;

    fn evaluate_timeline_grade(
        &mut self,
        sequence_time: TimelineTime,
    ) -> Result<Option<Arc<CompiledEffectGraph>>>;

    fn admit_transition(&mut self, transition: &FlatVideoTransition) -> Result<()>;
}

#[cfg(test)]
struct DirectClipEffectResolver {
    working_color_space: WorkingColorSpace,
}

#[cfg(test)]
impl ClipEffectResolver for DirectClipEffectResolver {
    fn evaluate(&mut self, clip: &FlatActiveClip) -> Result<Arc<CompiledEffectGraph>> {
        mondrian_effects::compile_clip_effect_graph(
            &clip.effects,
            &clip.masks,
            clip.clip_time,
            self.working_color_space,
        )
        .map_err(|error| MondrianError::EffectGraphEvaluationFailed { reason: error.to_string() })
    }

    fn evaluate_timeline_grade(
        &mut self,
        _sequence_time: TimelineTime,
    ) -> Result<Option<Arc<CompiledEffectGraph>>> {
        Ok(None)
    }

    fn admit_transition(&mut self, transition: &FlatVideoTransition) -> Result<()> {
        validate_flat_transition_definition(transition)
    }
}

struct PreparedClipEffectResolver<'a> {
    program: &'a PreparedVisualProgram,
}

impl ClipEffectResolver for PreparedClipEffectResolver<'_> {
    fn evaluate(&mut self, clip: &FlatActiveClip) -> Result<Arc<CompiledEffectGraph>> {
        self.program.evaluate_clip_effects(clip.clip_id, clip.clip_time)
    }

    fn evaluate_timeline_grade(
        &mut self,
        sequence_time: TimelineTime,
    ) -> Result<Option<Arc<CompiledEffectGraph>>> {
        self.program.evaluate_timeline_grade(sequence_time)
    }

    fn admit_transition(&mut self, transition: &FlatVideoTransition) -> Result<()> {
        self.program.ensure_transition_ready(transition.transition_id)
    }
}

struct SessionPreparedClipEffectResolver<'a> {
    program: &'a PreparedVisualProgram,
    session: &'a mut EffectExecutionSession,
}

impl ClipEffectResolver for SessionPreparedClipEffectResolver<'_> {
    fn evaluate(&mut self, clip: &FlatActiveClip) -> Result<Arc<CompiledEffectGraph>> {
        self.program
            .evaluate_clip_effects_with_session(clip.clip_id, clip.clip_time, self.session)
    }

    fn evaluate_timeline_grade(
        &mut self,
        sequence_time: TimelineTime,
    ) -> Result<Option<Arc<CompiledEffectGraph>>> {
        self.program.evaluate_timeline_grade_with_session(sequence_time, self.session)
    }

    fn admit_transition(&mut self, transition: &FlatVideoTransition) -> Result<()> {
        self.program.ensure_transition_ready(transition.transition_id)
    }
}

fn evaluate_timeline_render_plan_with_effects(
    source: &dyn RenderPlanSource,
    request: TimelineEvaluationRequest,
    effects: &mut dyn ClipEffectResolver,
) -> Result<TimelineRenderPlan> {
    let expected_time_base = source.source_time_base();
    let field_time_base = Rational::new(
        expected_time_base.num,
        expected_time_base
            .den
            .checked_mul(2)
            .ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "evaluate_timeline_render_plan".to_owned(),
                reason: format!(
                    "Sequence {} field evaluation grid overflowed",
                    source.source_sequence_id()
                ),
            })?,
    );
    if request.position.time_base != expected_time_base
        && request.position.time_base != field_time_base
    {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "evaluate_timeline_render_plan".to_owned(),
            reason: format!(
                "evaluation position grid {} does not match Sequence {} frame grid {} or field grid {}",
                request.position.time_base,
                source.source_sequence_id(),
                expected_time_base,
                field_time_base,
            ),
        });
    }
    if request.position.frame < 0 {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "evaluate_timeline_render_plan".to_owned(),
            reason: format!(
                "evaluation position frame {} is negative for Sequence {}",
                request.position.frame,
                source.source_sequence_id()
            ),
        });
    }
    let current_time = TimelineTime::from_frame_position(request.position)?;
    let active = source.flat_visual_items_at(current_time)?;
    let mut diagnostics = TimelineEvaluationDiagnostics {
        active_visual_items: active.len(),
        ..Default::default()
    };
    let mut elements = Vec::with_capacity(active.len());

    for item in active {
        match item {
            FlatVisualItem::Clip(clip) => {
                if let Some(element) = compile_flat_clip(
                    clip,
                    TimelineClipEndpointContext::Ordinary,
                    source,
                    request.position.frame,
                    &mut diagnostics,
                    effects,
                )? {
                    elements.push(element);
                }
            }
            FlatVisualItem::Transition(transition) => {
                elements.push(compile_transition(
                    *transition,
                    source,
                    request.position.frame,
                    &mut diagnostics,
                    effects,
                )?);
            }
        }
    }

    if let Some(effect_graph) = effects.evaluate_timeline_grade(current_time)? {
        elements.push(TimelineRenderPlanElement::TimelineGrade(
            TimelineGradePlan { effect_graph, frame_seed: request.position.frame },
        ));
    }

    diagnostics.emitted_elements = elements.len();

    Ok(TimelineRenderPlan {
        position: request.position,
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
    effects: &mut dyn ClipEffectResolver,
) -> Result<TimelineRenderPlanElement> {
    effects.admit_transition(&transition)?;
    match &transition.definition.definition {
        FlatVideoTransitionDefinition::CrossDissolve => {
            let left = compile_transition_input(
                transition.left,
                TimelineClipEndpointContext::TransitionLeft {
                    transition_id: transition.transition_id,
                },
                source,
                timeline_frame,
                diagnostics,
                effects,
            )?;
            let right = compile_transition_input(
                transition.right,
                TimelineClipEndpointContext::TransitionRight {
                    transition_id: transition.transition_id,
                },
                source,
                timeline_frame,
                diagnostics,
                effects,
            )?;
            let progress = transition.progress.normalized().ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "compile_video_transition".to_owned(),
                    reason: format!(
                        "video Transition {} has invalid progress coordinates",
                        transition.transition_id
                    ),
                }
            })?;
            Ok(TimelineRenderPlanElement::CrossDissolve(Box::new(
                TimelineCrossDissolvePlan {
                    transition_id: transition.transition_id,
                    left,
                    right,
                    progress,
                },
            )))
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

#[cfg(test)]
fn validate_flat_transition_definition(transition: &FlatVideoTransition) -> Result<()> {
    match &transition.definition.definition {
        FlatVideoTransitionDefinition::CrossDissolve
            if transition.definition.properties.iter().next().is_none()
                && transition
                    .definition
                    .params
                    .as_object()
                    .is_some_and(|params| params.is_empty()) =>
        {
            Ok(())
        }
        FlatVideoTransitionDefinition::CrossDissolve => Err(MondrianError::WorkflowStepFailed {
            step_id: "compile_video_transition".to_owned(),
            reason: format!(
                "Cross Dissolve {} carries unsupported definition state",
                transition.transition_id
            ),
        }),
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
    endpoint: TimelineClipEndpointContext,
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
    diagnostics: &mut TimelineEvaluationDiagnostics,
    effects: &mut dyn ClipEffectResolver,
) -> Result<TimelineTransitionInputPlan> {
    if clip.is_disabled || clip.opacity.clamp(0.0, 1.0) <= 0.0 {
        return Ok(TimelineTransitionInputPlan::Transparent);
    }
    let element = compile_flat_clip(clip, endpoint, source, timeline_frame, diagnostics, effects)?
        .ok_or_else(|| MondrianError::WorkflowStepFailed {
            step_id: "compile_video_transition".to_owned(),
            reason: "Transition endpoint did not produce a visual input".to_owned(),
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
        TimelineRenderPlanElement::TimelineGrade(_) => {
            unreachable!("a Clip cannot lower itself into the Timeline Grade")
        }
    }
}

fn compile_flat_clip(
    ac: FlatActiveClip,
    endpoint: TimelineClipEndpointContext,
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
    diagnostics: &mut TimelineEvaluationDiagnostics,
    effects: &mut dyn ClipEffectResolver,
) -> Result<Option<TimelineRenderPlanElement>> {
    let opacity = ac.opacity.clamp(0.0, 1.0);
    if ac.is_disabled || opacity <= 0.0 {
        diagnostics.skipped_zero_opacity += 1;
        return Ok(None);
    }

    let effect_graph = effects.evaluate(&ac)?;
    let frame_seed = timeline_frame;
    let placement = TimelineClipExecutionRef {
        sequence_id: source.source_sequence_id(),
        sequence_revision: source.source_sequence_revision(),
        clip_id: ac.clip_id,
        clip_time: ac.clip_time,
        endpoint,
    };
    Ok(Some(match ac.content {
        ClipContent::NestedSequence { sequence_id, color_processing } => {
            TimelineRenderPlanElement::NestedSequence(TimelineNestedSequencePlan {
                placement,
                sequence_id,
                source_sample: ac.source_sample,
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
                placement,
                effect_graph,
                opacity,
                blend_mode: ac.blend_mode,
                frame_seed,
            })
        }
        ClipContent::SolidColor { color, .. } => {
            TimelineRenderPlanElement::SolidColor(TimelineSolidColorPlan {
                placement,
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
                placement,
                title,
                opacity,
                blend_mode: ac.blend_mode,
                transform: ac.transform_matrix,
                effect_graph,
                frame_seed,
            })
        }
        ClipContent::Media { asset_id, interpretation } => {
            let source_sample = if let Some(frame_rate) = interpretation.frame_rate_override {
                mondrian_core::SourceSampleTarget::covering(TimelineTime::from_frame_position(
                    ac.source_sample.to_frame_position(frame_rate)?,
                )?)
            } else {
                ac.source_sample
            };
            TimelineRenderPlanElement::Media(TimelineMediaPlan {
                placement,
                asset_id,
                color_space_override: interpretation.color_space_override,
                pixel_aspect_ratio_override: interpretation.pixel_aspect_ratio_override,
                field_order_override: interpretation.field_order_override,
                alpha_interpretation: interpretation.alpha,
                frame_rate_override: interpretation.frame_rate_override,
                source_sample,
                opacity,
                blend_mode: ac.blend_mode,
                transform: ac.transform_matrix,
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
    use mondrian_core::TimeScale;
    use mondrian_timeline::clip::{Clip, Transform2D};
    use mondrian_timeline::sequence::Sequence;
    use mondrian_timeline::track::Track;
    use mondrian_timeline::VideoTransition;

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("valid test time")
    }

    fn fp(frame: i64, time_base: Rational) -> FramePosition {
        FramePosition::new(frame, time_base)
    }

    fn analysis_elements(
        source: &dyn RenderPlanSource,
        timeline_frame: i64,
    ) -> Vec<TimelineRenderPlanElement> {
        evaluate_timeline_render_plan(
            source,
            TimelineEvaluationRequest::analysis(fp(timeline_frame, source.source_time_base())),
        )
        .expect("evaluate timeline")
        .elements
    }

    #[test]
    fn evaluation_request_preview_carries_interactive_contract() {
        let position = fp(42, Rational::new(1, 24));
        let request = TimelineEvaluationRequest::preview(position, 0.5);

        assert_eq!(request.position, position);
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
        let request = TimelineEvaluationRequest::export(fp(12, Rational::new(1001, 30_000)));

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
    fn evaluation_rejects_a_position_on_a_different_grid() {
        let sequence = Sequence::new("wrong evaluation grid");
        let equivalent_but_not_identical =
            Rational::new(sequence.time_base().num * 2, sequence.time_base().den * 2);

        let error = evaluate_timeline_render_plan(
            &sequence,
            TimelineEvaluationRequest::analysis(fp(0, equivalent_but_not_identical)),
        )
        .expect_err("the request grid must match exactly");

        assert!(error.to_string().contains("does not match"));
    }

    #[test]
    fn evaluation_rejects_a_negative_frame_instead_of_clamping_it() {
        let sequence = Sequence::new("negative evaluation position");

        let error = evaluate_timeline_render_plan(
            &sequence,
            TimelineEvaluationRequest::analysis(fp(-1, sequence.time_base())),
        )
        .expect_err("negative evaluation must fail closed");

        assert!(error.to_string().contains("negative"));
    }

    #[test]
    fn sampled_extent_projection_preserves_authored_fit_across_preview_and_proxy_sizes() {
        let source_authoring = Resolution { width: 3840, height: 2160 };
        let output_authoring = Resolution { width: 1920, height: 1080 };
        let cases = [
            (
                Resolution { width: 3840, height: 2160 },
                Resolution { width: 1920, height: 1080 },
            ),
            (
                Resolution { width: 1920, height: 1080 },
                Resolution { width: 960, height: 540 },
            ),
            (
                Resolution { width: 1280, height: 720 },
                Resolution { width: 480, height: 270 },
            ),
            (
                Resolution { width: 960, height: 540 },
                Resolution { width: 480, height: 270 },
            ),
        ];

        for (source_sampled, output_sampled) in cases {
            let projected = project_affine_to_sampled_extents(
                [0.5, 0.0, 0.0, 0.0, 0.5, 0.0],
                source_authoring,
                source_sampled,
                output_authoring,
                output_sampled,
            )
            .expect("valid sampled extents");

            let bottom_right = glam::Vec2::new(
                projected[0] * source_sampled.width as f32
                    + projected[1] * source_sampled.height as f32
                    + projected[2],
                projected[3] * source_sampled.width as f32
                    + projected[4] * source_sampled.height as f32
                    + projected[5],
            );
            assert!((bottom_right.x - output_sampled.width as f32).abs() < 0.01);
            assert!((bottom_right.y - output_sampled.height as f32).abs() < 0.01);
        }
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
        title.set_source_origin(tt(100, tb)).expect("set source origin");
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

        let preview = evaluate_timeline_render_plan(
            &seq,
            TimelineEvaluationRequest::preview(fp(20, tb), 0.5),
        )
        .expect("preview plan");
        let export =
            evaluate_timeline_render_plan(&seq, TimelineEvaluationRequest::export(fp(20, tb)))
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
        interpretation.field_order_override = Some(FieldOrder::Progressive);
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
        assert_eq!(media.field_order_override, Some(FieldOrder::Progressive));
        assert_eq!(
            media.alpha_interpretation,
            AlphaInterpretation::Premultiplied
        );
        assert_eq!(media.transform, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
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
            media.source_sample,
            mondrian_core::SourceSampleTarget::covering(
                TimelineTime::new(3, 5).expect("exact source time"),
            )
        );
    }

    #[test]
    fn render_plan_preserves_exact_source_time_without_an_override_grid() {
        let mut seq = Sequence::new("render-plan-native-source-time");
        let tb = seq.time_base();
        let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
        clip.set_source_origin(TimelineTime::new(1, 7).expect("exact source offset"))
            .expect("set source origin");
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let plan = analysis_elements(&seq, 15);
        let TimelineRenderPlanElement::Media(media) = &plan[0] else {
            panic!("expected media plan");
        };
        assert_eq!(
            media.source_sample,
            mondrian_core::SourceSampleTarget::covering(
                TimelineTime::new(26, 35).expect("exact source time"),
            )
        );
    }

    #[test]
    fn reverse_render_plan_preserves_strict_predecessor_until_decode_grid() {
        let mut sequence = Sequence::new("reverse render plan");
        let time_base = sequence.time_base();
        let mut clip =
            Clip::new(AssetId::new(), tt(0, time_base), tt(20, time_base)).expect("valid Clip");
        clip.set_constant_source_time_map(
            TimelineTime::new(1, 1).expect("exclusive reverse origin"),
            TimeScale::NEGATIVE_ONE,
        )
        .expect("reverse map");
        sequence.video_tracks[0].add_clip(clip).expect("add Clip");

        let plan = analysis_elements(&sequence, 0);
        let TimelineRenderPlanElement::Media(media) = &plan[0] else {
            panic!("expected media plan");
        };
        assert_eq!(
            media.source_sample,
            mondrian_core::SourceSampleTarget::strict_predecessor(
                TimelineTime::new(1, 1).expect("exclusive reverse origin"),
            )
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
        ]
        .into();
        seq.video_tracks[1].blend_mode = BlendMode::Screen;
        seq.video_tracks[3].blend_mode = BlendMode::Multiply;
        let tb = seq.time_base();

        let media_asset = AssetId::new();
        let mut media = Clip::new(media_asset, tt(0, tb), tt(30, tb)).expect("valid clip");
        media.set_source_origin(tt(3, tb)).expect("set source origin");
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
        nested.set_source_origin(tt(20, tb)).expect("set source origin");
        let ClipContent::NestedSequence { color_processing, .. } = &mut nested.content else {
            panic!("expected nested Clip content");
        };
        *color_processing = NestedColorProcessing::ForceParentWorkingSpace;
        seq.video_tracks[2].add_clip(nested).expect("add nested");

        let adjustment = Clip::new_adjustment_layer(AssetId::new(), tt(0, tb), tt(30, tb))
            .expect("valid adjustment clip");
        seq.video_tracks[3].add_clip(adjustment).expect("add adjustment");

        let preview = evaluate_timeline_render_plan(
            &seq,
            TimelineEvaluationRequest::preview(fp(12, tb), 0.25),
        )
        .expect("preview plan");
        let export =
            evaluate_timeline_render_plan(&seq, TimelineEvaluationRequest::export(fp(12, tb)))
                .expect("export plan");

        assert_eq!(preview.position, export.position);
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

        let plan =
            evaluate_timeline_render_plan(&seq, TimelineEvaluationRequest::preview(fp(4, tb), 0.5))
                .expect("preview plan");

        assert_eq!(plan.position, fp(4, tb));
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
        left.set_source_origin(tt(5, time_base)).expect("set source origin");
        let mut right =
            Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right");
        right.set_source_origin(tt(20, time_base)).expect("set source origin");
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

        let plan = evaluate_timeline_render_plan(
            &sequence,
            TimelineEvaluationRequest::export(fp(10, time_base)),
        )
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
        assert_eq!(left.source_sample.time(), tt(15, time_base));
        assert_eq!(right.source_sample.time(), tt(20, time_base));

        let start = evaluate_timeline_render_plan(
            &sequence,
            TimelineEvaluationRequest::export(fp(8, time_base)),
        )
        .expect("transition start plan");
        let TimelineRenderPlanElement::CrossDissolve(start) = &start.elements[0] else {
            panic!("expected Cross Dissolve at start");
        };
        assert_eq!(start.progress, 0.0);
        let TimelineTransitionInputPlan::Media(right) = &start.right else {
            panic!("right input must be media");
        };
        assert_eq!(right.source_sample.time(), tt(18, time_base));
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

        let error = evaluate_timeline_render_plan(
            &sequence,
            TimelineEvaluationRequest::preview(fp(10, time_base), 1.0),
        )
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
        TimelineGrade {
            graph_signature: u64,
            frame_seed: i64,
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
                    source_time: media.source_sample.time(),
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
                        source_time: nested.source_sample.time(),
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
                TimelineRenderPlanElement::TimelineGrade(grade) => {
                    RenderPlanSemanticElement::TimelineGrade {
                        graph_signature: grade.effect_graph.signature_hash(),
                        frame_seed: grade.frame_seed,
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
                source_time: media.source_sample.time(),
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
                    source_time: nested.source_sample.time(),
                }
            }
        }
    }
}
