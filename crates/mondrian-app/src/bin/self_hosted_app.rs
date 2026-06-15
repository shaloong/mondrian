//! Mondrian self-hosted UI executable.
//!
//! Run with: `cargo run -p mondrian-app --bin self_hosted_app`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    mondrian_app::self_hosted::window::run_self_hosted_app()
}
