//! UI-independent Headless presentation over the production Preview Runtime.
//!
//! Performance and Golden validation Adapters share this coordinator so GPU
//! completion, output registration, Frame Presentation Ticket consumption, and
//! video preroll cannot acquire separate test-only meanings.

use std::time::Instant;

use anyhow::{ensure, Context};

use super::headless_viewer_gpu::{
    HeadlessViewerGpuAdapter, HeadlessViewerGpuError, HeadlessViewerGpuExecution,
    HeadlessViewerGpuOutput,
};
use super::playback_preview::observe_playback_video_preroll;
use super::preview_execution::PreviewGpuFrameState;
use super::preview_raster_frame::PreviewRasterFrame;
use super::preview_runtime::{
    PreviewPresentationContent, PreviewPresentationState, PreviewProductionRuntime,
};
use super::preview_unavailability::PreviewUnavailability;
use super::AppState;

/// Production Preview Runtime specialized for the no-Surface GPU Adapter.
pub(crate) type HeadlessPreviewRuntime = PreviewProductionRuntime<HeadlessViewerGpuOutput>;

/// Usable output made current by one bounded Headless presentation attempt.
#[derive(Debug)]
pub(crate) enum HeadlessPresentedOutput {
    /// A GPU output was executed or reused by the renderer Adapter.
    Gpu(Box<HeadlessViewerGpuExecution>),
    /// The Preview Runtime already retained the exact current GPU output.
    CurrentGpu,
    /// The exact current output is the semantic transparent canvas.
    Transparent,
    /// A validated final CPU raster made usable by the Headless consumer.
    Raster(PreviewRasterFrame),
}

/// Result of one bounded Headless presentation attempt.
#[derive(Debug)]
pub(crate) enum HeadlessPreviewCandidate {
    /// An exact current output is now usable.
    Ready {
        /// Concrete output path used for this presentation.
        output: HeadlessPresentedOutput,
        /// Whether this presentation consumed the current Playback demand.
        demand_completed: bool,
    },
    /// Required Preview work remains pending.
    Loading,
    /// The GPU Adapter could not accept more work in this bounded turn.
    Backpressured,
    /// The current intent cannot produce a valid output.
    Unavailable(PreviewUnavailability),
}

/// Execute one current Preview candidate through the real Headless GPU Adapter.
///
/// A successful demand completion is proven by consuming the exact pending
/// identity and incrementing Ready-delivery evidence. The App completion API
/// intentionally reports snapshot mutation, which can be false for a valid
/// paused seek, so that boolean is not used as acceptance evidence here.
pub(crate) fn present_headless_preview_candidate(
    preview: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu: &mut HeadlessViewerGpuAdapter,
) -> anyhow::Result<HeadlessPreviewCandidate> {
    match preview.gpu_preview_frame_for_state(state) {
        PreviewGpuFrameState::Ready(frame) => {
            let execution = match gpu.execute(&frame) {
                Ok(execution) => execution,
                Err(HeadlessViewerGpuError::Backpressure(_)) => {
                    return Ok(HeadlessPreviewCandidate::Backpressured);
                }
                Err(error) => return Err(error).context("execute Headless Viewer GPU candidate"),
            };
            ensure!(
                execution.gpu_completion_observed,
                "Headless Viewer GPU execution returned before GPU completion"
            );
            let output = execution.output.clone();
            preview.register_gpu_output(&frame, output);
            let demand_completed =
                complete_headless_presentation_ticket(state, frame.presentation_ticket())?;
            observe_playback_video_preroll(state, preview);
            Ok(HeadlessPreviewCandidate::Ready {
                output: HeadlessPresentedOutput::Gpu(Box::new(execution)),
                demand_completed,
            })
        }
        PreviewGpuFrameState::Current => {
            let demand_completed = complete_headless_presentation_ticket(
                state,
                preview.playback_presentation_ticket(state),
            )?;
            observe_playback_video_preroll(state, preview);
            Ok(HeadlessPreviewCandidate::Ready {
                output: HeadlessPresentedOutput::CurrentGpu,
                demand_completed,
            })
        }
        PreviewGpuFrameState::Transparent => {
            let demand_completed = complete_headless_presentation_ticket(
                state,
                preview.playback_presentation_ticket(state),
            )?;
            observe_playback_video_preroll(state, preview);
            Ok(HeadlessPreviewCandidate::Ready {
                output: HeadlessPresentedOutput::Transparent,
                demand_completed,
            })
        }
        PreviewGpuFrameState::Loading => Ok(HeadlessPreviewCandidate::Loading),
        PreviewGpuFrameState::Unavailable(reason) => {
            Ok(HeadlessPreviewCandidate::Unavailable(reason))
        }
    }
}

/// Arbitrate the complete production Headless presentation path.
///
/// GPU execution is attempted first. An explicit GPU blocker then falls
/// through to the same final CPU Raster path used by the Window Preview
/// Adapter; Loading and terminal unavailability retain their exact meanings.
pub(crate) fn present_headless_preview_output(
    preview: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu: &mut HeadlessViewerGpuAdapter,
) -> anyhow::Result<HeadlessPreviewCandidate> {
    match present_headless_preview_candidate(preview, state, gpu)? {
        HeadlessPreviewCandidate::Unavailable(_) => match preview.presentation_for_state(state) {
            PreviewPresentationState::Ready(PreviewPresentationContent::Raster(frame)) => {
                let demand_completed = complete_headless_presentation_ticket(
                    state,
                    preview.playback_presentation_ticket(state),
                )?;
                observe_playback_video_preroll(state, preview);
                Ok(HeadlessPreviewCandidate::Ready {
                    output: HeadlessPresentedOutput::Raster(frame),
                    demand_completed,
                })
            }
            PreviewPresentationState::Ready(PreviewPresentationContent::Gpu(_)) => {
                let demand_completed = complete_headless_presentation_ticket(
                    state,
                    preview.playback_presentation_ticket(state),
                )?;
                observe_playback_video_preroll(state, preview);
                Ok(HeadlessPreviewCandidate::Ready {
                    output: HeadlessPresentedOutput::CurrentGpu,
                    demand_completed,
                })
            }
            PreviewPresentationState::Transparent => {
                let demand_completed = complete_headless_presentation_ticket(
                    state,
                    preview.playback_presentation_ticket(state),
                )?;
                observe_playback_video_preroll(state, preview);
                Ok(HeadlessPreviewCandidate::Ready {
                    output: HeadlessPresentedOutput::Transparent,
                    demand_completed,
                })
            }
            PreviewPresentationState::Loading | PreviewPresentationState::Stale(_) => {
                Ok(HeadlessPreviewCandidate::Loading)
            }
            PreviewPresentationState::Unavailable(reason) => {
                Ok(HeadlessPreviewCandidate::Unavailable(reason))
            }
        },
        candidate => Ok(candidate),
    }
}

/// Consume one exact pending demand after the Headless output becomes usable.
pub(crate) fn complete_headless_presentation_ticket(
    state: &mut AppState,
    ticket: Option<mondrian_playback::FramePresentationTicket>,
) -> anyhow::Result<bool> {
    let Some(ticket) = ticket else {
        return Ok(false);
    };
    let identity = ticket.identity();
    ensure!(
        state.pending_playback_frame_demand_identity() == Some(identity),
        "Headless presentation ticket does not own the current Frame Demand"
    );
    let ready_before = state.playback_evidence_report().deliveries.ready;
    let _snapshot_changed = state.complete_frame_presentation(ticket, Instant::now());
    let ready_after = state.playback_evidence_report().deliveries.ready;
    ensure!(
        state.pending_playback_frame_demand_identity() != Some(identity)
            && ready_after.checked_sub(ready_before) == Some(1),
        "Headless presentation did not consume its exact Frame Demand as Ready"
    );
    Ok(true)
}
