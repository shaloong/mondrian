//! Shared public-queue fixture for the two real native gain/state cases.
use mondrian_audio::{
    AudioRenderContract, DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioSamplePosition, AudioSampleRate, AudioSampleRounding,
    AudioSourceComponentId, ColorSpace, FramePosition, ProjectColorEnvironment, Rational,
    TimelineTime,
};
use mondrian_export::preset::{AudioCodecConfig, Container, ExportArtifactEncoding, Resolution};
use mondrian_export::{
    ExportConfig, ExportMediaDependency, ExportOutputPolicy, ExportPreset, ExportSmartRenderPolicy,
    JobStatus, RenderJob, RenderQueue, TimelineExportRange, TimelineExportSnapshot,
};
use mondrian_media::MediaFileFingerprint;
use mondrian_timeline::audio::{AudioChannelStrip, AudioProcessorInstance};
use mondrian_timeline::{Clip, Sequence};
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn write_source(
    path: &Path,
    render: AudioRenderContract,
    frames: usize,
    sample: fn(usize, usize) -> f32,
) {
    let channels = u16::try_from(render.channel_count()).unwrap();
    let align = channels.checked_mul(4).unwrap();
    let bytes = u32::try_from(frames.checked_mul(usize::from(align)).unwrap()).unwrap();
    let mut file = BufWriter::new(std::fs::File::create(path).unwrap());
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + bytes).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16_u32.to_le_bytes()).unwrap();
    file.write_all(&3_u16.to_le_bytes()).unwrap(); // IEEE Float32.
    file.write_all(&channels.to_le_bytes()).unwrap();
    file.write_all(&render.sample_rate.to_le_bytes()).unwrap();
    file.write_all(&render.sample_rate.checked_mul(u32::from(align)).unwrap().to_le_bytes())
        .unwrap();
    file.write_all(&align.to_le_bytes()).unwrap();
    file.write_all(&32_u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&bytes.to_le_bytes()).unwrap();
    for frame in 0..frames {
        for channel in 0..usize::from(channels) {
            file.write_all(&sample(frame, channel).to_le_bytes()).unwrap();
        }
    }
    file.flush().unwrap();
}

pub fn export_gain_case(
    catalog: Arc<DiscoveredClapAudioProcessorSpecResolver>,
    instance: AudioProcessorInstance,
    render: AudioRenderContract,
    frame_rate: Rational,
    end_frame: i64,
    sample: fn(usize, usize) -> f32,
    expected_db: f64,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.wav");
    let output = directory.path().join("gain.mov");
    let asset = AssetId::new();
    let component = AudioSourceComponentId::primary();
    let mut sequence = Sequence::new("real native gain through immutable Export");
    sequence.settings.frame_rate = frame_rate;
    sequence.settings.audio_sample_rate = render.sample_rate;
    sequence.settings.audio_channel_layout = render.channel_layout;
    sequence.settings.color.program_output.color_space = ColorSpace::Rec709;
    let duration =
        TimelineTime::from_frame_position(FramePosition::new(end_frame, sequence.time_base()))
            .unwrap();
    let frames = usize::try_from(
        AudioSamplePosition::from_timeline_time(
            duration,
            AudioSampleRate::new(render.sample_rate).unwrap(),
            AudioSampleRounding::Ceil,
        )
        .unwrap()
        .sample(),
    )
    .unwrap();
    assert!(frames > 0);
    write_source(&source, render, frames, sample);
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(asset, TimelineTime::ZERO, duration).unwrap(),
            component,
        )
        .unwrap();
    // These are fixture inputs, not assertions about future mixer defaults.
    let unity = AudioChannelStrip {
        input_trim_db: 0.0,
        pre_fader: Default::default(),
        fader_db: 0.0,
        fader_automation: None,
        post_fader: Default::default(),
    };
    for channel in sequence.audio_program.track_channels.values_mut() {
        channel.strip = unity.clone();
    }
    for output in sequence.audio_program.outputs.iter_mut() {
        output.strip = unity.clone();
    }
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let author_before = serde_json::to_vec(&sequence).unwrap();
    let fingerprint = MediaFileFingerprint::capture(&source);
    let source_layout = match render.channel_layout {
        AudioChannelLayout::Mono => mondrian_media::info::ChannelLayout::Mono,
        AudioChannelLayout::Stereo => mondrian_media::info::ChannelLayout::Stereo,
        _ => panic!("gain fixture requires Mono or Stereo input"),
    };
    let dependency = ExportMediaDependency {
        path: source,
        source_fingerprint: fingerprint.clone(),
        source_container: "wav".into(),
        source_video_stream: None,
        video_stream_index: None,
        picture_source_extent: None,
        source_resolution: None,
        picture: None,
        audio_components: HashMap::from([(
            component,
            mondrian_media::AudioSourceSelection::new(0, source_layout, fingerprint),
        )]),
        interpretation: AssetMediaInterpretation::default(),
        color_diagnostic: None,
    };
    let mut preset = ExportPreset::h264_aac_sdr_1080p();
    preset.resolution = Some(Resolution { width: 16, height: 16 });
    let ExportArtifactEncoding::MediaFile(encoding) = &mut preset.artifact else {
        unreachable!()
    };
    encoding.container = Container::Mov;
    encoding.audio = AudioCodecConfig::Pcm { bit_depth: 24 };
    let config = ExportConfig {
        preset,
        timeline: Box::new(TimelineExportSnapshot::unprepared(
            ProjectColorEnvironment::default(),
            sequence.clone(),
            Vec::new(),
            HashMap::from([(asset, dependency)]),
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: end_frame },
        )),
        output_path: output.clone(),
        output_policy: ExportOutputPolicy::CreateNew,
        smart_render: ExportSmartRenderPolicy::Disabled,
        broadcast_qc: None,
        regulatory_pse: None,
        frozen_ancillary: None,
        approved_bmx: None,
    };
    let queue = RenderQueue::new_with_audio_processor_resolver(Arc::new(
        IsolatedAudioProcessorResolver::new(catalog),
    ));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let id = queue.enqueue(RenderJob::new(config)).expect("real immutable queue admission");
        let deadline = Instant::now() + Duration::from_secs(30);
        let job = loop {
            let job = queue.list_jobs().into_iter().find(|job| job.id == id).unwrap();
            if job.status.is_terminal() {
                break job;
            }
            assert!(Instant::now() < deadline, "original job deadline: {job:?}");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(matches!(job.status, JobStatus::Completed), "{job:?}");
        assert_eq!(
            serde_json::to_vec(&sequence).unwrap(),
            author_before,
            "author input stays immutable"
        );
        let probe = mondrian_export::validator::probe_export_output(&output).unwrap();
        let audio = probe.audio.expect("actual encoded audio stream");
        assert_eq!(audio.codec_name.as_deref(), Some("pcm_s24le"));
        assert_eq!(audio.sample_rate, Some(render.sample_rate));
        assert_eq!(
            audio.channels,
            Some(u32::try_from(render.channel_count()).unwrap())
        );
        let decoded = mondrian_media::ffmpeg_command()
            .unwrap()
            .args(["-nostdin", "-v", "error", "-xerror", "-i"])
            .arg(&output)
            .args(["-map", "0:a:0", "-c:a", "pcm_f32le", "-f", "f32le", "-"])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert!(decoded.stderr.is_empty());
        let channels = render.channel_count();
        assert_eq!(decoded.stdout.len(), frames * channels * 4);
        let gain = 10.0_f64.powf(expected_db / 20.0);
        // Original bound: two PCM24 LSBs cover Float32 arithmetic + quantization.
        let tolerance = 2.0 / 8_388_608.0;
        for (index, bytes) in decoded.stdout.chunks_exact(4).enumerate() {
            let actual = f32::from_le_bytes(bytes.try_into().unwrap());
            let expected = f64::from(sample(index / channels, index % channels)) * gain;
            assert!(
                actual.is_finite() && (f64::from(actual) - expected).abs() <= tolerance,
                "full PCM sample{index}: actual={actual}, expected={expected}"
            );
        }
    }));
    let closed = queue.shutdown_and_wait(Duration::from_secs(2));
    assert!(closed.all_resources_released(), "{closed:?}");
    assert!(
        closed.audio_native_children.children_registered >= 1,
        "{closed:?}"
    );
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}
