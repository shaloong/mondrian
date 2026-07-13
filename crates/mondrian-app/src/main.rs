//! Product entrypoint for the app UI Mondrian editor shell.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().any(|argument| argument == "--verify-runtime") {
        mondrian_media::verify_ffmpeg_runtime()?;
        mondrian_core::ensure_mondrian_default_ocio_loaded().map_err(std::io::Error::other)?;
        println!("Mondrian packaged runtime is ready");
        return Ok(());
    }
    mondrian_app::app_ui::window::run_app_ui()
}
