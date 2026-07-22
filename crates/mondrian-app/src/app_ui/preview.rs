//! Final Window Adapter over the UI-independent production Preview Runtime.
//!
//! This shallow Adapter converts application presentation state into Widget
//! models and constructs the Window-owned external-texture payload. Scheduling,
//! media execution, caching, color decisions, and playback coordination remain
//! in `app::preview_runtime`.

use mondrian_ui_widgets::{
    ViewerExternalTextureFrame, ViewerExternalTexturePresentation, ViewerFrameContent,
    ViewerFrameImage,
};

use crate::app::preview_execution::PreviewGpuFrame;
use crate::app::preview_raster_frame::PreviewRasterColorSpace;
use crate::app::preview_runtime::{PreviewPresentationContent, PreviewPresentationState};
use crate::app::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use crate::app::AppState;
use crate::app_ui::panels::{
    ViewerColorPipelineStatus, ViewerPreviewColorRejectionModel, ViewerPreviewSource,
    ViewerPreviewState,
};

/// Window specialization of the production Preview Runtime.
pub type WindowPreviewAdapter =
    crate::app::preview_runtime::PreviewProductionRuntime<ViewerExternalTextureFrame>;

/// Window-owned publication seam for renderer-registered Viewer textures.
pub(crate) trait WindowPreviewOutputRegistration {
    /// Publish one exact spatial output after renderer registration succeeds.
    fn set_external_viewer_frame(
        &self,
        frame: &PreviewGpuFrame,
        texture_key: impl Into<String>,
        presentation: ViewerExternalTexturePresentation,
    ) -> bool;
}

impl WindowPreviewOutputRegistration for WindowPreviewAdapter {
    fn set_external_viewer_frame(
        &self,
        frame: &PreviewGpuFrame,
        texture_key: impl Into<String>,
        presentation: ViewerExternalTexturePresentation,
    ) -> bool {
        let Some(output) = ViewerExternalTextureFrame::new_spatial(texture_key, presentation)
        else {
            self.reject_gpu_output_registration();
            return false;
        };
        self.register_gpu_output(frame, output)
    }
}

impl ViewerPreviewSource for WindowPreviewAdapter {
    fn viewer_preview_for_state(&self, state: &AppState) -> ViewerPreviewState {
        match self.presentation_for_state(state) {
            PreviewPresentationState::Ready(content) => viewer_frame_content(content)
                .map(ViewerPreviewState::Ready)
                .unwrap_or_else(ViewerPreviewState::Unavailable),
            PreviewPresentationState::Transparent => ViewerPreviewState::Transparent,
            PreviewPresentationState::Loading => ViewerPreviewState::Loading,
            PreviewPresentationState::Stale(content) => viewer_frame_content(content)
                .map(ViewerPreviewState::Stale)
                .unwrap_or_else(ViewerPreviewState::Unavailable),
            PreviewPresentationState::Unavailable(reason) => {
                ViewerPreviewState::Unavailable(reason)
            }
        }
    }

    fn viewer_color_rejection(&self) -> Option<ViewerPreviewColorRejectionModel> {
        self.last_color_rejection().map(|rejection| ViewerPreviewColorRejectionModel {
            asset_id: rejection.asset_id,
            path: rejection.path,
            missing_metadata_policy: rejection.missing_metadata_policy,
            source: rejection.source,
            override_color_space: rejection.override_color_space,
            detected_color_space: rejection.detected_color_space,
            working_color_space: rejection.working_color_space,
            diagnostic_summary: rejection.diagnostic_summary,
            diagnostic_issue_summary: rejection.diagnostic_issue_summary,
        })
    }

    fn viewer_color_pipeline_status(&self) -> Option<ViewerColorPipelineStatus> {
        let diagnostics = self.diagnostics();
        let summary = diagnostics.composite_color_path_summary();
        if summary.composite_plans() == 0
            && diagnostics.cpu_output_fallback_frames == 0
            && diagnostics.preview_gpu_output_blocker_breakdown.total() == 0
        {
            return None;
        }
        if diagnostics.preview_gpu_output_blocker_breakdown.total() > 0 {
            return Some(ViewerColorPipelineStatus::GpuBlocked {
                gpu_blockers: diagnostics.preview_gpu_output_blocker_breakdown.total(),
            });
        }
        if diagnostics.color_stage_gpu_blockers > 0 {
            return Some(ViewerColorPipelineStatus::GpuBlocked {
                gpu_blockers: diagnostics.color_stage_gpu_blockers,
            });
        }
        if diagnostics.cpu_output_fallback_frames > 0 {
            return Some(ViewerColorPipelineStatus::LegacyRgba8 {
                legacy_reasons: diagnostics.cpu_output_fallback_frames,
            });
        }
        if summary.uses_legacy_rgba8() {
            return Some(ViewerColorPipelineStatus::LegacyRgba8 {
                legacy_reasons: summary.legacy_breakdown.total(),
            });
        }
        Some(ViewerColorPipelineStatus::FloatLinear)
    }
}

fn viewer_frame_content(
    content: PreviewPresentationContent<ViewerExternalTextureFrame>,
) -> Result<ViewerFrameContent, PreviewUnavailability> {
    match content {
        PreviewPresentationContent::Gpu(output) => Ok(ViewerFrameContent::ExternalTexture(output)),
        PreviewPresentationContent::Raster(frame) => {
            let color_space = match frame.color_space {
                PreviewRasterColorSpace::Srgb => mondrian_ui_core::RasterImageColorSpace::Srgb,
            };
            ViewerFrameImage::new(
                frame.resource_key,
                frame.width,
                frame.height,
                color_space,
                frame.rgba,
            )
            .map(ViewerFrameContent::Raster)
            .ok_or_else(|| {
                PreviewUnavailability::failed(
                    PreviewOutputStage::FramePackaging,
                    "validated Preview raster could not be adapted to a Window image",
                )
            })
        }
    }
}
