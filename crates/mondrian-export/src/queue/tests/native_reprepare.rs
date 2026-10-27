//! Actual immutable queue -> native DSP -> validated/published PCM24 MOV.
//! Owned finite controls; no third-party, realtime or other-platform claim.

use super::native_transport_output::{decode_output_pcm, write_source_with_samples};
use super::*;
use mondrian_audio::{
    AudioProcessorResolver, DiscoveredClapAudioProcessorSpecResolver,
    DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioProcessorSidechainRoute, AudioRouteSource,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Primary,
    Stems,
}

#[test]
#[ignore = "current Product, owned CLAP mode19, FFmpeg libx264/PCM24; full primary-output queue"]
fn production_queue_clap_input_width_reprepare_publishes_complete_stereo_pcm() {
    qualify(19, true, Output::Primary);
}

#[test]
#[ignore = "current Product, owned CLAP mode20, FFmpeg libx264/PCM24; full primary-output queue"]
fn production_queue_clap_reverse_input_width_reprepare_publishes_complete_mono_detector_pcm() {
    qualify(20, true, Output::Primary);
}

#[test]
#[ignore = "current Product, owned CLAP mode21, FFmpeg libx264/PCM24; four successive installs"]
fn production_queue_clap_repeated_width_reprepare_preserves_every_published_sample() {
    qualify(21, true, Output::Primary);
}

#[test]
#[ignore = "current Product, owned CLAP mode7, FFmpeg libx264/PCM24; latency3-to7 and full output"]
fn production_queue_clap_latency_reprepare_preserves_complete_pdc_pcm() {
    qualify(7, true, Output::Primary);
}

#[test]
#[ignore = "current Product, owned CLAP mode8; new restart during capture must fail without publication"]
fn production_queue_clap_new_restart_during_capture_refuses_partial_publication() {
    qualify(8, true, Output::Primary);
}

#[test]
#[ignore = "current Product, owned VST3 mode5/6, FFmpeg libx264/PCM24; actual startup/priming restart"]
fn production_queue_vst3_priming_reprepare_preserves_complete_pdc_pcm() {
    qualify(5, false, Output::Primary);
}

#[test]
#[ignore = "current Product, owned CLAP mode19, scoped process TEMP; stems retain explicit unsupported reprepare"]
fn production_queue_clap_stem_reprepare_refusal_releases_partial_pcm() {
    qualify(19, true, Output::Stems);
}

fn input_sample(frame: usize) -> [f32; 2] {
    [
        (64 + frame % 65_521) as f32 / 262_144.0,
        -((32 + frame % 65_519) as f32 / 262_144.0),
    ]
}

fn qualify(mode: u8, clap: bool, output_kind: Output) {
    assert!(ffmpeg_is_available_for_test());
    let stems = output_kind == Output::Stems;
    let temporary = PathBuf::from(
        std::env::var_os("MONDRIAN_EXPORT_TEST_TEMP_ROOT")
            .expect("dedicated process TEMP supplied by the supervised native gate"),
    );
    assert_eq!(
        std::env::temp_dir().canonicalize().unwrap(),
        temporary.canonicalize().unwrap()
    );
    let native_path = |name| PathBuf::from(std::env::var_os(name).unwrap());
    let (resolver, mut instance): (Arc<dyn AudioProcessorResolver>, _) = if clap {
        let catalog = Arc::new(
            DiscoveredClapAudioProcessorSpecResolver::discover(
                native_path("MONDRIAN_CLAP_TEST_HELPER"),
                [native_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
            )
            .unwrap(),
        );
        let mut state = vec![mode];
        state.extend_from_slice(&0.5_f32.to_le_bytes());
        if matches!(mode, 7 | 8) {
            state.push(0);
        }
        let instance = catalog
            .create_instance(
                "org.mondrian.fixture.sidechain",
                reference_plugin_export_contract(),
                Some(state),
            )
            .unwrap();
        (
            Arc::new(IsolatedAudioProcessorResolver::new(catalog)),
            instance,
        )
    } else {
        let catalog = Arc::new(
            DiscoveredVst3AudioProcessorSpecResolver::discover(
                native_path("MONDRIAN_VST3_TEST_HELPER"),
                [native_path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")],
            )
            .unwrap(),
        );
        let mut state = b"MSS1".to_vec();
        state.extend_from_slice(&1_u32.to_le_bytes());
        state.extend_from_slice(&0.5_f64.to_le_bytes());
        state.extend_from_slice(&u32::from(mode).to_le_bytes());
        let instance = catalog
            .create_instance(
                &catalog.descriptors()[0].class_id,
                reference_plugin_export_contract(),
                Some(state),
            )
            .unwrap();
        (
            Arc::new(IsolatedAudioProcessorResolver::new(catalog)),
            instance,
        )
    };
    instance.parameters.clear();
    let instance_id = instance.id;
    let queue = RenderQueue::new_with_audio_processor_resolver(resolver);
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.wav");
    write_source_with_samples(&source, 48_000, 1, input_sample);
    let output = directory.path().join(if stems {
        "output.wavstems"
    } else {
        "output.mov"
    });
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut config = dummy_config(&output.to_string_lossy());
        config.timeline.sequence.settings.frame_rate = Rational::FPS_25;
        config.timeline.sequence.settings.audio_sample_rate = 48_000;
        config.timeline.sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        config.timeline.range =
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 25 };
        let asset = AssetId::new();
        let component = AudioSourceComponentId::primary();
        let track = config.timeline.sequence.audio_tracks[0].id;
        config
            .timeline
            .sequence
            .add_media_audio_clip(
                track,
                Clip::new(asset, TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
                component,
            )
            .unwrap();
        config.timeline.sequence.audio_program.outputs[0]
            .strip
            .pre_fader
            .processors
            .push(instance);
        if clap {
            config.timeline.sequence.audio_program.sidechain_routes.push(
                AudioProcessorSidechainRoute::new(
                    AudioRouteSource::Track {
                        track_id: track,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    instance_id,
                    "clap.audio-input.11",
                ),
            );
        }
        let mut dependency = test_media_dependency(
            source.clone(),
            None,
            AssetMediaInterpretation::default(),
            None,
        );
        dependency.audio_components.insert(
            component,
            mondrian_media::AudioSourceSelection::new(
                0,
                mondrian_media::info::ChannelLayout::Stereo,
                MediaFileFingerprint::capture(&source),
            ),
        );
        config.timeline.media.insert(asset, dependency);
        if stems {
            config.preset = crate::preset::ExportPreset::audio_stems_pcm24();
        } else {
            config.preset = crate::preset::ExportPreset::h264_aac_sdr_1080p();
            config.preset.resolution = Some(crate::preset::Resolution { width: 16, height: 16 });
            let crate::preset::ExportArtifactEncoding::MediaFile(encoding) =
                &mut config.preset.artifact
            else {
                unreachable!()
            };
            encoding.container = crate::preset::Container::Mov;
            encoding.audio = crate::preset::AudioCodecConfig::Pcm { bit_depth: 24 };
        }
        let id = queue.enqueue(RenderJob::new(config)).expect("admit actual immutable job");
        assert!(
            wait_until(30_000, || queue
                .list_jobs()
                .iter()
                .any(|job| job.id == id && job.status.is_terminal())),
            "original job deadline"
        );
        let job = queue.list_jobs().into_iter().find(|job| job.id == id).unwrap();
        if stems || (clap && mode == 8) {
            let JobStatus::Failed(failure) = &job.status else {
                panic!("{job:?}")
            };
            assert!(
                failure.detail.contains(if stems {
                    "audio-stem Program execution failed"
                } else {
                    "native export reprepare capture"
                }),
                "{failure:?}"
            );
            assert!(
                !output.exists(),
                "failed configuration must not publish an artifact"
            );
        } else {
            assert!(matches!(job.status, JobStatus::Completed), "{job:?}");
            let pcm = decode_output_pcm(&output);
            assert_eq!(
                pcm.len(),
                48_000 * 8,
                "all samples of the one-second deliverable"
            );
            for (frame, bytes) in pcm.chunks_exact(8).enumerate() {
                let [left, right] = input_sample(frame);
                // Independent fixture oracle; the existing Export boundary is9600
                // public frames at48k. Native latency is compensated by the graph.
                let block = frame / 9_600;
                let mono = match mode {
                    19 => block == 0,
                    20 => block != 0,
                    21 => block % 2 == 1,
                    7 => false,
                    5 if !clap => false,
                    _ => unreachable!(),
                };
                let expected = if !clap {
                    [left, right]
                } else if mono {
                    [left + 1.5 * ((left + right) * 0.5), right]
                } else {
                    [2.5 * left, 2.5 * right]
                };
                let actual = [
                    f32::from_le_bytes(bytes[..4].try_into().unwrap()),
                    f32::from_le_bytes(bytes[4..].try_into().unwrap()),
                ];
                assert_eq!(actual, expected, "mode{mode} published frame{frame}");
            }
        }
        println!(
            "clap={clap} mode{mode} actual queue terminal: {:?}",
            job.status
        );
    }));
    let closed = queue.shutdown_and_wait(Duration::from_secs(2));
    assert!(closed.all_resources_released(), "{closed:?}");
    assert_eq!(closed.audio_source_owners_started, 1);
    assert_eq!(closed.audio_source_owners_closed, 1);
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
    let mut names: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    let mut expected = vec![std::ffi::OsString::from("source.wav")];
    if !stems && !(clap && mode == 8) {
        expected.push(std::ffi::OsString::from("output.mov"));
    }
    expected.sort();
    assert_eq!(
        names, expected,
        "no output-directory staging artifact survives"
    );
    let leftover: Vec<_> = std::fs::read_dir(&temporary)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| {
            name.to_string_lossy().starts_with("mondrian-export-audio-")
                || name.to_string_lossy().starts_with("mondrian-export-stem-")
        })
        .collect();
    assert!(
        leftover.is_empty(),
        "job left private PCM artifacts: {leftover:?}"
    );
}
