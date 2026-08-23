use mondrian_core::{
    AuthoringTimeDomain, DomainTime, FramePosition, FrameRounding, Rational, SequenceId,
    SmpteCountingMode, SourceSampleTarget, TimeScale, TimeTransform, TimelineDisplaySettings,
    TimelineTime,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ContractFixture {
    schema_version: u32,
    vfr: VfrFixture,
    mixed_rate_nested: MixedRateNestedFixture,
    negative: NegativeFixture,
    long_project: LongProjectFixture,
}

#[derive(Debug, Deserialize)]
struct VfrFixture {
    sequence_rate: RateFixture,
    samples: Vec<VfrSampleFixture>,
}

#[derive(Debug, Deserialize)]
struct VfrSampleFixture {
    time: TimeFixture,
    floor_frame: i64,
    nearest_frame: i64,
    drop_frame_label: String,
}

#[derive(Debug, Deserialize)]
struct MixedRateNestedFixture {
    child_rate: RateFixture,
    child_frame: i64,
    parent_rate: RateFixture,
    parent_anchor: TimeFixture,
    expected_parent_time: TimeFixture,
    expected_parent_frame: i64,
    expected_parent_label: String,
}

#[derive(Debug, Deserialize)]
struct NegativeFixture {
    rate: RateFixture,
    time: TimeFixture,
    start_frame: i64,
    expected_frame: i64,
    expected_label: String,
}

#[derive(Debug, Deserialize)]
struct LongProjectFixture {
    rate: RateFixture,
    time: TimeFixture,
    expected_frame: i64,
    expected_wrapped_timecode: String,
    expected_frame_label: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct RateFixture {
    num: i64,
    den: i64,
}

impl RateFixture {
    fn rational(self) -> Rational {
        Rational::new(self.num, self.den)
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct TimeFixture {
    numerator: i64,
    denominator: i64,
}

impl TimeFixture {
    fn timeline_time(self) -> TimelineTime {
        TimelineTime::new(self.numerator, self.denominator).expect("valid fixture time")
    }
}

fn fixture() -> ContractFixture {
    serde_json::from_str(include_str!("reference/timeline_time_contract_v1.json"))
        .expect("timeline time fixture must remain valid JSON")
}

#[test]
fn irregular_vfr_pts_resolve_only_on_the_declared_sequence_grid() {
    let fixture = fixture();
    assert_eq!(fixture.schema_version, 1);
    let frame_rate = fixture.vfr.sequence_rate.rational();
    let display = TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 0)
        .resolve(frame_rate)
        .expect("valid drop-frame display");

    for sample in fixture.vfr.samples {
        let time = sample.time.timeline_time();
        assert_eq!(
            time.to_frame_position(frame_rate, FrameRounding::Floor)
                .expect("floor projection")
                .frame,
            sample.floor_frame
        );
        assert_eq!(
            time.to_frame_position(frame_rate, FrameRounding::Nearest)
                .expect("nearest projection")
                .frame,
            sample.nearest_frame
        );
        assert_eq!(
            display
                .format_timeline_time(time, FrameRounding::Nearest)
                .expect("VFR position label"),
            sample.drop_frame_label
        );
    }
}

#[test]
fn mixed_rate_nested_time_uses_an_explicit_domain_transform() {
    let fixture = fixture().mixed_rate_nested;
    let child_rate = fixture.child_rate.rational();
    let parent_rate = fixture.parent_rate.rational();
    let child_time = TimelineTime::from_frame_position(FramePosition::new(
        fixture.child_frame,
        Rational::new(child_rate.den, child_rate.num),
    ))
    .expect("child frame time");
    let child_domain = AuthoringTimeDomain::Sequence(SequenceId::new());
    let parent_domain = AuthoringTimeDomain::Sequence(SequenceId::new());
    let transform = TimeTransform {
        source_domain: child_domain,
        target_domain: parent_domain,
        source_anchor: TimelineTime::ZERO,
        target_anchor: fixture.parent_anchor.timeline_time(),
        scale: TimeScale::ONE,
    };
    let mapped = transform
        .map(DomainTime::new(child_domain, child_time))
        .expect("nested time mapping");

    assert_eq!(mapped.time, fixture.expected_parent_time.timeline_time());
    assert_eq!(
        mapped
            .time
            .to_frame_position(parent_rate, FrameRounding::Nearest)
            .expect("parent evaluation frame")
            .frame,
        fixture.expected_parent_frame
    );
    let display = TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 0)
        .resolve(parent_rate)
        .expect("parent display");
    assert_eq!(
        display
            .format_timeline_time(mapped.time, FrameRounding::Nearest)
            .expect("parent label"),
        fixture.expected_parent_label
    );
}

#[test]
fn negative_author_time_is_not_clamped_before_display_origin_application() {
    let fixture = fixture().negative;
    let frame_rate = fixture.rate.rational();
    let time = fixture.time.timeline_time();
    assert_eq!(
        time.to_frame_position(frame_rate, FrameRounding::Nearest)
            .expect("negative frame")
            .frame,
        fixture.expected_frame
    );
    let display =
        TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, fixture.start_frame)
            .resolve(frame_rate)
            .expect("negative display contract");
    assert_eq!(
        display
            .format_timeline_time(time, FrameRounding::Nearest)
            .expect("negative label"),
        fixture.expected_label
    );
}

#[test]
fn long_project_keeps_exact_time_while_smpte_wraps_by_declared_contract() {
    let fixture = fixture().long_project;
    let frame_rate = fixture.rate.rational();
    let time = fixture.time.timeline_time();
    let frame = time
        .to_frame_position(frame_rate, FrameRounding::Nearest)
        .expect("long-project frame");
    assert_eq!(frame.frame, fixture.expected_frame);
    assert_eq!(
        TimelineTime::from_frame_position(frame).expect("long-project roundtrip"),
        time
    );

    let timecode = TimelineDisplaySettings::timecode(SmpteCountingMode::NonDropFrame, 0)
        .resolve(frame_rate)
        .expect("long-project timecode");
    assert_eq!(
        timecode
            .format_timeline_time(time, FrameRounding::Nearest)
            .expect("wrapped SMPTE label"),
        fixture.expected_wrapped_timecode
    );

    let frames = TimelineDisplaySettings::frames(123_456)
        .resolve(frame_rate)
        .expect("long-project frame display");
    assert_eq!(
        frames
            .format_timeline_time(time, FrameRounding::Nearest)
            .expect("unbounded frame label"),
        fixture.expected_frame_label
    );
}

#[test]
fn source_sample_target_lowers_half_open_boundaries_without_epsilon() {
    let rate = Rational::new(25, 1);
    let exact_boundary = TimelineTime::new(10, 25).expect("exact boundary");
    let interior = TimelineTime::new(21, 50).expect("interior time");

    assert_eq!(
        SourceSampleTarget::covering(exact_boundary)
            .to_frame_position(rate)
            .expect("covering boundary")
            .frame,
        10
    );
    assert_eq!(
        SourceSampleTarget::strict_predecessor(exact_boundary)
            .to_frame_position(rate)
            .expect("strict predecessor boundary")
            .frame,
        9
    );
    assert_eq!(
        SourceSampleTarget::covering(interior)
            .to_frame_position(rate)
            .expect("covering interior")
            .frame,
        10
    );
    assert_eq!(
        SourceSampleTarget::strict_predecessor(interior)
            .to_frame_position(rate)
            .expect("strict predecessor interior")
            .frame,
        10
    );
    assert_eq!(
        SourceSampleTarget::for_scale(exact_boundary, TimeScale::NEGATIVE_ONE),
        SourceSampleTarget::strict_predecessor(exact_boundary)
    );
}
