//! Dedicated process main for the bounded proxy-control three-phase workflow.
use anyhow::{ensure, Context};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args_os().skip(1);
    let root =
        PathBuf::from(args.next().context("expected real fixture root and new output directory")?);
    let output = PathBuf::from(args.next().context("expected new output directory")?);
    let seconds = args
        .next()
        .map(|value| value.to_string_lossy().parse::<u64>())
        .transpose()?
        .unwrap_or(30);
    ensure!(args.next().is_none(), "unexpected argument");
    let report = mondrian_app::app::golden_project_acceptance::run_local_media_proxy_smoke(
        root, output, seconds,
    )?;
    println!("MONDRIAN_LOCAL_MEDIA_PROXY_SMOKE={}", report.display());
    Ok(())
}
