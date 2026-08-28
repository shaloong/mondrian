use mondrian_renderer::{
    GpuColorQualificationExecutionPolicy, GPU_COLOR_QUALIFICATION_POLICY_ENV,
    SEALED_GPU_COLOR_QUALIFICATION_POLICY,
};
use serde_json::Value;
use std::collections::BTreeSet;

const PROFILE_JSON: &str = include_str!("../../../tests/validation/gpu-color-qualification.json");
const COMMERCIAL_JSON: &str =
    include_str!("../../../tests/validation/windows-commercial-engine.json");
const WORKFLOW: &str = include_str!("../../../.github/workflows/windows-commercial-engine.yml");
const SUPERVISOR: &str =
    include_str!("../../../scripts/validation/invoke-gpu-color-qualification.ps1");
const SEALER: &str =
    include_str!("../../../scripts/validation/resolve-commercial-engine-evidence.ps1");
const COLOR_STAGE: &str = include_str!("../src/color_stage.rs");
const YUV_DECODE: &str = include_str!("../src/native_video/yuv_decode.rs");
const COLOR_PERF: &str = include_str!("color_view_gpu_perf.rs");

fn gate_source<'a>(gate: &Value) -> &'a str {
    match gate["target"].as_str().expect("target") {
        "lib" if gate["test"].as_str().is_some_and(|test| test.starts_with("color_stage::")) => {
            COLOR_STAGE
        }
        "lib" => YUV_DECODE,
        "integration" => COLOR_PERF,
        unexpected => panic!("unsupported profile target {unexpected}"),
    }
}

#[test]
fn sealed_gpu_color_profile_owns_complete_non_skippable_gate_set() {
    let profile: Value = serde_json::from_str(PROFILE_JSON).expect("GPU color profile JSON");
    assert_eq!(profile["schema_version"], 1);
    assert_eq!(profile["id"], "windows-gpu-color-v1");
    assert_eq!(
        profile["execution_policy"],
        SEALED_GPU_COLOR_QUALIFICATION_POLICY
    );
    assert_eq!(
        profile["policy_environment"],
        GPU_COLOR_QUALIFICATION_POLICY_ENV
    );
    assert_eq!(
        GpuColorQualificationExecutionPolicy::from_value(profile["execution_policy"].as_str()),
        Ok(GpuColorQualificationExecutionPolicy::SealedRequired)
    );

    let labels = profile["required_runner_labels"]
        .as_array()
        .expect("runner labels")
        .iter()
        .map(|value| value.as_str().expect("string label"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        labels,
        BTreeSet::from([
            "self-hosted",
            "Windows",
            "X64",
            "mondrian-reference",
            "mondrian-gpu-color",
        ])
    );
    assert_eq!(profile["required_adapter"]["backend"], "Dx12");
    assert_eq!(profile["required_adapter"]["device_type"], "DiscreteGpu");
    for invariant in [
        "machine_inventory_name_match_required",
        "driver_identity_required",
        "same_adapter_across_reports_required",
    ] {
        assert_eq!(profile["required_adapter"][invariant], true);
    }

    let gates = profile["gates"].as_array().expect("gate array");
    let gate_ids = gates
        .iter()
        .map(|gate| gate["id"].as_str().expect("gate id"))
        .collect::<BTreeSet<_>>();
    let required = BTreeSet::from([
        "output-smoke",
        "standard-all-views-accuracy",
        "standard-rec709-accuracy",
        "standard-pq-delta-e-itp",
        "aces-pq-delta-e-itp",
        "native-yuv-color-accuracy",
        "standard-view-4k-performance",
        "standard-input-4k-performance",
    ]);
    assert_eq!(gate_ids, required);
    assert_eq!(gates.len(), 8);

    let mut report_environments = BTreeSet::new();
    for gate in gates {
        assert!(gate["timeout_seconds"].as_u64().is_some_and(|timeout| timeout > 0));
        let test = gate["test"].as_str().expect("test name");
        let function_name = test.rsplit("::").next().expect("test function name");
        let gate_id = gate["id"].as_str().expect("gate id");
        let source = gate_source(gate);
        assert!(
            source.contains(&format!("\"{gate_id}\"")),
            "profile gate '{gate_id}' is not the execution-policy identity in its test source"
        );
        match gate["target"].as_str().expect("target") {
            "lib" => assert!(
                COLOR_STAGE.contains(function_name) || YUV_DECODE.contains(function_name),
                "profile test '{test}' is not present in its unit-test source"
            ),
            "integration" => {
                assert_eq!(gate["integration_test"], "color_view_gpu_perf");
                assert!(
                    COLOR_PERF.contains(function_name),
                    "missing integration test '{test}'"
                );
                assert_eq!(
                    gate["ignored"], true,
                    "hardware perf gates are explicit ignored tests"
                );
            }
            unexpected => panic!("unsupported profile target {unexpected}"),
        }
        if let Some(report) = gate.get("report") {
            assert!(report_environments
                .insert(report["environment"].as_str().expect("report environment")));
            assert!(report["schema_version"].as_u64().is_some());
            assert!(!report["scenario"].as_str().expect("scenario").is_empty());
        }
    }
    assert_eq!(report_environments.len(), 3);
    for invariant in [
        "all_gates_must_run",
        "all_gate_processes_must_pass",
        "skipped_reports_forbidden",
        "report_hashes_required",
        "source_sha_required",
        "clean_machine_report_required",
    ] {
        assert_eq!(profile["acceptance"][invariant], true);
    }
}

#[test]
fn commercial_workflow_and_sealer_require_gpu_color_evidence() {
    let profile: Value = serde_json::from_str(PROFILE_JSON).expect("GPU profile");
    let commercial: Value = serde_json::from_str(COMMERCIAL_JSON).expect("commercial contract");
    assert_eq!(commercial["schema_version"], 2);
    assert_eq!(commercial["gpu_color"]["profile_id"], profile["id"]);

    let profile_gate_ids = profile["gates"]
        .as_array()
        .expect("profile gates")
        .iter()
        .map(|gate| gate["id"].as_str().expect("profile gate id"))
        .collect::<BTreeSet<_>>();
    let commercial_gate_ids = commercial["gpu_color"]["required_gate_ids"]
        .as_array()
        .expect("commercial GPU gates")
        .iter()
        .map(|gate| gate.as_str().expect("commercial gate id"))
        .collect::<BTreeSet<_>>();
    assert_eq!(profile_gate_ids, commercial_gate_ids);

    for required in [
        "mondrian-gpu-color",
        "invoke-gpu-color-qualification.ps1",
        "-GpuColorReportPath",
        "gpu-color-qualification.json",
    ] {
        assert!(
            WORKFLOW.contains(required),
            "workflow is missing '{required}'"
        );
    }
    for required in [
        "Sealed GPU color qualification requires Windows",
        "did not prove execution of exactly one passing test",
        "emitted a forbidden diagnostic skip",
        "same_adapter_across_reports_required",
        "$process.Kill($true)",
        "CARGO_INCREMENTAL",
        "Observed wgpu adapter has no stable adapter name",
        "has weakened a mandatory acceptance invariant",
        "gpu-color-qualification.json",
    ] {
        assert!(
            SUPERVISOR.contains(required),
            "supervisor is missing '{required}'"
        );
    }
    for required in [
        "Assert-GpuColorReport",
        "exactly one report for every profile gate",
        "adapter identity does not match the sealed machine inventory",
        "gpu-color.json",
        "manifest.schema_version -ne 2",
    ] {
        assert!(SEALER.contains(required), "sealer is missing '{required}'");
    }
}
