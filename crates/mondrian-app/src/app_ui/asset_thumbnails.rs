//! Shallow Window Adapter for UI-independent asset-thumbnail execution.

use std::sync::Arc;

use mondrian_assets::AssetRecord;
use mondrian_timeline::sequence::ProgramColorContext;
use mondrian_ui_core::RasterImageColorSpace;
use mondrian_ui_widgets::RasterImage;

use crate::app::thumbnail_service::{
    AssetThumbnailService, ThumbnailDiagnostics, ThumbnailFailure, ThumbnailFailureReason,
    ThumbnailLookupState, ThumbnailRasterColorSpace,
};
use crate::app_ui::panels::{AssetThumbnailSource, AssetThumbnailState};

/// Window-facing Adapter over the production thumbnail service.
pub struct AssetThumbnailAdapter {
    service: Arc<AssetThumbnailService>,
}

impl AssetThumbnailAdapter {
    /// Create a Window Adapter with one production service instance.
    pub fn new() -> Self {
        Self { service: AssetThumbnailService::new() }
    }

    /// Forward the Project-owned engine plus Project future-Sequence defaults.
    pub fn set_color_context(&self, context: Option<ProgramColorContext>) {
        self.service.set_color_context(context);
    }

    /// Pump a bounded number of service completions.
    pub fn poll_finished(&self) -> bool {
        self.service.poll_finished()
    }

    /// Snapshot UI-independent execution evidence.
    pub fn diagnostics(&self) -> ThumbnailDiagnostics {
        self.service.diagnostics()
    }
}

impl Default for AssetThumbnailAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetThumbnailSource for AssetThumbnailAdapter {
    fn thumbnail_for_asset(&self, asset: &AssetRecord) -> AssetThumbnailState {
        match self.service.thumbnail_for_asset(asset) {
            ThumbnailLookupState::Unavailable => AssetThumbnailState::Unavailable,
            ThumbnailLookupState::Loading => AssetThumbnailState::Loading,
            ThumbnailLookupState::Failed(failure) => AssetThumbnailState::Failed(failure),
            ThumbnailLookupState::Ready(frame) => {
                let color_space = match frame.color_space {
                    ThumbnailRasterColorSpace::Srgb => RasterImageColorSpace::Srgb,
                };
                match RasterImage::new(
                    frame.resource_key,
                    frame.width,
                    frame.height,
                    color_space,
                    frame.rgba,
                ) {
                    Some(image) => AssetThumbnailState::Ready(image),
                    None => AssetThumbnailState::Failed(ThumbnailFailure::new(
                        ThumbnailFailureReason::InvalidRasterPayload,
                        "production thumbnail frame failed Window raster validation",
                    )),
                }
            }
        }
    }
}
