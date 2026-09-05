//! Real Window/Surface and wgpu Device reopen validation entrypoint.

use anyhow::{bail, Context};
use mondrian_app::app::product_action::{ProductAction, TimelineProductAction};
use mondrian_app::app_ui::surface_reopen_batch_receipt::AppUiSurfaceDeviceReopenValidationReceipt;
use mondrian_app::app_ui::surface_reopen_report::publish_surface_reopen_validation_report;
use mondrian_core::Rational;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

const MAXIMUM_BATCH_CYCLES: u32 = 24;

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
    let (project_path, self_test_root, output_path, requests) =
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
    let outcome = mondrian_app::app_ui::window::run_app_ui_surface_device_reopen_validation_batch(
        state, requests,
    );
    let (receipt, validation_failure) = match outcome {
        Ok(batch) => (
            AppUiSurfaceDeviceReopenValidationReceipt::seal_success(&batch)
                .context("failed to seal successful Surface/device validation")?,
            None,
        ),
        Err(error) => {
            let receipt = AppUiSurfaceDeviceReopenValidationReceipt::seal_failure(&error)
                .context("failed to seal failed Surface/device validation")?;
            (receipt, Some(error.to_string()))
        }
    };
    publish_surface_reopen_validation_report(&output_path, &receipt)
        .context("failed to publish Surface/device validation report")?;
    println!("MONDRIAN_SURFACE_REOPEN_REPORT={}", output_path.display());
    if let Some(failure) = validation_failure {
        bail!(failure);
    }
    Ok(())
}
