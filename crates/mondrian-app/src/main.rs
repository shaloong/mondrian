//! Product entrypoint for the app UI Mondrian editor shell.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    mondrian_app::app_ui::window::run_app_ui()
}
