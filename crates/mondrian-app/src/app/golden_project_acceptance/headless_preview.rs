//! Real Headless Viewer presentation evidence for Golden workflow slices.

use std::collections::BTreeSet;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use serde::Serialize;

use crate::app::headless_preview_presentation::{
    present_headless_preview_output, HeadlessCompletedGpuDisposition, HeadlessPresentedOutput,
    HeadlessPreviewCandidate, HeadlessPreviewRuntime,
};
use crate::app::headless_viewer_gpu::{
    HeadlessGpuCompletionDeadline, HeadlessViewerGpuAdapter, HeadlessViewerGpuAdapterInfo,
};
use crate::app::native_video_import::resolve_playback_hardware_decode_admission;
use crate::app::playback_preview::pump_playback_preview;
use crate::app::AppState;

/// Exact result of one usable current-frame presentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct GoldenViewerPresentationEvidence {
    pub(super) output: GoldenViewerOutputKind,
    pub(super) output_reused: bool,
    pub(super) new_gpu_completion_observed: bool,
    pub(super) demand_completed: bool,
}

/// Concrete path that made the exact current Viewer output usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum GoldenViewerOutputKind {
    GpuExecution,
    CurrentGpu,
    CpuRaster,
}

/// Aggregate identity and counts for one Golden Headless Viewer session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct GoldenHeadlessViewerEvidence {
    pub(super) adapter: HeadlessViewerGpuAdapterInfo,
    pub(super) presentations: u64,
    pub(super) gpu_executions: u64,
    pub(super) cached_gpu_executions: u64,
    pub(super) current_gpu_presentations: u64,
    pub(super) cpu_raster_presentations: u64,
    pub(super) reused_cpu_raster_presentations: u64,
    pub(super) completed_demands: u64,
}

/// Golden Adapter that owns one production Preview Runtime and real GPU device.
pub(super) struct GoldenHeadlessPreview {
    runtime: HeadlessPreviewRuntime,
    gpu: HeadlessViewerGpuAdapter,
    presentations: u64,
    gpu_executions: u64,
    cached_gpu_executions: u64,
    current_gpu_presentations: u64,
    cpu_raster_presentations: u64,
    reused_cpu_raster_presentations: u64,
    raster_resource_keys: BTreeSet<String>,
    completed_demands: u64,
}

impl GoldenHeadlessPreview {
    pub(super) fn new() -> anyhow::Result<Self> {
        let runtime = HeadlessPreviewRuntime::new();
        let mut gpu =
            HeadlessViewerGpuAdapter::new().context("create Golden Headless Viewer GPU Adapter")?;
        gpu.install_completion_waker(runtime.work_watch().completion_waker());
        let hardware_admission =
            resolve_playback_hardware_decode_admission(&gpu.native_import_support());
        runtime
            .set_renderer_hardware_decode_admission(
                hardware_admission,
                gpu.native_decode_device_root(),
            )
            .context("install Golden renderer-qualified decoder device")?;
        Ok(Self {
            runtime,
            gpu,
            presentations: 0,
            gpu_executions: 0,
            cached_gpu_executions: 0,
            current_gpu_presentations: 0,
            cpu_raster_presentations: 0,
            reused_cpu_raster_presentations: 0,
            raster_resource_keys: BTreeSet::new(),
            completed_demands: 0,
        })
    }

    /// Make the exact current nontransparent Viewer output usable.
    pub(super) fn present_current(
        &mut self,
        state: &mut AppState,
        timeout: Duration,
    ) -> anyhow::Result<GoldenViewerPresentationEvidence> {
        let deadline = Instant::now() + timeout;
        let mut queued_demand_completed = false;
        loop {
            pump_playback_preview(state, &self.runtime);
            match present_headless_preview_output(
                &self.runtime,
                state,
                &mut self.gpu,
                HeadlessGpuCompletionDeadline::at(deadline),
            )? {
                HeadlessPreviewCandidate::Ready { output, completed_demand } => {
                    let demand_completed = completed_demand.is_some();
                    if matches!(&output, HeadlessPresentedOutput::QueuedGpu) {
                        ensure!(
                            demand_completed,
                            "Golden queue-ordered Viewer publication did not complete the current Frame Demand"
                        );
                        queued_demand_completed = true;
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    if matches!(&output, HeadlessPresentedOutput::CurrentGpu)
                        && self.gpu.has_submission_in_flight()
                    {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    let demand_completed = demand_completed || queued_demand_completed;
                    ensure!(
                        demand_completed,
                        "Golden Viewer output did not complete the current Frame Demand"
                    );
                    let evidence = match output {
                        HeadlessPresentedOutput::Gpu { execution } => {
                            self.gpu_executions = self.gpu_executions.saturating_add(1);
                            GoldenViewerPresentationEvidence {
                                output: GoldenViewerOutputKind::GpuExecution,
                                output_reused: false,
                                new_gpu_completion_observed: execution.gpu_completion_observed,
                                demand_completed,
                            }
                        }
                        HeadlessPresentedOutput::QueuedGpu => {
                            unreachable!("queue publication is drained before Golden evidence")
                        }
                        HeadlessPresentedOutput::CurrentGpu => {
                            self.current_gpu_presentations =
                                self.current_gpu_presentations.saturating_add(1);
                            GoldenViewerPresentationEvidence {
                                output: GoldenViewerOutputKind::CurrentGpu,
                                output_reused: true,
                                new_gpu_completion_observed: false,
                                demand_completed,
                            }
                        }
                        HeadlessPresentedOutput::Raster(frame) => {
                            ensure!(
                                frame.width > 0 && frame.height > 0 && !frame.rgba.is_empty(),
                                "Golden Viewer CPU Raster is empty"
                            );
                            self.cpu_raster_presentations =
                                self.cpu_raster_presentations.saturating_add(1);
                            let output_reused =
                                !self.raster_resource_keys.insert(frame.resource_key.clone());
                            if output_reused {
                                self.reused_cpu_raster_presentations =
                                    self.reused_cpu_raster_presentations.saturating_add(1);
                            }
                            GoldenViewerPresentationEvidence {
                                output: GoldenViewerOutputKind::CpuRaster,
                                output_reused,
                                new_gpu_completion_observed: false,
                                demand_completed,
                            }
                        }
                        HeadlessPresentedOutput::Transparent => {
                            bail!(
                                "Golden Editorial frame resolved to transparent instead of authored Hero content"
                            );
                        }
                    };
                    self.presentations = self.presentations.saturating_add(1);
                    self.completed_demands = self.completed_demands.saturating_add(1);
                    return Ok(evidence);
                }
                HeadlessPreviewCandidate::CompletedGpu { execution, disposition } => {
                    ensure!(
                        execution.gpu_completion_observed,
                        "Golden Viewer received a completed GPU disposition without exact completion evidence"
                    );
                    match disposition {
                        HeadlessCompletedGpuDisposition::PublishedCurrent { completed_demand } => {
                            queued_demand_completed |= completed_demand.is_some();
                        }
                        HeadlessCompletedGpuDisposition::PreparedSuccessor => {}
                        HeadlessCompletedGpuDisposition::Released => {}
                        HeadlessCompletedGpuDisposition::TerminalDelivery(kind) => {
                            ensure!(
                                !matches!(
                                    kind,
                                    mondrian_playback::FrameDeliveryKind::Ready
                                        | mondrian_playback::FrameDeliveryKind::Degraded
                                ),
                                "Golden Viewer classified presentable GPU delivery as terminal"
                            );
                        }
                    }
                }
                HeadlessPreviewCandidate::Loading
                | HeadlessPreviewCandidate::Backpressured
                | HeadlessPreviewCandidate::DroppedLate => {}
                HeadlessPreviewCandidate::Unavailable(reason) => {
                    bail!(
                        "Golden Viewer cannot present the current Hero frame: {reason:?}; diagnostics: {:?}",
                        self.runtime.diagnostics()
                    );
                }
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for Golden Viewer presentation; diagnostics: {:?}",
                self.runtime.diagnostics()
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    pub(super) fn evidence(&self) -> GoldenHeadlessViewerEvidence {
        GoldenHeadlessViewerEvidence {
            adapter: self.gpu.adapter_info().clone(),
            presentations: self.presentations,
            gpu_executions: self.gpu_executions,
            cached_gpu_executions: self.cached_gpu_executions,
            current_gpu_presentations: self.current_gpu_presentations,
            cpu_raster_presentations: self.cpu_raster_presentations,
            reused_cpu_raster_presentations: self.reused_cpu_raster_presentations,
            completed_demands: self.completed_demands,
        }
    }
}
