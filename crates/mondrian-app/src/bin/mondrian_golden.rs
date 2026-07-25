//! Dedicated process entrypoint for complete Golden Project validation.
//!
//! Heavy media and GPU execution must run on the process main lifetime rather
//! than a short-lived libtest worker thread. The outer validation supervisor
//! consumes the typed report emitted by this binary.

use anyhow::{bail, Context};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    let Some(command) = arguments.next() else {
        bail!("expected command: complete-run");
    };
    if command != "complete-run" {
        bail!("unsupported Golden command: {}", command.to_string_lossy());
    }

    let mut output = None;
    while let Some(argument) = arguments.next() {
        if argument != "--output" {
            bail!("unexpected Golden argument: {}", argument.to_string_lossy());
        }
        let path = arguments.next().context("--output requires a report path")?;
        if output.replace(PathBuf::from(path)).is_some() {
            bail!("--output may be provided only once");
        }
    }

    let report = mondrian_app::app::golden_project_acceptance::run_complete_golden_project(output)?;
    println!("MONDRIAN_GOLDEN_COMPLETE_REPORT_PATH={}", report.display());
    Ok(())
}
