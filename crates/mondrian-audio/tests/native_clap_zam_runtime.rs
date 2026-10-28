//! Finite third-party CLAP sidechain qualification; no vendor DSP implementation.
//! Uses the actual Runtime and consumer Source for reset-triggered reconfiguration.
#![cfg(windows)]

use mondrian_audio::{
    compile_audio_program, probe_clap_plugin_registration, AudioCompileRequest,
    AudioContinuityEpoch, AudioDecodedSource, AudioExecutionError, AudioMediaResolver,
    AudioProcessingMode, AudioProgramRuntime, AudioRenderContract, AudioRenderRequest,
    AudioRuntimeResourceGrant, AudioStateEntry, ClapMainAudioInput,
    DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
    NativeAudioAuthorProgram, NativeAudioParameterHandoffContinuity, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioSourceComponentId,
    ExactAutomationCurve, ExecutionCancellationToken, NativeAudioAuthorCallback,
    NativeAudioParameterRescanKind, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioProcessorSidechainRoute, AudioRouteSource,
};
use mondrian_timeline::{Clip, Sequence};
use std::{path::PathBuf, sync::Arc};

struct Source {
    main: AssetId,
    detector_asset: AssetId,
    detector: f32,
}
impl AudioMediaResolver for Source {
    fn resolve(
        &self,
        asset: AssetId,
        component: AudioSourceComponentId,
        sample_rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        assert_eq!(sample_rate, 48_000);
        assert_eq!(component, AudioSourceComponentId::primary());
        let signal = if asset == self.main {
            [0.03125, -0.0625]
        } else {
            assert_eq!(asset, self.detector_asset);
            [self.detector; 2]
        };
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Stereo,
            Arc::new(Signal(signal)),
        ))
    }
}
struct Signal([f32; 2]);
impl AudioDecodedSource for Signal {
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        pcm: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        assert!(!cancellation.is_canceled());
        assert_eq!(pcm.len(), frames * 2);
        for (index, frame) in pcm.chunks_exact_mut(2).enumerate() {
            frame.copy_from_slice(&if start_frame + (index as i64) < 0 {
                [0.0; 2]
            } else {
                self.0
            });
        }
        Ok(())
    }
}

fn render_cases(cases: &[(f64, f32)]) -> Vec<Vec<f32>> {
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").unwrap());
    let plugin = PathBuf::from(std::env::var_os("MONDRIAN_CLAP_MIXED_VENDOR_TEST_PLUGIN").unwrap());
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
            .unwrap(),
    );
    const ID: &str = "com.zamaudio.ZamCompX2";
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(catalog.descriptors()[0].plugin_id, ID);
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let registration = probe_clap_plugin_registration(&helper, &plugin, ID, render, None).unwrap();
    assert_eq!(registration.main_audio_input, ClapMainAudioInput::Present);
    assert_eq!(registration.auxiliary_inputs.buses.len(), 1);
    let bus = &registration.auxiliary_inputs.buses[0];
    assert_eq!(bus.channel_layout, AudioChannelLayout::Mono);
    assert!(registration.auxiliary_outputs.buses.is_empty());
    assert_eq!(registration.parameters.len(), 11);
    assert_eq!(
        registration.parameters.iter().filter(|p| p.read_only).count(),
        2
    );
    let initial = catalog.create_instance(ID, render, None).unwrap();
    assert_eq!(initial.parameters.len(), 9);
    assert!(initial.parameters.values().all(|p| p.automation.is_none()));
    let state = initial.opaque_state.as_ref().expect("real native default state");
    assert!(!state.is_empty());
    let mut results = Vec::new();
    // Same low Main signal: only external detector and the public sidechain
    // parameter differ. No test-specific behavior exists in the host adapter.
    for &(enabled, detector) in cases {
        let mut instance = catalog.create_instance(ID, render, Some(state.to_vec())).unwrap();
        assert_eq!(instance.opaque_state.as_ref(), Some(state));
        for (id, value) in [
            (0, 1.0),
            (2, 0.0),
            (3, 4.0),
            (4, -18.0),
            (5, 0.0),
            (8, enabled),
        ] {
            let descriptor = registration.parameters.iter().find(|p| p.id == id).unwrap();
            let parameter_id = descriptor.parameter_id().unwrap();
            instance
                .parameters
                .get_mut(&parameter_id)
                .unwrap()
                .set_automation(ExactAutomationCurve::new(parameter_id.clone(), value).unwrap())
                .unwrap();
        }
        let mut sequence = Sequence::new("ZamCompX2 finite mixed-input qualification");
        sequence.settings.audio_channel_layout = render.channel_layout;
        sequence.settings.audio_sample_rate = render.sample_rate;
        let main = sequence.audio_tracks[0].id;
        let detector_track = sequence.add_audio_track();
        let main_asset = AssetId::new();
        let detector_asset = AssetId::new();
        for (track, asset) in [(main, main_asset), (detector_track, detector_asset)] {
            sequence
                .add_media_audio_clip(
                    track,
                    Clip::new(asset, TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
                    AudioSourceComponentId::primary(),
                )
                .unwrap();
        }
        let processor = instance.id;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
        sequence.audio_program.routes.retain(|route|
            matches!(route.source, AudioRouteSource::Track { track_id, .. } if track_id == main));
        sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
            AudioRouteSource::Track {
                track_id: detector_track,
                port: AudioChannelStripOutputPort::PostMute,
            },
            processor,
            bus.bus_key.clone(),
        ));
        let before = serde_json::to_value(&sequence).unwrap();
        let source = AudioOwnerInstallationSource::new();
        let (source_registration, verifier) = source.register().unwrap();
        let resolver = IsolatedAudioProcessorResolver::new(catalog.clone());
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &Source { main: main_asset, detector_asset, detector },
            &resolver,
            render,
            None,
            AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(runtime.public_output_lookahead_frames(), 0);
        runtime.bind_native_owner_installation(verifier.clone()).unwrap();
        let mut claim = source
            .reserve_install(&source_registration, None)
            .and_then(|reservation| source.begin_commit(reservation))
            .unwrap()
            .commit()
            .unwrap();
        let cancellation = ExecutionCancellationToken::new();
        runtime.install_native_owner_bindings(&claim, &cancellation).unwrap();
        // Loading this captured native state reports its existing values. Accept
        // that exact echo through the real Plan/Source/Ack transaction. Host
        // overrides stay authoritative; this is not a policy for arbitrary edits.
        let proposal = runtime
            .poll_native_observations(&cancellation)
            .unwrap()
            .and_then(|batch| batch.author)
            .expect("actual state-load rescan");
        assert_eq!(
            proposal.state(),
            Some(state),
            "only the unchanged loaded seed is admitted"
        );
        assert_eq!(
            (proposal.open_parameter_edits(), proposal.group_depth()),
            (0, 0)
        );
        let [NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values }] =
            proposal.callbacks()
        else {
            panic!(
                "unexpected initial native author edits: {:?}",
                proposal.callbacks()
            );
        };
        assert_eq!(*kind, NativeAudioParameterRescanKind::Values);
        let expected_values = registration
            .parameters
            .iter()
            .filter(|p| !p.read_only)
            .map(|p| (p.parameter_id().unwrap(), p.current_value.to_bits()))
            .collect::<Vec<_>>();
        assert_eq!(values, &expected_values);
        let programs = [NativeAudioAuthorProgram {
            sequence: sequence.id,
            program: Arc::new(
                compile_audio_program(
                    &sequence,
                    AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
                )
                .unwrap(),
            ),
        }];
        let mut handoff = runtime
            .prepare_native_author_parameter_handoff(
                &programs,
                &proposal,
                verifier.clone(),
                &cancellation,
            )
            .expect("unchanged seed rescan with host overrides retained");
        let reservation = source.reserve_install(&source_registration, Some(&claim)).unwrap();
        let mut authorization = source.authorize_runtime_installation(reservation).unwrap();
        let installed = runtime
            .install_native_author_parameter_handoff(
                &mut handoff,
                &mut authorization,
                &cancellation,
            )
            .unwrap();
        claim = installed.claim;
        assert_eq!(
            installed.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        drop(handoff);
        let acknowledged = runtime
            .acknowledge_native_author_parameter_handoff(&proposal, &cancellation)
            .unwrap();
        assert_eq!(acknowledged.claim.generation(), claim.generation());
        runtime.validate_native_author_source_closed(&cancellation).unwrap();
        assert_eq!(serde_json::to_value(&sequence).unwrap(), before);
        let mut reference = None;
        let mut epoch = 0;
        let mut installs = 0;
        for (start, partitions) in [(0, [64, 64]), (0, [17, 47]), (97, [47, 17])] {
            epoch += 1;
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch),
                    start_sample: start,
                })
                .unwrap_or_else(|error| {
                    let observed = runtime.poll_native_observations(&cancellation);
                    panic!("state entry failed: enabled={enabled}, detector={detector}, epoch={epoch}, error={error:?}, observed={observed:?}");
                });
            let mut output = Vec::new();
            let mut block_index = 0;
            while output.len() < 2048 {
                let frames = partitions[block_index % 2].min((2048 - output.len()) / 2);
                let mut pcm = vec![f32::NAN; frames * 2];
                let request = AudioRenderRequest {
                    transport_activity: None,
                    start_sample: start + output.len() as i64 / 2,
                    frames,
                };
                match runtime.render_into(request, &mut pcm) {
                    Ok(()) => {}
                    Err(AudioExecutionError::ReprepareRequired(ticket)) => {
                        assert!(installs < 2, "unexpected recurring restart: {ticket:?}");
                        assert!(
                            pcm.iter().all(|sample| sample.to_bits() == f32::NAN.to_bits()),
                            "preflight HOLD must preserve every output word"
                        );
                        let operation = runtime
                            .service_pending_native_reprepare(
                                ticket.product_notice(),
                                &cancellation,
                            )
                            .unwrap_or_else(|error| panic!(
                                "capture failed: enabled={enabled}, detector={detector}, epoch={epoch}, start={}, completed_frames={}, installs={installs}, error={error:?}",
                                request.start_sample, output.len() / 2,
                            ));
                        runtime
                            .prepare_native_reprepare_plan(
                                &operation,
                                verifier.clone(),
                                &cancellation,
                            )
                            .expect("prepare under original resource grant");
                        let reservation =
                            source.reserve_install(&source_registration, Some(&claim)).unwrap();
                        let mut authorization =
                            source.authorize_runtime_installation(reservation).unwrap();
                        let receipt = runtime
                            .install_native_reprepare_plan(
                                &operation,
                                &mut authorization,
                                &cancellation,
                            )
                            .expect("install retained native owner");
                        claim = receipt.claim().clone();
                        runtime.finish_native_reprepare_retirement(receipt.retire()).unwrap();
                        epoch += 1;
                        runtime
                            .enter_state(AudioStateEntry {
                                epoch: AudioContinuityEpoch::new(epoch),
                                start_sample: request.start_sample,
                            })
                            .expect("Fresh after installation");
                        installs += 1;
                        runtime.render_into(request, &mut pcm)
                            .unwrap_or_else(|error| panic!("installed Fresh must produce PCM: enabled={enabled}, detector={detector}, epoch={epoch}, error={error:?}"));
                    }
                    Err(error) => panic!("native render failed: {error:?}"),
                }
                assert!(pcm.iter().all(|x| x.is_finite()));
                output.extend(pcm);
                block_index += 1;
            }
            if let Some(expected) = &reference {
                assert_eq!(&output, expected, "partition/seek invariance");
            } else {
                reference = Some(output);
            }
        }
        assert_eq!(
            installs, 2,
            "both repeated state entries service actual reset restart"
        );
        assert_eq!(serde_json::to_value(&sequence).unwrap(), before);
        results.push(reference.unwrap());
    }
    results
}

#[test]
#[ignore = "requires official ZamCompX2 4.5 CLAP, matching Product and original native Job"]
fn installed_zam_compx2_clap_mono_sidechain_controls_canonical_stereo_pcm() {
    let results = render_cases(&[(1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]);
    for dry in &results[..2] {
        for frame in dry.chunks_exact(2) {
            assert_eq!(frame, [0.03125, -0.0625]);
        }
    }
    let compressed = &results[2];
    assert_ne!(compressed, &results[0]);
    for frame in compressed.chunks_exact(2).skip(512) {
        let gain = frame[0] / 0.03125;
        assert!(
            gain > 0.0 && gain < 0.5,
            "external detector must attenuate low Main"
        );
        assert_eq!(frame[1], -2.0 * frame[0], "stereo relationship is retained");
    }
}

// A separately named finite acceptance case; the complete sidechain gate above
// retains all three scenarios and must still pass before broader qualification.
#[test]
#[ignore = "official ZamCompX2 4.5 CLAP; 48k Offline, enabled silent detector only"]
fn installed_zam_compx2_clap_silent_detector_restarts_preserve_complete_dry_pcm() {
    let results = render_cases(&[(1.0, 0.0)]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].len(), 2048);
    for frame in results[0].chunks_exact(2) {
        assert_eq!(frame, [0.03125, -0.0625]);
    }
}
