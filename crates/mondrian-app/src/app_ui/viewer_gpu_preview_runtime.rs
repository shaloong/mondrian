//! Stateful GPU resources for one Viewer preview execution context.
//!
//! Windowing is an Adapter concern. The resources below instead belong to the
//! Viewer GPU execution lifetime and must be shared by every production or
//! headless Adapter that executes the same preview path.

use super::native_video_import::AppUiNativeVideoImportRuntime;
use mondrian_renderer::{
    GpuDisplayCalibrationRuntime, GpuFrameCompositor, GpuNativeDecodedFrameImportSupport,
    GpuViewerSpatialRuntime, RenderGpuOutputBoundaryRuntime,
};
use mondrian_ui_renderer::ExternalTextureKey;
use mondrian_ui_widgets::ViewerExternalTexturePresentation;

/// Long-lived GPU state for a single Viewer preview execution context.
///
/// Frame resources are cleared between candidates; pipelines and backend
/// capabilities remain resident for the lifetime of this object. Fields are
/// temporarily visible to the sibling Window Adapter while execution is moved
/// behind this module's stable interface.
pub(crate) struct ViewerGpuPreviewRuntime {
    pub(super) native_video_import: AppUiNativeVideoImportRuntime,
    pub(super) color_output: RenderGpuOutputBoundaryRuntime,
    pub(super) spatial: GpuViewerSpatialRuntime,
    pub(super) display_calibration: GpuDisplayCalibrationRuntime,
    pub(super) working_compositor: GpuFrameCompositor,
    registered_texture_key: Option<ExternalTextureKey>,
    presentation: Option<ViewerExternalTexturePresentation>,
}

impl ViewerGpuPreviewRuntime {
    /// Create one execution context for a renderer device.
    pub(crate) fn new(adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            native_video_import: AppUiNativeVideoImportRuntime::new(adapter, device, queue),
            color_output: RenderGpuOutputBoundaryRuntime::default(),
            spatial: GpuViewerSpatialRuntime::default(),
            display_calibration: GpuDisplayCalibrationRuntime::default(),
            working_compositor: GpuFrameCompositor::new(device),
            registered_texture_key: None,
            presentation: None,
        }
    }

    /// Native decoder import capability exposed to preview scheduling.
    pub(crate) fn native_import_support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.native_video_import.support()
    }

    /// Release resources scoped to the current candidate, retaining pipelines.
    pub(crate) fn clear_frame_resources(&mut self) {
        self.color_output.clear_frame_resources();
        self.spatial.clear_frame_resources();
        self.display_calibration.clear_frame_resources();
    }

    /// Remove and return the texture registration owned by this context.
    ///
    /// The Adapter must unregister the returned key from its renderer before
    /// discarding or replacing the associated frame resources.
    pub(crate) fn take_registered_texture_key(&mut self) -> Option<ExternalTextureKey> {
        self.registered_texture_key.take()
    }

    /// Record the renderer registration owned by this execution context.
    pub(crate) fn set_registered_texture_key(&mut self, key: ExternalTextureKey) {
        self.registered_texture_key = Some(key);
    }

    /// Current spatial presentation identity, if one is active.
    pub(crate) fn presentation(&self) -> Option<ViewerExternalTexturePresentation> {
        self.presentation
    }

    /// Replace the active spatial presentation identity.
    pub(crate) fn set_presentation(&mut self, presentation: ViewerExternalTexturePresentation) {
        self.presentation = Some(presentation);
    }

    /// Clear and report whether a spatial presentation was active.
    pub(crate) fn take_presentation(&mut self) -> bool {
        self.presentation.take().is_some()
    }

    /// Reset all retained execution resources after a device/surface transition.
    pub(crate) fn reset(&mut self) {
        debug_assert!(
            self.registered_texture_key.is_none(),
            "renderer registration must be released before resetting Viewer GPU resources"
        );
        self.color_output.clear_frame_resources();
        self.spatial.clear();
        self.display_calibration.clear();
        self.registered_texture_key = None;
        self.presentation = None;
    }
}
