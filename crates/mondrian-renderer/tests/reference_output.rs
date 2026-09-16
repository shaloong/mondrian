use mondrian_core::{
    AudioChannelLayout, ColorSpace, FramePosition, ProjectColorEnvironment, Rational,
};
use mondrian_reference_output::{
    ReferenceHdrSignal, ReferenceOutputPixelFormat, ReferenceOutputRange, ReferenceOutputScan,
    ReferenceOutputSignal,
};
use mondrian_renderer::color::{ProgramOutputRole, SourceColorModule};
use mondrian_renderer::{
    CpuEncodedColorFrame, CpuSourceColorFrame, ReferenceOutputProgram,
    RenderCpuColorExecutionSession, TimelineEvaluationRequest, TimelineRenderColorTarget,
    TimelineRenderIntent, TimelineRenderQuality,
};
use mondrian_timeline::sequence::SequenceSettings;

#[test]
fn reference_output_timeline_intent_is_full_raster_working_final() {
    let position = FramePosition::new(18, Rational::new(1, 60));
    let request = TimelineEvaluationRequest::reference_output(position);

    assert_eq!(request.position, position);
    assert_eq!(request.intent, TimelineRenderIntent::ReferenceOutput);
    assert_eq!(request.settings.quality, TimelineRenderQuality::Final);
    assert_eq!(
        request.settings.color_target,
        TimelineRenderColorTarget::Working
    );
    assert!(!request.settings.allow_frame_drop);
    assert_eq!(request.settings.resolution_scale, 1.0);
}

#[test]
fn canonical_program_output_lowers_to_clean_feed_video_and_exact_audio() {
    let settings = SequenceSettings::default();
    let context = settings
        .root_program_color_context(&ProjectColorEnvironment::default())
        .expect("Program color context");
    let signal = ReferenceOutputSignal {
        width: 6,
        height: 1,
        frame_rate: Rational::FPS_25,
        scan: ReferenceOutputScan::Progressive,
        pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
        color_space: ColorSpace::Rec709,
        range: ReferenceOutputRange::Legal,
        hdr: None,
        audio_layout: AudioChannelLayout::Stereo,
    };
    let program = ReferenceOutputProgram::prepare(&context, signal.clone()).expect("prepare");
    assert_eq!(
        program.boundary().target(),
        ProgramOutputRole::ReferenceOutput
    );

    let source = CpuSourceColorFrame::from(CpuEncodedColorFrame::source_rgba8(
        6,
        1,
        ColorSpace::Rec709,
        [
            0, 0, 0, 255, 32, 64, 96, 255, 64, 96, 128, 255, 96, 128, 160, 255, 128, 160, 192, 255,
            255, 255, 255, 255,
        ]
        .to_vec(),
    ));
    let mut color_session = RenderCpuColorExecutionSession::new(8);
    let working =
        SourceColorModule::execute_cpu(&source, &context.media_input(false), &mut color_session)
            .expect("working source");
    let audio = vec![0.0_f32; 1_920 * 2];

    let first = program
        .execute_cpu(working.frame(), 0, &audio, &mut color_session)
        .expect("clean feed");
    let second = program
        .execute_cpu(working.frame(), 0, &audio, &mut color_session)
        .expect("repeat clean feed");

    assert_eq!(first.video.row_bytes(), 16);
    assert_eq!(first.video.bytes().len(), 16);
    assert_eq!(first.video.sha256(), second.video.sha256());
    assert_eq!(first.audio.sample_frames(), 1_920);
    assert_eq!(first.video.signal(), &signal);
    assert_eq!(first.audio.signal(), &signal);
}

#[test]
fn reference_output_rejects_a_device_color_identity_that_differs_from_program_output() {
    let settings = SequenceSettings::default();
    let context = settings
        .root_program_color_context(&ProjectColorEnvironment::default())
        .expect("Program color context");
    let signal = ReferenceOutputSignal {
        width: 1920,
        height: 1080,
        frame_rate: Rational::FPS_25,
        scan: ReferenceOutputScan::Progressive,
        pixel_format: ReferenceOutputPixelFormat::Rgb444TwelveIn16Le,
        color_space: ColorSpace::Rec2100Pq,
        range: ReferenceOutputRange::Full,
        hdr: Some(ReferenceHdrSignal { mastering_display: None, content_light: None }),
        audio_layout: AudioChannelLayout::Stereo,
    };

    let error = match ReferenceOutputProgram::prepare(&context, signal) {
        Ok(_) => panic!("device color signal cannot relabel Rec.709 Program Output"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("color identity"));
}
