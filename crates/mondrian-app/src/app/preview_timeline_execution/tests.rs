use mondrian_core::types::Rational;
use mondrian_core::{ensure_mondrian_default_ocio_loaded, Color};
use mondrian_timeline::clip::Clip;

use super::*;

fn tt(frame: i64, time_base: Rational) -> mondrian_core::TimelineTime {
    mondrian_core::TimelineTime::new(
        frame.checked_mul(time_base.num).expect("test time fits"),
        time_base.den,
    )
    .expect("valid test time")
}

fn solid_sequence(name: &str, color: Color) -> Sequence {
    let mut sequence = Sequence::new(name);
    let time_base = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_solid_color(AssetId::new(), color, tt(0, time_base), tt(24, time_base))
                .expect("valid solid clip"),
        )
        .expect("insert solid clip");
    sequence
}

fn color_context(sequence: &Sequence) -> ProgramColorContext {
    sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
}

#[test]
fn solid_plan_is_ui_independent_and_has_mandatory_cache_identity() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let sequence = solid_sequence("root", Color::from_rgba8(20, 40, 80, 255));
    let target = Resolution { width: 64, height: 36 };
    let mut unexpected_media = |_| panic!("solid plan must not request media");

    let first = resolve_preview_timeline(
        &sequence,
        &[],
        3,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut unexpected_media,
        &mut |_| panic!("solid plan must not request titles"),
    );
    let PreviewTimelineResolution::Ready(first) = first else {
        panic!("solid Timeline should resolve");
    };
    assert_eq!(first.plan.elements.len(), 1);
    assert!(first.facts.is_empty());

    let mut unexpected_media = |_| panic!("solid plan must not request media");
    let second = resolve_preview_timeline(
        &sequence,
        &[],
        3,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut unexpected_media,
        &mut |_| panic!("solid plan must not request titles"),
    );
    let PreviewTimelineResolution::Ready(second) = second else {
        panic!("solid Timeline should resolve twice");
    };
    assert_eq!(first.plan.cache_key, second.plan.cache_key);
}

#[test]
fn preview_executes_cross_dissolve_through_shared_working_compositor() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let mut sequence = Sequence::new("preview Cross Dissolve");
    let time_base = sequence.time_base();
    let left = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(255, 0, 0, 255),
        tt(0, time_base),
        tt(2, time_base),
    )
    .expect("left solid");
    let right = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(0, 0, 255, 255),
        tt(2, time_base),
        tt(2, time_base),
    )
    .expect("right solid");
    let (left_id, right_id) = (left.id, right.id);
    sequence.video_tracks[0].add_clip(left).expect("left placement");
    sequence.video_tracks[0].add_clip(right).expect("right placement");
    sequence
        .video_transitions
        .push(mondrian_timeline::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(tt(1, time_base), tt(2, time_base))
                .expect("transition range"),
        ));
    sequence.validate_author_identities().expect("valid author graph");
    let target = Resolution { width: 1, height: 1 };
    let context = color_context(&sequence);
    let mut unexpected_media = |_| panic!("solid Transition must not request media");
    let PreviewTimelineResolution::Ready(resolved) = resolve_preview_timeline(
        &sequence,
        &[],
        2,
        target,
        PreviewResolutionScale::Full,
        context.clone(),
        &mut unexpected_media,
        &mut |_| panic!("solid Transition must not request titles"),
    ) else {
        panic!("Cross Dissolve must resolve");
    };
    assert!(matches!(
        resolved.plan.elements.as_slice(),
        [ResolvedPreviewElement::CrossDissolve { progress, .. }] if *progress == 0.5
    ));
    let mut scratch = TimelineCompositeScratch::default();
    let output = composite_resolved_preview_working(
        1,
        1,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        &mut scratch,
    )
    .expect("Preview Cross Dissolve composite");
    let pixel = output.frame.rgba_f32().data[0];
    assert!((pixel[0] - 0.5).abs() < 1.0e-6, "unexpected red: {pixel:?}");
    assert_eq!(pixel[1], 0.0);
    assert!(
        (pixel[2] - 0.5).abs() < 1.0e-6,
        "unexpected blue: {pixel:?}"
    );
    assert_eq!(pixel[3], 1.0);
    assert_eq!(output.composite_diagnostics.float_linear_composites, 1);
}

#[test]
fn nested_sequence_uses_shared_recursion_and_emits_execution_facts() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let child = solid_sequence("child", Color::from_rgba8(48, 120, 220, 255));
    let child_id = child.id;
    let mut parent = Sequence::new("parent");
    let time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(
                child_id,
                tt(0, time_base),
                tt(24, time_base),
                Some("child".to_owned()),
            )
            .expect("valid nested clip"),
        )
        .expect("insert nested clip");
    let mut unexpected_media = |_| panic!("nested solid plan must not request media");

    let resolution = resolve_preview_timeline(
        &parent,
        std::slice::from_ref(&child),
        3,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut unexpected_media,
        &mut |_| panic!("nested solid plan must not request titles"),
    );
    let PreviewTimelineResolution::Ready(resolved) = resolution else {
        panic!("nested Timeline should resolve");
    };
    assert_eq!(resolved.plan.elements.len(), 1);
    assert!(resolved
        .facts
        .iter()
        .any(|fact| matches!(fact, PreviewTimelineExecutionFact::Composite(_))));
    assert!(resolved
        .facts
        .iter()
        .any(|fact| matches!(fact, PreviewTimelineExecutionFact::CpuExecution(_))));
}

#[test]
fn nested_sequence_keeps_its_own_canvas_under_shared_runtime_quality() {
    let mut child = Sequence::new("child media");
    child.settings.resolution = Resolution { width: 1280, height: 720 };
    child.settings.preview.resolution_scale = 0.5;
    child.settings.color.input.auto_tone_map_media = false;
    let asset_id = AssetId::new();
    let child_time_base = child.time_base();
    child.video_tracks[0]
        .add_clip(
            Clip::new(asset_id, tt(0, child_time_base), tt(24, child_time_base))
                .expect("media clip"),
        )
        .expect("insert child media");

    let mut parent = Sequence::new("parent");
    let parent_time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(
                child.id,
                tt(0, parent_time_base),
                tt(24, parent_time_base),
                Some("child media".to_owned()),
            )
            .expect("nested clip"),
        )
        .expect("insert nested clip");

    let demands = collect_preview_timeline_media_demands(
        &parent,
        std::slice::from_ref(&child),
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Quarter,
        color_context(&parent),
    )
    .expect("nested media demands");
    assert_eq!(demands.len(), 1);
    assert_eq!(demands[0].asset_id, asset_id);
    assert!(
        !demands[0].input_color.input_tone_map,
        "nested media input policy must come from the child Sequence, not parent Program Output"
    );
    assert_eq!(
        demands[0].target_resolution,
        Resolution { width: 160, height: 90 }
    );

    let mut observed_resolution = None;
    let mut media = |request: PreviewTimelineMediaRequest| {
        observed_resolution = Some(request.target_resolution);
        PreviewTimelineMediaFrame::Pending
    };
    let result = resolve_preview_timeline(
        &parent,
        std::slice::from_ref(&child),
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Quarter,
        color_context(&parent),
        &mut media,
        &mut |_| panic!("nested media plan must not request titles"),
    );

    assert!(matches!(
        result,
        PreviewTimelineResolution::Pending {
            dependency: PreviewTimelinePendingDependency::Media(pending_id)
        } if pending_id == asset_id
    ));
    assert_eq!(
        observed_resolution,
        Some(Resolution { width: 160, height: 90 })
    );
}

#[test]
fn nested_sequence_projects_exact_time_onto_the_child_evaluation_grid() {
    let mut child = Sequence::new("30 fps child");
    child.settings.frame_rate = Rational::new(30, 1);
    let asset_id = AssetId::new();
    let child_time_base = child.time_base();
    child.video_tracks[0]
        .add_clip(
            Clip::new(asset_id, tt(0, child_time_base), tt(30, child_time_base))
                .expect("child media clip"),
        )
        .expect("insert child media");

    let mut parent = Sequence::new("24 fps parent");
    parent.settings.frame_rate = Rational::new(24, 1);
    let parent_time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(
                child.id,
                tt(0, parent_time_base),
                tt(24, parent_time_base),
                Some("30 fps child".to_owned()),
            )
            .expect("nested clip"),
        )
        .expect("insert nested clip");

    let demands = collect_preview_timeline_media_demands(
        &parent,
        std::slice::from_ref(&child),
        12,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&parent),
    )
    .expect("mixed-rate nested media demands");

    assert_eq!(demands.len(), 1);
    assert_eq!(demands[0].asset_id, asset_id);
    assert_eq!(
        demands[0].source_time,
        mondrian_core::TimelineTime::new(1, 2).expect("exact half second")
    );
}

#[test]
fn media_pending_and_unavailable_are_distinct_terminal_shapes() {
    let mut sequence = Sequence::new("media");
    let asset_id = AssetId::new();
    let time_base = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(Clip::new(asset_id, tt(0, time_base), tt(24, time_base)).expect("media clip"))
        .expect("insert media clip");
    let target = Resolution { width: 64, height: 36 };

    let mut pending = |_| PreviewTimelineMediaFrame::Pending;
    assert!(matches!(
        resolve_preview_timeline(
            &sequence,
            &[],
            0,
            target,
            PreviewResolutionScale::Full,
            color_context(&sequence),
            &mut pending,
            &mut |_| panic!("media plan must not request titles"),
        ),
        PreviewTimelineResolution::Pending {
            dependency: PreviewTimelinePendingDependency::Media(pending_id)
        } if pending_id == asset_id
    ));

    let mut unavailable = |_| PreviewTimelineMediaFrame::Unavailable {
        reason: PreviewUnavailability::blocked(PreviewOutputStage::MediaResolution, "offline"),
    };
    assert!(matches!(
        resolve_preview_timeline(
            &sequence,
            &[],
            0,
            target,
            PreviewResolutionScale::Full,
            color_context(&sequence),
            &mut unavailable,
            &mut |_| panic!("media plan must not request titles"),
        ),
        PreviewTimelineResolution::Unavailable { reason } if reason.detail().contains("offline")
    ));
}

#[test]
fn missing_nested_sequence_is_explicitly_unavailable() {
    let mut parent = Sequence::new("parent");
    let time_base = parent.time_base();
    let missing_id = SequenceId::new();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(missing_id, tt(0, time_base), tt(24, time_base), None)
                .expect("nested clip"),
        )
        .expect("insert nested clip");
    let mut unexpected_media = |_| panic!("missing nested plan must not request media");

    let demand_error = collect_preview_timeline_media_demands(
        &parent,
        &[],
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&parent),
    )
    .expect_err("missing nested demand must fail");
    assert!(demand_error.detail().contains(&missing_id.to_string()));

    assert!(matches!(
        resolve_preview_timeline(
            &parent,
            &[],
            0,
            Resolution { width: 64, height: 36 },
            PreviewResolutionScale::Full,
            color_context(&parent),
            &mut unexpected_media,
            &mut |_| panic!("missing nested plan must not request titles"),
        ),
        PreviewTimelineResolution::Unavailable { reason } if reason.detail().contains(&missing_id.to_string())
    ));
}

#[test]
fn empty_nested_sequence_is_transparent_instead_of_blocking_parent_output() {
    let child = Sequence::new("empty-child");
    let mut parent = Sequence::new("parent");
    let time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(child.id, tt(0, time_base), tt(24, time_base), None)
                .expect("nested clip"),
        )
        .expect("insert nested clip");
    let mut unexpected_media = |_| panic!("empty nested Sequence must not request media");

    let PreviewTimelineResolution::Ready(resolved) = resolve_preview_timeline(
        &parent,
        &[child],
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut unexpected_media,
        &mut |_| panic!("empty nested plan must not request titles"),
    ) else {
        panic!("empty nested Sequence must resolve as a transparent layer");
    };
    assert_eq!(resolved.plan.elements.len(), 1);
    let ResolvedPreviewElement::Media { frame, .. } = &resolved.plan.elements[0] else {
        panic!("nested Sequence must lower to a media layer");
    };
    let working = frame.working_frame().expect("transparent working frame");
    assert!(working.frame.rgba_f32().data.iter().all(|pixel| *pixel == [0.0; 4]));
}

#[test]
fn basic_title_enters_the_shared_working_linear_preview_path() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let mut sequence = Sequence::new("Basic Title");
    sequence.settings.resolution = Resolution { width: 640, height: 360 };
    let time_base = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_basic_title(
                "Mondrian",
                mondrian_core::default_basic_title_font_family(),
                tt(0, time_base),
                tt(24, time_base),
            )
            .expect("title"),
        )
        .expect("title placement");
    let mut rasterizer = mondrian_renderer::BasicTitleRasterizer::new();
    let mut title_frame = |request: PreviewTimelineTitleRequest| {
        PreviewTimelineTitleFrame::Ready(
            rasterizer
                .rasterize(
                    &request.title,
                    request.author_resolution,
                    request.title_safe_margin,
                    request.target_resolution,
                    request.working_color_space,
                )
                .expect("title raster"),
        )
    };
    let mut unexpected_media = |_| panic!("Basic Title must not request media decode");

    let PreviewTimelineResolution::Ready(resolved) = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        Resolution { width: 320, height: 180 },
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut unexpected_media,
        &mut title_frame,
    ) else {
        panic!("Basic Title must resolve");
    };
    let [ResolvedPreviewElement::Media { frame, .. }] = resolved.plan.elements.as_slice() else {
        panic!("Basic Title must lower to the shared source path");
    };
    let working = frame.working_frame().expect("working title");
    assert_eq!(
        working.frame.descriptor().alpha,
        mondrian_renderer::ColorFrameAlpha::StraightCoverage
    );
    assert!(working.frame.rgba_f32().data.iter().any(|pixel| pixel[3] > 0.0));
    assert_eq!(frame.decode_execution().media_layers, 0);
}
