//! Real public Export consumer: a state-load notification must not replace an
//! authored parameter override or strand the initial output window.
#![cfg(windows)]

use mondrian_audio::{
    AudioProcessingMode, AudioRenderContract, DiscoveredClapAudioProcessorSpecResolver,
    IsolatedAudioProcessorResolver,
};
use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioSourceComponentId, ColorSpace, ExactAutomationCurve,
    ParameterId, ProjectColorEnvironment, Rational, TimelineTime,
};
use mondrian_export::preset::{AudioCodecConfig, Container, ExportArtifactEncoding, Resolution};
use mondrian_export::{
    ExportConfig, ExportMediaDependency, ExportOutputPolicy, ExportPreset, ExportSmartRenderPolicy,
    JobStatus, RenderJob, RenderQueue, TimelineExportRange, TimelineExportSnapshot,
};
use mondrian_media::MediaFileFingerprint;
use mondrian_timeline::{Clip, Sequence};
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn write_source(path: &Path) {
    let rate = 48_000_u32;
    let bytes = rate * 8;
    let mut file = BufWriter::new(std::fs::File::create(path).unwrap());
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + bytes).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16_u32.to_le_bytes()).unwrap();
    file.write_all(&3_u16.to_le_bytes()).unwrap(); // IEEE Float32, two channels.
    file.write_all(&2_u16.to_le_bytes()).unwrap();
    file.write_all(&rate.to_le_bytes()).unwrap();
    file.write_all(&(rate * 8).to_le_bytes()).unwrap();
    file.write_all(&8_u16.to_le_bytes()).unwrap();
    file.write_all(&32_u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&bytes.to_le_bytes()).unwrap();
    for _ in 0..rate {
        for value in [0.03125_f32, -0.0625] {
            file.write_all(&value.to_le_bytes()).unwrap();
        }
    }
    file.flush().unwrap();
}

#[test]
#[ignore = "official ZamCompX2 4.5 CLAP, matching Product, FFmpeg PCM24 and supervised native Job"]
fn production_queue_zam_clap_preserves_authored_makeup_on_state_load() {
    export_makeup_case(None, Some(12.0), 12.0);
}

#[test]
#[ignore = "official Zam4.5 CLAP, actual captured Makeup12dB state, matching Product and supervised public Export"]
fn production_queue_zam_clap_nondefault_state_preserves_native_and_host_authority() {
    let state = std::fs::read(
        std::env::var_os("MONDRIAN_ZAM_NONDEFAULT_STATE").expect("actual native state capture"),
    )
    .unwrap();
    assert_eq!(
        state.len(),
        137,
        "current actual capture; no synthetic state"
    );
    for host_override in [None, Some(6.0)] {
        export_makeup_case(
            Some(state.clone()),
            host_override,
            host_override.unwrap_or(12.0),
        );
    }
}

fn export_makeup_case(state: Option<Vec<u8>>, host_override: Option<f64>, expected_db: f64) {
    let path = |key| PathBuf::from(std::env::var_os(key).expect("native qualification input"));
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            path("MONDRIAN_CLAP_TEST_HELPER"),
            [path("MONDRIAN_CLAP_MIXED_VENDOR_TEST_PLUGIN")],
        )
        .unwrap(),
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 16_384,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let mut instance = catalog
        .create_instance("com.zamaudio.ZamCompX2", render, state.clone())
        .unwrap();
    if let Some(expected) = &state {
        assert_eq!(
            instance.opaque_state.as_ref().unwrap().as_slice(),
            expected.as_slice()
        );
    }
    assert!(!instance.opaque_state.as_ref().unwrap().is_empty());
    assert!(instance.parameters.values().all(|parameter| parameter.automation.is_none()));
    let makeup = ParameterId::new("clap.param.5").unwrap();
    if let Some(value) = host_override {
        instance
            .parameters
            .get_mut(&makeup)
            .unwrap()
            .set_automation(ExactAutomationCurve::new(makeup, value).unwrap())
            .unwrap();
    }
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.wav");
    let output = directory.path().join("makeup.mov");
    write_source(&source);
    let asset = AssetId::new();
    let component = AudioSourceComponentId::primary();
    let mut sequence = Sequence::new("real CLAP cold state-load receipt");
    sequence.settings.frame_rate = Rational::FPS_25;
    sequence.settings.audio_sample_rate = 48_000;
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.settings.color.program_output.color_space = ColorSpace::Rec709;
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(asset, TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            component,
        )
        .unwrap();
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let fingerprint = MediaFileFingerprint::capture(&source);
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
            mondrian_media::AudioSourceSelection::new(
                0,
                mondrian_media::info::ChannelLayout::Stereo,
                fingerprint,
            ),
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
            sequence,
            Vec::new(),
            HashMap::from([(asset, dependency)]),
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 25 },
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
        let probe = mondrian_export::validator::probe_export_output(&output).unwrap();
        assert_eq!(
            probe.audio.and_then(|audio| audio.codec_name).as_deref(),
            Some("pcm_s24le")
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
        assert_eq!(decoded.stdout.len(), 48_000 * 2 * 4);
        let gain = 10.0_f64.powf(expected_db / 20.0);
        // Predeclared absolute bound: two PCM24 LSBs cover Float32 plugin
        // arithmetic plus final quantization, far below a lost12dB override.
        let tolerance = 2.0 / 8_388_608.0;
        for (index, sample) in decoded.stdout.chunks_exact(4).enumerate() {
            let actual = f32::from_le_bytes(sample.try_into().unwrap());
            let expected = if index % 2 == 0 { 0.03125 } else { -0.0625 } * gain;
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
