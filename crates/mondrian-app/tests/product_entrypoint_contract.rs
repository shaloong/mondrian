//! Route-level guards for keeping the product on the app UI path.

use std::path::Path;

#[derive(serde::Deserialize)]
struct AppManifest {
    package: AppPackage,
    #[serde(rename = "bin")]
    bins: Vec<BinEntry>,
}

#[derive(serde::Deserialize)]
struct AppPackage {
    #[serde(rename = "default-run")]
    default_run: String,
}

#[derive(serde::Deserialize)]
struct BinEntry {
    name: String,
    path: String,
    #[serde(default, rename = "required-features")]
    required_features: Vec<String>,
    #[serde(default = "enabled_by_default")]
    test: bool,
    #[serde(default = "enabled_by_default")]
    bench: bool,
}

fn enabled_by_default() -> bool {
    true
}

fn assert_validation_only(bin: &BinEntry) {
    assert_eq!(
        bin.required_features,
        ["validation"],
        "{} must remain feature-gated",
        bin.name
    );
    assert!(
        !bin.test && !bin.bench,
        "{} must remain outside product test/benchmark routes",
        bin.name
    );
}

#[test]
fn product_default_run_targets_app_ui_main_binary() {
    let manifest = include_str!("../Cargo.toml");
    let manifest: AppManifest = toml::from_str(manifest).expect("valid application Cargo manifest");
    assert_eq!(manifest.package.default_run, "mondrian");
    let bins = manifest.bins;
    let product_bin = bins.iter().find(|bin| bin.name == "mondrian").expect("mondrian product bin");
    assert_eq!(product_bin.path, "src/main.rs");
    let golden_bin = bins
        .iter()
        .find(|bin| bin.name == "mondrian-golden")
        .expect("Golden validation bin");
    assert_eq!(golden_bin.path, "src/bin/mondrian_golden.rs");
    assert_validation_only(golden_bin);
    let surface_reopen_bin = bins
        .iter()
        .find(|bin| bin.name == "mondrian-surface-reopen")
        .expect("Surface reopen validation bin");
    assert_eq!(
        surface_reopen_bin.path,
        "src/bin/mondrian_surface_reopen.rs"
    );
    assert_validation_only(surface_reopen_bin);
    let endurance_bin = bins
        .iter()
        .find(|bin| bin.name == "mondrian-endurance")
        .expect("commercial endurance validation bin");
    assert_eq!(endurance_bin.path, "src/bin/mondrian_endurance.rs");
    assert_validation_only(endurance_bin);

    let allowed_bins = [
        "mondrian",
        "mondrian-golden",
        "mondrian-surface-reopen",
        "mondrian-endurance",
        "ui_demo",
        "ui_color_test",
        "ui_widget_test",
        "ui_pipeline_test",
        "viewer_gpu_output_budget",
    ];
    for bin in bins {
        assert!(
            allowed_bins.contains(&bin.name.as_str()),
            "unexpected mondrian-app binary `{}` must be classified before it becomes a route",
            bin.name
        );
        assert!(
            !bin.name.to_ascii_lowercase().contains("egui")
                && !bin.path.to_ascii_lowercase().contains("egui"),
            "legacy egui binary route must not be exposed: {} -> {}",
            bin.name,
            bin.path
        );
    }
}

#[test]
fn cross_platform_ci_covers_the_complete_app_and_windows_golden_entrypoint() {
    let workflow = include_str!("../../../.github/workflows/ci.yml");
    for required in [
        r#"- name: 测试完整 App 产品路径
        env:
          CARGO_INCREMENTAL: "0"
        run: cargo test -p mondrian-app --all-targets -j1"#,
        r#"- name: 编译 Golden 验证入口 (Windows)
        if: runner.os == 'Windows'
        env:
          CARGO_INCREMENTAL: "0"
        run: cargo build -p mondrian-app --features validation --bin mondrian-golden -j1"#,
    ] {
        assert!(
            workflow.contains(required),
            "cross-platform CI must retain required App contract: {required}"
        );
    }
}

#[test]
fn complete_golden_supervisor_builds_the_packaged_product_worker() {
    let supervisor =
        include_str!("../../../scripts/validation/invoke-complete-golden-project-gate.ps1");
    assert!(
        supervisor.contains(
            r#""--bin", "mondrian",
    "--bin", "mondrian-golden""#
        ),
        "the complete Golden gate must build the product executable that owns hidden media workers"
    );
    assert!(
        supervisor.contains("packaged_product_worker_sha256 = $builtProductExecutableSha256"),
        "the complete Golden report must bind the packaged product worker identity"
    );
}

#[test]
fn complete_golden_supervisor_build_is_non_incremental() {
    let supervisor =
        include_str!("../../../scripts/validation/invoke-complete-golden-project-gate.ps1");
    assert!(
        supervisor.contains(
            r#"[Environment]::SetEnvironmentVariable(
        "CARGO_INCREMENTAL",
        "0",
        "Process"
    )"#
        ),
        "the complete Golden build must not consume a mixed-feature incremental cache"
    );
    assert!(
        supervisor.contains(
            r#"[Environment]::SetEnvironmentVariable(
        "CARGO_INCREMENTAL",
        $oldCargoIncremental,
        "Process"
    )"#
        ),
        "the complete Golden supervisor must restore the caller's build environment"
    );
}

#[test]
fn complete_golden_supervisor_binds_the_process_report_schema() {
    let supervisor =
        include_str!("../../../scripts/validation/invoke-complete-golden-project-gate.ps1");
    let commercial_engine_contract =
        include_str!("../../../tests/validation/windows-commercial-engine.json");
    assert!(commercial_engine_contract.contains(r#""process_report_schema_version": 2"#));
    assert!(supervisor.contains("$Report.schema_version -ne $ExpectedSchemaVersion"));
    assert!(supervisor.contains("complete_golden.process_report_schema_version"));
}

#[test]
fn complete_golden_supervisor_separates_contract_owned_deadlines() {
    let supervisor =
        include_str!("../../../scripts/validation/invoke-complete-golden-project-gate.ps1");
    let commercial_engine_contract =
        include_str!("../../../tests/validation/windows-commercial-engine.json");

    for required_contract_field in ["build_timeout_seconds", "process_timeout_seconds"] {
        assert!(
            commercial_engine_contract.contains(required_contract_field),
            "commercial qualification must own `{required_contract_field}`"
        );
    }
    assert!(
        supervisor.contains(
            r#"Invoke-BoundedPlaybackGateProcess `
        "cargo" `
        $buildArguments `
        $repositoryRoot `
        $effectiveBuildTimeoutSeconds `"#
        ),
        "the cold build must use its independent contract-owned deadline"
    );
    assert!(
        supervisor.contains(
            r#"Invoke-TerminalReportGateProcess `
            $goldenExecutable `
            $arguments `
            $repositoryRoot `
            $effectiveProcessTimeoutSeconds `"#
        ),
        "each already-built Golden pass must use its independent execution deadline"
    );
}

#[test]
fn product_main_calls_only_the_app_ui_window_runner() {
    let main_rs = include_str!("../src/main.rs");

    assert!(
        main_rs.contains("mondrian_app::app_ui::window::run_app_ui()"),
        "product main must launch the app UI winit/wgpu shell"
    );
    for forbidden in ["run_native", "eframe", "egui_ui", "MondrianApp"] {
        assert!(
            !main_rs.contains(forbidden),
            "product main must not mention legacy UI route `{forbidden}`"
        );
    }
}

#[test]
fn legacy_egui_reference_code_is_removed_from_product_crate() {
    let manifest = include_str!("../Cargo.toml");
    let lib_rs = include_str!("../src/lib.rs");
    let app_rs = include_str!("../src/app/mod.rs");
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    for forbidden in [
        "mod egui_ui",
        "mod shortcuts",
        "legacy_egui",
        "MondrianApp",
        "AppPreferences",
    ] {
        assert!(
            !lib_rs.contains(forbidden) && !app_rs.contains(forbidden),
            "legacy UI token `{forbidden}` must not remain in app crate entry modules"
        );
    }

    for removed_path in ["src/egui_ui", "src/app/legacy_egui", "src/shortcuts.rs"] {
        assert!(
            !manifest_dir.join(removed_path).exists(),
            "legacy UI path `{removed_path}` must stay deleted"
        );
    }

    for forbidden_dependency in ["egui", "eframe", "egui-wgpu", "egui_extras"] {
        assert!(
            !manifest.contains(forbidden_dependency),
            "legacy UI dependency `{forbidden_dependency}` must stay removed from mondrian-app"
        );
    }
}
