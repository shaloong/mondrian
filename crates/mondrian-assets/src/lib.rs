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
    AssetKind, AssetLibrary, AssetLibraryMembership, AssetLibraryRemovalOutcome,
    AssetLibrarySnapshot, AssetMediaProbeCandidate, AssetRecord,
};
mod migration;
mod native_path;
pub use migration::ASSET_LIBRARY_SCHEMA_VERSION;

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
