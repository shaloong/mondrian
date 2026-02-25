//! # mondrian-assets
//!
//! 素材资产系统：媒体素材库（导入 / 检索）

pub mod library;
pub mod schema;

pub use library::{AssetKind, AssetLibrary, AssetRecord};
