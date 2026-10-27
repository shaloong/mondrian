//! Positive topology-change qualification. A refusal is a failure of this gate.
use super::*;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

#[test]
#[ignore = "matching production helper and CLAP mode19; stable-ID native input-width migration"]
fn installed_clap_runtime_restart_rebinds_mono_detector_without_replacing_native_owner() {
    restart_detector_width(Qualification::Install);
}

#[test]
#[ignore = "current production helper and CLAP mode19; inactive proposal only, not I/O migration"]
fn installed_clap_runtime_restart_observes_changed_inputs_without_resuming_old_geometry() {
    restart_detector_width(Qualification::Observe);
}

#[test]
#[ignore = "matching production helper and CLAP mode19; Root-granted inactive input staging only"]
fn installed_clap_runtime_restart_stages_new_inputs_under_original_root_grant() {
    restart_detector_width(Qualification::StageInputs);
}

#[test]
#[ignore = "matching production helper and CLAP mode19; stopped shared mapping staging"]
fn installed_clap_runtime_restart_stages_shared_inputs_without_releasing_old_dsp() {
    restart_detector_width(Qualification::StageMapping);
}

#[test]
#[ignore = "matching production helper and CLAP mode19; native capture, graph install pending"]
fn installed_clap_runtime_restart_captures_changed_inputs_without_old_geometry_seed() {
    restart_detector_width(Qualification::Capture);
}

#[test]
#[ignore = "matching production helper and CLAP mode20; Stereo-to-Mono installation"]
fn installed_clap_runtime_restart_rebinds_stereo_detector_to_mono_on_same_owner() {
    restart_detector_width(Qualification::Reverse);
}
#[test]
#[ignore = "matching production helper and CLAP modes21/22; repeated bidirectional installation"]
fn installed_clap_runtime_restart_repeats_width_changes_without_fee_accumulation() {
    restart_detector_width(Qualification::Repeat);
}
#[test]
#[ignore = "matching production helper and CLAP mode19; late Source refusal after native commit"]
fn installed_clap_runtime_changed_inputs_refuse_foreign_source_without_retry_or_pcm() {
    restart_detector_width(Qualification::RefuseCas);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Qualification {
    Observe,
    StageInputs,
    StageMapping,
    Capture,
    Install,
    Reverse,
    Repeat,
    RefuseCas,
}

fn restart_detector_width(qualification: Qualification) {
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            path("MONDRIAN_CLAP_TEST_HELPER"),
            [path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
        )
        .expect("actual native catalog"),
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let initial_mode = match qualification {
        Qualification::Reverse => 20,
        Qualification::Repeat => 21,
        _ => 19,
    };
    let installs = matches!(
        qualification,
        Qualification::Install
            | Qualification::Reverse
            | Qualification::Repeat
            | Qualification::RefuseCas
    );
    let mut state = vec![initial_mode];
    state.extend_from_slice(&0.5_f32.to_le_bytes());
    let instance = catalog
        .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
        .expect("initial declared input shape");
    assert!(instance.parameters.is_empty());
    let mut sequence = sequence_with_audio_clip();
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.settings.audio_sample_rate = render.sample_rate;
    let occurrence = AudioProcessorOccurrence {
        instance_id: instance.id,
        owner: AudioProcessorOccurrenceOwner::Output(sequence.audio_program.outputs[0].id),
        insertion: AudioProcessorInsertionPoint::PreFader,
    };
    sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
        AudioRouteSource::Track {
            track_id: sequence.audio_tracks[0].id,
            port: AudioChannelStripOutputPort::PostMute,
        },
        instance.id,
        "clap.audio-input.11",
    ));
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let author_before = serde_json::to_value(&sequence).unwrap();
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &StereoRampResolver,
        &IsolatedAudioProcessorResolver::new(catalog),
        render,
        None,
        AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024),
    )
    .expect("initial canonical DAG");
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier.clone()).unwrap();
    let mut claim = source
        .reserve_install(&registration, None)
        .and_then(|reservation| source.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    let cancellation = ExecutionCancellationToken::new();
    runtime.install_native_owner_bindings(&claim, &cancellation).unwrap();
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();
    let initial = cursors(&runtime);
    let address = AudioNativeProcessorAddress {
        root: initial.session.unwrap().identity,
        local: initial.processor.unwrap().identity,
        occurrence,
        epoch: Some(AudioContinuityEpoch::new(1)),
    };
    let lease = runtime.native_owner_lease(address).unwrap();
    let object = runtime.observe_native_owner_object(lease).unwrap();
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: the authenticated original owner supplies this PID. This handle
    // can query/wait only and is closed by OwnedHandle after all assertions.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(runtime.public_output_lookahead_frames(), 3);
        for start in [0, 4] {
            let mut pcm = [99.0; 8];
            runtime
                .render_into(
                    AudioRenderRequest {
                        transport_activity: None,
                        start_sample: start,
                        frames: 4,
                    },
                    &mut pcm,
                )
                .expect("complete old mono-detector window");
            // [r,-r] folds to zero in the mono detector; Main weight2 * gain.5.
            assert_pcm(&pcm, start, if initial_mode == 19 { 1.0 } else { 2.5 });
        }
        let mut pcm = [99.0; 8];
        let ticket = match runtime.render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 8,
                frames: 4,
            },
            &mut pcm,
        ) {
            Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
            other => panic!("actual native restart must hold publication: {other:?}"),
        };
        assert_eq!(pcm, [99.0; 8]);
        let held = cursors(&runtime);
        let before_service = runtime.resource_footprint();
        let outcome = if installs {
            runtime.service_pending_native_reprepare(ticket.product_notice(), &cancellation)
        } else {
            runtime.service_native_reprepare(ticket, &cancellation)
        };
        if !installs {
            assert!(matches!(
                outcome,
                Err(NativeAudioReprepareError::AudioInputChangeRequired)
            ));
            let proposal = *runtime
                .native_reprepare_audio_input_proposal(ticket)
                .expect("same retained inactive observation");
            let NativeObservedRequest::Cohort(cohort) = ticket.request else {
                panic!("exact cohort");
            };
            assert_eq!(proposal.observed_cohort, cohort);
            assert_eq!(
                proposal
                    .ports()
                    .map(|port| (port.native_id, port.channel_layout, port.is_main))
                    .collect::<Vec<_>>(),
                [
                    (0, AudioChannelLayout::Stereo, true),
                    (11, AudioChannelLayout::Stereo, false),
                    (7, AudioChannelLayout::Stereo, false)
                ]
            );
            let mut unrelated = ticket;
            unrelated.continuity_epoch = Some(AudioContinuityEpoch::new(99));
            assert!(runtime.native_reprepare_audio_input_proposal(unrelated).is_err());
            assert_eq!(cursors(&runtime), held);
            verifier.validate(&claim).unwrap();
            assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
            if qualification == Qualification::Capture {
                let before = runtime.resource_footprint();
                assert!(runtime
                    .capture_native_reprepare_audio_inputs(ticket, &cancellation)
                    .is_err());
                assert_eq!(runtime.resource_footprint(), before);
            }
            if matches!(
                qualification,
                Qualification::StageInputs | Qualification::StageMapping | Qualification::Capture
            ) {
                let before = runtime.resource_footprint();
                let canceled = ExecutionCancellationToken::new();
                canceled.cancel();
                assert!(matches!(
                    runtime.stage_native_reprepare_audio_input_buffers(ticket, &canceled),
                    Err(NativeAudioReprepareError::Cancelled)
                ));
                assert_eq!(runtime.resource_footprint(), before);
                let receipt = runtime
                    .stage_native_reprepare_audio_input_buffers(ticket, &cancellation)
                    .expect("Root-admitted real replacement input owners");
                assert_eq!(receipt.request.proposal, proposal);
                assert_eq!(receipt.request.max_block_frames, 4);
                assert_eq!(
                    receipt.request.buffer_grant_bytes,
                    receipt.reservation_bytes
                );
                assert!(
                    receipt.allocated_bytes > 0
                        && receipt.allocated_bytes <= receipt.reservation_bytes
                );
                let staged = runtime.resource_footprint();
                assert_eq!(
                    staged.fixed_resident_bytes,
                    before.fixed_resident_bytes + receipt.reservation_bytes
                );
                assert!(runtime
                    .stage_native_reprepare_audio_input_buffers(ticket, &cancellation)
                    .is_err());
                assert_eq!(
                    runtime.resource_footprint(),
                    staged,
                    "duplicate stage neither allocates nor credits"
                );
                assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
                assert_eq!(cursors(&runtime), held);
            }
            if matches!(
                qualification,
                Qualification::StageMapping | Qualification::Capture
            ) {
                let before = runtime.resource_footprint();
                let canceled = ExecutionCancellationToken::new();
                canceled.cancel();
                assert!(matches!(
                    runtime.stage_native_reprepare_audio_input_mapping(ticket, &canceled),
                    Err(NativeAudioReprepareError::Cancelled)
                ));
                assert_eq!(runtime.resource_footprint(), before);
                assert!(runtime
                    .stage_native_reprepare_audio_input_mapping(unrelated, &cancellation)
                    .is_err());
                assert_eq!(runtime.resource_footprint(), before);
                let receipt = runtime
                    .stage_native_reprepare_audio_input_mapping(ticket, &cancellation)
                    .expect("both real same-child shared mappings and backing acknowledgement");
                assert_eq!(receipt.request.buffers.request.proposal, proposal);
                assert_eq!(receipt.request.channel_layout, AudioChannelLayout::Stereo);
                assert_eq!(receipt.request.parameter_lane_count, 0);
                assert_eq!(receipt.request.auxiliary_output_count, 0);
                assert!(receipt.shared_bytes >= 4 * (2 + 2 + 2) * 4);
                let bound = receipt.request.mapping_grant_bytes;
                assert!(receipt.parent_owner_bytes >= receipt.shared_bytes);
                assert!(receipt.child_owner_bytes >= receipt.shared_bytes);
                assert!(
                    receipt.parent_owner_bytes <= bound / 2
                        && receipt.child_owner_bytes <= bound / 2
                );
                let staged = runtime.resource_footprint();
                assert_eq!(
                    staged.fixed_resident_bytes,
                    before.fixed_resident_bytes + bound
                );
                assert!(runtime
                    .stage_native_reprepare_audio_input_mapping(ticket, &cancellation)
                    .is_err());
                assert_eq!(
                    runtime.resource_footprint(),
                    staged,
                    "no duplicate allocation or credit"
                );
                assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
                assert_eq!(cursors(&runtime), held);
                verifier.validate(&claim).unwrap();
            }
            if qualification == Qualification::Capture {
                let before = runtime.resource_footprint();
                let canceled = ExecutionCancellationToken::new();
                canceled.cancel();
                assert!(matches!(
                    runtime.capture_native_reprepare_audio_inputs(ticket, &canceled),
                    Err(NativeAudioReprepareError::Cancelled)
                ));
                assert!(runtime
                    .capture_native_reprepare_audio_inputs(unrelated, &cancellation)
                    .is_err());
                assert_eq!(runtime.resource_footprint(), before);
                let operation = runtime
                    .capture_native_reprepare_audio_inputs(ticket, &cancellation)
                    .expect("same-instance activation and actual state/latency/tail");
                let facts = operation.configuration_identity();
                assert_eq!(facts.serviced_cohort, cohort);
                assert_eq!(facts.control_token, proposal.control_token);
                assert_eq!(facts.format, NativeConfigurationFormat::ClapState);
                assert_eq!(facts.observed_contract.algorithmic_latency_frames(), 3);
                assert_eq!(
                    facts.observed_contract.tail(),
                    AudioProcessorTail::Finite(3)
                );
                let mut state = vec![0];
                state.extend_from_slice(&0.5_f32.to_le_bytes());
                assert_eq!(
                    runtime.native_reprepare_configuration(&operation).unwrap(),
                    state
                );
                assert_eq!(
                    runtime.resource_footprint(),
                    before,
                    "all reservations stay held"
                );
                assert!(runtime
                    .capture_native_reprepare_audio_inputs(ticket, &cancellation)
                    .is_err());
                let prepared = runtime
                    .prepare_native_reprepare_plan(&operation, verifier.clone(), &cancellation)
                    .expect("new input Factory and canonical graph under the original Root grant");
                assert_eq!(prepared.schedule().maximum_processor_auxiliary_channels, 4);
                assert_ne!(prepared.identity(), held.session.unwrap().identity);
                assert!(
                    runtime.resource_footprint().fixed_resident_bytes > before.fixed_resident_bytes
                );
                assert!(runtime
                    .prepare_native_reprepare_plan(&operation, verifier.clone(), &cancellation)
                    .is_err());
                assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
                verifier.validate(&claim).unwrap();
                assert_eq!(cursors(&runtime), held);
            }
            assert!(
                runtime.resource_footprint().fixed_resident_bytes
                    > before_service.fixed_resident_bytes
            );
            assert_eq!(
                runtime.render_into(
                    AudioRenderRequest {
                        transport_activity: None,
                        start_sample: 8,
                        frames: 4,
                    },
                    &mut pcm
                ),
                Err(AudioExecutionError::ReprepareRequired(ticket))
            );
            assert_eq!(pcm, [99.0; 8]);
            assert_eq!(
                runtime.enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(2),
                    start_sample: 8,
                }),
                Err(AudioExecutionError::ReprepareRequired(ticket))
            );
            assert_eq!(cursors(&runtime), held);
            assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
            let replacement_claim = source
                .reserve_install(&registration, Some(&claim))
                .and_then(|reservation| source.begin_commit(reservation))
                .unwrap()
                .commit()
                .unwrap();
            assert_ne!(replacement_claim.generation(), claim.generation());
            assert!(
                runtime.native_reprepare_audio_input_proposal(ticket).is_err(),
                "a real Source revocation cannot be revived by a retained observation"
            );
            return;
        }
        let rounds = if qualification == Qualification::Repeat {
            3
        } else {
            1
        };
        let mut pending_operation = Some(outcome.expect("same-instance native input capture"));
        let mut start = 8;
        let mut held = held;
        let mut normalized_by_mode = std::collections::BTreeMap::new();
        for round in 0..rounds {
            let operation = pending_operation.take().unwrap();
            let mode = match initial_mode {
                19 => 0,
                20 => 17,
                _ if round % 2 == 0 => 22,
                _ => 21,
            };
            let mono = matches!(mode, 17 | 22);
            let mut expected_state = vec![mode];
            expected_state.extend_from_slice(&0.5_f32.to_le_bytes());
            assert_eq!(
                runtime.native_reprepare_configuration(&operation).unwrap(),
                expected_state
            );
            let prepared = runtime
                .prepare_native_reprepare_plan(&operation, verifier.clone(), &cancellation)
                .expect("canonical replacement graph under original Root grant");
            assert_eq!(
                prepared.schedule().maximum_processor_auxiliary_channels,
                if mono { 3 } else { 4 }
            );
            assert_eq!(cursors(&runtime), held);
            let current_lease = runtime
                .native_owner_lease(AudioNativeProcessorAddress {
                    root: held.session.unwrap().identity,
                    local: held.processor.unwrap().identity,
                    occurrence,
                    epoch: Some(AudioContinuityEpoch::new(round + 1)),
                })
                .unwrap();
            assert_eq!(
                runtime.observe_native_owner_object(current_lease).unwrap(),
                object
            );
            let mut authorization = source
                .authorize_runtime_installation(
                    source.reserve_install(&registration, Some(&claim)).unwrap(),
                )
                .unwrap();
            let before_install = runtime.resource_footprint();
            if qualification == Qualification::RefuseCas {
                let unrelated = AudioOwnerInstallationSource::new();
                let (other_registration, _) = unrelated.register().unwrap();
                authorization = unrelated
                    .authorize_runtime_installation(
                        unrelated.reserve_install(&other_registration, None).unwrap(),
                    )
                    .unwrap();
                assert!(matches!(
                    runtime.install_native_reprepare_plan(
                        &operation,
                        &mut authorization,
                        &cancellation
                    ),
                    Err(NativeAudioReprepareError::Owner(
                        AudioNativeOwnerError::Installation(
                            mondrian_core::AudioOwnerInstallationError::WrongRegistration
                        )
                    ))
                ));
                verifier.validate(&claim).unwrap();
                assert_eq!(
                    source.current_claim().unwrap().unwrap().generation(),
                    claim.generation()
                );
                assert_eq!(cursors(&runtime), held);
                assert_eq!(
                    runtime.observe_native_owner_object(current_lease).unwrap(),
                    object
                );
                let failed = runtime.resource_footprint();
                assert_eq!(
                    failed.fixed_resident_bytes,
                    before_install.fixed_resident_bytes + expected_state.len()
                );
                let mut retry = source
                    .authorize_runtime_installation(
                        source.reserve_install(&registration, Some(&claim)).unwrap(),
                    )
                    .unwrap();
                assert!(matches!(
                    runtime.install_native_reprepare_plan(&operation, &mut retry, &cancellation),
                    Err(NativeAudioReprepareError::Admission(
                        "restart installation already attempted or not prepared"
                    ))
                ));
                assert_eq!(runtime.resource_footprint(), failed);
                pcm.fill(99.0);
                assert_eq!(
                    runtime.render_into(
                        AudioRenderRequest {
                            transport_activity: None,
                            start_sample: start,
                            frames: 4
                        },
                        &mut pcm
                    ),
                    Err(AudioExecutionError::ReprepareRequired(ticket))
                );
                assert_eq!(
                    runtime.enter_state(AudioStateEntry {
                        epoch: AudioContinuityEpoch::new(2),
                        start_sample: start
                    }),
                    Err(AudioExecutionError::ReprepareRequired(ticket))
                );
                assert_eq!(pcm, [99.0; 8]);
                assert_eq!(cursors(&runtime), held);
                drop(operation);
                assert_eq!(runtime.resource_footprint(), failed);
                assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
                return;
            }
            let receipt = runtime
                .install_native_reprepare_plan(&operation, &mut authorization, &cancellation)
                .expect("same-owner planes/maps and Source installation");
            assert_ne!(receipt.claim().generation(), claim.generation());
            verifier.validate(receipt.claim()).unwrap();
            assert_eq!(runtime.public_output_lookahead_frames(), 3);
            pcm.fill(99.0);
            assert_eq!(
                runtime.render_into(
                    AudioRenderRequest {
                        transport_activity: None,
                        start_sample: start,
                        frames: 4
                    },
                    &mut pcm
                ),
                Err(AudioExecutionError::StateEntryRequired)
            );
            assert_eq!(
                pcm, [0.0; 8],
                "admitted Unentered Runtime initializes output"
            );
            let epoch = AudioContinuityEpoch::new(round + 2);
            runtime.enter_state(AudioStateEntry { epoch, start_sample: start }).unwrap();
            let next = cursors(&runtime);
            let new_lease = runtime
                .native_owner_lease(AudioNativeProcessorAddress {
                    root: next.session.unwrap().identity,
                    local: next.processor.unwrap().identity,
                    occurrence,
                    epoch: Some(epoch),
                })
                .unwrap();
            assert_eq!(
                runtime.observe_native_owner_object(new_lease).unwrap(),
                object
            );
            for (offset, frames) in [(0, 1), (1, 3), (4, 4)] {
                let mut pcm = vec![99.0; frames * 2];
                runtime
                    .render_into(
                        AudioRenderRequest {
                            transport_activity: None,
                            start_sample: start + offset,
                            frames,
                        },
                        &mut pcm,
                    )
                    .expect("complete new geometry PCM");
                assert_pcm(&pcm, start + offset, if mono { 1.0 } else { 2.5 });
            }
            claim = receipt.claim().clone();
            let high = runtime.resource_footprint();
            runtime.finish_native_reprepare_retirement(receipt.retire()).unwrap();
            let normalized = runtime.resource_footprint();
            assert!(normalized.fixed_resident_bytes < high.fixed_resident_bytes);
            assert_eq!(normalized.runtime_occurrences, 1);
            if let Some(previous) = normalized_by_mode.insert(mode, normalized) {
                assert_eq!(
                    normalized, previous,
                    "same shape/state must not accumulate capture/codec/map/seed fees"
                );
            }
            assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
            if round + 1 < rounds {
                start += 8;
                pcm.fill(99.0);
                let ticket = match runtime.render_into(
                    AudioRenderRequest {
                        transport_activity: None,
                        start_sample: start,
                        frames: 4,
                    },
                    &mut pcm,
                ) {
                    Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
                    other => panic!("next real restart: {other:?}"),
                };
                assert_eq!(pcm, [99.0; 8]);
                held = cursors(&runtime);
                pending_operation = Some(
                    runtime
                        .service_pending_native_reprepare(ticket.product_notice(), &cancellation)
                        .expect("next same-child width capture under unchanged Root grant"),
                );
            }
        }
        assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
    }));
    // Keep failure reporting after original-owner cleanup, including RED gates.
    drop(runtime);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
    println!(
        "{}",
        serde_json::json!({"case":"clap-dynamic-detector-width",
        "native_pid":pid,"post_drop_wait_status":settled,
        "exit_query_succeeded":queried!=0,"exit_code":exit})
    );
    assert_eq!(
        settled, 0,
        "original child must settle even when qualification fails"
    );
    assert_ne!(queried, 0);
    assert_eq!(exit, 0);
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

fn assert_pcm(pcm: &[f32], start: i64, gain: f32) {
    for (offset, pair) in pcm.chunks_exact(2).enumerate() {
        let expected = (start + offset as i64 + 1) as f32 * gain;
        assert_eq!(pair, [expected, -expected]);
    }
}
