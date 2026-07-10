//! Shared raster image view model for widget-level image presentation.
//!
//! Widgets use this type to describe already-decoded RGBA images without
//! owning decoding, file access, or GPU texture lifetime.

use mondrian_ui_core::RasterImageColorSpace;
use std::sync::Arc;

/// RGBA image payload presented through the renderer-owned raster atlas.
#[derive(Debug, Clone)]
pub struct RasterImage {
    /// Stable image cache key for the renderer-owned raster atlas.
    pub key: String,
    /// Source image width in pixels.
    pub width: u32,
    /// Source image height in pixels.
    pub height: u32,
    /// Color space in which the RGBA8 bytes are encoded.
    pub color_space: RasterImageColorSpace,
    /// Encoded RGBA8 pixels, row-major, `width * height * 4` bytes.
    pub rgba: Arc<[u8]>,
}

impl RasterImage {
    /// Create a raster image. Returns `None` for invalid dimensions or byte
    /// lengths so callers cannot silently poison the renderer atlas.
    pub fn new(
        key: impl Into<String>,
        width: u32,
        height: u32,
        color_space: RasterImageColorSpace,
        rgba: impl Into<Arc<[u8]>>,
    ) -> Option<Self> {
        let rgba = rgba.into();
        let expected = width.checked_mul(height)?.checked_mul(4)? as usize;
        (width > 0 && height > 0 && rgba.len() == expected).then(|| Self {
            key: key.into(),
            width,
            height,
            color_space,
            rgba,
        })
    }
}
