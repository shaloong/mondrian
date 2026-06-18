//! Product entrypoint for the self-hosted Mondrian editor shell.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    mondrian_app::self_hosted::window::run_self_hosted_app()
}
