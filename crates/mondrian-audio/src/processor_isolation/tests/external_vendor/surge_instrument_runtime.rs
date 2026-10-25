//! One real Surge XT CLAP state/Note/Main-output Runtime consumer.
//! Other outputs are prepared, never used as a sound or algorithm oracle.
//! Some supplies a constant musical reference, optionally with meter. Successful
//! Note/PCM processing does not establish a musical-response algorithm oracle.

use super::{owner, Cursors};
use crate::processor_isolation::clap_discovery::probe_clap_plugin_registration_with_state;
use crate::processor_isolation::protocol::{auxiliary_to_wire, outputs_to_wire};
use crate::processor_isolation::{
    probe_clap_note_processing_offer, ClapMainAudioInput, DiscoveredClapAudioProcessorSpecResolver,
    IsolatedAudioProcessorResolver,
};
use crate::{
    AudioCompileRequest, AudioContinuityEpoch, AudioMediaResolver, AudioProcessingMode,
    AudioProcessorCursorState, AudioProgramRuntime, AudioRenderContract, AudioRenderRequest,
    AudioRuntimeResourceGrant, AudioStateEntry, NativeAudioAuthorProgram,
    NativeAudioParameterHandoffContinuity, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource,
    AudioSourceComponentId, AuthoringList, ExecutionCancellationToken, MusicalTimeReference,
    NativeAudioAuthorCallback, NativeAudioAuthorCompletion, TimelineTime,
};
use mondrian_timeline::audio::{AudioChannelStripOwner, AudioNote, AudioNoteLane, AudioNoteTarget};
use mondrian_timeline::{
    apply_audio_processor_rack_edit, AudioChannelStripRack, AudioProcessorOutputPort,
    AudioProcessorRackAddress, AudioProcessorRackEdit, AudioProcessorRackEditRequest, AudioRoute,
    AudioRouteDestination, AudioRouteSource, NativeAudioAuthorUpdate, NativeAudioParameterWrite,
    NativeAudioParameterWriteMode, Sequence,
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

const ID: &str = "org.surge-synth-team.surge-xt";
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
        Err("instrument-only Runtime must not resolve media".into())
    }
}

// The App's Rescan policy, applied through the original canonical transaction
// and Source installation. A copied proposal never supplies its own Ack.
pub(super) fn accept_actual_rescans(
    runtime: &mut AudioProgramRuntime,
    sequence: &mut Sequence,
    source: &AudioOwnerInstallationSource,
    expected_object: crate::NativeAudioObjectObservation,
    reference: Option<MusicalTimeReference>,
    max_rescan_values: usize,
    max_state_bytes: usize,
) -> (usize, usize) {
    let cancel = ExecutionCancellationToken::new();
    assert_eq!(sequence.audio_program.musical_time_reference, reference);
    assert_eq!(
        runtime
            .find_resource_session(runtime.resource_root_identity())
            .unwrap()
            .resource_plan()
            .program()
            .musical_time_reference(),
        reference
    );
    let track = sequence.audio_tracks[0].id;
    let output = sequence.audio_program.outputs[0].id;
    let score = sequence.audio_program.note_lanes.clone();
    let routes = sequence.audio_program.routes.clone();
    let mut rescan_handoffs = 0;
    for handoffs in 0..4 {
        let lease = owner(runtime);
        let target = lease.editor_target();
        assert_eq!(
            runtime.observe_native_owner_object(lease).unwrap(),
            expected_object
        );
        let batch = runtime.poll_native_observations(&cancel).expect("actual bounded native poll");
        let current = &sequence.audio_program.track_channels[&track].strip.pre_fader.processors[0];
        assert_eq!(target.instance, current.id);
        assert_eq!(
            target.owner,
            mondrian_core::NativeAudioEditorOccurrenceOwner::Track(track)
        );
        assert_eq!(
            target.insertion,
            mondrian_core::NativeAudioEditorInsertion::PreFader
        );
        if let Some(batch) = &batch {
            if let Some(parameters) = &batch.parameters {
                assert_eq!(parameters.target(), target);
                assert_eq!(parameters.process_id(), expected_object.process_id());
                assert_eq!(parameters.object_serial(), expected_object.object_serial());
                assert!(parameters.facts().len() <= mondrian_core::NATIVE_AUDIO_PARAMETER_FACTS);
                for fact in parameters.facts() {
                    fact.validate().expect("bounded actual parameter presentation");
                    assert!(fact.revision <= parameters.source_revision());
                    assert!(
                        current.parameters.contains_key(&fact.parameter),
                        "unknown parameter fact"
                    );
                }
            }
            if let Some(logs) = &batch.diagnostics {
                assert!(
                    !logs.loss().incomplete(),
                    "native diagnostic loss cannot be ignored"
                );
                let actual = logs.source();
                assert_eq!(actual.process_id, expected_object.process_id());
                assert_eq!(
                    (actual.root_plan, actual.root_session),
                    (target.root_plan, target.root_session)
                );
                assert_eq!(
                    (actual.local_plan, actual.local_session),
                    (target.local_plan, target.local_session)
                );
                assert_eq!(actual.instance, target.instance);
                assert_eq!(
                    actual.location,
                    mondrian_core::NativeAudioDiagnosticLocation::Track(track)
                );
                assert_eq!(
                    actual.insertion,
                    mondrian_core::NativeAudioDiagnosticInsertion::PreFader
                );
                for record in logs.records() {
                    assert_eq!(
                        record.facts(),
                        mondrian_core::NativeAudioDiagnosticMessageFacts::default(),
                        "incomplete diagnostic record"
                    );
                }
            }
        }
        let Some(proposal) = batch.and_then(|batch| batch.author) else {
            runtime
                .validate_native_author_source_closed(&cancel)
                .expect("no proposal is not proof of a closed Source");
            assert_eq!(
                runtime.observe_native_owner_object(owner(runtime)).unwrap(),
                expected_object
            );
            return (handoffs, rescan_handoffs);
        };
        assert_eq!(proposal.target(), target);
        assert_eq!(proposal.sequence(), sequence.id);
        assert!(proposal.revision() > 0);
        assert!(!proposal.callbacks().is_empty());
        assert!(proposal.callbacks().len() <= mondrian_core::NATIVE_AUDIO_AUTHOR_CALLBACKS);
        assert_eq!(proposal.open_parameter_edits(), 0);
        assert_eq!(proposal.group_depth(), 0);
        assert_eq!(
            proposal.completion(),
            NativeAudioAuthorCompletion::CoherentConfiguration
        );
        let state = proposal.state().expect("closed actual rescan requires coherent state");
        assert!(
            !state.is_empty() && state.len() <= max_state_bytes,
            "actual bounded coherent state"
        );
        let mut writes = Vec::new();
        let mut scanned = 0usize;
        for callback in proposal.callbacks() {
            match callback {
                NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values } => {
                    scanned = scanned.checked_add(values.len()).expect("bounded rescan sum");
                    assert!(
                        scanned <= max_rescan_values,
                        "explicit actual rescan value bound"
                    );
                    assert!(kind.refreshes_values() || values.is_empty());
                    for (id, bits) in values {
                        let parameter = current.parameters.get(id).expect("unknown rescan schema");
                        let value = f64::from_bits(*bits);
                        assert!(value.is_finite());
                        // Match the App: absent references stay absent; keyed
                        // automation stays sole authority. Unknown IDs refuse.
                        if kind.refreshes_values()
                            && parameter
                                .automation
                                .as_ref()
                                .is_some_and(|curve| curve.keyframes.is_empty())
                        {
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
                }
                NativeAudioAuthorCallback::DirtyChanged(_) => {}
                other => panic!("no canonical policy for actual callback: {other:?}"),
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
        apply_audio_processor_rack_edit(sequence, &request)
            .expect("atomic actual Track native-author transaction");
        assert_eq!(sequence.audio_program.musical_time_reference, reference);
        assert_eq!(sequence.audio_program.note_lanes, score);
        assert_eq!(sequence.audio_program.routes, routes);
        assert!(sequence.audio_program.sidechain_routes.is_empty());
        let programs = [NativeAudioAuthorProgram {
            sequence: sequence.id,
            program: Arc::new(
                crate::compile_audio_program(sequence, AudioCompileRequest::program(output))
                    .expect("same canonical output and Note score"),
            ),
        }];
        assert_eq!(programs[0].program.musical_time_reference(), reference);
        let mut before = Cursors::default();
        runtime.visit_continuity_evidence(&mut before).unwrap();
        let (registration, destination) = source.register().expect("same Source registration");
        let current_claim = source.current_claim().unwrap().expect("actual installed Source claim");
        let reservation = source.reserve_install(&registration, Some(&current_claim)).unwrap();
        let mut authorization = source.authorize_runtime_installation(reservation).unwrap();
        let mut prepared = runtime
            .prepare_native_author_parameter_handoff(&programs, &proposal, destination, &cancel)
            .expect("original grant and exact native-author handoff");
        let installed = runtime
            .install_native_author_parameter_handoff(&mut prepared, &mut authorization, &cancel)
            .expect("actual Plan/Source installation");
        assert_eq!(
            runtime
                .find_resource_session(runtime.resource_root_identity())
                .unwrap()
                .resource_plan()
                .program()
                .musical_time_reference(),
            reference
        );
        assert_eq!(
            installed.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        assert_eq!(installed.target.continuity_epoch, target.continuity_epoch);
        assert!(installed.claim.generation() > current_claim.generation());
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            installed.claim.generation()
        );
        drop(prepared);
        let acknowledged = runtime
            .acknowledge_native_author_parameter_handoff(&proposal, &cancel)
            .expect("Ack only after actual canonical installation");
        assert_eq!(
            acknowledged.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        assert_eq!(acknowledged.target, installed.target);
        assert_eq!(
            runtime
                .find_resource_session(runtime.resource_root_identity())
                .unwrap()
                .resource_plan()
                .program()
                .musical_time_reference(),
            reference
        );
        assert_eq!(
            acknowledged.claim.generation(),
            installed.claim.generation()
        );
        let fresh = owner(runtime);
        assert_eq!(fresh.editor_target(), acknowledged.target);
        assert_eq!(
            acknowledged.target.continuity_epoch,
            target.continuity_epoch
        );
        assert_eq!(
            runtime.observe_native_owner_object(fresh).unwrap(),
            expected_object
        );
        let mut after = Cursors::default();
        runtime.visit_continuity_evidence(&mut after).unwrap();
        assert_eq!(before.sessions.len(), after.sessions.len());
        assert_eq!(before.processors.len(), after.processors.len());
        for (before, after) in before.sessions.iter().zip(&after.sessions) {
            assert_eq!(
                before.state, after.state,
                "handoff must not Enter or advance Session"
            );
            assert_eq!(before.sample_rate, after.sample_rate);
            assert_eq!(
                before.public_output_lookahead_frames,
                after.public_output_lookahead_frames
            );
        }
        for (before, after) in before.processors.iter().zip(&after.processors) {
            assert_eq!(before.occurrence, after.occurrence);
            assert_eq!(
                before.state, after.state,
                "handoff must preserve the actual native input prefix"
            );
        }
        assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
        if scanned > 0 {
            rescan_handoffs += 1;
        }
    }
    panic!("native Source did not close within four actual proposal polls");
}

#[test]
#[ignore = "requires actual outer owner Job, current normal MONDRIAN_CLAP_TEST_HELPER and real MONDRIAN_SURGE_XT_CLAP_DLL"]
fn external_surge_xt_clap_captured_state_notes_main_runtime_without_media() {
    run_captured_state_notes_main_runtime(None);
}

#[test]
#[ignore = "requires actual outer owner Job, current normal MONDRIAN_CLAP_TEST_HELPER and real MONDRIAN_SURGE_XT_CLAP_DLL"]
fn external_surge_xt_clap_captured_state_notes_musical_runtime_without_media() {
    run_captured_state_notes_main_runtime(Some(
        MusicalTimeReference::new(
            TimelineTime::new(-19, 7).unwrap(),
            TimelineTime::new(3, 4).unwrap(),
        )
        .unwrap(),
    ));
}

#[test]
#[ignore = "requires outer native Job, current IPC21 Product and real Surge XT CLAP; authored 7/8 crosses a bar during the held Note"]
fn external_surge_xt_clap_captured_state_notes_constant_meter_runtime_without_media() {
    // At 48k/120 BPM, quarter3.25 at sample0 reaches bar1 at sample6000,
    // inside the original held Note127..8193 and complete16384-frame gate.
    run_captured_state_notes_main_runtime(Some(
        MusicalTimeReference::new(
            TimelineTime::new(-13, 8).unwrap(),
            TimelineTime::new(1, 2).unwrap(),
        )
        .unwrap()
        .with_time_signature(Some(mondrian_core::MusicalTimeSignature::new(7, 8).unwrap())),
    ));
}

fn run_captured_state_notes_main_runtime(reference: Option<MusicalTimeReference>) {
    let mut in_job = 0;
    // SAFETY: the current-process pseudo handle is valid; the BOOL output lives
    // for this call. Membership is a guard, not proof of the outer Job identity.
    let queried = unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) };
    assert_ne!(queried, 0, "query actual outer Job membership");
    assert_ne!(in_job, 0, "outer owner Job required before native creation");
    let helper = PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("current normal Product helper"),
    );
    let library = PathBuf::from(
        std::env::var_os("MONDRIAN_SURGE_XT_CLAP_DLL")
            .expect("explicit real Surge XT instrument library"),
    );
    assert!(helper.is_absolute() && helper.is_file());
    assert!(library.is_absolute() && library.is_file());
    let library = std::fs::canonicalize(library).unwrap();
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 64 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4_096,
        compensation_delay_scratch_budget_bytes: 64 * 1024 * 1024,
    };
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(helper.clone(), [library.clone()])
            .expect("actual isolated Surge descriptor discovery"),
    );
    let descriptor = catalog
        .descriptors()
        .into_iter()
        .find(|p| p.plugin_id == ID)
        .expect("actual Surge XT instrument descriptor")
        .clone();
    println!(
        "{}",
        json!({"case":"surge-xt-clap-main-runtime", "stage":"descriptor", "plugin_id":descriptor.plugin_id, "plugin_name":descriptor.name, "plugin_version":descriptor.version, "rate":render.sample_rate, "block":BLOCK, "mode":"Offline"})
    );
    let (captured, state) =
        probe_clap_plugin_registration_with_state(&helper, &library, ID, render, None, true)
            .expect(
            "actual default Surge state capture; original bounds and failures remain authoritative",
        );
    let state = state.expect("Surge must expose actual state capture");
    assert!(!state.is_empty() && state.len() <= 64 * 1024);
    if reference.is_some() {
        assert_eq!(
            state.len(),
            50_208,
            "same real Surge XT captured default state"
        );
    }
    let offer = probe_clap_note_processing_offer(&helper, &library, ID, render, Some(&state))
        .expect("same restored-instance NoteOffer5, no inferred Note dialect or port");
    assert_eq!(offer.schema_version, 6);
    assert_eq!(offer.notes.schema_version, 2);
    assert_eq!(
        offer.notes.contract.completion,
        crate::AudioNoteCompletionPolicy::NativeEnd
    );
    assert!(offer.notes.contract.ports.iter().all(|port| port.channel_count == 16));
    assert_eq!(offer.notes.plugin_id, ID);
    assert_eq!(offer.main_audio_input(), ClapMainAudioInput::Absent);
    assert_eq!(captured.main_audio_input, ClapMainAudioInput::Absent);
    assert_eq!(offer.main_audio_output_id(), captured.main_audio_output_id);
    assert_eq!(offer.auxiliary_outputs(), captured.auxiliary_outputs);
    assert_eq!(
        captured.auxiliary_outputs.buses.len(),
        2,
        "all three native outputs must be prepared"
    );
    assert!(captured
        .auxiliary_outputs
        .buses
        .iter()
        .all(|p| p.channel_layout == AudioChannelLayout::Stereo));
    assert_eq!(
        captured.auxiliary_inputs.buses.len(),
        1,
        "actual non-Main stereo input remains a zero-filled auxiliary input"
    );
    assert_eq!(
        offer.auxiliary_inputs().collect::<Vec<_>>(),
        captured
            .auxiliary_inputs
            .buses
            .iter()
            .map(|p| (p.bus_key.as_str(), p.channel_layout))
            .collect::<Vec<_>>()
    );
    assert!(captured
        .auxiliary_inputs
        .buses
        .iter()
        .all(|p| p.channel_layout == AudioChannelLayout::Stereo));
    assert_eq!(offer.processing_mode(), AudioProcessingMode::Offline);
    assert_eq!(offer.notes.sample_rate, render.sample_rate);
    assert_eq!(offer.notes.max_block_frames, BLOCK);
    assert_eq!(offer.notes.channel_layout, render.channel_layout);
    assert!(offer.notes.offline);
    assert!(
        (OFF + i64::from(offer.latency_frames()) + (BLOCK as i64)) < (TOTAL as i64),
        "Off must precede the observed finite tail window after actual latency"
    );
    let note_port = offer
        .notes
        .contract
        .ports
        .first()
        .expect("actual negotiated native CLAP Note input")
        .clone();
    println!(
        "{}",
        json!({"case":"surge-xt-clap-main-runtime", "stage":"captured_offer", "state_bytes":state.len(), "note_port_key":note_port.port_key, "note_native_id":note_port.native_id, "note_native_index":note_port.native_index, "main_output_id":offer.main_audio_output_id(), "auxiliary_outputs":outputs_to_wire(&captured.auxiliary_outputs), "auxiliary_inputs":auxiliary_to_wire(&captured.auxiliary_inputs), "latency_frames":offer.latency_frames(), "tail":format!("{:?}",offer.tail())})
    );
    let state_bytes = state.len();
    let instance = catalog
        .create_instance(ID, render, Some(state))
        .expect("canonical author instance must restore the captured Surge state");
    assert_eq!(instance.opaque_state.as_ref().unwrap().len(), state_bytes);
    let processor_id = instance.id;
    let mut sequence = Sequence::new("real Surge XT Note/Main Runtime control");
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
    let time = |sample| TimelineTime::new(sample, i64::from(render.sample_rate)).unwrap();
    sequence.audio_program.note_lanes.push(AudioNoteLane {
        schema_version: 1,
        id: AudioNoteLaneId::new(),
        target: AudioNoteTarget {
            track_id: track,
            processor_id,
            input_port_key: note_port.port_key,
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
    let selected = crate::compile_audio_program(&sequence, AudioCompileRequest::program(output))
        .expect("same canonical selected Signal Closure");
    assert_eq!(selected.musical_time_reference(), reference);
    assert_eq!(
        selected.note_lanes.len(),
        1,
        "Main routing must retain the actual Note consumer"
    );
    assert_eq!(selected.note_lanes[0].notes.len(), 1);
    assert_eq!(selected.note_lanes[0].notes[0].id, note_id);
    assert_eq!(selected.note_lanes[0].notes[0].end, time(OFF));
    const {
        assert!(ON > 0 && OFF > ON && OFF < TOTAL as i64 && OFF % BLOCK as i64 != 0);
    }
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
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
    )
    .expect("canonical real instrument Runtime including original validation shutdown");
    assert_eq!(
        runtime
            .find_resource_session(runtime.resource_root_identity())
            .unwrap()
            .resource_plan()
            .program()
            .musical_time_reference(),
        reference
    );
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source
        .reserve_install(&registration, None)
        .and_then(|r| source.begin_commit(r))
        .unwrap()
        .commit()
        .unwrap();
    let cancel = ExecutionCancellationToken::new();
    runtime
        .install_native_owner_bindings(&claim, &cancel)
        .expect("original Source installs the actual Runtime owner");
    // Cold installation must already be closed. Later genuine Rescan prefixes
    // use the canonical transaction/Source/Ack path; nothing is discarded.
    runtime
        .validate_native_author_source_closed(&cancel)
        .expect("cold native Source must already be serviced before state entry");
    let cold = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
    let pid = cold.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: acquire query/synchronize rights only. The public native lease
    // supplies PID; a fresh authenticated query below rejects an exited/replaced
    // source. Retain this acquired kernel handle through Drop; never reopen it.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    assert!(!raw.is_null(), "retain processing child observation handle");
    // SAFETY: OpenProcess returned one nonnull owned handle, adopted once.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
        cold
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
        .expect("actual instrument state entry, no seek/reset substitute");
    let (mut author_handoffs, mut rescan_handoffs) = accept_actual_rescans(
        &mut runtime,
        &mut sequence,
        &source,
        cold,
        reference,
        1024,
        64 * 1024,
    );
    let mut energy = 0.0_f64;
    let mut peak = 0.0_f32;
    for (completed_blocks, start) in (0..TOTAL).step_by(BLOCK).enumerate() {
        let mut pcm = vec![f32::NAN; BLOCK * render.channel_count()];
        if let Err(error) = runtime.render_into(
            AudioRenderRequest { transport_activity: None, start_sample: start as i64, frames: BLOCK },
            &mut pcm,
        ) {
            println!(
                "{}",
                json!({"case":"surge-xt-clap-main-runtime", "stage":"render_failed", "start_sample":start, "completed_blocks":completed_blocks, "error":format!("{error:?}")})
            );
            // A single failure-only observation retains the Runtime's actual
            // outstanding proposal. No opaque bytes are printed, no Ack is
            // constructed, and no subsequent render is attempted.
            match runtime.poll_native_observations(&cancel) {
                Ok(batch) => {
                    let proposal = batch.as_ref().and_then(|batch| batch.author.as_ref()).map(
                        |proposal| {
                            use mondrian_core::NativeAudioAuthorCallback as Callback;
                            let callbacks = proposal.callbacks().iter().map(|callback| {
                                match callback {
                                    Callback::Parameter { parameter, phase, value_bits } => json!({
                                        "kind":"Parameter", "parameter":parameter.as_str(),
                                        "phase":format!("{phase:?}"), "value_bits":value_bits,
                                    }),
                                    Callback::ParameterAtExecution { parameter, phase, value_bits, position } => json!({
                                        "kind":"ParameterAtExecution", "parameter":parameter.as_str(),
                                        "phase":format!("{phase:?}"), "value_bits":value_bits,
                                        "position":format!("{position:?}"),
                                    }),
                                    Callback::ParameterValuesRescanned { kind, values } => json!({
                                        "kind":"ParameterValuesRescanned", "rescan_kind":format!("{kind:?}"),
                                        "value_count":values.len(),
                                    }),
                                    Callback::ParameterReferencesCleared { parameter, flags } => json!({
                                        "kind":"ParameterReferencesCleared", "parameter":parameter.as_str(),
                                        "flags":flags,
                                    }),
                                    Callback::DirtyChanged(dirty) => json!({"kind":"DirtyChanged", "dirty":dirty}),
                                    Callback::GroupStarted => json!({"kind":"GroupStarted"}),
                                    Callback::GroupFinished => json!({"kind":"GroupFinished"}),
                                }
                            }).collect::<Vec<_>>();
                            json!({
                                "revision":proposal.revision(), "target":format!("{:?}",proposal.target()),
                                "completion":format!("{:?}",proposal.completion()),
                                "open_parameter_edits":proposal.open_parameter_edits(),
                                "group_depth":proposal.group_depth(),
                                "state_bytes":proposal.state().map(|state| state.len()),
                                "callback_count":proposal.callbacks().len(), "callbacks":callbacks,
                            })
                        },
                    );
                    let parameters = batch.as_ref().and_then(|batch| batch.parameters.as_ref()).map(|parameters| json!({
                        "target":format!("{:?}",parameters.target()), "process_id":parameters.process_id().get(),
                        "object_serial":parameters.object_serial().get(), "source_revision":parameters.source_revision(),
                        "fact_count":parameters.facts().len(),
                    }));
                    let diagnostics = batch.as_ref().and_then(|batch| batch.diagnostics.as_ref()).map(|diagnostics| {
                        let loss = diagnostics.loss();
                        json!({"record_count":diagnostics.records().len(), "loss":{
                            "capacity":loss.capacity, "contention":loss.contention,
                            "sequence_exhausted":loss.sequence_exhausted, "incomplete":loss.incomplete(),
                        }})
                    });
                    println!(
                        "{}",
                        json!({"case":"surge-xt-clap-main-runtime", "stage":"failure_observation", "start_sample":start, "completed_blocks":completed_blocks, "batch_present":batch.is_some(), "proposal":proposal, "parameters":parameters, "diagnostics":diagnostics})
                    );
                }
                Err(poll_error) => println!(
                    "{}",
                    json!({"case":"surge-xt-clap-main-runtime", "stage":"failure_observation_failed", "start_sample":start, "completed_blocks":completed_blocks, "error":format!("{poll_error:?}")})
                ),
            }
            panic!("native Main block must complete through the canonical graph: {error:?}");
        }
        assert!(
            pcm.iter().all(|sample| sample.is_finite()),
            "all requested stereo PCM must be written and finite"
        );
        let (handoffs, rescans) = accept_actual_rescans(
            &mut runtime,
            &mut sequence,
            &source,
            cold,
            reference,
            1024,
            64 * 1024,
        );
        author_handoffs += handoffs;
        rescan_handoffs += rescans;
        for sample in pcm {
            energy += f64::from(sample) * f64::from(sample);
            peak = peak.max(sample.abs());
        }
    }
    assert!(
        energy.is_finite() && energy > 0.0 && peak > 0.0,
        "real captured-state instrument Main must produce finite nonzero energy"
    );
    assert!(
        rescan_handoffs > 0,
        "actual nonempty rescan must traverse canonical Source installation and Ack"
    );
    assert_eq!(media_resolves.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
    let mut cursors = Cursors::default();
    runtime.visit_continuity_evidence(&mut cursors).unwrap();
    assert_eq!(cursors.processors.len(), 1);
    let processed_through = match cursors.processors[0].state {
        AudioProcessorCursorState::Active { epoch, next_input_signal_sample } => {
            assert_eq!(epoch, AudioContinuityEpoch::new(1));
            next_input_signal_sample.sample()
        }
        other => {
            panic!("actual native processor must have a successful input-signal prefix: {other:?}")
        }
    };
    assert!(
        processed_through > OFF,
        "actual DSP input prefix must cross the authored Off, not stop at the score boundary"
    );
    assert_eq!(
        runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
        cold
    );
    runtime
        .validate_native_author_source_closed(&cancel)
        .expect("native author requests remain visible after real processing");
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        WAIT_TIMEOUT
    );
    drop(runtime);
    // SAFETY: this same owned query/synchronize handle stays live throughout;
    // zero-wait observes original Drop only, without extending its shutdown budget.
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
    println!(
        "{}",
        json!({"case":"surge-xt-clap-main-runtime", "stage":"completed", "musical_reference":reference, "plugin_id":descriptor.plugin_id, "plugin_name":descriptor.name, "plugin_version":descriptor.version, "state_bytes":state_bytes, "rate":render.sample_rate, "block":BLOCK, "frames":TOTAL, "author_handoffs":author_handoffs, "nonempty_rescan_handoffs":rescan_handoffs, "channels":2, "mode":"Offline", "note_on":ON, "note_off":OFF, "next_input_signal_sample":processed_through, "main_energy":energy, "main_peak":peak, "media_resolves":media_resolves.load(Ordering::SeqCst), "native_pid":pid, "post_drop_wait_status":settled, "exit_query_succeeded":queried!=0, "exit_code":exit_code, "explicit_unbounded_runtime_grant":true, "auxiliary_output_sound_qualified":false, "algorithm_or_note_release_qualified":false, "terminal_off_qualified":false, "graceful_ack_qualified":false, "realtime_qualified":false})
    );
    assert_eq!(
        settled, WAIT_OBJECT_0,
        "real processing child must exit after original Drop"
    );
    assert_ne!(queried, 0);
    assert_eq!(exit_code, 0, "forced/error child exit cannot pass");
}
