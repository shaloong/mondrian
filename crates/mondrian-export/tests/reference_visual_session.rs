#![cfg(feature = "validation")]

use std::collections::HashMap;

use mondrian_core::{Color, ProjectColorEnvironment, Rational, Resolution, TimelineTime};
use mondrian_export::preset::{TimelineExportRange, TimelineExportSnapshot};
use mondrian_export::queue::{export_visual_frame_validation, FrozenTimelineReferenceFrameSession};
use mondrian_timeline::{clip::Clip, sequence::Sequence};

fn frozen_solid_timeline() -> TimelineExportSnapshot {
    let mut sequence = Sequence::new("persistent Reference visual");
    sequence.settings.resolution = Resolution { width: 2, height: 1 };
    sequence.settings.frame_rate = Rational::new(30, 1);
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_solid_color(
                mondrian_core::AssetId::new(),
                Color { r: 0.25, g: 0.5, b: 0.75, a: 1.0 },
                TimelineTime::ZERO,
                TimelineTime::new(2, 30).expect("two frames"),
            )
            .expect("solid Clip"),
        )
        .expect("insert solid Clip");
    let range = TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 2 };
    let prepared =
        mondrian_export::prepare_timeline_export_dependencies(&sequence, &[], range, false)
            .expect("prepare frozen visual closure");
    TimelineExportSnapshot::captured(
        ProjectColorEnvironment::default(),
        sequence,
        Vec::new(),
        HashMap::new(),
        range,
        prepared.execution_snapshot().clone(),
    )
}

#[test]
fn persistent_reference_visual_matches_exact_export_pixels_and_advances_contiguously() {
    let timeline = frozen_solid_timeline();
    let expected =
        export_visual_frame_validation(&timeline, 0, timeline.sequence.settings.resolution)
            .expect("one-shot Export validation");
    let mut session = FrozenTimelineReferenceFrameSession::new(timeline, 0)
        .expect("persistent Reference visual session");

    let first = session.render_next().expect("first Reference frame");
    assert_eq!(first.working_frame, expected.working_frame);
    assert_eq!(session.next_frame_index(), 1);
    session.render_next().expect("second Reference frame");
    assert_eq!(session.next_frame_index(), 2);
}

#[test]
fn persistent_reference_visual_latches_cancellation_failure() {
    let mut session = FrozenTimelineReferenceFrameSession::new(frozen_solid_timeline(), 0)
        .expect("persistent Reference visual session");
    session.cancel();

    let first = session.render_next().expect_err("cancelled render must fail");
    let second = session.render_next().expect_err("faulted session must stay failed");
    assert!(second.contains(&first));
    assert_eq!(session.next_frame_index(), 0);
}
