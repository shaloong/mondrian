//! Performance execution ownership, closure and raw evidence projection.
//!
//! One Module owns factory admission, panic capture, producer-first shutdown,
//! one absolute deadline and immutable complete receipts. Test measurement code
//! cannot manufacture closure or discard an owning startup failure.

use super::headless_preview_presentation::HeadlessPreviewRuntime;
#[cfg(test)]
use super::headless_realtime_playback::HeadlessRealtimePlaybackSession;
use super::headless_viewer_gpu::HeadlessViewerGpuAdapter;
use super::preview_runtime::{
    PreviewOwnedWorkerShutdown, PreviewProductionRuntime, PreviewRuntimeShutdownEvidence,
};
use super::AppState;
#[cfg(test)]
use anyhow::Context;
#[cfg(test)]
use mondrian_render_cache::TimelineRenderCacheDiagnostics;
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};
#[derive(Debug, Clone, Serialize)]
pub(super) struct PerfWorkerClosureReport {
    pub(super) domain: &'static str,
    pub(super) requested_workers: u32,
    pub(super) startup_attempted: bool,
    pub(super) started_workers: u32,
    pub(super) terminated_workers: u32,
    pub(super) panicked_workers: u32,
    pub(super) timed_out_workers: u32,
    pub(super) detached_workers: u32,
    pub(super) unexpected_worker_exits: u32,
    pub(super) queued_work_remaining: u64,
    pub(super) running_work_remaining: u64,
    pub(super) owned_resources_remaining: u64,
    pub(super) cumulative_failures: u64,
    pub(super) lifecycle_closed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PerfProjectClosureReport {
    pub(super) session_was_open: bool,
    pub(super) pending_close_was_active: bool,
    pub(super) authoring_session_released: bool,
    pub(super) pending_close_released: bool,
    pub(super) runtime_lease_released: bool,
    pub(super) retired_library_generations_remaining: u32,
    pub(super) lifecycle_failure: Option<String>,
    pub(super) all_resources_released: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct PerfAudioOutputClosureReport {
    pub(super) schema_version: u32,
    pub(super) workers_started: u32,
    pub(super) workers_terminated: u32,
    pub(super) worker_start_failures: u32,
    pub(super) worker_spawner_panics: u32,
    pub(super) worker_panics: u32,
    pub(super) current_thread_detachments: u32,
    pub(super) worker_owner_abandonments: u32,
    pub(super) worker_terminal_evidence_missing: u32,
    pub(super) all_workers_terminated: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct PerfAudioClosureReport {
    pub(super) schema_version: u32,
    pub(super) render_workers_started: u32,
    pub(super) render_workers_terminated: u32,
    pub(super) render_worker_panics: u32,
    pub(super) render_worker_owner_abandonments: u32,
    pub(super) render_worker_terminal_evidence_missing: u32,
    pub(super) render_current_thread_detachments: u32,
    pub(super) render_retirement_workers_started: u32,
    pub(super) render_retirement_workers_terminated: u32,
    pub(super) render_retirement_worker_panics: u32,
    pub(super) render_retirement_worker_owner_abandonments: u32,
    pub(super) render_retirement_worker_terminal_evidence_missing: u32,
    pub(super) render_retirement_current_thread_detachments: u32,
    pub(super) output: PerfAudioOutputClosureReport,
    pub(super) foreign_owner_panics: u32,
    pub(super) foreign_owner_abandonments: u32,
    pub(super) shutdown_coordinators_started: u32,
    pub(super) shutdown_coordinators_terminated: u32,
    pub(super) shutdown_coordinator_start_failures: u32,
    pub(super) shutdown_coordinator_panics: u32,
    pub(super) shutdown_coordinator_timeouts: u32,
    pub(super) shutdown_coordinator_detachments: u32,
    pub(super) shutdown_coordinator_spawner_panics: u32,
    pub(super) shutdown_coordinator_owner_abandonments: u32,
    pub(super) shutdown_resource_facts_complete_at_deadline: bool,
    pub(super) shutdown_owner_lifetime_unresolved_at_deadline: bool,
    pub(super) all_workers_terminated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PerfAudioSourceClosureReport {
    pub(super) strong_references_before_consumption: u32,
    pub(super) strong_references_remaining: u32,
    pub(super) cache: Option<mondrian_media::AudioSourceCacheShutdownEvidence>,
    pub(super) all_resources_released: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PerfAppClosureReport {
    pub(super) app_owner_consumed: bool,
    pub(super) project: PerfProjectClosureReport,
    pub(super) reference_output: mondrian_reference_output::ReferenceOutputModuleShutdownReceipt,
    pub(super) reference_output_resources_released: bool,
    pub(super) export: mondrian_export::ExportQueueShutdownEvidence,
    pub(super) export_terminal_snapshot: mondrian_export::ExportEnduranceSnapshot,
    pub(super) export_resources_released: bool,
    pub(super) audio: PerfAudioClosureReport,
    pub(super) audio_source_cache: PerfAudioSourceClosureReport,
    pub(super) workers: Vec<PerfWorkerClosureReport>,
    pub(super) all_resources_released: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct PerfRenderCacheWorkerClosureReport {
    pub(super) worker_started: bool,
    pub(super) worker_terminated: bool,
    pub(super) worker_panicked: bool,
    pub(super) current_thread_skipped: bool,
    pub(super) timed_out: bool,
    pub(super) detached: bool,
    pub(super) all_workers_terminated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PerfPreviewClosureReport {
    pub(super) work_callbacks: Option<super::preview_runtime::PreviewWorkCallbackEvidence>,
    pub(super) visual_dependency_worker: Option<&'static str>,
    pub(super) owner_slot: usize,
    pub(super) schema_version: u32,
    pub(super) workers_started: u32,
    pub(super) workers_terminated: u32,
    pub(super) worker_panics: u32,
    pub(super) worker_panic_payloads_abandoned: u32,
    pub(super) current_thread_detachments: u32,
    pub(super) unverified_async_reaps: u32,
    pub(super) worker_timeouts: u32,
    pub(super) worker_deadline_detachments: u32,
    pub(super) render_cache_schema_version: u32,
    pub(super) render_cache_required: bool,
    pub(super) render_cache_start_failed: bool,
    pub(super) render_cache_worker: Option<PerfRenderCacheWorkerClosureReport>,
    pub(super) render_cache_aggregate_outcome: &'static str,
    pub(super) all_resources_released: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PerfGpuClosureReport {
    pub(super) worker_shutdown: super::owned_worker_lifecycle::OwnedWorkerShutdown,
    pub(super) wake_callbacks: super::preview_work_notification::PreviewWorkCallbackEvidence,
    pub(super) native_wake_failures: u64,
    pub(super) wake_registration_rejections: u64,
    pub(super) renderer_retirement: Option<mondrian_renderer::ViewerGpuRetirementReceipt>,
    pub(super) worker_started: bool,
    pub(super) worker_terminated: bool,
    pub(super) worker_panicked: bool,
    pub(super) timed_out: bool,
    pub(super) retirement_requested: bool,
    pub(super) retirement_handoff_accepted: bool,
    pub(super) retirement_completed: bool,
    pub(super) generation_terminal_kind: Option<&'static str>,
    pub(super) all_resources_released: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PerfOwnerClosureReport {
    pub(super) schema_version: u32,
    pub(super) shared_deadline_budget_ms: u64,
    pub(super) preview_owner_count: usize,
    pub(super) gpu_owner_required: bool,
    pub(super) previews: Vec<PerfPreviewClosureReport>,
    pub(super) gpu: Option<PerfGpuClosureReport>,
    pub(super) gpu_preview_dependency_barrier:
        Option<super::headless_viewer_gpu::HeadlessPreviewGpuDependencyBarrierEvidence>,
    pub(super) app: PerfAppClosureReport,
    pub(super) all_resources_released: bool,
}

pub(super) fn perf_shutdown_budget_ms() -> u64 {
    std::env::var("MONDRIAN_PERF_SHUTDOWN_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(100, 120_000)
}

impl PerfWorkerClosureReport {
    pub(super) fn from_evidence(
        domain: &'static str,
        evidence: super::endurance_shutdown::EnduranceWorkerShutdownEvidence,
    ) -> Self {
        Self {
            domain,
            requested_workers: evidence.requested_workers,
            startup_attempted: evidence.startup_attempted,
            started_workers: evidence.started_workers,
            terminated_workers: evidence.terminated_workers,
            panicked_workers: evidence.panicked_workers,
            timed_out_workers: evidence.timed_out_workers,
            detached_workers: evidence.detached_workers,
            unexpected_worker_exits: evidence.unexpected_worker_exits,
            queued_work_remaining: evidence.queued_work_remaining,
            running_work_remaining: evidence.running_work_remaining,
            owned_resources_remaining: evidence.owned_resources_remaining,
            cumulative_failures: evidence.cumulative_failures,
            lifecycle_closed: evidence.lifecycle_closed(),
        }
    }
}

impl PerfAppClosureReport {
    pub(super) fn from_evidence(
        evidence: &super::endurance_shutdown::AppEnduranceShutdownEvidence,
    ) -> Self {
        let workers = vec![
            PerfWorkerClosureReport::from_evidence(
                "execution_memory_observer",
                evidence.execution_memory_observer,
            ),
            PerfWorkerClosureReport::from_evidence(
                "project_persistence",
                evidence.project_persistence,
            ),
            PerfWorkerClosureReport::from_evidence("audio_idle_warmup", evidence.audio_idle_warmup),
            PerfWorkerClosureReport::from_evidence("media_import", evidence.media_import),
            PerfWorkerClosureReport::from_evidence(
                "media_asset_mutation",
                evidence.media_asset_mutation,
            ),
            PerfWorkerClosureReport::from_evidence("visual_tracking", evidence.visual_tracking),
            PerfWorkerClosureReport::from_evidence("proxy_generation", evidence.proxy_generation),
        ];
        let project = PerfProjectClosureReport {
            session_was_open: evidence.project.session_was_open,
            pending_close_was_active: evidence.project.pending_close_was_active,
            authoring_session_released: evidence.project.authoring_session_released,
            pending_close_released: evidence.project.pending_close_released,
            runtime_lease_released: evidence.project.runtime_lease_released,
            retired_library_generations_remaining: evidence
                .project
                .retired_library_generations_remaining,
            lifecycle_failure: evidence.project.lifecycle_failure.clone(),
            all_resources_released: evidence.project.all_resources_released(),
        };
        let audio_output = evidence.audio.output;
        let audio = PerfAudioClosureReport {
            schema_version: evidence.audio.schema_version,
            render_workers_started: evidence.audio.render_workers_started,
            render_workers_terminated: evidence.audio.render_workers_terminated,
            render_worker_panics: evidence.audio.render_worker_panics,
            render_worker_owner_abandonments: evidence.audio.render_worker_owner_abandonments,
            render_worker_terminal_evidence_missing: evidence
                .audio
                .render_worker_terminal_evidence_missing,
            render_current_thread_detachments: evidence.audio.render_current_thread_detachments,
            render_retirement_workers_started: evidence.audio.render_retirement_workers_started,
            render_retirement_workers_terminated: evidence
                .audio
                .render_retirement_workers_terminated,
            render_retirement_worker_panics: evidence.audio.render_retirement_worker_panics,
            render_retirement_worker_owner_abandonments: evidence
                .audio
                .render_retirement_worker_owner_abandonments,
            render_retirement_worker_terminal_evidence_missing: evidence
                .audio
                .render_retirement_worker_terminal_evidence_missing,
            render_retirement_current_thread_detachments: evidence
                .audio
                .render_retirement_current_thread_detachments,
            output: PerfAudioOutputClosureReport {
                schema_version: audio_output.schema_version,
                workers_started: audio_output.workers_started,
                workers_terminated: audio_output.workers_terminated,
                worker_start_failures: audio_output.worker_start_failures,
                worker_spawner_panics: audio_output.worker_spawner_panics,
                worker_panics: audio_output.worker_panics,
                current_thread_detachments: audio_output.current_thread_detachments,
                worker_owner_abandonments: audio_output.worker_owner_abandonments,
                worker_terminal_evidence_missing: audio_output.worker_terminal_evidence_missing,
                all_workers_terminated: audio_output.all_workers_terminated(),
            },
            foreign_owner_panics: evidence.audio.foreign_owner_panics,
            foreign_owner_abandonments: evidence.audio.foreign_owner_abandonments,
            shutdown_coordinators_started: evidence.audio.shutdown_coordinators_started,
            shutdown_coordinators_terminated: evidence.audio.shutdown_coordinators_terminated,
            shutdown_coordinator_start_failures: evidence.audio.shutdown_coordinator_start_failures,
            shutdown_coordinator_panics: evidence.audio.shutdown_coordinator_panics,
            shutdown_coordinator_timeouts: evidence.audio.shutdown_coordinator_timeouts,
            shutdown_coordinator_detachments: evidence.audio.shutdown_coordinator_detachments,
            shutdown_coordinator_spawner_panics: evidence.audio.shutdown_coordinator_spawner_panics,
            shutdown_coordinator_owner_abandonments: evidence
                .audio
                .shutdown_coordinator_owner_abandonments,
            shutdown_resource_facts_complete_at_deadline: evidence
                .audio
                .shutdown_resource_facts_complete_at_deadline,
            shutdown_owner_lifetime_unresolved_at_deadline: evidence
                .audio
                .shutdown_owner_lifetime_unresolved_at_deadline,
            all_workers_terminated: evidence.audio.all_workers_terminated(),
        };
        let audio_source_cache = PerfAudioSourceClosureReport {
            strong_references_before_consumption: evidence
                .audio_source_cache
                .strong_references_before_consumption,
            strong_references_remaining: evidence.audio_source_cache.strong_references_remaining,
            cache: evidence.audio_source_cache.cache,
            all_resources_released: evidence.audio_source_cache.all_resources_released(),
        };
        Self {
            app_owner_consumed: evidence.app_owner_consumed,
            project,
            reference_output: evidence.reference_output.clone(),
            reference_output_resources_released: evidence.reference_output.all_resources_released(),
            export: evidence.export,
            export_terminal_snapshot: evidence.export_terminal_snapshot,
            export_resources_released: evidence.export.all_resources_released(),
            audio,
            audio_source_cache,
            workers,
            all_resources_released: evidence.all_resources_released(),
        }
    }
}

pub(super) fn preview_owned_worker_outcome_label(
    outcome: PreviewOwnedWorkerShutdown,
) -> &'static str {
    match outcome {
        PreviewOwnedWorkerShutdown::NotStarted => "not_started",
        PreviewOwnedWorkerShutdown::Terminated => "terminated",
        PreviewOwnedWorkerShutdown::Panicked => "panicked",
        PreviewOwnedWorkerShutdown::PanickedPayloadAbandoned => "panicked_payload_abandoned",
        PreviewOwnedWorkerShutdown::CurrentThreadSkipped => "current_thread_skipped",
        PreviewOwnedWorkerShutdown::TimedOutDetached => "timed_out_detached",
    }
}

impl PerfPreviewClosureReport {
    pub(super) fn from_evidence(
        owner_slot: usize,
        evidence: PreviewRuntimeShutdownEvidence,
    ) -> Self {
        let render_cache = evidence.timeline_render_cache;
        Self {
            work_callbacks: evidence.work_callbacks,
            visual_dependency_worker: evidence
                .visual_dependency_worker
                .map(preview_owned_worker_outcome_label),
            owner_slot,
            schema_version: evidence.schema_version,
            workers_started: evidence.workers_started,
            workers_terminated: evidence.workers_terminated,
            worker_panics: evidence.worker_panics,
            worker_panic_payloads_abandoned: evidence.worker_panic_payloads_abandoned,
            current_thread_detachments: evidence.current_thread_detachments,
            unverified_async_reaps: evidence.unverified_async_reaps,
            worker_timeouts: evidence.worker_timeouts,
            worker_deadline_detachments: evidence.worker_deadline_detachments,
            render_cache_schema_version: render_cache.schema_version,
            render_cache_required: render_cache.required,
            render_cache_start_failed: render_cache.start_failed,
            render_cache_worker: render_cache.worker.map(|worker| {
                PerfRenderCacheWorkerClosureReport {
                    worker_started: worker.worker_started,
                    worker_terminated: worker.worker_terminated,
                    worker_panicked: worker.worker_panicked,
                    current_thread_skipped: worker.current_thread_skipped,
                    timed_out: worker.timed_out,
                    detached: worker.detached,
                    all_workers_terminated: worker.all_workers_terminated(),
                }
            }),
            render_cache_aggregate_outcome: preview_owned_worker_outcome_label(
                render_cache.aggregate_outcome,
            ),
            all_resources_released: evidence.all_workers_terminated(),
        }
    }
}

impl PerfGpuClosureReport {
    pub(super) fn from_evidence(
        evidence: super::viewer_gpu_device_progress::ViewerGpuDeviceProgressShutdownEvidence,
    ) -> Self {
        let generation_terminal_kind = evidence.generation_terminal_kind.map(|kind| match kind {
            super::viewer_gpu_device_progress::ViewerGpuDeviceGenerationTerminalKind::DeviceLost => {
                "device_lost"
            }
            super::viewer_gpu_device_progress::ViewerGpuDeviceGenerationTerminalKind::DeviceDestroyed => {
                "device_destroyed"
            }
            super::viewer_gpu_device_progress::ViewerGpuDeviceGenerationTerminalKind::ProgressFailure => {
                "progress_failure"
            }
        });
        let all_resources_released = evidence.qualifies_normal_runtime();
        Self {
            worker_shutdown: evidence.worker_shutdown,
            wake_callbacks: evidence.wake_callbacks,
            native_wake_failures: evidence.native_wake_failures,
            wake_registration_rejections: evidence.wake_registration_rejections,
            worker_started: evidence.worker_started,
            worker_terminated: evidence.worker_terminated,
            worker_panicked: evidence.worker_panicked,
            timed_out: evidence.timed_out,
            retirement_requested: evidence.retirement_requested,
            retirement_handoff_accepted: evidence.retirement_handoff_accepted,
            retirement_completed: evidence.retirement_completed,
            renderer_retirement: evidence.renderer_retirement,
            generation_terminal_kind,
            all_resources_released,
        }
    }
}

impl PerfOwnerClosureReport {
    pub(super) fn from_evidence(
        shared_deadline_budget_ms: u64,
        gpu_owner_required: bool,
        preview_evidence: Vec<PreviewRuntimeShutdownEvidence>,
        gpu_evidence: Option<
            super::viewer_gpu_device_progress::ViewerGpuDeviceProgressShutdownEvidence,
        >,
        gpu_preview_dependency_barrier: Option<
            super::headless_viewer_gpu::HeadlessPreviewGpuDependencyBarrierEvidence,
        >,
        app_evidence: super::endurance_shutdown::AppEnduranceShutdownEvidence,
    ) -> Self {
        let preview_owner_count = preview_evidence.len();
        let previews = preview_evidence
            .into_iter()
            .enumerate()
            .map(|(slot, evidence)| PerfPreviewClosureReport::from_evidence(slot, evidence))
            .collect::<Vec<_>>();
        let gpu = gpu_evidence.map(PerfGpuClosureReport::from_evidence);
        let app = PerfAppClosureReport::from_evidence(&app_evidence);
        let all_resources_released = previews.iter().all(|preview| preview.all_resources_released)
            && if gpu_owner_required {
                gpu.as_ref().is_some_and(|gpu| gpu.all_resources_released)
                    && gpu_preview_dependency_barrier
                        .as_ref()
                        .is_some_and(|barrier| barrier.is_complete())
            } else {
                gpu.is_none() && gpu_preview_dependency_barrier.is_none()
            }
            && app.all_resources_released;
        Self {
            schema_version: 2,
            shared_deadline_budget_ms,
            preview_owner_count,
            gpu_owner_required,
            previews,
            gpu,
            gpu_preview_dependency_barrier,
            app,
            all_resources_released,
        }
    }

    pub(super) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.schema_version == 2,
            "unknown performance owner-closure schema"
        );
        anyhow::ensure!(
            self.preview_owner_count == self.previews.len(),
            "performance owner-closure Preview inventory changed"
        );
        anyhow::ensure!(
            self.previews
                .iter()
                .enumerate()
                .all(|(slot, preview)| preview.owner_slot == slot),
            "performance owner-closure Preview slots changed"
        );
        anyhow::ensure!(
            self.gpu_owner_required == self.gpu.is_some(),
            "performance owner-closure GPU inventory changed"
        );
        anyhow::ensure!(
            self.all_resources_released
                && self.previews.iter().all(|preview| {
                    preview.all_resources_released
                        && preview.schema_version == 4
                        && preview.work_callbacks.is_some_and(|callbacks| {
                            callbacks.all_resources_released()
                                && preview.workers_started
                                    >= 2 + u32::from(callbacks.worker_started)
                        })
                        && preview.workers_started == preview.workers_terminated
                        && preview.visual_dependency_worker == Some("terminated")
                        && preview.render_cache_required
                        && !preview.render_cache_start_failed
                        && preview
                            .render_cache_worker
                            .is_some_and(|worker| worker.all_workers_terminated)
                })
                && self.gpu.as_ref().is_none_or(|gpu| gpu.all_resources_released)
                && self
                    .gpu_preview_dependency_barrier
                    .as_ref()
                    .is_none_or(|barrier| barrier.is_complete())
                && self.app.all_resources_released,
            "performance owner-closure did not release every Preview/GPU/App resource: {self:?}"
        );
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn install_perf_timeline_render_cache<O: Clone>(
    preview: &PreviewProductionRuntime<O>,
    root: &Path,
) -> anyhow::Result<()> {
    const DISK_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
    const ARTIFACT_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
    const QUEUE_CAPACITY: usize = 4;
    let config = mondrian_render_cache::TimelineRenderCacheConfig::new(
        root.to_path_buf(),
        DISK_BUDGET_BYTES,
        ARTIFACT_BUDGET_BYTES,
        QUEUE_CAPACITY,
    )?;
    preview.install_timeline_render_cache_for_test(config).with_context(|| {
        format!(
            "start isolated performance Timeline render cache {}",
            root.display()
        )
    })
}

#[cfg(test)]
pub(super) fn persistent_cache_request_verified(
    before: TimelineRenderCacheDiagnostics,
    after: TimelineRenderCacheDiagnostics,
) -> bool {
    after.lookup_submissions > before.lookup_submissions && after.hits > before.hits
}

pub(super) fn shutdown_perf_owners<O: Clone>(
    mut previews: Vec<PreviewProductionRuntime<O>>,
    mut gpu: Option<HeadlessViewerGpuAdapter>,
    mut app: AppState,
    shared_deadline_budget_ms: u64,
    gpu_owner_required: bool,
) -> PerfOwnerClosureReport {
    let deadline = Instant::now() + Duration::from_millis(shared_deadline_budget_ms);
    for preview in &mut previews {
        preview.begin_endurance_shutdown();
    }
    app.begin_endurance_shutdown();
    let gpu_preview_dependency_barrier =
        gpu.as_mut().map(|gpu| gpu.release_preview_decoder_dependencies_until(deadline));
    let preview_evidence: Vec<_> =
        previews.into_iter().map(|preview| preview.shutdown_until(deadline)).collect();
    let previews_closed = preview_evidence.iter().all(|evidence| evidence.all_workers_terminated());
    let gpu_evidence = gpu.map(|gpu| {
        if previews_closed {
            gpu.shutdown_until(deadline)
        } else {
            gpu.quarantine_after_unresolved_preview()
        }
    });
    let app_evidence = app.shutdown_for_endurance(deadline);
    PerfOwnerClosureReport::from_evidence(
        shared_deadline_budget_ms,
        gpu_owner_required,
        preview_evidence,
        gpu_evidence,
        gpu_preview_dependency_barrier,
        app_evidence,
    )
}

#[derive(Debug, thiserror::Error)]
#[error("performance operation failed: {primary:#}; owner_closure={canonical_json}")]
pub(super) struct PerfOperationClosedFailure {
    primary: anyhow::Error,
    pub(super) owner_closure: PerfOwnerClosureReport,
    canonical_json: String,
}

pub(super) fn finish_perf_owner_run<T>(
    operation: anyhow::Result<T>,
    owner_closure: PerfOwnerClosureReport,
) -> anyhow::Result<(T, PerfOwnerClosureReport)> {
    let closure_validation = owner_closure.validate();
    let closure_json = serde_json::to_string(&owner_closure)
        .unwrap_or_else(|error| format!("{{\"serialization_error\":\"{error}\"}}"));
    match (operation, closure_validation) {
        (Ok(value), Ok(())) => Ok((value, owner_closure)),
        (Err(primary), Ok(())) | (Ok(_), Err(primary)) => Err(PerfOperationClosedFailure {
            primary,
            owner_closure,
            canonical_json: closure_json,
        }
        .into()),
        (Err(primary), Err(closure_error)) => Err(PerfOperationClosedFailure {
            primary: primary.context(format!("owner closure also failed: {closure_error:#}")),
            owner_closure,
            canonical_json: closure_json,
        }
        .into()),
    }
}

#[cfg(test)]
pub(super) fn create_perf_preview(
    app: AppState,
) -> anyhow::Result<(AppState, HeadlessPreviewRuntime)> {
    create_perf_preview_with(app, HeadlessPreviewRuntime::try_new)
}

pub(super) fn create_perf_preview_with(
    app: AppState,
    create: impl FnOnce() -> Result<
        HeadlessPreviewRuntime,
        super::preview_runtime::PreviewStartupFailure<
            super::headless_viewer_gpu::HeadlessViewerGpuOutput,
        >,
    >,
) -> anyhow::Result<(AppState, HeadlessPreviewRuntime)> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(create)) {
        Ok(Ok(preview)) => Ok((app, preview)),
        Ok(Err(failure)) => Err(shutdown_perf_startup::<
            super::headless_viewer_gpu::HeadlessViewerGpuOutput,
        >(
            app,
            Vec::new(),
            super::headless_execution_startup::HeadlessExecutionStartFailure::partial_preview(
                failure, None,
            ),
        )),
        Err(payload) => Err(shutdown_perf_startup::<
            super::headless_viewer_gpu::HeadlessViewerGpuOutput,
        >(
            app,
            Vec::new(),
            super::headless_execution_startup::HeadlessExecutionStartFailure::preview_construction(
                super::headless_execution_startup::startup_panic_diagnostic(payload),
            ),
        )),
    }
}

pub(super) fn run_with_perf_owners<O: Clone, T>(
    mut app: AppState,
    mut previews: Vec<PreviewProductionRuntime<O>>,
    mut gpu: Option<HeadlessViewerGpuAdapter>,
    operation: impl FnOnce(
        &mut AppState,
        &mut [PreviewProductionRuntime<O>],
        Option<&mut HeadlessViewerGpuAdapter>,
    ) -> anyhow::Result<T>,
) -> anyhow::Result<(T, PerfOwnerClosureReport)> {
    let operation_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        operation(&mut app, &mut previews, gpu.as_mut())
    }))
    .unwrap_or_else(|payload| {
        Err(
            super::execution_panic_diagnostic::execution_panic_diagnostic(
                payload,
                "performance operation",
            ),
        )
    });
    let budget_ms = perf_shutdown_budget_ms();
    let gpu_owner_required = gpu.is_some();
    let owner_closure = shutdown_perf_owners(previews, gpu, app, budget_ms, gpu_owner_required);
    finish_perf_owner_run(operation_result, owner_closure)
}

#[cfg(test)]
pub(super) fn run_with_perf_gpu_factory<O: Clone, T>(
    app: AppState,
    previews: Vec<PreviewProductionRuntime<O>>,
    create_gpu: impl FnOnce() -> Result<
        HeadlessViewerGpuAdapter,
        super::headless_execution_startup::HeadlessExecutionStartFailure,
    >,
    operation: impl FnOnce(
        &mut AppState,
        &mut [PreviewProductionRuntime<O>],
        Option<&mut HeadlessViewerGpuAdapter>,
    ) -> anyhow::Result<T>,
) -> anyhow::Result<(T, PerfOwnerClosureReport)> {
    let gpu_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(create_gpu))
        .unwrap_or_else(|payload| {
            Err(
                super::headless_execution_startup::HeadlessExecutionStartFailure::before_progress(
                    super::headless_execution_startup::startup_panic_diagnostic(payload),
                ),
            )
        });
    match gpu_result {
        Ok(gpu) => run_with_perf_owners(app, previews, Some(gpu), operation),
        Err(failure) => Err(shutdown_perf_startup(app, previews, failure)),
    }
}

/// Failed startup has its own actual inventory, never a missing normal GPU receipt.
#[derive(Debug, thiserror::Error)]
#[error("performance startup failed; startup_closure={canonical_json}")]
pub struct PerfStartupClosedFailure {
    #[source]
    primary: anyhow::Error,
    /// Consuming shutdown evidence of the actual App created for this operation.
    pub app: super::endurance_shutdown::AppEnduranceShutdownEvidence,
    /// Exact partial Headless construction inventory and its closure.
    pub headless: super::headless_execution_startup::HeadlessStartupShutdownEvidence,
    /// Exact receipts of all fully constructed Preview owners.
    pub previews: Vec<PreviewRuntimeShutdownEvidence>,
    canonical_json: String,
}

pub(super) fn shutdown_perf_startup<O: Clone>(
    mut app: AppState,
    mut previews: Vec<PreviewProductionRuntime<O>>,
    mut failure: super::headless_execution_startup::HeadlessExecutionStartFailure,
) -> anyhow::Error {
    let budget_ms = perf_shutdown_budget_ms();
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    for preview in &mut previews {
        preview.begin_endurance_shutdown();
    }
    failure.begin_shutdown();
    app.begin_endurance_shutdown();
    let mut previews = previews
        .into_iter()
        .map(|preview| preview.shutdown_until(deadline))
        .collect::<Vec<_>>();
    let (diagnostic, headless) = failure.shutdown_until(deadline);
    let headless_closed = headless.all_created_resources_released();
    previews.extend(headless.preview);
    let all_previews_closed = previews.iter().all(|receipt| receipt.all_workers_terminated());
    let preview_reports = previews
        .iter()
        .copied()
        .enumerate()
        .map(|(index, evidence)| PerfPreviewClosureReport::from_evidence(index, evidence))
        .collect::<Vec<_>>();
    let app = app.shutdown_for_endurance(deadline);
    let app_report = PerfAppClosureReport::from_evidence(&app);
    let all_resources_released =
        all_previews_closed && headless_closed && app_report.all_resources_released;
    let canonical_json = serde_json::json!({
        "shared_deadline_budget_ms": budget_ms,
        "previews": preview_reports,
        "preview_startup": headless.preview_startup,
        "gpu": headless.gpu,
        "opaque_panic_payload_abandoned": headless.opaque_panic_payload_abandoned,
        "preview_construction_unverified": headless.preview_construction_unverified,
        "app": app_report,
        "all_resources_released": all_resources_released,
    })
    .to_string();
    PerfStartupClosedFailure {
        primary: diagnostic,
        app,
        headless,
        previews,
        canonical_json,
    }
    .into()
}

#[cfg(test)]
pub(super) fn run_with_realtime_perf_owners<T>(
    mut app: AppState,
    mut realtime: HeadlessRealtimePlaybackSession,
    operation: impl FnOnce(&mut AppState, &mut HeadlessRealtimePlaybackSession) -> anyhow::Result<T>,
) -> anyhow::Result<(T, PerfOwnerClosureReport)> {
    let operation_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        operation(&mut app, &mut realtime)
    }))
    .unwrap_or_else(|payload| {
        Err(
            super::execution_panic_diagnostic::execution_panic_diagnostic(
                payload,
                "performance operation",
            ),
        )
    });
    let budget_ms = perf_shutdown_budget_ms();
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    let mut shutdown_owners = realtime.into_shutdown_owners();
    shutdown_owners.begin_shutdown();
    app.begin_endurance_shutdown();
    let shutdown = shutdown_owners.shutdown_until(deadline);
    let app_evidence = app.shutdown_for_endurance(deadline);
    let owner_closure = PerfOwnerClosureReport::from_evidence(
        budget_ms,
        true,
        vec![shutdown.preview],
        Some(shutdown.gpu),
        Some(shutdown.gpu_preview_dependency_barrier),
        app_evidence,
    );
    finish_perf_owner_run(operation_result, owner_closure)
}

#[cfg(test)]
pub(super) fn run_with_realtime_perf_gpu_factory<T>(
    app: AppState,
    create_gpu: impl FnOnce() -> Result<
        HeadlessViewerGpuAdapter,
        super::headless_execution_startup::HeadlessExecutionStartFailure,
    >,
    operation: impl FnOnce(&mut AppState, &mut HeadlessRealtimePlaybackSession) -> anyhow::Result<T>,
) -> anyhow::Result<(T, PerfOwnerClosureReport)> {
    let (app, preview) = create_perf_preview(app)?;
    let gpu_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(create_gpu))
        .unwrap_or_else(|payload| {
            Err(
                super::headless_execution_startup::HeadlessExecutionStartFailure::before_progress(
                    super::headless_execution_startup::startup_panic_diagnostic(payload),
                ),
            )
        });
    let gpu = match gpu_result {
        Ok(gpu) => gpu,
        Err(failure) => return Err(shutdown_perf_startup(app, vec![preview], failure)),
    };
    match HeadlessRealtimePlaybackSession::with_shutdown_owners(preview, gpu) {
        Ok(realtime) => run_with_realtime_perf_owners(app, realtime, operation),
        Err(failure) => Err(shutdown_perf_startup::<
            super::headless_viewer_gpu::HeadlessViewerGpuOutput,
        >(app, Vec::new(), failure)),
    }
}

/// Fixed bounded scenarios executed through the actual App/Preview owner Module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformanceOwnerProtocolCase {
    /// Normal work followed by complete cache, Preview and App closure.
    Clean,
    /// Ordinary workload failure must retain the clean raw closure.
    OperationError,
    /// Caught operation panic must retain the clean raw closure.
    OperationPanic,
    /// A missing profile-required cache must fail qualification after cleanup.
    MissingRequiredCache,
}

/// Immutable result of a bounded developer ownership probe, never a timing gate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PerformanceOwnerProtocolEvidence {
    /// Whether the operation and its required inventory both qualified.
    pub accepted: bool,
    /// Whether the exact returned raw closure satisfies the profile contract.
    pub closure_qualified: bool,
    /// Canonical complete Preview/GPU/App closure; contains no live owner.
    pub owner_closure_json: String,
}

/// Run one hardware-independent case without linking the complete App test suite.
/// The cache path belongs to the caller; the Module consumes every actual worker
/// before returning. This is validation support, not a second realtime runtime.
pub fn run_performance_owner_protocol_case(
    case: PerformanceOwnerProtocolCase,
    cache_root: &Path,
) -> anyhow::Result<PerformanceOwnerProtocolEvidence> {
    let config = if case == PerformanceOwnerProtocolCase::MissingRequiredCache {
        Err("injected required cache configuration failure".to_owned())
    } else {
        Ok(mondrian_render_cache::TimelineRenderCacheConfig::new(
            cache_root.to_path_buf(),
            256 * 1024 * 1024,
            64 * 1024 * 1024,
            4,
        )?)
    };
    let (app, preview) = create_perf_preview_with(AppState::new(), || {
        HeadlessPreviewRuntime::try_new_with_cache_config_for_validation(config)
    })?;
    let result = run_with_perf_owners(app, vec![preview], None, |_, _, _| match case {
        PerformanceOwnerProtocolCase::OperationError => {
            anyhow::bail!("injected bounded owner operation error")
        }
        PerformanceOwnerProtocolCase::OperationPanic => {
            panic!("injected bounded owner operation panic")
        }
        PerformanceOwnerProtocolCase::Clean
        | PerformanceOwnerProtocolCase::MissingRequiredCache => Ok(()),
    });
    let (accepted, closure) = match result {
        Ok(((), closure)) => (true, closure),
        Err(error) => match error.downcast_ref::<PerfOperationClosedFailure>() {
            Some(closed) => (false, closed.owner_closure.clone()),
            None => return Err(error),
        },
    };
    Ok(PerformanceOwnerProtocolEvidence {
        accepted,
        closure_qualified: closure.validate().is_ok(),
        owner_closure_json: serde_json::to_string(&closure)?,
    })
}
