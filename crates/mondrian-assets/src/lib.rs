//! # mondrian-assets
//!
//! 素材资产系统：媒体素材库（导入 / 检索）

pub mod audio_catalog;
pub mod library;
pub mod schema;

pub use audio_catalog::{
    AssetAudioComponent, AssetAudioComponentCatalog, AssetAudioStreamBinding,
    AudioComponentCatalogError,
};
pub use library::{
    AssetKind, AssetLibrary, AssetLibraryMembership, AssetLibraryMoveOutcome,
    AssetLibraryRemovalOutcome, AssetLibrarySnapshot, AssetMediaProbeCandidate, AssetRecord,
};
mod migration;
mod native_path;
pub use migration::ASSET_LIBRARY_SCHEMA_VERSION;

/// Resolve an existing file into the ordinary canonical native namespace used
/// by persisted Asset Library identity.
///
/// On Windows this deliberately removes the physical-I/O `\\?\` spelling;
/// callers comparing a candidate with [`AssetRecord::file_path`] must use this
/// boundary instead of `std::fs::canonicalize` directly.
pub fn canonical_asset_file_path(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    native_path::ordinary_canonical_path(path)
}

/// Resolve one existing path into the ordinary canonical native namespace.
///
/// This is the cross-crate identity boundary for filesystem ownership: on
/// Windows it removes the physical-I/O `\\?\` spelling and on Unix it resolves
/// through symlinks (macOS `/var` -> `/private/var`), so a path spelled
/// through either form compares equal. Callers must never compare a
/// canonicalized path against a raw `std::fs::canonicalize` result.
pub fn canonical_native_path(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    native_path::ordinary_canonical_path(path)
}

#[cfg(test)]
mod dependency_direction_tests {
    #[test]
    fn authoring_asset_library_does_not_depend_on_media_execution() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.contains("mondrian-media") && !manifest.contains("mondrian_media"),
            "mondrian-assets must consume foundation media contracts, not mondrian-media"
        );
    }
}
