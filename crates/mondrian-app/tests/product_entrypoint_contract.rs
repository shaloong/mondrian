//! Route-level guards for keeping legacy egui out of product launch paths.

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
fn product_default_run_targets_self_hosted_main_binary() {
    let manifest = include_str!("../Cargo.toml");
    let default_run = manifest
        .lines()
        .filter_map(|line| manifest_line_value(line.trim(), "default-run"))
        .next();
    assert_eq!(default_run.as_deref(), Some("mondrian"));

    let bins = manifest_bins(manifest);
    let product_bin = bins.iter().find(|bin| bin.name == "mondrian").expect("mondrian product bin");
    assert_eq!(product_bin.path, "src/main.rs");

    let allowed_bins = [
        "mondrian",
        "ui_demo",
        "ui_color_test",
        "ui_widget_test",
        "ui_pipeline_test",
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
fn product_main_calls_only_the_self_hosted_window_runner() {
    let main_rs = include_str!("../src/main.rs");

    assert!(
        main_rs.contains("mondrian_app::self_hosted::window::run_self_hosted_app()"),
        "product main must launch the self-hosted winit/wgpu shell"
    );
    for forbidden in ["run_native", "eframe", "egui_ui", "MondrianApp"] {
        assert!(
            !main_rs.contains(forbidden),
            "product main must not mention legacy UI route `{forbidden}`"
        );
    }
}

#[test]
fn legacy_egui_reference_is_not_public_crate_api() {
    let lib_rs = include_str!("../src/lib.rs");
    let app_rs = include_str!("../src/app/mod.rs");
    let legacy_boundary_rs = include_str!("../src/app/legacy_egui/mod.rs");
    let legacy_preferences_rs = include_str!("../src/app/legacy_egui/preferences_model.rs");

    assert!(
        lib_rs.contains("pub(crate) mod egui_ui;"),
        "legacy egui module should stay crate-private while it is reference code"
    );
    assert!(
        !lib_rs.contains("pub mod egui_ui;"),
        "legacy egui module must not be exported as public API"
    );
    assert!(
        lib_rs.contains("pub(crate) mod shortcuts;"),
        "legacy egui shortcut preference module should stay crate-private"
    );
    assert!(
        !lib_rs.contains("pub mod shortcuts;"),
        "legacy egui shortcut preference module must not expose egui key types as public API"
    );
    assert!(
        app_rs.contains("pub(crate) struct MondrianApp"),
        "legacy eframe app type should not be public crate API"
    );
    assert!(
        !app_rs.contains("pub struct MondrianApp"),
        "legacy eframe app type must not be exported as a product route"
    );
    assert!(
        app_rs.contains("mod legacy_egui;"),
        "legacy eframe modules should stay behind an explicit legacy boundary"
    );
    assert!(
        legacy_boundary_rs.contains("pub(in crate::app) mod preferences_model;"),
        "legacy egui preference schema should stay behind the legacy boundary"
    );
    assert!(
        !app_rs.contains("struct AppPreferences"),
        "legacy egui preference schema must not live in app/mod.rs"
    );
    assert!(
        legacy_preferences_rs.contains("struct AppPreferences"),
        "legacy egui preference schema should remain isolated while the reference app compiles"
    );
}
