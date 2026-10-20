//! External data qualification through canonical author/resolver/isolated worker.
//! No vendor identity, opaque-state fabrication, timeout override or product API.
//! Root's frozen outer Job is required to bound native cleanup and retain logs.

use super::*;
use crate as audio;
#[path = "../../../tests/support/external_vendor_support.rs"]
mod external_vendor_support;
use external_vendor_support::*;
use mondrian_core::ExactAutomationCurve;
use mondrian_timeline::audio::AudioProcessorInstance;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

use crate::AudioProgramRuntime;
use mondrian_core::{AssetId, AudioSourceComponentId, ExecutionCancellationToken};
use std::sync::atomic::{AtomicUsize, Ordering};

const CASE_ENV: &str = "MONDRIAN_EXTERNAL_VENDOR_CASE";
const CASE_HASH_ENV: &str = "MONDRIAN_EXTERNAL_VENDOR_CASE_SHA256";

// Shared finite Runtime media and read-only installed-owner observations.
#[derive(Default)]
struct Observations {
    polls: usize,
    parameter_batches: usize,
    diagnostic_records: usize,
    native_object: Option<(u32, u64)>,
    first_parameters: Option<String>,
    last_parameters: Option<String>,
}
fn observe(
    runtime: &mut AudioProgramRuntime,
    lease: crate::AudioNativeProcessorOwnerLease,
    evidence: &mut Observations,
) {
    evidence.polls += 1;
    let batch = runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("actual typed native control poll; no restart/error is cleared");
    let Some(batch) = batch else {
        return;
    };
    // Metadata and RO meter facts use the existing observation projector. A
    // real author transaction needs its canonical handoff, never a fake Ack.
    assert!(
        batch.author.is_none(),
        "unexpected real author work retained: {:?}",
        batch.author
    );
    if let Some(logs) = batch.diagnostics {
        assert!(!logs.loss().incomplete(), "native diagnostic loss");
        evidence.diagnostic_records += logs.records().len();
    }
    if let Some(parameters) = batch.parameters {
        assert_eq!(parameters.target(), lease.editor_target());
        let object = (
            parameters.process_id().get(),
            parameters.object_serial().get(),
        );
        assert_ne!(object.0, std::process::id());
        if let Some(expected) = evidence.native_object {
            assert_eq!(object, expected);
        }
        evidence.native_object = Some(object);
        evidence.parameter_batches += 1;
        let snapshot = format!("{:?}", parameters.facts());
        if evidence.first_parameters.is_none() {
            evidence.first_parameters = Some(snapshot.clone());
        }
        evidence.last_parameters = Some(snapshot);
    }
}

// Metadata-only qualification has its own case contract and no PCM oracle.
mod readonly;

mod again_runtime_pcm;
mod vst3_runtime_pcm;

// Finite auxiliary-input PCM observations; no vendor algorithm oracle.
mod surge_fx;

// Default capture and fresh CLAP metadata restoration; no PCM processing.
mod surge_fx_clap_probe;

mod mda_dx10_runtime;
mod surge_instrument_runtime;

// Steady native bypass through the public Runtime/Graph; finite real-plugin case.
mod ring_out_runtime_pcm;

#[test]
#[ignore = "external hash-bound vendor case, current helper and frozen outer native Job required"]
fn external_clap_opaque_state_native_bypass_pcm() {
    run_case(Format::Clap);
}

#[test]
#[ignore = "external hash-bound vendor case, current helper and frozen outer native Job required"]
fn external_vst3_opaque_state_native_bypass_pcm() {
    run_case(Format::Vst3);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Format {
    Clap,
    Vst3,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VendorCase {
    schema: u32,
    format: Format,
    helper: Binding,
    plugin: Binding,
    opaque_state: Binding,
    captured_list_response: Binding,
    captured_probe_response: Binding,
    pcm: Binding,
    expected_identity: String,
    expected_parameter_count: usize,
    bypass_native_id: u32,
    evidence_directory: PathBuf,
}

enum Catalog {
    Clap(DiscoveredClapAudioProcessorSpecResolver),
    Vst3(DiscoveredVst3AudioProcessorSpecResolver),
}
impl Catalog {
    fn instance(
        &self,
        id: &str,
        render: AudioRenderContract,
        state: &[u8],
    ) -> AudioProcessorInstance {
        match self {
            Self::Clap(c) => c.create_instance(id, render, Some(state.to_vec())),
            Self::Vst3(c) => c.create_instance(id, render, Some(state.to_vec())),
        }
        .expect("actual same-format opaque native restore")
    }
    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> IsolatedAudioProcessorWorkerSpec {
        match self {
            Self::Clap(c) => c.resolve(request),
            Self::Vst3(c) => c.resolve(request),
        }
        .expect("canonical author/resolver native spec")
    }
}

fn run_case(format: Format) {
    let case_path = PathBuf::from(std::env::var_os(CASE_ENV).expect("external case path"));
    let case_meta = fs::metadata(&case_path).expect("case metadata");
    let mut case_lease = Lease::open(
        &Binding {
            path: case_path,
            bytes: case_meta.len(),
            sha256: std::env::var(CASE_HASH_ENV).expect("case SHA256"),
        },
        MAX_JSON,
    );
    let case_bytes = case_lease.read();
    let case: VendorCase =
        serde_json::from_value(strict_json(&case_bytes)).expect("typed strict external case");
    assert_eq!(case.schema, 1);
    assert_eq!(case.format, format, "exact selected format");
    assert!(!case.expected_identity.is_empty() && case.expected_identity.len() <= 256);
    assert!((1..=1024).contains(&case.expected_parameter_count));
    no_reparse(&case.evidence_directory);
    assert!(fs::metadata(&case.evidence_directory).expect("evidence directory").is_dir());
    assert_eq!(
        fs::read_dir(&case.evidence_directory)
            .expect("fresh evidence directory")
            .count(),
        0
    );
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let mut state_lease = Lease::open(
        &case.opaque_state,
        if format == Format::Clap {
            64 * 1024
        } else {
            128 * 1024
        },
    );
    let mut list_lease = Lease::open(&case.captured_list_response, MAX_JSON);
    let mut probe_lease = Lease::open(&case.captured_probe_response, MAX_JSON);
    let mut pcm_lease = Lease::open(&case.pcm, (FRAMES * 2 * 4) as u64);
    let state = state_lease.read();
    let list = strict_json(&list_lease.read());
    let captured = strict_json(&probe_lease.read());
    let input_bytes = pcm_lease.read();
    let input = deterministic_pcm();
    assert_eq!(
        input_bytes,
        pcm_bytes(&input),
        "deterministic asymmetric stereo fixture"
    );
    write_new(&case.evidence_directory, "case.json", &case_bytes);
    write_new(
        &case.evidence_directory,
        "restored-opaque-state.bin",
        &state,
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };

    let (catalog, facts) = match format {
        Format::Clap => {
            exact_keys(&list, &["descriptors", "probe"]);
            assert!(list["probe"].is_null());
            let c = DiscoveredClapAudioProcessorSpecResolver::discover(
                helper.path.clone(),
                [plugin.path.clone()],
            )
            .expect("actual CLAP list");
            assert_eq!(
                c.descriptors().len(),
                1,
                "first slice requires sole observed identity"
            );
            assert_eq!(c.descriptors()[0].plugin_id, case.expected_identity);
            assert_eq!(json!([c.descriptors()[0]]), list["descriptors"]);
            exact_keys(&captured, &["descriptors", "probe"]);
            assert_eq!(captured["descriptors"], json!([]));
            let p = &captured["probe"];
            exact_keys(
                p,
                &[
                    "plugin_id",
                    "latency_frames",
                    "offline",
                    "tail",
                    "parameters",
                    "auxiliary_buses",
                    "captured_state",
                ],
            );
            assert_eq!(p["plugin_id"], case.expected_identity);
            assert_eq!(p["offline"], true);
            assert_eq!(
                p["latency_frames"], 0,
                "bypass bit-exact first slice requires zero declared latency"
            );
            assert_eq!(p["auxiliary_buses"], json!([]));
            assert_eq!(p["captured_state"], json!(state));
            for d in p["parameters"].as_array().expect("captured CLAP parameters") {
                exact_keys(
                    d,
                    &[
                        "id",
                        "name",
                        "min_value",
                        "max_value",
                        "default_value",
                        "current_value",
                        "automatable",
                        "stepped",
                        "enumeration",
                        "read_only",
                        "hidden",
                    ],
                );
            }
            let expected: Vec<ClapParameterDescriptor> =
                serde_json::from_value(p["parameters"].clone())
                    .expect("unsigned captured CLAP metadata");
            let actual = probe_clap_plugin_registration(
                &helper.path,
                &plugin.path,
                &case.expected_identity,
                render,
                Some(&state),
            )
            .expect("re-probe actual restored CLAP metadata");
            assert_eq!(actual.parameters, expected);
            assert_eq!(
                actual.binary_sha256.as_slice(),
                decode_hash(&plugin.sha256).as_slice()
            );
            assert_eq!(actual.execution_contract.algorithmic_latency_frames(), 0);
            let actual_tail = match actual.execution_contract.tail() {
                AudioProcessorTail::None => json!("None"),
                AudioProcessorTail::Finite(n) => json!({"Finite": n}),
                AudioProcessorTail::Infinite => json!("Infinite"),
            };
            assert_eq!(actual_tail, p["tail"]);
            assert!(actual.auxiliary_inputs.buses.is_empty());
            (Catalog::Clap(c), Facts::Clap(expected))
        }
        Format::Vst3 => {
            exact_keys(&list, &["kind", "descriptors"]);
            assert_eq!(list["kind"], "list");
            let c = DiscoveredVst3AudioProcessorSpecResolver::discover(
                helper.path.clone(),
                [plugin.path.clone()],
            )
            .expect("actual VST3 list");
            assert_eq!(c.descriptors().len(), 1);
            assert_eq!(c.descriptors()[0].class_id, case.expected_identity);
            assert_eq!(json!([c.descriptors()[0]]), list["descriptors"]);
            exact_keys(
                &captured,
                &[
                    "kind",
                    "class_id",
                    "vendor",
                    "latency_frames",
                    "tail_frames",
                    "parameters",
                    "current_values",
                    "captured_state",
                    "auxiliary_buses",
                ],
            );
            assert_eq!(captured["kind"], "probe");
            assert_eq!(captured["class_id"], case.expected_identity);
            assert_eq!(captured["latency_frames"], 0);
            assert_eq!(captured["auxiliary_buses"], json!([]));
            assert_eq!(captured["captured_state"], json!(state));
            for d in captured["parameters"].as_array().expect("captured VST3 parameters") {
                exact_keys(
                    d,
                    &[
                        "id",
                        "name",
                        "default_normalized",
                        "step_count",
                        "can_automate",
                        "read_only",
                        "flags",
                    ],
                );
            }
            let expected: Vec<Vst3ParameterDescriptor> =
                serde_json::from_value(captured["parameters"].clone())
                    .expect("unsigned captured VST3 metadata");
            let current: Vec<(u32, f64)> =
                serde_json::from_value(captured["current_values"].clone())
                    .expect("unsigned captured current values");
            let actual = super::super::vst3_discovery::probe_vst3_plugin_registration_with_state(
                &helper.path,
                &plugin.path,
                &case.expected_identity,
                render,
                Some(&state),
                false,
            )
            .expect("re-probe actual restored VST3 values");
            assert_eq!(actual.registration.parameters, expected);
            assert_eq!(actual.current_values, current);
            assert_eq!(json!(actual.registration.vendor), captured["vendor"]);
            assert_eq!(
                actual.registration.binary_sha256.as_slice(),
                decode_hash(&plugin.sha256).as_slice()
            );
            assert_eq!(
                actual.registration.execution_contract.algorithmic_latency_frames(),
                0
            );
            let expected_tail = match captured["tail_frames"]
                .as_u64()
                .expect("captured tail frames")
            {
                0 => AudioProcessorTail::None,
                n if n == u32::MAX as u64 => AudioProcessorTail::Infinite,
                n => AudioProcessorTail::Finite(usize::try_from(n).expect("finite native tail")),
            };
            assert_eq!(actual.registration.execution_contract.tail(), expected_tail);
            assert!(actual.registration.auxiliary_inputs.buses.is_empty());
            (Catalog::Vst3(c), Facts::Vst3(expected, current))
        }
    };
    let instance = catalog.instance(&case.expected_identity, render, &state);
    assert!(
        !instance.bypassed,
        "author bypass must not short circuit native processing"
    );
    assert_eq!(
        instance.opaque_state.as_ref().map(AsRef::as_ref),
        Some(state.as_slice())
    );
    assert_eq!(instance.parameters.len(), case.expected_parameter_count);
    facts.check_author(&instance);
    let prefix = if format == Format::Clap {
        "clap"
    } else {
        "vst3"
    };
    let bypass_id = ParameterId::new(format!("{prefix}.param.{}", case.bypass_native_id))
        .expect("exact unsigned native bypass ID");
    let bypass = instance.parameters.get(&bypass_id).expect("observed native bypass parameter");
    assert_eq!(
        bypass.ui.display_name.as_deref(),
        Some("Bypass"),
        "first slice requires the captured native Bypass fact"
    );
    assert!(bypass.schema.is_animatable);
    let initial_author = serde_json::to_vec(&instance).expect("canonical author serialize");
    let restored_author: AudioProcessorInstance =
        serde_json::from_slice(&initial_author).expect("canonical author deserialize");
    assert_eq!(restored_author, instance);
    write_new(
        &case.evidence_directory,
        "initial-author.json",
        &initial_author,
    );
    // A second actual native reload verifies author persistence against native
    // values; raw byte serialization alone is not opaque-state restoration.
    let fresh = catalog.instance(
        &case.expected_identity,
        render,
        restored_author.opaque_state.as_ref().expect("persisted opaque bytes").as_ref(),
    );
    facts.check_author(&fresh);
    assert_eq!(fresh.definition, restored_author.definition);
    assert_eq!(fresh.parameters, restored_author.parameters);
    assert_eq!(fresh.opaque_state, restored_author.opaque_state);
    for frames in [1, 17, 64] {
        process_case(
            &catalog,
            &restored_author,
            &bypass_id,
            render,
            &input,
            frames,
            true,
            &case.evidence_directory,
        );
    }
    process_case(
        &catalog,
        &restored_author,
        &bypass_id,
        render,
        &input,
        64,
        false,
        &case.evidence_directory,
    );
    write_new(&case.evidence_directory, "result.json", &serde_json::to_vec_pretty(&json!({
        "schema":1,"format":prefix,"actual_identity":case.expected_identity,
        "case_sha256":case_lease.sha256,"helper_sha256":helper.sha256,"plugin_sha256":plugin.sha256,
        "opaque_state_sha256":state_lease.sha256,"parameter_count":case.expected_parameter_count,
        "unsigned_bypass_id":case.bypass_native_id,"author_bypassed":false,
        "stereo_sample_rate":48000,"offline":true,"maximum_frames":64,"frames":FRAMES,"partitions":[1,17,64],
        "canonical_author_roundtrip":true,"actual_default_opaque_restore":true,"nondefault_state_discrimination":false,"native_bypass_pcm_bit_exact":true,
        "non_bypass_discrimination":true,"processed_instance_exit_observed":true,
        "processed_instance_graceful_ack_qualified":false,"outer_job_qualified_by_test":false,
        "graph_preview_export_qualified":false,"realtime_qualified":false,"performance_qualified":false,"full_compatibility_qualified":false
    })).expect("result JSON"));
    // Keep leases alive through all native admissions, processing and teardown.
    drop((
        case_lease,
        helper,
        plugin,
        state_lease,
        list_lease,
        probe_lease,
        pcm_lease,
    ));
}

fn process_case(
    catalog: &Catalog,
    source: &AudioProcessorInstance,
    bypass_id: &ParameterId,
    render: AudioRenderContract,
    input: &[f32],
    partition: usize,
    bypass: bool,
    directory: &Path,
) {
    let label = format!(
        "{}-{partition}",
        if bypass { "bypass" } else { "non-bypass" }
    );
    let mut author = source.clone();
    author
        .set_parameter_automation(
            ExactAutomationCurve::new(bypass_id.clone(), if bypass { 1.0 } else { 0.0 })
                .expect("native bypass constant"),
        )
        .expect("canonical parameter transaction");
    assert!(!author.bypassed);
    let bytes = serde_json::to_vec(&author).expect("persist author bypass change");
    let author: AudioProcessorInstance =
        serde_json::from_slice(&bytes).expect("reload canonical author with native bypass");
    write_new(directory, &format!("{label}-author.json"), &bytes);
    let request = AudioProcessorPrepareRequest::new(
        AudioProcessorOccurrence {
            instance_id: author.id,
            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
            insertion: AudioProcessorInsertionPoint::PreFader,
        },
        &author.definition,
        &author.parameters,
        author.opaque_state.as_ref().map(AsRef::as_ref),
        render,
    );
    let spec = catalog.resolve(request);
    assert!(spec.auxiliary_inputs.buses.is_empty());
    // Never override native operation budgets or ignore the factory's private
    // validation.shutdown() false gate. Outer Job cleanup cannot pass this gate.
    let factory = IsolatedAudioProcessorFactory::prepare(spec)
        .expect("native validation including shutdown must succeed");
    let mut processor = factory.create(None).expect("fresh actual native processing child");
    let before = processor
        .native_audio_object_observation()
        .expect("authenticated native object")
        .expect("native capability");
    let pid = before.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: nonzero PID came from the authenticated retained supervisor child.
    // Open only query/synchronize rights; OwnedHandle closes this independent
    // observation handle once. Never terminate or mutate the native process.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(
        !raw.is_null(),
        "retain exact live process observation handle"
    );
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        258,
        "child alive before entry"
    );
    processor.enter_state(73).expect("actual native state entry");
    let entered = processor
        .native_audio_object_observation()
        .expect("authenticated entered object")
        .expect("native object");
    assert_eq!(entered.process_id(), before.process_id());
    assert!(entered.object_serial() >= before.object_serial());
    let ids: Vec<_> = author.parameters.keys().cloned().collect();
    let ranges: Vec<_> = (0..ids.len()).map(|i| i..i + 1).collect();
    let events: Vec<_> = ids
        .iter()
        .map(|id| AudioParameterEvent {
            sample_offset: 0,
            value: author
                .parameters
                .get(id)
                .expect("canonical constant")
                .automation
                .as_ref()
                .expect("author curve")
                .default_value,
        })
        .collect();
    let mut output = Vec::with_capacity(input.len());
    let mut offset = 0;
    while offset < FRAMES {
        let frames = partition.min(FRAMES - offset);
        let start = 73 + offset as i64;
        let mut io = TestAudioIo {
            main: input[offset * 2..(offset + frames) * 2].to_vec(),
            auxiliary: Vec::new(),
        };
        let context = AudioProcessorProcessContext::new(
            AudioRenderRequest { start_sample: start, frames },
            AudioSampleRate::new(48000).expect("sample grid"),
            AudioChannelLayout::Stereo,
            AudioProcessingMode::Offline,
            AudioKernelBackend::ScalarReference,
        );
        processor
            .process(
                context,
                &mut io,
                AudioParameterEventBatch::new(start, frames, &ids, &ranges, &events),
            )
            .expect("native constant parameter + PCM window");
        assert_eq!(io.main.len(), frames * 2);
        output.extend(io.main);
        offset += frames;
    }
    assert_eq!(
        processor
            .native_audio_object_observation()
            .expect("same actual process after PCM"),
        Some(entered)
    );
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        258,
        "actual processing child alive before Drop"
    );
    write_new(
        directory,
        &format!("{label}-output.f32le"),
        &pcm_bytes(&output),
    );
    drop(processor);
    // No public processor shutdown exists. Zero-wait only observes the handle
    // after existing Drop; it is not a new lifecycle timeout or graceful ACK.
    let signalled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit_code = 0_u32;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
    let differences = input.iter().zip(&output).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
    let finite = output.iter().all(|v| v.is_finite());
    write_new(directory, &format!("{label}-receipt.json"), &serde_json::to_vec_pretty(&json!({
        "native_pid":pid,"object_serial_before":before.object_serial().get(),"object_serial_entered":entered.object_serial().get(),
        "partition":partition,"frames":FRAMES,"author_bypassed":false,"native_bypass":bypass,
        "input_sha256":format!("{:x}",Sha256::digest(pcm_bytes(input))),"output_sha256":format!("{:x}",Sha256::digest(pcm_bytes(&output))),
        "finite":finite,"differing_samples":differences,"post_drop_wait_status":signalled,
        "exit_query_succeeded":queried != 0,"exit_code":exit_code,"graceful_ack_qualified":false
    })).expect("actual PCM/lifecycle receipt"));
    assert_eq!(
        signalled, 0,
        "owned native process must have exited after existing Drop"
    );
    assert_ne!(queried, 0, "query retained native exit status");
    assert_eq!(
        exit_code, 0,
        "forced/error native exit cannot qualify repeated lifecycle"
    );
    assert!(finite, "native output must remain finite");
    assert_eq!(output.len(), input.len());
    if bypass {
        assert_eq!(
            differences, 0,
            "plugin-owned stereo bypass must be bit exact"
        );
    } else {
        assert!(
            differences > 0,
            "non-bypass native discrimination cannot silently count passthrough"
        );
    }
}
