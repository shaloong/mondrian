//! Unpublished Preview construction and exact native-owner installation order.
//!
//! Build the complete inert Runtime first. Every returned handle is installed
//! in that owner before a subsequent construction step can unwind.
use super::*;

use crate::app::preview_shutdown_evidence::{PreviewStartupInventory, PreviewStartupOwnerState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewStartupCheckpoint {
    Cache,
    Visual,
    CpuFallback,
    Media(usize),
    Observer,
}

/// Failed construction retains the unpublished Runtime, not just diagnostics.
#[must_use = "consume the partial Runtime under the original shutdown deadline"]
pub(crate) struct PreviewStartupFailure<O: Clone> {
    diagnostic: anyhow::Error,
    owner: PreviewStartupOwner<O>,
}

impl<O: Clone> std::fmt::Debug for PreviewStartupFailure<O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewStartupFailure")
            .field("diagnostic", &self.diagnostic)
            .field("inventory", &self.owner.inventory)
            .finish_non_exhaustive()
    }
}

impl<O: Clone> PreviewStartupFailure<O> {
    pub(crate) fn into_parts(self) -> (anyhow::Error, PreviewStartupOwner<O>) {
        (self.diagnostic, self.owner)
    }
}

/// Move-only, thread-affine owner of the actual partial Preview inventory.
pub(crate) struct PreviewStartupOwner<O: Clone> {
    runtime: Box<PreviewProductionRuntime<O>>,
    inventory: PreviewStartupInventory,
    opaque_panic_payload_abandoned: bool,
}

impl<O: Clone> PreviewStartupOwner<O> {
    pub(crate) fn begin_shutdown(&mut self) {
        self.runtime.begin_endurance_shutdown();
    }

    pub(crate) fn shutdown_until(
        self,
        deadline: Instant,
    ) -> crate::app::preview_shutdown_evidence::PreviewStartupShutdownEvidence {
        let (runtime, workers) = self.runtime.shutdown_until_with_inventory(deadline);
        crate::app::preview_shutdown_evidence::PreviewStartupShutdownEvidence {
            schema_version: 1,
            opaque_panic_payload_abandoned: self.opaque_panic_payload_abandoned,
            inventory: self.inventory,
            workers,
            runtime,
        }
    }
}

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Start production Preview while retaining all returned owners on unwind.
    pub(crate) fn try_new() -> Result<Self, PreviewStartupFailure<O>> {
        let budget = preview_decode_cpu_budget();
        let workers = media_preview_worker_count().min(budget.preview_worker_count);
        Self::try_start_with(
            budget,
            workers,
            MediaPreviewScheduler::default(),
            PreviewWorkerIsolation::RequiredPackaged,
            |_| {},
        )
    }

    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn try_start_with_checkpoint_for_host(
        checkpoint: impl FnMut(PreviewStartupCheckpoint),
    ) -> Result<Self, PreviewStartupFailure<O>> {
        let budget = preview_decode_cpu_budget();
        let workers = media_preview_worker_count().min(budget.preview_worker_count);
        Self::try_start_with(
            budget,
            workers,
            MediaPreviewScheduler::default(),
            PreviewWorkerIsolation::RequiredPackaged,
            checkpoint,
        )
    }

    fn try_start_with(
        budget: PreviewDecodeCpuBudget,
        workers: usize,
        scheduler: MediaPreviewScheduler,
        isolation: PreviewWorkerIsolation,
        checkpoint: impl FnMut(PreviewStartupCheckpoint),
    ) -> Result<Self, PreviewStartupFailure<O>> {
        Self::try_start_prepared(
            Self::prepare_unpublished(budget, scheduler),
            workers,
            isolation,
            checkpoint,
        )
    }

    fn try_start_prepared(
        runtime: Self,
        workers: usize,
        isolation: PreviewWorkerIsolation,
        checkpoint: impl FnMut(PreviewStartupCheckpoint),
    ) -> Result<Self, PreviewStartupFailure<O>> {
        let cache_required = runtime.timeline_render_cache.borrow().required();
        let mut owner = PreviewStartupOwner {
            runtime: Box::new(runtime),
            inventory: PreviewStartupInventory::new(workers, cache_required),
            opaque_panic_payload_abandoned: false,
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            owner
                .runtime
                .start_in_place(workers, isolation, &mut owner.inventory, checkpoint);
        })) {
            Ok(()) => Ok(*owner.runtime),
            Err(payload) => {
                let diagnostic = crate::app::execution_panic_diagnostic::execution_panic_diagnostic(
                    payload,
                    "Preview startup",
                );
                owner.opaque_panic_payload_abandoned =
                    crate::app::execution_panic_diagnostic::opaque_panic_payload_abandoned(
                        &diagnostic,
                    );
                Err(PreviewStartupFailure { diagnostic, owner })
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn try_start_with_checkpoint_for_test(
        workers: usize,
        checkpoint: impl FnMut(PreviewStartupCheckpoint),
    ) -> Result<Self, PreviewStartupFailure<O>> {
        Self::try_start_with(
            preview_decode_cpu_budget(),
            workers,
            MediaPreviewScheduler::default(),
            PreviewWorkerIsolation::DirectTestAdapter,
            checkpoint,
        )
    }

    pub(super) fn with_worker_count_and_scheduler(
        decode_cpu_budget: PreviewDecodeCpuBudget,
        worker_count: usize,
        scheduler: MediaPreviewScheduler,
        worker_isolation: PreviewWorkerIsolation,
    ) -> Self {
        let mut runtime = Self::prepare_unpublished(decode_cpu_budget, scheduler);
        let mut inventory = PreviewStartupInventory::new(
            worker_count,
            runtime.timeline_render_cache.borrow().required(),
        );
        runtime.start_in_place(worker_count, worker_isolation, &mut inventory, |_| {});
        runtime
    }

    fn prepare_unpublished(
        decode_cpu_budget: PreviewDecodeCpuBudget,
        scheduler: MediaPreviewScheduler,
    ) -> Self {
        let (work_notifier, work_watch) = preview_work_notification_channel();
        let timeline_render_cache =
            crate::app::preview_render_cache::PreviewTimelineRenderCache::prepare();
        let (job_tx, _) = scheduler.job_queue();
        let (_, result_rx) =
            mpsc::sync_channel::<MediaPreviewResult>(MEDIA_PREVIEW_COMPLETED_RESULT_QUEUE_CAPACITY);
        let shutdown = Arc::new(PreviewShutdownSignal::default());
        let decode_residency = Arc::new(PreviewDecodeResidencyCoordinator::new_with_notifier(
            work_notifier.clone(),
        ));
        let decode_worker_resources = mondrian_media::PreviewDecodeWorkerResources::default();
        Self {
            work_notifier: work_notifier.clone(),
            work_watch,
            jobs: job_tx,
            results: RefCell::new(result_rx),
            workers: RefCell::new(Vec::new()),
            unverified_async_worker_reaps: Cell::new(0),
            shutdown,
            decode_residency,
            observed_decode_residency_retry_revision: Cell::new(0),
            decode_residency_waiting: Cell::new(None),
            decode_worker_resources,
            frame_store: RefCell::new(PreviewFrameStoreAdapter::default()),
            media_aggregate_capacity_waiting: Cell::new(false),
            media_existing_work_waiters: RefCell::new(HashMap::new()),
            media_existing_work_retry_pending: Cell::new(false),
            media_existing_work_waiter_registrations: Cell::new(0),
            media_existing_work_retry_acknowledgements: Cell::new(0),
            scrub_adaptation: RefCell::new(PreviewScrubAdaptationState::default()),
            execution: RefCell::new(PreviewExecutionCoordinator::default()),
            transport_playing: Cell::new(false),
            transport_epoch: Cell::new(None),
            playback_pressure: Cell::new(PlaybackPressureState::default()),
            #[cfg(test)]
            last_video_preroll_observation: Cell::new(None),
            applied_resource_decision: Cell::new(None),
            applied_resource_trim: Cell::new(
                crate::app::execution_resource_coordination::ResourceTrimRequest::None,
            ),
            heterogeneous_effect_decision: Cell::new(
                crate::app::execution_resource_coordination::PreviewHeterogeneousEffectExecutionDecision::conservative_baseline(),
            ),
            scheduler,
            title_task: RefCell::new(PreviewTitleTask::with_notifier(work_notifier.clone())),
            visual_execution: None,
            visual_execution_start_failure: None,
            visual_execution_health_failed: Cell::new(false),
            cpu_fallback_task: None,
            cpu_fallback_start_failure: None,
            viewer_cpu_fallback_active: Cell::new(false),
            viewer_signal_monitoring: Cell::new((
                mondrian_core::ProgramScopesTap::default(),
                mondrian_core::SignalMonitoringSettings::default(),
            )),
            cpu_fallback_in_flight: RefCell::new(None),
            cpu_fallback_failure: RefCell::new(None),
            timeline_render_cache: RefCell::new(timeline_render_cache),
            visual_ready: RefCell::new(HashMap::new()),
            visual_failures: RefCell::new(HashMap::new()),
            media_execution_failures: RefCell::new(HashMap::new()),
            media_worker_health_failed: Cell::new(false),
            media_worker_start_failure: None,
            last_current_media_admission: Cell::new(None),
            last_gpu_loading_reason: Cell::new(None),
            visual_terminal_candidates: RefCell::new(Vec::new()),
            visual_program_authoring_session: Cell::new(None),
            visual_programs: RefCell::new(mondrian_renderer::PreparedVisualProgramCache::default()),
            future_media_window: RefCell::new(request_scheduler::FutureMediaWindowCache::default()),
            visual_dependencies: PreviewVisualDependencyObserver::prepare(
                work_notifier.clone(),
            ),
            visual_dependency_health_failed: Cell::new(false),
            scratch: RefCell::new(TimelineCompositeScratch::default()),
            evaluation_working_set: RefCell::new(EvaluationWorkingSet::new()),
            evaluation_working_set_clock: Cell::new(0),
            last_color_rejection: RefCell::new(None),
            unavailability_evidence: RefCell::new(PreviewUnavailabilityEvidence::default()),
            display_snapshot: RefCell::new(None),
            display_snapshot_identity: Cell::new(None),
            last_gallery_capture: RefCell::new(None),
            hardware_decode_admission: Cell::new(PreviewHardwareDecodeAdmissionState::default()),
            decode_cpu_budget,
            decode_worker_count: 0,
            decode_execution_watch: PreviewDecodeWorkerExecutionWatch::new(
                Vec::new(),
            ),
            metrics: PreviewMetrics::default(),
        }
    }

    fn start_in_place(
        &mut self,
        worker_count: usize,
        worker_isolation: PreviewWorkerIsolation,
        inventory: &mut PreviewStartupInventory,
        mut checkpoint: impl FnMut(PreviewStartupCheckpoint),
    ) {
        let work_notifier = self.work_notifier.clone();
        if inventory.cache != PreviewStartupOwnerState::Disabled {
            inventory.cache = PreviewStartupOwnerState::InProgress;
        }
        self.timeline_render_cache.get_mut().start_in_place(work_notifier.clone());
        if inventory.cache != PreviewStartupOwnerState::Disabled {
            inventory.cache = PreviewStartupOwnerState::from_installed(
                self.timeline_render_cache.get_mut().worker_started(),
            );
        }
        checkpoint(PreviewStartupCheckpoint::Cache);
        let scheduler = self.scheduler.clone();
        let (_, job_rx) = scheduler.job_queue();
        let (result_tx, result_rx) =
            mpsc::sync_channel::<MediaPreviewResult>(MEDIA_PREVIEW_COMPLETED_RESULT_QUEUE_CAPACITY);
        *self.results.get_mut() = result_rx;
        let shutdown = Arc::clone(&self.shutdown);
        let decode_residency = Arc::clone(&self.decode_residency);
        let decode_worker_resources = self.decode_worker_resources.clone();
        let decode_cpu_budget = self.decode_cpu_budget;
        let initial_frame_store = mondrian_playback::PreviewFrameStoreConfig::default();
        inventory.visual = PreviewStartupOwnerState::InProgress;
        let (visual_execution, visual_execution_start_failure) =
            match VisualExecutionTask::new_with_notifier(
                mondrian_playback::SystemMonotonicRuntimeClock::default(),
                VisualExecutionTaskConfig::default(),
                work_notifier.clone(),
            ) {
                Ok(task) => (Some(task), None),
                Err(error) => {
                    tracing::error!("failed to start Preview visual execution worker: {error}");
                    (None, Some(error.to_string()))
                }
            };
        self.visual_execution = visual_execution;
        self.visual_execution_start_failure = visual_execution_start_failure;
        inventory.visual =
            PreviewStartupOwnerState::from_installed(self.visual_execution.is_some());
        checkpoint(PreviewStartupCheckpoint::Visual);
        inventory.cpu_fallback = PreviewStartupOwnerState::InProgress;
        let (cpu_fallback_task, cpu_fallback_start_failure) =
            match PreviewCpuFallbackTask::new(work_notifier.clone()) {
                Ok(task) => (Some(task), None),
                Err(error) => {
                    tracing::error!("failed to start Preview CPU fallback worker: {error}");
                    (None, Some(error.to_string()))
                }
            };
        self.cpu_fallback_task = cpu_fallback_task;
        self.cpu_fallback_start_failure = cpu_fallback_start_failure;
        inventory.cpu_fallback =
            PreviewStartupOwnerState::from_installed(self.cpu_fallback_task.is_some());
        checkpoint(PreviewStartupCheckpoint::CpuFallback);
        self.workers.get_mut().reserve(worker_count);
        let mut decode_execution_observers = Vec::with_capacity(worker_count);
        let (demux_worker_executable, mut media_worker_start_failure) = if worker_count == 0
            || !worker_isolation.requires_packaged_worker()
        {
            (None, None)
        } else {
            match crate::app::packaged_worker::discover_preview_demux_worker() {
                Ok(executable) => (Some(executable), None),
                Err(error) => {
                    tracing::error!(
                        %error,
                        "Preview media workers were not started because required demux isolation is unavailable"
                    );
                    scheduler.close();
                    (None, Some(error.to_string()))
                }
            }
        };
        for worker_index in 0..worker_count {
            if worker_isolation.requires_packaged_worker() && demux_worker_executable.is_none() {
                break;
            }
            inventory.media[worker_index] = PreviewStartupOwnerState::InProgress;
            let worker_jobs = job_rx.clone();
            let worker_results = result_tx.clone();
            let worker_work_notifier = work_notifier.clone();
            let worker_scheduler = scheduler.clone();
            let worker_shutdown = Arc::clone(&shutdown);
            let worker_decode_residency = Arc::clone(&decode_residency);
            let worker_lane = media_preview_worker_lane(worker_index, worker_count);
            let (worker_decode_context_bootstrap, execution_observer) =
                match demux_worker_executable.clone() {
                    Some(executable) => mondrian_media::PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(executable),
                    #[cfg(test)]
                    None if worker_isolation == PreviewWorkerIsolation::DirectTestAdapter => {
                        mondrian_media::PreviewDecodeSessionContext::observed_bootstrap()
                    }
                    None => unreachable!("required packaged worker was checked before spawn"),
                };
            let worker_decode_context_bootstrap = worker_decode_context_bootstrap
                .with_worker_resources(decode_worker_resources.clone())
                .with_decoder_thread_limit(match worker_lane {
                    MediaPreviewWorkerLane::NonPlayback => {
                        decode_cpu_budget.decoder_threads_per_worker
                    }
                    MediaPreviewWorkerLane::Any | MediaPreviewWorkerLane::Playback => {
                        decode_cpu_budget.max_decoder_threads_per_worker
                    }
                });
            decode_residency.register_worker(worker_lane);
            match std::thread::Builder::new()
                .name(format!("mondrian-preview-worker-{worker_index}"))
                .spawn(move || {
                    media_preview_worker(
                        worker_lane,
                        worker_jobs,
                        worker_results,
                        worker_work_notifier,
                        worker_scheduler,
                        worker_shutdown,
                        worker_decode_residency,
                        worker_decode_context_bootstrap,
                    )
                }) {
                Ok(handle) => {
                    self.workers.get_mut().push(handle);
                    inventory.media[worker_index] = PreviewStartupOwnerState::Installed;
                    decode_execution_observers.push((worker_lane, execution_observer));
                    self.decode_worker_count += 1;
                }
                Err(err) => {
                    inventory.media[worker_index] = PreviewStartupOwnerState::Failed;
                    decode_residency.unregister_worker(worker_lane);
                    if media_worker_start_failure.is_none() {
                        media_worker_start_failure = Some(format!(
                            "failed to start Preview media worker {worker_index}: {err}"
                        ));
                    }
                    tracing::warn!(
                        worker_index,
                        "failed to start production preview worker: {err}"
                    );
                }
            }
            checkpoint(PreviewStartupCheckpoint::Media(worker_index));
        }
        if worker_count > 0 && self.decode_worker_count == 0 {
            scheduler.close();
            media_worker_start_failure.get_or_insert_with(|| {
                "no configured Preview media worker could be started".to_owned()
            });
        }
        decode_worker_resources.reconfigure_session_residency(
            mondrian_media::PreviewDecodeSessionResidencyConfig::from_family_resource_unit_budget(
                initial_frame_store.current_media_working_set_resource_unit_limit,
                self.decode_worker_count.max(1),
            ),
        );

        self.media_worker_health_failed.set(media_worker_start_failure.is_some());
        self.media_worker_start_failure = media_worker_start_failure;
        self.decode_execution_watch =
            PreviewDecodeWorkerExecutionWatch::new(decode_execution_observers);
        inventory.observer = PreviewStartupOwnerState::InProgress;
        let started = self.visual_dependencies.start_in_place();
        inventory.observer = PreviewStartupOwnerState::from_installed(started.is_ok());
        if let Err(error) = started {
            tracing::warn!(%error, "Preview visual dependency observer unavailable");
        }
        checkpoint(PreviewStartupCheckpoint::Observer);
    }
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
