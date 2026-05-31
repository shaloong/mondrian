use mondrian_core::{
    timeline_data::{
        AlphaInterpretation, ClipKind, FieldOrder, NestedColorProcessing,
        PixelAspectRatio, RenderPlanSource,
    },
    types::{AssetId, BlendMode, Color, ColorSpace, Rational, SequenceId, TimeCode},
};
use mondrian_effects::CompiledEffectGraph;
use std::sync::Arc;

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
    pub source_secs: f64,
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
    pub working_color_space: ColorSpace,
    pub output_color_space: ColorSpace,
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
    working_color_space: ColorSpace,
    output_color_space: ColorSpace,
) -> Vec<TimelineColorDiagnostic> {
    build_timeline_render_plan(source, timeline_frame)
        .into_iter()
        .filter_map(|element| match element {
            TimelineRenderPlanElement::Media(media) => Some(TimelineColorDiagnostic {
                asset_id: media.asset_id,
                input_color_space_override: media.color_space_override,
                working_color_space,
                output_color_space,
                tone_map: media.auto_tone_map,
                pixel_aspect_ratio_override: media.pixel_aspect_ratio_override,
                field_order_override: media.field_order_override,
                alpha_interpretation: media.alpha_interpretation,
                source_frame: media.source_frame,
                source_secs: media.source_secs,
            }),
            _ => None,
        })
        .collect()
}

pub fn build_timeline_render_plan(
    source: &dyn RenderPlanSource,
    timeline_frame: i64,
) -> Vec<TimelineRenderPlanElement> {
    let time_base = source.source_time_base();
    let current = TimeCode::new(timeline_frame, time_base);
    let active = source.flat_active_clips_at(current);
    let mut elements = Vec::with_capacity(active.len());

    for ac in active {
        let opacity = ac.opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }

        let effect_graph =
            mondrian_effects::compile_clip_effect_graph(&ac.effects, &ac.masks, current);

        match ac.kind {
            ClipKind::NestedSequence => {
                let Some(sequence_id) = ac.nested_sequence_id else { continue };
                let Some(eg) = effect_graph else { continue };
                elements.push(TimelineRenderPlanElement::NestedSequence(
                    TimelineNestedSequencePlan {
                        sequence_id,
                        source_frame: ac.source_time.frame.max(0),
                        source_secs: ac.source_time.to_secs().max(0.0),
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
                let Some(eg) = effect_graph else { continue };
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
                let Some(eg) = effect_graph else { continue };
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
                let Some(eg) = effect_graph else { continue };
                let source_frame = ac.source_time.frame.max(0);
                let source_time_base = ac
                    .interpretation
                    .frame_rate_override
                    .map(|fps| Rational::new(fps.den, fps.num))
                    .unwrap_or(ac.source_time.time_base);
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
                    source_secs: TimeCode::new(source_frame, source_time_base).to_secs().max(0.0),
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

    elements
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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::{AssetId, Resolution};
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::Sequence;

    #[test]
    fn render_plan_uses_track_blend_mode_for_media_and_adjustment() {
        let mut seq = Sequence::new("render-plan-blend");
        let tb = seq.time_base();
        seq.settings.resolution = Resolution::FHD;
        seq.video_tracks[0].blend_mode = BlendMode::Screen;
        seq.video_tracks[1].blend_mode = BlendMode::Multiply;

        let media = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        let adjustment =
            Clip::new_adjustment_layer(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        seq.video_tracks[0].add_clip(media).expect("add media");
        seq.video_tracks[1].add_clip(adjustment).expect("add adjustment");

        let plan = build_timeline_render_plan(&seq, 5);
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

        let mut media = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        media.blend_mode = Some(BlendMode::HardLight);
        seq.video_tracks[0].add_clip(media).expect("add media");

        let plan = build_timeline_render_plan(&seq, 5);
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
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        clip.interpretation.color_space_override = Some(mondrian_core::types::ColorSpace::Srgb);
        clip.interpretation.pixel_aspect_ratio_override = Some(PixelAspectRatio::Anamorphic2x);
        clip.interpretation.field_order_override = Some(FieldOrder::UpperFirst);
        clip.interpretation.alpha = AlphaInterpretation::Premultiplied;
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let plan = build_timeline_render_plan(&seq, 0);
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
        assert_eq!(media.alpha_interpretation, AlphaInterpretation::Premultiplied);
        assert!((media.transform[0] - 2.0).abs() < 1.0e-6);
    }

    #[test]
    fn render_plan_uses_frame_rate_override_for_decode_seconds() {
        let mut seq = Sequence::new("render-plan-frame-rate-override");
        let tb = seq.time_base();
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        clip.interpretation.frame_rate_override = Some(Rational::FPS_30);
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let plan = build_timeline_render_plan(&seq, 15);
        let TimelineRenderPlanElement::Media(media) = &plan[0] else {
            panic!("expected media plan");
        };
        assert_eq!(media.source_frame, 15);
        assert_eq!(media.source_time_base, Rational::new(1, 30));
        assert!((media.source_secs - 0.5).abs() < 1.0e-9);
    }

    #[test]
    fn color_diagnostics_expose_media_interpretation_and_sequence_output() {
        let mut seq = Sequence::new("color-diagnostics");
        let tb = seq.time_base();
        seq.settings.color_space = ColorSpace::Rec2020;
        seq.settings.color_management.output_color_space = ColorSpace::Rec2100Pq;
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        let asset_id = clip.asset_id;
        clip.interpretation.color_space_override = Some(ColorSpace::AppleLog);
        clip.interpretation.pixel_aspect_ratio_override = Some(PixelAspectRatio::DvcproHd);
        clip.interpretation.field_order_override = Some(FieldOrder::LowerFirst);
        clip.interpretation.alpha = AlphaInterpretation::Ignore;
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let diagnostics = collect_timeline_color_diagnostics(
            &seq,
            4,
            seq.settings.color_space,
            seq.settings.color_management.output_color_space,
        );
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.asset_id, asset_id);
        assert_eq!(
            diagnostic.input_color_space_override,
            Some(ColorSpace::AppleLog)
        );
        assert_eq!(diagnostic.working_color_space, ColorSpace::Rec2020);
        assert_eq!(diagnostic.output_color_space, ColorSpace::Rec2100Pq);
        assert_eq!(
            diagnostic.pixel_aspect_ratio_override,
            Some(PixelAspectRatio::DvcproHd)
        );
        assert_eq!(diagnostic.field_order_override, Some(FieldOrder::LowerFirst));
        assert_eq!(diagnostic.alpha_interpretation, AlphaInterpretation::Ignore);
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
            .add_clip(Clip::new_nested_sequence(
                child_id,
                TimeCode::new(0, tb),
                TimeCode::new(20, tb),
                Some("child".to_string()),
            ))
            .expect("add nested sequence");

        let plan = build_timeline_render_plan(&seq, 0);
        let TimelineRenderPlanElement::NestedSequence(nested) = &plan[0] else {
            panic!("expected nested sequence plan");
        };
        assert_eq!(
            nested.nested_processing,
            NestedColorProcessing::BakeChildOutputTransform
        );
    }
}
