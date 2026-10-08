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
