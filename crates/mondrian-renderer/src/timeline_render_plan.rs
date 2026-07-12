use mondrian_core::{
    timeline_data::{
        AlphaInterpretation, ClipKind, FieldOrder, NestedColorProcessing, PixelAspectRatio,
        RenderPlanSource,
    },
    types::{AssetId, BlendMode, Color, ColorSpace, FramePosition, Rational, SequenceId},
    ColorEncodingSpec, FrameRounding, Result, TimelineTime, WorkingColorSpace,
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
    /// Active clips returned by the source at the requested time.
    pub active_clips: usize,
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
    pub source_frame: i64,
    pub source_secs: f64,
    pub source_time_base: Rational,
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

#[derive(Debug, Clone)]
pub struct TimelineNestedSequencePlan {
    pub sequence_id: SequenceId,
    pub source_frame: i64,
    pub source_time: TimelineTime,
    pub nested_processing: NestedColorProcessing,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub enum TimelineRenderPlanElement {
    Media(TimelineMediaPlan),
    Adjustment(TimelineAdjustmentPlan),
    SolidColor(TimelineSolidColorPlan),
    NestedSequence(TimelineNestedSequencePlan),
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
    pub source_frame: i64,
    pub source_secs: f64,
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
            .filter_map(|element| match element {
                TimelineRenderPlanElement::Media(media) => Some(TimelineColorDiagnostic {
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
                    source_frame: media.source_frame,
                    source_secs: media.source_secs,
                }),
                _ => None,
            })
            .collect(),
    )
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
    let active = source.flat_active_clips_at(current_time)?;
    let mut diagnostics =
        TimelineEvaluationDiagnostics { active_clips: active.len(), ..Default::default() };
    let mut elements = Vec::with_capacity(active.len());

    for ac in active {
        let opacity = ac.opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            diagnostics.skipped_zero_opacity += 1;
            continue;
        }

        let effect_graph =
            mondrian_effects::compile_clip_effect_graph(&ac.effects, &ac.masks, current_time);

        match ac.kind {
            ClipKind::NestedSequence => {
                let Some(sequence_id) = ac.nested_sequence_id else {
                    diagnostics.skipped_unrenderable += 1;
                    continue;
                };
                let Some(eg) = effect_graph else {
                    diagnostics.skipped_unrenderable += 1;
                    continue;
                };
                elements.push(TimelineRenderPlanElement::NestedSequence(
                    TimelineNestedSequencePlan {
                        sequence_id,
                        source_frame: ac
                            .source_time
                            .to_frame_position(
                                Rational::new(time_base.den, time_base.num),
                                FrameRounding::Floor,
                            )?
                            .frame
                            .max(0),
                        source_time: ac.source_time.max(TimelineTime::ZERO),
                        nested_processing: source.nested_color_processing(),
                        opacity,
                        blend_mode: ac.blend_mode,
                        transform: ac.transform_matrix,
                        effect_graph: eg,
                        frame_seed: timeline_frame.max(0),
                    },
                ));
            }
            ClipKind::AdjustmentLayer => {
                let Some(eg) = effect_graph else {
                    diagnostics.skipped_unrenderable += 1;
                    continue;
                };
                elements.push(TimelineRenderPlanElement::Adjustment(
                    TimelineAdjustmentPlan {
                        effect_graph: eg,
                        opacity,
                        blend_mode: ac.blend_mode,
                        frame_seed: timeline_frame.max(0),
                    },
                ));
            }
            ClipKind::SolidColor => {
                let Some(eg) = effect_graph else {
                    diagnostics.skipped_unrenderable += 1;
                    continue;
                };
                let color = ac.solid_color.unwrap_or(Color::BLACK);
                elements.push(TimelineRenderPlanElement::SolidColor(
                    TimelineSolidColorPlan {
                        color,
                        opacity,
                        blend_mode: ac.blend_mode,
                        transform: ac.transform_matrix,
                        effect_graph: eg,
                        frame_seed: timeline_frame.max(0),
                    },
                ));
            }
            ClipKind::Media => {
                let Some(eg) = effect_graph else {
                    diagnostics.skipped_unrenderable += 1;
                    continue;
                };
                let source_frame_rate = ac
                    .interpretation
                    .frame_rate_override
                    .unwrap_or(Rational::new(time_base.den, time_base.num));
                let source_position =
                    ac.source_time.to_frame_position(source_frame_rate, FrameRounding::Floor)?;
                let source_frame = source_position.frame.max(0);
                let source_time_base = source_position.time_base;
                let transform = apply_pixel_aspect_to_affine(
                    ac.transform_matrix,
                    ac.interpretation.pixel_aspect_ratio_override,
                );
                elements.push(TimelineRenderPlanElement::Media(TimelineMediaPlan {
                    asset_id: ac.asset_id,
                    color_space_override: ac.interpretation.color_space_override,
                    pixel_aspect_ratio_override: ac.interpretation.pixel_aspect_ratio_override,
                    field_order_override: ac.interpretation.field_order_override,
                    alpha_interpretation: ac.interpretation.alpha,
                    source_frame,
                    source_secs: (source_frame as f64 * source_time_base.to_f64()).max(0.0),
                    source_time_base,
                    opacity,
                    blend_mode: ac.blend_mode,
                    transform,
                    effect_graph: eg,
                    frame_seed: timeline_frame.max(0),
                    auto_tone_map: source.auto_tone_map_media(),
                }));
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

pub fn mat3_to_affine(cols: [f32; 9]) -> [f32; 6] {
    [cols[0], cols[3], cols[6], cols[1], cols[4], cols[7]]
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
    use mondrian_core::automation::{PropertyMutation, PropertyValue};
    use mondrian_core::types::{AssetId, Resolution};
    use mondrian_timeline::clip::{Clip, Transform2D};
    use mondrian_timeline::sequence::Sequence;
    use mondrian_timeline::track::Track;

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
    fn render_plan_carries_clip_media_interpretation() {
        let mut seq = Sequence::new("render-plan-interpretation");
        let tb = seq.time_base();
        let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        clip.interpretation.color_space_override = Some(mondrian_core::types::ColorSpace::Srgb);
        clip.interpretation.pixel_aspect_ratio_override = Some(PixelAspectRatio::Anamorphic2x);
        clip.interpretation.field_order_override = Some(FieldOrder::UpperFirst);
        clip.interpretation.alpha = AlphaInterpretation::Premultiplied;
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
        clip.interpretation.frame_rate_override = Some(Rational::FPS_30);
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let plan = analysis_elements(&seq, 15);
        let TimelineRenderPlanElement::Media(media) = &plan[0] else {
            panic!("expected media plan");
        };
        assert_eq!(media.source_frame, 18);
        assert_eq!(media.source_time_base, Rational::new(1, 30));
        assert!((media.source_secs - 0.6).abs() < 1.0e-9);
    }

    #[test]
    fn color_diagnostics_expose_media_interpretation_and_sequence_output() {
        let mut seq = Sequence::new("color-diagnostics");
        let tb = seq.time_base();
        seq.settings.working_color_space = mondrian_core::WorkingColorSpace::LinearRec2020;
        seq.settings.color_management.output_color_space = ColorSpace::Rec2100Pq;
        let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        let asset_id = clip.asset_id;
        clip.interpretation.color_space_override = Some(ColorSpace::AppleLog);
        clip.interpretation.pixel_aspect_ratio_override = Some(PixelAspectRatio::DvcproHd);
        clip.interpretation.field_order_override = Some(FieldOrder::LowerFirst);
        clip.interpretation.alpha = AlphaInterpretation::Ignore;
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let diagnostics = collect_timeline_color_diagnostics(
            &seq,
            4,
            seq.settings.working_color_space,
            seq.settings.color_management.output_color_space,
        )
        .expect("collect diagnostics");
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.asset_id, asset_id);
        assert_eq!(
            diagnostic.input_color_space_override,
            Some(ColorSpace::AppleLog)
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
            mondrian_core::ColorEncodingKind::CameraLog
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
    fn render_plan_carries_nested_processing_mode() {
        let mut seq = Sequence::new("render-plan-nested-processing");
        seq.settings.color_management.nested_processing =
            NestedColorProcessing::BakeChildOutputTransform;
        let tb = seq.time_base();
        let child = Sequence::new("child");
        let child_id = child.id;

        seq.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child_id,
                    tt(0, tb),
                    tt(20, tb),
                    Some("child".to_string()),
                )
                .expect("valid nested clip"),
            )
            .expect("add nested sequence");

        let plan = analysis_elements(&seq, 0);
        let TimelineRenderPlanElement::NestedSequence(nested) = &plan[0] else {
            panic!("expected nested sequence plan");
        };
        assert_eq!(
            nested.nested_processing,
            NestedColorProcessing::BakeChildOutputTransform
        );
    }

    #[test]
    fn preview_and_export_requests_preserve_timeline_semantics() {
        let mut seq = Sequence::new("preview-export-contract");
        seq.settings.color_management.nested_processing =
            NestedColorProcessing::BakeChildOutputTransform;
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
        media.interpretation.color_space_override = Some(ColorSpace::Srgb);
        media.interpretation.pixel_aspect_ratio_override = Some(PixelAspectRatio::Anamorphic2x);
        media.interpretation.field_order_override = Some(FieldOrder::UpperFirst);
        media.interpretation.alpha = AlphaInterpretation::Premultiplied;
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
        assert_eq!(plan.diagnostics.active_clips, 2);
        assert_eq!(plan.diagnostics.emitted_elements, 1);
        assert_eq!(plan.diagnostics.skipped_zero_opacity, 1);
        assert_eq!(plan.len(), 1);
    }

    #[derive(Debug, PartialEq)]
    enum RenderPlanSemanticElement {
        Media {
            asset_id: AssetId,
            color_space_override: Option<ColorSpace>,
            pixel_aspect_ratio_override: Option<PixelAspectRatio>,
            field_order_override: Option<FieldOrder>,
            alpha_interpretation: AlphaInterpretation,
            source_frame: i64,
            source_micros: i64,
            source_time_base: Rational,
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
        NestedSequence {
            sequence_id: SequenceId,
            source_frame: i64,
            source_micros: i64,
            nested_processing: NestedColorProcessing,
            opacity: f32,
            blend_mode: BlendMode,
            transform: [f32; 6],
            frame_seed: i64,
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
                    source_frame: media.source_frame,
                    source_micros: micros(media.source_secs),
                    source_time_base: media.source_time_base,
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
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    RenderPlanSemanticElement::NestedSequence {
                        sequence_id: nested.sequence_id,
                        source_frame: nested.source_frame,
                        source_micros: micros(nested.source_time.to_f64()),
                        nested_processing: nested.nested_processing,
                        opacity: nested.opacity,
                        blend_mode: nested.blend_mode,
                        transform: nested.transform,
                        frame_seed: nested.frame_seed,
                    }
                }
            })
            .collect()
    }

    fn micros(seconds: f64) -> i64 {
        (seconds * 1_000_000.0).round() as i64
    }
}
