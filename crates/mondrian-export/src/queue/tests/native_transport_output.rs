use super::*;
use mondrian_audio::{
    AudioProcessingMode, AudioProcessorResolver, AudioRenderContract,
    DiscoveredClapAudioProcessorSpecResolver, DiscoveredVst3AudioProcessorSpecResolver,
    IsolatedAudioProcessorResolver,
};
use mondrian_timeline::audio::{
    AudioChannelStrip, AudioChannelStripOutputPort, AudioProcessorInstance, AudioProgramOutput,
    AudioRoute, AudioRouteDestination, AudioRouteSource, ProgramOutputMainSource,
};

#[test]
#[ignore = "current Product, owned CLAP sidechain fixture, FFmpeg with libx264/PCM24"]
fn production_queue_clap_transport_reaches_main_and_stem_artifacts() {
    qualify(true, false);
}

#[test]
#[ignore = "current Product, corrected owned VST3 sidechain fixture, FFmpeg with libx264/PCM24"]
fn production_queue_vst3_transport_reaches_main_and_stem_artifacts() {
    qualify(false, false);
}

#[test]
#[ignore = "current Product, owned CLAP meter fixture, FFmpeg with libx264/PCM24"]
fn production_queue_clap_meter_reaches_main_and_stem_artifacts() {
    qualify(true, true);
}

#[test]
#[ignore = "current Product, owned VST3 meter fixture, FFmpeg with libx264/PCM24"]
fn production_queue_vst3_meter_reaches_main_and_stem_artifacts() {
    qualify(false, true);
}

fn qualify(clap: bool, meter: bool) {
    assert!(
        ffmpeg_is_available_for_test(),
        "real output qualification requires FFmpeg"
    );
    let native_path =
        |name| PathBuf::from(std::env::var_os(name).expect("native qualification input"));
    let directory = tempfile::tempdir().unwrap();
    for rate in [44_100_u32, 48_000] {
        let render = AudioRenderContract {
            sample_rate: rate,
            processing_mode: AudioProcessingMode::Offline,
            ..reference_plugin_export_contract()
        };
        let (resolver, instances): (Arc<dyn AudioProcessorResolver>, Vec<AudioProcessorInstance>) =
            if clap {
                let catalog = Arc::new(
                    DiscoveredClapAudioProcessorSpecResolver::discover(
                        native_path("MONDRIAN_CLAP_TEST_HELPER"),
                        [native_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
                    )
                    .unwrap(),
                );
                let mut state = vec![if meter { 16 } else { 6 }];
                state.extend_from_slice(&0.5_f32.to_le_bytes());
                let instances = (0..2)
                    .map(|_| {
                        catalog
                            .create_instance(
                                "org.mondrian.fixture.sidechain",
                                render,
                                Some(state.clone()),
                            )
                            .unwrap()
                    })
                    .collect();
                (
                    Arc::new(IsolatedAudioProcessorResolver::new(catalog)),
                    instances,
                )
            } else {
                let catalog = Arc::new(
                    DiscoveredVst3AudioProcessorSpecResolver::discover(
                        native_path("MONDRIAN_VST3_TEST_HELPER"),
                        [native_path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")],
                    )
                    .unwrap(),
                );
                // The fixture owns this component-state format. The existing SDK
                // accepts legacy raw component input and captures its canonical snapshot.
                let mut component = b"MSS1".to_vec();
                component.extend_from_slice(&1_u32.to_le_bytes());
                component.extend_from_slice(&0.5_f64.to_le_bytes());
                component.extend_from_slice(&(if meter { 9_u32 } else { 3 }).to_le_bytes());
                let instances = (0..2)
                    .map(|_| {
                        catalog
                            .create_instance(
                                &catalog.descriptors()[0].class_id,
                                render,
                                Some(component.clone()),
                            )
                            .unwrap()
                    })
                    .collect();
                (
                    Arc::new(IsolatedAudioProcessorResolver::new(catalog)),
                    instances,
                )
            };
        let instances: Vec<_> = instances
            .into_iter()
            .map(|mut instance| {
                instance.parameters.clear();
                instance
            })
            .collect();
        let source = directory.path().join(format!("source-{rate}.wav"));
        write_source(&source, rate, if meter { 3 } else { 1 });
        let queue = RenderQueue::new_with_audio_processor_resolver(resolver);
        for stems in [false, true] {
            let output = directory.path().join(format!(
                "output-{rate}.{}",
                if stems { "wavstems" } else { "mov" }
            ));
            let mut config = dummy_config(&output.to_string_lossy());
            config.timeline.sequence.settings.frame_rate = Rational::FPS_25;
            config.timeline.sequence.settings.audio_sample_rate = rate;
            config.timeline.sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
            config.timeline.sequence.audio_program.musical_time_reference = meter.then(|| {
                mondrian_core::MusicalTimeReference::new(
                    TimelineTime::new(2, i64::from(rate)).unwrap(),
                    TimelineTime::new(3, 4).unwrap(),
                )
                .unwrap()
                .with_time_signature(Some(mondrian_core::MusicalTimeSignature::new(7, 8).unwrap()))
            });
            // 2.60..2.64 seconds spans bar1 at2.625+2/rate for both rates.
            let start_frame = if meter { 65 } else { 0 };
            config.timeline.range = TimelineExportRange::WorkArea {
                start_frame,
                end_frame_exclusive: start_frame + 1,
            };
            let asset = AssetId::new();
            let component = AudioSourceComponentId::primary();
            let track = config.timeline.sequence.audio_tracks[0].id;
            config
                .timeline
                .sequence
                .add_media_audio_clip(
                    track,
                    Clip::new(
                        asset,
                        TimelineTime::ZERO,
                        if meter { TimelineTime::new(3, 1).unwrap() } else { TimelineTime::ONE },
                    ).unwrap(),
                    component,
                )
                .unwrap();
            config.timeline.sequence.audio_program.outputs[0]
                .strip
                .pre_fader
                .processors
                .push(instances[0].clone());
            if stems {
                let id = mondrian_core::ProgramOutputId::new();
                let mut strip = AudioChannelStrip::default();
                strip.pre_fader.processors.push(instances[1].clone());
                config.timeline.sequence.audio_program.outputs.push(AudioProgramOutput {
                    id,
                    name: "Second native stem".into(),
                    main_source: ProgramOutputMainSource::RoutedInputs,
                    strip,
                });
                config.timeline.sequence.audio_program.routes.push(AudioRoute::new(
                    AudioRouteSource::Track {
                        track_id: track,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    AudioRouteDestination::Output(id),
                ));
                config.preset = crate::preset::ExportPreset::audio_stems_pcm24();
            } else {
                config.preset = crate::preset::ExportPreset::h264_aac_sdr_1080p();
                config.preset.resolution =
                    Some(crate::preset::Resolution { width: 16, height: 16 });
                let crate::preset::ExportArtifactEncoding::MediaFile(encoding) =
                    &mut config.preset.artifact
                else {
                    unreachable!()
                };
                encoding.container = crate::preset::Container::Mov;
                encoding.audio = crate::preset::AudioCodecConfig::Pcm { bit_depth: 24 };
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
            let ids: Vec<_> =
                config.timeline.sequence.audio_program.outputs.iter().map(|o| o.id).collect();
            // Admission freezes the same real delivery/audio selection used by the worker.
            let id = queue.enqueue(RenderJob::new(config)).expect("admit native export");
            assert!(
                wait_until(30_000, || queue
                    .list_jobs()
                    .iter()
                    .any(|j| j.id == id && j.status.is_terminal())),
                "native export did not reach its original terminal boundary"
            );
            let job = queue.list_jobs().into_iter().find(|j| j.id == id).unwrap();
            assert!(matches!(job.status, JobStatus::Completed), "{job:?}");
            if stems {
                let manifest: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(output.join(crate::audio_stems::AUDIO_STEM_MANIFEST_FILE_NAME))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(manifest["stems"].as_array().unwrap().len(), 2);
                for (index, id) in ids.into_iter().enumerate() {
                    assert_output(
                        &output.join(crate::audio_stems::stem_file_name(index, id)),
                        rate,
                        meter,
                    );
                }
            } else {
                assert_output(&output, rate, meter);
            }
            println!(
                "{}",
                serde_json::json!({
                    "case": "native-transport-production-queue",
                    "format": if clap { "clap" } else { "vst3" },
                    "authored_meter": meter,
                    "rate": rate, "stems": stems,
                    "artifacts": if stems { 2 } else { 1 },
                    "pcm_frames_per_artifact": rate / 25,
                    "video_encoder": job.diagnostics.visual.video_encoder,
                })
            );
        }
        let closed = queue.shutdown_and_wait(Duration::from_secs(2));
        assert!(closed.all_resources_released(), "{closed:?}");
        assert_eq!(closed.audio_source_owners_started, 2);
        assert_eq!(closed.audio_source_owners_closed, 2);
    }
}

fn write_source(path: &Path, rate: u32, seconds: u32) {
    let data_bytes = rate * seconds * 2 * 4;
    let mut wave = BufWriter::new(std::fs::File::create(path).unwrap());
    wave.write_all(b"RIFF").unwrap();
    wave.write_all(&(36 + data_bytes).to_le_bytes()).unwrap();
    wave.write_all(b"WAVEfmt ").unwrap();
    wave.write_all(&16_u32.to_le_bytes()).unwrap();
    wave.write_all(&3_u16.to_le_bytes()).unwrap();
    wave.write_all(&2_u16.to_le_bytes()).unwrap();
    wave.write_all(&rate.to_le_bytes()).unwrap();
    wave.write_all(&(rate * 8).to_le_bytes()).unwrap();
    wave.write_all(&8_u16.to_le_bytes()).unwrap();
    wave.write_all(&32_u16.to_le_bytes()).unwrap();
    wave.write_all(b"data").unwrap();
    wave.write_all(&data_bytes.to_le_bytes()).unwrap();
    for _ in 0..rate * seconds {
        wave.write_all(&0.25_f32.to_le_bytes()).unwrap();
        wave.write_all(&(-0.25_f32).to_le_bytes()).unwrap();
    }
    wave.flush().unwrap();
}

fn assert_output(path: &Path, rate: u32, meter: bool) {
    let probe = crate::validator::probe_export_output(path).unwrap();
    assert_eq!(
        probe.audio.and_then(|a| a.codec_name).as_deref(),
        Some("pcm_s24le")
    );
    let mut command = mondrian_media::ffmpeg_command().unwrap();
    let decoded = command
        .args(["-nostdin", "-v", "error", "-xerror", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-c:a", "pcm_f32le", "-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let frames = rate as usize / 25;
    assert_eq!(decoded.stdout.len(), frames * 2 * 4);
    for (frame, pair) in decoded.stdout.chunks_exact(8).enumerate() {
        // Independent dyadic PCM oracle: Main weight2, restored Gain.5,
        // explicit Playing+.125, canonical sample tag, and native PDC3 alignment.
        let sample = if meter { rate as usize * 65 / 25 + frame } else { frame };
        let meter_gain = if meter {
            80.0 / 1024.0
                + if 8 * (sample as i64 - 2) < 21 * i64::from(rate) { 1.0 / 16.0 } else { 1.0 / 8.0 }
        } else { 0.0 };
        let expected = 0.5 * (0.625 + (sample % 251) as f32 / 1024.0 + meter_gain);
        let left = f32::from_le_bytes(pair[..4].try_into().unwrap());
        let right = f32::from_le_bytes(pair[4..].try_into().unwrap());
        assert_eq!(
            [left, right],
            [expected, -expected],
            "{} frame{frame}",
            path.display()
        );
    }
}
