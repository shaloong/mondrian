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
    // Host loading records the requested config; prepare it natively before
    // comparing the complete setup/audit across the same-object state restore.
    plugin.reconfigure(RATE, BLOCK).expect("real requested setup before audit");
    let before = audit(&plugin);
    assert_eq!(before.sample_rate_bits, RATE.to_bits());
    assert_eq!(before.max_frames, BLOCK as u32);
    assert!(
        before.calls.is_empty() && before.events.is_empty(),
        "cold preparation is not DSP"
    );
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

fn main_configuration(fail: u32) -> Vec<u8> {
    let mut component = b"MNE4".to_vec();
    component.extend_from_slice(&4_u32.to_le_bytes());
    component.extend_from_slice(&fail.to_le_bytes());
    let mut envelope = b"VST3HOST_STATE\0\0".to_vec();
    for value in [1_u32, 12, 12] {
        envelope.extend_from_slice(&value.to_le_bytes());
    }
    envelope.extend_from_slice(&component);
    envelope.extend_from_slice(&component);
    envelope
}

// This is a complete physical record, not a globally unique DLL-static serial.
// A truncated/overwritten/missing record never counts as successful evidence.
fn main_file_audits(bytes: &[u8]) -> Vec<Audit> {
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let body = &bytes[offset..];
        assert!(body.len() >= 80, "truncated MNA4 header");
        assert_eq!(&body[..4], b"MNA4");
        assert_eq!(u32_at(body, 4), 4);
        let length = u32_at(body, 8) as usize;
        assert_eq!(u32_at(body, 12), 4);
        assert_eq!(u32_at(body, 16), std::process::id());
        assert_eq!(
            u32_at(body, 20),
            1,
            "actual control-thread terminate record"
        );
        assert_eq!(u32_at(body, 32), 1, "audit must be complete");
        assert_eq!(
            u32_at(body, 36),
            7,
            "Audio in/out + Main Event; Aux stayed inactive"
        );
        let instance = u64::from_le_bytes(body[24..32].try_into().unwrap());
        assert_ne!(instance, 0);
        let calls = u32_at(body, 52) as usize;
        let events = u32_at(body, 56) as usize;
        assert_eq!(u32_at(body, 60), 1024);
        assert_eq!(u32_at(body, 64), 8192);
        // Rust backing sizes are reported separately from the 16/48-byte wire rows.
        assert!(u32_at(body, 68) >= 16);
        assert!(u32_at(body, 72) >= 48);
        assert_eq!(u32_at(body, 76), 0);
        assert!(calls <= 1024 && events <= 8192);
        assert_eq!(length, 80 + calls * 16 + events * 48);
        assert!(length <= body.len(), "truncated MNA4 rows");
        let body = &body[..length];
        records.push(Audit {
            instance,
            sample_rate_bits: u64::from_le_bytes(body[40..48].try_into().unwrap()),
            max_frames: u32_at(body, 48),
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
            events: body[80 + calls * 16..].chunks_exact(48).map(<[u8]>::to_vec).collect(),
        });
        offset += length;
    }
    assert!(!records.is_empty(), "missing complete MNA4 lifecycle");
    records
}

#[test]
#[ignore = "requires rebuilt owned MONDRIAN_VST3_NOTE_ORACLE_PLUGIN and existing absolute MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR; direct SDK Main-only oracle, not canonical Runtime qualification"]
fn installed_vst3_main_note_oracle_preserves_main_only_pcm_tuples_and_bounded_audit() {
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR")
            .expect("external caller must explicitly provide the audit directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    let path = directory.join(format!("mne4-{}.audit", std::process::id()));
    let prefix = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len() as usize,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("audit metadata failed: {error}"),
    };
    let mut expected_records = Vec::new();
    let mut host = host();
    for scoped in [false, true] {
        let mut plugin = host.load_plugin_class(fixture(), CLASS).unwrap();
        let configuration = main_configuration(0);
        plugin.load_state(&configuration).expect("MNE4 component/controller restore");
        assert_eq!(plugin.save_state().unwrap(), configuration);
        let info = plugin.native_event_bus_info().unwrap();
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
        assert_eq!(
            info.inputs.iter().map(|bus| bus.host_active).collect::<Vec<_>>(),
            [true, false]
        );
        plugin.set_process_mode(ProcessMode::Offline).unwrap();
        plugin.start_processing().unwrap();
        let mut bad_buffers = plugin.create_bus_audio_buffers(BLOCK).unwrap();
        for channel in &mut bad_buffers.outputs[0].channels {
            channel.fill(123.0);
        }
        let bad = [note(true, 0, 0, 1, 0.5), note(true, 1, 1, 2, 0.5)];
        assert!(matches!(
            invoke(&mut plugin, &mut bad_buffers, &bad, scoped),
            Err(vst3_host::Error::NativeEvent(
                vst3_host::error::NativeEventError::InvalidEvent { index: 1 }
            ))
        ));
        assert!(bad_buffers.outputs[0].channels.iter().flatten().all(|sample| *sample == 123.0));
        let equal = [
            note(true, 0, 0, 1, 0.5),
            note(false, 0, 4, 1, 0.125),
            note(true, 0, 4, 2, 0.25),
            note(false, 0, 7, 2, 0.625),
        ];
        let (buffers, result) = process(&mut plugin, BLOCK, &equal, scoped);
        result.unwrap();
        assert_pcm(
            &buffers,
            &[0.25, 0.25, 0.25, 0.25, 0.125, 0.125, 0.125, 0.0],
        );
        let mut calls = vec![(8, 0, 0, 4)];
        let mut events: Vec<_> = equal
            .iter()
            .map(|event| expected_tuple(1, event, event.sample_offset))
            .collect();
        let mut next_id = 3;
        for epoch in 0..2 {
            if epoch != 0 {
                plugin.stop_processing().unwrap();
                plugin.load_state(&configuration).unwrap();
                assert_eq!(plugin.save_state().unwrap(), configuration);
                plugin.start_processing().unwrap();
            }
            // Each epoch has >1024 sequential pairs; existing mode1's 64 calls
            // cannot hold this evidence. Offs and later IDs are all retained.
            let mut remaining = 1025;
            while remaining != 0 {
                let pairs = remaining.min(BLOCK / 2);
                let frames = pairs * 2;
                let mut notes = Vec::new();
                for index in 0..pairs {
                    notes.push(note(true, 0, (index * 2) as i32, next_id, 0.5));
                    notes.push(note(false, 0, (index * 2 + 1) as i32, next_id, 0.125));
                    next_id += 1;
                }
                let (buffers, result) = process(&mut plugin, frames, &notes, scoped);
                result.unwrap();
                let literal: Vec<_> =
                    (0..frames).map(|sample| if sample % 2 == 0 { 0.25 } else { 0.0 }).collect();
                assert_pcm(&buffers, &literal);
                let call = calls.len() as u32 + 1;
                calls.push((frames as i32, 0, events.len() as u32, notes.len() as u32));
                events.extend(
                    notes.iter().map(|event| expected_tuple(call, event, event.sample_offset)),
                );
                remaining -= pairs;
            }
            assert_eq!(
                plugin.save_state().unwrap(),
                configuration,
                "audit is not restored DSP state"
            );
        }
        assert_eq!(calls.len(), 515);
        assert_eq!(events.len(), 4104);
        plugin.stop_processing().unwrap();
        let info = plugin.native_event_bus_info().unwrap();
        assert_eq!(
            info.inputs.iter().map(|bus| bus.host_active).collect::<Vec<_>>(),
            [true, false]
        );
        drop(plugin); // Actual same component termination publishes the complete record.
        expected_records.push((calls, events));
    }
    let bytes = std::fs::read(&path).expect("same-PID audit must exist after actual terminate");
    assert!(bytes.len() <= 2 * 1024 * 1024 && bytes.len() >= prefix);
    let records = main_file_audits(&bytes[prefix..]);
    assert_eq!(records.len(), expected_records.len());
    for (actual, (calls, events)) in records.iter().zip(expected_records) {
        assert_eq!(actual.sample_rate_bits, RATE.to_bits());
        assert_eq!(actual.max_frames, BLOCK as u32);
        assert_eq!(
            actual.calls, calls,
            "rejected Aux suffix made no native call"
        );
        assert_eq!(
            actual.events, events,
            "every native tuple retained in physical lifecycle order"
        );
    }
    println!(
        "MNE4 pid={} records={} calls_per_record=515 events_per_record=4104 audit_bytes={}",
        std::process::id(),
        records.len(),
        bytes.len() - prefix
    );
}

#[test]
#[ignore = "requires rebuilt owned MONDRIAN_VST3_NOTE_ORACLE_PLUGIN; same-object SDK signed sample-window boundary, not canonical transport qualification"]
fn installed_vst3_note_oracle_rejects_overflowing_signal_windows_before_native() {
    use vst3_host::audio::ProcessTimelineContext;
    use vst3_host::error::NativeEventError;

    let mut host = host();
    for scoped in [false, true] {
        let mut plugin = open(&mut host, 0);
        let initial = audit(&plugin);
        assert!(initial.calls.is_empty() && initial.events.is_empty());
        for (start, frames) in [(i64::MAX - 16, 24), (i64::MAX - 8, 9)] {
            plugin
                .set_process_timeline_context(ProcessTimelineContext {
                    sample_position: start,
                    sample_rate: RATE as u32,
                })
                .unwrap();
            let mut buffers = plugin.create_bus_audio_buffers(frames).unwrap();
            for bus in buffers.inputs.iter_mut().chain(&mut buffers.outputs) {
                for channel in &mut bus.channels {
                    channel.fill(123.0);
                }
            }
            let before = buffers.clone();
            let events = [
                note(true, 0, 0, 901, 0.5),
                note(false, 0, frames as i32 - 1, 901, 0.125),
            ];
            let result = invoke(&mut plugin, &mut buffers, &events, scoped);
            let after = audit(&plugin); // The same Plugin, after its DSP borrow joined.
            assert!(
                matches!(&result, Err(vst3_host::Error::NativeEvent(NativeEventError::InvalidFrames))),
                "scoped={scoped}, start={start}, frames={frames}, result={result:?}, actual_calls={:?}",
                after.calls
            );
            assert_eq!(
                buffers, before,
                "whole-window refusal must preserve every caller sample"
            );
            assert_eq!(
                after, initial,
                "no successful native prefix or Note publication"
            );
        }

        let mut expected_calls = Vec::new();
        let mut expected_events = Vec::new();
        for (index, (start, frames)) in
            [(i64::MIN, 17), (-17, 19), (i64::MAX - 19, 19)].into_iter().enumerate()
        {
            plugin
                .set_process_timeline_context(ProcessTimelineContext {
                    sample_position: start,
                    sample_rate: RATE as u32,
                })
                .unwrap();
            let id = 1001 + index as i32;
            let events = [
                note(true, 0, 0, id, 0.5),
                note(false, 0, frames as i32 - 1, id, 0.125),
            ];
            let (buffers, result) = process(&mut plugin, frames, &events, scoped);
            result.expect("negative origin, partial chunk and exact exclusive end MAX are valid");
            let mut literal = vec![0.25; frames];
            literal[frames - 1] = 0.0;
            assert_pcm(&buffers, &literal);
            for chunk in (0..frames).step_by(BLOCK) {
                let chunk_frames = (frames - chunk).min(BLOCK);
                let first = expected_events.len();
                let call = expected_calls.len() as u32 + 1;
                for event in &events {
                    if (chunk..chunk + chunk_frames).contains(&(event.sample_offset as usize)) {
                        expected_events.push(expected_tuple(
                            call,
                            event,
                            event.sample_offset - chunk as i32,
                        ));
                    }
                }
                expected_calls.push((
                    chunk_frames as i32,
                    0,
                    first as u32,
                    (expected_events.len() - first) as u32,
                ));
            }
            let actual = audit(&plugin);
            assert_eq!(actual.instance, initial.instance);
            assert_eq!(actual.calls, expected_calls);
            assert_eq!(actual.events, expected_events);
        }

        // The last accepted exclusive end is MAX. Without relocating that same
        // context, one more sample must refuse; no saturated/wrapped continuation.
        let before = audit(&plugin);
        let mut buffers = plugin.create_bus_audio_buffers(1).unwrap();
        for channel in &mut buffers.outputs[0].channels {
            channel.fill(123.0);
        }
        let pcm_before = buffers.clone();
        let result = invoke(
            &mut plugin,
            &mut buffers,
            &[note(true, 0, 0, 902, 0.5)],
            scoped,
        );
        assert!(matches!(
            result,
            Err(vst3_host::Error::NativeEvent(
                NativeEventError::InvalidFrames
            ))
        ));
        assert_eq!(buffers, pcm_before);
        assert_eq!(audit(&plugin), before);

        // A distinct, representable window remains usable after the pure refusal.
        plugin
            .set_process_timeline_context(ProcessTimelineContext {
                sample_position: -9,
                sample_rate: RATE as u32,
            })
            .unwrap();
        let events = [note(true, 0, 0, 1004, 0.5), note(false, 0, 8, 1004, 0.125)];
        let (buffers, result) = process(&mut plugin, 9, &events, scoped);
        result.unwrap();
        assert_pcm(
            &buffers,
            &[0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.0],
        );
        let first = expected_events.len() as u32;
        let call = expected_calls.len() as u32 + 1;
        expected_calls.extend([(8, 0, first, 1), (1, 0, first + 1, 1)]);
        expected_events.extend([
            expected_tuple(call, &events[0], 0),
            expected_tuple(call + 1, &events[1], 0),
        ]);
        let final_audit = audit(&plugin);
        assert_eq!(final_audit.instance, initial.instance);
        assert_eq!(final_audit.calls, expected_calls);
        assert_eq!(
            final_audit.events, expected_events,
            "refused Notes cannot replay"
        );
        plugin.stop_processing().unwrap();
    }
}

// Same real object and existing DLL class; no new host/API/clock authority.
fn open_anchored_main(host: &mut Vst3Host, retain: bool, fail: u32) -> Plugin {
    let mut plugin = host.load_plugin_class(fixture(), CLASS).unwrap();
    let mut state = main_configuration(fail);
    if retain {
        // Same versioned 12-byte component/controller config, independent MNE5.
        for offset in [28, 40] {
            state[offset..offset + 4].copy_from_slice(b"MNE5");
            state[offset + 4..offset + 8].copy_from_slice(&5_u32.to_le_bytes());
        }
    }
    plugin.load_state(&state).unwrap();
    let info = plugin.native_event_bus_info().unwrap();
    assert_eq!(info.inputs.len(), 2);
    assert!(info.outputs.is_empty());
    assert_eq!(
        info.inputs.iter().map(|row| row.host_active).collect::<Vec<_>>(),
        [true, false]
    );
    plugin.set_process_mode(ProcessMode::Offline).unwrap();
    plugin.start_processing().unwrap();
    plugin
}

#[test]
#[ignore = "requires rebuilt MONDRIAN_VST3_NOTE_ORACLE_PLUGIN and existing absolute MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR; SDK anchored checked Notes, not canonical routing"]
fn installed_vst3_anchored_checked_notes_preserve_literal_ppq_pcm_and_failure_boundaries() {
    use vst3_host::audio::{ProcessMusicalReference, ProcessTimelineContext};
    use vst3_host::error::NativeEventError;
    let directory = PathBuf::from(std::env::var_os("MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR").unwrap());
    assert!(directory.is_absolute() && directory.is_dir());
    let path = directory.join(format!("mne4-{}.audit", std::process::id()));
    let prefix = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len() as usize,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("audit metadata failed: {error}"),
    };
    // Generated independently from exact fractions, never SDK projection output.
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
    let mut expected_records = Vec::new();
    for (rate_index, rate) in [44_100_u32, 48_000].into_iter().enumerate() {
        let mut host = Vst3Host::builder()
            .sample_rate(f64::from(rate))
            .block_size(4)
            .input_channels(2)
            .output_channels(2)
            .build()
            .unwrap();
        for scoped in [false, true] {
            for near_max in [false, true] {
                let mut plugin = open_anchored_main(&mut host, false, 0);
                let (start, reference) = if near_max {
                    (
                        i64::MAX - 16,
                        ProcessMusicalReference::new(i64::MAX - 17, i64::from(rate), 1, 2).unwrap(),
                    )
                } else {
                    (-17, ProcessMusicalReference::new(-19, 7, 3, 4).unwrap())
                };
                let events = [
                    note(true, 0, 0, 10, 0.5),
                    note(false, 0, 3, 10, 0.125),
                    note(true, 0, 4, 11, 0.25),
                    note(false, 0, 8, 11, 0.125),
                ];
                plugin
                    .set_process_musical_timeline_context(
                        ProcessTimelineContext { sample_position: i64::MAX - 4, sample_rate: rate },
                        reference,
                    )
                    .unwrap();
                let mut rejected = plugin.create_bus_audio_buffers(9).unwrap();
                for channel in &mut rejected.outputs[0].channels {
                    channel.fill(123.0);
                }
                let before = rejected.clone();
                assert!(matches!(
                    invoke(&mut plugin, &mut rejected, &events, scoped),
                    Err(vst3_host::Error::NativeEvent(
                        NativeEventError::InvalidFrames
                    ))
                ));
                assert_eq!(rejected, before);
                plugin
                    .set_process_musical_timeline_context(
                        ProcessTimelineContext { sample_position: start, sample_rate: rate },
                        reference,
                    )
                    .unwrap();
                let mut invalid = events.clone();
                invalid[3].ppq_position = f64::NAN;
                assert!(matches!(
                    invoke(&mut plugin, &mut rejected, &invalid, scoped),
                    Err(vst3_host::Error::NativeEvent(
                        NativeEventError::InvalidEvent { index: 3 }
                    ))
                ));
                assert_eq!(rejected, before);
                invoke(&mut plugin, &mut rejected, &events, scoped).unwrap();
                assert_pcm(
                    &rejected,
                    &[0.25, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0],
                );
                let mut projected = events.clone();
                for (event, bits) in projected.iter_mut().zip(Q[rate_index][usize::from(near_max)])
                {
                    event.ppq_position = f64::from_bits(bits);
                }
                let mut tuples = vec![
                    expected_tuple(1, &projected[0], 0),
                    expected_tuple(1, &projected[1], 3),
                    expected_tuple(2, &projected[2], 0),
                    expected_tuple(3, &projected[3], 0),
                ];
                // Clearing Some on the same prepared native object restores caller PPQ responsibility.
                plugin
                    .set_process_timeline_context(ProcessTimelineContext {
                        sample_position: 100,
                        sample_rate: rate,
                    })
                    .unwrap();
                invoke(&mut plugin, &mut rejected, &events, scoped).unwrap();
                assert_pcm(
                    &rejected,
                    &[0.25, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0],
                );
                tuples.extend([
                    expected_tuple(4, &events[0], 0),
                    expected_tuple(4, &events[1], 3),
                    expected_tuple(5, &events[2], 0),
                    expected_tuple(6, &events[3], 0),
                ]);
                plugin.stop_processing().unwrap();
                drop(plugin);
                expected_records.push((
                    rate,
                    vec![
                        (4, 0, 0, 2),
                        (4, 0, 2, 1),
                        (1, 0, 3, 1),
                        (4, 0, 4, 2),
                        (4, 0, 6, 1),
                        (1, 0, 7, 1),
                    ],
                    tuples,
                ));
            }
        }
    }
    // Actual middle-native failure: successful prefix may have advanced. No
    // rollback, retry or replay of that batch's unexecuted final Off is allowed.
    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(4)
        .input_channels(2)
        .output_channels(2)
        .build()
        .unwrap();
    for scoped in [false, true] {
        let mut plugin = open_anchored_main(&mut host, false, 2);
        let reference = ProcessMusicalReference::new(-19, 7, 3, 4).unwrap();
        plugin
            .set_process_musical_timeline_context(
                ProcessTimelineContext { sample_position: -17, sample_rate: 48_000 },
                reference,
            )
            .unwrap();
        let events = [
            note(true, 0, 0, 10, 0.5),
            note(false, 0, 3, 10, 0.125),
            note(true, 0, 4, 11, 0.25),
            note(false, 0, 8, 11, 0.125),
        ];
        let mut pcm = plugin.create_bus_audio_buffers(9).unwrap();
        for channel in &mut pcm.outputs[0].channels {
            channel.fill(123.0);
        }
        assert!(matches!(
            invoke(&mut plugin, &mut pcm, &events, scoped),
            Err(vst3_host::Error::ProcessFailed(1))
        ));
        assert_eq!(pcm.outputs[0].channels[0][..4], [0.25, 0.25, 0.25, 0.0]);
        assert_eq!(
            pcm.outputs[0].channels[1][..4],
            [-0.125, -0.125, -0.125, 0.0]
        );
        assert!(pcm.outputs[0].channels.iter().all(|channel| channel[4..] == [123.0; 5]));
        let (empty, result) = process(&mut plugin, 1, &[], scoped);
        result.unwrap();
        assert_pcm(&empty, &[0.0]);
        let mut projected = events.clone();
        for (event, bits) in projected.iter_mut().zip(Q[1][0]) {
            event.ppq_position = f64::from_bits(bits);
        }
        plugin.stop_processing().unwrap();
        drop(plugin);
        expected_records.push((
            48_000,
            vec![(4, 0, 0, 2), (4, 1, 2, 1), (1, 0, 3, 0)],
            vec![
                expected_tuple(1, &projected[0], 0),
                expected_tuple(1, &projected[1], 3),
                expected_tuple(2, &projected[2], 0),
            ],
        ));
    }
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.len() >= prefix && bytes.len() <= 2 * 1024 * 1024);
    let records = main_file_audits(&bytes[prefix..]);
    assert_eq!(records.len(), 10);
    assert_eq!(records.len(), expected_records.len());
    for (actual, (rate, calls, events)) in records.iter().zip(expected_records) {
        assert_eq!(actual.sample_rate_bits, f64::from(rate).to_bits());
        assert_eq!(actual.max_frames, 4);
        assert_eq!(
            actual.calls, calls,
            "invalid whole windows/batches made no native callback"
        );
        assert_eq!(
            actual.events, events,
            "independent literal PPQ and exact native tuple history"
        );
    }
}

#[test]
#[ignore = "requires rebuilt MONDRIAN_VST3_NOTE_ORACLE_PLUGIN; actual MNE5 AddRef across process and cold empty-view query/Release"]
fn installed_vst3_anchored_note_list_retains_real_com_lifetime_and_closes_between_calls() {
    use vst3_host::audio::{ProcessMusicalReference, ProcessTimelineContext};
    fn observation(plugin: &Plugin) -> (u64, u32, u32, i32) {
        let bytes = plugin.save_state().expect("joined control-side retained COM query");
        assert_eq!(&bytes[..16], b"VST3HOST_STATE\0\0");
        assert_eq!((u32_at(&bytes, 20), u32_at(&bytes, 24)), (44, 12));
        assert_eq!(bytes.len(), 28 + 44 + 12);
        let body = &bytes[28..72];
        assert_eq!(&body[..4], b"MNE5");
        assert_eq!(u32_at(body, 4), 5);
        assert_eq!(&body[12..16], b"MNR5");
        assert_eq!(u32_at(body, 16), 5);
        assert_eq!(u32_at(body, 28), std::process::id());
        let instance = u64::from_le_bytes(body[20..28].try_into().unwrap());
        assert_ne!(instance, 0);
        (
            instance,
            u32_at(body, 32),
            u32_at(body, 36),
            i32::from_le_bytes(body[40..44].try_into().unwrap()),
        )
    }
    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(4)
        .input_channels(2)
        .output_channels(2)
        .build()
        .unwrap();
    for scoped in [false, true] {
        let mut plugin = open_anchored_main(&mut host, true, 0);
        let before = observation(&plugin);
        assert_eq!((before.1, before.2, before.3), (0, 0, -1));
        for pass in 0..2 {
            plugin
                .set_process_musical_timeline_context(
                    ProcessTimelineContext {
                        sample_position: -17 + pass * 9,
                        sample_rate: 48_000,
                    },
                    ProcessMusicalReference::new(-19, 7, 3, 4).unwrap(),
                )
                .unwrap();
            let events = [
                note(true, 0, 0, 10, 0.5),
                note(false, 0, 3, 10, 0.125),
                note(true, 0, 4, 11, 0.25),
                note(false, 0, 8, 11, 0.125),
            ];
            let (pcm, result) = process(&mut plugin, 9, &events, scoped);
            result.unwrap();
            assert_pcm(
                &pcm,
                &[0.25, 0.25, 0.25, 0.0, 0.125, 0.125, 0.125, 0.125, 0.0],
            );
            // No query between these two calls: the plugin holds the same real
            // AddRef across both SDK windows, not just while process() is on stack.
        }
        let after = observation(&plugin);
        assert_eq!(after, (before.0, 1, 1, 0));
        assert_eq!(
            observation(&plugin),
            after,
            "without a retained view no fake query/release is counted"
        );
        let state = plugin.save_state().unwrap();
        plugin.stop_processing().unwrap();
        for field in [28 + 12, 28 + 16] {
            let mut invalid = state.clone();
            invalid[field] ^= 1;
            assert!(
                plugin.load_state(&invalid).is_err(),
                "MNE5 rejects malformed trailer magic/version"
            );
            assert_eq!(observation(&plugin), after);
        }
        for extra in [false, true] {
            let mut invalid = state.clone();
            if extra {
                invalid.insert(28 + 44, 0);
                invalid[20..24].copy_from_slice(&45_u32.to_le_bytes());
            } else {
                invalid.remove(28 + 43);
                invalid[20..24].copy_from_slice(&43_u32.to_le_bytes());
            }
            assert!(
                plugin.load_state(&invalid).is_err(),
                "MNE5 rejects truncated/oversized transient audit"
            );
            assert_eq!(observation(&plugin), after);
        }
        plugin.load_state(&state).unwrap();
        assert_eq!(
            observation(&plugin),
            after,
            "configuration restore cannot rewind lifetime audit"
        );
        plugin.stop_processing().unwrap();
    }
}
