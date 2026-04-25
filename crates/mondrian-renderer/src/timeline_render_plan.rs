use mondrian_core::types::{AssetId, BlendMode, ColorSpace, Rational, SequenceId, TimeCode};
use mondrian_effects::CompiledEffectGraph;
use mondrian_timeline::sequence::Sequence;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TimelineMediaPlan {
    pub asset_id: AssetId,
    pub color_space_override: Option<ColorSpace>,
    pub source_frame: i64,
    pub source_secs: f64,
    pub source_time_base: Rational,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineAdjustmentPlan {
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineNestedSequencePlan {
    pub sequence_id: SequenceId,
    pub source_frame: i64,
    pub source_secs: f64,
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
    NestedSequence(TimelineNestedSequencePlan),
}

pub fn build_timeline_render_plan(
    sequence: &Sequence,
    timeline_frame: i64,
) -> Vec<TimelineRenderPlanElement> {
    let current = TimeCode::new(timeline_frame, sequence.time_base());
    let active = sequence.active_clips_at(current);
    let mut elements = Vec::with_capacity(active.len());

    for active_clip in active {
        let opacity = active_clip.opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }

        if active_clip.clip.is_nested_sequence() {
            let Some(sequence_id) = active_clip.clip.nested_sequence_id else {
                continue;
            };
            let Some(effect_graph) = active_clip.clip.evaluate_compiled_effect_graph(current)
            else {
                continue;
            };
            elements.push(TimelineRenderPlanElement::NestedSequence(
                TimelineNestedSequencePlan {
                    sequence_id,
                    source_frame: active_clip.source_time.frame.max(0),
                    source_secs: active_clip.source_time.to_secs().max(0.0),
                    opacity,
                    blend_mode: active_clip.blend_mode,
                    transform: mat3_to_affine(active_clip.transform_matrix.to_cols_array()),
                    effect_graph,
                    frame_seed: timeline_frame.max(0),
                },
            ));
            continue;
        }

        if active_clip.clip.is_adjustment_layer() {
            let Some(effect_graph) = active_clip.clip.evaluate_compiled_effect_graph(current)
            else {
                continue;
            };
            elements.push(TimelineRenderPlanElement::Adjustment(
                TimelineAdjustmentPlan {
                    effect_graph,
                    opacity,
                    blend_mode: active_clip.blend_mode,
                    frame_seed: timeline_frame.max(0),
                },
            ));
            continue;
        }

        let Some(effect_graph) = active_clip.clip.evaluate_compiled_effect_graph(current) else {
            continue;
        };
        elements.push(TimelineRenderPlanElement::Media(TimelineMediaPlan {
            asset_id: active_clip.clip.asset_id,
            color_space_override: active_clip.clip.interpretation.color_space_override,
            source_frame: active_clip.source_time.frame.max(0),
            source_secs: active_clip.source_time.to_secs().max(0.0),
            source_time_base: active_clip.source_time.time_base,
            opacity,
            blend_mode: active_clip.blend_mode,
            transform: mat3_to_affine(active_clip.transform_matrix.to_cols_array()),
            effect_graph,
            frame_seed: timeline_frame.max(0),
        }));
    }

    elements
}

pub fn mat3_to_affine(cols: [f32; 9]) -> [f32; 6] {
    [cols[0], cols[3], cols[6], cols[1], cols[4], cols[7]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::{AssetId, Resolution};
    use mondrian_timeline::clip::Clip;

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
    fn render_plan_carries_clip_color_space_override() {
        let mut seq = Sequence::new("render-plan-color-override");
        let tb = seq.time_base();
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        clip.interpretation.color_space_override = Some(mondrian_core::types::ColorSpace::Srgb);
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let plan = build_timeline_render_plan(&seq, 0);
        let TimelineRenderPlanElement::Media(media) = &plan[0] else {
            panic!("expected media plan");
        };
        assert_eq!(
            media.color_space_override,
            Some(mondrian_core::types::ColorSpace::Srgb)
        );
    }
}
