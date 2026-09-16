//! Bounded tests over production Thumbnail and device-catalog Modules.
//! This does not replace actual Host, native Window or GPU qualification.

#[cfg(test)]
#[path = "../src/app_ui/audio_device_catalog.rs"]
mod audio_device_catalog;
#[cfg(test)]
#[allow(dead_code)] // Full production Interface remains compiled for source-linked tests.
#[path = "../src/app/owned_worker_lifecycle.rs"]
pub(crate) mod owned_worker_lifecycle;
#[cfg(test)]
#[allow(dead_code)] // Thumbnail uses only its actual Still access-intent lowering.
#[path = "../src/app/preview_access_mode.rs"]
pub(crate) mod preview_access_mode;
#[cfg(test)]
#[path = "../src/app/single_worker_activity.rs"]
pub(crate) mod single_worker_activity;
#[cfg(test)]
#[allow(dead_code)] // Window-only raster/diagnostic fields remain in the real Module.
#[path = "../src/app/thumbnail_service.rs"]
mod thumbnail_service;

#[cfg(test)]
mod app {
    pub(crate) use crate::{owned_worker_lifecycle, preview_access_mode, single_worker_activity};
    pub use mondrian_app::app::ui_actions;
}

#[cfg(not(test))]
fn main() -> std::process::ExitCode {
    eprintln!("Run cargo test --release -p mondrian-app --features validation --example window_service_protocol -j 1 -- --test-threads=1");
    std::process::ExitCode::FAILURE
}
