//! One owned VST3 latency restart through the existing Audio Runtime/Source.
//! Offline software PCM only: no Media/output quiescence, device clock, author
//! state transaction, retained private DSP history or complete heap qualification.

use super::*;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::Ordering::SeqCst;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

fn runtime() -> RetainedRestartRuntime {
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(
            path("MONDRIAN_VST3_TEST_HELPER"),
            [path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")],
        )
        .expect("actual owned VST3 catalog"),
    );
    let mut render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let instance = catalog
        .create_instance(
            &catalog.descriptors()[0].class_id,
            render,
            Some(crate::processor_isolation::vst3_sidechain_snapshot(0.5, 5)),
        )
        .expect("actual authored component/controller mode5 envelope");
    assert_eq!(
        instance.parameters.len(),
        1,
        "one real normalized gain lane"
    );
    let mut sequence = sequence_with_audio_clip();
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.settings.audio_sample_rate = render.sample_rate;
    let occurrence = AudioProcessorOccurrence {
        instance_id: instance.id,
        owner: AudioProcessorOccurrenceOwner::Output(sequence.audio_program.outputs[0].id),
        insertion: AudioProcessorInsertionPoint::PreFader,
    };
    let spec = catalog
        .resolve(AudioProcessorPrepareRequest::new(
            occurrence,
            &instance.definition,
            &instance.parameters,
            instance.opaque_state.as_ref().map(AsRef::as_ref),
            render,
        ))
        .expect("real fixed VST3 metadata");
    // This same no-Note Spec carries the actual catalog NativeProbed evidence;
    // consuming it creates no additional validation worker.
    let factory = crate::processor_isolation::IsolatedAudioProcessorFactory::prepare(spec)
        .expect("existing admitted Factory metadata");
    let contract = factory.execution_contract();
    assert_eq!(contract.algorithmic_latency_frames(), 3);
    assert!(factory.note_input_contract().ports.is_empty());
    let processor_session_bytes = contract.session_scratch_bytes();
    let grant = AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024);
    assert!(
        processor_session_bytes > 0 && processor_session_bytes <= grant.max_fixed_resident_bytes
    );
    render.processor_session_scratch_budget_bytes = processor_session_bytes;
    let detector = factory
        .auxiliary_input_contract()
        .buses
        .first()
        .expect("actual first auxiliary input")
        .bus_key
        .clone();
    drop(factory);
    sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
        AudioRouteSource::Track {
            track_id: sequence.audio_tracks[0].id,
            port: AudioChannelStripOutputPort::PostMute,
        },
        instance.id,
        detector,
    ));
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let author_before = serde_json::to_value(&sequence).unwrap();
    let processors = IsolatedAudioProcessorResolver::new(catalog);
    let runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &StereoRampResolver,
        &processors,
        render,
        None,
        grant,
    )
    .expect("real fixed-root VST3 Runtime");
    assert_eq!(
        runtime.resource_footprint().processor_session_bytes,
        processor_session_bytes
    );
    RetainedRestartRuntime {
        runtime,
        sequence,
        occurrence,
        render,
        grant,
        processor_session_bytes,
        mode: 5,
        author_before,
        lost_reply: None,
    }
}

fn assert_pcm(pcm: &[f32], start: i64) {
    assert_eq!(pcm.len(), 8);
    for (offset, pair) in pcm.chunks_exact(2).enumerate() {
        let value = (start as usize + offset + 1) as f32 * 2.5;
        assert_eq!(
            pair,
            [value, -value],
            "main2 + detector3 at gain.5, public sample {}",
            start + offset as i64
        );
    }
}

#[test]
#[ignore = "current normal product and owned VST3 mode5/6; fixed-root latency restart, explicit fresh component/PDC PCM; no Media quiescence"]
fn installed_vst3_runtime_latency_restart_enters_fresh_component_with_canonical_pcm() {
    let RetainedRestartRuntime {
        mut runtime,
        sequence,
        occurrence,
        grant,
        processor_session_bytes,
        author_before,
        ..
    } = runtime();
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier.clone()).unwrap();
    let claim = source
        .begin_commit(source.reserve_install(&registration, None).unwrap())
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
    let old_identity = initial.session.unwrap().identity;
    let address = AudioNativeProcessorAddress {
        root: old_identity,
        local: old_identity,
        occurrence,
        epoch: Some(AudioContinuityEpoch::new(1)),
    };
    let lease = runtime.native_owner_lease(address).expect("actual initial installed lease");
    let object = runtime
        .observe_native_owner_object(lease)
        .expect("authenticated initial VST3 object");
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: authenticated original child PID; retain only query/synchronize.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let counts = Arc::new(ChildCounts::default());
    let mut registrar = None;
    let mut installed = false;
    let mut domain_bound = false;
    let mut observed_fresh_object = None;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(runtime.public_output_lookahead_frames(), 3);
        let mut pending = None;
        let mut completed_public_frames = 0;
        for start in [0, 4, 8] {
            let mut pcm = [99.0; 8];
            match runtime.render_into(
                AudioRenderRequest { start_sample: start, frames: 4 },
                &mut pcm,
            ) {
                Ok(()) => {
                    assert_pcm(&pcm, start);
                    completed_public_frames += 4;
                }
                Err(AudioExecutionError::ReprepareRequired(ticket)) => {
                    pending = Some((ticket, start));
                    break;
                }
                other => panic!("actual first VST3 latency callback: {other:?}"),
            }
        }
        // The first successful native Process changes mode5→6. Its controller
        // sync may become visible during PDC priming, before any public block
        // finishes. Do not force CLAP's old public8/execution11 trigger here.
        let (ticket, held_start) =
            pending.expect("real flags8 HOLD within the original bounded native work");
        let NativeObservedRequest::Cohort(cohort) = ticket.request else {
            panic!("actual serial cohort");
        };
        assert_ne!(cohort.serial, 0);
        assert_ne!(cohort.serial, u32::MAX);
        assert_eq!(
            cohort.request,
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 }
        );
        assert_eq!(ticket.prepared_plan_id, old_identity.prepared_plan_id);
        assert_eq!(ticket.session_id, old_identity.session_id);
        assert_eq!(ticket.occurrence, occurrence);
        assert_eq!(ticket.continuity_epoch, Some(AudioContinuityEpoch::new(1)));
        let held = cursors(&runtime);
        assert_eq!(held.session.unwrap().identity, old_identity);
        assert_eq!(held.processor.unwrap().occurrence, occurrence);
        let original = runtime.resource_footprint();
        let mut untouched = [99.0; 8];
        assert_eq!(
            runtime.render_into(
                AudioRenderRequest { start_sample: held_start, frames: 4 },
                &mut untouched
            ),
            Err(AudioExecutionError::ReprepareRequired(ticket))
        );
        assert_eq!(untouched, [99.0; 8], "known HOLD is a preflight refusal");
        let canceled = ExecutionCancellationToken::new();
        canceled.cancel();
        assert!(matches!(
            runtime.service_native_reprepare(ticket, &canceled),
            Err(NativeAudioReprepareError::Cancelled)
        ));
        let mut foreign = ticket;
        foreign.session_id = AudioRenderSessionId::new();
        assert!(matches!(
            runtime.service_native_reprepare(foreign, &cancellation),
            Err(NativeAudioReprepareError::Admission(_))
        ));
        assert_eq!(runtime.resource_footprint(), original);
        assert_eq!(cursors(&runtime), held);
        verifier.validate(&claim).unwrap();
        let operation = runtime
            .service_native_reprepare(ticket, &cancellation)
            .expect("real same-child VST3 component/controller capture");
        let captured = operation.configuration_identity();
        assert_eq!(captured.serviced_cohort, cohort);
        assert_eq!(
            captured.format,
            NativeConfigurationFormat::Vst3ComponentAndController
        );
        assert_eq!(captured.observed_contract.algorithmic_latency_frames(), 7);
        assert_eq!(
            captured.observed_contract.tail(),
            AudioProcessorTail::Finite(7)
        );
        let seed_len = runtime.native_reprepare_configuration(&operation).unwrap().len();
        assert!(seed_len > 0 && seed_len <= 64 * 1024);
        assert_eq!(captured.configuration_bytes, seed_len);
        let captured_resources = runtime.resource_footprint();
        assert!(captured_resources.fixed_resident_bytes > original.fixed_resident_bytes);
        assert!(captured_resources.fixed_resident_bytes <= grant.max_fixed_resident_bytes);
        assert_eq!(cursors(&runtime), held);
        assert_eq!(runtime.native_owner_lease(address).unwrap(), lease);
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
        assert!(matches!(
            runtime.prepare_native_reprepare_plan(&operation, verifier.clone(), &canceled),
            Err(NativeAudioReprepareError::Cancelled)
        ));
        assert_eq!(runtime.resource_footprint(), captured_resources);
        let unrelated = AudioOwnerInstallationSource::new();
        let (_, wrong) = unrelated.register().unwrap();
        assert!(matches!(
            runtime.prepare_native_reprepare_plan(&operation, wrong, &cancellation),
            Err(NativeAudioReprepareError::Admission(_))
        ));
        assert_eq!(runtime.resource_footprint(), captured_resources);
        let plan = runtime
            .prepare_native_reprepare_plan(&operation, verifier.clone(), &cancellation)
            .expect("same Program canonical PDC7 and empty Session");
        assert_ne!(plan.identity().session_id, old_identity.session_id);
        assert_ne!(
            plan.identity().prepared_plan_id,
            old_identity.prepared_plan_id
        );
        assert_eq!(plan.schedule().public_output_lookahead_frames, 7);
        assert_eq!(plan.schedule().processor_occurrence_count, 1);
        assert_eq!(plan.schedule().sidechain_binding_count, 1);
        assert_eq!(
            plan.schedule().processor_session_scratch_bytes,
            processor_session_bytes
        );
        let prepared = runtime.resource_footprint();
        assert_eq!(cursors(&runtime), held);
        verifier.validate(&claim).unwrap();
        // A late real domain must survive the same Hosted Box transfer; no new
        // ProcessOwner is registered by either preparation or installation.
        registrar =
            Some(mondrian_core::NativeChildSettlementRegistrar::try_new(counts.clone()).unwrap());
        runtime.bind_native_child_settlement(registrar.as_ref().unwrap()).unwrap();
        domain_bound = true;
        assert_eq!(counts.children.load(SeqCst), 1);
        assert_eq!(counts.scopes.load(SeqCst), 2);
        let mut authorization = source
            .authorize_runtime_installation(
                source.reserve_install(&registration, Some(&claim)).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            runtime.install_native_reprepare_plan(&operation, &mut authorization, &canceled),
            Err(NativeAudioReprepareError::Cancelled)
        ));
        assert_eq!(runtime.resource_footprint(), prepared);
        assert_eq!(cursors(&runtime), held);
        let receipt = runtime
            .install_native_reprepare_plan(&operation, &mut authorization, &cancellation)
            .expect("single VST3 Stage/Commit then canonical Source CAS");
        installed = true;
        assert_eq!(receipt.identity(), plan.identity());
        assert_eq!(receipt.previous_epoch(), Some(AudioContinuityEpoch::new(1)));
        let next_claim = receipt.claim().clone();
        verifier.validate(&next_claim).unwrap();
        assert!(verifier.validate(&claim).is_err());
        assert_ne!(next_claim.generation(), claim.generation());
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            next_claim.generation()
        );
        let unentered = cursors(&runtime);
        assert_eq!(unentered.session.unwrap().identity, plan.identity());
        assert_eq!(
            unentered.session.unwrap().state,
            AudioSessionCursorState::Unentered
        );
        assert_eq!(
            unentered.processor.unwrap().state,
            AudioProcessorCursorState::Unentered
        );
        assert_eq!(runtime.public_output_lookahead_frames(), 7);
        assert!(runtime.native_owner_lease(address).is_err());
        untouched.fill(99.0);
        assert_eq!(
            runtime.render_into(
                AudioRenderRequest { start_sample: 20, frames: 4 },
                &mut untouched
            ),
            Err(AudioExecutionError::StateEntryRequired)
        );
        assert_eq!(
            untouched, [0.0; 8],
            "Runtime admitted refusal initializes scratch/caller PCM"
        );
        let high = runtime.resource_footprint();
        assert_eq!(
            high.fixed_resident_bytes,
            prepared.fixed_resident_bytes + seed_len
        );
        assert_eq!(
            high.prepared_logical_bytes,
            plan.resources().prepared_logical_bytes
        );
        assert_eq!(
            high.compensation_delay_bytes,
            plan.resources().compensation_delay_bytes
        );
        assert_eq!(high.runtime_occurrences, 1);
        assert_eq!(counts.children.load(SeqCst), 1);
        assert_eq!(counts.scopes.load(SeqCst), 3);
        assert!(matches!(
            runtime.service_native_reprepare(ticket, &cancellation),
            Err(NativeAudioReprepareError::Admission(
                "an actual restart attempt is already retained"
            ))
        ));
        let completion = receipt.retire();
        assert_eq!(counts.children.load(SeqCst), 1);
        assert_eq!(counts.scopes.load(SeqCst), 2);
        assert_eq!(
            runtime.resource_footprint(),
            high,
            "owner Drop alone cannot refund Root credit"
        );
        runtime.finish_native_reprepare_retirement(completion).unwrap();
        assert_eq!(cursors(&runtime), unentered);
        let normalized = runtime.resource_footprint();
        assert!(normalized.fixed_resident_bytes < high.fixed_resident_bytes);
        assert_eq!(
            normalized.prepared_logical_bytes,
            high.prepared_logical_bytes
        );
        assert_eq!(normalized.runtime_occurrences, 1);
        runtime
            .enter_state_cancellable(
                AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(2),
                    start_sample: 20,
                },
                &cancellation,
            )
            .expect(
                "explicit Fresh Enter reloads the installed component/controller configuration",
            );
        let fresh_address = AudioNativeProcessorAddress {
            root: plan.identity(),
            local: plan.identity(),
            occurrence,
            epoch: Some(AudioContinuityEpoch::new(2)),
        };
        let fresh_lease = runtime.native_owner_lease(fresh_address).unwrap();
        assert_ne!(fresh_lease.binding(), lease.binding());
        assert_eq!(fresh_lease.installed_root_serial(), next_claim.generation());
        assert!(runtime.native_owner_lease(address).is_err());
        let fresh_object = runtime
            .observe_native_owner_object(fresh_lease)
            .expect("actual authenticated observation after native reload, not PID inference");
        assert_eq!(fresh_object.process_id(), object.process_id());
        assert!(fresh_object.object_serial() > object.object_serial());
        observed_fresh_object = Some(fresh_object);
        for start in [20, 24] {
            let mut pcm = [99.0; 8];
            runtime
                .render_into(
                    AudioRenderRequest { start_sample: start, frames: 4 },
                    &mut pcm,
                )
                .expect("complete public canonical PDC7 window without seven leading zeros");
            assert_pcm(&pcm, start);
        }
        let active = cursors(&runtime);
        let rate = active.session.unwrap().sample_rate;
        assert_eq!(
            active.session.unwrap().state,
            AudioSessionCursorState::Active {
                epoch: AudioContinuityEpoch::new(2),
                next_public_sample: mondrian_core::AudioSamplePosition::new(28, rate),
                next_execution_sample: mondrian_core::AudioSamplePosition::new(35, rate),
                alignment_primed: true,
            }
        );
        assert_eq!(
            active.processor.unwrap().state,
            AudioProcessorCursorState::Active {
                epoch: AudioContinuityEpoch::new(2),
                next_input_signal_sample: mondrian_core::AudioSamplePosition::new(35, rate),
            }
        );
        assert_eq!(
            runtime.observe_native_owner_object(fresh_lease).unwrap(),
            fresh_object
        );
        assert_eq!(runtime.resource_footprint(), normalized);
        assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
        // Actual Core slot supersession, after positive PCM/retirement facts.
        // A stale Runtime cannot start another capture or spend new credit.
        let before_supersede = runtime.resource_footprint();
        let superseding = source
            .begin_commit(source.reserve_install(&registration, Some(&next_claim)).unwrap())
            .unwrap()
            .commit()
            .unwrap();
        verifier.validate(&superseding).unwrap();
        assert!(matches!(
            runtime.service_native_reprepare(ticket, &cancellation),
            Err(NativeAudioReprepareError::Owner(
                AudioNativeOwnerError::Installation(
                    mondrian_core::AudioOwnerInstallationError::Superseded
                )
            ))
        ));
        assert_eq!(runtime.resource_footprint(), before_supersede);
        assert_eq!(cursors(&runtime), active);
        assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
        println!(
            "{}",
            serde_json::json!({"case":"vst3-runtime-latency-restart", "old_public_frames":completed_public_frames,
            "held_start":held_start,"held":format!("{held:?}"),"old_object":format!("{object:?}"),
            "fresh_object":format!("{fresh_object:?}"),"seed_bytes":seed_len,"lookahead":7,
            "new_public":28,"new_execution":35,"media_quiescence_qualified":false})
        );
    }));
    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    drop(runtime);
    drop(registrar);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
    println!(
        "{}",
        serde_json::json!({"case":"vst3-runtime-latency-restart-cleanup","native_pid":pid,
        "alive_before_drop":alive,"post_drop_wait_status":settled,"exit_query_succeeded":queried!=0,
        "exit_code":exit,"installed":installed,"fresh_object_observed":observed_fresh_object.is_some(),
        "media_quiescence_qualified":false})
    );
    assert_eq!(settled, 0, "original VST3 native owner cleanup");
    assert_ne!(queried, 0);
    assert_eq!(exit, 0);
    assert_eq!(counts.scopes.load(SeqCst), 0);
    assert_eq!(counts.children.load(SeqCst), 0);
    assert_eq!(
        counts.settled.load(SeqCst),
        usize::from(domain_bound),
        "one actual ProcessOwner after real domain attachment, no replacement child"
    );
    assert!(!counts.unavailable.load(SeqCst));
    if let Err(payload) = target {
        std::panic::resume_unwind(payload);
    }
    assert_eq!(alive, 258);
}
