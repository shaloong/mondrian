//! Official AGain's captured state, constant Gain, and native Bypass in Runtime.
use super::vst3_runtime_pcm::{observe_runtime_pcm, RuntimeObservationConfiguration};
use super::*;
use crate::processor_isolation::vst3_discovery::{
    probe_vst3_plugin_registration_with_state, DiscoveryResponse,
};

const CLASS_ID: &str = "C18D3C1E719E4E29924D3ECAA5E4DA18";
// Assertion against the real capture, never a generated state passed to native.
const CAPTURED_STATE: [u8; 56] = [
    86, 83, 84, 51, 72, 79, 83, 84, 95, 83, 84, 65, 84, 69, 0, 0, 1, 0, 0, 0, 28, 0, 0, 0, 255,
    255, 255, 255, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0,
];

#[test]
#[ignore = "current official AGain Sample Accurate case, normal Product and outer native Job"]
fn external_vst3_again_runtime_gain_and_native_bypass_pcm() {
    let path = PathBuf::from(std::env::var_os(CASE_ENV).expect("official reference case path"));
    let mut case_lease = Lease::open(
        &Binding {
            bytes: fs::metadata(&path).expect("case metadata").len(),
            path,
            sha256: std::env::var(CASE_HASH_ENV).expect("case SHA256"),
        },
        MAX_JSON,
    );
    let bytes = case_lease.read();
    let case: VendorCase = serde_json::from_value(strict_json(&bytes)).expect("strict case schema");
    assert_eq!(case.schema, 1);
    assert_eq!(case.format, Format::Vst3);
    assert_eq!(case.expected_identity, CLASS_ID);
    assert_eq!(case.expected_parameter_count, 2);
    assert_eq!(
        case.bypass_native_id, 0,
        "zero is a real native parameter ID"
    );
    assert_eq!(case.opaque_state.bytes, 56);
    no_reparse(&case.evidence_directory);
    assert!(fs::metadata(&case.evidence_directory).unwrap().is_dir());
    assert_eq!(fs::read_dir(&case.evidence_directory).unwrap().count(), 0);
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let mut state_lease = Lease::open(&case.opaque_state, 128 * 1024);
    let mut list_lease = Lease::open(&case.captured_list_response, MAX_JSON);
    let mut probe_lease = Lease::open(&case.captured_probe_response, MAX_JSON);
    let mut input_lease = Lease::open(&case.pcm, (FRAMES * 2 * 4) as u64);
    let state = state_lease.read();
    assert_eq!(state.as_slice(), CAPTURED_STATE.as_slice());
    let list = strict_json(&list_lease.read());
    let captured = strict_json(&probe_lease.read());
    exact_keys(&list, &["kind", "schema_version", "descriptors"]);
    let DiscoveryResponse::List { schema_version, descriptors } =
        serde_json::from_value(list.clone()).expect("strict current List2")
    else {
        panic!("current List response required");
    };
    assert_eq!(schema_version, 2);
    assert_eq!(
        list,
        json!({
            "kind":"list_v2", "schema_version":2,
            "descriptors":[
                {"class_id":CLASS_ID,"name":"AGain Sample Accurate","vendor":"Steinberg Media Technologies","version":"3.8.0.0"},
                {"class_id":"CC48BF25529043DA80223510FFE8BD02","name":"Advanced Tutorial","vendor":"Steinberg Media Technologies","version":"3.8.0.0"}
            ]
        })
    );
    assert_eq!(
        descriptors.iter().filter(|d| d.class_id == CLASS_ID).count(),
        1
    );
    exact_keys(
        &captured,
        &[
            "kind",
            "schema_version",
            "event_bus_contract",
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
    let DiscoveryResponse::Probe {
        schema_version,
        event_bus_contract,
        class_id,
        vendor,
        latency_frames,
        tail_frames,
        parameters,
        current_values,
        captured_state,
        auxiliary_buses,
    } = serde_json::from_value(captured.clone()).expect("strict current Probe2")
    else {
        panic!("current Probe response required");
    };
    assert_eq!(schema_version, 2);
    assert_eq!(class_id, CLASS_ID);
    assert_eq!(vendor.as_deref(), Some("Steinberg Media Technologies"));
    assert_eq!((latency_frames, tail_frames), (0, 0));
    assert_eq!(
        json!(event_bus_contract),
        json!({"schema_version":1,"inputs":[],"outputs":[]})
    );
    assert!(auxiliary_buses.is_empty());
    assert_eq!(captured_state.as_deref(), Some(state.as_slice()));
    assert_eq!(
        captured["parameters"],
        json!([
            {"id":0,"name":"Bypass","default_normalized":0.0,"step_count":1,"can_automate":true,"read_only":false,"flags":65537},
            {"id":1,"name":"Gain","default_normalized":1.0,"step_count":0,"can_automate":true,"read_only":false,"flags":1}
        ])
    );
    assert_eq!(current_values, [(0, 0.0), (1, 0.0)]);
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(
            helper.path.clone(),
            [plugin.path.clone()],
        )
        .expect("actual complete reference List"),
    );
    assert_eq!(json!(catalog.descriptors()), json!(descriptors));
    assert_eq!(
        catalog.descriptors().iter().filter(|d| d.class_id == CLASS_ID).count(),
        1
    );
    let actual = probe_vst3_plugin_registration_with_state(
        &helper.path,
        &plugin.path,
        CLASS_ID,
        render,
        Some(&state),
        false,
    )
    .expect("actual current same-state restoration");
    assert!(actual.captured_state.is_none());
    assert_eq!(actual.registration.class_id, CLASS_ID);
    assert_eq!(
        actual.registration.binary_sha256.as_slice(),
        decode_hash(&plugin.sha256).as_slice()
    );
    assert_eq!(actual.registration.vendor, vendor);
    assert_eq!(actual.registration.parameters, parameters);
    assert_eq!(actual.current_values, current_values);
    assert_eq!(actual.registration.event_bus_contract, event_bus_contract);
    assert!(actual.registration.auxiliary_inputs.buses.is_empty());
    assert_eq!(
        actual.registration.execution_contract.algorithmic_latency_frames(),
        0
    );
    assert_eq!(
        actual.registration.execution_contract.tail(),
        AudioProcessorTail::None
    );
    let facts = Facts::Vst3(parameters, current_values);
    let input = deterministic_pcm();
    assert_eq!(input_lease.read(), pcm_bytes(&input));
    let unity: Vec<f32> = input.iter().map(|&raw| 0.0_f32 + raw).collect();
    // Independent of SDK helpers: one exact dyadic multiplication, followed by
    // canonical Graph +0 normalization. SDK /fp:fast does not change these
    // finite, non-subnormal dyadics. No ramp/offset-Bypass behavior is asserted.
    let half: Vec<f32> = unity.iter().map(|&raw| 0.0_f32 + raw * 0.5_f32).collect();
    assert!(half.iter().any(|sample| *sample != 0.0));
    assert!(half.iter().all(|sample| sample.is_finite()));
    assert!(half.iter().zip(&unity).any(|(a, b)| a.to_bits() != b.to_bits()));
    write_new(&case.evidence_directory, "case.json", &bytes);
    write_new(
        &case.evidence_directory,
        "original-opaque-state.bin",
        &state,
    );
    let period: Arc<[f32]> = input.into();
    for (label, native_bypass, expected) in [
        ("restore-a", false, &half),
        ("restore-b", false, &half),
        ("native-bypass", true, &unity),
    ] {
        let output = observe_runtime_pcm(
            catalog.clone(),
            &state,
            period.clone(),
            &facts,
            RuntimeObservationConfiguration {
                case: &case,
                constant_parameter_values: &[(1, 0.5)],
                sequence_name: "Official AGain captured state through canonical Runtime",
                auxiliary_input_key: None,
                musical_time_reference: None,
                native_bypass,
            },
            label,
            &case.evidence_directory,
        );
        assert_eq!(output.len(), expected.len());
        for (index, (actual, expected)) in output.iter().zip(expected).enumerate() {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{label} sample {index}"
            );
        }
    }
    write_json(
        &case.evidence_directory,
        "observations.json",
        json!({
            "schema":1, "canonical_runtime_and_output_rack":true, "runs":3,
            "class_id":CLASS_ID, "original_state_bytes":56,
            "native_bypass_id":0, "gain_native_id":1, "constant_gain":0.5,
            "musical_time_reference":null, "frames_per_run":FRAMES,
            "fresh_restore_half_gain_bit_exact":true, "native_bypass_unity_bit_exact":true,
            "scope":"48kHz stereo Float32, block64, SDK slice16, sequence-zero Hold constants",
            "nondefault_private_state_qualified":false,
            "ramps_offset_bypass_realtime_performance_qualified":false,
            "original_shutdown_failure_closed":false
        }),
    );
    drop((
        case_lease,
        helper,
        plugin,
        state_lease,
        list_lease,
        probe_lease,
        input_lease,
    ));
}
