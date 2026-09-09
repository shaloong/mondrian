//! Process-main owner for real ordinary Mondrian COL-045 captures.
use anyhow::{ensure, Context};
use std::path::PathBuf;
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let input =
        PathBuf::from(args.next().context("expected stimulus directory and new output directory")?);
    let output = PathBuf::from(args.next().context("expected new output directory")?);
    ensure!(args.next().is_none(), "unexpected argument");
    let report =
        mondrian_app::app::golden_project_acceptance::run_cross_application_capture(input, output)?;
    println!("MONDRIAN_CROSS_APPLICATION_CAPTURE={}", report.display());
    Ok(())
}
