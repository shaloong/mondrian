//! Real SDK zero-native-audio-input qualification, independently from MNE4.
//! MNE6 is an explicit inactive-restored mode of the existing class. No Product,
//! canonical Note admission, third-party DSP, RT or full-heap claim is made.
#![cfg(windows)]

use std::path::PathBuf;
use vst3_host::audio::{
    AudioBusRole, BusAudioBuffers, BusDirection, MediaType, NativeEventBusRole,
    ProcessMusicalReference, ProcessTimelineContext,
};
use vst3_host::error::NativeEventError;
use vst3_host::midi::{PluginEvent, PluginEventData};
use vst3_host::plugin::ProcessMode;
use vst3_host::{Plugin, Vst3Host};

const CLASS: &str = "6E4F05A12D8B4C639F017B42A4C2E611";
const BLOCK: usize = 4;
const AUDIT_BOUND: usize = 2 * 1024 * 1024;

fn fixture() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN")
            .expect("explicit rebuilt owned MNE6 DLL"),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}
fn host(rate: u32) -> Vst3Host {
    Vst3Host::builder()
        .sample_rate(f64::from(rate))
        .block_size(BLOCK)
        // The class defaults to the original stereo-input MNE1. This does not
        // manufacture a zero-input plugin through a host channel-count hint.
        .input_channels(2)
        .output_channels(2)
        .build()
        .unwrap()
}
fn configuration(fail_call: u32) -> Vec<u8> {
    let mut component = b"MNE6".to_vec();
    component.extend_from_slice(&6_u32.to_le_bytes());
    component.extend_from_slice(&fail_call.to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for value in [1_u32, 12, 12] {
        state.extend_from_slice(&value.to_le_bytes());
    }
    state.extend_from_slice(&component);
    state.extend_from_slice(&component);
    state
}
fn open(host: &mut Vst3Host, fail_call: u32) -> Plugin {
    let mut plugin = host.load_plugin_class(fixture(), CLASS).unwrap();
    let default = plugin.native_audio_bus_info().unwrap();
    assert_eq!((default.inputs.len(), default.outputs.len()), (1, 1));
    assert_eq!(default.inputs[0].channel_count, 2);
    let state = configuration(fail_call);
    plugin.load_state(&state).expect("same class, actual inactive MNE6 restore");
    assert_eq!(plugin.save_state().unwrap(), state);

    let audio = plugin.native_audio_bus_info().unwrap();
    assert!(
        audio.inputs.is_empty(),
        "actual restored declaration, not cached PluginInfo"
    );
    assert_eq!(audio.outputs.len(), 1);
    assert_eq!(audio.outputs[0].role, AudioBusRole::Main);
    assert_eq!(audio.outputs[0].channel_count, 2);
    assert!(audio.outputs[0].default_active);
    assert!(plugin.set_bus_active(MediaType::Audio, BusDirection::Input, 0, true).is_err());
    assert!(plugin.native_audio_bus_info().unwrap().inputs.is_empty());
    let events = plugin.native_event_bus_info().unwrap();
    assert_eq!(events.inputs.len(), 2);
    assert!(events.outputs.is_empty());
    assert_eq!(events.inputs[0].role, NativeEventBusRole::Main);
    assert_eq!(events.inputs[1].role, NativeEventBusRole::Auxiliary);
    assert_eq!(
        events
            .inputs
            .iter()
            .map(|row| (row.channel_count, row.host_active))
            .collect::<Vec<_>>(),
        [(16, true), (16, false)]
    );
    // A longer configuration is rejected before mutating the fixture. Its own
    // getState output remains a legal restore, not an invented transient state.
    let mut invalid = state.clone();
    invalid[20..24].copy_from_slice(&13_u32.to_le_bytes());
    invalid.insert(40, 0);
    assert!(plugin.load_state(&invalid).is_err());
    assert_eq!(plugin.save_state().unwrap(), state);
    // Both real Product probe/worker paths use this ordering after restore.
    // set_process_mode rebuilds the SDK buffers from current native metadata.
    plugin.set_process_mode(ProcessMode::Offline).unwrap();
    assert!(plugin.audio_bus_layout().unwrap().inputs.is_empty());
    plugin.start_processing().unwrap();
    let buffers = plugin.create_bus_audio_buffers(1).unwrap();
    assert!(buffers.inputs.is_empty());
    assert_eq!(buffers.outputs.len(), 1);
    plugin
}
fn notes() -> [PluginEvent; 4] {
    [
        note(true, 0, 10, 0.5),
        note(false, 3, 10, 0.125),
        note(true, 4, 11, 0.25),
        note(false, 8, 11, 0.125),
    ]
}
fn note(on: bool, offset: i32, id: i32, velocity: f32) -> PluginEvent {
    PluginEvent {
        bus_index: 0,
        sample_offset: offset,
        ppq_position: -3.5,
        flags: 0,
        data: if on {
            PluginEventData::NoteOn {
                channel: 3,
                pitch: 61,
                tuning: 0.0,
                velocity,
                length: 0,
                note_id: id,
            }
        } else {
            PluginEventData::NoteOff {
                channel: 3,
                pitch: 61,
                tuning: 0.0,
                velocity,
                note_id: id,
            }
        },
    }
}
fn invoke(
    plugin: &mut Plugin,
    buffers: &mut BusAudioBuffers,
    events: &[PluginEvent],
    scoped: bool,
) -> vst3_host::Result<()> {
    if scoped {
        let split = plugin.split_editor_processing().unwrap();
        let mut processor = split.processor;
        let result = std::thread::scope(|scope| {
            scope
                .spawn(|| processor.process_bus_audio_with_note_events(buffers, events))
                .join()
                .expect("actual joined native DSP role")
        });
        drop(processor);
        drop(split.editor);
        result
    } else {
        plugin.process_bus_audio_with_note_events(buffers, events)
    }
}
fn assert_pcm(buffers: &BusAudioBuffers, left: &[f32]) {
    assert!(buffers.inputs.is_empty());
    assert_eq!(buffers.outputs.len(), 1);
    let channels = &buffers.outputs[0].channels;
    assert_eq!(channels.len(), 2);
    assert_eq!(
        (channels[0].len(), channels[1].len()),
        (left.len(), left.len())
    );
    for (frame, &left) in left.iter().enumerate() {
        let right = if left == 0.0 { 0.0 } else { -left * 0.5 };
        assert_eq!(
            [channels[0][frame].to_bits(), channels[1][frame].to_bits()],
            [left.to_bits(), right.to_bits()]
        );
    }
}
type NativeCall = (i32, i32, u32, u32);
#[derive(Debug)]
struct Audit {
    rate_bits: u64,
    calls: Vec<NativeCall>,
    tuples: Vec<[u8; 48]>,
}
struct AuditTail {
    path: PathBuf,
    prefix: usize,
}
impl AuditTail {
    fn begin() -> Self {
        let directory = PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR")
                .expect("external caller supplies a preexisting absolute audit directory"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        let path = directory.join(format!("mne6-{}.audit", std::process::id()));
        let prefix = match std::fs::metadata(&path) {
            Ok(metadata) => usize::try_from(metadata.len()).unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("audit prefix: {error}"),
        };
        assert!(prefix <= AUDIT_BOUND);
        Self { path, prefix }
    }
    fn read(self) -> Vec<Audit> {
        let length = std::fs::metadata(&self.path).unwrap().len();
        assert!(length >= self.prefix as u64 && length <= AUDIT_BOUND as u64);
        let bytes = std::fs::read(self.path).unwrap();
        assert!(bytes.len() >= self.prefix && bytes.len() <= AUDIT_BOUND);
        let mut records = Vec::new();
        let mut offset = self.prefix;
        while offset < bytes.len() {
            let body = &bytes[offset..];
            assert!(body.len() >= 80);
            assert_eq!(&body[..4], b"MNA4");
            assert_eq!(u32_at(body, 4), 4);
            assert_eq!(u32_at(body, 12), 6);
            assert_eq!(u32_at(body, 16), std::process::id());
            assert_eq!(
                u32_at(body, 20),
                1,
                "actual joined control-side termination"
            );
            assert_ne!(u64::from_le_bytes(body[24..32].try_into().unwrap()), 0);
            assert_eq!(u32_at(body, 32), 1, "complete bounded audit");
            assert_eq!(u32_at(body, 36), 6, "AudioOut + Main Event; no AudioIn/Aux");
            assert_eq!(u32_at(body, 48), BLOCK as u32);
            let calls = u32_at(body, 52) as usize;
            let events = u32_at(body, 56) as usize;
            assert_eq!((u32_at(body, 60), u32_at(body, 64)), (1024, 8192));
            assert!(calls <= 1024 && events <= 8192);
            assert!(u32_at(body, 68) >= 16 && u32_at(body, 72) >= 48);
            assert_eq!(u32_at(body, 76), 0);
            let length = u32_at(body, 8) as usize;
            assert_eq!(length, 80 + calls * 16 + events * 48);
            assert!(length <= body.len(), "incomplete physical record");
            records.push(Audit {
                rate_bits: u64::from_le_bytes(body[40..48].try_into().unwrap()),
                calls: body[80..80 + calls * 16]
                    .chunks_exact(16)
                    .map(|row| {
                        (
                            i32::from_le_bytes(row[..4].try_into().unwrap()),
                            i32::from_le_bytes(row[4..8].try_into().unwrap()),
                            u32_at(row, 8),
                            u32_at(row, 12),
                        )
                    })
                    .collect(),
                tuples: body[80 + calls * 16..length]
                    .chunks_exact(48)
                    .map(|row| row.try_into().unwrap())
                    .collect(),
            });
            offset += length;
        }
        assert!(!records.is_empty());
        records
    }
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn tuple(call: u32, on: bool, offset: i32, id: i32, velocity: f32, ppq: u64) -> [u8; 48] {
    let mut row = [0_u8; 48];
    row[..4].copy_from_slice(&call.to_le_bytes());
    row[8..12].copy_from_slice(&offset.to_le_bytes());
    row[12..20].copy_from_slice(&ppq.to_le_bytes());
    row[22..24].copy_from_slice(&(if on { 0_u16 } else { 1 }).to_le_bytes());
    row[24..26].copy_from_slice(&3_i16.to_le_bytes());
    row[26..28].copy_from_slice(&61_i16.to_le_bytes());
    row[32..36].copy_from_slice(&velocity.to_bits().to_le_bytes());
    row[40..44].copy_from_slice(&id.to_le_bytes());
    row
}

// Independently derived Fraction→binary64 bits at original offsets0,3,4,8.
// These are literals, not SDK projection results or accumulated PPQ.
const Q: [[[u64; 4]; 2]; 2] = [
    [
        [
            0x400cf2c1c345a6f2,
            0x400cf2f151373595,
            0x400cf3012b326521,
            0x400cf340931f2351,
        ],
        [
            0x3f07c6f8c751f177,
            0x3f27c6f8c751f177,
            0x3f2db8b6f9266dd5,
            0x3f3abfd7e03c2fa6,
        ],
    ],
    [
        [
            0x400cf2d7a85bcd6b,
            0x400cf303592b5545,
            0x400cf311e970828d,
            0x400cf34c2a8537b0,
        ],
        [
            0x3f05d867c3ece2a5,
            0x3f25d867c3ece2a5,
            0x3f2b4e81b4e81b4f,
            0x3f389374bc6a7efa,
        ],
    ],
];
#[test]
#[ignore = "rebuilt MNE6 owned DLL + existing absolute audit dir; real SDK zero-audio-input, not Product/canonical"]
fn installed_vst3_zero_input_checked_notes_preserve_literal_pcm_ppq_and_native_shape() {
    let tail = AuditTail::begin();
    let mut expected = Vec::new();
    for (r, rate) in [44_100, 48_000].into_iter().enumerate() {
        let mut host = host(rate);
        for scoped in [false, true] {
            for near_max in [false, true] {
                let mut plugin = open(&mut host, 0);
                let (start, reference) = if near_max {
                    (
                        i64::MAX - 16,
                        ProcessMusicalReference::new(i64::MAX - 17, i64::from(rate), 1, 2).unwrap(),
                    )
                } else {
                    (-17, ProcessMusicalReference::new(-19, 7, 3, 4).unwrap())
                };
                plugin
                    .set_process_musical_timeline_context(
                        ProcessTimelineContext { sample_position: i64::MAX - 4, sample_rate: rate },
                        reference,
                    )
                    .unwrap();
                let events = notes();
                let mut buffers = plugin.create_bus_audio_buffers(9).unwrap();
                assert!(buffers.inputs.is_empty());
                for channel in &mut buffers.outputs[0].channels {
                    channel.fill(123.0);
                }
                let before = buffers.clone();
                assert!(matches!(
                    invoke(&mut plugin, &mut buffers, &events, scoped),
                    Err(vst3_host::Error::NativeEvent(
                        NativeEventError::InvalidFrames
                    ))
                ));
                assert_eq!(buffers, before);
                plugin
                    .set_process_musical_timeline_context(
                        ProcessTimelineContext { sample_position: start, sample_rate: rate },
                        reference,
                    )
                    .unwrap();
                for invalid in [true, false] {
                    let mut rejected = events.clone();
                    if invalid {
                        rejected[3].ppq_position = f64::NAN;
                    } else {
                        rejected[3].bus_index = 1; // Real retained but inactive Aux.
                    }
                    assert!(matches!(
                        invoke(&mut plugin, &mut buffers, &rejected, scoped),
                        Err(vst3_host::Error::NativeEvent(
                            NativeEventError::InvalidEvent { index: 3 }
                        ))
                    ));
                    assert_eq!(buffers, before);
                }
                invoke(&mut plugin, &mut buffers, &events, scoped).unwrap();
                assert_pcm(
                    &buffers,
                    &[0.25, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0],
                );
                let mut empty = plugin.create_bus_audio_buffers(1).unwrap();
                invoke(&mut plugin, &mut empty, &[], scoped).unwrap();
                assert_pcm(&empty, &[0.0]);
                let q = Q[r][usize::from(near_max)];
                let mut tuples = vec![
                    tuple(1, true, 0, 10, 0.5, q[0]),
                    tuple(1, false, 3, 10, 0.125, q[1]),
                    tuple(2, true, 0, 11, 0.25, q[2]),
                    tuple(3, false, 0, 11, 0.125, q[3]),
                ];
                // None restores explicit caller PPQ on this same prepared
                // zero-input object; it does not authorize canonical None Notes.
                plugin
                    .set_process_timeline_context(ProcessTimelineContext {
                        sample_position: 100,
                        sample_rate: rate,
                    })
                    .unwrap();
                invoke(&mut plugin, &mut buffers, &events, scoped).unwrap();
                assert_pcm(
                    &buffers,
                    &[0.25, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0],
                );
                for (call, on, offset, id, velocity) in [
                    (5, true, 0, 10, 0.5),
                    (5, false, 3, 10, 0.125),
                    (6, true, 0, 11, 0.25),
                    (7, false, 0, 11, 0.125),
                ] {
                    tuples.push(tuple(call, on, offset, id, velocity, (-3.5_f64).to_bits()));
                }
                plugin.stop_processing().unwrap();
                let saved = plugin.save_state().unwrap();
                assert_eq!(saved, configuration(0));
                plugin.load_state(&saved).unwrap();
                assert_eq!(plugin.save_state().unwrap(), saved);
                assert!(plugin.native_audio_bus_info().unwrap().inputs.is_empty());
                drop(plugin);
                expected.push((
                    rate,
                    vec![
                        (4, 0, 0, 2),
                        (4, 0, 2, 1),
                        (1, 0, 3, 1),
                        (1, 0, 4, 0),
                        (4, 0, 4, 2),
                        (4, 0, 6, 1),
                        (1, 0, 7, 1),
                    ],
                    tuples,
                ));
            }
        }
    }
    let records = tail.read();
    assert_eq!(records.len(), 8);
    for (actual, (rate, calls, tuples)) in records.into_iter().zip(expected) {
        assert_eq!(actual.rate_bits, f64::from(rate).to_bits());
        assert_eq!(
            actual.calls, calls,
            "bad batch/window never reached native processing"
        );
        assert_eq!(
            actual.tuples, tuples,
            "exact same-object full native history"
        );
    }
}

#[test]
#[ignore = "rebuilt MNE6 owned DLL + absolute audit dir; real SDK failure prefix and no replay, not a DSP rollback"]
fn installed_vst3_zero_input_note_failures_keep_consumed_prefix_without_replay() {
    let tail = AuditTail::begin();
    let mut host = host(48_000);
    for scoped in [false, true] {
        let mut plugin = open(&mut host, 2);
        plugin
            .set_process_musical_timeline_context(
                ProcessTimelineContext { sample_position: -17, sample_rate: 48_000 },
                ProcessMusicalReference::new(-19, 7, 3, 4).unwrap(),
            )
            .unwrap();
        let mut buffers = plugin.create_bus_audio_buffers(9).unwrap();
        for channel in &mut buffers.outputs[0].channels {
            channel.fill(123.0);
        }
        assert!(matches!(
            invoke(&mut plugin, &mut buffers, &notes(), scoped),
            Err(vst3_host::Error::ProcessFailed(1))
        ));
        assert_eq!(buffers.outputs[0].channels[0][..4], [0.25, 0.25, 0.25, 0.0]);
        assert_eq!(
            buffers.outputs[0].channels[1][..4],
            [-0.125, -0.125, -0.125, 0.0]
        );
        assert!(buffers.outputs[0].channels.iter().all(|channel| channel[4..] == [123.0; 5]));
        let mut empty = plugin.create_bus_audio_buffers(1).unwrap();
        invoke(&mut plugin, &mut empty, &[], scoped).unwrap();
        assert_pcm(&empty, &[0.0]);
        plugin.stop_processing().unwrap();
        drop(plugin);
    }
    let records = tail.read();
    assert_eq!(records.len(), 2);
    for actual in records {
        assert_eq!(actual.rate_bits, 48_000_f64.to_bits());
        assert_eq!(actual.calls, [(4, 0, 0, 2), (4, 1, 2, 1), (1, 0, 3, 0)]);
        assert_eq!(
            actual.tuples,
            [
                tuple(1, true, 0, 10, 0.5, Q[1][0][0]),
                tuple(1, false, 3, 10, 0.125, Q[1][0][1]),
                tuple(2, true, 0, 11, 0.25, Q[1][0][2]),
            ]
        );
    }
}
