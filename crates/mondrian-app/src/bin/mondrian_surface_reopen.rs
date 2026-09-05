//! Real Window/Surface and wgpu Device reopen validation entrypoint.

use anyhow::{bail, Context};
use mondrian_app::app::product_action::{ProductAction, TimelineProductAction};
use mondrian_core::Rational;
use serde::Serialize;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

const MAXIMUM_BATCH_CYCLES: u32 = 24;

#[derive(Serialize)]
struct SurfaceReopenReport<'a> {
    schema_version: u32,
    window_run_receipt_json: &'a str,
    window_run_receipt_sha256: &'a str,
    recovery_receipt_json: &'a str,
    recovery_receipt_sha256: &'a str,
    physical_native_termination_qualified: bool,
}

#[derive(Serialize)]
struct SurfaceReopenBatchReport<'a> {
    schema_version: u32,
    receipts: Vec<SurfaceReopenReport<'a>>,
}

fn parse_u32(argument: &OsStr, label: &str) -> anyhow::Result<u32> {
    argument
        .to_str()
        .with_context(|| format!("{label} is not valid UTF-8"))?
        .parse::<u32>()
        .with_context(|| format!("invalid {label}"))
}

fn strict_utf8<'a>(argument: &'a OsStr, label: &str) -> anyhow::Result<&'a str> {
    argument.to_str().with_context(|| format!("{label} is not valid UTF-8"))
}

fn main() -> anyhow::Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let (project_path, self_test_root, output_path, requests, batch_report) =
        match arguments.as_slice() {
            [mode, root, output, cycle, operation] if mode == "--self-test" => (
                None,
                Some(PathBuf::from(root)),
                PathBuf::from(output),
                vec![
                    mondrian_app::app_ui::window::AppUiSurfaceDeviceReopenValidationRequest {
                        cycle_index: parse_u32(cycle, "recovery cycle index")?,
                        operation_id: strict_utf8(operation, "operation id")?.to_owned(),
                        timeout: Duration::from_secs(60),
                    },
                ],
                false,
            ),
            [mode, root, output, first_cycle, count, operation_prefix]
                if mode == "--self-test-batch" =>
            {
                let first_cycle = parse_u32(first_cycle, "first recovery cycle index")?;
                let count = parse_u32(count, "recovery cycle count")?;
                if count == 0 {
                    bail!("recovery cycle count must be nonzero");
                }
                if count > MAXIMUM_BATCH_CYCLES {
                    bail!("recovery cycle count exceeds {MAXIMUM_BATCH_CYCLES}");
                }
                let operation_prefix = strict_utf8(operation_prefix, "operation prefix")?;
                let requests = (0..count)
                    .map(|offset| {
                        let cycle_index = first_cycle
                            .checked_add(offset)
                            .context("recovery cycle index overflow")?;
                        Ok(mondrian_app::app_ui::window::
                            AppUiSurfaceDeviceReopenValidationRequest {
                                cycle_index,
                                operation_id: format!("{operation_prefix}.{cycle_index}"),
                                timeout: Duration::from_secs(60),
                            })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                (
                    None,
                    Some(PathBuf::from(root)),
                    PathBuf::from(output),
                    requests,
                    true,
                )
            }
            [project, output, cycle, operation] => (
                Some(PathBuf::from(project)),
                None,
                PathBuf::from(output),
                vec![
                    mondrian_app::app_ui::window::AppUiSurfaceDeviceReopenValidationRequest {
                        cycle_index: parse_u32(cycle, "recovery cycle index")?,
                        operation_id: strict_utf8(operation, "operation id")?.to_owned(),
                        timeout: Duration::from_secs(60),
                    },
                ],
                false,
            ),
            _ => bail!(
                "expected <project.mdp> <output.json> <cycle-index> <operation-id>, --self-test <work-root> <output.json> <cycle-index> <operation-id>, or --self-test-batch <work-root> <output.json> <first-cycle> <count> <operation-prefix>"
            ),
        };
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
    let batch = mondrian_app::app_ui::window::run_app_ui_surface_device_reopen_validation_batch(
        state, requests,
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let reports = batch
        .receipts()
        .iter()
        .map(|receipt| SurfaceReopenReport {
            schema_version: 2,
            window_run_receipt_json: receipt.canonical_json(),
            window_run_receipt_sha256: receipt.sha256(),
            recovery_receipt_json: receipt.recovery_receipt().canonical_json(),
            recovery_receipt_sha256: receipt.recovery_receipt().sha256(),
            physical_native_termination_qualified: receipt.qualifies_physical_native_termination(),
        })
        .collect::<Vec<_>>();
    let report = if batch_report {
        serde_json::to_vec(&SurfaceReopenBatchReport { schema_version: 2, receipts: reports })?
    } else {
        let report = reports
            .first()
            .context("single Surface/device validation returned no receipt")?;
        serde_json::to_vec(report)?
    };
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
