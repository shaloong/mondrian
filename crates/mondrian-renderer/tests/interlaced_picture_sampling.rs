use mondrian_core::{
    timeline_data::FieldOrder, AssetId, Color, FramePosition, Rational, TimelineTime,
};
use mondrian_renderer::picture_sampling::{
    PictureFieldLineParity, PictureSamplePhase, ProgramPictureSamples, ProgramPictureSampling,
};
use mondrian_renderer::{
    evaluate_prepared_visual_program, PreparedVisualProgram, TimelineEvaluationRequest,
    TimelineRenderPlanElement,
};
use mondrian_timeline::{Clip, Sequence, Track};

#[test]
fn public_sampling_seam_exposes_two_exact_1080i25_evaluations() {
    let sampling = ProgramPictureSampling::new(Rational::new(1, 25), FieldOrder::UpperFirst);
    assert_eq!(sampling.field_time_base(), Some(Rational::new(1, 50)));
    let ProgramPictureSamples::Interlaced { first, second } =
        sampling.samples(7).expect("qualified sample")
    else {
        panic!("interlaced sample pair")
    };
    assert_eq!(first.phase(), PictureSamplePhase::FirstField);
    assert_eq!(second.phase(), PictureSamplePhase::SecondField);
    assert_eq!(
        first.position(),
        FramePosition::new(14, Rational::new(1, 50))
    );
    assert_eq!(
        second.position(),
        FramePosition::new(15, Rational::new(1, 50))
    );
    assert_eq!(first.field_line_parity(), Some(PictureFieldLineParity::Top));
    assert_eq!(
        second.field_line_parity(),
        Some(PictureFieldLineParity::Bottom)
    );
    assert_eq!(
        TimelineTime::from_frame_position(second.position()).expect("exact time"),
        TimelineTime::new(3, 10).expect("0.3 seconds")
    );
}

#[test]
fn public_sampling_seam_preserves_ntsc_fractional_field_grid() {
    let sampling = ProgramPictureSampling::new(Rational::new(1001, 30_000), FieldOrder::UpperFirst);
    let ProgramPictureSamples::Interlaced { first, second } =
        sampling.samples(1).expect("qualified sample")
    else {
        panic!("interlaced sample pair")
    };
    assert_eq!(
        first.position(),
        FramePosition::new(2, Rational::new(1001, 60_000))
    );
    assert_eq!(
        second.position(),
        FramePosition::new(3, Rational::new(1001, 60_000))
    );
    assert_ne!(first.position(), second.position());
}

#[test]
fn effects_plan_observes_distinct_exact_field_samples() {
    let mut sequence = Sequence::new("field-aware effects");
    sequence.settings.frame_rate = Rational::FPS_25;
    sequence.settings.field_order = FieldOrder::UpperFirst;
    sequence.video_tracks.clear();
    let mut track = Track::new_video("V1");
    track
        .add_clip(
            Clip::new_solid_color(
                AssetId::new(),
                Color::BLACK,
                TimelineTime::ZERO,
                TimelineTime::new(1, 1).expect("one second"),
            )
            .expect("solid Clip"),
        )
        .expect("add Clip");
    sequence.video_tracks.push(track);
    let program = PreparedVisualProgram::prepare(&sequence).expect("prepared Program");
    let ProgramPictureSamples::Interlaced { first, second } =
        ProgramPictureSampling::new(sequence.time_base(), sequence.settings.field_order)
            .samples(0)
            .expect("field pair")
    else {
        panic!("interlaced field pair")
    };
    let first_plan = evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::export(first.position()),
    )
    .expect("first field plan");
    let second_plan = evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::export(second.position()),
    )
    .expect("second field plan");
    let TimelineRenderPlanElement::SolidColor(first_layer) = &first_plan.elements[0] else {
        panic!("solid first field")
    };
    let TimelineRenderPlanElement::SolidColor(second_layer) = &second_plan.elements[0] else {
        panic!("solid second field")
    };
    assert_ne!(first_layer.frame_seed, second_layer.frame_seed);
    assert_ne!(first_plan.position, second_plan.position);
}
