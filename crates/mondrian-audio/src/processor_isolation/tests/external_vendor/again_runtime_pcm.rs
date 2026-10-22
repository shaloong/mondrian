//! Official AGain's captured state and Gain automation through the same Runtime.
use super::vst3_runtime_pcm::{
    observe_runtime_pcm, observe_runtime_pcm_with_automation, RuntimeAutomationStimulus,
    RuntimeObservationConfiguration,
};
use super::*;
use crate::processor_isolation::vst3_discovery::{
    probe_vst3_plugin_registration_with_state, DiscoveryResponse,
};
use mondrian_core::{AutomationSegmentInterpolation, ExactAutomationKeyframe, TimelineTime};

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
    run_again_runtime_case(false);
}

#[test]
#[ignore = "official AGain Gain automation RED diagnostic, current Product and original native Job"]
fn external_vst3_again_runtime_gain_automation_canonical_pcm() {
    run_again_runtime_case(true);
}

fn run_again_runtime_case(automation: bool) {
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
        serde_json::from_value(list.clone()).expect("strict current List7")
    else {
        panic!("current List response required");
    };
    assert_eq!(schema_version, 7);
    assert_eq!(
        list,
        json!({
            "kind":"list_v7", "schema_version":7,
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
            "main_audio_input",
            "audio_output_count",
            "main_audio_output_count",
            "class_id",
            "vendor",
            "latency_frames",
            "tail_frames",
            "parameters",
            "current_values",
            "captured_state",
            "auxiliary_buses",
            "input_names",
        ],
    );
    let DiscoveryResponse::Probe {
        schema_version,
        event_bus_contract,
        main_audio_input,
        audio_output_count,
        main_audio_output_count,
        class_id,
        vendor,
        latency_frames,
        tail_frames,
        parameters,
        current_values,
        captured_state,
        auxiliary_buses,
        input_names,
    } = serde_json::from_value(captured.clone()).expect("strict current Probe7")
    else {
        panic!("current Probe response required");
    };
    assert_eq!(schema_version, 7);
    assert_eq!(main_audio_input, crate::Vst3MainAudioInput::Present);
    assert_eq!(audio_output_count, 1);
    assert_eq!(main_audio_output_count, 1);
    assert_eq!(class_id, CLASS_ID);
    assert_eq!(vendor.as_deref(), Some("Steinberg Media Technologies"));
    assert_eq!((latency_frames, tail_frames), (0, 0));
    assert_eq!(
        json!(event_bus_contract),
        json!({"schema_version":1,"inputs":[],"outputs":[]})
    );
    assert!(auxiliary_buses.is_empty());
    assert!(input_names.is_empty());
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
    assert_eq!(actual.registration.main_audio_input, main_audio_input);
    assert_eq!(actual.registration.audio_output_count, audio_output_count);
    assert_eq!(
        actual.registration.main_audio_output_count,
        main_audio_output_count
    );
    assert_eq!(actual.input_names, input_names);
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
    if automation {
        observe_gain_automation(catalog, &case, &state, period, &facts);
        drop((
            case_lease,
            helper,
            plugin,
            state_lease,
            list_lease,
            probe_lease,
            input_lease,
        ));
        return;
    }
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

// Gain ticks are an independent literal sample table, not host/SDK evaluation.
// The first ramp is +2/64 per sample, the second -1/64, the third +2/64.
// The authored tail from sample128 through4095 is exactly32/64.
#[rustfmt::skip]
const GAIN_Q6_PREFIX: [u8; 129] = [
    // Samples0..15: hold1/4.
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
    // Samples16..31: linear1/4 at15 to3/4 at31.
    18, 20, 22, 24, 26, 28, 30, 32, 34, 36, 38, 40, 42, 44, 46, 48,
    // Samples32..63: hold3/4.
    48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48,
    48, 48, 48, 48, 48, 48, 48, 48,
    // Samples64..80: linear1/2 at64 to1/4 at80.
    32, 31, 30, 29, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19, 18, 17, 16,
    // Samples81..95: hold1/4.
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
    // Samples96..111: hold0, including the left key of the final ramp.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // Samples112..127: linear0 at111 to1/2 at127; sample128 stays1/2.
    2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32, 32,
];

fn gain_at_sample(sample: usize) -> f32 {
    f32::from(GAIN_Q6_PREFIX.get(sample).copied().unwrap_or(32)) / 64.0
}

fn gain_curve() -> ExactAutomationCurve {
    use AutomationSegmentInterpolation::{Hold, Linear};
    let id = ParameterId::new("vst3.param.1").unwrap();
    let mut curve = ExactAutomationCurve::new(id, 0.25).unwrap();
    for (sample, value, interpolation) in [
        (0, 0.25, Hold),
        (15, 0.25, Linear),
        (31, 0.75, Hold),
        (63, 0.75, Hold),
        (64, 0.5, Linear),
        (80, 0.25, Hold),
        (95, 0.25, Hold),
        (96, 0.0, Hold),
        (111, 0.0, Linear),
        (127, 0.5, Hold),
        (128, 0.5, Hold),
    ] {
        let mut key =
            ExactAutomationKeyframe::linear(TimelineTime::new(sample, 48_000).unwrap(), value);
        key.interpolation_to_next = interpolation;
        curve.set_keyframe(key).unwrap();
    }
    curve
}

fn observe_gain_automation(
    catalog: Arc<DiscoveredVst3AudioProcessorSpecResolver>,
    case: &VendorCase,
    state: &[u8],
    input: Arc<[f32]>,
    facts: &Facts,
) {
    let curves = [gain_curve()];
    let expected: Vec<f32> = input
        .chunks_exact(2)
        .enumerate()
        .flat_map(|(frame, stereo)| {
            stereo.iter().map(move |sample| 0.0_f32 + sample * gain_at_sample(frame))
        })
        .collect();
    assert_eq!(expected.len(), FRAMES * 2);
    assert!(expected[..30].iter().any(|sample| *sample != 0.0));
    assert!(expected[256..].iter().any(|sample| *sample != 0.0));
    assert!(expected[192..224].iter().all(|sample| sample.to_bits() == 0));
    write_new(
        &case.evidence_directory,
        "canonical-gain-expected.f32le",
        &pcm_bytes(&expected),
    );
    let mut reports = Vec::new();
    let mut total_mismatches = 0;
    let mut first_output: Option<Vec<f32>> = None;
    let mut partition_differences = Vec::new();
    for (label, pattern) in [
        ("automation-64", &[64][..]),
        ("automation-17-31-16", &[17, 31, 16][..]),
    ] {
        let output = observe_runtime_pcm_with_automation(
            catalog.clone(),
            state,
            input.clone(),
            facts,
            RuntimeObservationConfiguration {
                case,
                constant_parameter_values: &[],
                sequence_name: "Official AGain canonical Gain automation PCM diagnostic",
                auxiliary_input_key: None,
                musical_time_reference: None,
                native_bypass: false,
            },
            label,
            &case.evidence_directory,
            RuntimeAutomationStimulus { curves: &curves, window_pattern: pattern },
        );
        assert_eq!(output.len(), expected.len());
        if let Some(first) = &first_output {
            partition_differences = first
                .iter()
                .zip(&output)
                .enumerate()
                .filter(|(_, (a, b))| a.to_bits() != b.to_bits())
                .map(|(sample, (a, b))| {
                    json!({
                        "frame":sample / 2, "channel":sample % 2,
                        "pattern_64_bits":a.to_bits(), "pattern_17_31_16_bits":b.to_bits(),
                    })
                })
                .collect();
        } else {
            first_output = Some(output.clone());
        }
        let mismatches: Vec<_> = output
            .iter()
            .zip(&expected)
            .enumerate()
            .filter(|(_, (actual, wanted))| actual.to_bits() != wanted.to_bits())
            .map(|(sample, (actual, wanted))| {
                json!({
                    "frame":sample / 2, "channel":sample % 2,
                    "actual_bits":actual.to_bits(), "expected_bits":wanted.to_bits(),
                })
            })
            .collect();
        total_mismatches += mismatches.len();
        // Diagnostic only: the official sample advances Gain before multiplying.
        // Its beginChanges resets sampleCounter=0, processNextValuePoint subtracts
        // that counter without +/-1, and advance(1) may reach the following point.
        // This hypothesis never changes the canonical g[n] primary oracle or PASS.
        let mut forward_step_mismatches = Vec::new();
        let mut windows = Vec::new();
        let mut start = 0;
        let mut call = 0;
        while start < FRAMES {
            let frames = pattern[call % pattern.len()].min(FRAMES - start);
            let end = start + frames;
            windows.push((start, frames));
            for frame in start..end {
                let gain = gain_at_sample((frame + 1).min(end - 1));
                for channel in 0..2 {
                    let sample = frame * 2 + channel;
                    let diagnostic = 0.0_f32 + input[sample] * gain;
                    if output[sample].to_bits() != diagnostic.to_bits() {
                        forward_step_mismatches.push(sample);
                    }
                }
            }
            start = end;
            call += 1;
        }
        let witnesses: Vec<_> = [
            0,
            14,
            15,
            16,
            17,
            30,
            31,
            32,
            62,
            63,
            64,
            65,
            79,
            80,
            81,
            94,
            95,
            96,
            97,
            110,
            111,
            112,
            126,
            127,
            128,
            129,
            FRAMES - 2,
            FRAMES - 1,
        ]
        .into_iter()
        .map(|frame| {
            json!({
                "frame":frame, "gain_q6":GAIN_Q6_PREFIX.get(frame).copied().unwrap_or(32),
                "input_bits":[input[frame*2].to_bits(), input[frame*2+1].to_bits()],
                "expected_bits":[expected[frame*2].to_bits(), expected[frame*2+1].to_bits()],
                "actual_bits":[output[frame*2].to_bits(), output[frame*2+1].to_bits()],
            })
        })
        .collect();
        let first_end = pattern[0] * 2;
        let last_start = windows.last().unwrap().0 * 2;
        println!("AGAIN_GAIN_CANONICAL_DIAGNOSTIC: {label} mismatches={} forward_step_hypothesis_mismatches={}",
            mismatches.len(), forward_step_mismatches.len());
        reports.push(json!({
            "label":label, "public_window_pattern":pattern, "declared_windows":windows,
            "canonical_mismatch_count":mismatches.len(), "all_canonical_mismatches":mismatches,
            "boundary_and_terminal_witnesses":witnesses,
            "first_window_actual_bits":output[..first_end].iter().map(|v|v.to_bits()).collect::<Vec<_>>(),
            "last_window_actual_bits":output[last_start..].iter().map(|v|v.to_bits()).collect::<Vec<_>>(),
            "forward_step_hypothesis_only":{
                "changes_primary_oracle":false,
                "mismatch_sample_indices":forward_step_mismatches,
            },
        }));
    }
    write_json(
        &case.evidence_directory,
        "observations.json",
        json!({
            "schema":1, "class_id":CLASS_ID, "original_state_bytes":56,
            "gain_native_id":1, "native_bypass_authored":false,
            "frames_per_run":FRAMES, "runs":2, "gain_q6_prefix":GAIN_Q6_PREFIX.as_slice(),
            "canonical_mismatch_count":total_mismatches, "reports":reports,
            "cross_partition_difference_count":partition_differences.len(),
            "all_cross_partition_differences":partition_differences,
            "canonical_gain_pcm_pass":total_mismatches == 0,
            "partition_invariance_qualified":false, "host_bug_attribution":false,
            "offset_bypass_or_realtime_or_performance_qualified":false,
            "original_shutdown_failure_closed":false,
        }),
    );
    assert_eq!(total_mismatches, 0,
        "canonical g[n] Gain PCM differs; preserve both complete diagnostics, attribute no host/plugin cause");
}
