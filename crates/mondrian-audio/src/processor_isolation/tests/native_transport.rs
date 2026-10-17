use super::*;

#[test]
#[ignore = "current product and rebuilt owned CLAP fixture with transport state 6"]
fn installed_clap_canonical_transport_signal_time() {
    transport_reference(true);
}

#[test]
#[ignore = "current product and rebuilt owned VST3 fixture with transport state 3"]
fn installed_vst3_canonical_transport_signal_time() {
    transport_reference(false);
}

fn transport_reference(clap: bool) {
    let helper = std::env::var_os(if clap {
        "MONDRIAN_CLAP_TEST_HELPER"
    } else {
        "MONDRIAN_VST3_TEST_HELPER"
    })
    .map(PathBuf::from)
    .expect("current isolated worker helper");
    let plugin = std::env::var_os(if clap {
        "MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN"
    } else {
        "MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN"
    })
    .map(PathBuf::from)
    .expect("rebuilt native transport fixture");
    let clap_catalog = if clap {
        Some(
            DiscoveredClapAudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
                .expect("CLAP discovery"),
        )
    } else {
        None
    };
    let vst3_catalog = if clap {
        None
    } else {
        Some(
            DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [plugin])
                .expect("VST3 discovery"),
        )
    };
    for rate in [44_100, 48_000] {
        for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
            let render = AudioRenderContract {
                sample_rate: rate,
                processing_mode: mode,
                ..test_render_contract()
            };
            let mut instance = if let Some(catalog) = &clap_catalog {
                let mut state = vec![6];
                state.extend_from_slice(&0.5_f32.to_le_bytes());
                catalog
                    .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
                    .expect("restore time-dependent CLAP state")
            } else {
                let catalog = vst3_catalog.as_ref().expect("VST3 catalog");
                catalog
                    .create_instance(
                        &catalog.descriptors()[0].class_id,
                        render,
                        Some(vst3_sidechain_snapshot(0.5, 3)),
                    )
                    .expect("restore time-dependent VST3 state")
            };
            instance.parameters.clear();
            let request = AudioProcessorPrepareRequest::new(
                AudioProcessorOccurrence {
                    instance_id: instance.id,
                    owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                    insertion: AudioProcessorInsertionPoint::PreFader,
                },
                &instance.definition,
                &instance.parameters,
                instance.opaque_state.as_ref().map(AsRef::as_ref),
                render,
            );
            let spec = if let Some(catalog) = &clap_catalog {
                catalog.resolve(request)
            } else {
                vst3_catalog.as_ref().expect("VST3 catalog").resolve(request)
            }
            .expect("resolve native time-dependent contract");
            assert_eq!(
                spec.plugin_execution_contract.algorithmic_latency_frames(),
                3
            );
            let keys: Vec<_> =
                spec.auxiliary_inputs.buses.iter().map(|bus| bus.bus_key.clone()).collect();
            assert_eq!(
                keys,
                if clap {
                    vec!["clap.audio-input.11", "clap.audio-input.7"]
                } else {
                    vec!["vst3.audio-input.1", "vst3.audio-input.2"]
                }
            );
            for partitions in [&[4, 4, 4, 2][..], &[3, 2, 1, 4, 4][..]] {
                let factory = IsolatedAudioProcessorFactory::prepare(spec.clone())
                    .expect("native isolated factory");
                let mut processor = factory.create().expect("native isolated occurrence");
                // Sequence input-signal coordinates are already latency-adjusted
                // by processor_host before this typed adapter boundary. Keep the
                // originating request and PDC explicit in this independent oracle.
                for (sequence_origin, input_delay) in
                    [(73_i64, 0_i64), (0, 17), (991, 5), (0, 0), (19, 0)]
                {
                    let signal_origin = sequence_origin - input_delay;
                    processor.enter_state(signal_origin).expect("seek resets native history");
                    let mut actual = Vec::new();
                    let mut offset = 0;
                    for &frames in partitions {
                        let mut io = NativeSidechainAudioIo {
                            main: transport_samples(0, offset, frames),
                            inputs: keys
                                .iter()
                                .enumerate()
                                .map(|(bus, key)| {
                                    (key.clone(), transport_samples(bus + 1, offset, frames))
                                })
                                .collect(),
                        };
                        let start = signal_origin + offset as i64;
                        let context = AudioProcessorProcessContext::new(
                            AudioRenderRequest { start_sample: start, frames },
                            AudioSampleRate::new(rate).expect("grid"),
                            AudioChannelLayout::Stereo,
                            mode,
                            AudioKernelBackend::ScalarReference,
                        );
                        processor
                            .process(
                                context,
                                &mut io,
                                AudioParameterEventBatch::new(start, frames, &[], &[], &[]),
                            )
                            .expect("native timeline transport must exist with truthful validity");
                        actual.extend(io.main);
                        offset += frames;
                    }
                    for (frame, pair) in actual.chunks_exact(2).enumerate() {
                        let expected = if frame < 3 {
                            0.0
                        } else {
                            let source = frame - 3;
                            let affine = 2 * transport_numerator(0, source)
                                + 3 * transport_numerator(1, source)
                                + 5 * transport_numerator(2, source);
                            let signed_tag = ((signal_origin + source as i64) % 251) as i32;
                            // Native time modulates gain, preserving silence
                            // and the declared finite delay/tail contract.
                            (affine * (512 + signed_tag)) as f32 / 1_048_576.0
                        };
                        assert_eq!(pair,[expected,-expected*0.5],
                            "{} rate{rate}, {mode:?}, sequence{sequence_origin}, PDC{input_delay}, signal{signal_origin}, partitions{partitions:?}, frame{frame}",if clap {"CLAP"}else{"VST3"});
                    }
                }
            }
        }
    }
}

fn transport_numerator(bus: usize, sample: usize) -> i32 {
    ((sample + [0, 3, 7][bus]) % 31) as i32 - 15
}

fn transport_samples(bus: usize, start: usize, frames: usize) -> Vec<f32> {
    (start..start + frames)
        .flat_map(|sample| {
            let left = transport_numerator(bus, sample) as f32 / 1024.0;
            [left, -left * 0.5]
        })
        .collect()
}

#[cfg(windows)]
#[test]
#[ignore = "fresh normal Product and owned CLAP sidechain mode11; actual canonical Sequence tempo/Scope/retime/PDC"]
fn installed_clap_canonical_tempo_scope_retime_keeps_sequence_clock() {
    use crate::{
        AudioContinuityEpoch, AudioContinuityEvidenceVisitor, AudioDecodedSource,
        AudioExecutionError, AudioMediaResolver, AudioProcessorCursorEvidence,
        AudioProcessorCursorState, AudioProgramRuntime, AudioRuntimeResourceGrant,
        AudioSessionCursorEvidence, AudioStateEntry, ResolvedAudioSource,
    };
    use mondrian_core::{
        AssetId, AudioOwnerInstallationSource, AudioSourceComponentId, ExactAutomationCurve,
        ExactAutomationKeyframe, ExecutionCancellationToken, MusicalTimeReference, TimeScale,
        TimelineTime,
    };
    use mondrian_timeline::{Clip, Sequence};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, WaitForSingleObject,
    };

    struct IndexedMedia;
    impl AudioDecodedSource for IndexedMedia {
        fn read_interleaved(
            &self,
            start: i64,
            frames: usize,
            destination: &mut [f32],
            _: &ExecutionCancellationToken,
        ) -> Result<(), String> {
            if destination.len() != frames * 2 {
                return Err("stereo extent".into());
            }
            for (offset, pair) in destination.chunks_exact_mut(2).enumerate() {
                let frame = start.checked_add(offset as i64).ok_or("source position")?;
                let value = if frame < 0 {
                    0.0
                } else {
                    ((frame % 31) - 15) as f32 / 1024.0
                };
                pair.copy_from_slice(&[value, -0.5 * value]);
            }
            Ok(())
        }
    }
    struct Media(AtomicUsize);
    impl AudioMediaResolver for Media {
        fn resolve(
            &self,
            _: AssetId,
            _: AudioSourceComponentId,
            _: u32,
        ) -> Result<ResolvedAudioSource, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ResolvedAudioSource::new(
                AudioChannelLayout::Stereo,
                Arc::new(IndexedMedia),
            ))
        }
    }
    #[derive(Default)]
    struct Cursors {
        sessions: Vec<AudioSessionCursorEvidence>,
        processors: Vec<AudioProcessorCursorEvidence>,
    }
    impl AudioContinuityEvidenceVisitor for Cursors {
        fn session(
            &mut self,
            value: AudioSessionCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            self.sessions.push(value);
            Ok(())
        }
        fn processor(
            &mut self,
            value: AudioProcessorCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            self.processors.push(value);
            Ok(())
        }
    }
    fn owners(runtime: &AudioProgramRuntime) -> Vec<crate::AudioNativeProcessorOwnerLease> {
        let mut cursors = Cursors::default();
        runtime.visit_continuity_evidence(&mut cursors).unwrap();
        assert_eq!((cursors.sessions.len(), cursors.processors.len()), (1, 2));
        cursors
            .processors
            .iter()
            .map(|processor| {
                let epoch = match processor.state {
                    AudioProcessorCursorState::Unentered => None,
                    AudioProcessorCursorState::WaitingForFirstSignal { epoch }
                    | AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
                    state => panic!("unexpected native state {state:?}"),
                };
                runtime
                    .native_owner_lease(crate::AudioNativeProcessorAddress {
                        root: cursors.sessions[0].identity,
                        local: processor.identity,
                        occurrence: processor.occurrence,
                        epoch,
                    })
                    .unwrap()
            })
            .collect()
    }
    let native_path = |name| PathBuf::from(std::env::var_os(name).expect("native transport input"));
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            native_path("MONDRIAN_CLAP_TEST_HELPER"),
            [native_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
        )
        .unwrap(),
    );
    let processors = IsolatedAudioProcessorResolver::new(catalog.clone());
    for rate in [44_100, 48_000] {
        let time = |sample| TimelineTime::new(sample, i64::from(rate)).unwrap();
        for with_music in [false, true] {
            let render = AudioRenderContract {
                sample_rate: rate,
                processing_mode: AudioProcessingMode::Offline,
                // Two isolated native owners exceed the small unit-test default.
                // This transport oracle does not qualify a scratch-budget edge.
                processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
                ..test_render_contract()
            };
            let mut sequence = Sequence::new("actual tempo-only Scope with retimed source");
            sequence.audio_program.musical_time_reference = with_music.then(|| {
                MusicalTimeReference::new(
                    TimelineTime::new(-19, 7).unwrap(),
                    TimelineTime::new(3, 4).unwrap(),
                )
                .unwrap()
            });
            let mut clip = Clip::new(AssetId::new(), time(0), time(64)).unwrap();
            clip.set_constant_source_time_map(time(5), TimeScale::new(3, 2).unwrap())
                .unwrap();
            sequence
                .add_media_audio_clip(
                    sequence.audio_tracks[0].id,
                    clip,
                    AudioSourceComponentId::primary(),
                )
                .unwrap();
            let scope = &mut sequence.audio_program.processing_scopes[0];
            sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_in =
                TimelineTime::new(5, 1).unwrap();
            let mut gain = ExactAutomationCurve::new(
                ParameterId::new_static(mondrian_timeline::audio::INPUT_GAIN_DB_PARAMETER_ID),
                -6.0,
            )
            .unwrap();
            gain.set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(5, 1).unwrap(),
                -6.0,
            ))
            .unwrap();
            gain.set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(5, 1).unwrap().checked_add(time(64)).unwrap(),
                0.0,
            ))
            .unwrap();
            scope.input_gain_automation = Some(gain);
            // Original mode0 contributes one genuine native 3-frame delay.
            // Mode11 must see its PDC-adjusted Sequence signal, not Scope-local
            // automation seconds or the retimed 3/2-speed source coordinate.
            for mode in [0, 11] {
                let mut state = vec![mode];
                state.extend_from_slice(&0.5_f32.to_le_bytes());
                let instance = catalog
                    .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
                    .unwrap();
                assert!(instance.parameters.is_empty());
                scope.processors.processors.push(instance);
            }
            let media = Media(AtomicUsize::new(0));
            let mut runtime =
                AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
                    &sequence,
                    &[],
                    &media,
                    &processors,
                    render,
                    None,
                    AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
                )
                .unwrap();
            assert_eq!(runtime.public_output_lookahead_frames(), 6);
            assert_eq!(media.0.load(Ordering::SeqCst), 1);
            let source = AudioOwnerInstallationSource::new();
            let (registration, verifier) = source.register().unwrap();
            runtime.bind_native_owner_installation(verifier).unwrap();
            let claim = source
                .reserve_install(&registration, None)
                .and_then(|r| source.begin_commit(r))
                .unwrap()
                .commit()
                .unwrap();
            runtime
                .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
                .unwrap();
            let cold: Vec<_> = owners(&runtime)
                .into_iter()
                .map(|lease| runtime.observe_native_owner_object(lease).unwrap())
                .collect();
            let handles: Vec<_> = cold
                .iter()
                .map(|object| {
                    let pid = object.process_id().get();
                    assert_ne!(pid, std::process::id());
                    // SAFETY: this observed actual Runtime child supplies PID; the
                    // extra read-only HANDLE is held only to observe this exact Drop.
                    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
                    assert!(!raw.is_null());
                    unsafe { OwnedHandle::from_raw_handle(raw.cast()) }
                })
                .collect();
            for (epoch, origin, partitions) in [
                (1, 0_i64, &[4_usize, 4, 4, 2][..]),
                (2, 9_i64, &[3, 2, 1, 4, 4][..]),
            ] {
                runtime
                    .enter_state(AudioStateEntry {
                        epoch: AudioContinuityEpoch::new(epoch),
                        start_sample: origin,
                    })
                    .unwrap();
                let mut position = origin;
                for &frames in partitions {
                    let mut pcm = vec![99.0; frames * 2];
                    runtime
                        .render_into(
                            AudioRenderRequest { start_sample: position, frames },
                            &mut pcm,
                        )
                        .unwrap();
                    for (offset, pair) in pcm.chunks_exact(2).enumerate() {
                        let sample = position + offset as i64;
                        let indexed_source = 5 + (3 * sample).div_euclid(2);
                        let source = ((indexed_source % 31) - 15) as f32 / 1024.0;
                        let db = -6.0 + 6.0 * sample as f64 / 64.0;
                        let scope_input = source * 10.0_f64.powf(db / 20.0) as f32;
                        let native_gain = 0.5_f32
                            + (sample % 251) as f32 / 1024.0
                            + if with_music { 80.0 / 1024.0 } else { 0.0 };
                        let expected = (scope_input * 2.0) * native_gain;
                        assert!((pair[0] - expected).abs() <= 2.0e-6, "rate={rate} music={with_music} epoch={epoch} sample={sample} actual={} expected={expected}", pair[0]);
                        assert!((pair[1] + expected * 0.5).abs() <= 1.0e-6);
                    }
                    position += frames as i64;
                }
                let actual: Vec<_> = owners(&runtime)
                    .into_iter()
                    .map(|lease| runtime.observe_native_owner_object(lease).unwrap())
                    .collect();
                assert_eq!(
                    actual, cold,
                    "same actual CLAP objects survive the new epoch"
                );
            }
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
            drop(runtime);
            for handle in &handles {
                // Same observed child OS completion is separate from the
                // supervisor's original 100ms graceful-shutdown result.
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2000) },
                    0
                );
                let mut code = u32::MAX;
                assert_ne!(
                    unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut code) },
                    0
                );
                assert_eq!(code, 0);
            }
            println!(
                "{}",
                serde_json::json!({"case":"canonical-clap-tempo-only-scope-retime", "rate":rate,"music":with_music,"bpm":if with_music {80}else{0},"source_speed":"3/2","scope_in_seconds":5,"native_pdc_frames":3,"total_lookahead_frames":6,"epochs":2,"actual_native_pids":cold.iter().map(|o|o.process_id().get()).collect::<Vec<_>>(),"beats_or_rt_qualified":false})
            );
        }
    }
}
