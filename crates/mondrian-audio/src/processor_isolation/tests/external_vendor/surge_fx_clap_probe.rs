//! Default opaque-state capture and fresh isolated CLAP metadata restoration.
//! Current values are observations; this gate does not process PCM.

use super::*;
use crate::processor_isolation::clap_discovery::probe_clap_plugin_registration_with_state;

const PROBE_CASE_ENV: &str = "MONDRIAN_EXTERNAL_CLAP_PROBE_CASE";
const PROBE_CASE_HASH_ENV: &str = "MONDRIAN_EXTERNAL_CLAP_PROBE_CASE_SHA256";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeCase {
    schema: u32,
    helper: Binding,
    plugin: Binding,
    expected_identity: String,
    evidence_directory: PathBuf,
}

fn write_json(directory: &Path, name: &str, value: Value) {
    let bytes = serde_json::to_vec_pretty(&value).expect("complete observation JSON");
    assert!(bytes.len() as u64 <= MAX_JSON, "bounded observation JSON");
    write_new(directory, name, &bytes);
}

fn probe_failed(directory: &Path, stage: &str, error: AudioProcessorHostError) -> ! {
    write_json(
        directory,
        "failure.json",
        json!({
            "schema": 1, "stage": stage,
            "error_debug": format!("{error:?}"), "error_display": error.to_string(),
            "processed_pcm": false,
        }),
    );
    panic!("{stage} retained its actual typed probe failure: {error:?}");
}

// Compare only definition declarations. Keep exact float bits, do not turn
// changing current-value readings or native enumeration order into an oracle.
fn declarations(parameters: &[ClapParameterDescriptor]) -> Value {
    let by_id: std::collections::BTreeMap<_, _> = parameters
        .iter()
        .map(|p| {
            (
                p.id.to_string(),
                json!({
                    "id": p.id, "name": p.name,
                    "min_value_bits": p.min_value.to_bits(),
                    "max_value_bits": p.max_value.to_bits(),
                    "default_value_bits": p.default_value.to_bits(),
                    "automatable": p.automatable, "stepped": p.stepped,
                    "enumeration": p.enumeration, "read_only": p.read_only,
                    "hidden": p.hidden,
                }),
            )
        })
        .collect();
    assert_eq!(by_id.len(), parameters.len(), "unique validated native IDs");
    json!(by_id)
}

fn metadata(registration: &ClapPluginRegistration) -> Value {
    let contract = registration.execution_contract;
    let tail = match contract.tail() {
        AudioProcessorTail::None => json!("None"),
        AudioProcessorTail::Finite(frames) => json!({"Finite": frames}),
        AudioProcessorTail::Infinite => json!("Infinite"),
    };
    json!({
        "plugin_id": registration.plugin_id,
        "library_path": registration.library_path,
        "binary_sha256": registration.binary_sha256,
        "execution_contract": {
            "algorithmic_latency_frames": contract.algorithmic_latency_frames(),
            "tail": tail, "requires_state_entry": contract.requires_state_entry(),
            "realtime_capable": contract.realtime_capable(),
            "offline_capable": contract.offline_capable(),
            "session_scratch_bytes": contract.session_scratch_bytes(),
        },
        "parameters": registration.parameters,
        "auxiliary_inputs": registration.auxiliary_inputs.buses.iter().map(|bus| json!({
            "bus_key": bus.bus_key, "channel_layout": bus.channel_layout,
        })).collect::<Vec<_>>(),
    })
}

#[test]
#[ignore = "hash-bound external CLAP probe case, fresh Product and frozen outer native Job required"]
fn external_clap_default_capture_fresh_restore_metadata() {
    let path = PathBuf::from(std::env::var_os(PROBE_CASE_ENV).expect("probe case path"));
    let mut case_lease = Lease::open(
        &Binding {
            bytes: fs::metadata(&path).expect("probe case metadata").len(),
            path,
            sha256: std::env::var(PROBE_CASE_HASH_ENV).expect("probe case SHA256"),
        },
        MAX_JSON,
    );
    let bytes = case_lease.read();
    let case: ProbeCase = serde_json::from_value(strict_json(&bytes)).expect("strict probe case");
    assert_eq!(case.schema, 1);
    assert_eq!(case.expected_identity, "org.surge-synth-team.surge-xt-fx");
    assert_eq!(case.plugin.bytes, 9_640_960);
    assert_eq!(
        case.plugin.sha256,
        "30845ff762cd3642925a977b35d1fa86998d53ee6363eaa26504cd6a5161546f"
    );
    no_reparse(&case.evidence_directory);
    assert!(fs::metadata(&case.evidence_directory)
        .expect("fresh evidence directory")
        .is_dir());
    assert!(fs::read_dir(&case.evidence_directory)
        .expect("empty evidence directory")
        .next()
        .is_none());
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    write_new(&case.evidence_directory, "case.json", &bytes);
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };
    // The existing discovery implementation owns both fresh children and its
    // unchanged 5s deadline, 512KiB response and 64KiB state capture limits.
    let (captured, state) = probe_clap_plugin_registration_with_state(
        &helper.path,
        &plugin.path,
        &case.expected_identity,
        render,
        None,
        true,
    )
    .unwrap_or_else(|error| probe_failed(&case.evidence_directory, "default_capture", error));
    write_json(
        &case.evidence_directory,
        "captured-metadata.json",
        json!({
            "metadata": metadata(&captured), "captured_state_present": state.is_some(),
            "captured_state_bytes": state.as_ref().map(Vec::len),
            "captured_state_sha256": state.as_ref().map(|bytes| format!("{:x}", Sha256::digest(bytes))),
        }),
    );
    // Missing State support is an explicit failed qualification, never a skip
    // or synthetic state. A successful but empty save is retained then refused.
    let state = state.expect("actual CLAP default capture must support State");
    write_new(
        &case.evidence_directory,
        "original-default-state.bin",
        &state,
    );
    assert!(
        !state.is_empty() && state.len() <= 64 * 1024,
        "nonempty bounded actual state"
    );
    let fresh = probe_clap_plugin_registration(
        &helper.path,
        &plugin.path,
        &case.expected_identity,
        render,
        Some(&state),
    )
    .unwrap_or_else(|error| probe_failed(&case.evidence_directory, "fresh_restore", error));
    let captured_json = metadata(&captured);
    let fresh_json = metadata(&fresh);
    write_json(
        &case.evidence_directory,
        "fresh-restored-metadata.json",
        fresh_json.clone(),
    );
    for registration in [&captured, &fresh] {
        assert_eq!(registration.plugin_id, case.expected_identity);
        assert_eq!(registration.library_path, plugin.path);
        assert_eq!(
            registration.binary_sha256.as_slice(),
            decode_hash(&plugin.sha256).as_slice()
        );
        assert!(registration.execution_contract.offline_capable());
        assert!(!registration.execution_contract.realtime_capable());
    }
    assert_eq!(
        captured.execution_contract, fresh.execution_contract,
        "same default seed preserves realized execution facts"
    );
    assert_eq!(
        captured.auxiliary_inputs, fresh.auxiliary_inputs,
        "same default seed preserves ordered auxiliary contracts"
    );
    assert_eq!(
        declarations(&captured.parameters),
        declarations(&fresh.parameters),
        "same default seed preserves native parameter declarations"
    );
    let current_values_equal = captured.parameters.iter().all(|first| {
        fresh
            .parameters
            .iter()
            .find(|second| second.id == first.id)
            .is_some_and(|second| second.current_value.to_bits() == first.current_value.to_bits())
    });
    write_json(
        &case.evidence_directory,
        "result.json",
        json!({
            "schema": 1, "case_sha256": case_lease.sha256,
            "helper_sha256": helper.sha256, "plugin_sha256": plugin.sha256,
            "actual_identity": case.expected_identity,
            "render": { "sample_rate": 48_000, "channel_layout": "Stereo",
                "max_block_frames": 64, "processing_mode": "Offline" },
            "captured_state_bytes": state.len(),
            "captured_state_sha256": format!("{:x}", Sha256::digest(&state)),
            "parameter_count": captured.parameters.len(),
            "actual_default_capture_and_fresh_restore_metadata_agreement": true,
            "declarations_compared_by_native_id": true,
            "current_values_are_observations_only": true,
            "observed_current_values_equal": current_values_equal,
            "observed_full_typed_metadata_bytes_equal":
                serde_json::to_vec(&captured_json).expect("capture typed metadata")
                == serde_json::to_vec(&fresh_json).expect("fresh typed metadata"),
            "raw_discovery_wire_bytes_qualified": false,
            "state_recapture_equality_qualified": false,
            "nondefault_state_discrimination_qualified": false,
            "private_dsp_state_qualified": false, "processed_pcm": false,
            "native_bypass_tested": false, "project_archive_qualified": false,
            "gui_qualified": false, "realtime_qualified": false,
            "performance_qualified": false, "graph_preview_export_qualified": false,
            "full_compatibility_qualified": false, "outer_job_qualified_by_test": false,
        }),
    );
    // These read-only leases span both actual ProcessOwner-owned native calls.
    drop((case_lease, helper, plugin));
}

// Separate instrument observation: keep the original FX comparison and its failure unchanged.
const INSTRUMENT_PROBE_CASE_ENV: &str = "MONDRIAN_EXTERNAL_CLAP_INSTRUMENT_PROBE_CASE";
const INSTRUMENT_PROBE_CASE_HASH_ENV: &str = "MONDRIAN_EXTERNAL_CLAP_INSTRUMENT_PROBE_CASE_SHA256";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstrumentProbeCase {
    schema: u32,
    helper: Binding,
    plugin: Binding,
    evidence_directory: PathBuf,
}

#[test]
#[ignore = "hash-bound real CLAP instrument List/Probe case and frozen outer native Job required"]
fn external_clap_instrument_list_probe_default_capture_fresh_restore() {
    let path =
        PathBuf::from(std::env::var_os(INSTRUMENT_PROBE_CASE_ENV).expect("instrument case path"));
    let mut case_lease = Lease::open(
        &Binding {
            bytes: fs::metadata(&path).expect("instrument case metadata").len(),
            path,
            sha256: std::env::var(INSTRUMENT_PROBE_CASE_HASH_ENV).expect("instrument case SHA256"),
        },
        MAX_JSON,
    );
    let bytes = case_lease.read();
    let case: InstrumentProbeCase =
        serde_json::from_value(strict_json(&bytes)).expect("strict instrument case");
    assert_eq!(case.schema, 1);
    assert_eq!(case.plugin.bytes, 20_966_912);
    assert_eq!(
        case.plugin.sha256,
        "6fb598b3759d359ef670e2632ca0d8ff2c19be5389c0ba4801168643d7c5d219"
    );
    no_reparse(&case.evidence_directory);
    assert!(fs::metadata(&case.evidence_directory)
        .expect("fresh instrument directory")
        .is_dir());
    assert!(fs::read_dir(&case.evidence_directory)
        .expect("empty instrument directory")
        .next()
        .is_none());
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let dir = &case.evidence_directory;
    write_new(dir, "case.json", &bytes);
    let refuse = |stage: &str, reason: &str| -> ! {
        write_json(
            dir,
            "failure.json",
            json!({"schema":1, "stage":stage,
            "qualification_refusal":reason, "native_call_returned_error":false, "processed_pcm":false}),
        );
        panic!("{stage}: {reason}");
    };
    let descriptors =
        crate::processor_isolation::scan_clap_library_descriptors(&helper.path, &plugin.path)
            .unwrap_or_else(|error| probe_failed(dir, "list", error));
    write_json(
        dir,
        "list.json",
        json!({"schema":1, "descriptors":descriptors,
        "helper_sha256":helper.sha256, "plugin_sha256":plugin.sha256, "processed_pcm":false}),
    );
    if descriptors.len() != 1 {
        refuse("list_selection", "List requires exactly one descriptor");
    }
    let id = &descriptors[0].plugin_id;
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };
    let observed = |registration: &ClapPluginRegistration| {
        json!({
            "metadata":metadata(registration), "main_audio_input":registration.main_audio_input,
            "main_audio_input_inferred_from_name":false, "processed_pcm":false,
        })
    };
    // All calls retain the existing ProcessOwner, 5s/512KiB/1024-parameter/64KiB-state limits.
    let noncapture = probe_clap_plugin_registration(&helper.path, &plugin.path, id, render, None)
        .unwrap_or_else(|error| probe_failed(dir, "noncapture_probe", error));
    write_json(dir, "noncapture-metadata.json", observed(&noncapture));
    let (captured, state) = probe_clap_plugin_registration_with_state(
        &helper.path,
        &plugin.path,
        id,
        render,
        None,
        true,
    )
    .unwrap_or_else(|error| probe_failed(dir, "default_capture", error));
    write_json(
        dir,
        "captured-metadata.json",
        json!({"observation":observed(&captured),
        "captured_state_present":state.is_some(), "captured_state_bytes":state.as_ref().map(Vec::len),
        "captured_state_sha256":state.as_ref().map(|bytes| format!("{:x}", Sha256::digest(bytes)))}),
    );
    let Some(state) = state else {
        refuse("default_capture_state", "capture returned no State support");
    };
    if state.is_empty() || state.len() > 64 * 1024 {
        refuse(
            "default_capture_state",
            "State must be nonempty and <=64KiB",
        );
    }
    write_new(dir, "original-default-state.bin", &state);
    let fresh =
        probe_clap_plugin_registration(&helper.path, &plugin.path, id, render, Some(&state))
            .unwrap_or_else(|error| probe_failed(dir, "fresh_restore", error));
    write_json(dir, "fresh-restored-metadata.json", observed(&fresh));
    for registration in [&noncapture, &captured, &fresh] {
        if registration.plugin_id != *id
            || registration.library_path != plugin.path
            || registration.binary_sha256.as_slice() != decode_hash(&plugin.sha256).as_slice()
            || !registration.execution_contract.offline_capable()
            || registration.execution_contract.realtime_capable()
        {
            refuse(
                "identity_and_execution",
                "leased identity or offline mode changed",
            );
        }
    }
    // Match native IDs and the existing host's numeric structural semantics.
    // Name/hidden/current and enumeration order are observations, never this equality oracle.
    let structure_equal =
        captured.parameters.len() == fresh.parameters.len()
            && captured.parameters.iter().all(|expected| {
                fresh.parameters.iter().find(|actual| actual.id == expected.id).is_some_and(
                    |actual| {
                        actual.min_value == expected.min_value
                            && actual.max_value == expected.max_value
                            && actual.default_value == expected.default_value
                            && actual.automatable == expected.automatable
                            && actual.stepped == expected.stepped
                            && actual.enumeration == expected.enumeration
                            && actual.read_only == expected.read_only
                    },
                )
            });
    let contract_equal = captured.execution_contract == fresh.execution_contract
        && captured.main_audio_input == fresh.main_audio_input
        && captured.auxiliary_inputs == fresh.auxiliary_inputs;
    write_json(
        dir,
        "comparison.json",
        json!({"schema":1, "parameter_structure_equal":structure_equal,
        "realized_execution_and_input_contract_equal":contract_equal,
        "display_and_current_values_are_observations_only":true,
        "current_value_bits_equal_by_id":captured.parameters.iter().all(|first| fresh.parameters.iter()
            .find(|second| second.id == first.id).is_some_and(|second| second.current_value.to_bits() == first.current_value.to_bits())),
        "raw_discovery_wire_bytes_qualified":false, "processed_pcm":false}),
    );
    if !structure_equal || !contract_equal {
        refuse(
            "fresh_restore_comparison",
            "restored structure or host contract differs",
        );
    }
    write_json(
        dir,
        "result.json",
        json!({"schema":1, "case_sha256":case_lease.sha256,
        "helper_sha256":helper.sha256, "plugin_sha256":plugin.sha256, "actual_identity":id,
        "parameter_count":captured.parameters.len(), "main_audio_input":captured.main_audio_input,
        "captured_state_bytes":state.len(), "captured_state_sha256":format!("{:x}", Sha256::digest(&state)),
        "actual_list_probe_capture_and_fresh_restore_completed":true,
        "display_and_current_values_are_observations_only":true,
        "processed_pcm":false, "notes_qualified":false, "native_bypass_tested":false,
        "state_recapture_equality_qualified":false, "nondefault_state_discrimination_qualified":false,
        "gui_qualified":false, "realtime_qualified":false, "performance_qualified":false,
        "graph_preview_export_qualified":false, "full_compatibility_qualified":false,
        "outer_job_qualified_by_test":false}),
    );
    drop((case_lease, helper, plugin));
}

const AUDIO_PORT_CENSUS_CASE_ENV: &str = "MONDRIAN_CLAP_AUDIO_PORT_CENSUS_CASE";
const AUDIO_PORT_CENSUS_CASE_HASH_ENV: &str = "MONDRIAN_CLAP_AUDIO_PORT_CENSUS_CASE_SHA256";

#[test]
#[ignore = "hash-bound direct-native deactivated port census; frozen outer 180s Job required"]
fn external_clap_instrument_default_deactivated_audio_port_census() {
    let path =
        PathBuf::from(std::env::var_os(AUDIO_PORT_CENSUS_CASE_ENV).expect("census case path"));
    let mut case_lease = Lease::open(
        &Binding {
            bytes: fs::metadata(&path).expect("census case metadata").len(),
            path,
            sha256: std::env::var(AUDIO_PORT_CENSUS_CASE_HASH_ENV).expect("census case SHA256"),
        },
        MAX_JSON,
    );
    let bytes = case_lease.read();
    let case: InstrumentProbeCase =
        serde_json::from_value(strict_json(&bytes)).expect("strict census case");
    assert_eq!(case.schema, 1);
    no_reparse(&case.evidence_directory);
    assert!(fs::metadata(&case.evidence_directory).expect("census directory").is_dir());
    assert!(fs::read_dir(&case.evidence_directory)
        .expect("census directory entries")
        .next()
        .is_none());
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let dir = &case.evidence_directory;
    write_new(dir, "case.json", &bytes);
    // Only List uses the normal Product's existing 5s discovery owner. The
    // fresh census instance lives in this test process under the outer Job.
    let descriptors =
        crate::processor_isolation::scan_clap_library_descriptors(&helper.path, &plugin.path)
            .unwrap_or_else(|error| probe_failed(dir, "list", error));
    write_json(
        dir,
        "list.json",
        json!({"schema":1, "descriptors":descriptors,
        "helper_sha256":helper.sha256, "plugin_sha256":plugin.sha256, "processed_pcm":false}),
    );
    if descriptors.len() != 1 {
        probe_failed(
            dir,
            "list_selection",
            AudioProcessorHostError::InvalidContract(
                "CLAP census requires exactly one actual List descriptor".to_owned(),
            ),
        );
    }
    let id = &descriptors[0].plugin_id;
    let (observation, failure) =
        crate::processor_isolation::clap_discovery::deactivated_audio_port_census_for_test(
            &plugin.path,
            id,
        )
        .unwrap_or_else(|error| probe_failed(dir, "census_setup", error));
    // The helper returns only after native instance/module Drop has returned,
    // including when a bounded count/get refusal retained an incomplete prefix.
    write_json(dir, "census.json", observation);
    if let Some(error) = failure {
        probe_failed(dir, "deactivated_audio_port_census", error);
    }
    write_json(
        dir,
        "result.json",
        json!({
            "schema":1, "case_sha256":case_lease.sha256, "helper_sha256":helper.sha256,
            "plugin_sha256":plugin.sha256, "actual_identity":id,
            "topology_observation_complete":true, "native_instance_drop_returned":true,
            "native_library_drop_returned":true, "direct_native_census_in_test_process":true,
            "processing_probe_qualified":false, "processed_pcm":false, "notes_qualified":false,
            "state_qualified":false, "realtime_qualified":false, "full_compatibility_qualified":false,
            "direct_native_internal_deadline":false, "required_outer_job_deadline_ms":180000,
            "outer_job_qualified_by_test":false,
        }),
    );
    drop((case_lease, helper, plugin));
}
