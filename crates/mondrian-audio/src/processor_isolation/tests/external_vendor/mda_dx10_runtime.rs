//! Official MIT SDK mda DX10: finite real-instrument Runtime qualification.
//! Metadata plus audible output does not independently observe native input
//! pointers; the separate owned MNE6 gate must verify that exact SDK boundary.
use super::{owner, Cursors};
use crate::processor_isolation::vst3_discovery::probe_vst3_plugin_registration_with_state;
use crate::processor_isolation::{
    scan_vst3_binary_descriptors, DiscoveredVst3AudioProcessorSpecResolver,
    IsolatedAudioProcessorResolver, Vst3EventBusRole, Vst3MainAudioInput,
};
use crate::{
    AudioContinuityEpoch, AudioMediaResolver, AudioProcessingMode, AudioProcessorCursorState,
    AudioProgramRuntime, AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant,
    AudioStateEntry, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource,
    AudioSourceComponentId, AuthoringList, ExecutionCancellationToken, MusicalTimeReference,
    TimelineTime,
};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
use mondrian_timeline::Sequence;
use serde_json::json;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

const CLASS: &str = "5653544D4441786D6461206478313000";
const BLOCK: usize = 256;
const TOTAL: usize = 16_384;
const ON: i64 = 127;
const OFF: i64 = 8_193;

struct NoMedia(Arc<AtomicUsize>);
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("the authored mda instrument score contains no media".into())
    }
}

#[test]
#[ignore = "normal Product, explicit official SDK mda DLL and outer native Job"]
fn external_mda_dx10_vst3_captured_state_notes_without_media() {
    let mut in_job = 0;
    // SAFETY: the pseudo handle is valid and the writable BOOL is call-local.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "the caller must own the complete native tree");
    let file = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = file("MONDRIAN_VST3_TEST_HELPER");
    let library = file("MONDRIAN_MDA_VST3_PLUGIN");
    let descriptors = scan_vst3_binary_descriptors(&helper, &library).unwrap();
    let matches: Vec<_> = descriptors.iter().filter(|item| item.class_id == CLASS).collect();
    assert_eq!(matches.len(), 1);
    let descriptor = matches[0];
    assert_eq!(descriptor.name, "mda DX10");
    assert_eq!(descriptor.vendor.as_deref(), Some("mda"));
    assert_eq!(descriptor.version.as_deref(), Some("3.8.0.0"));

    for rate in [44_100, 48_000] {
        let render = AudioRenderContract {
            sample_rate: rate,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: BLOCK,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
            ..super::super::test_render_contract()
        };
        let captured =
            probe_vst3_plugin_registration_with_state(&helper, &library, CLASS, render, None, true)
                .expect("actual default-state capture with the strict current Probe");
        let state = captured.captured_state.as_ref().expect("real nonempty plugin state");
        assert!(!state.is_empty() && state.len() <= 64 * 1024);
        let restored = probe_vst3_plugin_registration_with_state(
            &helper,
            &library,
            CLASS,
            render,
            Some(state),
            false,
        )
        .expect("fresh actual native restoration, never a fabricated state payload");
        for probe in [&captured, &restored] {
            assert_eq!(
                probe.registration.main_audio_input,
                Vst3MainAudioInput::Absent
            );
            assert!(probe.registration.auxiliary_inputs.buses.is_empty());
            assert!(probe.input_names.is_empty());
            let events = &probe.registration.event_bus_contract;
            assert!(events.outputs.as_slice().is_empty());
            let rows = events.inputs.as_slice();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].native_index, 0);
            assert_eq!(rows[0].role, Vst3EventBusRole::Main);
            assert_eq!(rows[0].channel_count, 1);
            assert!(rows[0].host_active);
        }
        assert_eq!(
            captured.registration.parameters,
            restored.registration.parameters
        );
        assert_eq!(captured.current_values, restored.current_values);
        assert_eq!(
            captured.registration.event_bus_contract,
            restored.registration.event_bus_contract
        );
        let catalog = Arc::new(
            DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [library.clone()])
                .unwrap(),
        );

        for partition in [&[BLOCK][..], &[31_usize, 225, 127, 129]] {
            let instance = catalog.create_instance(CLASS, render, Some(state.clone())).unwrap();
            assert_eq!(
                instance.opaque_state.as_deref().map(Vec::as_slice),
                Some(state.as_slice())
            );
            let processor_id = instance.id;
            let mut sequence = Sequence::new("official mda DX10 Note-only Runtime");
            sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
            sequence.audio_program.musical_time_reference = Some(
                MusicalTimeReference::new(
                    TimelineTime::new(0, 1).unwrap(),
                    TimelineTime::new(1, 2).unwrap(),
                )
                .unwrap(),
            );
            let track = sequence.audio_tracks[0].id;
            sequence
                .audio_program
                .track_channels
                .get_mut(&track)
                .unwrap()
                .strip
                .pre_fader
                .processors
                .push(instance);
            let time = |sample| TimelineTime::new(sample, i64::from(rate)).unwrap();
            sequence.audio_program.note_lanes.push(AudioNoteLane {
                schema_version: 1,
                id: AudioNoteLaneId::new(),
                target: AudioNoteTarget {
                    track_id: track,
                    processor_id,
                    input_port_key: "vst3.note-input.0".into(),
                },
                notes: AuthoringList::from(vec![AudioNote {
                    id: AudioNoteId::new(),
                    start: time(ON),
                    end: time(OFF),
                    channel: 0,
                    key: 60,
                    on_velocity: 0.75,
                    off_velocity: 0.25,
                }]),
            });
            let media = Arc::new(AtomicUsize::new(0));
            let processors = IsolatedAudioProcessorResolver::new(catalog.clone());
            let mut runtime =
                AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
                    &sequence,
                    &[],
                    &NoMedia(media.clone()),
                    &processors,
                    render,
                    None,
                    AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
                )
                .expect("real zero-input instrument through the sole compiled Runtime");
            let source = AudioOwnerInstallationSource::new();
            let (registration, verifier) = source.register().unwrap();
            runtime.bind_native_owner_installation(verifier).unwrap();
            let claim = source.reserve_install(&registration, None).unwrap();
            let claim = source.begin_commit(claim).unwrap().commit().unwrap();
            let cancel = ExecutionCancellationToken::new();
            runtime.install_native_owner_bindings(&claim, &cancel).unwrap();
            runtime.validate_native_author_source_closed(&cancel).unwrap();
            let object = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
            let pid = object.process_id().get();
            assert_ne!(pid, std::process::id());
            // SAFETY: acquire only query/synchronize rights; adopt this returned
            // nonnull owned handle once and retain it through the original Drop.
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            assert!(!raw.is_null());
            let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
            assert_eq!(
                runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
                object
            );
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
                WAIT_TIMEOUT
            );
            runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .unwrap();
            let mut start = 0;
            let mut blocks = 0;
            let mut energy = 0_f64;
            let mut peak = 0_f32;
            let mut quiet_frames = 0;
            for &size in partition.iter().cycle() {
                let frames = size.min(TOTAL - start);
                let mut pcm = vec![f32::NAN; frames * 2];
                runtime
                    .render_into(
                        AudioRenderRequest {
                            transport_activity: None,
                            start_sample: start as i64,
                            frames,
                        },
                        &mut pcm,
                    )
                    .unwrap();
                runtime.validate_native_author_source_closed(&cancel).unwrap();
                assert!(pcm.iter().all(|value| value.is_finite()));
                for (offset, frame) in pcm.chunks_exact(2).enumerate() {
                    if start + offset < ON as usize {
                        assert!(
                            frame.iter().all(|value| *value == 0.0),
                            "silence before the authored On"
                        );
                        quiet_frames += 1;
                    }
                    for &value in frame {
                        energy += f64::from(value) * f64::from(value);
                        peak = peak.max(value.abs());
                    }
                }
                assert_eq!(
                    runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
                    object
                );
                assert_eq!(
                    source.current_claim().unwrap().unwrap().generation(),
                    claim.generation()
                );
                start += frames;
                blocks += 1;
                if start == TOTAL {
                    break;
                }
            }
            assert_eq!(quiet_frames, ON as usize);
            assert!(energy.is_finite() && energy > 0.0 && peak > 0.0);
            assert_eq!(media.load(Ordering::SeqCst), 0);
            assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
            let mut cursors = Cursors::default();
            runtime.visit_continuity_evidence(&mut cursors).unwrap();
            assert_eq!(cursors.processors.len(), 1);
            assert!(matches!(cursors.processors[0].state,
                AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                if epoch == AudioContinuityEpoch::new(1) && next_input_signal_sample.sample() >= TOTAL as i64));
            drop(runtime);
            // SAFETY: the same acquired handle remains owned and live. This
            // observes Drop, without a new timeout, ACK claim or reopen-by-PID.
            let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
            let mut exit_code = 0;
            let queried =
                unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
            assert_eq!(settled, WAIT_OBJECT_0);
            assert_ne!(queried, 0);
            assert_eq!(exit_code, 0, "observed child exit code must be zero");
            println!(
                "{}",
                json!({"case":"mda-dx10-runtime", "stage":"completed", "version":descriptor.version,
                "class":CLASS,"rate":rate,"partition":partition,"block":BLOCK,"frames":start,"blocks":blocks,
                "state_bytes":state.len(),"main_input":"Absent","event_channels":1,"note_on":ON,"note_off":OFF,
                "pre_on_silent_frames":quiet_frames,"energy":energy,"peak":peak,"media_resolves":0,
                "native_pid":pid,"post_drop_wait_status":settled,"exit_code":exit_code,
                "explicit_unbounded_runtime_grant":true,"algorithm_or_terminal_off_qualified":false,
                "graceful_ack_qualified":false,"realtime_qualified":false})
            );
        }
    }
}

#[test]
#[ignore = "normal Product IPC19, official SDK mda DLL and outer native Job; finite rescan consumer, no native tuple/ID/RT qualification"]
fn external_mda_dx10_vst3_rescan_handoff_preserves_note_score_pcm() {
    use crate::{
        compile_audio_program, AudioCompileRequest, AudioExecutionError,
        NativeAudioAuthorAdmissionError, NativeAudioAuthorHoldReason, NativeAudioAuthorProgram,
        NativeAudioParameterHandoffContinuity, NativeRequestCohort,
    };
    use mondrian_core::{
        AudioOwnerInstallationError, NativeAudioAuthorCallback, NativeAudioAuthorCompletion,
        NativeAudioParameterRescanKind,
    };
    use mondrian_timeline::audio::AudioChannelStripOwner;
    use mondrian_timeline::{
        apply_audio_processor_rack_edit, AudioChannelStripRack, AudioProcessorRackAddress,
        AudioProcessorRackEdit, AudioProcessorRackEditRequest, NativeAudioAuthorUpdate,
        NativeAudioParameterWrite, NativeAudioParameterWriteMode,
    };

    fn cursors(runtime: &AudioProgramRuntime) -> Cursors {
        let mut value = Cursors::default();
        runtime.visit_continuity_evidence(&mut value).unwrap();
        assert_eq!((value.sessions.len(), value.processors.len()), (1, 1));
        value
    }
    fn unchanged(before: &Cursors, after: &Cursors) {
        assert_eq!(after.sessions, before.sessions);
        assert_eq!(after.processors, before.processors);
    }
    fn same_progress(before: &Cursors, after: &Cursors) {
        assert_eq!(
            after.sessions[0].identity.session_id,
            before.sessions[0].identity.session_id
        );
        assert_eq!(
            after.sessions[0].sample_rate,
            before.sessions[0].sample_rate
        );
        assert_eq!(after.sessions[0].state, before.sessions[0].state);
        assert_eq!(
            after.sessions[0].public_output_lookahead_frames,
            before.sessions[0].public_output_lookahead_frames
        );
        assert_eq!(
            after.processors[0].identity.session_id,
            before.processors[0].identity.session_id
        );
        assert_eq!(
            after.processors[0].occurrence,
            before.processors[0].occurrence
        );
        assert_eq!(after.processors[0].state, before.processors[0].state);
    }
    // Immutable oracle copies only; execution remains the original Runtime and
    // its single compiled Note score/ledger, never a second event producer.
    struct Score {
        authored: Vec<AudioNoteLane>,
        compiled: Vec<crate::plan::CompiledNoteLane>,
    }
    fn accept_one_rescan(
        runtime: &mut AudioProgramRuntime,
        sequence: &mut Sequence,
        source: &AudioOwnerInstallationSource,
        score: &Score,
        object: crate::NativeAudioObjectObservation,
        cancel: &ExecutionCancellationToken,
    ) -> (NativeRequestCohort, bool) {
        let root = runtime.resource_root_identity();
        let old = owner(runtime);
        let before = cursors(runtime);
        let old_claim = source.current_claim().unwrap().unwrap();
        let old_verifier = runtime.native_owner_installation_verifier().unwrap();
        // Exactly one actual producer/Poll. Busy input, feedback, malformed or
        // mixed Source remains a real refusal, not a polling/retirement loop.
        let batch = runtime
            .poll_native_observations(cancel)
            .expect("actual MDA author Poll retains typed native failures")
            .expect("the held MDA Source must deliver one complete actual prefix");
        let proposal =
            batch.author.expect("the actual completion must carry coherent MDA author work");
        assert_eq!(proposal.target(), old.editor_target());
        assert_eq!(proposal.sequence(), sequence.id);
        assert!(proposal.terminal());
        assert_eq!(
            proposal.completion(),
            NativeAudioAuthorCompletion::CoherentConfiguration
        );
        assert_eq!(
            (proposal.open_parameter_edits(), proposal.group_depth()),
            (0, 0)
        );
        let Some((NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values }, suffix)) =
            proposal.callbacks().split_first()
        else {
            panic!(
                "expected one actual value-rescan prefix: {:?}",
                proposal.callbacks()
            );
        };
        assert_eq!(*kind, NativeAudioParameterRescanKind::Values);
        assert!(
            suffix
                .iter()
                .all(|callback| matches!(callback, NativeAudioAuthorCallback::DirtyChanged(false))),
            "only the collector's coherent capture Dirty(false) suffix is admitted"
        );
        let a = runtime
            .find_resource_session(root)
            .unwrap()
            .continuing_rescan_cohort(&proposal, root, old_claim.generation())
            .unwrap()
            .expect("authentic retained SomeA, never a serial copied from later live pending");
        assert_eq!(
            a.request,
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 }
        );
        assert!(a.serial != 0 && a.serial != u32::MAX);
        let state = proposal.state().expect("complete same-owner MDA component/controller state");
        assert!(!state.is_empty() && state.len() <= crate::NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES);
        unchanged(&before, &cursors(runtime));
        assert_eq!(owner(runtime), old);
        assert_eq!(runtime.observe_native_owner_object(old).unwrap(), object);
        old_verifier.validate(&source.current_claim().unwrap().unwrap()).unwrap();
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            old_claim.generation()
        );
        let pending = runtime.validate_native_author_source_closed(cancel).unwrap_err();
        assert!(
            matches!(
                pending,
                NativeAudioAuthorAdmissionError::Pending {
                    reason: NativeAudioAuthorHoldReason::OutstandingDelivery,
                    ..
                }
            ),
            "actual={pending:?}"
        );

        let track = sequence.audio_tracks[0].id;
        let current = &sequence
            .audio_program
            .track_channels
            .get(&track)
            .unwrap()
            .strip
            .pre_fader
            .processors[0];
        assert_eq!(current.id, proposal.target().instance);
        assert_eq!(
            values.len(),
            current.parameters.len(),
            "complete actual writable parameter census"
        );
        let prior_parameters = current.parameters.clone();
        let mut ids = std::collections::BTreeSet::new();
        let mut writes = Vec::with_capacity(values.len());
        for (id, bits) in values {
            assert!(
                ids.insert(id),
                "actual rescan must not duplicate a parameter identity"
            );
            let parameter = current.parameters.get(id).expect("actual probed canonical schema");
            let value = f64::from_bits(*bits);
            assert!(value.is_finite());
            // A full native census is not permission to adopt an absent lane.
            // Match the App: only an existing unkeyed reference is refreshed.
            if parameter.automation.as_ref().is_some_and(|curve| curve.keyframes.is_empty()) {
                writes.push(
                    NativeAudioParameterWrite {
                        schema: Box::new(parameter.schema.clone()),
                        value,
                        mode: NativeAudioParameterWriteMode::Unkeyed,
                    }
                    .into(),
                );
            }
        }
        let request = AudioProcessorRackEditRequest {
            address: AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Track { track_id: track },
                rack: AudioChannelStripRack::PreFader,
            },
            edit: AudioProcessorRackEdit::SetNativeAuthorState {
                processor_id: current.id,
                expected_definition: current.definition.clone(),
                update: NativeAudioAuthorUpdate {
                    parameters: writes,
                    opaque_state: Some(state.clone()),
                },
            },
        };
        let transaction = apply_audio_processor_rack_edit(sequence, &request)
            .expect("real atomic canonical MDA author transaction, including unchanged values");
        let accepted = &sequence
            .audio_program
            .track_channels
            .get(&track)
            .unwrap()
            .strip
            .pre_fader
            .processors[0];
        assert_eq!(accepted.opaque_state.as_ref(), Some(state));
        assert_eq!(accepted.parameters.len(), prior_parameters.len());
        for (id, bits) in values {
            let prior = &prior_parameters[id];
            let actual = &accepted.parameters[id];
            assert_eq!(actual.schema, prior.schema);
            assert_eq!(actual.ui, prior.ui);
            if prior.automation.as_ref().is_some_and(|curve| curve.keyframes.is_empty()) {
                let curve =
                    actual.automation.as_ref().expect("existing unkeyed reference retained");
                assert!(curve.keyframes.is_empty());
                assert_eq!(curve.default_value.to_bits(), *bits);
            } else {
                assert_eq!(
                    actual.automation, prior.automation,
                    "absent and keyed authority stays unchanged"
                );
            }
        }
        assert_eq!(
            sequence.audio_program.note_lanes.as_slice(),
            score.authored.as_slice()
        );
        let program = compile_audio_program(
            sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
        )
        .unwrap();
        assert_eq!(program.note_lanes, score.compiled);
        let programs =
            [NativeAudioAuthorProgram { sequence: sequence.id, program: Arc::new(program) }];
        let (registration, destination) = source.register().unwrap();
        let reservation = source.reserve_install(&registration, Some(&old_claim)).unwrap();
        let mut authorization = source.authorize_runtime_installation(reservation).unwrap();
        let mut prepared = runtime
            .prepare_native_author_parameter_handoff(&programs, &proposal, destination, cancel)
            .expect("authentic exact4 Active direct-root continuing handoff");
        unchanged(&before, &cursors(runtime));
        old_verifier.validate(&source.current_claim().unwrap().unwrap()).unwrap();
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            old_claim.generation()
        );
        assert_eq!(owner(runtime), old);
        assert_eq!(runtime.observe_native_owner_object(old).unwrap(), object);
        let installed = runtime
            .install_native_author_parameter_handoff(&mut prepared, &mut authorization, cancel)
            .expect("one real Source-authorized MDA parameter-plan publication");
        assert_eq!(
            installed.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        let next = owner(runtime);
        assert_eq!(next.editor_target(), installed.target);
        assert_ne!(next.binding(), old.binding());
        assert_eq!(
            installed.target.local_session,
            old.editor_target().local_session
        );
        assert_eq!(installed.target.continuity_epoch, Some(1));
        assert_eq!(
            installed.target.installed_generation,
            installed.claim.generation()
        );
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            installed.claim.generation()
        );
        let verifier = runtime.native_owner_installation_verifier().unwrap();
        verifier.validate(&installed.claim).unwrap();
        assert!(matches!(
            authorization.begin_commit(&verifier),
            Err(AudioOwnerInstallationError::ConsumedAuthorization)
        ));
        same_progress(&before, &cursors(runtime));
        assert_ne!(
            runtime.resource_root_identity().prepared_plan_id,
            root.prepared_plan_id
        );
        assert_eq!(runtime.resource_root_identity().session_id, root.session_id);
        assert_eq!(runtime.observe_native_owner_object(next).unwrap(), object);
        let installed_cursors = cursors(runtime);
        drop(prepared);
        let acknowledged = runtime
            .acknowledge_native_author_parameter_handoff(&proposal, cancel)
            .expect("exact old SomeA Ack only after accepted canonical author and Source install");
        assert_eq!(
            acknowledged.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        assert_eq!(acknowledged.target, installed.target);
        assert_eq!(
            acknowledged.claim.generation(),
            installed.claim.generation()
        );
        verifier.validate(&acknowledged.claim).unwrap();
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            installed.claim.generation()
        );
        unchanged(&installed_cursors, &cursors(runtime));
        assert_eq!(owner(runtime), next);
        assert_eq!(runtime.observe_native_owner_object(next).unwrap(), object);
        runtime
            .validate_native_author_source_closed(cancel)
            .expect("actual MDA Source/outstanding closed after exact acknowledgement");
        (a, transaction.changed)
    }

    let mut in_job = 0;
    // SAFETY: valid current-process pseudo handle and call-local writable BOOL.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(
        in_job, 0,
        "the existing caller must own the complete native tree"
    );
    let file = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current MDA material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = file("MONDRIAN_VST3_TEST_HELPER");
    let library = file("MONDRIAN_MDA_VST3_PLUGIN");
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        ..super::super::test_render_contract()
    };
    let captured =
        probe_vst3_plugin_registration_with_state(&helper, &library, CLASS, render, None, true)
            .expect("actual official MDA default-state capture");
    assert_eq!(
        captured.registration.main_audio_input,
        Vst3MainAudioInput::Absent
    );
    assert!(captured.registration.auxiliary_inputs.buses.is_empty());
    assert!(captured.input_names.is_empty());
    let rows = captured.registration.event_bus_contract.inputs.as_slice();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].native_index,
            rows[0].role,
            rows[0].channel_count,
            rows[0].host_active
        ),
        (0, Vst3EventBusRole::Main, 1, true)
    );
    assert!(captured.registration.event_bus_contract.outputs.is_empty());
    let state = captured.captured_state.as_ref().expect("real nonempty MDA state");
    assert!(!state.is_empty() && state.len() <= 64 * 1024);
    let catalog =
        Arc::new(DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [library]).unwrap());
    let descriptors = catalog.descriptors();
    let actual: Vec<_> =
        descriptors.iter().filter(|descriptor| descriptor.class_id == CLASS).collect();
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].name, "mda DX10");
    assert_eq!(actual[0].vendor.as_deref(), Some("mda"));
    assert_eq!(actual[0].version.as_deref(), Some("3.8.0.0"));
    let instance = catalog
        .create_instance(CLASS, render, Some(state.clone()))
        .expect("actual fresh restore preserves real state and all original parameter lanes");
    assert_eq!(
        instance.opaque_state.as_deref().map(Vec::as_slice),
        Some(state.as_slice())
    );
    let processor_id = instance.id;
    let mut sequence =
        Sequence::new("official MDA DX10 canonical parameter-rescan Note continuation");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.audio_program.musical_time_reference = Some(
        MusicalTimeReference::new(TimelineTime::ZERO, TimelineTime::new(1, 2).unwrap()).unwrap(),
    );
    let track = sequence.audio_tracks[0].id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .push(instance);
    let time = |sample| TimelineTime::new(sample, 48_000).unwrap();
    sequence.audio_program.note_lanes.push(AudioNoteLane {
        schema_version: 1,
        id: AudioNoteLaneId::new(),
        target: AudioNoteTarget {
            track_id: track,
            processor_id,
            input_port_key: "vst3.note-input.0".into(),
        },
        notes: AuthoringList::from(vec![AudioNote {
            id: AudioNoteId::new(),
            start: time(ON),
            end: time(OFF),
            channel: 0,
            key: 60,
            on_velocity: 0.75,
            off_velocity: 0.25,
        }]),
    });
    let media = Arc::new(AtomicUsize::new(0));
    let processors = IsolatedAudioProcessorResolver::new(catalog);
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &NoMedia(media.clone()),
        &processors,
        render,
        None,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
    )
    .expect("real zero-input Note Runtime with the original explicit unbounded root grant");
    assert_eq!(media.load(Ordering::SeqCst), 0);
    let score = Score {
        authored: sequence.audio_program.note_lanes.to_vec(),
        compiled: runtime
            .find_resource_session(runtime.resource_root_identity())
            .unwrap()
            .resource_plan()
            .program()
            .note_lanes
            .clone(),
    };
    assert_eq!(score.compiled.len(), 1);
    assert_eq!(
        score.compiled[0].notes.as_slice(),
        score.authored[0].notes.as_slice()
    );
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source.reserve_install(&registration, None).unwrap();
    let claim = source.begin_commit(claim).unwrap().commit().unwrap();
    let cancel = ExecutionCancellationToken::new();
    runtime.install_native_owner_bindings(&claim, &cancel).unwrap();
    runtime.validate_native_author_source_closed(&cancel).unwrap();
    let object = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: this exact installed Runtime authenticated the native owner.
    // Query/synchronize rights only; its original supervisor remains sole owner.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        WAIT_TIMEOUT
    );
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();
    let mut handoffs = 0;
    let mut unchanged_transactions = 0;
    let mut held_between_on_off = 0;
    let mut previous = None;
    let mut energy = 0.0_f64;
    let mut resumed_pressed_energy = 0.0_f64;
    let mut quiet_frames = 0;
    // Exactly 64 admitted requests, each at most one Source service and one
    // execution after a proved non-consuming refusal. No native failure retry.
    for start in (0..TOTAL).step_by(BLOCK) {
        let request = AudioRenderRequest {
            transport_activity: None,
            start_sample: start as i64,
            frames: BLOCK,
        };
        let before = cursors(&runtime);
        let old = owner(&runtime);
        let old_claim = source.current_claim().unwrap().unwrap();
        let old_verifier = runtime.native_owner_installation_verifier().unwrap();
        let mut pcm = vec![93.0; BLOCK * 2];
        let resumed_after_audible_prefix = match runtime.render_into(request, &mut pcm) {
            Ok(()) => false,
            Err(error) => {
                let AudioExecutionError::NativeAuthorPending(held) = error else {
                    panic!("only actual Source-first HOLD permits this one service: {error:?}");
                };
                assert_eq!(held.target, old.editor_target());
                assert_eq!(held.reason, NativeAudioAuthorHoldReason::PendingCallbacks);
                assert_eq!(pcm, vec![93.0; BLOCK * 2]);
                unchanged(&before, &cursors(&runtime));
                assert_eq!(owner(&runtime), old);
                assert_eq!(runtime.observe_native_owner_object(old).unwrap(), object);
                old_verifier.validate(&source.current_claim().unwrap().unwrap()).unwrap();
                assert_eq!(
                    source.current_claim().unwrap().unwrap().generation(),
                    old_claim.generation()
                );
                assert_eq!(media.load(Ordering::SeqCst), 0);
                let (cohort, changed) = accept_one_rescan(
                    &mut runtime,
                    &mut sequence,
                    &source,
                    &score,
                    object,
                    &cancel,
                );
                if let Some(serial) = previous {
                    assert!(cohort.serial > serial);
                }
                previous = Some(cohort.serial);
                handoffs += 1;
                unchanged_transactions += usize::from(!changed);
                // An authored On does not imply an already audible prefix:
                // the independent SDK offset test observes a silent first
                // window for On127. Service every hold, but qualify continuity
                // only when this handoff has actual audible PCM on both sides.
                let audible_prefix = start as i64 > ON && (start as i64) < OFF && energy > 0.0;
                if audible_prefix {
                    held_between_on_off += 1;
                }
                // The exact same unconsumed request, not seek/re-entry or a
                // smaller substitute. A second refusal/native error is final.
                runtime.render_into(request, &mut pcm).expect(
                    "the same PCM request must execute once after its authenticated handoff",
                );
                audible_prefix
            }
        };
        assert!(pcm.iter().all(|sample| sample.is_finite()));
        for (offset, frame) in pcm.chunks_exact(2).enumerate() {
            let sample = start + offset;
            if (sample as i64) < ON {
                assert!(
                    frame.iter().all(|value| *value == 0.0),
                    "silence before the authored On"
                );
                quiet_frames += 1;
            }
            for &value in frame {
                let value_energy = f64::from(value) * f64::from(value);
                energy += value_energy;
                if resumed_after_audible_prefix && sample as i64 > ON && (sample as i64) < OFF {
                    resumed_pressed_energy += value_energy;
                }
            }
        }
        assert_eq!(media.load(Ordering::SeqCst), 0);
        assert_eq!(
            runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
            object
        );
        let now = cursors(&runtime);
        assert_eq!(
            now.sessions[0].identity.session_id,
            before.sessions[0].identity.session_id
        );
        assert_eq!(
            now.processors[0].occurrence,
            before.processors[0].occurrence
        );
        assert!(
            matches!(now.processors[0].state, AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
            if epoch == AudioContinuityEpoch::new(1) && next_input_signal_sample.sample() == (start + BLOCK) as i64)
        );
    }
    assert_eq!(quiet_frames, ON as usize);
    assert!(energy.is_finite() && energy > 0.0);
    assert!(
        held_between_on_off > 0
            && resumed_pressed_energy.is_finite()
            && resumed_pressed_energy > 0.0
    );
    let final_cursors = cursors(&runtime);
    // One fixed final natural prefix, with no further PCM to create another
    // preset callback. Any missing/mixed/refused prefix remains a real failure.
    let (final_cohort, changed) = accept_one_rescan(
        &mut runtime,
        &mut sequence,
        &source,
        &score,
        object,
        &cancel,
    );
    assert!(final_cohort.serial > previous.expect("at least one actual Source handoff"));
    handoffs += 1;
    unchanged_transactions += usize::from(!changed);
    same_progress(&final_cursors, &cursors(&runtime));
    assert_eq!(
        sequence.audio_program.note_lanes.as_slice(),
        score.authored.as_slice()
    );
    assert_eq!(media.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
    runtime.validate_native_author_source_closed(&cancel).unwrap();
    drop(runtime);
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2_000) },
        WAIT_OBJECT_0
    );
    let mut exit = u32::MAX;
    assert_ne!(
        unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
        0
    );
    assert_eq!(exit, 0);
    println!(
        "{}",
        json!({"case":"mda-dx10-rescan-runtime", "rate":48000,
        "frames":TOTAL,"block":BLOCK,"public_requests":TOTAL/BLOCK,
        "note_on":ON,"note_off":OFF,"handoffs":handoffs,
        "unchanged_canonical_transactions":unchanged_transactions,
        "handoffs_between_on_and_off":held_between_on_off,
        "energy":energy,"resumed_pressed_energy":resumed_pressed_energy,
        "native_pid":pid,"media_resolves":0,"explicit_unbounded_root_grant":true,
        "native_tuple_or_id_audit":false,"dsp_voice_state_audited":false,
        "terminal_off_acoustics_qualified":false,"graceful_100ms_qualified":false,"realtime_qualified":false})
    );
}

/// Two fresh SDK owners differ only in NoteOn offset. This intentionally has no
/// Runtime, author service, graph, media or voice-state observation after On.
#[test]
#[ignore = "normal Product capture, explicit official SDK mda DLL and outer native Job"]
fn external_mda_dx10_vst3_sdk_note_on_offsets() {
    use std::collections::BTreeMap;
    use vst3_host::audio::{
        AudioBusRole, NativeEventBusRole, ProcessMusicalReference, ProcessTimelineContext,
    };
    use vst3_host::controller_parameters::ControllerParameterQueryFrame;
    use vst3_host::initial_parameter_input::{InitialParameterStorage, InitialParameterValue};
    use vst3_host::plugin::ProcessMode;
    use vst3_host::{PluginEvent, PluginEventData, Vst3Host};

    let mut in_job = 0;
    // SAFETY: the process pseudo handle and call-local BOOL are valid here.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "the caller must own the complete native tree");
    let file = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = file("MONDRIAN_VST3_TEST_HELPER");
    let library = file("MONDRIAN_MDA_VST3_PLUGIN");
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        ..super::super::test_render_contract()
    };
    // One original Product default capture, before either SDK owner receives On.
    // Neither direct instance saves state, drains callbacks or flushes parameters.
    let captured =
        probe_vst3_plugin_registration_with_state(&helper, &library, CLASS, render, None, true)
            .expect("actual default-state capture with the strict current Probe");
    let state = captured.captured_state.as_ref().expect("real nonempty default state");
    assert!(!state.is_empty() && state.len() <= 64 * 1024);
    assert_eq!(
        captured.registration.main_audio_input,
        Vst3MainAudioInput::Absent
    );
    assert!(captured.registration.auxiliary_inputs.buses.is_empty());
    assert!(captured.input_names.is_empty());
    let admitted = &captured.registration.parameters;
    assert_eq!(admitted.len(), 19);
    assert_eq!(captured.current_values.len(), admitted.len());

    // The resolver builds payload.parameter_ids in the authored BTreeMap order,
    // not native numeric ID or controller enumeration order. Reproduce its same
    // scalar-to-normalized projection for every writable lane, including preset.
    let mut ordered = BTreeMap::new();
    for (descriptor, &(id, normalized)) in admitted.iter().zip(&captured.current_values) {
        assert_eq!(descriptor.id, id);
        if !descriptor.read_only {
            let scalar = descriptor.author_scalar_from_normalized(normalized).unwrap();
            let normalized = descriptor.normalized_from_author_scalar(scalar).unwrap();
            assert!(ordered.insert(descriptor.parameter_id().unwrap(), (id, normalized)).is_none());
        }
    }
    let parameter_queue: Vec<_> = ordered.into_values().collect();
    assert_eq!(parameter_queue.len(), 19);
    let parameter_order: Vec<_> = parameter_queue
        .iter()
        .map(|&(id, value)| json!({"native_id":id,"normalized_bits":value.to_bits()}))
        .collect();
    let reference = ProcessMusicalReference::new(0, 1, 1, 2).unwrap();
    let _native_main = vst3_host::window::WindowsEditorMainThread::initialize().unwrap();
    let mut observations = Vec::with_capacity(2);
    let mut checks = Vec::with_capacity(2);
    for on_offset in [0_usize, 127] {
        let mut host = Vst3Host::builder()
            .plugin_interfaces(crate::processor_isolation::vst3_worker::product_plugin_interfaces())
            .sample_rate(48_000.0)
            .block_size(BLOCK)
            .input_channels(2)
            .output_channels(2)
            .build()
            .unwrap();
        let mut plugin = host.load_plugin_class(&library, CLASS).unwrap();
        assert_eq!(plugin.info().uid, CLASS);
        assert_eq!(plugin.info().name, "mda DX10");
        assert_eq!(plugin.info().vendor, "mda");
        plugin.load_state(state).expect("restore the same actual captured default");
        plugin.set_process_mode(ProcessMode::Offline).unwrap();
        let audio = plugin.native_audio_bus_info().unwrap();
        assert!(audio.inputs.is_empty());
        assert_eq!(audio.outputs.len(), 1);
        assert_eq!(audio.outputs[0].role, AudioBusRole::Main);
        assert_eq!(audio.outputs[0].channel_count, 2);
        assert!(audio.outputs[0].default_active);
        let events = plugin.native_event_bus_info().unwrap();
        assert_eq!(events.inputs.len(), 1);
        assert!(events.outputs.is_empty());
        assert_eq!(events.inputs[0].role, NativeEventBusRole::Main);
        assert_eq!(events.inputs[0].channel_count, 1);
        assert!(events.inputs[0].host_active);
        let mut buffers = plugin.create_bus_audio_buffers(BLOCK).unwrap();
        assert!(buffers.inputs.is_empty());
        assert_eq!(buffers.outputs.len(), 1);
        assert!(buffers.outputs[0].active);
        assert_eq!(buffers.outputs[0].channels.len(), 2);
        plugin.start_processing().unwrap();

        // Match the Worker's initial-only capability. N current query values do
        // not imply N initial COM points: inspect the actual input owner too.
        let cohort = plugin.pending_restart_cohort().expect("local raw owner");
        assert!(!cohort.exhausted());
        assert!(matches!(cohort.flags().bits(), 0 | 4 | 20));
        let source = plugin.editor_event_source_status().unwrap();
        assert_eq!(source.failure(), None);
        assert_eq!(source.open_parameter_edits(), 0);
        assert_eq!(source.group_depth(), 0);
        let mut frame = ControllerParameterQueryFrame::default();
        let mut initial_values = Vec::with_capacity(admitted.len());
        let count = plugin
            .visit_controller_parameters_bounded(
                admitted.len(),
                &mut frame,
                &mut |index, actual| {
                    let expected = &admitted[index];
                    assert_eq!(actual.id(), expected.id);
                    assert!(actual.title_matches(&expected.name));
                    assert_eq!(
                        actual.default_normalized().to_bits(),
                        expected.default_normalized.to_bits()
                    );
                    assert_eq!(actual.step_count(), expected.step_count);
                    assert_eq!(actual.flags(), expected.flags);
                    assert!(actual.normalized().is_finite());
                    assert!((0.0..=1.0).contains(&actual.normalized()));
                    let &(id, normalized) = &captured.current_values[index];
                    assert_eq!(actual.id(), id);
                    assert_eq!(actual.normalized().to_bits(), normalized.to_bits());
                    if !expected.read_only {
                        initial_values
                            .push(InitialParameterValue { id, normalized: actual.normalized() });
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(count, admitted.len());
        assert_eq!(plugin.pending_restart_cohort(), Some(cohort));
        assert_eq!(plugin.editor_event_source_status().unwrap(), source);
        let pending_input_before = plugin.pending_processor_parameter_input().unwrap();
        plugin
            .handoff_initial_parameter_input(
                cohort,
                source,
                &initial_values,
                InitialParameterStorage::for_parameter_count(initial_values.len()).unwrap(),
            )
            .expect("original initial handoff, never manual retirement or a hidden DSP flush");
        let after = plugin.editor_event_source_status().unwrap();
        let after_cohort = plugin.pending_restart_cohort().unwrap();
        let pending_input_after = plugin.pending_processor_parameter_input().unwrap();
        assert!(after.closed_empty());
        assert_eq!(after.pending_dsp_feedback_values(), 0);
        assert_eq!(after_cohort.serial(), cohort.serial());
        assert_eq!(after_cohort.flags().bits(), 0);
        assert!(!pending_input_after,
            "initial input is a real blocker; do not clear or mix it with authored points: {source:?} -> {after:?}");
        let initial = json!({
            "cohort_serial":cohort.serial(),"cohort_flags":cohort.flags().bits(),
            "source_revision":source.revision(),"pending_events":source.pending_events(),
            "pending_feedback":source.pending_dsp_feedback_values(),
            "pending_input_before":pending_input_before,"pending_input_after":pending_input_after,
            "remaining_events":after.pending_events(),
            "remaining_feedback":after.pending_dsp_feedback_values(),
            "remaining_flags":after_cohort.flags().bits()
        });
        let on = [PluginEvent {
            bus_index: 0,
            sample_offset: on_offset as i32,
            // Some derives the actual PPQ from the immutable start + offset.
            ppq_position: 0.0,
            flags: 0,
            data: PluginEventData::NoteOn {
                channel: 0,
                pitch: 60,
                tuning: 0.0,
                velocity: 0.75,
                length: 0,
                note_id: 0,
            },
        }];
        let vst3_host::plugin::PluginEditorProcessingSplit { editor, mut processor } =
            plugin.split_editor_processing().unwrap();
        let pcm = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let mut pcm = [Vec::with_capacity(2 * BLOCK), Vec::with_capacity(2 * BLOCK)];
                    // Fixed second request diagnoses postponement; it never
                    // retries a failed call or repeats On. No main-role service,
                    // getState, controller query or stop/resume occurs between.
                    for start in [0_i64, BLOCK as i64] {
                        processor
                            .set_process_musical_timeline_context(
                                ProcessTimelineContext {
                                    sample_position: start,
                                    sample_rate: 48_000,
                                },
                                reference,
                            )
                            .unwrap();
                        for &(id, normalized) in &parameter_queue {
                            processor.queue_parameter_at(id, normalized, 0).unwrap();
                        }
                        buffers.clear();
                        processor
                            .process_bus_audio_with_note_events(
                                &mut buffers,
                                if start == 0 { &on } else { &[] },
                            )
                            .expect("one checked SDK request; no native failure retry");
                        assert!(buffers.inputs.is_empty());
                        assert_eq!(buffers.outputs.len(), 1);
                        assert_eq!(buffers.outputs[0].channels.len(), 2);
                        for (channel, output) in buffers.outputs[0].channels.iter().enumerate() {
                            assert_eq!(output.len(), BLOCK);
                            pcm[channel].extend_from_slice(output);
                        }
                    }
                    pcm
                })
                .join()
                .expect("joined same-owner SDK processing role")
        });
        drop(processor);
        drop(editor);
        // All post-On observations below are copied PCM only. The final ordinary
        // Plugin Drop happens after both fixed requests; it is not an intervening
        // stop/resume, state capture or a callback service to obtain sound.
        let mut windows = Vec::with_capacity(2);
        let mut finite = true;
        for start in [0, BLOCK] {
            let values = pcm.iter().flat_map(|channel| &channel[start..start + BLOCK]);
            let window_finite = values.clone().all(|value| value.is_finite());
            let energy = values.clone().map(|&v| f64::from(v).powi(2)).sum::<f64>();
            let peak = values.clone().map(|&v| f64::from(v.abs())).fold(0.0_f64, f64::max);
            let nonzero = values.filter(|&&value| value != 0.0).count();
            let first_nonzero_sample = (start..start + BLOCK)
                .find(|&sample| pcm.iter().any(|channel| channel[sample] != 0.0));
            finite &= window_finite;
            windows.push(json!({"start":start,"frames":BLOCK,"finite":window_finite,
                "energy":energy,"peak":peak,"nonzero":nonzero,
                "first_nonzero_sample":first_nonzero_sample}));
        }
        let pre_on_nonzero = pcm
            .iter()
            .flat_map(|channel| &channel[..on_offset])
            .filter(|&&value| value != 0.0)
            .count();
        let post_on_energy = pcm
            .iter()
            .flat_map(|channel| &channel[on_offset..])
            .map(|&value| f64::from(value).powi(2))
            .sum::<f64>();
        checks.push((on_offset, finite, pre_on_nonzero, post_on_energy));
        observations.push(json!({"on_offset":on_offset,"sdk_requests":2,
            "initial":initial,"pre_on_nonzero":pre_on_nonzero,
            "post_on_energy":post_on_energy,"windows":windows}));
        drop(plugin);
        drop(host);
    }
    // Preserve BOTH initial observations even if either produces only silence.
    // Counts below are SDK requests, not evidence of internal native FFI counts.
    println!(
        "{}",
        json!({"case":"mda-dx10-sdk-note-offset","rate":48000,
        "block":BLOCK,"sdk_requests":4,"parameter_count":parameter_queue.len(),
        "parameter_order":parameter_order,"observations":observations,
        "worker_ipc_qualified":false,"runtime_handoff_qualified":false,
        "native_ffi_count_qualified":false,"dsp_voice_state_audited":false,
        "realtime_qualified":false})
    );
    for (offset, finite, pre_on_nonzero, post_on_energy) in checks {
        assert!(finite, "nonfinite SDK PCM for NoteOn offset {offset}");
        assert_eq!(pre_on_nonzero, 0, "PCM before NoteOn offset {offset}");
        assert!(
            post_on_energy.is_finite() && post_on_energy > 0.0,
            "no audible SDK PCM in two fixed windows after NoteOn offset {offset}"
        );
    }
}
