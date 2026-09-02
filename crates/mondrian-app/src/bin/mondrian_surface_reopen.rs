//! Real Window/Surface and wgpu Device reopen validation entrypoint.

use anyhow::{bail, Context};
use mondrian_app::app::product_action::{ProductAction, TimelineProductAction};
use mondrian_core::Rational;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Serialize)]
struct SurfaceReopenReport<'a> {
    schema_version: u32,
    receipt_json: &'a str,
    receipt_sha256: &'a str,
}

fn main() -> anyhow::Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let (project_path, self_test_root, output_index) = match arguments.as_slice() {
        [mode, root, ..] if mode == "--self-test" => (None, Some(PathBuf::from(root)), 2),
        [project, ..] => (Some(PathBuf::from(project)), None, 1),
        [] => bail!(
            "expected <project.mdp> <output.json> <cycle-index> <operation-id>, or --self-test <work-root> <output.json> <cycle-index> <operation-id>"
        ),
    };
    if arguments.len() != output_index + 3 {
        bail!("invalid Surface/device reopen argument count");
    }
    let output_path = PathBuf::from(&arguments[output_index]);
    let cycle_index = arguments[output_index + 1]
        .to_string_lossy()
        .parse::<u32>()
        .context("invalid recovery cycle index")?;
    let operation_id = arguments[output_index + 2].to_string_lossy().into_owned();
    if output_path.exists() {
        bail!("output report already exists: {}", output_path.display());
    }

    let mut state = mondrian_app::app::AppState::new();
    if let Some(project_path) = project_path {
        state
            .open_project_file(project_path)
            .context("failed to open Surface/device reopen Project")?;
    } else {
        let self_test_root = self_test_root.context("missing self-test root")?;
        std::fs::create_dir_all(&self_test_root)?;
        state
            .create_new_project_at(
                self_test_root.join("surface-reopen-fixture.mdp"),
                "Surface Reopen Fixture",
                640,
                360,
                Rational::new(60, 1),
            )
            .context("failed to create Surface/device reopen fixture")?;
        state.ensure_minimum_tracks();
        state
            .dispatch_action(
                ProductAction::Timeline(TimelineProductAction::CreateBasicTitle)
                    .into_external_action(),
            )
            .context("failed to author Surface/device reopen fixture picture")?;
    }
    let receipt = mondrian_app::app_ui::window::run_app_ui_surface_device_reopen_validation(
        state,
        cycle_index,
        operation_id,
        Duration::from_secs(60),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let report = serde_json::to_vec(&SurfaceReopenReport {
        schema_version: 1,
        receipt_json: receipt.canonical_json(),
        receipt_sha256: receipt.sha256(),
    })?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    output.write_all(&report)?;
    output.sync_all()?;
    println!("MONDRIAN_SURFACE_REOPEN_REPORT={}", output_path.display());
    Ok(())
}
