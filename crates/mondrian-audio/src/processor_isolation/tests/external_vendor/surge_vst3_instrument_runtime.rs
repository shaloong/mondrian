//! Real Surge XT VST3: finite captured-state/Note/Main Runtime consumer.
//! Uses the complete native parameter census and the original Source/Ack path.
//! Fixed musical reference only; no algorithm, release, auxiliary sound or RT oracle.

use super::surge_instrument_runtime::accept_actual_rescans;
use super::{owner, Cursors};
use crate::processor_isolation::vst3_discovery::probe_vst3_plugin_registration_with_state;
use crate::processor_isolation::{
    DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver, Vst3EventBusRole,
    Vst3MainAudioInput, Vst3ParameterDescriptor,
};
use crate::{
    AudioCompileRequest, AudioContinuityEpoch, AudioExecutionError, AudioMediaResolver,
    AudioNoteCompletionPolicy, AudioProcessingMode, AudioProcessorCursorState, AudioProgramRuntime,
    AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant, AudioStateEntry,
    NativeAudioAuthorHoldReason, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource,
    AudioSourceComponentId, AuthoringList, AuthoringMap, ExecutionCancellationToken,
    MusicalTimeReference, ParameterId, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorDefinitionRef, AudioProcessorInstance,
    AudioProcessorParameter,
};
use mondrian_timeline::{
    AudioProcessorOutputPort, AudioRoute, AudioRouteDestination, AudioRouteSource, Sequence,
};
use serde_json::json;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
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
// Same predeclared middle window as the stopped SDK native-state witness.
const SUSTAIN_START: usize = 2_048;
const SUSTAIN_END: usize = 6_144;
const SUSTAIN_BLOCKS: usize = (SUSTAIN_END - SUSTAIN_START) / BLOCK;
const MIN_SUSTAIN_ENERGY: f64 = 1e-12;
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

// Check the actual native-owned author directly against the sole descriptor
// projection. The shared Facts checker requires Some constants and must not be
// satisfied with a fabricated Some copy of this None-owned model.
fn check_native_owned_author(
    instance: &AudioProcessorInstance,
    parameters: &[Vst3ParameterDescriptor],
) {
    assert_eq!(parameters.len(), PARAMETERS);
    assert_eq!(instance.parameters.len(), PARAMETERS);
    let mut ids = std::collections::BTreeSet::new();
    for descriptor in parameters {
        assert!(!descriptor.read_only, "complete writable native census");
        let id = descriptor.parameter_id().expect("actual unsigned VST3 ID");
        assert!(ids.insert(id.clone()), "unique actual VST3 IDs");
        let parameter = instance.parameters.get(&id).expect("complete native-owned author");
        assert_eq!(
            parameter.schema,
            descriptor.authoring_schema().expect("sole canonical VST3 schema projection")
        );
        assert_eq!(
            parameter.ui.display_name.as_deref(),
            Some(descriptor.name.as_str())
        );
        assert!(!parameter.ui.hidden);
        assert!(
            parameter.automation.is_none(),
            "native state owns every initial value"
        );
    }
    assert_eq!(
        instance.parameters.values().filter(|p| p.schema.is_animatable).count(),
        AUTOMATABLE
    );
    assert_eq!(
        instance.parameters.values().filter(|p| !p.schema.is_animatable).count(),
        PARAMETERS - AUTOMATABLE
    );
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
    native_owned_parameters: &AuthoringMap<ParameterId, AudioProcessorParameter>,
) -> (usize, usize) {
    let current = &sequence.audio_program.track_channels[&sequence.audio_tracks[0].id]
        .strip
        .pre_fader
        .processors[0];
    assert_eq!(
        &current.parameters, native_owned_parameters,
        "a real parameter edit cannot be disguised as native-owned None"
    );
    let old_state = current.opaque_state.clone();
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
    let current = &sequence.audio_program.track_channels[&sequence.audio_tracks[0].id]
        .strip
        .pre_fader
        .processors[0];
    assert_eq!(
        &current.parameters, native_owned_parameters,
        "rescan may refresh raw state but cannot synthesize host overrides"
    );
    let state = current
        .opaque_state
        .as_ref()
        .expect("actual captured or rescanned native state");
    assert!(!state.is_empty() && state.len() <= 128 * 1024);
    if result.0 == 0 {
        assert_eq!(
            current.opaque_state, old_state,
            "no publication cannot replace raw state"
        );
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

// Shared author construction only; both consumers execute the public Runtime.
fn canonical_note_sequence(
    instance: AudioProcessorInstance,
    reference: Option<MusicalTimeReference>,
) -> Sequence {
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
    let output = sequence.audio_program.outputs[0].id;
    sequence.audio_program.routes.clear();
    sequence.audio_program.routes.push(AudioRoute::new(
        AudioRouteSource::ProcessorOutput { processor_id, port: AudioProcessorOutputPort::Main },
        AudioRouteDestination::Output(output),
    ));
    assert!(sequence.audio_program.sidechain_routes.is_empty());
    sequence
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
        parameter
            .author_scalar_from_normalized(value)
            .expect("actual normalized current value belongs to its canonical schema");
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
    check_native_owned_author(&instance, &captured.registration.parameters);
    let author_bytes = serde_json::to_vec(&instance).expect("canonical author serialization");
    let restored_author: AudioProcessorInstance = serde_json::from_slice(&author_bytes)
        .expect("canonical author reload without a private-wire size assertion");
    assert_eq!(restored_author, instance);
    let instance = restored_author;
    // A structurally shared test snapshot preserves the validated full schema,
    // UI and None profile after every real Source transaction. It is never
    // submitted as a second parameter authority or sent as DSP overrides.
    let native_owned_parameters = instance.parameters.clone();
    let parameter_ids: Vec<_> = instance.parameters.keys().cloned().collect();
    let mut sequence = canonical_note_sequence(instance, reference);
    let score = sequence.audio_program.note_lanes.clone();
    let output = sequence.audio_program.outputs[0].id;
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
    let (mut handoffs, mut rescans) = service_author(
        &mut runtime,
        &mut sequence,
        &source,
        object,
        reference,
        &native_owned_parameters,
    );
    let mut held_requests = 0;
    let mut energy = 0.0_f64;
    let mut peak = 0.0_f32;
    let mut sustain_block_energy = [0.0_f64; SUSTAIN_BLOCKS];
    let mut sustain_blocks_measured = 0;
    for start in (0..TOTAL).step_by(BLOCK) {
        let request = AudioRenderRequest { transport_activity: None, start_sample: start as i64, frames: BLOCK };
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
            let (accepted, scanned) = service_author(
                &mut runtime,
                &mut sequence,
                &source,
                object,
                reference,
                &native_owned_parameters,
            );
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
        let mut block_energy = 0.0_f64;
        for sample in pcm {
            let square = f64::from(sample) * f64::from(sample);
            energy += square;
            block_energy += square;
            peak = peak.max(sample.abs());
        }
        if (SUSTAIN_START..SUSTAIN_END).contains(&start) {
            assert!(
                start + BLOCK <= SUSTAIN_END,
                "complete predeclared sustain block"
            );
            sustain_block_energy[(start - SUSTAIN_START) / BLOCK] = block_energy;
            sustain_blocks_measured += 1;
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
        let (accepted, scanned) = service_author(
            &mut runtime,
            &mut sequence,
            &source,
            object,
            reference,
            &native_owned_parameters,
        );
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
    let sustain_energy: f64 = sustain_block_energy.iter().sum();
    drop(runtime);
    // Zero wait observes original Drop without adding a shutdown allowance.
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit = u32::MAX;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
    let mut completed = json!({"case":"surge-xt-vst3-main-runtime","stage":"completed",
        "class":CLASS,"plugin_version":descriptor.version,"state_bytes":state.len(),
        "parameters":PARAMETERS,"automatable":AUTOMATABLE,"nonautomatable":PARAMETERS-AUTOMATABLE,
        "musical_reference":reference,"rate":48000,"block":BLOCK,"frames":TOTAL,
        "note_on":ON,"note_off":OFF,"next_input_signal_sample":processed_through,
        "main_energy":energy,"main_peak":peak,
        "author_handoffs":handoffs,
        "nonempty_rescan_handoffs":rescans,"nonconsuming_held_requests":held_requests,
        "native_pid":pid,"object_serial":object.object_serial().get(),"media_resolves":0,
        "fixed_resident_bytes":final_footprint.fixed_resident_bytes,
        "prepared_logical_bytes":final_footprint.prepared_logical_bytes,
        "post_drop_wait_status":settled,"exit_query_succeeded":queried!=0,"exit_code":exit,
        "finite_runtime_grant":true,"complete_heap_qualified":false,
        "auxiliary_output_sound_qualified":false,"algorithm_or_note_release_qualified":false,
        "terminal_off_acoustics_qualified":false,"native_ffi_tuple_qualified":false,
        "graceful_ack_qualified":false,"realtime_qualified":false});
    completed["sustain_window"] = json!([SUSTAIN_START, SUSTAIN_END]);
    completed["sustain_block_frames"] = json!(BLOCK);
    completed["sustain_blocks_measured"] = json!(sustain_blocks_measured);
    completed["sustain_block_energy"] = json!(sustain_block_energy);
    completed["sustain_energy"] = json!(sustain_energy);
    completed["minimum_sustain_energy_exclusive"] = json!(MIN_SUSTAIN_ENERGY);
    completed["native_owned_none_parameters"] = json!(native_owned_parameters.len());
    println!("{completed}");
    assert_eq!(
        settled, WAIT_OBJECT_0,
        "processing child exits after original Drop"
    );
    assert_ne!(queried, 0);
    assert_eq!(exit, 0, "forced/error native exit cannot pass");
    assert_eq!(
        sustain_blocks_measured, 16,
        "all fixed middle blocks were actually rendered"
    );
    assert!(
        sustain_block_energy.iter().all(|energy| energy.is_finite() && *energy > 0.0),
        "every complete predeclared middle block must contain finite positive Main energy"
    );
    assert!(
        sustain_energy.is_finite() && sustain_energy > MIN_SUSTAIN_ENERGY,
        "native-owned Main must sustain sound across the predeclared middle window"
    );
}

#[test]
#[ignore = "requires outer native Job, current Product, real Surge XT VST3 and fixed MONDRIAN_SURGE_VST3_ORIGIN_STATE"]
fn external_surge_xt_vst3_origin_reentry_rebuilds_and_sustains_main() {
    run_surge_cold_entries(
        [0, 0],
        [6_144, 8_448],
        [24, 33],
        [[SUSTAIN_START, SUSTAIN_END]; 2],
    );
}

#[test]
#[ignore = "requires outer native Job, current Product, real Surge XT VST3 and fixed MONDRIAN_SURGE_VST3_ORIGIN_STATE; cold nonzero retrigger only"]
fn external_surge_xt_vst3_nonzero_cold_entry_retriggers_and_sustains_main() {
    // Both entries are strictly inside the unchanged authored Note127..8193.
    // Each fixed sustain window starts 2048 samples after its fresh retrigger,
    // remains before Off8193, and contains the original 16 complete blocks.
    run_surge_cold_entries(
        [1_024, 2_048],
        [7_168, 8_448],
        [24, 25],
        [[3_072, 7_168], [4_096, 8_192]],
    );
}

// One original public Runtime/Source/Ack/Drop consumer. These explicit cases
// differ only in their cold entry coordinates and predeclared measurement spans.
fn run_surge_cold_entries(
    starts: [usize; 2],
    ends: [usize; 2],
    blocks: [usize; 2],
    sustain_windows: [[usize; 2]; 2],
) {
    const STATE_BYTES: usize = 51_928;
    let nonzero = starts[0] != 0;
    assert!(ends[0] < OFF as usize && ends[1] > OFF as usize);
    for phase in 0..2 {
        assert!(starts[phase] < ends[phase]);
        assert_eq!(ends[phase] - starts[phase], blocks[phase] * BLOCK);
        assert_eq!(
            sustain_windows[phase][1] - sustain_windows[phase][0],
            SUSTAIN_BLOCKS * BLOCK
        );
        assert!(
            starts[phase] <= sustain_windows[phase][0] && sustain_windows[phase][1] <= ends[phase]
        );
        assert!(sustain_windows[phase][1] <= OFF as usize);
        if nonzero {
            assert!(ON < starts[phase] as i64 && (starts[phase] as i64) < OFF);
        }
    }
    let mut in_job = 0;
    // SAFETY: call-local writable BOOL and current process pseudo handle.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "original outer owner Job before native creation");
    let file = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = file("MONDRIAN_VST3_TEST_HELPER");
    let library = std::fs::canonicalize(file("MONDRIAN_SURGE_XT_VST3_DLL")).unwrap();
    let state_path = file("MONDRIAN_SURGE_VST3_ORIGIN_STATE");
    assert_eq!(
        std::fs::metadata(&state_path).unwrap().len(),
        STATE_BYTES as u64
    );
    let state = std::fs::read(&state_path).expect("read the retained seed; never recapture");
    assert_eq!(state.len(), STATE_BYTES);
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 64 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4_096,
        compensation_delay_scratch_budget_bytes: 64 * 1024 * 1024,
    };
    let reference = Some(
        MusicalTimeReference::new(TimelineTime::ZERO, TimelineTime::new(1, 2).unwrap()).unwrap(),
    );
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [library.clone()])
            .expect("current Product discovery"),
    );
    let descriptors = catalog.descriptors();
    let matching: Vec<_> =
        descriptors.iter().filter(|descriptor| descriptor.class_id == CLASS).collect();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].name, "Surge XT");
    assert_eq!(matching[0].version.as_deref(), Some("1.3.4"));
    assert_eq!(matching[0].vendor.as_deref(), Some("Surge Synth Team"));
    let restored = probe_vst3_plugin_registration_with_state(
        &helper,
        &library,
        CLASS,
        render,
        Some(&state),
        false,
    )
    .expect("restore the fixed original seed without a new capture");
    assert!(restored.captured_state.is_none());
    assert_eq!(restored.registration.class_id, CLASS);
    assert_eq!(
        restored.registration.main_audio_input,
        Vst3MainAudioInput::Absent
    );
    assert_eq!(
        restored.registration.execution_contract.algorithmic_latency_frames(),
        0
    );
    assert_eq!(restored.registration.parameters.len(), PARAMETERS);
    assert_eq!(restored.current_values.len(), PARAMETERS);
    let instance = catalog
        .create_instance(CLASS, render, Some(state.clone()))
        .expect("public author factory with fixed native-owned seed");
    assert!(!instance.bypassed);
    assert_eq!(
        instance.opaque_state.as_deref().map(Vec::as_slice),
        Some(state.as_slice())
    );
    check_native_owned_author(&instance, &restored.registration.parameters);
    assert!(matches!(&instance.definition,
        AudioProcessorDefinitionRef::Vst3 {class_id, vendor, schema_version:1, binary_sha256:Some(hash)}
        if class_id == CLASS && vendor.as_deref() == Some("Surge Synth Team") && *hash == restored.registration.binary_sha256));
    let native_owned_parameters = instance.parameters.clone();
    let selected_parameter = instance.parameters.keys().next().unwrap().clone();
    let mut sequence = canonical_note_sequence(instance, reference);
    let score = sequence.audio_program.note_lanes.clone();
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
    .expect("original public Runtime and finite grant");
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source
        .begin_commit(source.reserve_install(&registration, None).unwrap())
        .unwrap()
        .commit()
        .unwrap();
    let cancel = ExecutionCancellationToken::new();
    runtime.install_native_owner_bindings(&claim, &cancel).unwrap();
    let initial_object = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
    let pid = initial_object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: authentic supervised child PID, query/synchronize only; one owner.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let target = catch_unwind(AssertUnwindSafe(|| {
        runtime
            .validate_native_author_source_closed(&cancel)
            .expect("initial owner has no discarded author work");
        let first = nonzero.then(|| {
            (
                owner(&runtime),
                runtime.resource_root_identity(),
                source.current_claim().unwrap().unwrap(),
            )
        });
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(1),
                start_sample: starts[0] as i64,
            })
            .expect(if nonzero {
                "first explicit cold score entry"
            } else {
                "first score-origin entry"
            });
        if let Some((old, root, claim)) = first {
            // Before any post-Enter author service, the first nonzero entry
            // anchors the actual prepared fresh object and revokes its old lease.
            let fresh = owner(&runtime);
            assert_eq!(
                runtime.observe_native_owner_object(fresh).unwrap(),
                initial_object
            );
            assert_ne!(fresh.binding(), old.binding());
            assert_eq!(fresh.address().root, old.address().root);
            assert_eq!(fresh.address().local, old.address().local);
            assert_eq!(fresh.address().occurrence, old.address().occurrence);
            assert_eq!(fresh.address().epoch, Some(AudioContinuityEpoch::new(1)));
            assert_eq!(runtime.resource_root_identity(), root);
            assert_eq!(
                source.current_claim().unwrap().unwrap().generation(),
                claim.generation()
            );
            runtime.native_owner_installation_verifier().unwrap().validate(&claim).unwrap();
            assert!(runtime.validate_native_owner(old).is_err());
            assert!(runtime.observe_native_owner_object(old).is_err());
            assert!(matches!(
                runtime
                    .query_native_parameter(
                        &mondrian_core::NativeAudioParameterQuery {
                            target: old.editor_target(),
                            parameter: selected_parameter.clone(),
                        },
                        &cancel
                    )
                    .unwrap(),
                mondrian_core::NativeAudioParameterQueryOutcome::Refused(
                    mondrian_core::NativeAudioParameterQueryRefusal::Unavailable
                )
            ));
            let entered = cursors(&runtime);
            assert!(matches!(entered.sessions[0].state,
                crate::AudioSessionCursorState::Active { epoch, next_public_sample, next_execution_sample, .. }
                    if epoch == AudioContinuityEpoch::new(1)
                        && next_public_sample.sample() == starts[0] as i64
                        && next_execution_sample.sample() == starts[0] as i64));
            assert!(matches!(entered.processors[0].state,
                AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                    if epoch == AudioContinuityEpoch::new(1)
                        && next_input_signal_sample.sample() == starts[0] as i64));
        }
        let mut object = initial_object;
        let mut handoffs = 0;
        let mut rescans = 0;
        let mut held_requests = 0;
        let mut energies = [0.0_f64; 2];
        let mut peaks = [0.0_f32; 2];
        let mut sustain = [[0.0_f64; SUSTAIN_BLOCKS]; 2];
        let mut measured = [0_usize; 2];
        let mut epoch_serials = [initial_object.object_serial().get(); 2];
        for (phase, end) in ends.into_iter().enumerate() {
            if phase == 1 {
                // The first component still owes Off8193. Service the real
                // current Source, then capture current identities, never the
                // original Plan before any genuine author publication.
                let (accepted, scanned) = service_author(
                    &mut runtime,
                    &mut sequence,
                    &source,
                    object,
                    reference,
                    &native_owned_parameters,
                );
                handoffs += accepted;
                rescans += scanned;
                runtime.validate_native_author_source_closed(&cancel).unwrap();
                let old = owner(&runtime);
                let old_target = old.editor_target();
                let retired_object = runtime.observe_native_owner_object(old).unwrap();
                assert_eq!(retired_object, object);
                let old_root = runtime.resource_root_identity();
                let old_claim = source.current_claim().unwrap().unwrap();
                let old_verifier = runtime.native_owner_installation_verifier().unwrap();
                assert!(
                    matches!(cursors(&runtime).processors[0].state, AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                    if epoch == AudioContinuityEpoch::new(1) && next_input_signal_sample.sample() == ends[0] as i64)
                );
                runtime
                    .enter_state(AudioStateEntry {
                        epoch: AudioContinuityEpoch::new(2),
                        start_sample: starts[phase] as i64,
                    })
                    .expect(if nonzero {
                        "one actual cold reentry; never retry a failed initialization"
                    } else {
                        "one actual origin reentry; never retry a failed initialization"
                    });
                // These checks precede all post-Enter author Poll/install/Ack:
                // later real publications cannot explain old-lease revocation.
                let fresh = owner(&runtime);
                object = runtime.observe_native_owner_object(fresh).unwrap();
                assert_eq!(object.process_id().get(), pid);
                assert!(object.object_serial() > retired_object.object_serial());
                epoch_serials[phase] = object.object_serial().get();
                assert_ne!(fresh.binding(), old.binding());
                assert_eq!(fresh.address().root, old.address().root);
                assert_eq!(fresh.address().local, old.address().local);
                assert_eq!(fresh.address().occurrence, old.address().occurrence);
                assert_eq!(fresh.address().epoch, Some(AudioContinuityEpoch::new(2)));
                assert_eq!(runtime.resource_root_identity(), old_root);
                assert_eq!(
                    source.current_claim().unwrap().unwrap().generation(),
                    old_claim.generation()
                );
                old_verifier.validate(&old_claim).unwrap();
                assert!(runtime.validate_native_owner(old).is_err());
                assert!(runtime.observe_native_owner_object(old).is_err());
                assert!(matches!(
                    runtime
                        .query_native_parameter(
                            &mondrian_core::NativeAudioParameterQuery {
                                target: old_target,
                                parameter: selected_parameter.clone()
                            },
                            &cancel
                        )
                        .unwrap(),
                    mondrian_core::NativeAudioParameterQueryOutcome::Refused(
                        mondrian_core::NativeAudioParameterQueryRefusal::Unavailable
                    )
                ));
                let entered = cursors(&runtime);
                assert!(
                    matches!(entered.sessions[0].state, crate::AudioSessionCursorState::Active { epoch, next_public_sample, next_execution_sample, .. }
                    if epoch == AudioContinuityEpoch::new(2) && next_public_sample.sample() == starts[phase] as i64 && next_execution_sample.sample() == starts[phase] as i64)
                );
                assert!(
                    matches!(entered.processors[0].state, AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                    if epoch == AudioContinuityEpoch::new(2) && next_input_signal_sample.sample() == starts[phase] as i64)
                );
            }
            let (accepted, scanned) = service_author(
                &mut runtime,
                &mut sequence,
                &source,
                object,
                reference,
                &native_owned_parameters,
            );
            handoffs += accepted;
            rescans += scanned;
            for start in (starts[phase]..end).step_by(BLOCK) {
                let request = AudioRenderRequest { transport_activity: None, start_sample: start as i64, frames: BLOCK };
                let before = cursors(&runtime);
                let old = owner(&runtime);
                let old_claim = source.current_claim().unwrap().unwrap();
                let mut pcm = vec![OUTPUT_SENTINEL; BLOCK * 2];
                if let Err(error) = runtime.render_into(request, &mut pcm) {
                    let AudioExecutionError::NativeAuthorPending(held) = error else {
                        panic!("actual render failure: {error:?}")
                    };
                    assert_eq!(held.target, old.editor_target());
                    assert_eq!(held.reason, NativeAudioAuthorHoldReason::PendingCallbacks);
                    assert!(pcm.iter().all(|sample| sample.to_bits() == OUTPUT_SENTINEL.to_bits()));
                    same_progress(&before, &cursors(&runtime));
                    assert_eq!(owner(&runtime), old);
                    assert_eq!(runtime.observe_native_owner_object(old).unwrap(), object);
                    assert_eq!(
                        source.current_claim().unwrap().unwrap().generation(),
                        old_claim.generation()
                    );
                    let (accepted, scanned) = service_author(
                        &mut runtime,
                        &mut sequence,
                        &source,
                        object,
                        reference,
                        &native_owned_parameters,
                    );
                    assert!(
                        accepted > 0,
                        "only a real Source install/exact Ack permits one retry"
                    );
                    handoffs += accepted;
                    rescans += scanned;
                    held_requests += 1;
                    same_progress(&before, &cursors(&runtime));
                    runtime
                        .render_into(request, &mut pcm)
                        .expect("the same unconsumed request once after real Ack");
                }
                assert!(pcm.iter().all(|sample| sample.is_finite()));
                let block_energy: f64 = pcm
                    .iter()
                    .map(|sample| {
                        let value = f64::from(*sample);
                        value * value
                    })
                    .sum();
                energies[phase] += block_energy;
                for sample in pcm {
                    peaks[phase] = peaks[phase].max(sample.abs());
                }
                let [sustain_start, sustain_end] = sustain_windows[phase];
                if (sustain_start..sustain_end).contains(&start) {
                    assert!(start + BLOCK <= sustain_end);
                    sustain[phase][(start - sustain_start) / BLOCK] = block_energy;
                    measured[phase] += 1;
                }
                assert_eq!(media_resolves.load(Ordering::SeqCst), 0);
                assert_eq!(
                    runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
                    object
                );
                let now = cursors(&runtime);
                assert!(
                    matches!(now.processors[0].state, AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
                    if epoch == AudioContinuityEpoch::new(phase as u64 + 1) && next_input_signal_sample.sample() == (start + BLOCK) as i64)
                );
                let (accepted, scanned) = service_author(
                    &mut runtime,
                    &mut sequence,
                    &source,
                    object,
                    reference,
                    &native_owned_parameters,
                );
                handoffs += accepted;
                rescans += scanned;
                same_progress(&now, &cursors(&runtime));
                assert_eq!(sequence.audio_program.note_lanes, score);
                check_native_owned_author(
                    &sequence.audio_program.track_channels[&sequence.audio_tracks[0].id]
                        .strip
                        .pre_fader
                        .processors[0],
                    &restored.registration.parameters,
                );
            }
        }
        runtime.validate_native_author_source_closed(&cancel).unwrap();
        assert_eq!(
            std::fs::read(&state_path).unwrap(),
            state,
            "fixed external seed was only read"
        );
        assert_eq!(measured, [SUSTAIN_BLOCKS; 2]);
        for phase in 0..2 {
            assert!(energies[phase].is_finite() && energies[phase] > 0.0 && peaks[phase] > 0.0);
            assert!(sustain[phase].iter().all(|energy| energy.is_finite() && *energy > 0.0));
            assert!(sustain[phase].iter().sum::<f64>() > MIN_SUSTAIN_ENERGY);
        }
        assert_eq!(media_resolves.load(Ordering::SeqCst), 0);
        (
            epoch_serials,
            energies,
            peaks,
            sustain,
            handoffs,
            rescans,
            held_requests,
        )
    }));
    let cleanup = catch_unwind(AssertUnwindSafe(|| {
        drop(runtime);
        // The original consumer uses zero-wait after consuming Drop. No added
        // stop/RPC/wait budget and no DLL-unload or whole-100ms assertion.
        let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
        let mut exit = u32::MAX;
        let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
        assert_eq!(settled, WAIT_OBJECT_0);
        assert_ne!(queried, 0);
        assert_eq!(exit, 0, "original Drop must settle the child cleanly");
        drop(source);
        (settled, queried != 0, exit)
    }));
    let (serials, energies, peaks, sustain, handoffs, rescans, held_requests) = match target {
        Ok(result) => result,
        Err(primary) => resume_unwind(primary),
    };
    let (settled, queried, exit) = match cleanup {
        Ok(result) => result,
        Err(error) => resume_unwind(error),
    };
    // A real cold retrigger/sustain witness, not cross-epoch PCM equivalence,
    // continuous-history/seek equivalence, terminal-Off acoustics or FFI IDs.
    let mut observation = json!({"case":"surge-xt-vst3-origin-reentry", "stage":"completed",
        "class":CLASS,"plugin_version":"1.3.4","state_bytes":STATE_BYTES,
        "parameters":PARAMETERS,"native_owned_none_parameters":PARAMETERS,
        "musical_reference":reference,"rate":48000,"block":BLOCK,"epoch_ends":ends,
        "epoch_blocks":blocks,"native_pid":pid,"object_serials":serials,
        "main_energy":energies,"main_peak":peaks,"sustain_window":[SUSTAIN_START,SUSTAIN_END],
        "sustain_block_energy":sustain,"minimum_sustain_energy_exclusive":MIN_SUSTAIN_ENERGY,
        "author_handoffs":handoffs,"nonempty_rescan_handoffs":rescans,"nonconsuming_held_requests":held_requests,
        "media_resolves":0,"post_drop_wait_status":settled,"exit_query_succeeded":queried,"exit_code":exit,
        "cross_epoch_bitexact_qualified":false,"nonzero_seek_qualified":false,
        "terminal_off_acoustics_qualified":false,"native_ffi_tuple_qualified":false});
    if nonzero {
        observation["case"] = json!("surge-xt-vst3-nonzero-cold-entry");
        observation["epoch_starts"] = json!(starts);
        observation["sustain_window"] = json!(sustain_windows);
        observation["nonzero_cold_entry_qualified"] = json!(true);
        observation["continuous_dsp_history_qualified"] = json!(false);
    }
    println!("{observation}");
}
