//! Integration smoke tests for the core pipeline:
//! Sequence → Clip → Effect Graph → Timeline Render Plan.

use mondrian_core::types::{Color, TimeCode};
use mondrian_effects::EffectType;
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::Sequence;

#[test]
fn create_sequence_with_default_tracks() {
    let seq = Sequence::new("Integration Test Seq");
    assert!(!seq.video_tracks.is_empty());
    assert!(!seq.audio_tracks.is_empty());
}

#[test]
fn add_solid_color_clip_and_query_active() {
    let mut seq = Sequence::new("Solid Color Test");
    let asset_id = mondrian_core::types::AssetId::new();
    let time_base = seq.time_base();

    let clip = Clip::new_solid_color(
        asset_id,
        Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
        TimeCode::new(0, time_base),
        TimeCode::new(100, time_base),
    );

    seq.video_tracks[0].add_clip(clip);

    let active = seq.active_clips_at(TimeCode::new(50, time_base));
    assert_eq!(active.len(), 1);
    assert!(active[0].clip.is_solid_color());
}

#[test]
fn clip_with_effect_graph_compiles() {
    let mut seq = Sequence::new("Effects Test");
    let asset_id = mondrian_core::types::AssetId::new();
    let time_base = seq.time_base();

    let mut clip = Clip::new_solid_color(
        asset_id,
        Color { r: 0.5, g: 0.5, b: 0.5, a: 1.0 },
        TimeCode::new(0, time_base),
        TimeCode::new(100, time_base),
    );

    clip.add_effect(EffectType::GaussianBlur);

    seq.video_tracks[0].add_clip(clip);

    let active = seq.active_clips_at(TimeCode::new(50, time_base));
    assert_eq!(active.len(), 1);

    let graph = active[0]
        .clip
        .evaluate_compiled_effect_graph(TimeCode::new(50, time_base));
    assert!(
        graph.is_some(),
        "Effect graph should compile for a clip with GaussianBlur"
    );
}

#[test]
fn build_render_plan_from_sequence() {
    let mut seq = Sequence::new("Render Plan Test");
    let asset_id = mondrian_core::types::AssetId::new();
    let time_base = seq.time_base();

    let clip = Clip::new_solid_color(
        asset_id,
        Color { r: 0.2, g: 0.4, b: 0.8, a: 1.0 },
        TimeCode::new(0, time_base),
        TimeCode::new(60, time_base),
    );

    seq.video_tracks[0].add_clip(clip);

    let plan = mondrian_renderer::timeline_render_plan::build_timeline_render_plan(&seq, 30);

    assert!(
        !plan.is_empty(),
        "Render plan should include the solid color clip at frame 30"
    );
}

#[test]
fn sequence_respects_clip_disabled() {
    let mut seq = Sequence::new("Disabled Clip Test");
    let asset_id = mondrian_core::types::AssetId::new();
    let time_base = seq.time_base();

    let mut clip = Clip::new_solid_color(
        asset_id,
        Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
        TimeCode::new(0, time_base),
        TimeCode::new(100, time_base),
    );
    clip.is_disabled = true;

    seq.video_tracks[0].add_clip(clip);

    let active = seq.active_clips_at(TimeCode::new(50, time_base));
    assert!(
        active.is_empty(),
        "Disabled clip should not appear in active clips"
    );
}

#[test]
fn multiple_tracks_composite_order() {
    let mut seq = Sequence::new("Multi-Track Test");
    let asset1 = mondrian_core::types::AssetId::new();
    let asset2 = mondrian_core::types::AssetId::new();
    let time_base = seq.time_base();

    let clip1 = Clip::new_solid_color(
        asset1,
        Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
        TimeCode::new(0, time_base),
        TimeCode::new(100, time_base),
    );
    let clip2 = Clip::new_solid_color(
        asset2,
        Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
        TimeCode::new(0, time_base),
        TimeCode::new(100, time_base),
    );

    seq.video_tracks[0].add_clip(clip1);
    seq.video_tracks[1].add_clip(clip2);

    let active = seq.active_clips_at(TimeCode::new(50, time_base));
    // Both tracks should contribute active clips
    assert_eq!(active.len(), 2);
    // Tracks are enumerated bottom-to-top: V1 (track 0) first, V2 (track 1) on top
    assert_eq!(active[0].track_index, 0);
    assert_eq!(active[1].track_index, 1);
}

#[test]
fn adjustment_layer_is_included_in_active_clips() {
    let mut seq = Sequence::new("Adjustment Test");
    let asset_id = mondrian_core::types::AssetId::new();
    let adj_id = mondrian_core::types::AssetId::new();
    let time_base = seq.time_base();

    let media_clip = Clip::new_solid_color(
        asset_id,
        Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
        TimeCode::new(0, time_base),
        TimeCode::new(100, time_base),
    );
    let adj_clip = Clip::new_adjustment_layer(
        adj_id,
        TimeCode::new(0, time_base),
        TimeCode::new(100, time_base),
    );

    seq.video_tracks[0].add_clip(media_clip);
    seq.video_tracks[1].add_clip(adj_clip);

    let active = seq.active_clips_at(TimeCode::new(50, time_base));
    assert!(active.iter().any(|a| a.clip.is_adjustment_layer()));
}
