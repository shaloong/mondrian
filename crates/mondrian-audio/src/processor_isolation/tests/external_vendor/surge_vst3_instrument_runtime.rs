//! Real Surge XT VST3: finite captured-state/Note/Main Runtime consumer.
//! Uses the complete native parameter census and the original Source/Ack path.
//! Fixed musical reference only; no algorithm, release, auxiliary sound or RT oracle.

use super::surge_instrument_runtime::accept_actual_rescans;
use super::{owner, Cursors, Facts};
use crate::processor_isolation::vst3_discovery::probe_vst3_plugin_registration_with_state;
use crate::processor_isolation::{
    DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver, Vst3EventBusRole,
    Vst3MainAudioInput,
};
use crate::{
    AudioCompileRequest, AudioContinuityEpoch, AudioExecutionError, AudioMediaResolver,
    AudioNoteCompletionPolicy, AudioProcessingMode, AudioProcessorCursorState, AudioProgramRuntime,
    AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant, AudioStateEntry,
    NativeAudioAuthorHoldReason, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource,
    AudioSourceComponentId, AuthoringList, ExecutionCancellationToken, MusicalTimeReference,
    TimelineTime,
};
use mondrian_timeline::audio::{
    AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorDefinitionRef, AudioProcessorInstance,
};
use mondrian_timeline::{
    AudioProcessorOutputPort, AudioRoute, AudioRouteDestination, AudioRouteSource, Sequence,
};
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

const CLASS: &str = "ABCDEF019182FAEB566D624153675854";
const PARAMETERS: usize = 2_855;
const AUTOMATABLE: usize = 774;
const BLOCK: usize = 256;
const TOTAL: usize = 16_384;
const ON: i64 = 127;
const OFF: i64 = 8_193;
const OUTPUT_SENTINEL: f32 = f32::from_bits(0x7fc0_5a31);
const GRANT: AudioRuntimeResourceGrant =
    AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 16 * 1024 * 1024);

struct NoMedia(Arc<AtomicUsize>);
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("instrument-only Runtime must not resolve media".into())
    }
}

fn cursors(runtime: &AudioProgramRuntime) -> Cursors {
    let mut value = Cursors::default();
    runtime.visit_continuity_evidence(&mut value).unwrap();
    assert_eq!((value.sessions.len(), value.processors.len()), (1, 1));
    value
}

fn same_progress(before: &Cursors, after: &Cursors) {
    assert_eq!(
        before.sessions[0].identity.session_id,
        after.sessions[0].identity.session_id
    );
    assert_eq!(
        before.sessions[0].sample_rate,
        after.sessions[0].sample_rate
    );
    assert_eq!(before.sessions[0].state, after.sessions[0].state);
    assert_eq!(
        before.sessions[0].public_output_lookahead_frames,
        after.sessions[0].public_output_lookahead_frames
    );
    assert_eq!(
        before.processors[0].identity.session_id,
        after.processors[0].identity.session_id
    );
    assert_eq!(
        before.processors[0].occurrence,
        after.processors[0].occurrence
    );
    assert_eq!(before.processors[0].state, after.processors[0].state);
}

// The existing format-neutral transaction owns author acceptance, installation
// and exact Ack. This consumer additionally validates the Source's current claim
// and the finite aggregate grant after every actual publication.
fn service_author(
    runtime: &mut AudioProgramRuntime,
    sequence: &mut Sequence,
    source: &AudioOwnerInstallationSource,
    object: crate::NativeAudioObjectObservation,
    reference: Option<MusicalTimeReference>,
) -> (usize, usize) {
    let old_claim = source.current_claim().unwrap().unwrap();
    let result = accept_actual_rescans(
        runtime,
        sequence,
        source,
        object,
        reference,
        4_096,
        128 * 1024,
    );
    let claim = source.current_claim().unwrap().unwrap();
    let verifier = runtime.native_owner_installation_verifier().unwrap();
    verifier.validate(&claim).expect("current real Source claim");
    if result.0 == 0 {
        assert_eq!(claim.generation(), old_claim.generation());
    } else {
        assert!(claim.generation() > old_claim.generation());
        assert!(
            verifier.validate(&old_claim).is_err(),
            "old Source claim must be stale"
        );
    }
    let footprint = runtime.resource_footprint();
    assert_eq!(footprint.runtime_occurrences, 1);
    assert!(footprint.fixed_resident_bytes <= GRANT.max_fixed_resident_bytes);
    assert!(footprint.prepared_logical_bytes <= GRANT.max_prepared_logical_bytes);
    assert_eq!(footprint.media_window_bytes, 0);
    assert_eq!(
        sequence.audio_program.track_channels[&sequence.audio_tracks[0].id]
            .strip
            .pre_fader
            .processors[0]
            .parameters
            .len(),
        PARAMETERS
    );
    result
}

#[test]
#[ignore = "requires outer native Job, current normal MONDRIAN_VST3_TEST_HELPER and real MONDRIAN_SURGE_XT_VST3_DLL"]
fn external_surge_xt_vst3_captured_state_notes_main_runtime_without_media() {
    let mut in_job = 0;
    // SAFETY: valid pseudo handle and call-local writable BOOL. Membership is
    // a guard; Root's caller still owns/qualifies the complete outer Job tree.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "outer owner Job required before native creation");
    let file = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = file("MONDRIAN_VST3_TEST_HELPER");
    let library = std::fs::canonicalize(file("MONDRIAN_SURGE_XT_VST3_DLL")).unwrap();
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 64 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4_096,
        compensation_delay_scratch_budget_bytes: 64 * 1024 * 1024,
    };
    // The existing VST3 adapter requires a reference for Note/PPQ projection.
    // A fixed 120 BPM reference is not a tempo-response or playing-state test.
    let reference = Some(
        MusicalTimeReference::new(TimelineTime::ZERO, TimelineTime::new(1, 2).unwrap()).unwrap(),
    );
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [library.clone()])
            .expect("current Product typed descriptor discovery"),
    );
    let descriptors = catalog.descriptors();
    let matches: Vec<_> = descriptors.iter().filter(|p| p.class_id == CLASS).collect();
    assert_eq!(matches.len(), 1);
    let descriptor = (**matches[0]).clone();
    assert_eq!(descriptor.name, "Surge XT");
    assert_eq!(descriptor.vendor.as_deref(), Some("Surge Synth Team"));
    assert_eq!(descriptor.version.as_deref(), Some("1.3.4"));
    println!(
        "{}",
        json!({"case":"surge-xt-vst3-main-runtime", "stage":"descriptor",
        "class":CLASS,"plugin_name":descriptor.name,"plugin_version":descriptor.version,
        "rate":render.sample_rate,"block":BLOCK,"mode":"Offline"})
    );

    // These are fresh normal Product operations using the current typed wire.
    // The prior stopped SDK witness is neither an input nor a Product oracle.
    let captured =
        probe_vst3_plugin_registration_with_state(&helper, &library, CLASS, render, None, true)
            .expect("actual Product default state capture within original packet/state bounds");
    let state = captured.captured_state.as_ref().expect("real Surge default state");
    assert!(!state.is_empty() && state.len() <= 128 * 1024);
    let restored = probe_vst3_plugin_registration_with_state(
        &helper,
        &library,
        CLASS,
        render,
        Some(state),
        false,
    )
    .expect("fresh actual Product restore, never a fabricated capture");
    assert!(restored.captured_state.is_none());
    for probe in [&captured, &restored] {
        let registration = &probe.registration;
        assert_eq!(registration.class_id, CLASS);
        assert_eq!(registration.vendor.as_deref(), Some("Surge Synth Team"));
        assert_eq!(registration.main_audio_input, Vst3MainAudioInput::Absent);
        assert_eq!(
            registration.execution_contract.algorithmic_latency_frames(),
            0
        );
        assert_eq!(
            (
                registration.audio_output_count,
                registration.main_audio_output_count
            ),
            (3, 3)
        );
        assert_eq!(registration.auxiliary_inputs.buses.len(), 1);
        let input = &registration.auxiliary_inputs.buses[0];
        assert_eq!(input.bus_key, "vst3.audio-input.0");
        assert_eq!(input.channel_layout, AudioChannelLayout::Stereo);
        assert_eq!(probe.input_names.len(), 1);
        assert_eq!(probe.input_names[0].bus_key, input.bus_key);
        assert_eq!(probe.input_names[0].name, "Sidechain");
        let events = &registration.event_bus_contract;
        assert_eq!(events.schema_version, 1);
        assert!(events.outputs.is_empty());
        assert_eq!(events.inputs.len(), 1);
        let input = &events.inputs.as_slice()[0];
        assert_eq!(
            (input.native_index, input.role, input.channel_count),
            (0, Vst3EventBusRole::Main, 16)
        );
        assert!(input.default_active && input.host_active);
        assert_eq!(registration.parameters.len(), PARAMETERS);
        assert_eq!(probe.current_values.len(), PARAMETERS);
        assert!(registration.parameters.iter().all(|p| !p.read_only));
        assert_eq!(
            registration.parameters.iter().filter(|p| p.can_automate).count(),
            AUTOMATABLE
        );
    }
    assert_eq!(
        captured.registration.parameters,
        restored.registration.parameters
    );
    assert_eq!(
        captured.registration.binary_sha256,
        restored.registration.binary_sha256
    );
    assert_eq!(
        captured.registration.execution_contract,
        restored.registration.execution_contract
    );
    assert_eq!(
        captured.registration.event_bus_contract,
        restored.registration.event_bus_contract
    );
    for ((parameter, &(id, value)), &(restored_id, restored_value)) in captured
        .registration
        .parameters
        .iter()
        .zip(&captured.current_values)
        .zip(&restored.current_values)
    {
        assert_eq!(
            (parameter.id, id),
            (id, restored_id),
            "native ordinal and identity"
        );
        assert_eq!(
            value.to_bits(),
            restored_value.to_bits(),
            "restored current value"
        );
    }
    println!(
        "{}",
        json!({"case":"surge-xt-vst3-main-runtime", "stage":"captured_restore",
        "state_bytes":state.len(),"parameters":PARAMETERS,"automatable":AUTOMATABLE,
        "nonautomatable":PARAMETERS-AUTOMATABLE,"native_main_outputs":3,
        "auxiliary_input":"vst3.audio-input.0","event_channels":16,
        "latency_frames":captured.registration.execution_contract.algorithmic_latency_frames(),
        "tail":format!("{:?}",captured.registration.execution_contract.tail())})
    );

    let instance = catalog
        .create_instance(CLASS, render, Some(state.clone()))
        .expect("canonical author through fresh Product capture-state restoration");
    assert!(!instance.bypassed);
    assert_eq!(
        instance.opaque_state.as_deref().map(Vec::as_slice),
        Some(state.as_slice())
    );
    assert!(matches!(&instance.definition,
        AudioProcessorDefinitionRef::Vst3 {class_id, vendor, schema_version:1, binary_sha256:Some(hash)}
        if class_id == CLASS && vendor.as_deref() == Some("Surge Synth Team")
            && *hash == captured.registration.binary_sha256));
    // Facts is only the existing pure typed schema/value checker. No legacy
    // GenericCase/parser, old JSON state spelling or <=1024 case limit is used.
    Facts::Vst3(
        captured.registration.parameters.clone(),
        captured.current_values.clone(),
    )
    .check_author(&instance);
    assert_eq!(
        instance.parameters.values().filter(|p| p.schema.is_animatable).count(),
        AUTOMATABLE
    );
    assert_eq!(
        instance.parameters.values().filter(|p| !p.schema.is_animatable).count(),
        PARAMETERS - AUTOMATABLE
    );
    let author_bytes = serde_json::to_vec(&instance).expect("canonical author serialization");
    let restored_author: AudioProcessorInstance = serde_json::from_slice(&author_bytes)
        .expect("canonical author reload without a private-wire size assertion");
    assert_eq!(restored_author, instance);
    let instance = restored_author;
    let parameter_ids: Vec<_> = instance.parameters.keys().cloned().collect();
    let processor_id = instance.id;
    let mut sequence = Sequence::new("real Surge XT VST3 Note/Main Runtime control");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.audio_program.musical_time_reference = reference;
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
    let note_id = AudioNoteId::new();
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
            id: note_id,
            start: time(ON),
            end: time(OFF),
            channel: 0,
            key: 60,
            on_velocity: 0.75,
            off_velocity: 0.25,
        }]),
    });
    let score = sequence.audio_program.note_lanes.clone();
    let output = sequence.audio_program.outputs[0].id;
    sequence.audio_program.routes.clear();
    sequence.audio_program.routes.push(AudioRoute::new(
        AudioRouteSource::ProcessorOutput { processor_id, port: AudioProcessorOutputPort::Main },
        AudioRouteDestination::Output(output),
    ));
    assert!(sequence.audio_program.sidechain_routes.is_empty());
    let selected = crate::compile_audio_program(&sequence, AudioCompileRequest::program(output))
        .expect("canonical selected Signal Closure retains the actual Note consumer");
    assert_eq!(selected.musical_time_reference(), reference);
    assert_eq!(selected.note_lanes.len(), 1);
    assert_eq!(selected.note_lanes[0].id, score[0].id);
    assert_eq!(selected.note_lanes[0].target, score[0].target);
    assert_eq!(
        selected.note_lanes[0].notes.as_slice(),
        score[0].notes.as_slice()
    );
    drop(selected);
    let media_resolves = Arc::new(AtomicUsize::new(0));
    let processors = IsolatedAudioProcessorResolver::new(catalog);
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &NoMedia(media_resolves.clone()),
        &processors,
        render,
        None,
        GRANT,
    )
    .expect("canonical real Runtime with the original finite aggregate resource grant");
    {
        let plan = runtime
            .find_resource_session(runtime.resource_root_identity())
            .unwrap()
            .resource_plan();
        assert_eq!(plan.program().musical_time_reference(), reference);
        assert_eq!(plan.schedule.processors.len(), 1);
        let processor = &plan.schedule.processors[0];
        assert_eq!(processor.parameter_ids, parameter_ids);
        assert_eq!(
            processor.factory.auxiliary_inputs(),
            &captured.registration.auxiliary_inputs
        );
        let outputs = &processor.factory.auxiliary_outputs().buses;
        assert_eq!(
            outputs.len(),
            2,
            "all native outputs are prepared in canonical order"
        );
        for (bus, key) in outputs.iter().zip(["vst3.audio-output.1", "vst3.audio-output.2"]) {
            assert_eq!(bus.bus_key, key);
            assert_eq!(bus.channel_layout, AudioChannelLayout::Stereo);
        }
        let notes = processor.factory.note_inputs();
        assert_eq!(
            notes.completion,
            AudioNoteCompletionPolicy::SuccessfulOffBlock
        );
        assert_eq!(notes.ports.len(), 1);
        let port = &notes.ports[0];
        assert_eq!(port.port_key, "vst3.note-input.0");
        assert_eq!(
            (port.native_id, port.native_index, port.channel_count),
            (0, 0, 16)
        );
    }
    let initial_footprint = runtime.resource_footprint();
    println!(
        "{}",
        json!({"case":"surge-xt-vst3-main-runtime", "stage":"runtime_prepared",
        "runtime_occurrences":initial_footprint.runtime_occurrences,
        "fixed_resident_bytes":initial_footprint.fixed_resident_bytes,
        "prepared_logical_bytes":initial_footprint.prepared_logical_bytes,
        "processor_session_bytes":initial_footprint.processor_session_bytes,
        "parameter_event_bytes":initial_footprint.parameter_event_bytes,
        "render_scratch_bytes":initial_footprint.render_scratch_bytes,
        "compensation_delay_bytes":initial_footprint.compensation_delay_bytes,
        "media_window_bytes":initial_footprint.media_window_bytes,
        "grant_occurrences":GRANT.max_runtime_occurrences,
        "grant_fixed_bytes":GRANT.max_fixed_resident_bytes,
        "grant_logical_bytes":GRANT.max_prepared_logical_bytes})
    );
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source.reserve_install(&registration, None).unwrap();
    let claim = source.begin_commit(claim).unwrap().commit().unwrap();
    let cancel = ExecutionCancellationToken::new();
    runtime.install_native_owner_bindings(&claim, &cancel).unwrap();
    runtime
        .validate_native_author_source_closed(&cancel)
        .expect("cold Source must be closed before state entry");
    let object = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: acquire query/synchronize rights only for the authenticated child;
    // adopt the returned owned handle once and retain it through original Drop.
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
        .expect("original state entry without a seek/reset substitute");
    let (mut handoffs, mut rescans) =
        service_author(&mut runtime, &mut sequence, &source, object, reference);
    let mut held_requests = 0;
    let mut energy = 0.0_f64;
    let mut peak = 0.0_f32;
    for start in (0..TOTAL).step_by(BLOCK) {
        let request = AudioRenderRequest { start_sample: start as i64, frames: BLOCK };
        let before = cursors(&runtime);
        let old = owner(&runtime);
        let old_claim = source.current_claim().unwrap().unwrap();
        let old_verifier = runtime.native_owner_installation_verifier().unwrap();
        let mut pcm = vec![OUTPUT_SENTINEL; BLOCK * 2];
        if let Err(error) = runtime.render_into(request, &mut pcm) {
            let AudioExecutionError::NativeAuthorPending(held) = error else {
                panic!("only a proved non-consuming Source HOLD permits one service: {error:?}");
            };
            assert_eq!(held.target, old.editor_target());
            assert_eq!(held.reason, NativeAudioAuthorHoldReason::PendingCallbacks);
            assert!(
                pcm.iter().all(|sample| sample.to_bits() == OUTPUT_SENTINEL.to_bits()),
                "Source HOLD must leave every output word untouched"
            );
            let after = cursors(&runtime);
            assert_eq!(before.sessions, after.sessions);
            assert_eq!(before.processors, after.processors);
            assert_eq!(owner(&runtime), old);
            assert_eq!(runtime.observe_native_owner_object(old).unwrap(), object);
            assert_eq!(
                source.current_claim().unwrap().unwrap().generation(),
                old_claim.generation()
            );
            old_verifier.validate(&source.current_claim().unwrap().unwrap()).unwrap();
            assert_eq!(media_resolves.load(Ordering::SeqCst), 0);
            let (accepted, scanned) =
                service_author(&mut runtime, &mut sequence, &source, object, reference);
            assert!(
                accepted > 0,
                "actual HOLD requires a canonical Source transaction and Ack"
            );
            same_progress(&before, &cursors(&runtime));
            handoffs += accepted;
            rescans += scanned;
            held_requests += 1;
            // This is the unique previously unconsumed request. A second HOLD
            // or any native failure is final; no retry loop or voice reset.
            runtime
                .render_into(request, &mut pcm)
                .expect("same unconsumed request completes once after authentic Source Ack");
        }
        assert!(
            pcm.iter().all(|sample| sample.is_finite()),
            "complete finite stereo PCM"
        );
        for sample in pcm {
            energy += f64::from(sample) * f64::from(sample);
            peak = peak.max(sample.abs());
        }
        assert_eq!(media_resolves.load(Ordering::SeqCst), 0);
        assert_eq!(
            runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
            object
        );
        let now = cursors(&runtime);
        assert_eq!(
            now.processors[0].occurrence,
            before.processors[0].occurrence
        );
        assert_eq!(
            now.sessions[0].identity.session_id,
            before.sessions[0].identity.session_id
        );
        assert!(matches!(now.processors[0].state,
            AudioProcessorCursorState::Active {epoch,next_input_signal_sample}
            if epoch == AudioContinuityEpoch::new(1)
                && next_input_signal_sample.sample() == (start + BLOCK) as i64));
        let (accepted, scanned) =
            service_author(&mut runtime, &mut sequence, &source, object, reference);
        handoffs += accepted;
        rescans += scanned;
        same_progress(&now, &cursors(&runtime));
    }
    assert!(
        energy.is_finite() && energy > 0.0 && peak > 0.0,
        "real instrument Main energy"
    );
    assert_eq!(sequence.audio_program.note_lanes, score);
    let final_cursors = cursors(&runtime);
    let processed_through = match final_cursors.processors[0].state {
        AudioProcessorCursorState::Active { epoch, next_input_signal_sample } => {
            assert_eq!(epoch, AudioContinuityEpoch::new(1));
            next_input_signal_sample.sample()
        }
        other => panic!("actual successful DSP prefix is required: {other:?}"),
    };
    assert_eq!(processed_through, TOTAL as i64);
    assert_eq!(media_resolves.load(Ordering::SeqCst), 0);
    runtime.validate_native_author_source_closed(&cancel).unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        WAIT_TIMEOUT
    );
    let final_footprint = runtime.resource_footprint();
    drop(runtime);
    // Zero wait observes original Drop without adding a shutdown allowance.
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit = u32::MAX;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
    println!(
        "{}",
        json!({"case":"surge-xt-vst3-main-runtime","stage":"completed",
        "class":CLASS,"plugin_version":descriptor.version,"state_bytes":state.len(),
        "parameters":PARAMETERS,"automatable":AUTOMATABLE,"nonautomatable":PARAMETERS-AUTOMATABLE,
        "musical_reference":reference,"rate":48000,"block":BLOCK,"frames":TOTAL,
        "note_on":ON,"note_off":OFF,"next_input_signal_sample":processed_through,
        "main_energy":energy,"main_peak":peak,"author_handoffs":handoffs,
        "nonempty_rescan_handoffs":rescans,"nonconsuming_held_requests":held_requests,
        "native_pid":pid,"object_serial":object.object_serial().get(),"media_resolves":0,
        "fixed_resident_bytes":final_footprint.fixed_resident_bytes,
        "prepared_logical_bytes":final_footprint.prepared_logical_bytes,
        "post_drop_wait_status":settled,"exit_query_succeeded":queried!=0,"exit_code":exit,
        "finite_runtime_grant":true,"complete_heap_qualified":false,
        "auxiliary_output_sound_qualified":false,"algorithm_or_note_release_qualified":false,
        "terminal_off_acoustics_qualified":false,"native_ffi_tuple_qualified":false,
        "graceful_ack_qualified":false,"realtime_qualified":false})
    );
    assert_eq!(
        settled, WAIT_OBJECT_0,
        "processing child exits after original Drop"
    );
    assert_ne!(queried, 0);
    assert_eq!(exit, 0, "forced/error native exit cannot pass");
}
