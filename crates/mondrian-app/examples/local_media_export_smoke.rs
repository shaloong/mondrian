//! Dedicated process main for independent real-media repeated Export validation.
use anyhow::{ensure, Context};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let root =
        PathBuf::from(args.next().context("expected real fixture root and new output directory")?);
    let output = PathBuf::from(args.next().context("expected new output directory")?);
    ensure!(args.next().is_none(), "unexpected argument");
    let report =
        mondrian_app::app::golden_project_acceptance::run_local_media_export_smoke(root, output)?;
    println!("MONDRIAN_LOCAL_MEDIA_EXPORT_SMOKE={}", report.display());
    Ok(())
}
