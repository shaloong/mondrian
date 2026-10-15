//! Direct-owner startup observations; no Product/IPC/DSP qualification.

use super::*;
use serde_json::json;
use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use vst3_host::controller_parameters::ControllerParameterQueryFrame;

const MAX_JSON: u64 = 512 * 1024;
const MAX_REPORT: usize = 4 * 1024 * 1024;
const PHASES: [&str; 5] = [
    "after_load",
    "after_restore",
    "after_set_process_mode",
    "after_validate",
    "after_start_processing",
];

#[derive(Serialize)]
struct Boundary {
    process_id: u32,
    object_serial: Option<u64>,
    cohort: Option<Cohort>,
    source: Result<Source, String>,
}

#[derive(Serialize, PartialEq, Eq)]
struct Cohort {
    serial: u32,
    flags: u32,
    exhausted: bool,
}

#[derive(Serialize, PartialEq, Eq)]
struct Source {
    revision: u64,
    pending_events: usize,
    pending_dsp_feedback_values: usize,
    open_parameter_edits: usize,
    group_depth: u32,
    failure: Option<String>,
    closed_empty: bool,
}

impl Boundary {
    fn read(plugin: &Plugin) -> Self {
        Self {
            process_id: std::process::id(),
            object_serial: plugin.native_editor_attachment_owner().map(|o| o.serial().get()),
            cohort: plugin.pending_restart_cohort().map(|c| Cohort {
                serial: c.serial(),
                flags: c.flags().bits() as u32,
                exhausted: c.exhausted(),
            }),
            source: plugin
                .editor_event_source_status()
                .map(|s| Source {
                    revision: s.revision(),
                    pending_events: s.pending_events(),
                    pending_dsp_feedback_values: s.pending_dsp_feedback_values(),
                    open_parameter_edits: s.open_parameter_edits(),
                    group_depth: s.group_depth(),
                    failure: s.failure().map(|error| format!("{error:?}")),
                    closed_empty: s.closed_empty(),
                })
                .map_err(|error| error.to_string()),
        }
    }
}

/// Every ParameterInfo field is preserved explicitly, never SDK padding.
/// Float bits retain non-finite values and signed zero without JSON loss.
#[derive(Serialize, PartialEq, Eq)]
struct Parameter {
    index: usize,
    id: u32,
    title_utf16: Vec<u16>,
    short_title_utf16: Vec<u16>,
    units_utf16: Vec<u16>,
    unit_id: i32,
    step_count: i32,
    flags_bits: u32,
    default_normalized_bits: u64,
    normalized_bits: u64,
    normalized_in_range: bool,
    admitted_descriptor_matches: bool,
}

#[derive(Serialize)]
struct Phase {
    phase: &'static str,
    before: Boundary,
    after: Boundary,
    admitted_parameter_count: usize,
    observed_parameter_count: Option<usize>,
    parameters: Vec<Parameter>,
    query_error: Option<String>,
    // Complete means a stable bounded observation, not schema equality,
    // source quiescence, author acceptance or permission to retire flags.
    complete: bool,
}

impl Phase {
    fn read(phase: &'static str, plugin: &Plugin, admitted: &[Vst3ParameterDescriptor]) -> Self {
        let before = Boundary::read(plugin);
        let mut parameters = Vec::with_capacity(admitted.len().min(MAX_PARAMETERS));
        let query = if admitted.len() > MAX_PARAMETERS {
            Err(vst3_host::Error::InvalidParameter(
                "diagnostic parameter bound exceeded".into(),
            ))
        } else {
            plugin.visit_controller_parameters_bounded(
                admitted.len(),
                &mut ControllerParameterQueryFrame::default(),
                &mut |index, observed| {
                    let expected = admitted.get(index);
                    parameters.push(Parameter {
                        index,
                        id: observed.id(),
                        title_utf16: observed.title_utf16().to_vec(),
                        short_title_utf16: observed.short_title_utf16().to_vec(),
                        units_utf16: observed.units_utf16().to_vec(),
                        unit_id: observed.unit_id(),
                        step_count: observed.step_count(),
                        flags_bits: observed.flags(),
                        default_normalized_bits: observed.default_normalized().to_bits(),
                        normalized_bits: observed.normalized().to_bits(),
                        normalized_in_range: observed.normalized().is_finite()
                            && (0.0..=1.0).contains(&observed.normalized()),
                        admitted_descriptor_matches: expected.is_some_and(|expected| {
                            observed.id() == expected.id
                                && observed.title_matches(&expected.name)
                                && observed.default_normalized() == expected.default_normalized
                                && observed.step_count() == expected.step_count
                                && observed.flags() == expected.flags
                        }),
                    });
                    Ok(())
                },
            )
        };
        let after = Boundary::read(plugin);
        let complete = matches!(&query, Ok(count) if *count == admitted.len())
            && before.object_serial.is_some()
            && before.object_serial == after.object_serial
            && before.cohort.is_some()
            && before.cohort == after.cohort
            && before.source.is_ok()
            && before.source == after.source
            && parameters.iter().all(|p| {
                p.normalized_in_range && f64::from_bits(p.default_normalized_bits).is_finite()
            });
        Self {
            phase,
            before,
            after,
            admitted_parameter_count: admitted.len(),
            observed_parameter_count: query.as_ref().ok().copied(),
            parameters,
            query_error: query.err().map(|error| error.to_string()),
            complete,
        }
    }
}

struct Trace {
    directory: PathBuf,
    phases: Vec<Phase>,
    write_errors: Vec<String>,
}

thread_local! {
    // Test-thread-local instrumentation, never a product lifecycle authority.
    static TRACE: RefCell<Option<Trace>> = const { RefCell::new(None) };
}

struct Recording;

impl Recording {
    fn begin(directory: PathBuf) -> Self {
        TRACE.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "one direct diagnostic per owner thread"
            );
            *slot.borrow_mut() = Some(Trace {
                directory,
                phases: Vec::with_capacity(PHASES.len()),
                write_errors: Vec::new(),
            });
        });
        Self
    }

    fn finish(self) -> Trace {
        TRACE.with(|slot| slot.borrow_mut().take().expect("active diagnostic recorder"))
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        TRACE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

/// No observer means no native query, allocation, file write or flag service.
pub(super) fn observe(phase: &'static str, plugin: &Plugin, admitted: &[Vst3ParameterDescriptor]) {
    let destination = TRACE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|trace| (trace.directory.clone(), trace.phases.len()))
    });
    let Some((directory, index)) = destination else {
        return;
    };
    assert!(index < PHASES.len(), "bounded diagnostic phase count");
    let observation = Phase::read(phase, plugin, admitted);
    let written = write_json(
        &directory,
        &format!("phase-{index}-{phase}.json"),
        &observation,
    );
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let trace = slot.as_mut().expect("same owner recorder");
        if let Err(error) = written {
            trace.write_errors.push(error.to_string());
        }
        trace.phases.push(observation);
    });
}

fn no_reparse(path: &Path) {
    assert!(path.is_absolute(), "absolute diagnostic input/output");
    for ancestor in path.ancestors() {
        if !ancestor.as_os_str().is_empty() {
            let metadata = fs::symlink_metadata(ancestor).expect("existing diagnostic ancestor");
            assert_eq!(
                metadata.file_attributes() & 0x400,
                0,
                "reparse diagnostic ancestor"
            );
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    path: PathBuf,
    bytes: u64,
    sha256: String,
}

struct HeldInput {
    file: File,
    binding: Binding,
}

impl HeldInput {
    fn open(binding: Binding, maximum: u64) -> Self {
        no_reparse(&binding.path);
        assert!(
            binding.bytes > 0 && binding.bytes <= maximum,
            "diagnostic input bound"
        );
        assert!(
            binding.sha256.len() == 64 && binding.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        );
        let file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&binding.path)
            .expect("read-only diagnostic input lease");
        assert!(file.metadata().unwrap().is_file());
        let mut held = Self { file, binding };
        held.verify();
        held
    }

    fn verify(&mut self) {
        assert_eq!(self.file.metadata().unwrap().len(), self.binding.bytes);
        self.file.seek(SeekFrom::Start(0)).unwrap();
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = self.file.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        assert_eq!(
            format!("{:x}", hash.finalize()),
            self.binding.sha256.to_ascii_lowercase()
        );
        self.file.seek(SeekFrom::Start(0)).unwrap();
    }

    fn read(&mut self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.binding.bytes as usize);
        self.file.seek(SeekFrom::Start(0)).unwrap();
        self.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, self.binding.bytes);
        bytes
    }
}

// Read-only projection of the already hash-bound existing VendorCase. Its
// unrelated helper, PCM and case-evidence fields are deliberately not used.
#[derive(Deserialize)]
struct DiagnosticInputs {
    schema: u32,
    format: String,
    plugin: Binding,
    opaque_state: Binding,
    captured_probe_response: Binding,
    expected_identity: String,
    expected_parameter_count: usize,
}

fn write_json(directory: &Path, name: &str, value: &impl Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > MAX_REPORT {
        return Err(std::io::Error::other("diagnostic report bound exceeded"));
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(directory.join(name))?;
    file.write_all(&bytes)?;
    file.sync_all()
}

#[test]
#[ignore = "hash-bound external VST3 case, empty diagnostic output and owned native Job required"]
fn native_startup_metadata_phase_observation() {
    let case_path = PathBuf::from(std::env::var_os("MONDRIAN_EXTERNAL_VENDOR_CASE").expect("case"));
    no_reparse(&case_path);
    let mut case = HeldInput::open(
        Binding {
            bytes: fs::metadata(&case_path).unwrap().len(),
            path: case_path,
            sha256: std::env::var("MONDRIAN_EXTERNAL_VENDOR_CASE_SHA256").expect("case SHA256"),
        },
        MAX_JSON,
    );
    let inputs: DiagnosticInputs =
        serde_json::from_slice(&case.read()).expect("bound VendorCase projection");
    assert_eq!(inputs.schema, 1);
    assert_eq!(inputs.format, "vst3");
    assert!(inputs.expected_parameter_count <= MAX_PARAMETERS);
    let mut plugin = HeldInput::open(inputs.plugin, MAX_BINARY_BYTES);
    let mut state = HeldInput::open(inputs.opaque_state, MAX_STATE_BYTES as u64);
    let mut probe = HeldInput::open(inputs.captured_probe_response, MAX_JSON);
    let state_bytes = state.read();
    let super::super::vst3_discovery::DiscoveryResponse::Probe {
        class_id,
        vendor,
        latency_frames,
        tail_frames,
        parameters,
        captured_state,
        auxiliary_buses,
        ..
    } = serde_json::from_slice(&probe.read()).expect("exact captured probe")
    else {
        panic!("captured Probe required")
    };
    assert_eq!(class_id, inputs.expected_identity);
    assert_eq!(parameters.len(), inputs.expected_parameter_count);
    assert_eq!(captured_state.as_deref(), Some(state_bytes.as_slice()));
    let lanes = parameters
        .iter()
        .filter(|p| !p.read_only)
        .map(|p| (p.parameter_id().expect("native lane identity"), p.id))
        .collect::<BTreeMap<_, _>>();
    let tail = match tail_frames {
        0 => AudioProcessorTail::None,
        u32::MAX => AudioProcessorTail::Infinite,
        frames => AudioProcessorTail::Finite(frames as usize),
    };
    let contract =
        AudioProcessorExecutionContract::new(latency_frames as usize, tail, true, false, true, 0)
            .expect("captured offline contract");
    let render = crate::AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: usize::MAX,
        public_output_lookahead_budget_frames: usize::MAX,
        compensation_delay_scratch_budget_bytes: usize::MAX,
    };
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
        native_lane: native_worker_lane_profile().expect("known native lane layout"),
        payload: serde_json::to_vec(&Vst3Payload {
            class_id,
            vendor,
            binary_sha256: fingerprint(&plugin.binding.path).unwrap(),
            binary_path: plugin.binding.path.clone(),
            state: Some(state_bytes.clone()),
            parameters,
            parameter_ids: lanes.values().copied().collect(),
        })
        .unwrap(),
        plugin_execution_contract: contract,
        auxiliary_inputs: super::super::protocol::auxiliary_from_wire(auxiliary_buses),
        note_input_contract: crate::AudioProcessorNoteInputContract::default(),
        parameter_ids: lanes.keys().cloned().collect(),
        render_contract: render,
    };
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_STARTUP_METADATA_OUTPUT").expect("diagnostic output"),
    );
    no_reparse(&directory);
    assert!(directory.is_dir());
    assert_eq!(
        fs::read_dir(&directory).unwrap().count(),
        0,
        "fresh diagnostic output"
    );
    let recording = Recording::begin(directory.clone());
    let prepared = Vst3AudioProcessorWorkerFactory.prepare_native(request);
    let trace = recording.finish();
    let final_object = prepared.as_ref().ok().and_then(|worker| {
        worker
            .plugin
            .as_ref()?
            .native_editor_attachment_owner()
            .map(|owner| owner.serial().get())
    });
    let prepare_error = prepared.as_ref().err().map(ToString::to_string);
    write_json(
        &directory,
        "report.json",
        &json!({
            "kind":"DIRECT_WORKER_NONCONSUMING_STARTUP_METADATA_DIAGNOSTIC",
            "product_front_door":false,"enter_invoked":false,"process_invoked":false,
            "diagnostic_restart_service_invoked":false,"diagnostic_controller_setter_invoked":false,
            "admission_authorized":false,
            "pid":std::process::id(),"final_object_serial":final_object,
            "prepare_error":prepare_error,"write_errors":trace.write_errors,
            "phases":trace.phases,
        }),
    )
    .expect("create-only diagnostic report");
    case.verify();
    plugin.verify();
    state.verify();
    probe.verify();
    assert!(
        prepared.is_ok(),
        "original factory refusal: {prepare_error:?}"
    );
    assert!(trace.write_errors.is_empty());
    assert_eq!(
        trace.phases.iter().map(|p| p.phase).collect::<Vec<_>>(),
        PHASES
    );
    assert!(
        trace.phases.iter().all(|p| p.complete),
        "incomplete or reentrant query; read phase evidence"
    );
    assert!(trace.phases.iter().all(|p| p.before.object_serial == final_object));
    let worker = prepared.unwrap();
    assert!(worker.initial_signal_available);
    assert_eq!(worker.last_end, None);
    assert!(worker.last_parameter_values.iter().all(Option::is_none));
    assert_eq!(
        worker.payload.state.as_deref(),
        Some(state_bytes.as_slice())
    );
    // Drop the same actual native owner; no Enter, reset, Process or service.
    drop(worker);
}

#[test]
#[ignore = "owned initial-values SDK fixture and native Job required"]
fn native_startup_metadata_nonconsuming_sdk_controls() {
    for mode in [1, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
        // Existing fixture preparation stays uninstrumented. Its audit call
        // arms the real controller's controlled read behavior, not a fake
        // Phase or a fabricated restart callback.
        let worker = super::initial_parameter_values_tests::prepare(0.375, mode);
        let before = super::initial_parameter_values_tests::audit(&worker);
        let plugin = worker.plugin.as_ref().unwrap();
        let cohort = plugin.pending_restart_cohort();
        let phase = Phase::read("sdk_control", plugin, &worker.payload.parameters);
        let after = super::initial_parameter_values_tests::audit(&worker);
        assert_eq!(after.process, before.process);
        assert_eq!(after.setters, before.setters);
        assert_eq!(after.wrong_thread, 0);
        assert_eq!(worker.last_end, None);
        assert!(worker.initial_signal_available);
        assert!(worker.last_parameter_values.iter().all(Option::is_none));
        assert_eq!(phase.before.object_serial, phase.after.object_serial);
        assert_eq!(phase.after.cohort.as_ref().unwrap().flags, 4);
        assert_eq!(plugin.pending_restart_cohort().unwrap().flags().bits(), 4);
        if mode == 6 {
            assert!(!phase.complete);
            assert!(phase.after.cohort.as_ref().unwrap().serial > cohort.unwrap().serial());
            assert_eq!(
                plugin.pending_restart_cohort().unwrap().serial(),
                phase.after.cohort.as_ref().unwrap().serial
            );
        } else {
            assert_eq!(plugin.pending_restart_cohort(), cohort);
        }
        match mode {
            1 | 7 => {
                assert!(phase.complete);
                let observed = &phase.parameters[0];
                let mut gain = vec![0_u16; 128];
                gain[..4].copy_from_slice(&['G' as u16, 'a' as u16, 'i' as u16, 'n' as u16]);
                assert_eq!(observed.title_utf16, gain);
                assert_eq!(observed.short_title_utf16, gain);
                assert_eq!(observed.units_utf16, vec![0; 128]);
                assert_eq!(observed.unit_id, 0);
                assert_eq!(observed.step_count, 0);
                assert_eq!(observed.flags_bits, 1);
                assert_eq!(observed.default_normalized_bits, 0.5_f64.to_bits());
                assert_eq!(observed.id, if mode == 7 { 99 } else { 0 });
                assert_eq!(observed.admitted_descriptor_matches, mode == 1);
                assert_eq!(
                    observed.normalized_bits,
                    if mode == 7 { 0.0_f64 } else { 0.375_f64 }.to_bits()
                );
                assert_eq!(after.metadata, before.metadata + 1);
                assert_eq!(after.values, before.values + 1);
            }
            4 | 5 => {
                assert!(!phase.complete);
                assert!(phase.query_error.is_none());
                assert!(!phase.parameters[0].normalized_in_range);
                let value = f64::from_bits(phase.parameters[0].normalized_bits);
                if mode == 4 {
                    assert!(value.is_nan());
                } else {
                    assert_eq!(value, 1.25);
                }
            }
            6 => assert!(phase.query_error.is_none()),
            8 | 10 | 11 => {
                assert!(!phase.complete);
                assert!(phase.query_error.is_some());
                assert!(phase.parameters.is_empty());
                assert_eq!(after.values, before.values);
            }
            9 => {
                assert!(!phase.complete);
                assert!(phase.query_error.is_none());
                let source = phase.after.source.as_ref().unwrap();
                assert!(source.pending_events > 0);
                assert!(source.pending_dsp_feedback_values > 0);
                assert_eq!(
                    plugin.editor_event_source_status().unwrap().pending_events(),
                    source.pending_events
                );
                assert_eq!(
                    plugin.editor_event_source_status().unwrap().pending_dsp_feedback_values(),
                    source.pending_dsp_feedback_values
                );
            }
            12 => {
                assert!(!phase.complete);
                assert_eq!(phase.observed_parameter_count, Some(0));
                assert!(phase.query_error.is_none());
                assert!(phase.parameters.is_empty());
                assert_eq!(after.values, before.values);
            }
            _ => unreachable!(),
        }
    }
}
