#![cfg(windows)]
use mondrian_validation_launcher::{FileBinding, LaunchPlan, LaunchReport};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn binding(path: &Path) -> FileBinding {
    FileBinding {
        path: path.to_path_buf(),
        sha256: format!(
            "{:x}",
            Sha256::digest(std::fs::read(path).expect("fixture bytes"))
        ),
    }
}
fn campaign(mode: &str) -> (std::process::ExitStatus, LaunchReport) {
    let root = tempfile::tempdir().expect("fixture directory");
    let launcher = binding(Path::new(env!(
        "CARGO_BIN_EXE_mondrian-validation-launcher"
    )));
    let application = binding(Path::new(env!("CARGO_BIN_EXE_native_bootstrap_probe")));
    let runtime = binding(
        &PathBuf::from(std::env::var_os("SystemRoot").expect("Windows system root"))
            .join("System32/kernel32.dll"),
    );
    let machine_path = root.path().join("machine.json");
    std::fs::write(&machine_path, serde_json::to_vec(&serde_json::json!({ "verifier_tools": { "preloader": launcher, "runtime_files": [runtime] } })).expect("machine JSON")).expect("machine file");
    let request_path = root.path().join("request.json");
    std::fs::write(&request_path, serde_json::to_vec(&serde_json::json!({ "machine_plan": binding(&machine_path), "identity": { "runtime_image_sha256": application.sha256 }, "output_manifest_path": root.path().join("manifest.json"), "fixture_mode": mode })).expect("request JSON")).expect("request file");
    let report_path = root.path().join("launch-report.json");
    let plan = LaunchPlan {
        schema_version: 1,
        launcher,
        application,
        runtime_files: vec![runtime],
        request: binding(&request_path),
        deadline_ms: 15_000,
        report_path: report_path.clone(),
    };
    let plan_path = root.path().join("launch.json");
    std::fs::write(&plan_path, serde_json::to_vec(&plan).expect("plan JSON")).expect("plan file");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_mondrian-validation-launcher"))
        .arg(&plan_path)
        .status()
        .expect("native launcher");
    let report =
        serde_json::from_slice(&std::fs::read(&report_path).expect("raw launcher receipt"))
            .expect("typed raw receipt");
    (status, report)
}

#[test]
fn native_fresh_child_attests_objects_and_closes_exact_namespace() {
    let (status, report) = campaign("success");
    assert!(status.success(), "{:?}", report.errors);
    assert!(report.errors.is_empty());
    assert!(report.capsule_removed && report.descendants_reaped);
    assert!(report.child_manifest.is_some());
    let evidence = report.attestation.expect("real native handshake");
    assert_ne!(evidence.launcher_pid, evidence.child_pid);
    assert_eq!(evidence.owned_images.len(), 2);
    assert!(evidence.mapped_image_paths.contains(&evidence.owned_images[0].staged_path));
    assert!(!evidence.owned_images[0].staged_path.parent().expect("capsule").exists());
}

#[test]
fn root_exit_with_live_descendant_is_failed_then_reaped() {
    let (status, report) = campaign("descendant");
    assert!(!status.success());
    assert!(report.errors.iter().any(|error| error.contains("live native descendants")));
    assert!(report.descendants_reaped && report.capsule_removed);
}

#[test]
fn child_refusal_retains_failure_without_fabricating_attestation() {
    let (status, report) = campaign("refuse");
    assert!(!status.success());
    assert!(report.attestation.is_none());
    assert!(!report.errors.is_empty());
    assert!(report.descendants_reaped && report.capsule_removed);
}
