//! Future owned Windows adapter gate, not third-party/IPC/whole-host evidence.
//!
//! Place as a clap_worker child test module after the production seams in
//! DELIVERY.md exist. The current explicit unsupported guard must make enabling
//! these ignored controls fail; never remove that guard solely to run this test.
#![cfg(windows)]

use super::super::protocol::{SharedNoteEnd, SharedNoteEvent, SharedParameterEvent};
use super::*;
use crate::{
    AudioNoteEndEvent, AudioNoteEventKind, AudioNoteExecutionLimits, AudioProcessingMode,
    AudioProcessorNoteInputContract, AudioProcessorNoteInputPort, AudioProcessorTail,
    AudioRenderContract, NativeAudioObservationConsumption,
};
use bytemuck::Zeroable;
use mondrian_core::{AudioChannelLayout, ParameterId};
use std::cell::Cell;
use std::path::Path;

const DLL_ENV: &str = "MONDRIAN_INTERNAL_NOTE_GATE_DLL";
const DLL_PATH: &str = "E:/codex-artifact-storage/mondrian-3349-plugin-host-sprint/clap-note-gate-build-target-r2/debug/mondrian_clap_note_gate_fixture.dll";
const DLL_SHA256: &str = "B239C594F9547EE3A9F3FA107622A6FE663AAF73425E11248BDB1E0A0F32AF1D";
const DLL_BYTES: u64 = 214_528;
const NOTE_IDS: [u32; 2] = [0, 0x8000_0017];

fn pinned_dll() -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os(DLL_ENV).expect("explicit pinned owned DLL env required"));
    assert!(path.is_absolute());
    assert_eq!(
        std::fs::canonicalize(&path).expect("existing env DLL"),
        std::fs::canonicalize(Path::new(DLL_PATH)).expect("accepted producer DLL")
    );
    assert_eq!(
        std::fs::metadata(&path).expect("DLL extent").len(),
        DLL_BYTES
    );
    let hash = fingerprint_clap_binary(&path).expect("actual DLL bytes");
    let hex: String = hash.iter().map(|byte| format!("{byte:02X}")).collect();
    assert_eq!(
        hex, DLL_SHA256,
        "this control accepts only the source6461/B239 producer"
    );
    path
}

fn render() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 14,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}

fn requested_notes() -> AudioProcessorNoteInputContract {
    AudioProcessorNoteInputContract {
        completion: crate::AudioNoteCompletionPolicy::NativeEnd,
        ports: NOTE_IDS
            .into_iter()
            .enumerate()
            .map(|(index, id)| AudioProcessorNoteInputPort {
                channel_count: 16,
                port_key: format!("clap.note-input.{id}"),
                native_id: id,
                native_index: index as u16,
            })
            .collect(),
        // Deliberately small offered storage: concrete fixture has eight voices.
        // This request is not proof; production must requery and match actual
        // restored ports and independently admit these limits before activation.
        limits: AudioNoteExecutionLimits {
            input_events: 8,
            output_ends: 2,
            retained_records: 8,
            live_voices: 8,
        },
    }
}

fn parameter_definitions() -> Vec<super::super::ClapParameterDescriptor> {
    [
        (17, "Gain", 1.0, 1.0, false, false),
        (19, "End scenario", 7.0, 0.0, true, false),
        (23, "Exact Off count", f64::from(u32::MAX), 0.0, true, true),
    ]
    .into_iter()
    .map(|(id, name, max_value, default_value, stepped, read_only)| {
        super::super::ClapParameterDescriptor {
            id,
            name: name.to_owned(),
            min_value: 0.0,
            max_value,
            default_value,
            current_value: default_value,
            automatable: !read_only,
            stepped,
            enumeration: false,
            read_only,
            hidden: false,
        }
    })
    .collect()
}

fn prepare_gate() -> Box<dyn IsolatedAudioProcessorWorker> {
    let path = pinned_dll();
    // SAFETY: this optional control loads only the original owned, accepted
    // source6461/B239 fixture pinned above, through the real SDK loader.
    let entry = unsafe { PluginEntry::load(&path) }.expect("actual dynamic PluginEntry load");
    assert_eq!(
        pinned_dll(),
        path,
        "producer bytes remain fixed after loading"
    );
    let notes = requested_notes();
    notes.validate().expect("valid requested extent, not native capability");
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
        auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
        native_lane: None,
        payload: Vec::new(),
        plugin_execution_contract: AudioProcessorExecutionContract::new(
            0,
            AudioProcessorTail::Finite(4),
            true,
            true,
            true,
            0,
        )
        .expect("fixture lifecycle contract"),
        auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
        note_input_contract: notes.clone(), // protocol candidate's exact request field
        parameter_ids: vec![
            ParameterId::new("clap.param.17").unwrap(),
            ParameterId::new("clap.param.19").unwrap(),
        ],
        render_contract: render(),
    };
    // Production preparation restores this actual object, checks native ports
    // and HostNotePorts capability, and reserves the real typed/IPC layouts.
    // Current guard must refuse; do not turn Err into a skip or empty success.
    let native_main =
        std::rc::Rc::new(vst3_host::window::WindowsEditorMainThread::initialize().unwrap());
    let mut worker = prepare_loaded_on_main(
        native_main,
        request,
        entry,
        c"org.mondrian.fixture.note-gate",
        None,
        ClapMainAudioInput::Present,
        0x201, // Actual fixed owned ABI output ID, independent of input ID0x101.
        &parameter_definitions(),
        &[17, 19],
    )
    .expect("BLOCKED until production restored-instance Note contract/capability is implemented");
    assert_eq!(
        worker.note_input_contract(),
        notes,
        "a metadata-only result or requested contract cannot substitute actual preparation"
    );
    worker.enter_state(0).expect("one fresh native continuity epoch");
    worker
}

#[derive(Clone, Copy)]
enum Declared {
    Param {
        at: u32,
        lane: u32,
        value: f64,
    },
    Note {
        at: u32,
        kind: AudioNoteEventKind,
        port: i16,
        channel: i16,
        key: i16,
        id: i32,
        velocity: f64,
    },
}
impl Declared {
    fn at(self) -> u32 {
        match self {
            Self::Param { at, .. } | Self::Note { at, .. } => at,
        }
    }
}

fn on(at: u32, port: i16, channel: i16, key: i16, id: i32, velocity: f64) -> Declared {
    Declared::Note {
        at,
        kind: AudioNoteEventKind::On,
        port,
        channel,
        key,
        id,
        velocity,
    }
}
fn off(at: u32, port: i16, channel: i16, key: i16, id: i32, velocity: f64) -> Declared {
    Declared::Note {
        at,
        kind: AudioNoteEventKind::Off,
        port,
        channel,
        key,
        id,
        velocity,
    }
}
fn end(id: i32, port: i16, channel: i16, key: i16) -> AudioNoteEndEvent {
    AudioNoteEndEvent {
        native_note_id: id,
        port_index: port,
        channel,
        key,
        flags: 0,
    }
}

/// All expected output values/termination windows are independent literal
/// dyadics. Event encoding below is input only, never an expected-output oracle.
struct Case<'a> {
    declared: &'a [Declared],
    expected_pcm: &'a [f32],
    expected_ends: &'a [(u32, AudioNoteEndEvent)],
    split: &'a [u32],
}

fn run_blocks(
    case: &Case<'_>,
    blocks: &[u32],
    mut process: impl for<'b> FnMut(
        IsolatedAudioProcessorWorkerBlock<'b>,
    ) -> Result<(), AudioProcessorHostError>,
) {
    assert_eq!(
        blocks.iter().map(|count| *count as usize).sum::<usize>(),
        case.expected_pcm.len()
    );
    let contract = requested_notes();
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    let parameter_ids = [
        ParameterId::new("clap.param.17").unwrap(),
        ParameterId::new("clap.param.19").unwrap(),
    ];
    let mut actual_pcm = [0.0_f32; 14];
    let mut start = 0_u32;
    let mut delivered = 0_usize;
    let mut observed_ends = 0_usize;
    for &frames in blocks {
        assert!((1..=14).contains(&frames));
        let window_end = start + frames;
        let mut pcm = [1.0_f32; 14];
        let mut parameters = [SharedParameterEvent { lane: 0, sample_offset: 0, value: 0.0 }; 8];
        let mut notes = [SharedNoteEvent::zeroed(); 8];
        let mut note_count = 0;
        let mut parameter_count = 0;
        for &event in case.declared {
            if !(start..window_end).contains(&event.at()) {
                continue;
            }
            delivered += 1;
            match event {
                Declared::Param { at, lane, value } => {
                    parameters[parameter_count] =
                        SharedParameterEvent { lane, sample_offset: at - start, value };
                    parameter_count += 1;
                }
                Declared::Note { at, kind, port, channel, key, id, velocity } => {
                    notes[note_count] = SharedNoteEvent {
                        sample_offset: at - start,
                        native_id: id,
                        port_index: port,
                        channel,
                        key,
                        kind: if kind == AudioNoteEventKind::Off {
                            0
                        } else {
                            1
                        },
                        velocity,
                    };
                    note_count += 1;
                }
            }
        }
        let mut ends = [SharedNoteEnd::zeroed(); 2];
        let end_count = Cell::new(0);
        let refused = Cell::new(false);
        let block = IsolatedAudioProcessorWorkerBlock::new(
            i64::from(start),
            frames as usize,
            render(),
            &mut pcm[..frames as usize],
            &auxiliary,
            &[],
            &parameter_ids,
            &parameters[..parameter_count],
        )
        .with_note_regions(
            &contract,
            &notes[..note_count],
            &mut ends,
            &end_count,
            &refused,
        );
        process(block)
            .expect("actual native ABI processing, including Param-End(MAX)-Param acceptance");
        assert!(
            !refused.get(),
            "ignored output false must fault, not publish PCM/End success"
        );
        let expected: Vec<_> = case
            .expected_ends
            .iter()
            .filter(|(after_sample, _)| (start..window_end).contains(after_sample))
            .map(|(_, tuple)| *tuple)
            .collect();
        let copied: Vec<_> = ends[..end_count.get()]
            .iter()
            .map(|event| event.realize(contract.ports.len()).expect("actual copied tuple"))
            .collect();
        assert_eq!(
            copied, expected,
            "only this completed native operation supplies End observations"
        );
        observed_ends += copied.len();
        actual_pcm[start as usize..window_end as usize].copy_from_slice(&pcm[..frames as usize]);
        start = window_end;
    }
    assert_eq!(
        delivered,
        case.declared.len(),
        "each declared native event reaches one window"
    );
    assert_eq!(observed_ends, 2);
    assert_eq!(
        &actual_pcm[..case.expected_pcm.len()],
        case.expected_pcm,
        "literal independently reviewed dyadic PCM, never Gate/lowering output"
    );
}

#[derive(Clone, Copy)]
enum Route {
    Inline,
    BorrowedDspRole,
}

fn run_partition(case: &Case<'_>, blocks: &[u32], route: Route) {
    let mut worker = prepare_gate();
    match route {
        Route::Inline => run_blocks(case, blocks, |block| worker.process(block)),
        Route::BorrowedDspRole => {
            assert!(worker.uses_native_processing_split());
            let split = worker.split_native_processing().expect("same native object roles");
            let mut main = split.main;
            let mut processing = split.processing;
            std::thread::scope(|scope| {
                let dsp = scope
                    .spawn(move || run_blocks(case, blocks, |block| processing.process(block)));
                // Fixture has no GUI or main wait. This bounded main tick is
                // real service, but not with_processing_scope/IPC scheduling.
                main.service_native_main_thread().expect("actual owning main thread");
                dsp.join().expect("same instance DSP role joined");
            });
            main.finish_processing_scope().expect("real main-role finish after join");
        }
    }
    let observations = worker
        .poll_native_observations(NativeAudioObservationConsumption::Preserve)
        .expect("non-consuming source observation after actual native return/join");
    let source = observations.author.expect("existing actual parameter source").source;
    assert_eq!(
        source.pending_events(),
        4,
        "two retained Gain values around each of the two actual End callbacks"
    );
    assert_eq!(
        source.revision(),
        4,
        "Ends never reserve parameter source order"
    );
    assert_eq!(source.failure(), None);
    assert!(!source.publishing());
    // No Drain/Ack, author commit, source retirement or fake Session approval.
    drop(worker);
    pinned_dll();
}

fn matrix(route: Route) {
    let normal = [
        on(2, 0, 0, 60, 10, 0.0),
        on(2, 1, 0, 60, 11, 1.0),
        Declared::Param { at: 4, lane: 0, value: 0.5 },
        off(5, 0, 0, 60, 10, 0.25),
        off(9, 1, 0, 60, 11, 1.0),
    ];
    let normal_pcm = [
        0.0,
        0.0,
        5.0 / 32.0,
        5.0 / 32.0,
        5.0 / 64.0,
        5.0 / 64.0,
        19.0 / 256.0,
        9.0 / 128.0,
        17.0 / 256.0,
        1.0 / 16.0,
        3.0 / 64.0,
        1.0 / 32.0,
        1.0 / 64.0,
        0.0,
    ];
    let same_key = [
        on(0, 0, 0, 60, 20, 0.0),
        on(0, 0, 0, 60, 21, 1.0),
        off(1, 0, 0, 60, 20, 0.0),
        off(6, 0, 0, 60, 21, 0.0),
    ];
    let same_pcm = [
        3.0 / 32.0,
        3.0 / 32.0,
        11.0 / 128.0,
        5.0 / 64.0,
        9.0 / 128.0,
        1.0 / 16.0,
        1.0 / 16.0,
        3.0 / 64.0,
        1.0 / 32.0,
        1.0 / 64.0,
        0.0,
    ];
    let early = [
        Declared::Param { at: 0, lane: 0, value: 0.25 },
        Declared::Param { at: 0, lane: 1, value: 1.0 },
        on(0, 1, 3, 61, 30, 0.5),
        Declared::Param { at: 2, lane: 0, value: 0.5 },
        Declared::Param { at: 3, lane: 1, value: 0.0 },
        off(4, 1, 3, 61, 30, 0.0),
        on(4, 0, 0, 60, 31, 1.0),
        off(5, 0, 0, 60, 31, 0.0),
    ];
    let early_pcm = [
        3.0 / 128.0,
        3.0 / 128.0,
        0.0,
        0.0,
        1.0 / 32.0,
        1.0 / 32.0,
        3.0 / 128.0,
        1.0 / 64.0,
        1.0 / 128.0,
        0.0,
    ];
    let cases = [
        Case {
            declared: &normal,
            expected_pcm: &normal_pcm,
            expected_ends: &[(8, end(10, 0, 0, 60)), (12, end(11, 1, 0, 60))],
            split: &[3, 2, 4, 5],
        },
        Case {
            declared: &same_key,
            expected_pcm: &same_pcm,
            expected_ends: &[(4, end(20, 0, 0, 60)), (9, end(21, 0, 0, 60))],
            split: &[2, 3, 6],
        },
        Case {
            declared: &early,
            expected_pcm: &early_pcm,
            expected_ends: &[(1, end(30, 1, 3, 61)), (8, end(31, 0, 0, 60))],
            split: &[1, 1, 3, 5],
        },
    ];
    for case in &cases {
        run_partition(case, &[case.expected_pcm.len() as u32], route);
        run_partition(case, case.split, route);
        run_partition(case, &[1; 14][..case.expected_pcm.len()], route);
    }
}

#[test]
#[ignore = "BLOCKED: Root must qualify actual restored note ports/HostNotePorts; explicit pinned B239 DLL env"]
fn owned_windows_dynamic_note_gate_inline_literal_pcm_matrix() {
    matrix(Route::Inline);
}

#[test]
#[ignore = "BLOCKED: production Note capability; direct borrowed DSP role is adapter-only, not IPC/scope qualification"]
fn owned_windows_dynamic_note_gate_borrowed_dsp_literal_pcm_matrix() {
    matrix(Route::BorrowedDspRole);
}

#[test]
#[ignore = "BLOCKED: production Notes capability; original early-ended Off must still reach native ABI"]
fn owned_windows_dynamic_note_gate_early_end_wrong_original_off_is_native_process_error() {
    let mut worker = prepare_gate();
    let contract = requested_notes();
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    let ids = [
        ParameterId::new("clap.param.17").unwrap(),
        ParameterId::new("clap.param.19").unwrap(),
    ];
    let parameters = [
        SharedParameterEvent { lane: 0, sample_offset: 0, value: 0.25 },
        SharedParameterEvent { lane: 1, sample_offset: 0, value: 1.0 },
        SharedParameterEvent { lane: 0, sample_offset: 2, value: 0.5 },
        SharedParameterEvent { lane: 1, sample_offset: 3, value: 0.0 },
    ];
    let notes = [
        SharedNoteEvent {
            sample_offset: 0,
            native_id: 30,
            port_index: 1,
            channel: 3,
            key: 61,
            kind: 1,
            velocity: 0.5,
        },
        // Valid scalar index but not the original On's input port. The fixture
        // retains its early-ended pairing until Off and rejects this native call.
        SharedNoteEvent {
            sample_offset: 4,
            native_id: 30,
            port_index: 0,
            channel: 3,
            key: 61,
            kind: 0,
            velocity: 0.0,
        },
        SharedNoteEvent {
            sample_offset: 4,
            native_id: 31,
            port_index: 0,
            channel: 0,
            key: 60,
            kind: 1,
            velocity: 1.0,
        },
        SharedNoteEvent {
            sample_offset: 5,
            native_id: 31,
            port_index: 0,
            channel: 0,
            key: 60,
            kind: 0,
            velocity: 0.0,
        },
    ];
    let mut pcm = [1.0_f32; 10];
    let mut ends = [SharedNoteEnd::zeroed(); 2];
    let count = Cell::new(0);
    let refused = Cell::new(false);
    let block = IsolatedAudioProcessorWorkerBlock::new(
        0,
        10,
        render(),
        &mut pcm,
        &auxiliary,
        &[],
        &ids,
        &parameters,
    )
    .with_note_regions(&contract, &notes, &mut ends, &count, &refused);
    let error = worker
        .process(block)
        .expect_err("Off must not be silently dropped after early End");
    assert!(
        matches!(error, AudioProcessorHostError::Process(ref detail)
        if detail.contains("CLAP process failed")),
        "expected actual native process refusal: {error:?}"
    );
    // Failed direct adapter PCM is not a successful root/IPC publication. No
    // completion credit, author Ack or partial-output acceptance is inferred.
}

// Append to the existing Windows native_clap_note_gate child module so its
// exact pinned_dll()/DLL_ENV/DLL_PATH/B239 bytes gate remains the single source.
// This optional control qualifies coherent child/parent metadata query only.
#[test]
#[ignore = "requires normal Product helper and the explicit fixed owned B239 DLL"]
fn native_owned_note_gate_v2_parent_query_binds_complete_same_instance_facts() {
    let helper = PathBuf::from(
        std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .expect("set MONDRIAN_CLAP_TEST_HELPER to the newly built normal Product"),
    );
    assert!(helper.is_absolute());
    assert!(std::fs::metadata(&helper).unwrap().is_file());
    let path = pinned_dll();
    let mut state = [0_u8; 16];
    state[..4].copy_from_slice(b"MNG1");
    state[4..12].copy_from_slice(&0.5_f64.to_le_bytes());
    state[12..].copy_from_slice(&2_u32.to_le_bytes());
    for opaque in [None, Some(state.as_slice())] {
        let offer = super::super::clap_discovery::probe_clap_note_processing_offer(
            &helper,
            &path,
            "org.mondrian.fixture.note-gate",
            render(),
            opaque,
        )
        .expect("actual schema5 normal helper, full capture and parent decoder");
        assert_eq!(
            pinned_dll(),
            path,
            "producer bytes remain accepted after query"
        );
        assert_eq!(offer.schema_version, 5);
        assert_eq!(offer.notes.schema_version, 2);
        assert_eq!(
            offer.notes.contract.completion,
            crate::AudioNoteCompletionPolicy::NativeEnd
        );
        assert!(offer.notes.contract.ports.iter().all(|port| port.channel_count == 16));
        assert_eq!(offer.notes.plugin_id, "org.mondrian.fixture.note-gate");
        assert_eq!(
            offer.notes.binary_sha256,
            fingerprint_clap_binary(&path).unwrap()
        );
        assert_eq!(
            offer.notes.state_sha256,
            super::super::clap_note_input_metadata::state_digest(opaque)
        );
        assert_eq!(offer.notes.sample_rate, 48_000);
        assert_eq!(offer.notes.channel_layout, AudioChannelLayout::Mono);
        assert_eq!(offer.notes.max_block_frames, 14);
        assert!(offer.notes.offline);
        let standalone = super::super::clap_discovery::probe_clap_note_input_contract(
            &helper,
            &path,
            "org.mondrian.fixture.note-gate",
            render(),
            opaque,
        )
        .expect("actual independent NoteProbe2 request and response");
        assert_eq!(standalone.schema_version, 2);
        assert_eq!(standalone.plugin_id, offer.notes.plugin_id);
        assert_eq!(standalone.binary_sha256, offer.notes.binary_sha256);
        assert_eq!(standalone.state_sha256, offer.notes.state_sha256);
        assert_eq!(standalone.sample_rate, offer.notes.sample_rate);
        assert_eq!(standalone.channel_layout, offer.notes.channel_layout);
        assert_eq!(standalone.max_block_frames, offer.notes.max_block_frames);
        assert_eq!(standalone.offline, offer.notes.offline);
        assert_eq!(standalone.contract, offer.notes.contract);
        assert_eq!(
            pinned_dll(),
            path,
            "standalone query preserves accepted producer bytes"
        );
        assert_eq!(
            offer.notes.contract.limits,
            AudioNoteExecutionLimits {
                input_events: crate::AUDIO_NOTE_EVENTS_PER_BLOCK,
                output_ends: crate::AUDIO_NOTE_ENDS_PER_BLOCK,
                retained_records: crate::AUDIO_NOTE_RECORDS,
                live_voices: crate::AUDIO_NOTE_RECORDS,
            }
        );
        assert_eq!(
            offer
                .notes
                .contract
                .ports
                .iter()
                .map(|port| (port.native_id, port.native_index, port.port_key.as_str()))
                .collect::<Vec<_>>(),
            [
                (0, 0, "clap.note-input.0"),
                (0x8000_0017, 1, "clap.note-input.2147483671")
            ]
        );
        let mut parameters = parameter_definitions();
        if opaque.is_some() {
            parameters[0].current_value = 0.5;
            parameters[1].current_value = 2.0;
        }
        assert_eq!(offer.parameters(), parameters.as_slice());
        assert_eq!(offer.auxiliary_inputs().len(), 0);
        assert_eq!(offer.latency_frames(), 0);
        assert_eq!(offer.tail(), AudioProcessorTail::Finite(4));
        assert_eq!(offer.processing_mode(), AudioProcessingMode::Offline);
        let facts = serde_json::to_value(&offer).unwrap();
        let processing = &facts["processing"];
        assert_eq!(processing["plugin_id"], "org.mondrian.fixture.note-gate");
        assert_eq!(processing["latency_frames"], 0);
        assert_eq!(processing["tail"], serde_json::json!({"Finite":4}));
        assert_eq!(processing["offline"], true);
        assert_eq!(
            processing["parameters"],
            serde_json::to_value(parameters).unwrap()
        );
        assert_eq!(processing["auxiliary_buses"], serde_json::json!([]));
        assert_eq!(processing["captured_state"], serde_json::Value::Null);
    }
}
