//! Dedicated process main for the bounded local real-media software smoke.
use anyhow::{ensure, Context};
use std::path::PathBuf;
fn main() -> anyhow::Result<()> {
    mondrian_platform::prepare_graphics_process()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize local smoke diagnostics: {error}"))?;
    let mut args = std::env::args_os().skip(1);
    let root = args.next().context("expected fixture root; use --help")?;
    if root == "--help" {
        println!("local_media_smoke <fixture-root> <new-output-dir> <seconds> <640x360|1920x1080|3840x2160> <24|30|60> <all|playback|export|recovery> <1|2> <rec709|pq|hlg> <mixed|single60>\n\nall: three production phases, 15–900 seconds each\nplayback: real Preview/Audio with seek/cache, 15–1800 seconds\nexport: continuous Export with Playback alone, 15–900 seconds\nrecovery: the same third phase alone, 15–900 seconds\nOutput cadence and Program Output are explicit; original source resolution, cadence and color metadata remain unchanged.\nlayers: full-duration video source count, quality remains Full\nmixed: rotate the three source cadences\nsingle60: only the proven 3840x2160 Main10 60/1 source, one layer and 60 fps output; two-second clips still repeat\nSource provenance must be independently verified; a 60/1 probe does not disprove prior retiming.\nExport remains active until observation ends; 256-artifact/2-GiB phase budgets fail closed.\nNot a commercial/HDR/physical Viewer qualification.");
        return Ok(());
    }
    let root = PathBuf::from(root);
    let output = PathBuf::from(args.next().context("expected new output directory")?);
    let seconds = args.next().context("expected seconds")?.to_string_lossy().parse::<u64>()?;
    let resolution = match args.next().context("expected output resolution")?.to_str() {
        Some("640x360") => mondrian_core::Resolution { width: 640, height: 360 },
        Some("1920x1080") => mondrian_core::Resolution { width: 1920, height: 1080 },
        Some("3840x2160") => mondrian_core::Resolution { width: 3840, height: 2160 },
        _ => anyhow::bail!("expected explicit 640x360, 1920x1080 or 3840x2160"),
    };
    let frame_rate = match args.next().context("expected output frame rate")?.to_str() {
        Some("24") => mondrian_core::Rational::FPS_24,
        Some("30") => mondrian_core::Rational::FPS_30,
        Some("60") => mondrian_core::Rational::FPS_60,
        _ => anyhow::bail!("expected explicit 24, 30 or 60 fps"),
    };
    use mondrian_app::app::golden_project_acceptance::{
        LocalMediaSmokePhases, LocalMediaSmokeSettings, LocalMediaSmokeSources,
    };
    let phases = match args.next().context("expected phases")?.to_str() {
        Some("all") => LocalMediaSmokePhases::All,
        Some("playback") => LocalMediaSmokePhases::PlaybackOnly,
        Some("export") => LocalMediaSmokePhases::ExportWithPlaybackOnly,
        Some("recovery") => LocalMediaSmokePhases::ConcurrentRecoveryOnly,
        _ => anyhow::bail!("expected all, playback, export or recovery"),
    };
    let video_layers =
        args.next().context("expected video layers")?.to_string_lossy().parse::<u8>()?;
    let program_color_space =
        match args.next().context("expected Program Output: rec709, pq or hlg")?.to_str() {
            Some("rec709") => mondrian_core::ColorSpace::Rec709,
            Some("pq") => mondrian_core::ColorSpace::Rec2100Pq,
            Some("hlg") => mondrian_core::ColorSpace::Rec2100Hlg,
            _ => anyhow::bail!("expected Program Output: rec709, pq or hlg"),
        };
    let video_sources = match args.next().context("expected sources: mixed or single60")?.to_str() {
        Some("mixed") => LocalMediaSmokeSources::MixedCadence,
        Some("single60") => LocalMediaSmokeSources::Single60,
        _ => anyhow::bail!("expected sources: mixed or single60"),
    };
    ensure!(args.next().is_none(), "unexpected argument");
    let report = mondrian_app::app::golden_project_acceptance::run_local_media_smoke(
        root,
        output,
        LocalMediaSmokeSettings {
            phase_seconds: seconds,
            resolution,
            frame_rate,
            phases,
            video_layers,
            video_sources,
            program_color_space,
        },
    )?;
    println!("MONDRIAN_LOCAL_MEDIA_SMOKE={}", report.display());
    Ok(())
}
