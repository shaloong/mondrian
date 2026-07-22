//! Product entrypoint for the app UI Mondrian editor shell.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().any(|argument| argument == "--internal-demux-worker-v1") {
        return run_internal_demux_worker().map_err(Into::into);
    }
    if std::env::args_os().any(|argument| argument == "--verify-runtime") {
        mondrian_media::verify_ffmpeg_runtime()?;
        mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(std::io::Error::other)?;
        println!("Mondrian packaged runtime is ready");
        return Ok(());
    }
    mondrian_app::app_ui::window::run_app_ui()
}

fn run_internal_demux_worker() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    anyhow::ensure!(
        arguments.next().as_deref() == Some(std::ffi::OsStr::new("--internal-demux-worker-v1")),
        "invalid internal Preview demux worker mode"
    );
    anyhow::ensure!(
        arguments.next().is_none(),
        "unexpected Preview demux worker argument"
    );
    mondrian_media::run_preview_demux_worker()
}
