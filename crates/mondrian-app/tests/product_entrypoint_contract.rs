//! Route-level guards for keeping the product on the app UI path.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BinEntry {
    name: String,
    path: String,
}

fn manifest_line_value(line: &str, key: &str) -> Option<String> {
    let (actual_key, value) = line.split_once('=')?;
    (actual_key.trim() == key)
        .then(|| value.trim().trim_matches('"').to_owned())
        .filter(|value| !value.is_empty())
}

fn manifest_bins(manifest: &str) -> Vec<BinEntry> {
    let mut bins = Vec::new();
    let mut in_bin = false;
    let mut name = None;
    let mut path = None;

    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed == "[[bin]]" {
            if let (Some(name), Some(path)) = (name.take(), path.take()) {
                bins.push(BinEntry { name, path });
            }
            in_bin = true;
            continue;
        }
        if trimmed.starts_with('[') && trimmed != "[[bin]]" {
            if let (Some(name), Some(path)) = (name.take(), path.take()) {
                bins.push(BinEntry { name, path });
            }
            in_bin = false;
            continue;
        }
        if !in_bin {
            continue;
        }
        name = manifest_line_value(trimmed, "name").or(name);
        path = manifest_line_value(trimmed, "path").or(path);
    }

    if let (Some(name), Some(path)) = (name, path) {
        bins.push(BinEntry { name, path });
    }

    bins
}

#[test]
fn product_default_run_targets_app_ui_main_binary() {
    let manifest = include_str!("../Cargo.toml");
    let default_run = manifest
        .lines()
        .filter_map(|line| manifest_line_value(line.trim(), "default-run"))
        .next();
    assert_eq!(default_run.as_deref(), Some("mondrian"));

    let bins = manifest_bins(manifest);
    let product_bin = bins.iter().find(|bin| bin.name == "mondrian").expect("mondrian product bin");
    assert_eq!(product_bin.path, "src/main.rs");
    let golden_bin = bins
        .iter()
        .find(|bin| bin.name == "mondrian-golden")
        .expect("Golden validation bin");
    assert_eq!(golden_bin.path, "src/bin/mondrian_golden.rs");
    assert!(
        manifest.contains(
            r#"[[bin]]
name = "mondrian-golden"
path = "src/bin/mondrian_golden.rs"
required-features = ["validation"]
test = false
bench = false"#,
        ),
        "Golden validation must remain feature-gated and outside product/test routes"
    );

    let allowed_bins = [
        "mondrian",
        "mondrian-golden",
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
fn windows_ci_covers_the_complete_app_and_golden_entrypoint() {
    let workflow = include_str!("../../../.github/workflows/ci.yml");
    for required in [
        r#"- name: 测试完整 App 产品路径 (Windows)
        if: runner.os == 'Windows'
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
            "Windows CI must retain required App contract: {required}"
        );
    }
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
