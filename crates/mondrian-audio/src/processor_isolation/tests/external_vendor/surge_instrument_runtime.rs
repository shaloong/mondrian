//! One real Surge XT CLAP state/Note/Main-output Runtime consumer.
//! Other outputs are prepared, never used as a sound or algorithm oracle.

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
    AudioRuntimeResourceGrant, AudioStateEntry, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource,
    AudioSourceComponentId, AuthoringList, ExecutionCancellationToken, TimelineTime,
};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
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

#[test]
#[ignore = "requires actual outer owner Job, current normal MONDRIAN_CLAP_TEST_HELPER and real MONDRIAN_SURGE_XT_CLAP_DLL"]
fn external_surge_xt_clap_captured_state_notes_main_runtime_without_media() {
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
    let offer = probe_clap_note_processing_offer(&helper, &library, ID, render, Some(&state))
        .expect("same restored-instance NoteOffer4, no inferred Note dialect or port");
    assert_eq!(offer.schema_version, 4);
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
    // No proposal is drained/discarded and no Ack is fabricated. An unresolved
    // native author source refuses this slice and remains an explicit failure.
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
    let mut energy = 0.0_f64;
    let mut peak = 0.0_f32;
    for start in (0..TOTAL).step_by(BLOCK) {
        let mut pcm = vec![f32::NAN; BLOCK * render.channel_count()];
        runtime
            .render_into(
                AudioRenderRequest { start_sample: start as i64, frames: BLOCK },
                &mut pcm,
            )
            .expect("native Main block must complete through the canonical graph");
        assert!(
            pcm.iter().all(|sample| sample.is_finite()),
            "all requested stereo PCM must be written and finite"
        );
        for sample in pcm {
            energy += f64::from(sample) * f64::from(sample);
            peak = peak.max(sample.abs());
        }
    }
    assert!(
        energy.is_finite() && energy > 0.0 && peak > 0.0,
        "real captured-state instrument Main must produce finite nonzero energy"
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
        json!({"case":"surge-xt-clap-main-runtime", "stage":"completed", "plugin_id":descriptor.plugin_id, "plugin_name":descriptor.name, "plugin_version":descriptor.version, "state_bytes":state_bytes, "rate":render.sample_rate, "block":BLOCK, "frames":TOTAL, "channels":2, "mode":"Offline", "note_on":ON, "note_off":OFF, "next_input_signal_sample":processed_through, "main_energy":energy, "main_peak":peak, "media_resolves":media_resolves.load(Ordering::SeqCst), "native_pid":pid, "post_drop_wait_status":settled, "exit_query_succeeded":queried!=0, "exit_code":exit_code, "explicit_unbounded_runtime_grant":true, "auxiliary_output_sound_qualified":false, "algorithm_or_note_release_qualified":false, "terminal_off_qualified":false, "graceful_ack_qualified":false, "realtime_qualified":false})
    );
    assert_eq!(
        settled, WAIT_OBJECT_0,
        "real processing child must exit after original Drop"
    );
    assert_ne!(queried, 0);
    assert_eq!(exit_code, 0, "forced/error child exit cannot pass");
}
