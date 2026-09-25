//! Native OpenFX ABI qualification against an independently built reference plugin.

use std::path::{Path, PathBuf};

use mondrian_app::openfx_adapter::inspect_openfx_binary;

fn product_executable() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_mondrian"))
}

#[test]
fn unavailable_binary_never_enters_native_worker() {
    let missing = PathBuf::from("not-an-absolute-openfx-plugin.ofx");
    assert!(inspect_openfx_binary(product_executable(), &missing).is_err());
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_REFERENCE_BINARY to the independently built official Basic example"]
fn official_basic_image_effect_header_is_discovered_in_child() {
    let reference = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY")
            .expect("official Basic plugin path is required"),
    );
    let inspection = inspect_openfx_binary(product_executable(), &reference)
        .expect("real OpenFX descriptor from supervised child");
    assert_eq!(inspection.binary_sha256.len(), 64);
    assert!(inspection.plugins.iter().any(|plugin| {
        plugin.identifier == "uk.co.thefoundry.BasicGainPlugin" && plugin.api_version == 1
    }));
}
