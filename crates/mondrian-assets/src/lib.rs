//! # mondrian-assets
//!
//! 素材资产系统：媒体素材库（导入 / 检索）

pub mod audio_catalog;
pub mod generator;
pub mod library;
pub mod schema;

pub use audio_catalog::{
    AssetAudioComponent, AssetAudioComponentCatalog, AssetAudioStreamBinding,
    AudioComponentCatalogError,
};
pub use library::{AssetKind, AssetLibrary, AssetRecord};
mod migration;
pub use migration::ASSET_LIBRARY_SCHEMA_VERSION;
