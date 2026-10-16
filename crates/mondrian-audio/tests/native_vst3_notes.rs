//! Owned DLL, local SDK adapter qualification only. No canonical Note route,
//! IPC/third-party/RT/whole-heap or all-or-nothing DSP rollback claim.
#![cfg(windows)]

use std::path::PathBuf;
use vst3_host::audio::{BusAudioBuffers, BusDirection, MediaType, NativeEventBusRole};
use vst3_host::midi::{PluginEvent, PluginEventData};
use vst3_host::plugin::ProcessMode;
use vst3_host::{Plugin, Vst3Host};

const CLASS: &str = "6E4F05A12D8B4C639F017B42A4C2E611";
const RATE: f64 = 48_000.0;
const BLOCK: usize = 8;
const STATE_BOUND: usize = 256 * 1024;

fn fixture() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN")
            .expect("rebuilt owned Note oracle DLL; never skip an ignored native body"),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}
fn host() -> Vst3Host {
    Vst3Host::builder()
        .sample_rate(RATE)
        .block_size(BLOCK)
        .input_channels(2)
        .output_channels(2)
        .build()
        .unwrap()
}
fn configuration(fail: u32) -> Vec<u8> {
    let mut component = b"MNE1".to_vec();
    component.extend_from_slice(&1_u32.to_le_bytes());
    component.extend_from_slice(&fail.to_le_bytes());
    let mut envelope = b"VST3HOST_STATE\0\0".to_vec();
    envelope.extend_from_slice(&1_u32.to_le_bytes());
    envelope.extend_from_slice(&(component.len() as u32).to_le_bytes());
    envelope.extend_from_slice(&(component.len() as u32).to_le_bytes());
    envelope.extend_from_slice(&component);
    envelope.extend_from_slice(&component);
    envelope
}
fn open(host: &mut Vst3Host, fail: u32) -> Plugin {
    let mut plugin = host.load_plugin_class(fixture(), CLASS).expect("owned independent class");
    let before = audit(&plugin);
    plugin
        .load_state(&configuration(fail))
        .expect("same-object configuration restore");
    assert_eq!(
        before,
        audit(&plugin),
        "configuration cannot replay or rewrite audit"
    );
    let info = plugin.native_event_bus_info().expect("restored inactive native event census");
    assert_eq!(info.inputs.len(), 2);
    assert!(info.outputs.is_empty());
    assert_eq!(info.inputs[0].role, NativeEventBusRole::Main);
    assert_eq!(info.inputs[1].role, NativeEventBusRole::Auxiliary);
    assert_eq!(
        info.inputs.iter().map(|bus| bus.channel_count).collect::<Vec<_>>(),
        [16, 16]
    );
    assert_eq!(
        info.inputs.iter().map(|bus| bus.default_active).collect::<Vec<_>>(),
        [true, false]
    );
    for index in 0..2 {
        plugin
            .set_bus_active(MediaType::Event, BusDirection::Input, index, false)
            .unwrap();
    }
    assert!(plugin
        .native_event_bus_info()
        .unwrap()
        .inputs
        .iter()
        .all(|bus| !bus.host_active));
    for index in 0..2 {
        plugin
            .set_bus_active(MediaType::Event, BusDirection::Input, index, true)
            .unwrap();
    }
    assert!(plugin.native_event_bus_info().unwrap().inputs.iter().all(|bus| bus.host_active));
    // host_active records successful host calls; it is not a nonexistent plugin active-query.
    plugin.set_bus_active(MediaType::Audio, BusDirection::Input, 0, true).unwrap();
    plugin.set_bus_active(MediaType::Audio, BusDirection::Output, 0, true).unwrap();
    plugin.set_process_mode(ProcessMode::Offline).unwrap();
    plugin.start_processing().unwrap();
    let prepared = audit(&plugin);
    assert_eq!(prepared.sample_rate_bits, RATE.to_bits());
    assert_eq!(prepared.max_frames, BLOCK as u32);
    assert_eq!(prepared.instance, before.instance);
    assert_eq!(
        prepared.calls, before.calls,
        "preparation is not a Process callback"
    );
    assert_eq!(prepared.events, before.events);
    plugin
}
fn note(on: bool, bus: i32, offset: i32, id: i32, velocity: f32) -> PluginEvent {
    PluginEvent {
        bus_index: bus,
        sample_offset: offset,
        ppq_position: -3.5,
        flags: 0xc001,
        data: if on {
            PluginEventData::NoteOn {
                channel: 3,
                pitch: 61,
                tuning: -0.25,
                velocity,
                length: 19,
                note_id: id,
            }
        } else {
            PluginEventData::NoteOff {
                channel: 3,
                pitch: 61,
                velocity,
                note_id: id,
                tuning: -0.25,
            }
        },
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Audit {
    instance: u64,
    sample_rate_bits: u64,
    max_frames: u32,
    calls: Vec<(i32, i32, u32, u32)>,
    events: Vec<Vec<u8>>,
}
fn u32_at(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index..index + 4].try_into().unwrap())
}
fn audit(plugin: &Plugin) -> Audit {
    let bytes = plugin.save_state().expect("actual component audit query after DSP join");
    assert!(bytes.len() <= STATE_BOUND);
    assert_eq!(&bytes[..16], b"VST3HOST_STATE\0\0");
    assert_eq!(u32_at(&bytes, 16), 1);
    let component_len = u32_at(&bytes, 20) as usize;
    let controller_len = u32_at(&bytes, 24) as usize;
    assert_eq!(controller_len, 12);
    assert_eq!(bytes.len(), 28 + component_len + controller_len);
    let body = &bytes[28..28 + component_len];
    assert_eq!(&body[..4], b"MNE1");
    assert_eq!(u32_at(body, 4), 1);
    assert_eq!(&body[12..16], b"MNA1");
    assert_eq!(u32_at(body, 16), 1);
    let instance = u64::from_le_bytes(body[20..28].try_into().unwrap());
    assert_ne!(instance, 0);
    let calls = u32_at(body, 28) as usize;
    let events = u32_at(body, 32) as usize;
    assert!(calls <= 64 && events <= 4096);
    assert_eq!(
        u32_at(body, 36),
        1,
        "retained prefix/overflow is not complete evidence"
    );
    let sample_rate_bits = u64::from_le_bytes(body[40..48].try_into().unwrap());
    let max_frames = u32_at(body, 48);
    assert_eq!(u32_at(body, 52), 0);
    assert_eq!(component_len, 56 + calls * 16 + events * 48);
    let rows = body[56..56 + calls * 16]
        .chunks_exact(16)
        .map(|row| {
            (
                i32::from_le_bytes(row[..4].try_into().unwrap()),
                i32::from_le_bytes(row[4..8].try_into().unwrap()),
                u32_at(row, 8),
                u32_at(row, 12),
            )
        })
        .collect();
    Audit {
        instance,
        sample_rate_bits,
        max_frames,
        calls: rows,
        events: body[56 + calls * 16..].chunks_exact(48).map(<[u8]>::to_vec).collect(),
    }
}
fn expected_tuple(call: u32, event: &PluginEvent, local_offset: i32) -> Vec<u8> {
    // Literal wire oracle, independent of Host plugin_event_to_raw and fixture encoder.
    let (kind, channel, pitch, tuning, velocity, length, id) = match event.data {
        PluginEventData::NoteOn { channel, pitch, tuning, velocity, length, note_id } => {
            (0_u16, channel, pitch, tuning, velocity, length, note_id)
        }
        PluginEventData::NoteOff { channel, pitch, velocity, note_id, tuning } => {
            (1_u16, channel, pitch, tuning, velocity, 0, note_id)
        }
        _ => panic!("only the explicit Note tuple contract"),
    };
    let mut out = Vec::with_capacity(48);
    out.extend_from_slice(&call.to_le_bytes());
    out.extend_from_slice(&event.bus_index.to_le_bytes());
    out.extend_from_slice(&local_offset.to_le_bytes());
    out.extend_from_slice(&event.ppq_position.to_bits().to_le_bytes());
    out.extend_from_slice(&event.flags.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&channel.to_le_bytes());
    out.extend_from_slice(&pitch.to_le_bytes());
    out.extend_from_slice(&tuning.to_bits().to_le_bytes());
    out.extend_from_slice(&velocity.to_bits().to_le_bytes());
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out
}
fn process(
    plugin: &mut Plugin,
    frames: usize,
    events: &[PluginEvent],
    scoped: bool,
) -> (BusAudioBuffers, vst3_host::Result<()>) {
    let mut buffers = plugin.create_bus_audio_buffers(frames).unwrap();
    let result = invoke(plugin, &mut buffers, events, scoped);
    (buffers, result)
}
fn invoke(
    plugin: &mut Plugin,
    buffers: &mut BusAudioBuffers,
    events: &[PluginEvent],
    scoped: bool,
) -> vst3_host::Result<()> {
    if scoped {
        let split = plugin.split_editor_processing().expect("same native owner split");
        let mut processor = split.processor;
        let result = std::thread::scope(|scope| {
            scope
                .spawn(|| processor.process_bus_audio_with_note_events(buffers, events))
                .join()
                .expect("joined real DSP thread")
        });
        drop(processor);
        drop(split.editor);
        result
    } else {
        plugin.process_bus_audio_with_note_events(buffers, events)
    }
}
fn assert_pcm(buffers: &BusAudioBuffers, expected: &[f32]) {
    assert_eq!(buffers.outputs.len(), 1);
    assert_eq!(buffers.outputs[0].channels.len(), 2);
    for (channel, samples) in buffers.outputs[0].channels.iter().enumerate() {
        assert_eq!(samples.len(), expected.len());
        for (&actual, &expected) in samples.iter().zip(expected) {
            let expected = if expected == 0.0 {
                0.0
            } else if channel == 0 {
                expected
            } else {
                -expected * 0.5
            };
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
    }
}

#[test]
#[ignore = "requires rebuilt owned MONDRIAN_VST3_NOTE_ORACLE_PLUGIN; direct SDK adapter, no canonical Note support"]
fn installed_vst3_note_oracle_preserves_tuples_across_chunks_windows_and_scoped_roles() {
    let mut host = host();
    for scoped in [false, true] {
        for partitions in [vec![24], vec![5, 7, 12], vec![8, 8, 8]] {
            let mut plugin = open(&mut host, 0);
            let first = audit(&plugin);
            let events = [
                note(true, 0, 0, 101, 0.5),
                note(true, 1, 7, 202, 0.25),
                note(false, 0, 8, 101, 0.125),
                note(true, 0, 8, 303, 0.25),
                note(false, 1, 15, 202, 0.375),
                note(false, 0, 23, 303, 0.625),
            ];
            let literal = [
                0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.5, 0.375, 0.375, 0.375, 0.375, 0.375,
                0.375, 0.375, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.0,
            ];
            let mut start = 0;
            let mut expected_events = Vec::new();
            let mut expected_calls = Vec::new();
            for frames in partitions {
                let window = events
                    .iter()
                    .filter(|event| {
                        (start..start + frames).contains(&(event.sample_offset as usize))
                    })
                    .cloned()
                    .map(|mut event| {
                        event.sample_offset -= start as i32;
                        event
                    })
                    .collect::<Vec<_>>();
                let (buffers, result) = process(&mut plugin, frames, &window, scoped);
                result.expect("checked whole window, native chunks execute once");
                assert_pcm(&buffers, &literal[start..start + frames]);
                for chunk in (0..frames).step_by(BLOCK) {
                    let count_before = expected_events.len();
                    let chunk_frames = (frames - chunk).min(BLOCK);
                    let call = (expected_calls.len() + 1) as u32;
                    for event in window.iter().filter(|event| {
                        (chunk..chunk + chunk_frames).contains(&(event.sample_offset as usize))
                    }) {
                        expected_events.push(expected_tuple(
                            call,
                            event,
                            event.sample_offset - chunk as i32,
                        ));
                    }
                    expected_calls.push((
                        chunk_frames as i32,
                        0,
                        count_before as u32,
                        (expected_events.len() - count_before) as u32,
                    ));
                }
                start += frames;
            }
            assert_eq!(start, 24);
            let after = audit(&plugin);
            assert_eq!(after.instance, first.instance);
            assert_eq!(after.calls, expected_calls);
            assert_eq!(after.events, expected_events);
            assert_eq!(
                after,
                audit(&plugin),
                "reading native audit is non-consuming"
            );
            plugin.stop_processing().unwrap();
        }
    }
}

#[test]
#[ignore = "requires rebuilt owned MONDRIAN_VST3_NOTE_ORACLE_PLUGIN; deliberate SDK failure, no retry"]
fn installed_vst3_note_oracle_failed_middle_chunk_is_not_replayed() {
    let mut host = host();
    for scoped in [false, true] {
        let mut plugin = open(&mut host, 2);
        let first = audit(&plugin);
        let events = [
            note(true, 0, 0, 11, 0.5),
            note(false, 0, 8, 11, 0.125),
            note(true, 1, 16, 22, 0.25),
        ];
        let (_, result) = process(&mut plugin, 24, &events, scoped);
        assert!(result.is_err(), "native callback 2 must remain failed");
        let failed = audit(&plugin);
        assert_eq!(failed.instance, first.instance);
        assert_eq!(failed.calls, [(8, 0, 0, 1), (8, 1, 1, 1)]);
        assert_eq!(
            failed.events,
            [
                expected_tuple(1, &events[0], 0),
                expected_tuple(2, &events[1], 0)
            ]
        );
        // The first successful chunk advanced its real voice. Failed chunk was
        // observed but not applied; remaining chunk never ran. No rollback/retry.
        let (buffers, result) = process(&mut plugin, 8, &[], scoped);
        result.unwrap();
        assert_pcm(&buffers, &[0.25; 8]);
        let next = audit(&plugin);
        assert_eq!(next.instance, first.instance);
        assert_eq!(next.calls, [(8, 0, 0, 1), (8, 1, 1, 1), (8, 0, 2, 0)]);
        assert_eq!(
            next.events, failed.events,
            "failed/unexecuted notes cannot replay next window"
        );
        plugin.stop_processing().unwrap();
    }
}

#[test]
#[ignore = "requires rebuilt owned MONDRIAN_VST3_NOTE_ORACLE_PLUGIN; checked rejection before native and exact cap"]
fn installed_vst3_note_oracle_rejects_invalid_batches_before_native_and_accepts_4096() {
    let mut host = host();
    for scoped in [false, true] {
        let mut plugin = open(&mut host, 0);
        let initial = audit(&plugin);
        let valid = note(true, 0, 0, 1, 0.5);
        let mut invalid = Vec::new();
        for offset in [-1, 8, i32::MAX] {
            let mut event = valid.clone();
            event.sample_offset = offset;
            invalid.push(event);
        }
        let mut event = valid.clone();
        event.bus_index = 2;
        invalid.push(event);
        let mut event = valid.clone();
        event.flags = 2;
        invalid.push(event);
        let mut event = valid.clone();
        event.ppq_position = f64::NAN;
        invalid.push(event);
        for data in [
            PluginEventData::NoteOn {
                channel: 3,
                pitch: 61,
                tuning: 0.0,
                velocity: 0.5,
                length: 0,
                note_id: -1,
            },
            PluginEventData::NoteOn {
                channel: 16,
                pitch: 61,
                tuning: 0.0,
                velocity: 0.5,
                length: 0,
                note_id: 1,
            },
            PluginEventData::NoteOn {
                channel: 3,
                pitch: 128,
                tuning: 0.0,
                velocity: 0.5,
                length: 0,
                note_id: 1,
            },
            PluginEventData::NoteOn {
                channel: 3,
                pitch: 61,
                tuning: f32::NAN,
                velocity: 0.5,
                length: 0,
                note_id: 1,
            },
            PluginEventData::NoteOn {
                channel: 3,
                pitch: 61,
                tuning: 0.0,
                velocity: 1.25,
                length: 0,
                note_id: 1,
            },
            PluginEventData::NoteOn {
                channel: 3,
                pitch: 61,
                tuning: 0.0,
                velocity: 0.5,
                length: -1,
                note_id: 1,
            },
            PluginEventData::NoteOff {
                channel: 3,
                pitch: 61,
                tuning: 0.0,
                velocity: 0.5,
                note_id: -1,
            },
            PluginEventData::PolyPressure { channel: 3, pitch: 61, pressure: 0.5, note_id: 1 },
        ] {
            let mut event = valid.clone();
            event.data = data;
            invalid.push(event);
        }
        for bad in invalid {
            let (_, result) = process(&mut plugin, 8, &[valid.clone(), bad], scoped);
            assert!(result.is_err());
            assert_eq!(
                audit(&plugin),
                initial,
                "a bad suffix must not publish a valid prefix or call native"
            );
        }
        let (_, result) = process(
            &mut plugin,
            8,
            &[note(true, 0, 5, 1, 0.5), note(false, 0, 3, 1, 0.125)],
            scoped,
        );
        assert!(result.is_err());
        assert_eq!(
            audit(&plugin),
            initial,
            "out-of-order batch is rejected before SDK"
        );
        for shape in 0..4 {
            let mut buffers = plugin.create_bus_audio_buffers(8).unwrap();
            match shape {
                0 => buffers.sample_rate = 44_100.0,
                1 => {
                    buffers.inputs[0].channels[1].pop();
                }
                2 => {
                    buffers.outputs[0].channels[0].pop();
                }
                _ => buffers.block_size = 7,
            }
            assert!(invoke(
                &mut plugin,
                &mut buffers,
                std::slice::from_ref(&valid),
                scoped
            )
            .is_err());
            assert_eq!(
                audit(&plugin),
                initial,
                "shape/rate rejection cannot call native"
            );
        }
        let mut zero = plugin.create_bus_audio_buffers(8).unwrap();
        zero.block_size = 0;
        for bus in zero.inputs.iter_mut().chain(&mut zero.outputs) {
            for channel in &mut bus.channels {
                channel.clear();
            }
        }
        assert!(invoke(&mut plugin, &mut zero, std::slice::from_ref(&valid), scoped).is_err());
        assert_eq!(audit(&plugin), initial, "zero window cannot consume a Note");
        let (_, result) = process(&mut plugin, 8, &vec![valid.clone(); 4097], scoped);
        assert!(result.is_err());
        assert_eq!(audit(&plugin), initial);
        let mut boundary = Vec::with_capacity(4096);
        for _ in 0..2048 {
            boundary.push(valid.clone());
            boundary.push(note(false, 0, 0, 1, 0.125));
        }
        let (buffers, result) = process(&mut plugin, 8, &boundary, scoped);
        result.expect("4096 exact, ordered On/Off pairs fit the actual native list");
        assert_pcm(&buffers, &[0.0; 8]);
        let accepted = audit(&plugin);
        assert_eq!(accepted.instance, initial.instance);
        assert_eq!(accepted.calls, [(8, 0, 0, 4096)]);
        assert_eq!(
            accepted.events,
            boundary.iter().map(|event| expected_tuple(1, event, 0)).collect::<Vec<_>>()
        );
        let (buffers, result) = process(&mut plugin, 8, &[], scoped);
        result.unwrap();
        assert_pcm(&buffers, &[0.0; 8]);
        let after = audit(&plugin);
        assert_eq!(after.calls, [(8, 0, 0, 4096), (8, 0, 4096, 0)]);
        assert_eq!(after.events, accepted.events);
        plugin.stop_processing().unwrap();
    }
}
