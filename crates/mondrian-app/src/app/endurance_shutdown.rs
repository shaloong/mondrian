//! Shared consuming-shutdown evidence for App-owned background workers.
//!
//! Ordinary product teardown remains bounded and best-effort. Qualification
//! teardown uses the same cancellation seams, but retains exact join, panic,
//! timeout, detach, and residual-work facts instead of equating `Drop` with
//! worker closure.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(any(test, feature = "validation"))]
use std::sync::Arc;

#[cfg(any(test, feature = "validation"))]
use mondrian_export::ExportQueueShutdownEvidence;
#[cfg(any(test, feature = "validation"))]
use mondrian_media::{
    AudioPlaybackShutdownEvidence, AudioSourceCache, AudioSourceCacheShutdownEvidence,
};
#[cfg(any(test, feature = "validation"))]
use mondrian_reference_output::ReferenceOutputModuleShutdownReceipt;

#[cfg(any(test, feature = "validation"))]
use super::project_lifecycle::ProjectClosePoll;
#[cfg(any(test, feature = "validation"))]
use super::AppState;

/// Exact terminal inventory for one App-owned background worker domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnduranceWorkerShutdownEvidence {
    /// Workers configured for this service instance.
    pub requested_workers: u32,
    /// Whether this service attempted to create its configured workers.
    pub startup_attempted: bool,
    /// Workers whose operating-system threads were created.
    pub started_workers: u32,
    /// Workers joined after returning normally.
    pub terminated_workers: u32,
    /// Workers joined after panicking.
    pub panicked_workers: u32,
    /// Workers that did not return before the shared App shutdown deadline.
    pub timed_out_workers: u32,
    /// Join handles relinquished after the shared deadline expired.
    pub detached_workers: u32,
    /// Workers that exited before explicit shutdown, excluding exits already
    /// classified as a joined panic.
    pub unexpected_worker_exits: u32,
    /// Admitted queue entries still retained at the terminal observation.
    pub queued_work_remaining: u64,
    /// Physically executing work still retained at the terminal observation.
    pub running_work_remaining: u64,
    /// Other worker-owned resources still retained at the terminal observation.
    pub owned_resources_remaining: u64,
    /// Cumulative domain failures observed before or during shutdown.
    pub cumulative_failures: u64,
}

impl EnduranceWorkerShutdownEvidence {
    /// Whether every configured worker was available for workload admission.
    pub const fn all_expected_workers_started(self) -> bool {
        !self.startup_attempted || self.requested_workers == self.started_workers
    }

    /// Whether every created worker returned normally without residual work.
    pub const fn all_workers_terminated(self) -> bool {
        self.started_workers == self.terminated_workers
            && self.panicked_workers == 0
            && self.timed_out_workers == 0
            && self.detached_workers == 0
            && self.queued_work_remaining == 0
            && self.running_work_remaining == 0
            && self.owned_resources_remaining == 0
    }

    /// Whether admission availability and consuming shutdown both closed cleanly.
    pub const fn lifecycle_closed(self) -> bool {
        self.all_expected_workers_started()
            && self.all_workers_terminated()
            && self.unexpected_worker_exits == 0
    }

    pub(crate) const fn from_join(
        requested_workers: usize,
        startup_attempted: bool,
        join: EnduranceWorkerJoinOutcome,
        queued_work_remaining: usize,
        running_work_remaining: usize,
        owned_resources_remaining: usize,
        cumulative_failures: u64,
    ) -> Self {
        Self {
            requested_workers: saturating_usize_to_u32(requested_workers),
            startup_attempted,
            started_workers: join.started_workers,
            terminated_workers: join.terminated_workers,
            panicked_workers: join.panicked_workers,
            timed_out_workers: join.timed_out_workers,
            detached_workers: join.detached_workers,
            unexpected_worker_exits: 0,
            queued_work_remaining: saturating_usize_to_u64(queued_work_remaining),
            running_work_remaining: saturating_usize_to_u64(running_work_remaining),
            owned_resources_remaining: saturating_usize_to_u64(owned_resources_remaining),
            cumulative_failures,
        }
    }

    pub(crate) const fn with_unexpected_worker_exits(mut self, exits: u32) -> Self {
        self.unexpected_worker_exits = exits;
        self
    }
}

/// One background execution domain's owner-derived endurance gauges.
#[cfg(any(test, feature = "validation"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AppBackgroundDomainSnapshot {
    pub(crate) queue_depth: u64,
    pub(crate) owned_resource_units: u64,
    pub(crate) cumulative_failures: u64,
    pub(crate) worker_health_failures: u64,
}

#[cfg(any(test, feature = "validation"))]
impl AppBackgroundDomainSnapshot {
    fn terminal_from_worker(evidence: EnduranceWorkerShutdownEvidence) -> Result<Self, String> {
        let startup_failures = if evidence.startup_attempted {
            evidence.requested_workers.saturating_sub(evidence.started_workers)
        } else {
            0
        };
        let deadline_failures = evidence.timed_out_workers.max(evidence.detached_workers);
        let worker_health_failures = u64::from(startup_failures)
            .checked_add(u64::from(evidence.panicked_workers))
            .and_then(|value| value.checked_add(u64::from(deadline_failures)))
            .and_then(|value| value.checked_add(u64::from(evidence.unexpected_worker_exits)))
            .ok_or_else(|| "background worker-health failure count overflowed u64".to_owned())?;
        let mut queue_depth = evidence
            .queued_work_remaining
            .checked_add(evidence.running_work_remaining)
            .ok_or_else(|| "background terminal queue depth overflowed u64".to_owned())?;
        let mut owned_resource_units = evidence.owned_resources_remaining;
        if !evidence.lifecycle_closed() {
            queue_depth = queue_depth.max(1);
            owned_resource_units = owned_resource_units.max(1);
        }
        Ok(Self {
            queue_depth,
            owned_resource_units,
            cumulative_failures: evidence.cumulative_failures,
            worker_health_failures,
        })
    }

    fn merge_terminal(self, terminal: Self) -> Self {
        Self {
            queue_depth: terminal.queue_depth,
            owned_resource_units: terminal.owned_resource_units,
            cumulative_failures: self.cumulative_failures.max(terminal.cumulative_failures),
            worker_health_failures: self
                .worker_health_failures
                .max(terminal.worker_health_failures),
        }
    }

    fn checked_add(self, other: Self) -> Result<Self, String> {
        Ok(Self {
            queue_depth: self
                .queue_depth
                .checked_add(other.queue_depth)
                .ok_or_else(|| "background queue depth overflowed u64".to_owned())?,
            owned_resource_units: self
                .owned_resource_units
                .checked_add(other.owned_resource_units)
                .ok_or_else(|| "background resource inventory overflowed u64".to_owned())?,
            cumulative_failures: self
                .cumulative_failures
                .checked_add(other.cumulative_failures)
                .ok_or_else(|| "background failure count overflowed u64".to_owned())?,
            worker_health_failures: self
                .worker_health_failures
                .checked_add(other.worker_health_failures)
                .ok_or_else(|| "background worker-health count overflowed u64".to_owned())?,
        })
    }
}

/// Fixed-shape runtime snapshot for every App-owned auxiliary execution domain.
#[cfg(any(test, feature = "validation"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AppBackgroundEnduranceSnapshot {
    pub(crate) audio_idle_warmup: AppBackgroundDomainSnapshot,
    pub(crate) media_import: AppBackgroundDomainSnapshot,
    pub(crate) media_asset_mutation: AppBackgroundDomainSnapshot,
    pub(crate) visual_tracking: AppBackgroundDomainSnapshot,
    pub(crate) proxy_generation: AppBackgroundDomainSnapshot,
    pub(crate) infrastructure: AppBackgroundDomainSnapshot,
}

#[cfg(any(test, feature = "validation"))]
impl AppBackgroundEnduranceSnapshot {
    pub(crate) fn totals(self) -> Result<AppBackgroundDomainSnapshot, String> {
        self.audio_idle_warmup
            .checked_add(self.media_import)?
            .checked_add(self.media_asset_mutation)?
            .checked_add(self.visual_tracking)?
            .checked_add(self.proxy_generation)?
            .checked_add(self.infrastructure)
    }

    pub(crate) fn merge_terminal(self, terminal: Self) -> Self {
        Self {
            audio_idle_warmup: self.audio_idle_warmup.merge_terminal(terminal.audio_idle_warmup),
            media_import: self.media_import.merge_terminal(terminal.media_import),
            media_asset_mutation: self
                .media_asset_mutation
                .merge_terminal(terminal.media_asset_mutation),
            visual_tracking: self.visual_tracking.merge_terminal(terminal.visual_tracking),
            proxy_generation: self.proxy_generation.merge_terminal(terminal.proxy_generation),
            infrastructure: self.infrastructure.merge_terminal(terminal.infrastructure),
        }
    }
}

/// Project-authority and library lifetime facts after consuming one App phase.
#[cfg(any(test, feature = "validation"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppProjectShutdownEvidence {
    /// Whether shutdown began with an installed authoring Session.
    pub session_was_open: bool,
    /// Whether an asynchronous Project close was already active.
    pub pending_close_was_active: bool,
    /// Whether no authoring Session remains in the consumed App owner.
    pub authoring_session_released: bool,
    /// Whether no persistence close ticket remains installed.
    pub pending_close_released: bool,
    /// Whether the active kernel-backed runtime lease was released.
    pub runtime_lease_released: bool,
    /// Retired library generations still retaining live or uncollectable state.
    pub retired_library_generations_remaining: u32,
    /// Stable lifecycle failure retained instead of being discarded by Drop.
    pub lifecycle_failure: Option<String>,
}

#[cfg(any(test, feature = "validation"))]
impl AppProjectShutdownEvidence {
    /// Whether Project, persistence handoff, library, and lease ownership closed.
    pub const fn all_resources_released(&self) -> bool {
        self.authoring_session_released
            && self.pending_close_released
            && self.runtime_lease_released
            && self.retired_library_generations_remaining == 0
            && self.lifecycle_failure.is_none()
    }
}

/// App-side ownership evidence around the consuming Audio Source Cache receipt.
#[cfg(any(test, feature = "validation"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppAudioSourceCacheShutdownEvidence {
    /// Strong references observed before the App relinquished its Cache Arc.
    pub strong_references_before_consumption: u32,
    /// Strong references remaining outside the App owner.
    pub strong_references_remaining: u32,
    /// Consuming Cache/FFmpeg receipt, present only when the Arc was unique.
    pub cache: Option<AudioSourceCacheShutdownEvidence>,
}

#[cfg(any(test, feature = "validation"))]
impl AppAudioSourceCacheShutdownEvidence {
    /// Whether all Cache, decoder Session, child, and pump ownership closed.
    pub fn all_resources_released(&self) -> bool {
        self.strong_references_remaining == 0
            && self.cache.as_ref().is_some_and(|cache| cache.all_resources_released())
    }
}

/// Complete consuming receipt for every execution owner embedded in AppState.
#[cfg(any(test, feature = "validation"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEnduranceShutdownEvidence {
    /// Whether the sole AppState owner itself was consumed after receipts were sealed.
    pub app_owner_consumed: bool,
    /// Project Session, library generation, and kernel lease evidence.
    pub project: AppProjectShutdownEvidence,
    /// Physical/simulated Reference Output Module consuming evidence.
    pub reference_output: ReferenceOutputModuleShutdownReceipt,
    /// Export queue consuming worker evidence.
    pub export: ExportQueueShutdownEvidence,
    /// Realtime Audio render/output consuming evidence.
    pub audio: AudioPlaybackShutdownEvidence,
    /// Decoded Audio Source Cache and FFmpeg child/pump evidence.
    pub audio_source_cache: AppAudioSourceCacheShutdownEvidence,
    /// Lazy native memory-observer worker evidence.
    pub execution_memory_observer: EnduranceWorkerShutdownEvidence,
    /// Project persistence service worker evidence.
    pub project_persistence: EnduranceWorkerShutdownEvidence,
    /// Speculative Audio warmup worker evidence.
    pub audio_idle_warmup: EnduranceWorkerShutdownEvidence,
    /// Media Import worker-pool evidence.
    pub media_import: EnduranceWorkerShutdownEvidence,
    /// Ordered Asset-mutation parent worker evidence.
    pub media_asset_mutation: EnduranceWorkerShutdownEvidence,
    /// Mask tracking worker evidence.
    pub visual_tracking: EnduranceWorkerShutdownEvidence,
    /// Lazy Proxy generation worker-pool evidence.
    pub proxy_generation: EnduranceWorkerShutdownEvidence,
}

#[cfg(any(test, feature = "validation"))]
impl AppEnduranceShutdownEvidence {
    /// Whether non-worker App ownership closed, excluding realtime Audio.
    ///
    /// Auxiliary worker closure is projected from its per-domain terminal
    /// evidence, so this aggregate contains only owners without another
    /// detailed Headless bucket.
    pub fn all_residual_owner_resources_released(&self) -> bool {
        self.app_owner_consumed
            && self.project.all_resources_released()
            && self.reference_output.all_resources_released()
            && self.export.all_resources_released()
            && self.audio_source_cache.all_resources_released()
    }

    /// Whether every App-owned non-realtime-Audio domain closed without detach.
    ///
    /// Headless terminal projection accounts realtime Audio separately, so
    /// this seam prevents one Audio closure failure from being counted twice.
    pub fn all_non_audio_resources_released(&self) -> bool {
        self.all_residual_owner_resources_released()
            && self.execution_memory_observer.lifecycle_closed()
            && self.project_persistence.lifecycle_closed()
            && self.audio_idle_warmup.lifecycle_closed()
            && self.media_import.lifecycle_closed()
            && self.media_asset_mutation.lifecycle_closed()
            && self.visual_tracking.lifecycle_closed()
            && self.proxy_generation.lifecycle_closed()
    }

    /// Whether every App-owned worker/resource domain closed without detach.
    pub fn all_resources_released(&self) -> bool {
        self.audio.all_workers_terminated() && self.all_non_audio_resources_released()
    }

    pub(crate) fn background_terminal_snapshot(
        &self,
    ) -> Result<AppBackgroundEnduranceSnapshot, String> {
        Ok(AppBackgroundEnduranceSnapshot {
            audio_idle_warmup: AppBackgroundDomainSnapshot::terminal_from_worker(
                self.audio_idle_warmup,
            )?,
            media_import: AppBackgroundDomainSnapshot::terminal_from_worker(self.media_import)?,
            media_asset_mutation: AppBackgroundDomainSnapshot::terminal_from_worker(
                self.media_asset_mutation,
            )?,
            visual_tracking: AppBackgroundDomainSnapshot::terminal_from_worker(
                self.visual_tracking,
            )?,
            proxy_generation: AppBackgroundDomainSnapshot::terminal_from_worker(
                self.proxy_generation,
            )?,
            infrastructure: AppBackgroundDomainSnapshot::terminal_from_worker(
                self.project_persistence,
            )?
            .checked_add(AppBackgroundDomainSnapshot::terminal_from_worker(
                self.execution_memory_observer,
            )?)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnduranceWorkerJoinOutcome {
    started_workers: u32,
    terminated_workers: u32,
    panicked_workers: u32,
    timed_out_workers: u32,
    detached_workers: u32,
}

impl EnduranceWorkerJoinOutcome {
    pub(crate) const fn panicked_workers(self) -> u32 {
        self.panicked_workers
    }

    pub(crate) const fn all_workers_returned_normally(self) -> bool {
        self.started_workers == self.terminated_workers
            && self.panicked_workers == 0
            && self.timed_out_workers == 0
            && self.detached_workers == 0
    }
}

/// Join every already-signaled worker against one App-wide absolute deadline.
///
/// Handles still running at the deadline are relinquished and reported as both
/// timed out and detached. Callers must stop the campaign rather than starting
/// another phase after such a receipt.
pub(crate) fn join_workers_until(
    handles: &mut Vec<JoinHandle<()>>,
    deadline: Instant,
) -> EnduranceWorkerJoinOutcome {
    let started_workers = saturating_usize_to_u32(handles.len());
    let mut terminated_workers = 0_u32;
    let mut panicked_workers = 0_u32;
    loop {
        let mut index = handles.len();
        while index > 0 {
            index -= 1;
            if handles[index].is_finished() {
                let handle = handles.swap_remove(index);
                if handle.join().is_ok() {
                    terminated_workers = terminated_workers.saturating_add(1);
                } else {
                    panicked_workers = panicked_workers.saturating_add(1);
                }
            }
        }
        if handles.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let timed_out_workers = saturating_usize_to_u32(handles.len());
    handles.clear();
    EnduranceWorkerJoinOutcome {
        started_workers,
        terminated_workers,
        panicked_workers,
        timed_out_workers,
        detached_workers: timed_out_workers,
    }
}

#[cfg(any(test, feature = "validation"))]
impl AppState {
    /// Capture exact runtime gauges and monotonic failures for every auxiliary
    /// App-owned execution domain without changing admission or ownership.
    pub(crate) fn background_endurance_snapshot(
        &self,
    ) -> Result<AppBackgroundEnduranceSnapshot, String> {
        let audio_idle_warmup = self.audio_idle_warmup.diagnostics();
        let audio_running = u64::from(audio_idle_warmup.running.is_some());
        let audio_queue_depth =
            checked_usize_to_u64(audio_idle_warmup.queued, "Audio idle warmup queue depth")?
                .checked_add(audio_running)
                .ok_or_else(|| "Audio idle warmup queue depth overflowed u64".to_owned())?;
        let audio_failures =
            audio_idle_warmup
                .failures
                .checked_add(audio_idle_warmup.rejections)
                .ok_or_else(|| "Audio idle warmup failure count overflowed u64".to_owned())?;

        let media_import = self.media_import.diagnostics();
        let import_startup_failures = media_import
            .requested_workers
            .checked_sub(media_import.started_workers)
            .ok_or_else(|| {
                "Media Import reported more started workers than requested".to_owned()
            })?;
        let import_worker_health = checked_usize_to_u64(
            import_startup_failures,
            "Media Import worker startup failures",
        )?
        .checked_add(u64::from(media_import.worker_unexpectedly_exited))
        .ok_or_else(|| "Media Import worker-health count overflowed u64".to_owned())?;
        let import_failures = media_import
            .preparation_failures
            .checked_add(media_import.publication_failures)
            .and_then(|value| value.checked_add(media_import.rejections))
            .ok_or_else(|| "Media Import failure count overflowed u64".to_owned())?;

        let media_asset_mutation = self.media_asset_mutations.diagnostics();
        let mutation_failures = media_asset_mutation
            .failures
            .checked_add(media_asset_mutation.rejections)
            .ok_or_else(|| "Media Asset mutation failure count overflowed u64".to_owned())?;
        let mutation_worker_health = u64::from(!media_asset_mutation.worker_available)
            .checked_add(u64::from(media_asset_mutation.worker_unexpectedly_exited))
            .ok_or_else(|| "Media Asset mutation worker-health count overflowed u64".to_owned())?;

        let visual_tracking = self.visual_tracking.diagnostics();
        let tracking_resources = visual_tracking
            .running
            .checked_add(visual_tracking.terminal_results_pending)
            .ok_or_else(|| "Visual tracking resource inventory overflowed u64".to_owned())?;
        let tracking_failures = visual_tracking
            .failures
            .checked_add(visual_tracking.rejections)
            .and_then(|value| value.checked_add(visual_tracking.accounting_anomalies))
            .ok_or_else(|| "Visual tracking failure count overflowed u64".to_owned())?;
        let tracking_startup_failure =
            u64::from(visual_tracking.worker_startup_attempted && !visual_tracking.worker_started);
        let tracking_worker_health = tracking_startup_failure
            .checked_add(visual_tracking.worker_unexpected_exits)
            .ok_or_else(|| "Visual tracking worker-health count overflowed u64".to_owned())?;

        let proxy_generation = self.proxy_generation.diagnostics();
        let proxy_queue_depth =
            checked_usize_to_u64(proxy_generation.queued, "Proxy generation queued attempts")?
                .checked_add(checked_usize_to_u64(
                    proxy_generation.running,
                    "Proxy generation running attempts",
                )?)
                .ok_or_else(|| "Proxy generation queue depth overflowed u64".to_owned())?;
        let proxy_failures = proxy_generation
            .failures
            .checked_add(proxy_generation.rejections)
            .ok_or_else(|| "Proxy generation failure count overflowed u64".to_owned())?;
        let proxy_startup_failures = if proxy_generation.worker_startup_attempted {
            proxy_generation
                .requested_workers
                .checked_sub(proxy_generation.started_workers)
                .ok_or_else(|| {
                    "Proxy generation reported more started workers than requested".to_owned()
                })?
        } else {
            0
        };
        let proxy_worker_health = checked_usize_to_u64(
            proxy_startup_failures,
            "Proxy generation worker startup failures",
        )?
        .checked_add(u64::from(proxy_generation.worker_unexpectedly_exited))
        .ok_or_else(|| "Proxy generation worker-health count overflowed u64".to_owned())?;

        let persistence = self.project_persistence.endurance_runtime_facts();
        let native_memory = self.execution_resources.endurance_runtime_facts();
        let infrastructure_queue_depth = persistence
            .queue_depth
            .checked_add(persistence.running_work)
            .and_then(|value| value.checked_add(native_memory.queue_depth))
            .and_then(|value| value.checked_add(native_memory.running_work))
            .ok_or_else(|| "Infrastructure queue depth overflowed u64".to_owned())?;
        let infrastructure_owned_resources = persistence
            .owned_resources
            .checked_add(native_memory.owned_resources)
            .ok_or_else(|| "Infrastructure resource inventory overflowed u64".to_owned())?;
        let infrastructure_failures = persistence
            .cumulative_failures
            .checked_add(native_memory.cumulative_failures)
            .ok_or_else(|| "Infrastructure failure count overflowed u64".to_owned())?;
        let infrastructure_worker_health = persistence
            .worker_health_failures
            .checked_add(native_memory.worker_health_failures)
            .ok_or_else(|| "Infrastructure worker-health count overflowed u64".to_owned())?;

        Ok(AppBackgroundEnduranceSnapshot {
            audio_idle_warmup: AppBackgroundDomainSnapshot {
                queue_depth: audio_queue_depth,
                owned_resource_units: audio_running,
                cumulative_failures: audio_failures,
                worker_health_failures: u64::from(
                    !audio_idle_warmup.worker_available
                        || audio_idle_warmup.worker_unexpectedly_exited,
                ),
            },
            media_import: AppBackgroundDomainSnapshot {
                queue_depth: checked_usize_to_u64(
                    media_import.transport_occupied_files,
                    "Media Import transport occupancy",
                )?,
                owned_resource_units: checked_usize_to_u64(
                    media_import.running_files,
                    "Media Import running files",
                )?,
                cumulative_failures: import_failures,
                worker_health_failures: import_worker_health,
            },
            media_asset_mutation: AppBackgroundDomainSnapshot {
                queue_depth: checked_usize_to_u64(
                    media_asset_mutation.transport_occupied,
                    "Media Asset mutation transport occupancy",
                )?,
                owned_resource_units: checked_usize_to_u64(
                    media_asset_mutation.running,
                    "Media Asset mutation running operations",
                )?,
                cumulative_failures: mutation_failures,
                worker_health_failures: mutation_worker_health,
            },
            visual_tracking: AppBackgroundDomainSnapshot {
                queue_depth: visual_tracking.transport_occupied,
                owned_resource_units: tracking_resources,
                cumulative_failures: tracking_failures,
                worker_health_failures: tracking_worker_health,
            },
            proxy_generation: AppBackgroundDomainSnapshot {
                queue_depth: proxy_queue_depth,
                owned_resource_units: checked_usize_to_u64(
                    proxy_generation.running,
                    "Proxy generation running attempts",
                )?,
                cumulative_failures: proxy_failures,
                worker_health_failures: proxy_worker_health,
            },
            infrastructure: AppBackgroundDomainSnapshot {
                queue_depth: infrastructure_queue_depth,
                owned_resource_units: infrastructure_owned_resources,
                cumulative_failures: infrastructure_failures,
                worker_health_failures: infrastructure_worker_health,
            },
        })
    }

    /// Close admission and broadcast cooperative cancellation to every App-owned
    /// domain that can be signaled without consuming its terminal receipt.
    pub(crate) fn begin_endurance_shutdown(&mut self) {
        if self.authoring.is_some()
            && self.pending_project_close.is_none()
            && self.project_close_fault.is_none()
            && let Err(error) = self.begin_project_close()
        {
            // The consuming phase retries and retains this failure in the
            // Project receipt. Signaling every other owner must still proceed.
            tracing::warn!(%error, "failed to begin Project close during endurance signal phase");
        }
        self.project_persistence.begin_endurance_shutdown();
        self.render_queue.begin_shutdown();
        self.reference_output.begin_endurance_shutdown();
        self.begin_audio_playback_endurance_shutdown();
        self.audio_source_cache.begin_shutdown();
        self.audio_idle_warmup.begin_endurance_shutdown();
        self.media_import.begin_endurance_shutdown();
        self.media_asset_mutations.begin_endurance_shutdown();
        self.visual_tracking.begin_endurance_shutdown();
        self.proxy_generation.begin_endurance_shutdown();
        self.execution_resources.begin_endurance_shutdown();
    }

    /// Consume AppState and synchronously collect every embedded owner receipt.
    pub(crate) fn shutdown_for_endurance(
        mut self,
        deadline: Instant,
    ) -> AppEnduranceShutdownEvidence {
        self.begin_endurance_shutdown();

        let reference_output = self.reference_output.finish_endurance_shutdown(deadline);
        let session_was_open = self.authoring.is_some();
        let pending_close_was_active = self.pending_project_close.is_some();
        let mut lifecycle_failure = None;
        if session_was_open && self.pending_project_close.is_none() {
            if self.project_close_fault.is_some() {
                lifecycle_failure = Some(
                    "Project close entered endurance shutdown with poisoned persistence authority"
                        .to_owned(),
                );
            } else if let Err(error) = self.begin_project_close() {
                lifecycle_failure = Some(error.to_string());
            }
        }
        while lifecycle_failure.is_none() && self.pending_project_close.is_some() {
            match self.poll_project_close() {
                ProjectClosePoll::Pending => {
                    if Instant::now() >= deadline {
                        lifecycle_failure = Some(
                            "Project persistence handoff exceeded the shared App shutdown deadline"
                                .to_owned(),
                        );
                    } else {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                ProjectClosePoll::Closed => break,
                ProjectClosePoll::Inactive => {
                    if self.authoring.is_some() {
                        lifecycle_failure = Some(
                            "Project close became inactive before releasing its authoring Session"
                                .to_owned(),
                        );
                    }
                    break;
                }
                ProjectClosePoll::SaveRejected(reason) | ProjectClosePoll::Faulted(reason) => {
                    lifecycle_failure = Some(reason);
                }
            }
        }

        let audio = self.shutdown_audio_playback_until(deadline);
        let export = self.render_queue.shutdown_until(deadline);
        let proxy_generation = self.proxy_generation.finish_endurance_shutdown(deadline);
        let media_import = self.media_import.finish_endurance_shutdown(deadline);
        let media_asset_mutation = self.media_asset_mutations.finish_endurance_shutdown(deadline);
        let visual_tracking = self.visual_tracking.finish_endurance_shutdown(deadline);
        let audio_idle_warmup = self.audio_idle_warmup.finish_endurance_shutdown(deadline);
        let execution_memory_observer =
            self.execution_resources.finish_endurance_shutdown(deadline);

        let replacement_cache = Arc::new(AudioSourceCache::shutdown_placeholder(
            self.audio_sample_rate,
        ));
        let source_cache = std::mem::replace(&mut self.audio_source_cache, replacement_cache);
        let strong_references_before_consumption =
            saturating_usize_to_u32(Arc::strong_count(&source_cache));
        let audio_source_cache = match Arc::try_unwrap(source_cache) {
            Ok(cache) => AppAudioSourceCacheShutdownEvidence {
                strong_references_before_consumption,
                strong_references_remaining: 0,
                cache: Some(cache.shutdown_until(deadline)),
            },
            Err(cache) => {
                let strong_references_remaining =
                    saturating_usize_to_u32(Arc::strong_count(&cache).saturating_sub(1));
                drop(cache);
                AppAudioSourceCacheShutdownEvidence {
                    strong_references_before_consumption,
                    strong_references_remaining,
                    cache: None,
                }
            }
        };

        let project_persistence = self.project_persistence.finish_endurance_shutdown(deadline);
        self.collect_released_project_libraries();
        let project = AppProjectShutdownEvidence {
            session_was_open,
            pending_close_was_active,
            authoring_session_released: self.authoring.is_none(),
            pending_close_released: self.pending_project_close.is_none(),
            runtime_lease_released: self.project_runtime_lease.is_none(),
            retired_library_generations_remaining: saturating_usize_to_u32(
                self.retired_project_libraries.len(),
            ),
            lifecycle_failure,
        };
        let mut evidence = AppEnduranceShutdownEvidence {
            app_owner_consumed: false,
            project,
            reference_output,
            export,
            audio,
            audio_source_cache,
            execution_memory_observer,
            project_persistence,
            audio_idle_warmup,
            media_import,
            media_asset_mutation,
            visual_tracking,
            proxy_generation,
        };
        drop(self);
        evidence.app_owner_consumed = true;
        evidence
    }
}

const fn saturating_usize_to_u32(value: usize) -> u32 {
    if value > u32::MAX as usize {
        u32::MAX
    } else {
        value as u32
    }
}

const fn saturating_usize_to_u64(value: usize) -> u64 {
    if value > u64::MAX as usize {
        u64::MAX
    } else {
        value as u64
    }
}

#[cfg(any(test, feature = "validation"))]
fn checked_usize_to_u64(value: usize, field: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{field} exceeded u64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_deadline_reports_normal_panic_and_detach_exactly() {
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let blocked_release = std::sync::Arc::clone(&release);
        let mut handles = vec![
            std::thread::spawn(|| {}),
            std::thread::spawn(|| panic!("injected worker panic")),
            std::thread::spawn(move || {
                while !blocked_release.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::yield_now();
                }
            }),
        ];
        let outcome = join_workers_until(&mut handles, Instant::now() + Duration::from_millis(50));
        release.store(true, std::sync::atomic::Ordering::Release);
        let evidence = EnduranceWorkerShutdownEvidence::from_join(3, true, outcome, 0, 0, 1, 0);
        assert_eq!(evidence.started_workers, 3);
        assert_eq!(evidence.terminated_workers, 1);
        assert_eq!(evidence.panicked_workers, 1);
        assert_eq!(evidence.timed_out_workers, 1);
        assert_eq!(evidence.detached_workers, 1);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn fresh_app_consuming_shutdown_closes_every_eager_owner() {
        let state = AppState::new();
        let evidence = state.shutdown_for_endurance(
            Instant::now()
                .checked_add(Duration::from_secs(10))
                .expect("fresh App shutdown deadline"),
        );

        assert!(evidence.app_owner_consumed);
        assert!(evidence.project.all_resources_released());
        assert!(evidence.reference_output.all_resources_released());
        assert_eq!(evidence.export.schema_version, 4);
        assert!(evidence.export.worker_started);
        assert!(!evidence.export.worker_start_failed);
        assert!(evidence.export.worker_terminated);
        assert!(!evidence.export.worker_panicked);
        assert!(!evidence.export.worker_timed_out);
        assert!(!evidence.export.worker_detached);
        assert!(!evidence.export.worker_owner_abandoned);
        assert_eq!(evidence.export.pending_jobs, 0);
        assert_eq!(evidence.export.active_jobs, 0);
        assert!(evidence.audio.all_workers_terminated());
        assert!(evidence.audio_source_cache.all_resources_released());
        assert!(evidence.project_persistence.lifecycle_closed());
        assert!(evidence.audio_idle_warmup.lifecycle_closed());
        assert!(evidence.media_import.lifecycle_closed());
        assert!(evidence.media_asset_mutation.lifecycle_closed());
        assert!(evidence.visual_tracking.lifecycle_closed());
        assert!(evidence.execution_memory_observer.lifecycle_closed());
        assert!(evidence.proxy_generation.lifecycle_closed());
        assert!(evidence.all_resources_released());

        for eager in [
            evidence.project_persistence,
            evidence.audio_idle_warmup,
            evidence.media_import,
            evidence.media_asset_mutation,
            evidence.visual_tracking,
        ] {
            assert!(eager.startup_attempted);
            assert_eq!(eager.started_workers, eager.requested_workers);
            assert_eq!(eager.terminated_workers, eager.started_workers);
        }

        assert!(!evidence.execution_memory_observer.startup_attempted);
        assert!(!evidence.proxy_generation.startup_attempted);
        assert_eq!(
            evidence
                .background_terminal_snapshot()
                .expect("fresh App terminal background snapshot"),
            AppBackgroundEnduranceSnapshot::default()
        );
    }
}
