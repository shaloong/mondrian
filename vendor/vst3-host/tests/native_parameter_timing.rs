//! Direct SDK queue timing control; no Product, worker or canonical evaluator.
#![cfg(target_os = "windows")]

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use vst3_host::audio::ProcessTimelineContext;
use vst3_host::controller_parameters::ControllerParameterQueryFrame;
use vst3_host::initial_parameter_input::{InitialParameterStorage, InitialParameterValue};
use vst3_host::plugin::ProcessMode;
use vst3_host::{PluginInterface, PluginInterfaceSupport, Vst3Host};

const CLASS_ID: &str = "C18D3C1E719E4E29924D3ECAA5E4DA18";
// Original captured bytes, compared with the caller's real seed file before load.
const CAPTURED_STATE: [u8; 56] = [
    86, 83, 84, 51, 72, 79, 83, 84, 95, 83, 84, 65, 84, 69, 0, 0, 1, 0, 0, 0, 28, 0, 0, 0, 255,
    255, 255, 255, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0,
];

fn explicit_path(variable: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(variable).expect(variable));
    assert!(path.is_absolute(), "{variable} must be absolute");
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).expect("existing explicit path");
        assert_eq!(
            metadata.file_attributes() & 0x400,
            0,
            "reparse point: {ancestor:?}"
        );
    }
    path
}

fn lease_file(path: &Path, bytes: u64) -> File {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1) // FILE_SHARE_READ: deny replacement/writes during all owners.
        .open(path)
        .expect("original read-only input lease");
    let metadata = file.metadata().unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.len(), bytes);
    file
}

fn write_pcm(path: &Path, frames: &[[f32; 2]]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    for frame in frames {
        for sample in frame {
            file.write_all(&sample.to_le_bytes()).unwrap();
        }
    }
}

#[test]
#[ignore = "explicit official AGain DLL/56B state, empty output directory and original native Job"]
fn external_again_parameter_queue_canonical_pcm() {
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
    // SAFETY: process pseudo handle and call-local BOOL are valid for this query.
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) },
        0
    );
    assert_ne!(in_job, 0, "the original caller must bound native cleanup");
    assert_eq!(
        std::env::var("MONDRIAN_VST3_INVENTORY_CLASS_ID").unwrap(),
        CLASS_ID
    );
    let library = explicit_path("MONDRIAN_VST3_INVENTORY_PLUGIN");
    let state_path = explicit_path("MONDRIAN_VST3_PARAMETER_TIMING_STATE");
    let output = explicit_path("MONDRIAN_VST3_PARAMETER_TIMING_OUTPUT");
    assert!(output.is_dir());
    assert_eq!(
        fs::read_dir(&output).unwrap().count(),
        0,
        "one empty current output directory"
    );
    // The existing caller pins the DLL hash; these handles retain those inputs.
    let library_lease = lease_file(&library, 734_720);
    let mut state_lease = lease_file(&state_path, 56);
    let mut state = [0; 56];
    state_lease.read_exact(&mut state).unwrap();
    assert_eq!(state, CAPTURED_STATE);
    fs::write(output.join("original-state.bin"), state).unwrap();
    let input = [[0.25_f32, -0.5_f32]; 8];
    write_pcm(&output.join("input.f32le"), &input);

    struct Case {
        name: &'static str,
        queue: &'static [(i32, f64)],
        expected: [[f32; 2]; 8],
    }
    // Independent literal PCM, not a host/SDK interpolation or shifted oracle.
    let cases = [
        Case {
            name: "constant",
            queue: &[(0, 0.5), (7, 0.5)],
            expected: [[0.125, -0.25]; 8],
        },
        Case {
            name: "ramp",
            queue: &[(0, 0.25), (4, 0.75), (7, 0.75)],
            expected: [
                [0.0625, -0.125],
                [0.09375, -0.1875],
                [0.125, -0.25],
                [0.15625, -0.3125],
                [0.1875, -0.375],
                [0.1875, -0.375],
                [0.1875, -0.375],
                [0.1875, -0.375],
            ],
        },
        Case {
            name: "jump",
            // VST3 represents a jump by adjacent old/new sample points.
            queue: &[(0, 0.25), (3, 0.25), (4, 0.75), (7, 0.75)],
            expected: [
                [0.0625, -0.125],
                [0.0625, -0.125],
                [0.0625, -0.125],
                [0.0625, -0.125],
                [0.1875, -0.375],
                [0.1875, -0.375],
                [0.1875, -0.375],
                [0.1875, -0.375],
            ],
        },
    ];
    let native_main = vst3_host::window::WindowsEditorMainThread::initialize().unwrap();
    let mut reports = Vec::with_capacity(cases.len());
    let mut total_mismatches = 0;
    for case in cases {
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
            .with_process_isolation(false)
            .initial_process_mode(ProcessMode::Offline)
            .sample_rate(48_000.0)
            .block_size(64)
            .input_channels(2)
            .output_channels(2)
            .build()
            .unwrap();
        let mut plugin = host.load_plugin_class(&library, CLASS_ID).unwrap();
        assert_eq!(plugin.info().uid, CLASS_ID);
        assert_eq!(plugin.info().name, "AGain Sample Accurate");
        plugin
            .load_state(&state)
            .expect("same original state for each fresh owner");
        let mut buffers = plugin.create_bus_audio_buffers(8).unwrap();
        assert_eq!((buffers.inputs.len(), buffers.outputs.len()), (1, 1));
        for bus in [&buffers.inputs[0], &buffers.outputs[0]] {
            assert!(bus.active);
            assert_eq!(bus.channels.len(), 2);
            assert!(bus.channels.iter().all(|channel| channel.len() == 8));
        }
        buffers.inputs[0].channels[0].fill(0.25);
        buffers.inputs[0].channels[1].fill(-0.5);
        assert_eq!(plugin.sample_rate(), 48_000.0);
        assert_eq!(plugin.block_size(), 64);
        plugin.start_processing().unwrap();
        let cohort = plugin.pending_restart_cohort().unwrap();
        assert!(!cohort.exhausted());
        assert!(matches!(cohort.flags().bits(), 0 | 4 | 20));
        let source = plugin.editor_event_source_status().unwrap();
        assert_eq!(source.failure(), None);
        assert_eq!(
            (source.open_parameter_edits(), source.group_depth()),
            (0, 0)
        );
        let mut frame = ControllerParameterQueryFrame::default();
        let mut initial_values = Vec::with_capacity(2);
        assert_eq!(
            plugin
                .visit_controller_parameters_bounded(2, &mut frame, &mut |index, actual| {
                    assert_eq!(actual.id(), index as u32);
                    assert_eq!(actual.normalized().to_bits(), 0.0_f64.to_bits());
                    initial_values.push(InitialParameterValue {
                        id: actual.id(),
                        normalized: actual.normalized(),
                    });
                    Ok(())
                })
                .unwrap(),
            2
        );
        plugin
            .handoff_initial_parameter_input(
                cohort,
                source,
                &initial_values,
                InitialParameterStorage::for_parameter_count(2).unwrap(),
            )
            .expect("real initial-only handoff; no manual clear or zero-sample flush");
        assert!(plugin.editor_event_source_status().unwrap().closed_empty());
        let after_cohort = plugin.pending_restart_cohort().unwrap();
        assert_eq!(after_cohort.serial(), cohort.serial());
        assert_eq!(after_cohort.flags().bits(), 0);
        assert!(!plugin.pending_processor_parameter_input().unwrap());
        let vst3_host::plugin::PluginEditorProcessingSplit {
            editor,
            mut processor,
        } = plugin.split_editor_processing().unwrap();
        let actual = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    processor
                        .set_process_timeline_context(ProcessTimelineContext {
                            sample_position: 0,
                            sample_rate: 48_000,
                        })
                        .unwrap();
                    for &(offset, value) in case.queue {
                        processor.queue_parameter_at(1, value, offset).unwrap();
                    }
                    // No control query, state operation, service or stop follows the queue.
                    processor
                        .process_bus_audio_with_note_events(&mut buffers, &[])
                        .unwrap();
                    (0..8)
                        .map(|index| {
                            [
                                buffers.outputs[0].channels[0][index],
                                buffers.outputs[0].channels[1][index],
                            ]
                        })
                        .collect::<Vec<_>>()
                })
                .join()
                .expect("joined direct SDK processing role")
        });
        drop(processor);
        drop(editor);
        drop(plugin);
        drop(host);
        // Save every case even on a primary mismatch; all SDK owners already dropped.
        write_pcm(&output.join(format!("{}-actual.f32le", case.name)), &actual);
        write_pcm(
            &output.join(format!("{}-expected.f32le", case.name)),
            &case.expected,
        );
        let mut mismatches = Vec::new();
        let mut forward_step_mismatches = Vec::new();
        for index in 0..8 {
            for channel in 0..2 {
                let bits = actual[index][channel].to_bits();
                if bits != case.expected[index][channel].to_bits() {
                    mismatches.push(serde_json::json!({"frame":index,"channel":channel,
                        "actual_bits":bits,"expected_bits":case.expected[index][channel].to_bits()}));
                }
                if bits != case.expected[(index + 1).min(7)][channel].to_bits() {
                    forward_step_mismatches.push(index * 2 + channel);
                }
            }
        }
        total_mismatches += mismatches.len();
        reports.push(serde_json::json!({"case":case.name,"sdk_process_api_calls":1,
            "queue":case.queue,"gain_native_id":1,"actual":actual,"expected":case.expected,
            "canonical_mismatches":mismatches,"forward_step_hypothesis_only":forward_step_mismatches,
            "secondary_changes_primary_oracle":false,"sdk_owner_drop_returned":true,
            "dll_unload_observed":false,"native_ffi_count_qualified":false}));
    }
    drop(native_main);
    drop((library_lease, state_lease));
    let record = serde_json::json!({"schema":1,"class_id":CLASS_ID,"frames_per_case":8,
        "sample_rate":48000,"channels":2,"mode":"Offline","max_block_frames":64,
        "fresh_sdk_owners":3,"original_state_bytes":56,"reports":reports,
        "canonical_mismatch_count":total_mismatches,"canonical_gain_pcm_pass":total_mismatches==0,
        "worker_ipc_qualified":false,"canonical_runtime_qualified":false,"cause_attribution":false});
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("observations.json"))
        .unwrap();
    serde_json::to_writer_pretty(file, &record).unwrap();
    assert_eq!(
        total_mismatches, 0,
        "canonical g[n] differs in direct SDK known queues; no host/plugin attribution"
    );
}
