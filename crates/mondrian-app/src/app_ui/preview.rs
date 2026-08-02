//! Final Window Adapter over the UI-independent production Preview Runtime.
//!
//! This shallow Adapter converts application presentation state into Widget
//! models and constructs the Window-owned external-texture payload. Scheduling,
//! media execution, caching, color decisions, and playback coordination remain
//! in `app::preview_runtime`.

use mondrian_ui_widgets::{ViewerExternalTextureFrame, ViewerFrameContent, ViewerFrameImage};

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

/// Immutable Window projection of the last output admitted by presentation
/// authority.
///
/// Widget model refreshes consume this snapshot instead of reevaluating
/// Preview and attaching payload-free Ready feedback to a later demand.
pub(crate) struct WindowPreviewSnapshot<'a> {
    state: &'a ViewerPreviewState,
    adapter: &'a WindowPreviewAdapter,
}

impl<'a> WindowPreviewSnapshot<'a> {
    pub(crate) const fn new(
        state: &'a ViewerPreviewState,
        adapter: &'a WindowPreviewAdapter,
    ) -> Self {
        Self { state, adapter }
    }
}

impl ViewerPreviewSource for WindowPreviewSnapshot<'_> {
    fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
        self.state.clone()
    }

    fn viewer_color_rejection(&self) -> Option<ViewerPreviewColorRejectionModel> {
        <WindowPreviewAdapter as ViewerPreviewSource>::viewer_color_rejection(self.adapter)
    }

    fn viewer_color_pipeline_status(&self) -> Option<ViewerColorPipelineStatus> {
        <WindowPreviewAdapter as ViewerPreviewSource>::viewer_color_pipeline_status(self.adapter)
    }
}

impl ViewerPreviewSource for WindowPreviewAdapter {
    fn viewer_preview_for_state(&self, state: &AppState) -> ViewerPreviewState {
        match self.presentation(state.preview_frame_execution_request(std::time::Instant::now())) {
            PreviewPresentationState::Ready(candidate) => {
                { viewer_frame_content(candidate.into_value()) }
                    .map(ViewerPreviewState::Ready)
                    .unwrap_or_else(ViewerPreviewState::Unavailable)
            }
            PreviewPresentationState::Transparent(_) => ViewerPreviewState::Transparent,
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
            executable_color_space: rejection.executable_color_space,
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

pub(crate) fn viewer_frame_content(
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
