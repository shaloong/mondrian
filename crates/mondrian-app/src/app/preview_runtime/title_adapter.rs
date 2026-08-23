//! Production Adapter between Timeline title demands and the background raster task.

use super::*;
use crate::app::preview_timeline_execution::{
    PreviewTimelineTitleFrame, PreviewTimelineTitleRequest,
};
use crate::app::preview_title_task::{
    PreviewTitleRasterFailure, PreviewTitleRasterOutcome, PreviewTitleRasterRequest,
};

impl<O: Clone> PreviewProductionRuntime<O> {
    pub(super) fn title_frame_for_plan(
        &self,
        request: PreviewTimelineTitleRequest,
    ) -> PreviewTimelineTitleFrame {
        let outcome = self.title_task.borrow_mut().resolve(PreviewTitleRasterRequest {
            title: request.title,
            author_resolution: request.author_resolution,
            title_safe_margin: request.title_safe_margin,
            sampled_resolution: request.target_resolution,
            working_color_space: request.working_color_space,
        });
        match outcome {
            PreviewTitleRasterOutcome::Ready(frame) => PreviewTimelineTitleFrame::Ready(frame),
            PreviewTitleRasterOutcome::Pending => {
                self.execution.borrow_mut().set_pending(true);
                PreviewTimelineTitleFrame::Pending
            }
            PreviewTitleRasterOutcome::Unavailable(failure) => {
                let reason = match failure {
                    PreviewTitleRasterFailure::Raster(error) => PreviewUnavailability::blocked(
                        PreviewOutputStage::GeneratedSource,
                        error.to_string(),
                    ),
                    PreviewTitleRasterFailure::Worker(detail) => {
                        PreviewUnavailability::failed(PreviewOutputStage::GeneratedSource, detail)
                    }
                };
                PreviewTimelineTitleFrame::Unavailable { reason }
            }
        }
    }
}
