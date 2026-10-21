//! Stopped external VST3 observations through the existing SDK loader.
//! The original inventory gate does not capture state. The separate bounded
//! capture gate supplies no restore blob and processes no PCM. Neither gate
//! qualifies Product admission or the plug-in's audio behavior.
#![cfg(target_os = "windows")]

use std::path::PathBuf;
use vst3_host::audio::{AudioBusQueryFrame, AudioBusRole, MAX_OBSERVED_AUDIO_BUSES};
use vst3_host::host::PluginLoadPreparation;
use vst3_host::plugin::ProcessMode;
use vst3_host::{PluginInterface, PluginInterfaceSupport, Vst3Host};

fn load_external_stopped_plugin() -> (Vst3Host, vst3_host::Plugin, String) {
    let path = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_INVENTORY_PLUGIN")
            .expect("set MONDRIAN_VST3_INVENTORY_PLUGIN to an existing absolute VST3 DLL"),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "external VST3 DLL must exist"
    );
    let class_id = std::env::var("MONDRIAN_VST3_INVENTORY_CLASS_ID")
        .expect("set MONDRIAN_VST3_INVENTORY_CLASS_ID to the exact native class ID")
        .to_ascii_uppercase();
    assert!(
        class_id.len() == 32 && class_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "external class ID must contain exactly 32 hexadecimal digits"
    );

    // Match the Windows Product cold probe's interface/profile and no-state
    // sequence, without importing Audio/Timeline or applying its shape policy.
    let interfaces = PluginInterfaceSupport::none()
        .with(PluginInterface::Component)
        .with(PluginInterface::AudioProcessor)
        .with(PluginInterface::EditController)
        .with(PluginInterface::ConnectionPoint)
        .with(PluginInterface::ProcessContextRequirements)
        .with(PluginInterface::PlugView)
        .with(PluginInterface::PlugViewContentScaleSupport)
        .with(PluginInterface::DataExchangeReceiver);
    let mut host = Vst3Host::builder()
        .plugin_interfaces(interfaces)
        .initial_process_mode(ProcessMode::Offline)
        .sample_rate(48_000.0)
        .block_size(256)
        .input_channels(2)
        .output_channels(2)
        .load_preparation(PluginLoadPreparation::Eager)
        .with_process_isolation(false)
        .build()
        .expect("SDK host matching the Product cold probe profile");
    // Eager SDK load itself activates declared default buses, negotiates their
    // advertised arrangements, calls setupProcessing and attempts component
    // activation. A separate controller may receive initial component-state
    // synchronization too. Stopped does not mean inactive or no native actions.
    let mut plugin = host
        .load_plugin_class(&path, &class_id)
        .expect("load exact external class");
    assert_eq!(
        plugin.info().uid,
        class_id,
        "a replacement class is not the selected class"
    );
    // The Product probe also repeats this setter after load. It reruns setup
    // while stopped, including component deactivation/reactivation when needed.
    // No state restore, explicit bus negotiation/activation, start or DSP follows.
    plugin
        .set_process_mode(ProcessMode::Offline)
        .expect("same stopped Offline setup as probe");
    assert_eq!(plugin.sample_rate(), 48_000.0);
    assert_eq!(plugin.block_size(), 256);
    assert!(!plugin.is_processing());

    (host, plugin, class_id)
}

#[test]
#[ignore = "requires an explicit external VST3 DLL and class ID in the native Job"]
fn external_vst3_stopped_bounded_audio_bus_inventory() {
    let (host, plugin, class_id) = load_external_stopped_plugin();
    let mut frame = AudioBusQueryFrame::default();
    let observed = plugin
        .observe_audio_buses_bounded(&mut frame)
        .expect("complete stopped metadata/layout/arrangement observation");
    let directions = [
        (
            "input",
            observed.native_inputs,
            observed.layout_inputs,
            observed.input_arrangements,
        ),
        (
            "output",
            observed.native_outputs,
            observed.layout_outputs,
            observed.output_arrangements,
        ),
    ];
    // Validate both complete extents before materializing or publishing any row.
    // The SDK bounds each stage to 32; matching extents preserves native indexes.
    // These census limits do not bound eager loading, startup or the plug-in heap.
    for (direction, native, prepared, arrangements) in directions {
        assert!(
            native.len() <= MAX_OBSERVED_AUDIO_BUSES,
            "{direction} native bound"
        );
        assert_eq!(
            native.len(),
            prepared.len(),
            "{direction} native/prepared extent"
        );
        assert_eq!(
            native.len(),
            arrangements.len(),
            "{direction} native/arrangement extent"
        );
    }
    let mut rows = Vec::with_capacity(observed.native_inputs.len() + observed.native_outputs.len());
    for (direction, native, prepared, arrangements) in directions {
        for (index, ((native, prepared), arrangement)) in
            native.iter().zip(prepared).zip(arrangements).enumerate()
        {
            rows.push(serde_json::json!({
                "direction": direction,
                "index": index,
                "native_role": match native.role {
                    AudioBusRole::Main => "main",
                    AudioBusRole::Auxiliary => "auxiliary",
                },
                "native_channels": native.channel_count,
                "prepared_channels": prepared.channel_count,
                "host_active": prepared.active,
                "raw_arrangement": arrangement.raw(),
            }));
        }
    }
    assert!(!plugin.is_processing());
    let report = serde_json::json!({
        "scope": "sdk_stopped_audio_bus_inventory",
        "class_id": class_id,
        "phase": "after_eager_load_and_stopped_offline_setup",
        "sample_rate": 48000,
        "requested_input_channels": 2,
        "requested_output_channels": 2,
        "max_block_frames": 256,
        "process_mode": "offline",
        "state": null,
        "input_count": observed.native_inputs.len(),
        "output_count": observed.native_outputs.len(),
        // Each fresh arrangement is published only after kResultOk. A failed
        // query rejects the complete census before this report can be emitted.
        "arrangement_query_status": "checked_success",
        "buses": rows,
    });
    // All native owners are destroyed on this same test thread before stdout.
    // host_active above is the retained host ACK, not a native GetActive query.
    drop(plugin);
    drop(host);
    println!("{report}");
}

#[test]
#[ignore = "requires an explicit external VST3 DLL/class and the original 10s Job with 2s cleanup"]
fn external_vst3_stopped_bounded_default_state_capture() {
    use std::io::Write as _;

    const STATE_LIMIT: usize = 128 * 1024;
    const BUFFER_GRANT: usize = 3 * STATE_LIMIT;
    const HEADER_BYTES: usize = 28;
    // The grant covers capture buffers, not Eager loading or the native heap.
    // Only the caller may authorize a raw output path. No default path or
    // directory is created, and an existing artifact is never overwritten.
    let output = std::env::var_os("MONDRIAN_VST3_INVENTORY_STATE_OUTPUT").map(PathBuf::from);
    if let Some(path) = &output {
        assert!(
            path.is_absolute(),
            "explicit absolute state output required"
        );
        assert!(path.file_name().is_some(), "state output must name a file");
        assert!(
            path.parent().is_some_and(|parent| parent.is_dir()),
            "state output parent must already exist"
        );
        assert!(!path.exists(), "state output must be fresh");
    }
    let (host, mut plugin, class_id) = load_external_stopped_plugin();
    // This is the Eager/stopped SDK phase, before Product's complete bus and
    // parameter admission. Bounded streams expose IBStream only; Product's
    // legacy save_state stream additionally supplies Project attributes.
    let captured = plugin
        .save_state_bounded(STATE_LIMIT, BUFFER_GRANT)
        .expect("actual stopped bounded native state capture; pending input is not bypassed");
    assert!(!plugin.is_processing(), "capture must not start processing");
    let receipt = captured.buffer_receipt();
    let bytes = captured.bytes();
    assert!(
        (HEADER_BYTES..=STATE_LIMIT).contains(&bytes.len()),
        "complete canonical state extent"
    );
    assert_eq!(&bytes[..16], b"VST3HOST_STATE\0\0");
    let word = |offset: usize| {
        u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("bounded state header"),
        )
    };
    assert_eq!(
        word(16),
        1,
        "canonical component/controller envelope version"
    );
    let component_bytes = word(20) as usize;
    let controller_bytes = match word(24) {
        u32::MAX => None,
        length => Some(length as usize),
    };
    assert_eq!(
        HEADER_BYTES
            .checked_add(component_bytes)
            .and_then(|n| n.checked_add(controller_bytes.unwrap_or(0))),
        Some(bytes.len()),
        "complete native state payload, without truncation"
    );
    assert!(receipt.envelope_capacity_bytes >= bytes.len());
    let stream_extent = STATE_LIMIT - HEADER_BYTES;
    assert!(
        receipt.stream_payload_bytes == stream_extent
            || receipt.stream_payload_bytes == 2 * stream_extent,
        "one component stream and at most one separate controller stream"
    );
    let peak = receipt
        .peak_buffer_bytes()
        .expect("checked capture buffer receipt");
    assert!(
        peak <= BUFFER_GRANT,
        "capture buffers remain within the original SDK grant"
    );
    let bytes = captured.into_bytes();
    let raw_bytes = bytes.len();
    // No native owner remains while writing the optional bounded raw artifact
    // or publishing the successful observation. All failures above stay FAIL.
    drop(plugin);
    drop(host);
    if let Some(path) = &output {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("fresh explicitly authorized state output");
        file.write_all(&bytes)
            .expect("write the exact bounded raw state");
        file.flush().expect("complete bounded state output");
        assert_eq!(
            file.metadata().expect("state output metadata").len(),
            raw_bytes as u64
        );
    }
    let report = serde_json::json!({
        "scope": "sdk_stopped_bounded_default_state_capture",
        "class_id": class_id,
        "phase": "after_eager_load_and_stopped_offline_setup",
        "sample_rate": 48000,
        "max_block_frames": 256,
        "process_mode": "offline",
        "stream_attributes": "unavailable",
        "raw_state_bytes": raw_bytes,
        "component_bytes": component_bytes,
        "controller_bytes": controller_bytes,
        "state_limit_bytes": STATE_LIMIT,
        "stream_payload_allocation_bytes": receipt.stream_payload_bytes,
        "envelope_capacity_bytes": receipt.envelope_capacity_bytes,
        "peak_capture_buffer_bytes": peak,
        "capture_buffer_grant_bytes": BUFFER_GRANT,
        "raw_output_written": output.is_some(),
    });
    println!("{report}");
}

#[derive(Debug, PartialEq)]
struct ParameterCensusRow {
    id: u32,
    title: [u16; 128],
    default_bits: u64,
    step_count: i32,
    flags: u32,
    current_bits: u64,
}

fn surge_current_parameter_census(plugin: &vst3_host::Plugin) -> Vec<ParameterCensusRow> {
    use vst3_host::controller_parameters::ControllerParameterQueryFrame;

    const COUNT: usize = 2_855;
    let mut frame = ControllerParameterQueryFrame::default();
    let mut rows = Vec::with_capacity(COUNT);
    let count = plugin
        .visit_controller_parameters_bounded(COUNT, &mut frame, &mut |index, actual| {
            assert_eq!(index, rows.len(), "complete native ordinal");
            assert!(
                !actual.is_read_only(),
                "the complete actual Surge census is writable"
            );
            assert!(actual.step_count() >= 0);
            assert!(actual.default_normalized().is_finite());
            assert!((0.0..=1.0).contains(&actual.default_normalized()));
            assert!(actual.normalized().is_finite());
            assert!((0.0..=1.0).contains(&actual.normalized()));
            assert!(!rows
                .iter()
                .any(|row: &ParameterCensusRow| row.id == actual.id()));
            rows.push(ParameterCensusRow {
                id: actual.id(),
                title: *actual.title_utf16(),
                default_bits: actual.default_normalized().to_bits(),
                step_count: actual.step_count(),
                flags: actual.flags(),
                current_bits: actual.normalized().to_bits(),
            });
            Ok(())
        })
        .expect("full bounded native controller census, without a drain or setter");
    assert_eq!(count, COUNT);
    assert_eq!(rows.len(), COUNT);
    let can_automate = vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::kCanAutomate as u32;
    assert_eq!(
        rows.iter()
            .filter(|row| row.flags & can_automate != 0)
            .count(),
        774
    );
    rows
}

#[derive(Clone, Copy, Default, serde::Serialize)]
struct NoteWindowObservation {
    energy: f64,
    peak: f32,
    nonzero: usize,
}

#[test]
#[ignore = "requires explicit real Surge XT VST3/class, actual 51928B state input and the 180s native Job with 2s cleanup"]
fn external_surge_vst3_full_parameter_replay_sustained_note_ab() {
    use std::collections::BTreeMap;
    use std::io::Read as _;
    use vst3_host::audio::{
        BusDirection, MediaType, NativeEventBusRole, ProcessMusicalReference,
        ProcessTimelineContext,
    };
    use vst3_host::initial_parameter_input::{InitialParameterStorage, InitialParameterValue};
    use vst3_host::plugin::NativeEditorEvent;
    use vst3_host::{PluginEvent, PluginEventData};

    // This test crate has no windows-sys dependency. Query only membership via
    // the existing kernel API; Root's caller must qualify the complete Job tree.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn IsProcessInJob(
            process: *mut std::ffi::c_void,
            job: *mut std::ffi::c_void,
            result: *mut i32,
        ) -> i32;
    }
    let mut in_job = 0;
    // SAFETY: a valid process pseudo handle and one call-local writable BOOL.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "the original native Job must own both SDK arms");

    const CLASS: &str = "ABCDEF019182FAEB566D624153675854";
    const BLOCK: usize = 256;
    const BLOCKS: usize = 48;
    const ON: usize = 127;
    const OFF: usize = 8_193;
    const MID_START: usize = 8;
    const MID_END: usize = 24;
    const MIN_A_MIDDLE_ENERGY: f64 = 1e-12;
    const MAX_B_A_MIDDLE_RATIO: f64 = 1e-6;
    // Predeclared windows and criteria are immutable in this diagnostic. A
    // passing differential supports whole-census replay suppression only; it
    // cannot identify a particular MIDI proxy or qualify Product processing.
    const {
        assert!(ON < BLOCK && OFF / BLOCK == 32 && OFF % BLOCK == 1);
        assert!(MID_START == 8 && MID_END == 24 && MID_END * BLOCK < OFF);
        assert!(BLOCKS == 48);
    }
    let state_path = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_INVENTORY_STATE_INPUT")
            .expect("explicit existing actual Surge capture, never generated default bytes"),
    );
    assert!(state_path.is_absolute() && state_path.is_file());
    let mut state_file = std::fs::File::open(&state_path).expect("actual captured state input");
    assert_eq!(state_file.metadata().unwrap().len(), 51_928);
    let mut state = Vec::with_capacity(51_928);
    std::io::Read::by_ref(&mut state_file)
        .take(51_929)
        .read_to_end(&mut state)
        .unwrap();
    assert_eq!(state.len(), 51_928, "bounded actual capture extent");
    assert_eq!(&state[..16], b"VST3HOST_STATE\0\0");
    let word = |offset: usize| u32::from_le_bytes(state[offset..offset + 4].try_into().unwrap());
    assert_eq!((word(16), word(20), word(24)), (1, 51_900, u32::MAX));
    drop(state_file);
    let _native_main = vst3_host::window::WindowsEditorMainThread::initialize().unwrap();
    let reference = ProcessMusicalReference::new(0, 1, 1, 2).unwrap();
    let mut expected_census: Option<Vec<ParameterCensusRow>> = None;
    let mut expected_initial_source: Option<(bool, bool, Vec<NativeEditorEvent>, Vec<(u32, f64)>)> =
        None;
    let mut reports = Vec::with_capacity(2);
    let mut middle_energy = [0.0_f64; 2];
    let mut middle_nonzero_blocks = [0_usize; 2];
    let mut pre_on_nonzero = [0_usize; 2];
    let mut final_source_valid = [false; 2];
    for (arm, replay) in [false, true].into_iter().enumerate() {
        let (host, mut plugin, class_id) = load_external_stopped_plugin();
        assert_eq!(class_id, CLASS);
        assert_eq!(plugin.info().uid, CLASS);
        assert_eq!(plugin.info().name, "Surge XT");
        assert_eq!(plugin.info().vendor, "Surge Synth Team");
        plugin
            .load_state(&state)
            .expect("fresh native owner restores the identical actual state");
        plugin.set_process_mode(ProcessMode::Offline).unwrap();
        let layout = plugin.audio_bus_layout().unwrap();
        assert_eq!((layout.inputs.len(), layout.outputs.len()), (1, 3));
        for (direction, buses) in [
            (BusDirection::Input, &layout.inputs),
            (BusDirection::Output, &layout.outputs),
        ] {
            for (index, bus) in buses.iter().enumerate() {
                assert_eq!(bus.channel_count, 2);
                if !bus.active {
                    plugin
                        .set_bus_active(MediaType::Audio, direction, index as i32, true)
                        .unwrap();
                }
            }
        }
        let audio = plugin.native_audio_bus_info().unwrap();
        assert_eq!((audio.inputs.len(), audio.outputs.len()), (1, 3));
        assert_eq!(audio.inputs[0].role, AudioBusRole::Auxiliary);
        assert_eq!(audio.inputs[0].channel_count, 2);
        assert!(audio
            .outputs
            .iter()
            .all(|bus| bus.role == AudioBusRole::Main && bus.channel_count == 2));
        let events = plugin.native_event_bus_info().unwrap();
        assert_eq!(events.inputs.len(), 1);
        assert!(events.outputs.is_empty());
        assert_eq!(events.inputs[0].role, NativeEventBusRole::Main);
        assert_eq!(events.inputs[0].channel_count, 16);
        assert!(events.inputs[0].host_active);
        let mut buffers = plugin.create_bus_audio_buffers(BLOCK).unwrap();
        assert_eq!((buffers.inputs.len(), buffers.outputs.len()), (1, 3));
        assert!(buffers
            .inputs
            .iter()
            .chain(&buffers.outputs)
            .all(|bus| bus.active
                && bus.channels.len() == 2
                && bus.channels.iter().all(|channel| channel.len() == BLOCK)));
        plugin.start_processing().unwrap();
        let owner = plugin
            .native_editor_attachment_owner()
            .expect("same local native object");
        let cohort = plugin
            .pending_restart_cohort()
            .expect("actual raw initial cohort");
        assert!(!cohort.exhausted() && matches!(cohort.flags().bits(), 0 | 4 | 20));
        let source = plugin.editor_event_source_status().unwrap();
        assert_eq!(source.failure(), None);
        assert_eq!(
            (source.open_parameter_edits(), source.group_depth()),
            (0, 0)
        );
        let census = surge_current_parameter_census(&plugin);
        if let Some(expected) = &expected_census {
            assert_eq!(census.len(), expected.len());
            for (index, (actual, expected)) in census.iter().zip(expected).enumerate() {
                assert_eq!(
                    actual, expected,
                    "fresh native descriptor/current ordinal {index}"
                );
            }
        }
        assert_eq!(plugin.pending_restart_cohort(), Some(cohort));
        assert_eq!(plugin.editor_event_source_status().unwrap(), source);
        let values: Vec<_> = census
            .iter()
            .map(|row| InitialParameterValue {
                id: row.id,
                normalized: f64::from_bits(row.current_bits),
            })
            .collect();
        let input_before = plugin.pending_processor_parameter_input().unwrap();
        let mut source_events = Vec::with_capacity(4_096);
        let mut source_feedback = Vec::with_capacity(4_096);
        let observed_source = plugin
            .observe_editor_source_bounded(4_096, &mut source_events, &mut source_feedback)
            .expect("complete non-consuming initial Source and feedback observation");
        assert_eq!(observed_source, source);
        assert_eq!(source_events.len(), source.pending_events());
        assert_eq!(source_feedback.len(), source.pending_dsp_feedback_values());
        // Passing the complete observations does NOT enqueue their queried
        // values. The original producer alone stages actual intact Value
        // callbacks; markers consume no points. A real nonempty input cannot be
        // discarded or flushed to make this differential fit its precondition.
        plugin
            .handoff_initial_parameter_input(
                cohort,
                source,
                &values,
                InitialParameterStorage::for_parameter_count(values.len()).unwrap(),
            )
            .expect("original complete Source/cohort initial-only handoff");
        let after = plugin.editor_event_source_status().unwrap();
        let after_cohort = plugin.pending_restart_cohort().unwrap();
        let input_after = plugin.pending_processor_parameter_input().unwrap();
        assert!(after.closed_empty());
        assert_eq!(after.pending_dsp_feedback_values(), 0);
        assert_eq!(after_cohort.serial(), cohort.serial());
        assert_eq!(after_cohort.flags().bits(), 0);
        // Legal input produced by this exact Source is common to A/B. Its
        // original COM points remain for the first normal PCM/Note call; no
        // empty-input fiction, separate warm-up, flush or replacement is used.
        if let Some((expected_before, expected_after, expected_events, expected_feedback)) =
            &expected_initial_source
        {
            assert_eq!(
                (input_before, input_after),
                (*expected_before, *expected_after)
            );
            assert_eq!(source_events.len(), expected_events.len());
            for (index, (actual, expected)) in source_events.iter().zip(expected_events).enumerate()
            {
                match (actual, expected) {
                    (
                        NativeEditorEvent::Parameter(actual),
                        NativeEditorEvent::Parameter(expected),
                    ) => {
                        assert_eq!(actual.id, expected.id, "Source parameter {index}");
                        assert_eq!(actual.kind, expected.kind, "Source phase {index}");
                        assert_eq!(
                            actual.value.map(f64::to_bits),
                            expected.value.map(f64::to_bits),
                            "Source value {index}"
                        );
                    }
                    (
                        NativeEditorEvent::ParameterRescanRequested { flags: actual, .. },
                        NativeEditorEvent::ParameterRescanRequested {
                            flags: expected, ..
                        },
                    ) => {
                        assert_eq!(actual, expected, "Source rescan request {index}");
                    }
                    _ => {
                        panic!("incomparable initial Source event {index}: {actual:?}/{expected:?}")
                    }
                }
            }
            assert_eq!(source_feedback.len(), expected_feedback.len());
            for (index, (&(id, value), &(expected_id, expected_value))) in
                source_feedback.iter().zip(expected_feedback).enumerate()
            {
                assert_eq!(
                    (id, value.to_bits()),
                    (expected_id, expected_value.to_bits()),
                    "common initial Source feedback {index}"
                );
            }
        }
        // Mirror Product's existing ParameterId(String) BTreeMap order and its
        // stepped scalar projection, including all 2081 nonautomatable rows.
        // Neither native name, flags nor the 774 automation subset selects DSP.
        let mut ordered = BTreeMap::new();
        for row in &census {
            let current = f64::from_bits(row.current_bits);
            let normalized = if row.step_count > 0 {
                (current * f64::from(row.step_count)).round() / f64::from(row.step_count)
            } else {
                current
            };
            assert!(ordered
                .insert(format!("vst3.param.{}", row.id), (row.id, normalized))
                .is_none());
        }
        let overrides: Vec<_> = ordered.into_values().collect();
        assert_eq!(overrides.len(), 2_855);
        let on = [PluginEvent {
            bus_index: 0,
            sample_offset: ON as i32,
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
        let off = [PluginEvent {
            bus_index: 0,
            sample_offset: (OFF % BLOCK) as i32,
            ppq_position: 0.0,
            flags: 0,
            data: PluginEventData::NoteOff {
                channel: 0,
                pitch: 60,
                tuning: 0.0,
                velocity: 0.25,
                note_id: 0,
            },
        }];
        let vst3_host::plugin::PluginEditorProcessingSplit {
            editor,
            mut processor,
        } = plugin.split_editor_processing().unwrap();
        let observations = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let mut observations = [NoteWindowObservation::default(); BLOCKS];
                    for (block, observed) in observations.iter_mut().enumerate() {
                        processor
                            .set_process_musical_timeline_context(
                                ProcessTimelineContext {
                                    sample_position: (block * BLOCK) as i64,
                                    sample_rate: 48_000,
                                },
                                reference,
                            )
                            .unwrap();
                        if replay {
                            for &(id, normalized) in &overrides {
                                processor.queue_parameter_at(id, normalized, 0).unwrap();
                            }
                        }
                        // Match Product's zero-filled sidechain and all output buses.
                        buffers.clear();
                        let notes = if block == 0 {
                            &on[..]
                        } else if block == OFF / BLOCK {
                            &off[..]
                        } else {
                            &[]
                        };
                        processor
                            .process_bus_audio_with_note_events(&mut buffers, notes)
                            .expect("one checked native request without retry, flush or reset");
                        assert_eq!((buffers.inputs.len(), buffers.outputs.len()), (1, 3));
                        for bus in &buffers.outputs {
                            assert!(bus.active && bus.channels.len() == 2);
                            assert!(bus.channels.iter().all(|channel| channel.len() == BLOCK
                                && channel.iter().all(|sample| sample.is_finite())));
                        }
                        for channel in &buffers.outputs[0].channels {
                            for (offset, &sample) in channel.iter().enumerate() {
                                if block == 0 && offset < ON && sample != 0.0 {
                                    pre_on_nonzero[arm] += 1;
                                }
                                observed.energy += f64::from(sample).powi(2);
                                observed.peak = observed.peak.max(sample.abs());
                                observed.nonzero += usize::from(sample != 0.0);
                            }
                        }
                        assert!(observed.energy.is_finite());
                    }
                    observations
                })
                .join()
                .expect("joined same-owner SDK DSP role")
        });
        drop(processor);
        drop(editor);
        assert_eq!(plugin.native_editor_attachment_owner(), Some(owner));
        // Observe without draining/servicing callbacks or deferred controller
        // display synchronization. No post-On state capture or controller query.
        let final_source = plugin.editor_event_source_status().unwrap();
        let final_cohort = plugin.pending_restart_cohort().unwrap();
        let input_final = plugin.pending_processor_parameter_input().unwrap();
        final_source_valid[arm] = final_source.failure().is_none()
            && final_source.open_parameter_edits() == 0
            && final_source.group_depth() == 0
            && !input_final;
        middle_energy[arm] = observations[MID_START..MID_END]
            .iter()
            .map(|v| v.energy)
            .sum();
        middle_nonzero_blocks[arm] = observations[MID_START..MID_END]
            .iter()
            .filter(|v| v.energy > 0.0)
            .count();
        let attack_energy: f64 = observations[..4].iter().map(|v| v.energy).sum();
        let tail_energy: f64 = observations[40..48].iter().map(|v| v.energy).sum();
        let initial = serde_json::json!({
            "cohort_serial":cohort.serial(),"cohort_flags":cohort.flags().bits(),
            "source_revision":source.revision(),"source_events":source.pending_events(),
            "source_feedback":source.pending_dsp_feedback_values(),
            "pending_input_before":input_before,"pending_input_after":input_after,
            "remaining_events":after.pending_events(),"remaining_feedback":after.pending_dsp_feedback_values(),
            "remaining_cohort_serial":after_cohort.serial(),"remaining_cohort_flags":after_cohort.flags().bits(),
        });
        let final_status = serde_json::json!({
            "source_revision":final_source.revision(),"source_events":final_source.pending_events(),
            "source_feedback":final_source.pending_dsp_feedback_values(),
            "source_failure":final_source.failure(),"open_edits":final_source.open_parameter_edits(),
            "group_depth":final_source.group_depth(),"cohort_serial":final_cohort.serial(),
            "cohort_flags":final_cohort.flags().bits(),"pending_processor_input":input_final,
        });
        // Ordinary same-thread lifecycle only, after all 48 requests and the
        // fixed Off. There is no restart/stop/resume between NoteOn and NoteOff.
        drop(plugin);
        drop(host);
        reports.push(serde_json::json!({
            "arm":if replay {"B_full_product_constants"} else {"A_no_additional_host_constants"},
            "owner_serial":owner.serial().get(),"census_parameters":census.len(),
            "per_block_overrides":if replay {overrides.len()} else {0},
            "initial":initial,"after_fixed_dsp":final_status,
            "pre_on_nonzero_samples":pre_on_nonzero[arm],
            "attack_energy":attack_energy,"middle_energy":middle_energy[arm],
            "middle_nonzero_blocks":middle_nonzero_blocks[arm],"tail_energy":tail_energy,
            "main_blocks":observations.as_slice(),
        }));
        if expected_census.is_none() {
            expected_census = Some(census);
            expected_initial_source =
                Some((input_before, input_after, source_events, source_feedback));
        }
    }
    let supports_replay_suppression = final_source_valid.iter().all(|valid| *valid)
        && pre_on_nonzero == [0, 0]
        && middle_energy[0] > MIN_A_MIDDLE_ENERGY
        && middle_nonzero_blocks[0] == MID_END - MID_START
        && middle_energy[1] <= middle_energy[0] * MAX_B_A_MIDDLE_RATIO;
    println!(
        "{}",
        serde_json::json!({
            "scope":"sdk_surge_whole_census_replay_differential","class_id":CLASS,
            "sample_rate":48000,"block_frames":BLOCK,"blocks_per_arm":BLOCKS,
            "raw_state_bytes":state.len(),"note_on_sample":ON,"note_off_sample":OFF,
            "fixed_musical_reference":"quarter_zero_0_seconds_duration_1_over_2_seconds",
            "attack_window":[0,1024],"middle_window":[2048,6144],"tail_window":[10240,12288],
            "minimum_A_middle_energy_exclusive":MIN_A_MIDDLE_ENERGY,
            "maximum_B_A_middle_energy_ratio_inclusive":MAX_B_A_MIDDLE_RATIO,
            "supports_whole_replay_suppression":supports_replay_suppression,"arms":reports,
            "native_owners_dropped_before_report":true,"unserviced_source_preserved":true,
            "specific_midi_proxy_causality_qualified":false,"product_runtime_qualified":false,
            "plugin_algorithm_or_note_release_qualified":false,"realtime_qualified":false,
        })
    );
    assert!(
        final_source_valid.iter().all(|valid| *valid),
        "unexpected Source failure, open edit/group or retained processor input"
    );
    assert_eq!(
        pre_on_nonzero,
        [0, 0],
        "no unexpected pre-On PCM in either fresh native owner"
    );
    assert!(
        middle_energy[0] > MIN_A_MIDDLE_ENERGY,
        "native-state arm lacks the predeclared sustained-note reference"
    );
    assert_eq!(
        middle_nonzero_blocks[0],
        MID_END - MID_START,
        "every predeclared A middle block must contain actual sound"
    );
    assert!(
        supports_replay_suppression,
        "full replay suppression hypothesis did not meet the predeclared differential"
    );
}
