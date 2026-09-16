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
use crate::app::headless_realtime_playback::HeadlessRealtimePlaybackSession;
use crate::app::headless_viewer_gpu::{
    HeadlessGpuCompletionDeadline, HeadlessViewerGpuAdapter, HeadlessViewerGpuAdapterInfo,
};
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

/// Raw closure retained independently of whether presentation succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct GoldenViewerShutdownEvidence {
    preview: crate::app::preview_runtime::PreviewRuntimeShutdownEvidence,
    gpu: crate::app::viewer_gpu_device_progress::ViewerGpuDeviceProgressShutdownEvidence,
    gpu_preview_dependency_barrier:
        crate::app::headless_viewer_gpu::HeadlessPreviewGpuDependencyBarrierEvidence,
}

impl GoldenViewerShutdownEvidence {
    fn all_resources_released(&self) -> bool {
        self.preview.all_workers_terminated()
            && self.gpu.qualifies_normal_runtime()
            && self.gpu_preview_dependency_barrier.is_complete()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{primary}; Golden Viewer closure qualified={qualified}", qualified = .shutdown.all_resources_released())]
pub(super) struct GoldenViewerClosedFailure {
    primary: anyhow::Error,
    pub(super) shutdown: GoldenViewerShutdownEvidence,
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
    session: HeadlessRealtimePlaybackSession,
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
    /// Keep the complete owner outside the operation's error/unwind scope.
    pub(super) fn run_with<T>(
        operation: impl FnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<(T, GoldenViewerShutdownEvidence)> {
        Self::run_with_deadline(None, operation)
    }

    /// Retain the caller's original deadline through startup, operation and closure.
    #[cfg(feature = "validation")]
    pub(super) fn run_with_until<T>(
        deadline: Instant,
        operation: impl FnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<(T, GoldenViewerShutdownEvidence)> {
        ensure!(
            Instant::now() < deadline,
            "Golden Viewer deadline elapsed before admission"
        );
        Self::run_with_deadline(Some(deadline), operation)
    }

    fn run_with_deadline<T>(
        deadline: Option<Instant>,
        operation: impl FnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<(T, GoldenViewerShutdownEvidence)> {
        let close_deadline = || {
            let local = Instant::now() + Duration::from_secs(30);
            deadline.map_or(local, |original| original.min(local))
        };
        let mut owner = Self::new(close_deadline())?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ensure!(
                deadline.is_none_or(|limit| Instant::now() < limit),
                "Golden Viewer startup exceeded original deadline"
            );
            let value = operation(&mut owner)?;
            ensure!(
                deadline.is_none_or(|limit| Instant::now() < limit),
                "Golden Viewer operation exceeded original deadline"
            );
            Ok(value)
        }))
        .unwrap_or_else(|payload| {
            Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
        });
        let shutdown = owner.shutdown_until(close_deadline());
        match result {
            Ok(value) if shutdown.all_resources_released() => Ok((value, shutdown)),
            result => Err(GoldenViewerClosedFailure {
                primary: result
                    .err()
                    .unwrap_or_else(|| anyhow::anyhow!("Golden Viewer ownership did not close")),
                shutdown,
            }
            .into()),
        }
    }

    fn shutdown_until(self, deadline: Instant) -> GoldenViewerShutdownEvidence {
        let shutdown = self.session.into_shutdown_owners().shutdown_until(deadline);
        GoldenViewerShutdownEvidence {
            preview: shutdown.preview,
            gpu: shutdown.gpu,
            gpu_preview_dependency_barrier: shutdown.gpu_preview_dependency_barrier,
        }
    }

    pub(super) fn new(deadline: Instant) -> anyhow::Result<Self> {
        Self::with_preview_factory(deadline, HeadlessPreviewRuntime::try_new)
    }

    fn with_preview_factory(
        deadline: Instant,
        create: impl FnOnce() -> Result<
            HeadlessPreviewRuntime,
            crate::app::preview_runtime::PreviewStartupFailure<
                crate::app::headless_viewer_gpu::HeadlessViewerGpuOutput,
            >,
        >,
    ) -> anyhow::Result<Self> {
        let runtime = std::panic::catch_unwind(std::panic::AssertUnwindSafe(create))
            .map_err(|payload| crate::app::headless_execution_startup::HeadlessExecutionStartFailure::preview_construction(
                crate::app::headless_execution_startup::startup_panic_diagnostic(payload),
            ).into_closed_error(deadline))?
            .map_err(|failure| crate::app::headless_execution_startup::HeadlessExecutionStartFailure::partial_preview(failure, None).into_closed_error(deadline))?;
        let gpu = match HeadlessViewerGpuAdapter::new() {
            Ok(gpu) => gpu,
            Err(failure) => return Err(failure.with_preview(runtime).into_closed_error(deadline)),
        };
        let session = HeadlessRealtimePlaybackSession::with_shutdown_owners(runtime, gpu)
            .map_err(|failure| failure.into_closed_error(deadline))?;
        Ok(Self {
            session,
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
        let deadline =
            Instant::now().checked_add(timeout).context("Golden Viewer deadline overflow")?;
        let (runtime, gpu) = self.session.bound_resources()?;
        let mut queued_demand_completed = false;
        loop {
            // Queue-ordered and still-in-flight candidates continue below;
            // every loop edge must consume the same original deadline.
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for Golden Viewer presentation; diagnostics: {:?}",
                runtime.diagnostics()
            );
            pump_playback_preview(state, runtime);
            match present_headless_preview_output(
                runtime,
                state,
                gpu,
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
                        && gpu.has_submission_in_flight()
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
                        runtime.diagnostics()
                    );
                }
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for Golden Viewer presentation; diagnostics: {:?}",
                runtime.diagnostics()
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    pub(super) fn evidence(&self) -> anyhow::Result<GoldenHeadlessViewerEvidence> {
        Ok(GoldenHeadlessViewerEvidence {
            adapter: self.session.gpu()?.adapter_info().clone(),
            presentations: self.presentations,
            gpu_executions: self.gpu_executions,
            cached_gpu_executions: self.cached_gpu_executions,
            current_gpu_presentations: self.current_gpu_presentations,
            cpu_raster_presentations: self.cpu_raster_presentations,
            reused_cpu_raster_presentations: self.reused_cpu_raster_presentations,
            completed_demands: self.completed_demands,
        })
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    #[ignore = "requires a real GPU; explicitly select for local ownership qualification"]
    fn golden_whole_viewer_operation_retains_closure_on_success_error_and_panic() {
        let (value, receipt) =
            GoldenHeadlessPreview::run_with(|_| Ok(42_u32)).expect("normal operation");
        assert_eq!(value, 42);
        assert!(receipt.all_resources_released(), "{receipt:?}");
        for panic_operation in [false, true] {
            let error = GoldenHeadlessPreview::run_with(|_| -> anyhow::Result<()> {
                if panic_operation {
                    panic!("Golden injected operation panic");
                }
                anyhow::bail!("Golden injected operation error")
            })
            .expect_err("operation must fail");
            let closed =
                error.downcast_ref::<GoldenViewerClosedFailure>().expect("raw closed failure");
            assert!(closed.shutdown.all_resources_released(), "{closed:?}");
            assert!(closed.primary.to_string().contains("Golden injected operation"));
        }
    }

    #[test]
    fn golden_partial_preview_startup_closes_before_attempting_gpu() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let error = match GoldenHeadlessPreview::with_preview_factory(deadline, || {
            HeadlessPreviewRuntime::try_start_with_checkpoint_for_test(0, |stage| {
                if stage == crate::app::preview_runtime::PreviewStartupCheckpoint::CpuFallback {
                    panic!("Golden partial Preview original failure");
                }
            })
        }) {
            Err(error) => error,
            Ok(preview) => {
                let _ = preview.shutdown_until(deadline);
                panic!("constructor checkpoint not reached");
            }
        };
        let closed = error
            .downcast_ref::<crate::app::headless_execution_startup::HeadlessStartupClosedFailure>()
            .expect("owner-free closed startup error");
        assert!(closed
            .diagnostic
            .to_string()
            .contains("Golden partial Preview original failure"));
        assert!(closed.evidence.preview.is_none());
        assert!(closed.evidence.preview_startup.is_some());
        assert_eq!(
            closed.evidence.gpu,
            crate::app::headless_execution_startup::HeadlessStartupGpuShutdownEvidence::NotStarted
        );
        assert!(
            closed.evidence.all_created_resources_released(),
            "{:?}",
            closed.evidence
        );
    }
}
