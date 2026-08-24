//! Structured preview diagnostics and fail-closed performance/color reports.

mod color_health;
pub use color_health::*;
mod performance;
pub(super) use performance::classify_preview_decode_bottleneck;
use performance::classify_preview_render_bottleneck;
pub use performance::{
    build_preview_decode_performance_report,
    build_preview_decode_performance_report_with_required_access_modes,
    build_preview_render_performance_report,
};

use super::*;
use std::path::PathBuf;

/// Aggregated CPU-side viewer render stage timings after media decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewRenderStageDurations {
    /// Time spent resolving sequence elements, media cache keys, and current-frame readiness.
    pub resolve_us: u64,
    /// Time spent checking external/final viewer frame caches.
    pub final_cache_lookup_us: u64,
    /// Time spent preparing working-space inputs for CPU composition.
    pub working_prepare_us: u64,
    /// Time spent in CPU timeline compositing and basic property/effect application.
    pub cpu_composite_us: u64,
    /// Time spent applying the final CPU output/color boundary.
    pub cpu_output_boundary_us: u64,
    /// Time spent hashing, packaging, and storing the final raster viewer frame.
    pub frame_packaging_us: u64,
}

impl PreviewRenderStageDurations {
    pub(crate) fn from_cpu_execution(durations: PreviewCpuExecutionDurations) -> Self {
        Self {
            working_prepare_us: durations.working_prepare_us,
            cpu_composite_us: durations.cpu_composite_us,
            cpu_output_boundary_us: durations.cpu_output_boundary_us,
            ..Self::default()
        }
    }

    pub(crate) fn accumulate_cpu_execution(&mut self, durations: PreviewCpuExecutionDurations) {
        self.accumulate(Self::from_cpu_execution(durations));
    }

    pub(crate) fn accumulate(&mut self, other: Self) {
        self.resolve_us = self.resolve_us.saturating_add(other.resolve_us);
        self.final_cache_lookup_us =
            self.final_cache_lookup_us.saturating_add(other.final_cache_lookup_us);
        self.working_prepare_us = self.working_prepare_us.saturating_add(other.working_prepare_us);
        self.cpu_composite_us = self.cpu_composite_us.saturating_add(other.cpu_composite_us);
        self.cpu_output_boundary_us =
            self.cpu_output_boundary_us.saturating_add(other.cpu_output_boundary_us);
        self.frame_packaging_us = self.frame_packaging_us.saturating_add(other.frame_packaging_us);
    }
}

/// Playback-clock scheduling contract observed by the app preview service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewPlaybackScheduleDiagnostics {
    /// Remaining budget sampled from the absolute current Frame Demand deadline.
    pub last_current_deadline_budget_us: Option<u64>,
    /// Current playback requests that received a display deadline.
    pub current_deadline_assignments: u64,
    /// Current playback requests whose frame rate could not produce a valid deadline.
    pub current_deadline_missing_frame_rate: u64,
    /// Current playback frames admitted for decode under the playback clock.
    pub current_decode_decisions: u64,
    /// Current playback frames dropped because their display deadline was missed.
    pub current_drop_late_decisions: u64,
    /// Current playback frame requests skipped while another realtime decode was already pending.
    pub current_sustained_pressure_skips: u64,
    /// Current playback frames that should drive proxy or hardware-decode work.
    pub current_proxy_or_hardware_recommended_decisions: u64,
    /// Current playback frames whose native GPU residency path was blocked at renderer import.
    pub current_native_import_unavailable_decisions: u64,
    /// Current playback frames that requested hardware decode but produced no effective hardware frame.
    pub current_hardware_fallback_not_engaged_decisions: u64,
    /// Current playback path resolutions that requested app-layer proxy generation.
    pub current_proxy_generation_requests: u64,
    /// Current playback proxy generation candidates already queued for the same source revision.
    pub current_proxy_generation_request_dedupes: u64,
    /// Consecutive current playback frames dropped or expired before a successful current frame.
    pub current_late_streak: u64,
    /// Whether playback is currently suppressing prefetch to recover from sustained late frames.
    pub sustained_pressure_active: bool,
    /// Times playback entered sustained pressure recovery.
    pub sustained_pressure_events: u64,
    /// Times a successful current playback frame exited sustained pressure recovery.
    pub sustained_pressure_recoveries: u64,
    /// Playback prefetch passes skipped while sustained pressure recovery was active.
    pub prefetch_skipped_sustained_pressure: u64,
    /// Wall-clock horizon used to derive the forward prefetch window.
    pub forward_prefetch_horizon_us: u64,
    /// Most recent forward prefetch window derived from sequence frame rate.
    pub last_forward_prefetch_window_frames: Option<usize>,
    /// Minimum allowed forward prefetch window.
    pub forward_prefetch_min_frames: usize,
    /// Maximum allowed forward prefetch window.
    pub forward_prefetch_max_frames: usize,
    /// Playback prefetch passes whose frame rate produced a valid dynamic window.
    pub forward_prefetch_window_evaluations: u64,
    /// Playback prefetch passes skipped because frame rate could not produce a valid window.
    pub forward_prefetch_invalid_frame_rate: u64,
}

/// Playback hardware-decode admission selected by the app preview scheduler.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewHardwareDecodeAdmissionDiagnostics {
    /// Request that will be attached to playback decode jobs.
    pub playback_request: PreviewHardwareDecodeRequest,
    /// Whether renderer native decoded-frame import support reached preview scheduling.
    pub renderer_native_import_support_known: bool,
    /// Whether the renderer reports native decoded-frame import support.
    pub renderer_native_import_ready: bool,
    /// Physical transfer mode implemented by the active Renderer backend.
    pub renderer_import_mode: Option<mondrian_renderer::GpuNativeDecodedFrameImportMode>,
    /// Whether playback is allowed to request GPU-resident decode.
    pub native_import_admission_ready: bool,
    /// Stable reason playback cannot request GPU-resident decode, when gated.
    pub admission_blocker:
        Option<crate::app::native_video_import::PreviewHardwareDecodeAdmissionBlocker>,
    /// Renderer-supported native decoder handle-kind count.
    pub renderer_supported_handle_kinds: u8,
    /// Renderer-supported decoded source texture-format count.
    pub renderer_supported_source_texture_formats: u8,
}

/// Current concrete media-Adapter execution progress for the bounded Preview workers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeWorkerExecutionDiagnostics {
    /// Progress for the single general-purpose worker on small CPU budgets.
    pub any: Option<mondrian_media::PreviewDecodeExecutionProgress>,
    /// Progress for the continuous Playback worker when two workers are available.
    pub playback: Option<mondrian_media::PreviewDecodeExecutionProgress>,
    /// Progress for the shared Interactive/Still worker when two workers are available.
    pub non_playback: Option<mondrian_media::PreviewDecodeExecutionProgress>,
}

/// Cloneable read authority for the bounded Preview workers' media progress.
///
/// This watch contains observer handles only. It does not retain the Preview
/// Runtime, own worker lifecycle, or grant cancellation/recovery authority, so
/// acceptance infrastructure may sample it from a separate thread even when
/// the main test or presentation thread is blocked.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewDecodeWorkerExecutionWatch {
    observers: Vec<(MediaPreviewWorkerLane, PreviewDecodeExecutionObserver)>,
}

impl PreviewDecodeWorkerExecutionWatch {
    pub(super) fn new(
        observers: Vec<(MediaPreviewWorkerLane, PreviewDecodeExecutionObserver)>,
    ) -> Self {
        Self { observers }
    }

    /// Capture one coherent point-in-time snapshot without touching Runtime state.
    pub(crate) fn snapshot(&self) -> PreviewDecodeWorkerExecutionDiagnostics {
        let mut snapshot = PreviewDecodeWorkerExecutionDiagnostics::default();
        for (lane, observer) in &self.observers {
            let progress = Some(observer.snapshot());
            match lane {
                MediaPreviewWorkerLane::Any => snapshot.any = progress,
                MediaPreviewWorkerLane::Playback => snapshot.playback = progress,
                MediaPreviewWorkerLane::NonPlayback => snapshot.non_playback = progress,
            }
        }
        snapshot
    }
}

impl PreviewHardwareDecodeAdmissionDiagnostics {
    fn playback_native_import_gated(self) -> bool {
        self.renderer_native_import_support_known
            && !self.native_import_admission_ready
            && self.admission_blocker.is_some()
    }
}

/// Bounded future-media lowering and source-revalidation evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewFutureMediaWindowDiagnostics {
    /// Retained immutable frame contracts reused after live source revalidation.
    pub cache_hits: u64,
    /// Canonical future-frame semantic evaluations performed on cache misses.
    pub semantic_frame_evaluations: u64,
    /// Media demands lowered into immutable physical request contracts.
    pub media_request_lowerings: u64,
    /// Complete window identities retired by authoring, execution-policy, or lifecycle edges.
    pub identity_invalidations: u64,
    /// Live file-revision observations used to authorize retained contracts.
    ///
    /// One planning turn observes each unique physical path at most once. A
    /// later turn must observe the path again.
    pub source_fingerprint_observations: u64,
}

/// Point-in-time preview service counters for local performance diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDiagnostics {
    /// Complete Preview resource decisions applied at the Host policy Seam.
    pub resource_decision_applications: u64,
    /// Bounded prepared visual Program residency and author-binding evidence.
    pub visual_program_cache: PreparedVisualProgramCacheDiagnostics,
    /// Bounded future-media lowering and source-revalidation evidence.
    pub future_media_window: PreviewFutureMediaWindowDiagnostics,
    /// Whether the sole visual execution worker terminated unexpectedly.
    pub visual_execution_health_failed: bool,
    /// Viewer preview render requests received by the service.
    pub render_requests: u64,
    /// Requests that produced a current ready frame.
    pub ready_frames: u64,
    /// Requests waiting for the first current frame.
    pub loading_frames: u64,
    /// Requests that reused a scoped previous frame while the current frame is pending.
    pub stale_frames: u64,
    /// Requests with no renderable preview frame.
    pub unavailable_frames: u64,
    /// Typed aggregate evidence for unavailable GPU candidates and final presentations.
    pub unavailability: crate::app::preview_unavailability::PreviewUnavailabilityEvidenceSnapshot,
    /// Playback current-frame requests expired so buffering cannot hold the shell indefinitely.
    pub playback_current_stalled_expirations: u64,
    /// Authoritative timeline resolutions through the evaluation coordinator;
    /// deduplicated acquires and wait hits do not count.
    pub timeline_resolve_count: u64,
    /// Acquires satisfied by the evaluation working set without resolving.
    pub timeline_evaluation_hits: u64,
    /// Acquires satisfied by a retained typed wait entry without resolving.
    pub timeline_evaluation_wait_hits: u64,
    /// Acquires that missed both working set and wait entries.
    pub timeline_evaluation_misses: u64,
    /// Requests for a CPU working-frame candidate for the app-window GPU output path.
    pub gpu_preview_candidate_requests: u64,
    /// GPU preview candidate requests that produced a working-frame candidate.
    pub gpu_preview_candidate_ready: u64,
    /// GPU preview candidate requests skipped because the matching external texture is current.
    pub gpu_preview_candidate_current: u64,
    /// Exact transparent-canvas candidates requiring no texture execution.
    pub gpu_preview_candidate_transparent: u64,
    /// GPU preview candidate requests waiting on pending media.
    pub gpu_preview_candidate_loading: u64,
    /// GPU preview candidate requests with no renderable frame.
    pub gpu_preview_candidate_unavailable: u64,
    /// Pixels in working-frame candidates handed to the app-window GPU output path.
    pub gpu_preview_candidate_pixels: u64,
    /// External GPU preview frames accepted into the preview service.
    pub gpu_preview_external_frames_registered: u64,
    /// External GPU preview frames rejected before becoming viewer content.
    pub gpu_preview_external_frames_rejected: u64,
    /// External GPU preview frames cleared by the app-window output path.
    pub gpu_preview_external_frames_cleared: u64,
    /// Media input color resolutions that used a clip/media override.
    pub input_color_resolution_override: u64,
    /// Media input color resolutions that used detected media metadata.
    pub input_color_resolution_detected_metadata: u64,
    /// Media input color resolutions that assumed Rec.709 through missing-metadata policy.
    pub input_color_resolution_missing_assume_rec709: u64,
    /// Media input color resolutions rejected by missing-metadata policy.
    pub input_color_resolution_missing_rejected: u64,
    /// Media input color resolutions that treated the asset as non-color data.
    pub input_color_resolution_data_texture: u64,
    /// Media preview path resolutions that used an existing proxy file.
    pub media_proxy_path_hits: u64,
    /// Media preview path resolutions that wanted a proxy but fell back to source.
    pub media_proxy_path_misses: u64,
    /// Media preview path resolutions that rejected a stale proxy file.
    pub media_proxy_path_stale: u64,
    /// Media preview path resolutions that intentionally used source media.
    pub media_proxy_path_bypasses: u64,
    /// Playback path resolutions that requested app-layer proxy generation.
    pub media_proxy_generation_requests: u64,
    /// Playback proxy generation candidates already queued for the same source revision.
    pub media_proxy_generation_request_dedupes: u64,
    /// Scrub current-frame requests that used normal decode policy.
    pub scrub_adaptive_normal_requests: u64,
    /// Scrub current-frame requests that tightened policy for a hot seek region.
    pub scrub_adaptive_hot_region_requests: u64,
    /// Scrub current-frame requests that tightened policy because scrub latency is slow.
    pub scrub_adaptive_slow_latency_requests: u64,
    /// Scrub current-frame requests that used conservative recovery policy after slow latency.
    pub scrub_adaptive_recovery_requests: u64,
    /// Media preview cache hits.
    pub media_cache_hits: u64,
    /// Media preview cache misses.
    pub media_cache_misses: u64,
    /// Requests skipped because a media preview key is known to have failed.
    pub media_failure_hits: u64,
    /// Last exhaustive admission result for a current Timeline media source.
    pub last_current_media_admission: Option<&'static str>,
    /// Current immutable Preview execution generation.
    pub preview_execution_generation: u64,
    /// Exact current-candidate bindings waiting on reused Broker work.
    pub media_existing_work_waiters: usize,
    /// Existing-work retry authority retained until candidate evaluation acknowledges it.
    pub media_existing_work_retry_pending: bool,
    /// Current media requests that registered exact existing-work waiters.
    pub media_existing_work_waiter_registrations: u64,
    /// Retained existing-work retries acknowledged by real candidate evaluation.
    pub media_existing_work_retry_acknowledgements: u64,
    /// Last exhaustive reason the GPU candidate returned Loading.
    pub last_gpu_loading_reason: Option<&'static str>,
    /// Coordinated CPU budget used for preview workers and FFmpeg decoder threads.
    pub decode_cpu_budget: PreviewDecodeCpuBudget,
    /// Preview decode workers successfully started for this service.
    pub decode_worker_count: usize,
    /// Whether every configured media result producer terminated before shutdown.
    pub media_worker_health_failed: bool,
    /// Lock-free point-in-time execution stage for every bounded decode worker.
    pub decode_worker_execution: PreviewDecodeWorkerExecutionDiagnostics,
    /// Latest decoder-residency phase revision.
    pub decode_residency_revision: u64,
    /// Completed retirement barriers available to Runtime retry projection.
    pub decode_residency_actionable_retry_revision: u64,
    /// Latest retirement-barrier retry revision projected by this Runtime.
    pub decode_residency_observed_retry_revision: u64,
    /// Active decoder-residency family (`playback` or `interactive`).
    pub decode_residency_family: Option<&'static str>,
    /// Playback/interactive residency transitions published by the Runtime.
    pub decode_residency_transitions: u64,
    /// Decode admissions held while the opposite worker family retired.
    pub decode_residency_blocked_admissions: u64,
    /// Worker retirements required by the active phase revision.
    pub decode_residency_required_acknowledgements: u32,
    /// Required worker retirements completed for the active phase revision.
    pub decode_residency_acknowledged_retirements: u32,
    /// App-level playback hardware-decode admission state.
    pub hardware_decode_admission: PreviewHardwareDecodeAdmissionDiagnostics,
    /// Successful background media decodes received by the production Runtime.
    pub decode_successes: u64,
    /// Successful startup-preroll media decodes excluded from steady-state latency budgets.
    pub decode_startup_preroll_frames: u64,
    /// Total startup-preroll decode execution time.
    pub decode_startup_preroll_total_duration_us: u64,
    /// Slowest startup-preroll decode execution time.
    pub decode_startup_preroll_max_duration_us: u64,
    /// Total queue wait for startup-preroll decode work.
    pub decode_startup_preroll_queue_wait_total_us: u64,
    /// Slowest queue wait for startup-preroll decode work.
    pub decode_startup_preroll_queue_wait_max_us: u64,
    /// Failed background media decodes received by the production Runtime.
    pub decode_failures: u64,
    /// Failed background media decodes caused by a structured decode timeout.
    pub decode_timeout_failures: u64,
    /// Failed background media decodes caused by access-mode forward-scan budget exhaustion.
    pub decode_budget_exhausted_failures: u64,
    /// Playback-owned cancellation evidence from all semantic frame-work classes.
    pub decode_cancellation: mondrian_playback::FrameCancellationEvidenceReport,
    /// Concrete media Adapter mechanisms and checkpoints that observed decode cancellation.
    pub decode_cancellation_checkpoints: mondrian_media::PreviewDecodeCancellationEvidence,
    /// Background media decodes canceled before producing a frame.
    pub decode_canceled_jobs: u64,
    /// Background media decodes canceled because the preview service is shutting down.
    pub decode_canceled_shutdown_jobs: u64,
    /// Background media decodes canceled because their pending request became obsolete.
    pub decode_canceled_obsolete_jobs: u64,
    /// Prefetch decodes canceled by the app-level prefetch deadline.
    pub decode_canceled_prefetch_deadline_jobs: u64,
    /// Current playback decodes canceled because their display deadline was missed.
    pub decode_canceled_playback_deadline_jobs: u64,
    /// Prefetch decodes canceled because visible current-frame work was pending.
    pub decode_canceled_prefetch_preempted_jobs: u64,
    /// Still-frame decodes canceled because realtime current-frame work was pending.
    pub decode_canceled_still_preempted_jobs: u64,
    /// Background media decodes canceled without a structured app-level reason.
    pub decode_canceled_unknown_jobs: u64,
    /// Total worker execution time spent in canceled decode jobs.
    pub decode_canceled_total_duration_us: u64,
    /// Slowest worker execution time for a canceled decode job.
    pub decode_canceled_max_duration_us: u64,
    /// Most recent worker execution time for a canceled decode job.
    pub decode_canceled_last_duration_us: u64,
    /// Canceled jobs with attributable request-to-logical-cancellation timing.
    pub decode_cancel_observation_samples: u64,
    /// Total authority-request to `LogicalCancellationObserved` latency.
    pub decode_cancel_observation_total_us: u64,
    /// Slowest authority-request to `LogicalCancellationObserved` latency.
    pub decode_cancel_observation_max_us: u64,
    /// Most recent authority-request to `LogicalCancellationObserved` latency.
    pub decode_cancel_observation_last_us: u64,
    /// Total latency after canceled decode jobs first observed cancellation.
    pub decode_canceled_return_latency_total_us: u64,
    /// Slowest latency after a canceled decode job first observed cancellation.
    pub decode_canceled_return_latency_max_us: u64,
    /// Most recent latency after a canceled decode job first observed cancellation.
    pub decode_canceled_return_latency_last_us: u64,
    /// Successful decodes produced by in-process FFmpeg CPU RGBA8/f32 paths.
    pub decode_in_process_cpu_frames: u64,
    /// Successful decodes produced by the external ffmpeg CPU RGBA path.
    pub decode_external_ffmpeg_cpu_rgba_frames: u64,
    /// Successful playback decodes served from the playback session-local ring.
    pub decode_playback_session_ring_hit_frames: u64,
    /// Successful decodes served from a decoder-session-local cache.
    ///
    /// This currently aliases playback-session ring hits. App-owned Preview
    /// Frame Store hits bypass media decode and therefore never enter this
    /// counter.
    pub decode_cache_hit_frames: u64,
    /// Decode results requested through the playback cursor access contract.
    pub decode_playback_cursor_frames: u64,
    /// Decode results requested through the scrub cursor access contract.
    pub decode_scrub_cursor_frames: u64,
    /// Decode results requested through the random-access still-frame contract.
    pub decode_random_access_still_frames: u64,
    /// Canceled decode jobs requested through the playback cursor access contract.
    pub decode_canceled_playback_cursor_jobs: u64,
    /// Canceled decode jobs requested through the scrub cursor access contract.
    pub decode_canceled_scrub_cursor_jobs: u64,
    /// Canceled decode jobs requested through the random-access still-frame contract.
    pub decode_canceled_random_access_still_jobs: u64,
    /// Total preview decode duration in microseconds.
    pub decode_total_duration_us: u64,
    /// Slowest preview decode duration in microseconds.
    pub decode_max_duration_us: u64,
    /// Most recent successful preview decode duration in microseconds.
    pub decode_last_duration_us: u64,
    /// Total time Ready jobs spent waiting in the preview worker queue.
    pub decode_queue_wait_total_us: u64,
    /// Slowest Ready job queue wait.
    pub decode_queue_wait_max_us: u64,
    /// Most recent Ready job queue wait.
    pub decode_queue_wait_last_us: u64,
    /// Slowest Ready current-frame decode queue wait.
    pub decode_current_queue_wait_max_us: u64,
    /// Slowest Ready prefetch decode queue wait.
    pub decode_prefetch_queue_wait_max_us: u64,
    /// Queue wait for jobs rejected as expired before codec execution.
    pub decode_expired_queue_wait: PreviewDecodeQueueWaitProfile,
    /// Decode requests that required a decoder seek before frame selection.
    pub decode_seeked_frames: u64,
    /// Total decoded frames consumed by preview decode requests.
    pub decode_decoded_frame_count: u64,
    /// Largest decoded-frame count consumed by one preview decode request.
    pub decode_max_decoded_frame_count: u64,
    /// Decode results that reported no FFmpeg decoder threading.
    pub decode_threading_none_frames: u64,
    /// Decode results that reported frame-level FFmpeg decoder threading.
    pub decode_threading_frame_frames: u64,
    /// Decode results that reported slice-level FFmpeg decoder threading.
    pub decode_threading_slice_frames: u64,
    /// Most recent FFmpeg decoder thread count reported by preview decode.
    pub decode_last_threading_count: u64,
    /// Largest FFmpeg decoder thread count reported by preview decode.
    pub decode_max_threading_count: u64,
    /// Aggregated stage-level timings reported by preview decode.
    pub decode_stage_durations: PreviewDecodeStageDurations,
    /// Stage-level timings from the slowest decoded preview frame.
    pub decode_max_frame_stage_durations: PreviewDecodeStageDurations,
    /// Queue wait observed by the same decoded preview frame that produced
    /// `decode_max_frame_stage_durations`.
    pub decode_max_frame_queue_wait_us: u64,
    /// Dominant bottleneck for the same decoded preview frame that produced
    /// `decode_max_frame_stage_durations`.
    pub decode_max_frame_bottleneck: PreviewDecodeBottleneck,
    /// Decode profile split by playback, scrub, and random-access still modes.
    pub decode_access_mode_profiles: PreviewDecodeAccessModeProfiles,
    /// Viewer render requests with post-decode stage timing evidence.
    pub render_timed_frames: u64,
    /// Total post-decode viewer render duration in microseconds.
    pub render_total_duration_us: u64,
    /// Slowest post-decode viewer render duration in microseconds.
    pub render_max_duration_us: u64,
    /// Most recent post-decode viewer render duration in microseconds.
    pub render_last_duration_us: u64,
    /// Aggregated CPU-side viewer render stage timings after media decode.
    pub render_stage_durations: PreviewRenderStageDurations,
    /// CPU-side viewer render stage timings from the slowest post-decode frame.
    pub render_max_frame_stage_durations: PreviewRenderStageDurations,
    /// Foreground completion polling passes for decoded preview results.
    pub completion_poll_calls: u64,
    /// Decoded preview results processed by foreground completion polling.
    pub completion_poll_results: u64,
    /// Total foreground completion polling duration in microseconds.
    pub completion_poll_total_duration_us: u64,
    /// Slowest foreground completion polling pass in microseconds.
    pub completion_poll_max_duration_us: u64,
    /// Most recent foreground completion polling pass in microseconds.
    pub completion_poll_last_duration_us: u64,
    /// Largest configured completion-result count budget observed by diagnostics.
    pub completion_poll_max_results_per_poll: u64,
    /// Completion polling passes that stopped at the result-count budget.
    pub completion_poll_count_budget_exhaustions: u64,
    /// Completion polling passes that yielded after the foreground time budget.
    pub completion_poll_time_budget_exhaustions: u64,
    /// Media preview jobs accepted by the worker queue.
    pub enqueued_jobs: u64,
    /// Playback prefetch passes skipped because visible current-frame media was pending.
    pub prefetch_skipped_current_pending: u64,
    /// Prefetch passes that observed queued or in-flight current media work.
    ///
    /// The serialized field name predates pipelined admission. Current work
    /// remains first in Broker order, but does not itself suppress a
    /// resource-admissible future prefix.
    pub prefetch_skipped_current_work: u64,
    /// Playback prefetch passes skipped because queued/running prefetch already held the window.
    pub prefetch_skipped_prefetch_backlog: u64,
    /// Media preview jobs dropped because the bounded worker queue was full.
    pub queue_full_drops: u64,
    /// Media preview jobs rejected by the worker queue for invalid priority/access-mode pairs.
    pub queue_invalid_access_mode_drops: u64,
    /// Queued prefetch jobs evicted so current-frame decode work can run.
    pub queue_evicted_prefetch_jobs: u64,
    /// Queued still-frame jobs evicted so real-time current work can run.
    pub queue_evicted_still_jobs: u64,
    /// User escape-path cancellations requested by transport, close, or quit.
    pub interactive_cancel_requests: u64,
    /// Scheduler pending requests canceled by user escape-path cancellations.
    pub interactive_cancel_scheduler_requests: u64,
    /// Worker-queue jobs cleared by user escape-path cancellations.
    pub interactive_cancel_queued_jobs: u64,
    /// Queued jobs removed because their scheduler-side pending request was canceled.
    pub queue_canceled_jobs: u64,
    /// Obsolete queued jobs removed before scheduling current-frame decode.
    pub queue_pruned_obsolete_jobs: u64,
    /// Queued prefetch jobs promoted after the same key became current-frame work.
    pub queue_promoted_current_jobs: u64,
    /// Media preview jobs dropped because the worker channel was disconnected.
    pub worker_disconnected_drops: u64,
    /// Current worker transport queue depth grouped by scheduling contract.
    pub worker_queue: MediaPreviewJobQueueDiagnostics,
    /// Scheduler-side request, drop, completion, and pruning counters.
    pub scheduler: MediaPreviewSchedulerDiagnostics,
    /// Playback-clock deadline and forward-prefetch scheduling contract.
    pub playback_schedule: PreviewPlaybackScheduleDiagnostics,
    /// Final viewer preview frame cache hits.
    pub viewer_frame_cache_hits: u64,
    /// Final viewer preview frame cache misses.
    pub viewer_frame_cache_misses: u64,
    /// Playback-owned physical Preview Frame Store ledger and cache evidence.
    ///
    /// Consumers must evaluate optional cache residency separately from the
    /// aggregate hard current-working-set grant. App diagnostics do not
    /// reconstruct or rename that capacity authority.
    pub frame_store: mondrian_playback::PreviewFrameStoreDiagnostics,
    /// Persistent post-composite Timeline cache queue and disk evidence.
    pub timeline_render_cache: mondrian_render_cache::TimelineRenderCacheDiagnostics,
    /// Whether the optional persistent cache worker could not start.
    pub timeline_render_cache_start_failed: bool,
    /// Source/media color transforms into the timeline working space.
    pub color_input_transform_calls: u64,
    /// Pixels processed by source/media color transforms into the timeline working space.
    pub color_input_transform_pixels: u64,
    /// Timeline working-space transforms into preview/output encoding.
    pub color_output_transform_calls: u64,
    /// Pixels processed by timeline working-space transforms into preview/output encoding.
    pub color_output_transform_pixels: u64,
    /// Internal color transforms such as program-output to monitor adaptation.
    pub color_intermediate_transform_calls: u64,
    /// Pixels processed by internal color transforms.
    pub color_intermediate_transform_pixels: u64,
    /// Color transforms that crossed the temporary RGBA8 CPU boundary.
    pub color_rgba8_boundary_calls: u64,
    /// Render color stage plans executed by preview.
    pub color_stage_plans: u64,
    /// Render color stages executed by preview.
    pub color_stage_total_stages: u64,
    /// CPU source/media input stages executed by preview.
    pub color_stage_cpu_input_stages: u64,
    /// CPU working-to-output stages executed by preview.
    pub color_stage_cpu_output_stages: u64,
    /// GPU color stages planned by preview.
    pub color_stage_gpu_color_stages: u64,
    /// CPU-to-GPU upload stages planned by preview.
    pub color_stage_upload_stages: u64,
    /// GPU-to-CPU readback stages planned by preview.
    pub color_stage_readback_stages: u64,
    /// GPU color stage blockers surfaced by preview.
    pub color_stage_gpu_blockers: u64,
    /// GPU blockers caused by missing shader modules.
    pub color_stage_gpu_shader_module_blockers: u64,
    /// GPU blockers caused by missing OCIO LUT/uniform bind groups.
    pub color_stage_gpu_ocio_resource_blockers: u64,
    /// GPU blockers caused by missing fullscreen wrappers.
    pub color_stage_gpu_wrapper_blockers: u64,
    /// GPU blockers caused by missing render pipelines.
    pub color_stage_gpu_render_pipeline_blockers: u64,
    /// GPU blockers caused by missing OCIO config.
    pub color_stage_gpu_ocio_config_blockers: u64,
    /// GPU blockers caused by unavailable OCIO processor.
    pub color_stage_gpu_ocio_processor_blockers: u64,
    /// GPU blockers caused by failed OCIO GPU shader extraction.
    pub color_stage_gpu_ocio_shader_extraction_blockers: u64,
    /// Pixels covered by preview color stage plans.
    pub color_stage_pixels: u64,
    /// Timeline composite plans executed by preview.
    pub color_composite_plans: u64,
    /// Timeline composite elements processed by preview.
    pub color_composite_elements: u64,
    /// Composite plans that stayed on the float/linear path.
    pub color_composite_float_linear: u64,
    /// Composite plans that fell back to the legacy RGBA8 path.
    pub color_composite_legacy_rgba8: u64,
    /// Legacy RGBA8 fallbacks caused by media layer blend modes.
    pub color_composite_legacy_media_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by media layer transforms.
    pub color_composite_legacy_media_transform: u64,
    /// Legacy RGBA8 fallbacks caused by media effect graphs.
    pub color_composite_legacy_media_effect: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer blend modes.
    pub color_composite_legacy_solid_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer transforms.
    pub color_composite_legacy_solid_transform: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer effects.
    pub color_composite_legacy_solid_effect: u64,
    /// Legacy RGBA8 fallbacks caused by adjustment layer blend modes.
    pub color_composite_legacy_adjustment_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by adjustment effect graphs.
    pub color_composite_legacy_adjustment_effect: u64,
    /// Composite plans blocked on unresolved effect-domain semantics.
    pub color_composite_blocked_domains: u64,
    /// Media effects blocked on unresolved effect-domain semantics.
    pub color_composite_blocked_media_effect_domain: u64,
    /// Solid effects blocked on unresolved effect-domain semantics.
    pub color_composite_blocked_solid_effect_domain: u64,
    /// Adjustment effects blocked on unresolved effect-domain semantics.
    pub color_composite_blocked_adjustment_effect_domain: u64,
    /// Number of raster preview frames that used CPU output transform fallback.
    pub cpu_output_fallback_frames: u64,
    /// Pixels processed through CPU output transform fallback.
    pub cpu_output_fallback_pixels: u64,
    /// Number of preview frames with structured GPU output blockers.
    pub preview_gpu_output_blocker_frames: u64,
    /// Structured GPU output blocker breakdown across preview frames.
    pub preview_gpu_output_blocker_breakdown:
        crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    /// GPU compositing capability diagnostics.
    pub gpu_compositing: mondrian_renderer::GpuCompositingDiagnostics,
}

/// Structured color-management rejection captured from the viewer preview path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewColorRejection {
    /// Asset that could not be interpreted for preview.
    pub asset_id: AssetId,
    /// Media path shown in diagnostics.
    pub path: PathBuf,
    /// Active missing-metadata policy that rejected the asset.
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    /// Resolution branch that produced the rejection.
    pub source: InputColorResolutionSource,
    /// Clip/media color-space override in effect, if any.
    pub override_color_space: Option<ColorSpace>,
    /// Validated metadata identity that was eligible to drive pixels.
    pub executable_color_space: Option<ColorSpace>,
    /// Sequence working color space active during the decision.
    pub working_color_space: WorkingColorSpace,
    /// Compact media color diagnostic summary from `mondrian-media`.
    pub diagnostic_summary: String,
    /// Machine-readable media color diagnostic issue summary.
    pub diagnostic_issue_summary: VideoColorDiagnosticIssueSummary,
}

/// Stable preview color-path health summary for perf JSONL and diagnostics tooling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewColorHealthSummary {
    /// Preview composite plans represented by this snapshot.
    pub composite_plans: u64,
    /// Inputs resolved from detected media metadata.
    pub detected_metadata: u64,
    /// Inputs resolved from user overrides.
    pub override_count: u64,
    /// Inputs resolved by missing-metadata policy assumptions.
    pub policy_assumptions: u64,
    /// Inputs bypassing color management as data/utility textures.
    pub data_textures: u64,
    /// Inputs rejected by missing-metadata policy.
    pub policy_rejections: u64,
    /// Inputs resolved from explicit metadata or user overrides.
    pub explicit_metadata_or_override: u64,
    /// CPU input color-transform stages.
    pub cpu_input_stages: u64,
    /// CPU output/display color-transform stages.
    pub cpu_output_stages: u64,
    /// Native GPU color-transform stages.
    pub gpu_color_stages: u64,
    /// GPU scheduling blockers across native GPU color stages.
    pub gpu_blockers: u64,
    /// Structured native GPU blocker reasons.
    pub gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown,
    /// Upload/readback transfer stages around color work.
    pub transfer_stages: u64,
    /// Temporary RGBA8 CPU boundary crossings observed by preview transforms.
    pub rgba8_boundary_calls: u64,
    /// Float/linear timeline composites.
    pub float_linear_composites: u64,
    /// Legacy RGBA8 timeline composites.
    pub legacy_rgba8_composites: u64,
    /// Structured legacy RGBA8 fallback reason count.
    pub legacy_reason_total: u64,
    /// Structured legacy RGBA8 fallback reasons.
    pub legacy_breakdown: TimelineCompositeLegacyBreakdown,
    /// Composite plans blocked on unresolved effect-domain semantics.
    pub blocked_color_domain_composites: u64,
    /// Structured unresolved effect-domain reasons.
    pub domain_blockers: TimelineCompositeDomainBlockerBreakdown,
    /// Whether all diagnosed composites stayed in the float/linear path.
    pub fully_float_linear: bool,
    /// Whether native GPU color scheduling was free of upload/readback and blockers.
    pub gpu_path_ready: bool,
    /// Number of raster preview frames that used CPU output transform fallback.
    pub cpu_output_fallback_frames: u64,
    /// Pixels processed through CPU output transform fallback.
    pub cpu_output_fallback_pixels: u64,
    /// Structured GPU output blocker breakdown across preview frames.
    pub preview_gpu_output_blocker_breakdown:
        crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    /// GPU compositing capability diagnostics.
    pub gpu_compositing: mondrian_renderer::GpuCompositingDiagnostics,
}

/// Fixed latency buckets for compact preview decode distribution diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeLatencyBuckets {
    /// Samples at or below 10 ms.
    pub le_10ms: u64,
    /// Samples above 10 ms and at or below 16 ms.
    pub le_16ms: u64,
    /// Samples above 16 ms and at or below 25 ms.
    pub le_25ms: u64,
    /// Samples above 25 ms and at or below 40 ms.
    pub le_40ms: u64,
    /// Samples above 40 ms and at or below 50 ms.
    pub le_50ms: u64,
    /// Samples above 50 ms and at or below 80 ms.
    pub le_80ms: u64,
    /// Samples above 80 ms.
    pub gt_80ms: u64,
}

impl PreviewDecodeLatencyBuckets {
    fn record(&mut self, duration_us: u64) {
        match duration_us {
            0..=10_000 => self.le_10ms = self.le_10ms.saturating_add(1),
            10_001..=16_000 => self.le_16ms = self.le_16ms.saturating_add(1),
            16_001..=25_000 => self.le_25ms = self.le_25ms.saturating_add(1),
            25_001..=40_000 => self.le_40ms = self.le_40ms.saturating_add(1),
            40_001..=50_000 => self.le_50ms = self.le_50ms.saturating_add(1),
            50_001..=80_000 => self.le_80ms = self.le_80ms.saturating_add(1),
            _ => self.gt_80ms = self.gt_80ms.saturating_add(1),
        }
    }

    pub(super) fn total(self) -> u64 {
        self.le_10ms
            .saturating_add(self.le_16ms)
            .saturating_add(self.le_25ms)
            .saturating_add(self.le_40ms)
            .saturating_add(self.le_50ms)
            .saturating_add(self.le_80ms)
            .saturating_add(self.gt_80ms)
    }

    fn estimated_p95_upper_bound_us(self) -> Option<u64> {
        self.estimated_quantile_upper_bound_us(95)
    }

    fn estimated_quantile_upper_bound_us(self, percentile: u64) -> Option<u64> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        let rank = total.saturating_mul(percentile.min(100)).saturating_add(99) / 100;
        let mut cumulative = 0_u64;
        for (count, upper_bound_us) in [
            (self.le_10ms, Some(10_000)),
            (self.le_16ms, Some(16_000)),
            (self.le_25ms, Some(25_000)),
            (self.le_40ms, Some(40_000)),
            (self.le_50ms, Some(50_000)),
            (self.le_80ms, Some(80_000)),
            (self.gt_80ms, None),
        ] {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= rank {
                return upper_bound_us;
            }
        }
        None
    }
}

/// Queue-wait evidence for one broker dequeue disposition.
///
/// `Ready` and `Expired` observations use separate instances so deadline
/// cleanup that never entered codec execution cannot contaminate execution
/// admission latency gates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeQueueWaitProfile {
    /// Jobs with explicit queue-wait evidence.
    pub samples: u64,
    /// Total queue wait across the observed jobs.
    pub total_us: u64,
    /// Slowest observed queue wait.
    pub max_us: u64,
    /// Most recent observed queue wait.
    pub last_us: u64,
    /// Slowest current-frame queue wait.
    pub current_max_us: u64,
    /// Slowest prefetch queue wait.
    pub prefetch_max_us: u64,
    /// Fixed distribution buckets for queue wait.
    pub buckets: PreviewDecodeLatencyBuckets,
}

impl PreviewDecodeQueueWaitProfile {
    pub(super) fn record(&mut self, priority: MediaPreviewRequestPriority, queue_wait_us: u64) {
        self.samples = self.samples.saturating_add(1);
        self.total_us = self.total_us.saturating_add(queue_wait_us);
        self.max_us = self.max_us.max(queue_wait_us);
        self.last_us = queue_wait_us;
        match priority {
            MediaPreviewRequestPriority::Current => {
                self.current_max_us = self.current_max_us.max(queue_wait_us);
            }
            MediaPreviewRequestPriority::Prefetch => {
                self.prefetch_max_us = self.prefetch_max_us.max(queue_wait_us);
            }
        }
        self.buckets.record(queue_wait_us);
    }
}

/// Decode execution classes with materially different latency obligations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum PreviewDecodeWorkClass {
    /// Session-local forward-ring result; the decoder was bypassed.
    CacheHit,
    /// First compatible decoder Session for this access-mode lane.
    SessionOpened,
    /// Existing incompatible/retired decoder Session was replaced.
    SessionReplaced,
    /// Compatible Session continued forward without seeking.
    ForwardSteady,
    /// Compatible Session performed a seek before returning the frame.
    ReusedSeek,
    /// Compatible Session returned without a proven forward-reuse or seek classification.
    ReusedOther,
    /// Producer lifecycle facts were missing or contradictory.
    Unclassified,
}

impl PreviewDecodeWorkClass {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::CacheHit => "CacheHit",
            Self::SessionOpened => "SessionOpened",
            Self::SessionReplaced => "SessionReplaced",
            Self::ForwardSteady => "ForwardSteady",
            Self::ReusedSeek => "ReusedSeek",
            Self::ReusedOther => "ReusedOther",
            Self::Unclassified => "Unclassified",
        }
    }
}

/// Extended fixed histogram used by class-specific decode gates.
///
/// Unlike the legacy compact aggregate histogram, the overflow bucket has no
/// fabricated upper bound. Quantile evaluation returns `None` when the target
/// rank falls into that open interval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeWorkLatencyBuckets {
    /// Samples at or below 10 ms.
    pub le_10ms: u64,
    /// Samples above 10 ms and at or below 16 ms.
    pub le_16ms: u64,
    /// Samples above 16 ms and at or below 25 ms.
    pub le_25ms: u64,
    /// Samples above 25 ms and at or below 40 ms.
    pub le_40ms: u64,
    /// Samples above 40 ms and at or below 50 ms.
    pub le_50ms: u64,
    /// Samples above 50 ms and at or below 80 ms.
    pub le_80ms: u64,
    /// Samples above 80 ms and at or below 120 ms.
    pub le_120ms: u64,
    /// Samples above 120 ms and at or below 250 ms.
    pub le_250ms: u64,
    /// Samples above 250 ms and at or below 500 ms.
    pub le_500ms: u64,
    /// Samples above 500 ms and at or below 1 second.
    pub le_1s: u64,
    /// Samples above 1 second and at or below 2 seconds.
    pub le_2s: u64,
    /// Samples above 2 seconds and at or below 5 seconds.
    pub le_5s: u64,
    /// Samples above 5 seconds; this interval has no finite upper bound.
    pub gt_5s: u64,
}

impl PreviewDecodeWorkLatencyBuckets {
    fn record(&mut self, duration_us: u64) {
        match duration_us {
            0..=10_000 => self.le_10ms = self.le_10ms.saturating_add(1),
            10_001..=16_000 => self.le_16ms = self.le_16ms.saturating_add(1),
            16_001..=25_000 => self.le_25ms = self.le_25ms.saturating_add(1),
            25_001..=40_000 => self.le_40ms = self.le_40ms.saturating_add(1),
            40_001..=50_000 => self.le_50ms = self.le_50ms.saturating_add(1),
            50_001..=80_000 => self.le_80ms = self.le_80ms.saturating_add(1),
            80_001..=120_000 => self.le_120ms = self.le_120ms.saturating_add(1),
            120_001..=250_000 => self.le_250ms = self.le_250ms.saturating_add(1),
            250_001..=500_000 => self.le_500ms = self.le_500ms.saturating_add(1),
            500_001..=1_000_000 => self.le_1s = self.le_1s.saturating_add(1),
            1_000_001..=2_000_000 => self.le_2s = self.le_2s.saturating_add(1),
            2_000_001..=5_000_000 => self.le_5s = self.le_5s.saturating_add(1),
            _ => self.gt_5s = self.gt_5s.saturating_add(1),
        }
    }

    pub(super) fn total(self) -> u64 {
        [
            self.le_10ms,
            self.le_16ms,
            self.le_25ms,
            self.le_40ms,
            self.le_50ms,
            self.le_80ms,
            self.le_120ms,
            self.le_250ms,
            self.le_500ms,
            self.le_1s,
            self.le_2s,
            self.le_5s,
            self.gt_5s,
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }

    pub(super) fn p95_upper_bound_us(self) -> Option<u64> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        let rank = total.saturating_mul(95).saturating_add(99) / 100;
        let mut cumulative = 0_u64;
        for (count, upper_bound_us) in [
            (self.le_10ms, Some(10_000)),
            (self.le_16ms, Some(16_000)),
            (self.le_25ms, Some(25_000)),
            (self.le_40ms, Some(40_000)),
            (self.le_50ms, Some(50_000)),
            (self.le_80ms, Some(80_000)),
            (self.le_120ms, Some(120_000)),
            (self.le_250ms, Some(250_000)),
            (self.le_500ms, Some(500_000)),
            (self.le_1s, Some(1_000_000)),
            (self.le_2s, Some(2_000_000)),
            (self.le_5s, Some(5_000_000)),
            (self.gt_5s, None),
        ] {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= rank {
                return upper_bound_us;
            }
        }
        None
    }
}

/// Latency evidence for one access-mode/work-class cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeWorkLatencyProfile {
    /// Successful frames classified into this cell.
    pub frames: u64,
    /// Total worker execution time for successful frames.
    pub total_duration_us: u64,
    /// Slowest worker execution time for a successful frame.
    pub max_duration_us: u64,
    /// Bounded latency distribution for quantile gates.
    pub latency_buckets: PreviewDecodeWorkLatencyBuckets,
}

impl PreviewDecodeWorkLatencyProfile {
    fn record(&mut self, duration_us: u64) {
        self.frames = self.frames.saturating_add(1);
        self.total_duration_us = self.total_duration_us.saturating_add(duration_us);
        self.max_duration_us = self.max_duration_us.max(duration_us);
        self.latency_buckets.record(duration_us);
    }
}

/// Exhaustive successful-frame latency classes for one access mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeWorkClassProfiles {
    /// Decoder-bypassing ring/cache hits.
    pub cache_hit: PreviewDecodeWorkLatencyProfile,
    /// Frames that opened a new Session.
    pub session_opened: PreviewDecodeWorkLatencyProfile,
    /// Frames that replaced an incompatible/retired Session.
    pub session_replaced: PreviewDecodeWorkLatencyProfile,
    /// Forward, seek-free reuse.
    pub forward_steady: PreviewDecodeWorkLatencyProfile,
    /// Reused Session with a seek.
    pub reused_seek: PreviewDecodeWorkLatencyProfile,
    /// Reused Session without enough facts for forward/seek specialization.
    pub reused_other: PreviewDecodeWorkLatencyProfile,
    /// Missing or contradictory producer lifecycle facts.
    pub unclassified: PreviewDecodeWorkLatencyProfile,
}

impl PreviewDecodeWorkClassProfiles {
    pub(super) fn total_frames(self) -> u64 {
        [
            self.cache_hit.frames,
            self.session_opened.frames,
            self.session_replaced.frames,
            self.forward_steady.frames,
            self.reused_seek.frames,
            self.reused_other.frames,
            self.unclassified.frames,
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }

    pub(super) fn profile(self, class: PreviewDecodeWorkClass) -> PreviewDecodeWorkLatencyProfile {
        match class {
            PreviewDecodeWorkClass::CacheHit => self.cache_hit,
            PreviewDecodeWorkClass::SessionOpened => self.session_opened,
            PreviewDecodeWorkClass::SessionReplaced => self.session_replaced,
            PreviewDecodeWorkClass::ForwardSteady => self.forward_steady,
            PreviewDecodeWorkClass::ReusedSeek => self.reused_seek,
            PreviewDecodeWorkClass::ReusedOther => self.reused_other,
            PreviewDecodeWorkClass::Unclassified => self.unclassified,
        }
    }

    fn record(&mut self, class: PreviewDecodeWorkClass, duration_us: u64) {
        match class {
            PreviewDecodeWorkClass::CacheHit => self.cache_hit.record(duration_us),
            PreviewDecodeWorkClass::SessionOpened => self.session_opened.record(duration_us),
            PreviewDecodeWorkClass::SessionReplaced => self.session_replaced.record(duration_us),
            PreviewDecodeWorkClass::ForwardSteady => self.forward_steady.record(duration_us),
            PreviewDecodeWorkClass::ReusedSeek => self.reused_seek.record(duration_us),
            PreviewDecodeWorkClass::ReusedOther => self.reused_other.record(duration_us),
            PreviewDecodeWorkClass::Unclassified => self.unclassified.record(duration_us),
        }
    }
}

/// Preview decode profile for one access mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeAccessModeProfile {
    /// Successful decode/cache results for this access mode.
    pub frames: u64,
    /// Successful in-process CPU RGBA8/f32 results for this access mode.
    pub in_process_cpu_frames: u64,
    /// Successful external ffmpeg CPU RGBA results for this access mode.
    pub external_ffmpeg_cpu_rgba_frames: u64,
    /// Successful playback ring hits for this access mode.
    pub playback_session_ring_hit_frames: u64,
    /// Successful decoder-session-local cache hits for this access mode.
    pub cache_hit_frames: u64,
    /// Total end-to-end decode duration for this access mode.
    pub total_duration_us: u64,
    /// Slowest end-to-end decode duration for this access mode.
    pub max_duration_us: u64,
    /// Most recent end-to-end decode duration for this access mode.
    pub last_duration_us: u64,
    /// Fixed distribution buckets for end-to-end decode duration.
    pub latency_buckets: PreviewDecodeLatencyBuckets,
    /// Non-overlapping latency evidence by decoder work class.
    pub work_classes: PreviewDecodeWorkClassProfiles,
    /// Total worker-queue wait before decode started for this access mode.
    pub queue_wait_total_us: u64,
    /// Slowest worker-queue wait before decode started for this access mode.
    pub queue_wait_max_us: u64,
    /// Most recent worker-queue wait before decode started for this access mode.
    pub queue_wait_last_us: u64,
    /// Fixed distribution buckets for worker-queue wait.
    pub queue_wait_buckets: PreviewDecodeLatencyBuckets,
    /// Jobs with explicit worker-queue wait evidence for this access mode.
    pub queue_wait_samples: u64,
    /// Queue wait for jobs rejected as expired before codec execution.
    pub expired_queue_wait: PreviewDecodeQueueWaitProfile,
    /// Canceled decode jobs for this access mode.
    pub canceled_jobs: u64,
    /// Canceled jobs caused by preview shutdown for this access mode.
    pub canceled_shutdown_jobs: u64,
    /// Canceled jobs caused by obsolete pending work for this access mode.
    pub canceled_obsolete_jobs: u64,
    /// Canceled prefetch jobs that exceeded their deadline for this access mode.
    pub canceled_prefetch_deadline_jobs: u64,
    /// Canceled current playback jobs that missed their display deadline.
    pub canceled_playback_deadline_jobs: u64,
    /// Canceled prefetch jobs that yielded to visible current-frame work.
    pub canceled_prefetch_preempted_jobs: u64,
    /// Canceled still-frame jobs that yielded to realtime current-frame work.
    pub canceled_still_preempted_jobs: u64,
    /// Canceled jobs without a structured reason for this access mode.
    pub canceled_unknown_jobs: u64,
    /// Canceled attempts first observed while opening or probing a decoder Session.
    pub canceled_session_open_attempts: u64,
    /// Canceled requests that had opened a new decoder Session.
    pub canceled_session_opened_attempts: u64,
    /// Canceled requests that had replaced an incompatible decoder Session.
    pub canceled_session_replaced_attempts: u64,
    /// Canceled requests that reused a compatible decoder Session.
    pub canceled_session_reused_attempts: u64,
    /// Canceled requests that bypassed decoder-Session work through a cache.
    pub canceled_session_bypassed_cache_attempts: u64,
    /// Canceled requests that entered no Session or lacked exact lifecycle evidence.
    pub canceled_session_unclassified_attempts: u64,
    /// Total decoder Session open/replacement time retained from canceled requests.
    pub canceled_session_open_total_duration_us: u64,
    /// Slowest decoder Session open/replacement time retained from a canceled request.
    pub canceled_session_open_max_duration_us: u64,
    /// Total worker execution time spent in canceled decode jobs for this access mode.
    pub canceled_total_duration_us: u64,
    /// Slowest worker execution time for a canceled decode job in this access mode.
    pub canceled_max_duration_us: u64,
    /// Most recent canceled decode worker execution time in this access mode.
    pub canceled_last_duration_us: u64,
    /// Canceled jobs with attributable request-to-logical-cancellation timing in this mode.
    pub cancel_observation_samples: u64,
    /// Total request-to-logical-cancellation latency for this access mode.
    pub cancel_observation_total_us: u64,
    /// Slowest request-to-logical-cancellation latency for this access mode.
    pub cancel_observation_max_us: u64,
    /// Most recent request-to-logical-cancellation latency for this access mode.
    pub cancel_observation_last_us: u64,
    /// Total latency after canceled jobs first observed cancellation for this access mode.
    pub canceled_return_latency_total_us: u64,
    /// Slowest latency after a canceled job first observed cancellation for this access mode.
    pub canceled_return_latency_max_us: u64,
    /// Most recent latency after a canceled job first observed cancellation for this access mode.
    pub canceled_return_latency_last_us: u64,
    /// Failed decode jobs for this access mode.
    pub failed_jobs: u64,
    /// Failed decode jobs caused by structured decode timeouts for this access mode.
    pub timeout_failures: u64,
    /// Failed decode jobs caused by forward-scan budget exhaustion for this access mode.
    pub budget_exhausted_failures: u64,
    /// Exact Playback/still requests rejected because decode selected another timestamp.
    pub temporal_mismatch_failures: u64,
    /// Decode requests for this access mode that required a seek.
    pub seeked_frames: u64,
    /// Results that intentionally selected a non-exact temporal approximation.
    pub temporal_approximation_frames: u64,
    /// Decode results that used keyframe-before exact seek semantics.
    pub keyframe_seek_strategy_frames: u64,
    /// Decode results that used bounded any-frame low-latency seek semantics.
    pub bounded_any_seek_strategy_frames: u64,
    /// Largest forward session reuse window reported by this access mode.
    pub forward_reuse_frame_window_max: u64,
    /// Largest forward scan budget reported by this access mode.
    pub forward_decode_budget_frames_max: u64,
    /// Largest bounded-any seek window reported by this access mode.
    pub any_seek_window_ms_max: u64,
    /// Decode results that reused an existing access-mode-local session.
    pub session_reused_frames: u64,
    /// Decode results that opened a new access-mode-local session.
    pub session_opened_frames: u64,
    /// Decode results that replaced an incompatible/retired access-mode-local session.
    pub session_replaced_frames: u64,
    /// Decode results that bypassed the decoder through its session-local ring.
    pub session_bypassed_cache_frames: u64,
    /// Decode results without valid session lifecycle evidence.
    pub session_unclassified_frames: u64,
    /// Decode results produced by continuing forward in an existing session without seeking.
    pub forward_reused_frames: u64,
    /// Decode results where the media session had observed keyframe seek-index evidence.
    pub seek_index_available_frames: u64,
    /// Decode results whose seek was bounded by a session-local keyframe index anchor.
    pub seek_index_used_frames: u64,
    /// Largest distinct keyframe count observed in the session-local seek index.
    pub seek_index_keyframes_max: u64,
    /// Largest video-packet observation count used to build session-local seek-index evidence.
    pub seek_index_observed_packets_max: u64,
    /// Decode results whose keyframe index was seeded from container/probe metadata.
    pub seek_index_probe_backed_frames: u64,
    /// Decode results whose keyframe index was learned from packets decoded in-session.
    pub seek_index_session_observed_frames: u64,
    /// Decode results that reported active hardware decode at the media boundary.
    pub hardware_decode_active_frames: u64,
    /// Decode results that reported zero-copy decoded-frame residency.
    pub zero_copy_active_frames: u64,
    /// Decode results whose media frame residency was GPU texture backed.
    pub gpu_texture_resident_frames: u64,
    /// Decode results whose source decoder surface was NV12.
    pub decoded_nv12_surface_frames: u64,
    /// Decode results whose source decoder surface was P010.
    pub decoded_p010_surface_frames: u64,
    /// Decode results blocked because hardware texture residency is not connected.
    pub hardware_decode_texture_residency_blocker_frames: u64,
    /// Decode requests that preferred hardware decode, allowing CPU transfer.
    pub hardware_decode_prefer_hardware_requested_frames: u64,
    /// Decode requests that preferred GPU-resident native frames.
    pub hardware_decode_prefer_gpu_requested_frames: u64,
    /// Decode requests that required GPU-resident native frames.
    pub hardware_decode_require_gpu_requested_frames: u64,
    /// Decode requests where no hardware-resident path was requested.
    pub hardware_decode_auto_requested_frames: u64,
    /// Decode requests that selected CPU RGBA because hardware was not requested.
    pub hardware_decode_cpu_not_requested_frames: u64,
    /// Decode requests that selected CPU RGBA because hardware residency is unavailable.
    pub hardware_decode_cpu_unavailable_frames: u64,
    /// Decode requests blocked at a backend that cannot return native GPU residency.
    pub hardware_decode_backend_unavailable_frames: u64,
    /// Decode requests whose FFmpeg decoder has no matching hardware config.
    pub hardware_decode_codec_unsupported_frames: u64,
    /// Decode requests that attempted FFmpeg hardware device context creation.
    pub hardware_decode_device_context_attempted_frames: u64,
    /// Decode requests where FFmpeg created a hardware device context.
    pub hardware_decode_device_context_created_frames: u64,
    /// Decode requests blocked because FFmpeg could not create a hardware device context.
    pub hardware_decode_device_context_unavailable_frames: u64,
    /// Decode requests where FFmpeg hardware decode transfers frames back to CPU RGBA.
    pub hardware_decode_cpu_transfer_frames: u64,
    /// Decode requests whose session configured hardware decode with CPU transfer.
    pub hardware_decode_cpu_transfer_configured_frames: u64,
    /// Decode requests whose session observed hardware frames transferred to CPU.
    pub hardware_decode_cpu_transfer_observed_frames: u64,
    /// Decode requests whose FFmpeg hardware CPU-transfer setup failed before decoder open.
    pub hardware_decode_cpu_transfer_setup_failed_frames: u64,
    /// Decode requests whose FFmpeg hardware-configured decoder failed to open.
    pub hardware_decode_cpu_transfer_decoder_open_failed_frames: u64,
    /// Decode requests configured for hardware CPU transfer but still waiting for a hardware frame.
    pub hardware_decode_cpu_transfer_awaiting_frame_frames: u64,
    /// Decode requests blocked at a CPU RGBA backend boundary.
    pub hardware_decode_backend_boundary_frames: u64,
    /// Decode requests that selected GPU-resident native decode.
    pub hardware_decode_gpu_resident_native_frames: u64,
    /// Decode requests with a D3D12VA backend candidate.
    pub hardware_decode_candidate_d3d12va_frames: u64,
    /// Decode requests with a D3D11VA backend candidate.
    pub hardware_decode_candidate_d3d11va_frames: u64,
    /// Decode requests with a legacy DXVA2 backend candidate.
    pub hardware_decode_candidate_dxva2_frames: u64,
    /// Decode requests with a VideoToolbox backend candidate.
    pub hardware_decode_candidate_videotoolbox_frames: u64,
    /// Decode requests with a VA-API backend candidate.
    pub hardware_decode_candidate_vaapi_frames: u64,
    /// Decode requests with a legacy VDPAU backend candidate.
    pub hardware_decode_candidate_vdpau_frames: u64,
    /// Decode requests with a CUDA/NVDEC backend candidate.
    pub hardware_decode_candidate_cuda_frames: u64,
    /// Decode requests where a backend candidate exists but no decoder adapter is connected.
    pub hardware_decode_adapter_unavailable_frames: u64,
    /// Total decoded frames consumed by this access mode.
    pub decoded_frame_count: u64,
    /// Largest decoded-frame count consumed by one request in this access mode.
    pub max_decoded_frame_count: u64,
    /// Aggregated media-layer stage timings for this access mode.
    pub stage_durations: PreviewDecodeStageDurations,
    /// Stage timings from the slowest frame in this access mode.
    pub max_frame_stage_durations: PreviewDecodeStageDurations,
    /// Queue wait from the same slowest frame in this access mode.
    pub max_frame_queue_wait_us: u64,
    /// Dominant bottleneck from the same slowest frame in this access mode.
    pub max_frame_bottleneck: PreviewDecodeBottleneck,
}

fn preview_decode_work_class(diagnostics: PreviewDecodeDiagnostics) -> PreviewDecodeWorkClass {
    let decoder_path =
        diagnostics.path != PreviewDecodePath::PlaybackSessionRingHit && !diagnostics.cache_hit;
    let no_session_open = diagnostics.stage_durations.session_open_us == 0;
    match diagnostics.session_disposition {
        PreviewDecodeSessionDisposition::Opened if decoder_path && !diagnostics.forward_reused => {
            PreviewDecodeWorkClass::SessionOpened
        }
        PreviewDecodeSessionDisposition::Replaced
            if decoder_path && !diagnostics.forward_reused =>
        {
            PreviewDecodeWorkClass::SessionReplaced
        }
        PreviewDecodeSessionDisposition::BypassedCache
            if diagnostics.path == PreviewDecodePath::PlaybackSessionRingHit
                && diagnostics.cache_hit
                && !diagnostics.seek_performed
                && !diagnostics.forward_reused
                && diagnostics.stage_durations.session_open_us == 0 =>
        {
            PreviewDecodeWorkClass::CacheHit
        }
        PreviewDecodeSessionDisposition::Reused
            if decoder_path
                && no_session_open
                && diagnostics.forward_reused
                && !diagnostics.seek_performed =>
        {
            PreviewDecodeWorkClass::ForwardSteady
        }
        PreviewDecodeSessionDisposition::Reused
            if decoder_path
                && no_session_open
                && diagnostics.seek_performed
                && !diagnostics.forward_reused =>
        {
            PreviewDecodeWorkClass::ReusedSeek
        }
        PreviewDecodeSessionDisposition::Reused
            if decoder_path
                && no_session_open
                && !diagnostics.seek_performed
                && !diagnostics.forward_reused =>
        {
            PreviewDecodeWorkClass::ReusedOther
        }
        PreviewDecodeSessionDisposition::Opened
        | PreviewDecodeSessionDisposition::Replaced
        | PreviewDecodeSessionDisposition::Unspecified
        | PreviewDecodeSessionDisposition::BypassedCache
        | PreviewDecodeSessionDisposition::Reused => PreviewDecodeWorkClass::Unclassified,
    }
}

impl PreviewDecodeAccessModeProfile {
    fn mode_local_evidence_frames(self) -> u64 {
        // The only media-layer reuse path is the access-mode-local playback
        // Session ring. App Frame Store hits are counted separately and never
        // enter this decode profile.
        self.frames
    }

    fn successful_lifecycle_frames(self) -> u64 {
        self.session_opened_frames
            .saturating_add(self.session_replaced_frames)
            .saturating_add(self.session_reused_frames)
            .saturating_add(self.session_bypassed_cache_frames)
            .saturating_add(self.session_unclassified_frames)
    }

    fn canceled_session_lifecycle_attempts(self) -> u64 {
        self.canceled_session_opened_attempts
            .saturating_add(self.canceled_session_replaced_attempts)
            .saturating_add(self.canceled_session_reused_attempts)
            .saturating_add(self.canceled_session_bypassed_cache_attempts)
            .saturating_add(self.canceled_session_unclassified_attempts)
    }

    fn session_churn_events(self) -> u64 {
        self.session_opened_frames
            .saturating_add(self.session_replaced_frames)
            .saturating_add(self.canceled_session_open_attempts)
    }

    fn session_churn_attempts(self) -> u64 {
        self.frames.saturating_add(self.canceled_session_lifecycle_attempts())
    }

    fn hardware_decode_requested_frames(self) -> u64 {
        self.hardware_decode_prefer_hardware_requested_frames
            .saturating_add(self.hardware_decode_prefer_gpu_requested_frames)
            .saturating_add(self.hardware_decode_require_gpu_requested_frames)
    }

    fn hardware_decode_effective_frames(self) -> u64 {
        self.hardware_decode_cpu_transfer_observed_frames
            .saturating_add(self.hardware_decode_gpu_resident_native_frames)
    }

    fn hardware_decode_fallback_not_engaged_frames(self) -> u64 {
        self.hardware_decode_requested_frames()
            .saturating_sub(self.hardware_decode_effective_frames())
    }

    pub(super) fn record(&mut self, diagnostics: PreviewDecodeDiagnostics, queue_wait_us: u64) {
        self.frames = self.frames.saturating_add(1);
        match diagnostics.path {
            PreviewDecodePath::InProcessFfmpegCpuRgba
            | PreviewDecodePath::InProcessFfmpegCpuFloat => {
                self.in_process_cpu_frames = self.in_process_cpu_frames.saturating_add(1);
            }
            PreviewDecodePath::InProcessFfmpegNative => {}
            PreviewDecodePath::ExternalFfmpegCpuRgba => {
                self.external_ffmpeg_cpu_rgba_frames =
                    self.external_ffmpeg_cpu_rgba_frames.saturating_add(1);
            }
            PreviewDecodePath::PlaybackSessionRingHit => {
                self.playback_session_ring_hit_frames =
                    self.playback_session_ring_hit_frames.saturating_add(1);
                self.cache_hit_frames = self.cache_hit_frames.saturating_add(1);
            }
        }
        self.total_duration_us = self.total_duration_us.saturating_add(diagnostics.elapsed_us);
        self.latency_buckets.record(diagnostics.elapsed_us);
        let work_class = preview_decode_work_class(diagnostics);
        self.work_classes.record(work_class, diagnostics.elapsed_us);
        if diagnostics.elapsed_us >= self.max_duration_us {
            self.max_duration_us = diagnostics.elapsed_us;
            self.max_frame_stage_durations = diagnostics.stage_durations;
            self.max_frame_queue_wait_us = queue_wait_us;
            self.max_frame_bottleneck =
                classify_preview_decode_bottleneck(diagnostics.stage_durations, queue_wait_us);
        }
        self.last_duration_us = diagnostics.elapsed_us;
        if diagnostics.seek_performed {
            self.seeked_frames = self.seeked_frames.saturating_add(1);
        }
        if diagnostics.temporal_approximation {
            self.temporal_approximation_frames =
                self.temporal_approximation_frames.saturating_add(1);
        }
        match diagnostics.seek_strategy {
            PreviewDecodeSeekStrategy::KeyframeBefore => {
                self.keyframe_seek_strategy_frames =
                    self.keyframe_seek_strategy_frames.saturating_add(1);
            }
            PreviewDecodeSeekStrategy::BoundedAnyFrame => {
                self.bounded_any_seek_strategy_frames =
                    self.bounded_any_seek_strategy_frames.saturating_add(1);
            }
        }
        self.forward_reuse_frame_window_max = self
            .forward_reuse_frame_window_max
            .max(diagnostics.forward_reuse_frame_window.max(0) as u64);
        self.forward_decode_budget_frames_max = self
            .forward_decode_budget_frames_max
            .max(u64::from(diagnostics.forward_decode_budget_frames));
        self.any_seek_window_ms_max =
            self.any_seek_window_ms_max.max(diagnostics.any_seek_window_ms);
        match work_class {
            PreviewDecodeWorkClass::SessionOpened => {
                self.session_opened_frames = self.session_opened_frames.saturating_add(1);
            }
            PreviewDecodeWorkClass::SessionReplaced => {
                self.session_replaced_frames = self.session_replaced_frames.saturating_add(1);
            }
            PreviewDecodeWorkClass::ForwardSteady
            | PreviewDecodeWorkClass::ReusedSeek
            | PreviewDecodeWorkClass::ReusedOther => {
                self.session_reused_frames = self.session_reused_frames.saturating_add(1);
            }
            PreviewDecodeWorkClass::CacheHit => {
                self.session_bypassed_cache_frames =
                    self.session_bypassed_cache_frames.saturating_add(1);
            }
            PreviewDecodeWorkClass::Unclassified => {
                self.session_unclassified_frames =
                    self.session_unclassified_frames.saturating_add(1);
            }
        }
        if diagnostics.forward_reused {
            self.forward_reused_frames = self.forward_reused_frames.saturating_add(1);
        }
        if diagnostics.seek_index_available {
            self.seek_index_available_frames = self.seek_index_available_frames.saturating_add(1);
        }
        if diagnostics.seek_index_used {
            self.seek_index_used_frames = self.seek_index_used_frames.saturating_add(1);
        }
        self.seek_index_keyframes_max =
            self.seek_index_keyframes_max.max(u64::from(diagnostics.seek_index_keyframes));
        self.seek_index_observed_packets_max = self
            .seek_index_observed_packets_max
            .max(u64::from(diagnostics.seek_index_observed_packets));
        match diagnostics.seek_index_source {
            PreviewSeekIndexSource::None => {}
            PreviewSeekIndexSource::ProbeBacked => {
                self.seek_index_probe_backed_frames =
                    self.seek_index_probe_backed_frames.saturating_add(1);
            }
            PreviewSeekIndexSource::SessionObserved => {
                self.seek_index_session_observed_frames =
                    self.seek_index_session_observed_frames.saturating_add(1);
            }
        }
        if diagnostics.hardware_decode_active {
            self.hardware_decode_active_frames =
                self.hardware_decode_active_frames.saturating_add(1);
        }
        if diagnostics.zero_copy_active {
            self.zero_copy_active_frames = self.zero_copy_active_frames.saturating_add(1);
        }
        if diagnostics.decoded_frame_residency == DecodedFrameResidency::GpuTexture {
            self.gpu_texture_resident_frames = self.gpu_texture_resident_frames.saturating_add(1);
        }
        match diagnostics.decoded_surface_format {
            DecodedVideoSurfaceFormat::Nv12 => {
                self.decoded_nv12_surface_frames =
                    self.decoded_nv12_surface_frames.saturating_add(1);
            }
            DecodedVideoSurfaceFormat::P010 => {
                self.decoded_p010_surface_frames =
                    self.decoded_p010_surface_frames.saturating_add(1);
            }
            _ => {}
        }
        if diagnostics.hardware_decode_blocker
            == PreviewHardwareDecodeBlocker::TextureResidencyNotConnected
        {
            self.hardware_decode_texture_residency_blocker_frames =
                self.hardware_decode_texture_residency_blocker_frames.saturating_add(1);
        }
        if diagnostics.hardware_decode_ffmpeg_device_context_attempted {
            self.hardware_decode_device_context_attempted_frames =
                self.hardware_decode_device_context_attempted_frames.saturating_add(1);
            if diagnostics.hardware_decode_ffmpeg_device_context_created {
                self.hardware_decode_device_context_created_frames =
                    self.hardware_decode_device_context_created_frames.saturating_add(1);
            } else {
                self.hardware_decode_device_context_unavailable_frames =
                    self.hardware_decode_device_context_unavailable_frames.saturating_add(1);
            }
        }
        if diagnostics.hardware_decode_cpu_transfer_configured {
            self.hardware_decode_cpu_transfer_configured_frames =
                self.hardware_decode_cpu_transfer_configured_frames.saturating_add(1);
        }
        if diagnostics.hardware_decode_cpu_transfer_observed {
            self.hardware_decode_cpu_transfer_observed_frames =
                self.hardware_decode_cpu_transfer_observed_frames.saturating_add(1);
        }
        match diagnostics.hardware_decode_cpu_transfer_status {
            PreviewHardwareDecodeCpuTransferStatus::NotAttempted => {}
            PreviewHardwareDecodeCpuTransferStatus::ConfiguredAwaitingFrame => {
                self.hardware_decode_cpu_transfer_awaiting_frame_frames =
                    self.hardware_decode_cpu_transfer_awaiting_frame_frames.saturating_add(1);
            }
            PreviewHardwareDecodeCpuTransferStatus::SetupFailed => {
                self.hardware_decode_cpu_transfer_setup_failed_frames =
                    self.hardware_decode_cpu_transfer_setup_failed_frames.saturating_add(1);
            }
            PreviewHardwareDecodeCpuTransferStatus::DecoderOpenFailed => {
                self.hardware_decode_cpu_transfer_decoder_open_failed_frames =
                    self.hardware_decode_cpu_transfer_decoder_open_failed_frames.saturating_add(1);
            }
            PreviewHardwareDecodeCpuTransferStatus::Observed => {}
        }
        match diagnostics.hardware_decode_request {
            PreviewHardwareDecodeRequest::Auto => {
                self.hardware_decode_auto_requested_frames =
                    self.hardware_decode_auto_requested_frames.saturating_add(1);
            }
            PreviewHardwareDecodeRequest::PreferHardwareDecode => {
                self.hardware_decode_prefer_hardware_requested_frames =
                    self.hardware_decode_prefer_hardware_requested_frames.saturating_add(1);
            }
            PreviewHardwareDecodeRequest::PreferGpuResident => {
                self.hardware_decode_prefer_gpu_requested_frames =
                    self.hardware_decode_prefer_gpu_requested_frames.saturating_add(1);
            }
            PreviewHardwareDecodeRequest::RequireGpuResident => {
                self.hardware_decode_require_gpu_requested_frames =
                    self.hardware_decode_require_gpu_requested_frames.saturating_add(1);
            }
        }
        match diagnostics.hardware_decode_decision {
            PreviewHardwareDecodeDecision::CpuRgbaNotRequested => {
                self.hardware_decode_cpu_not_requested_frames =
                    self.hardware_decode_cpu_not_requested_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable => {
                self.hardware_decode_cpu_unavailable_frames =
                    self.hardware_decode_cpu_unavailable_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaBackendUnavailable => {
                self.hardware_decode_backend_unavailable_frames =
                    self.hardware_decode_backend_unavailable_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaCodecUnsupported => {
                self.hardware_decode_codec_unsupported_frames =
                    self.hardware_decode_codec_unsupported_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::CpuRgbaBackendBoundary => {
                self.hardware_decode_backend_boundary_frames =
                    self.hardware_decode_backend_boundary_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer => {
                self.hardware_decode_cpu_transfer_frames =
                    self.hardware_decode_cpu_transfer_frames.saturating_add(1);
            }
            PreviewHardwareDecodeDecision::GpuResidentNative => {
                self.hardware_decode_gpu_resident_native_frames =
                    self.hardware_decode_gpu_resident_native_frames.saturating_add(1);
            }
        }
        match diagnostics.hardware_decode_candidate_backend {
            Some(HwAccelBackend::D3D12VA) => {
                self.hardware_decode_candidate_d3d12va_frames =
                    self.hardware_decode_candidate_d3d12va_frames.saturating_add(1);
            }
            Some(HwAccelBackend::D3D11VA) => {
                self.hardware_decode_candidate_d3d11va_frames =
                    self.hardware_decode_candidate_d3d11va_frames.saturating_add(1);
            }
            Some(HwAccelBackend::Dxva2) => {
                self.hardware_decode_candidate_dxva2_frames =
                    self.hardware_decode_candidate_dxva2_frames.saturating_add(1);
            }
            Some(HwAccelBackend::VideoToolbox) => {
                self.hardware_decode_candidate_videotoolbox_frames =
                    self.hardware_decode_candidate_videotoolbox_frames.saturating_add(1);
            }
            Some(HwAccelBackend::Vaapi) => {
                self.hardware_decode_candidate_vaapi_frames =
                    self.hardware_decode_candidate_vaapi_frames.saturating_add(1);
            }
            Some(HwAccelBackend::Vdpau) => {
                self.hardware_decode_candidate_vdpau_frames =
                    self.hardware_decode_candidate_vdpau_frames.saturating_add(1);
            }
            Some(HwAccelBackend::Cuda) => {
                self.hardware_decode_candidate_cuda_frames =
                    self.hardware_decode_candidate_cuda_frames.saturating_add(1);
            }
            Some(HwAccelBackend::None) | None => {}
        }
        if diagnostics.hardware_decode_candidate_backend.is_some()
            && !diagnostics.hardware_decode_adapter_available
        {
            self.hardware_decode_adapter_unavailable_frames =
                self.hardware_decode_adapter_unavailable_frames.saturating_add(1);
        }
        let decoded_frame_count = u64::from(diagnostics.decoded_frame_count);
        self.decoded_frame_count = self.decoded_frame_count.saturating_add(decoded_frame_count);
        self.max_decoded_frame_count = self.max_decoded_frame_count.max(decoded_frame_count);
        self.stage_durations.accumulate(diagnostics.stage_durations);
    }

    fn record_queue_wait(&mut self, queue_wait_us: u64) {
        self.queue_wait_samples = self.queue_wait_samples.saturating_add(1);
        self.queue_wait_total_us = self.queue_wait_total_us.saturating_add(queue_wait_us);
        self.queue_wait_max_us = self.queue_wait_max_us.max(queue_wait_us);
        self.queue_wait_last_us = queue_wait_us;
        self.queue_wait_buckets.record(queue_wait_us);
    }

    fn record_expired_queue_wait(
        &mut self,
        priority: MediaPreviewRequestPriority,
        queue_wait_us: u64,
    ) {
        self.expired_queue_wait.record(priority, queue_wait_us);
    }

    fn record_session_cancellation(
        &mut self,
        cancellation: mondrian_media::PreviewDecodeCancellation,
    ) {
        match cancellation.session_disposition {
            PreviewDecodeSessionDisposition::Opened => {
                self.canceled_session_opened_attempts =
                    self.canceled_session_opened_attempts.saturating_add(1);
                self.canceled_session_open_attempts =
                    self.canceled_session_open_attempts.saturating_add(1);
            }
            PreviewDecodeSessionDisposition::Replaced => {
                self.canceled_session_replaced_attempts =
                    self.canceled_session_replaced_attempts.saturating_add(1);
                self.canceled_session_open_attempts =
                    self.canceled_session_open_attempts.saturating_add(1);
            }
            PreviewDecodeSessionDisposition::Reused => {
                self.canceled_session_reused_attempts =
                    self.canceled_session_reused_attempts.saturating_add(1);
            }
            PreviewDecodeSessionDisposition::BypassedCache => {
                self.canceled_session_bypassed_cache_attempts =
                    self.canceled_session_bypassed_cache_attempts.saturating_add(1);
            }
            PreviewDecodeSessionDisposition::Unspecified => {
                self.canceled_session_unclassified_attempts =
                    self.canceled_session_unclassified_attempts.saturating_add(1);
            }
        }
        self.canceled_session_open_total_duration_us = self
            .canceled_session_open_total_duration_us
            .saturating_add(cancellation.session_open_us);
        self.canceled_session_open_max_duration_us =
            self.canceled_session_open_max_duration_us.max(cancellation.session_open_us);
    }

    fn apply_cancellation(&mut self, profile: mondrian_playback::FrameCancellationProfile) {
        self.canceled_jobs = profile.cancellations;
        self.canceled_shutdown_jobs = profile.shutdown;
        self.canceled_obsolete_jobs = profile.superseded;
        self.canceled_prefetch_deadline_jobs = profile.prefetch_deadline;
        self.canceled_playback_deadline_jobs = profile.playback_deadline;
        self.canceled_prefetch_preempted_jobs = profile.prefetch_preempted_by_current;
        self.canceled_still_preempted_jobs = profile.still_preempted_by_realtime_current;
        self.canceled_unknown_jobs = profile.unknown;
        self.canceled_total_duration_us = profile.execution.total_us;
        self.canceled_max_duration_us = profile.execution.max_us;
        self.canceled_last_duration_us = profile.execution.last_us;
        self.cancel_observation_samples = profile.request_to_logical_cancellation.samples;
        self.cancel_observation_total_us = profile.request_to_logical_cancellation.total_us;
        self.cancel_observation_max_us = profile.request_to_logical_cancellation.max_us;
        self.cancel_observation_last_us = profile.request_to_logical_cancellation.last_us;
        self.canceled_return_latency_total_us = profile.logical_cancellation_to_return.total_us;
        self.canceled_return_latency_max_us = profile.logical_cancellation_to_return.max_us;
        self.canceled_return_latency_last_us = profile.logical_cancellation_to_return.last_us;
    }

    fn record_failure(&mut self, reason: MediaPreviewFailureReason) {
        self.failed_jobs = self.failed_jobs.saturating_add(1);
        match reason {
            MediaPreviewFailureReason::Timeout => {
                self.timeout_failures = self.timeout_failures.saturating_add(1);
            }
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted => {
                self.budget_exhausted_failures = self.budget_exhausted_failures.saturating_add(1);
            }
            MediaPreviewFailureReason::TemporalMismatch => {
                self.temporal_mismatch_failures = self.temporal_mismatch_failures.saturating_add(1);
            }
            MediaPreviewFailureReason::DecodeError
            | MediaPreviewFailureReason::WorkerPanicked
            | MediaPreviewFailureReason::ResidencyContractViolation
            | MediaPreviewFailureReason::ResidencyCapacityRejected => {}
        }
    }
}

/// Preview decode profiles split by access mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeAccessModeProfiles {
    /// Sustained playback and forward-prefetch decode profile.
    pub playback_cursor: PreviewDecodeAccessModeProfile,
    /// Latest-wins interactive scrub decode profile.
    pub scrub_cursor: PreviewDecodeAccessModeProfile,
    /// Deterministic still-frame/random-access decode profile.
    pub random_access_still: PreviewDecodeAccessModeProfile,
}

impl PreviewDecodeAccessModeProfiles {
    fn total_frames(self) -> u64 {
        self.playback_cursor
            .frames
            .saturating_add(self.scrub_cursor.frames)
            .saturating_add(self.random_access_still.frames)
    }

    fn total_expired_queue_wait_samples(self) -> u64 {
        self.playback_cursor
            .expired_queue_wait
            .samples
            .saturating_add(self.scrub_cursor.expired_queue_wait.samples)
            .saturating_add(self.random_access_still.expired_queue_wait.samples)
    }

    fn profile_for(self, access_mode: PreviewDecodeAccessMode) -> PreviewDecodeAccessModeProfile {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => self.playback_cursor,
            PreviewDecodeAccessMode::ScrubCursor => self.scrub_cursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame => self.random_access_still,
        }
    }

    pub(super) fn record(&mut self, diagnostics: PreviewDecodeDiagnostics, queue_wait_us: u64) {
        match diagnostics.access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record(diagnostics, queue_wait_us);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record(diagnostics, queue_wait_us);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record(diagnostics, queue_wait_us);
            }
        }
    }

    pub(super) fn record_queue_wait(
        &mut self,
        access_mode: PreviewDecodeAccessMode,
        queue_wait_us: u64,
    ) {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record_queue_wait(queue_wait_us);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record_queue_wait(queue_wait_us);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record_queue_wait(queue_wait_us);
            }
        }
    }

    pub(super) fn record_expired_queue_wait(
        &mut self,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        queue_wait_us: u64,
    ) {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record_expired_queue_wait(priority, queue_wait_us);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record_expired_queue_wait(priority, queue_wait_us);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record_expired_queue_wait(priority, queue_wait_us);
            }
        }
    }

    pub(super) fn record_session_cancellation(
        &mut self,
        access_mode: PreviewDecodeAccessMode,
        cancellation: mondrian_media::PreviewDecodeCancellation,
    ) {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record_session_cancellation(cancellation);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record_session_cancellation(cancellation);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record_session_cancellation(cancellation);
            }
        }
    }

    pub(super) fn apply_cancellation(
        &mut self,
        report: mondrian_playback::FrameCancellationEvidenceReport,
    ) {
        self.playback_cursor.apply_cancellation(report.playback);
        self.scrub_cursor.apply_cancellation(report.interactive);
        self.random_access_still.apply_cancellation(report.still);
    }

    pub(super) fn record_failure(
        &mut self,
        access_mode: PreviewDecodeAccessMode,
        reason: MediaPreviewFailureReason,
    ) {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.record_failure(reason);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.record_failure(reason);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.record_failure(reason);
            }
        }
    }

    pub(super) fn slowest_access_mode(self) -> Option<PreviewDecodeAccessMode> {
        self.named_profiles()
            .into_iter()
            .filter(|(_, profile)| profile.max_duration_us > 0)
            .max_by_key(|(_, profile)| profile.max_duration_us)
            .map(|(access_mode, _)| access_mode)
    }

    fn named_profiles(self) -> [(PreviewDecodeAccessMode, PreviewDecodeAccessModeProfile); 3] {
        [
            (
                PreviewDecodeAccessMode::PlaybackCursor,
                self.playback_cursor,
            ),
            (PreviewDecodeAccessMode::ScrubCursor, self.scrub_cursor),
            (
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                self.random_access_still,
            ),
        ]
    }
}

/// Stable preview decode performance summary for perf JSONL and diagnostics tooling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodePerformanceSummary {
    /// Coordinated CPU budget used for preview workers and FFmpeg decoder threads.
    pub cpu_budget: PreviewDecodeCpuBudget,
    /// App-level playback hardware-decode admission state.
    pub hardware_decode_admission: PreviewHardwareDecodeAdmissionDiagnostics,
    /// Successful preview decode/cache results.
    pub decode_successes: u64,
    /// Successful bounded startup-preroll decodes, outside steady-state budgets.
    pub startup_preroll_frames: u64,
    /// Total bounded startup-preroll decode execution time.
    pub startup_preroll_total_duration_us: u64,
    /// Slowest bounded startup-preroll decode execution time.
    pub startup_preroll_max_duration_us: u64,
    /// Total bounded startup-preroll queue wait.
    pub startup_preroll_queue_wait_total_us: u64,
    /// Slowest bounded startup-preroll queue wait.
    pub startup_preroll_queue_wait_max_us: u64,
    /// Failed preview decode results.
    pub decode_failures: u64,
    /// Failed preview decode results caused by structured decode timeouts.
    pub decode_timeout_failures: u64,
    /// Failed preview decode results caused by access-mode forward-scan budget exhaustion.
    pub decode_budget_exhausted_failures: u64,
    /// Playback-owned cancellation evidence used by production and Headless gates.
    pub cancellation: mondrian_playback::FrameCancellationEvidenceReport,
    /// Canceled preview decode jobs.
    pub canceled_jobs: u64,
    /// Canceled preview decode jobs caused by shutdown.
    pub canceled_shutdown_jobs: u64,
    /// Canceled preview decode jobs caused by obsolete pending work.
    pub canceled_obsolete_jobs: u64,
    /// Prefetch decode jobs canceled by their deadline.
    pub canceled_prefetch_deadline_jobs: u64,
    /// Current playback decode jobs canceled because their display deadline was missed.
    pub canceled_playback_deadline_jobs: u64,
    /// Prefetch decode jobs canceled because visible current-frame work was pending.
    pub canceled_prefetch_preempted_jobs: u64,
    /// Still-frame decode jobs canceled because realtime current-frame work was pending.
    pub canceled_still_preempted_jobs: u64,
    /// Canceled preview decode jobs with no structured reason.
    pub canceled_unknown_jobs: u64,
    /// Total worker execution time spent in canceled decode jobs.
    pub canceled_total_duration_us: u64,
    /// Slowest worker execution time for a canceled decode job.
    pub canceled_max_duration_us: u64,
    /// Most recent worker execution time for a canceled decode job.
    pub canceled_last_duration_us: u64,
    /// Canceled jobs with attributable request-to-logical-cancellation timing.
    pub cancel_observation_samples: u64,
    /// Total authority-request to `LogicalCancellationObserved` latency.
    pub cancel_observation_total_us: u64,
    /// Slowest authority-request to `LogicalCancellationObserved` latency.
    pub cancel_observation_max_us: u64,
    /// Most recent authority-request to `LogicalCancellationObserved` latency.
    pub cancel_observation_last_us: u64,
    /// Total latency after canceled decode jobs first observed cancellation.
    pub canceled_return_latency_total_us: u64,
    /// Slowest latency after a canceled decode job first observed cancellation.
    pub canceled_return_latency_max_us: u64,
    /// Most recent latency after a canceled decode job first observed cancellation.
    pub canceled_return_latency_last_us: u64,
    /// Successful decodes served from in-process FFmpeg CPU RGBA8/f32 paths.
    pub in_process_cpu_frames: u64,
    /// Successful decodes served from the external ffmpeg CPU RGBA path.
    pub external_ffmpeg_cpu_rgba_frames: u64,
    /// Successful playback decodes served from the playback session-local ring.
    pub playback_session_ring_hit_frames: u64,
    /// Successful decodes served from a decoder-session-local cache.
    pub cache_hit_frames: u64,
    /// Decode results requested through the playback cursor access contract.
    pub playback_cursor_frames: u64,
    /// Decode results requested through the scrub cursor access contract.
    pub scrub_cursor_frames: u64,
    /// Decode results requested through the random-access still-frame contract.
    pub random_access_still_frames: u64,
    /// Canceled decode jobs requested through the playback cursor access contract.
    pub canceled_playback_cursor_jobs: u64,
    /// Canceled decode jobs requested through the scrub cursor access contract.
    pub canceled_scrub_cursor_jobs: u64,
    /// Canceled decode jobs requested through the random-access still-frame contract.
    pub canceled_random_access_still_jobs: u64,
    /// Maximum end-to-end decode duration.
    pub max_duration_us: u64,
    /// Most recent end-to-end decode duration.
    pub last_duration_us: u64,
    /// Total end-to-end decode duration.
    pub total_duration_us: u64,
    /// Slow-frame budget applied by the report.
    pub slow_frame_budget_us: u64,
    /// Total time Ready jobs spent waiting in the preview worker queue.
    pub queue_wait_total_us: u64,
    /// Slowest Ready job queue wait.
    pub queue_wait_max_us: u64,
    /// Most recent Ready job queue wait.
    pub queue_wait_last_us: u64,
    /// Slowest Ready current-frame decode queue wait.
    pub current_queue_wait_max_us: u64,
    /// Slowest Ready prefetch decode queue wait.
    pub prefetch_queue_wait_max_us: u64,
    /// Queue wait for jobs rejected as expired before codec execution.
    pub expired_queue_wait: PreviewDecodeQueueWaitProfile,
    /// Media preview jobs accepted by the worker queue.
    pub enqueued_jobs: u64,
    /// Playback prefetch passes skipped while visible current-frame work was pending.
    pub prefetch_skipped_current_pending: u64,
    /// Playback prefetch passes skipped while current-frame worker work was queued or running.
    pub prefetch_skipped_current_work: u64,
    /// Playback prefetch passes skipped because queued/running prefetch already covered the window.
    pub prefetch_skipped_prefetch_backlog: u64,
    /// Jobs dropped because the bounded worker queue was full.
    pub queue_full_drops: u64,
    /// Jobs rejected by the worker queue for invalid priority/access-mode pairs.
    pub queue_invalid_access_mode_drops: u64,
    /// Queued prefetch jobs evicted so current-frame decode can run.
    pub queue_evicted_prefetch_jobs: u64,
    /// Queued still-frame jobs evicted so real-time current work can run.
    pub queue_evicted_still_jobs: u64,
    /// User escape-path cancellations requested by transport, close, or quit.
    pub interactive_cancel_requests: u64,
    /// Scheduler pending requests canceled by user escape-path cancellations.
    pub interactive_cancel_scheduler_requests: u64,
    /// Worker-queue jobs cleared by user escape-path cancellations.
    pub interactive_cancel_queued_jobs: u64,
    /// Queued jobs removed because their scheduler-side pending request was canceled.
    pub queue_canceled_jobs: u64,
    /// Playback buffering releases caused by a stalled realtime current-frame request.
    pub playback_current_stalled_expirations: u64,
    /// Obsolete queued jobs removed before scheduling current-frame decode.
    pub queue_pruned_obsolete_jobs: u64,
    /// Queued prefetch jobs promoted after the same key became current-frame work.
    pub queue_promoted_current_jobs: u64,
    /// Jobs dropped because preview workers were unavailable.
    pub worker_disconnected_drops: u64,
    /// Current worker transport queue depth grouped by scheduling contract.
    pub worker_queue: MediaPreviewJobQueueDiagnostics,
    /// Decode requests that required a seek.
    pub seeked_frames: u64,
    /// Total decoded frames consumed before frame selection.
    pub decoded_frame_count: u64,
    /// Maximum decoded frames consumed by one request.
    pub max_decoded_frame_count: u64,
    /// Aggregated media-layer decode stage timings.
    pub stage_durations: PreviewDecodeStageDurations,
    /// Stage timings from the slowest decode frame.
    pub max_frame_stage_durations: PreviewDecodeStageDurations,
    /// Queue wait from the same slowest decode frame.
    pub max_frame_queue_wait_us: u64,
    /// Decode profile split by playback, scrub, and random-access still modes.
    pub access_mode_profiles: PreviewDecodeAccessModeProfiles,
    /// Access mode that produced the slowest successful decode frame.
    pub slowest_access_mode: Option<PreviewDecodeAccessMode>,
    /// Dominant stage inferred from the slowest successful decode frame.
    pub primary_bottleneck: PreviewDecodeBottleneck,
    /// Scheduler-side access-mode/drop/stale diagnostics captured with decode evidence.
    pub scheduler: MediaPreviewSchedulerDiagnostics,
    /// Playback-clock deadline and forward-prefetch scheduling contract.
    pub playback_schedule: PreviewPlaybackScheduleDiagnostics,
}

/// Dominant preview decode bottleneck inferred from stage diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum PreviewDecodeBottleneck {
    /// No decode evidence was captured.
    #[default]
    None,
    /// Opening or reconfiguring the decode session dominated.
    SessionOpen,
    /// Waiting for downstream native-output leases before decoder reuse dominated.
    OutputLease,
    /// Waiting in the preview decode worker queue dominated.
    QueueWait,
    /// Cache lookup dominated.
    CacheLookup,
    /// Seek and decoder flush dominated.
    Seek,
    /// Packet demux/decode dominated.
    PacketDecode,
    /// FFmpeg hardware-frame transfer back to CPU dominated.
    HardwareTransfer,
    /// FFmpeg software scale or CPU RGBA copy dominated.
    CpuRgbaBoundary,
    /// External ffmpeg process wait dominated.
    ExternalProcess,
}

/// Schema version for preview decode performance reports.
pub const PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION: u32 = 36;

/// Default steady-state Preview decode budget.
pub const PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US: u64 = 50_000;

/// Default cold decode-session readiness budget.
///
/// Session construction is not a recurring frame deadline, but it remains
/// bounded user-visible first-frame work and is never discarded from evidence.
pub const PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US: u64 = 5_000_000;

/// One versioned latency/coverage obligation in the decode report matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodeWorkBudget {
    /// Access mode governed by this matrix cell.
    pub access_mode: PreviewDecodeAccessMode,
    /// Concrete decoder work class governed by this matrix cell.
    pub work_class: PreviewDecodeWorkClass,
    /// Minimum successful samples required by this report profile.
    pub min_samples: u64,
    /// Maximum worker execution time for any successful sample.
    pub max_worker_execution_us: u64,
    /// Optional bounded p95 worker-execution target.
    pub p95_worker_execution_us: Option<u64>,
}

/// Serialized policy applied to a Preview decode performance report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodePerformancePolicy {
    /// Explicit access-mode/work-class latency matrix.
    pub work_budgets: Vec<PreviewDecodeWorkBudget>,
    /// Queue wait target kept separate from worker execution.
    pub queue_wait_budget_us: u64,
    /// Minimum absolute Session open/replacement churn allowance.
    pub session_churn_grace_frames: u64,
    /// Maximum session open/replacement ratio after the grace allowance.
    pub max_session_churn_basis_points: u64,
}

/// Versioned preview decode performance report for UI, telemetry, and perf artifacts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodePerformanceReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall preview decode performance verdict.
    pub verdict: PreviewDecodePerformanceVerdict,
    /// Access modes this report profile required to be sampled.
    pub required_access_modes: Vec<PreviewDecodeAccessMode>,
    /// Exact latency and coverage policy used to evaluate this report.
    pub policy: PreviewDecodePerformancePolicy,
    /// Structured decode performance summary used as report evidence.
    pub summary: Option<PreviewDecodePerformanceSummary>,
    /// Structured checks by preview decode area.
    pub checks: Vec<PreviewDecodePerformanceCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<PreviewDecodePerformanceRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<PreviewDecodePerformanceAction>,
}

/// Stable preview render performance summary for post-decode viewer work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewRenderPerformanceSummary {
    /// Viewer render requests with timing evidence.
    pub timed_frames: u64,
    /// Maximum post-decode render duration.
    pub max_duration_us: u64,
    /// Most recent post-decode render duration.
    pub last_duration_us: u64,
    /// Total post-decode render duration.
    pub total_duration_us: u64,
    /// Slow-frame budget applied by the report.
    pub slow_frame_budget_us: u64,
    /// Aggregated post-decode render stage timings.
    pub stage_durations: PreviewRenderStageDurations,
    /// Stage timings from the slowest post-decode render frame.
    pub max_frame_stage_durations: PreviewRenderStageDurations,
    /// Dominant post-decode render bottleneck inferred from the slowest-frame timings.
    pub primary_bottleneck: PreviewRenderBottleneck,
    /// Typed unavailable-output evidence from GPU candidates and final presentation.
    pub unavailability: crate::app::preview_unavailability::PreviewUnavailabilityEvidenceSnapshot,
}

/// Dominant post-decode preview render bottleneck.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum PreviewRenderBottleneck {
    /// No post-decode render evidence was captured.
    #[default]
    None,
    /// Sequence/plan/media readiness resolution dominated.
    Resolve,
    /// Final viewer/external frame cache lookup dominated.
    FinalCacheLookup,
    /// Working-frame preparation dominated.
    WorkingPreparation,
    /// CPU timeline compositing and property/effect work dominated.
    CpuComposite,
    /// CPU output/color boundary dominated.
    CpuOutputBoundary,
    /// Final raster frame packaging dominated.
    FramePackaging,
}

/// Schema version for preview render performance reports.
pub const PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION: u32 = 2;

/// Default post-decode viewer render budget: one frame should complete in tens of ms.
pub const PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US: u64 = 50_000;

/// Versioned preview render performance report for UI, telemetry, and perf artifacts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewRenderPerformanceReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall post-decode render performance verdict.
    pub verdict: PreviewRenderPerformanceVerdict,
    /// Structured render performance summary used as report evidence.
    pub summary: Option<PreviewRenderPerformanceSummary>,
    /// Structured checks by preview render area.
    pub checks: Vec<PreviewRenderPerformanceCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<PreviewRenderPerformanceRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<PreviewRenderPerformanceAction>,
}

/// Overall post-decode preview render performance verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PreviewRenderPerformanceVerdict {
    /// Preview render met the applied performance budget.
    Pass,
    /// Preview render violated the budget or had no evidence.
    Fail,
}

/// Preview render performance diagnostic area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PreviewRenderPerformanceArea {
    /// Evidence capture and summary availability.
    CaptureIntegrity,
    /// End-to-end post-decode render latency budget.
    LatencyBudget,
    /// Sequence/plan/media readiness resolution.
    Resolve,
    /// Final viewer/external frame cache lookup.
    FinalCacheLookup,
    /// Working-frame preparation before CPU composition.
    WorkingPreparation,
    /// CPU timeline compositing and property/effect work.
    CpuComposite,
    /// CPU output/color boundary.
    CpuOutputBoundary,
    /// Final raster frame packaging.
    FramePackaging,
}

/// Preview render performance check severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PreviewRenderPerformanceSeverity {
    /// Check passed.
    Pass,
    /// Check failed.
    Fail,
}

/// One preview render performance check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewRenderPerformanceCheck {
    /// Diagnostic area for this check.
    pub area: PreviewRenderPerformanceArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: PreviewRenderPerformanceSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One preview render performance root cause.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewRenderPerformanceRootCause {
    /// Diagnostic area for this root cause.
    pub area: PreviewRenderPerformanceArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: PreviewRenderPerformanceSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One preview render performance action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewRenderPerformanceAction {
    /// Diagnostic area for this action.
    pub area: PreviewRenderPerformanceArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

/// Overall preview decode performance verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PreviewDecodePerformanceVerdict {
    /// Preview decode met the applied performance budget.
    Pass,
    /// Preview decode has warning evidence but no hard budget failure.
    Warn,
    /// Preview decode violated the applied performance budget or had no evidence.
    Fail,
}

/// Preview decode performance diagnostic area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PreviewDecodePerformanceArea {
    /// Evidence capture and summary availability.
    CaptureIntegrity,
    /// End-to-end decode latency budget.
    LatencyBudget,
    /// Playback, scrub, and still-frame access-mode-specific decode budgets.
    AccessMode,
    /// Random access, seeking, and GOP pressure.
    RandomAccess,
    /// Codec packet/decode work.
    CodecDecode,
    /// CPU RGBA software scale/copy boundary.
    CpuRgbaBoundary,
    /// Proxy/cache readiness.
    ProxyCache,
    /// Preview decode worker queue scheduling.
    Scheduling,
    /// External process decode path.
    ExternalProcess,
}

/// Preview decode performance check severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PreviewDecodePerformanceSeverity {
    /// Check passed.
    Pass,
    /// Check produced warning evidence.
    Warn,
    /// Check failed.
    Fail,
}

/// One preview decode performance check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodePerformanceCheck {
    /// Diagnostic area for this check.
    pub area: PreviewDecodePerformanceArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: PreviewDecodePerformanceSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One preview decode performance root cause.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodePerformanceRootCause {
    /// Diagnostic area for this root cause.
    pub area: PreviewDecodePerformanceArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: PreviewDecodePerformanceSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One preview decode performance action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewDecodePerformanceAction {
    /// Diagnostic area for this action.
    pub area: PreviewDecodePerformanceArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

impl PreviewColorRejection {
    pub(super) fn new(
        asset_id: AssetId,
        path: PathBuf,
        resolution: InputColorResolution,
        diagnostic_summary: String,
        diagnostic_issue_summary: VideoColorDiagnosticIssueSummary,
    ) -> Self {
        Self {
            asset_id,
            path,
            missing_metadata_policy: resolution.missing_metadata_policy,
            source: resolution.source,
            override_color_space: resolution.override_color_space,
            executable_color_space: resolution.executable_color_space,
            working_color_space: resolution.working_color_space,
            diagnostic_summary,
            diagnostic_issue_summary,
        }
    }
}

impl PreviewDiagnostics {
    /// Return structured preview decode performance evidence when decode activity exists.
    pub fn decode_performance_summary(
        self,
        slow_frame_budget_us: u64,
    ) -> Option<PreviewDecodePerformanceSummary> {
        let decode_successes = self.decode_successes;
        if decode_successes
            .saturating_add(self.decode_failures)
            .saturating_add(self.decode_canceled_jobs)
            .saturating_add(self.playback_current_stalled_expirations)
            == 0
        {
            return None;
        }
        let stage_durations = self.decode_stage_durations;
        let max_frame_stage_durations = self.decode_max_frame_stage_durations;
        let max_frame_queue_wait_us = self.decode_max_frame_queue_wait_us;
        let mut primary_bottleneck = self.decode_max_frame_bottleneck;
        if primary_bottleneck == PreviewDecodeBottleneck::None {
            primary_bottleneck = classify_preview_decode_bottleneck(
                max_frame_stage_durations,
                max_frame_queue_wait_us,
            );
        }
        Some(PreviewDecodePerformanceSummary {
            cpu_budget: self.decode_cpu_budget,
            hardware_decode_admission: self.hardware_decode_admission,
            decode_successes,
            startup_preroll_frames: self.decode_startup_preroll_frames,
            startup_preroll_total_duration_us: self.decode_startup_preroll_total_duration_us,
            startup_preroll_max_duration_us: self.decode_startup_preroll_max_duration_us,
            startup_preroll_queue_wait_total_us: self.decode_startup_preroll_queue_wait_total_us,
            startup_preroll_queue_wait_max_us: self.decode_startup_preroll_queue_wait_max_us,
            decode_failures: self.decode_failures,
            decode_timeout_failures: self.decode_timeout_failures,
            decode_budget_exhausted_failures: self.decode_budget_exhausted_failures,
            cancellation: self.decode_cancellation,
            canceled_jobs: self.decode_canceled_jobs,
            canceled_shutdown_jobs: self.decode_canceled_shutdown_jobs,
            canceled_obsolete_jobs: self.decode_canceled_obsolete_jobs,
            canceled_prefetch_deadline_jobs: self.decode_canceled_prefetch_deadline_jobs,
            canceled_playback_deadline_jobs: self.decode_canceled_playback_deadline_jobs,
            canceled_prefetch_preempted_jobs: self.decode_canceled_prefetch_preempted_jobs,
            canceled_still_preempted_jobs: self.decode_canceled_still_preempted_jobs,
            canceled_unknown_jobs: self.decode_canceled_unknown_jobs,
            canceled_total_duration_us: self.decode_canceled_total_duration_us,
            canceled_max_duration_us: self.decode_canceled_max_duration_us,
            canceled_last_duration_us: self.decode_canceled_last_duration_us,
            cancel_observation_samples: self.decode_cancel_observation_samples,
            cancel_observation_total_us: self.decode_cancel_observation_total_us,
            cancel_observation_max_us: self.decode_cancel_observation_max_us,
            cancel_observation_last_us: self.decode_cancel_observation_last_us,
            canceled_return_latency_total_us: self.decode_canceled_return_latency_total_us,
            canceled_return_latency_max_us: self.decode_canceled_return_latency_max_us,
            canceled_return_latency_last_us: self.decode_canceled_return_latency_last_us,
            in_process_cpu_frames: self.decode_in_process_cpu_frames,
            external_ffmpeg_cpu_rgba_frames: self.decode_external_ffmpeg_cpu_rgba_frames,
            playback_session_ring_hit_frames: self.decode_playback_session_ring_hit_frames,
            cache_hit_frames: self.decode_cache_hit_frames,
            playback_cursor_frames: self.decode_playback_cursor_frames,
            scrub_cursor_frames: self.decode_scrub_cursor_frames,
            random_access_still_frames: self.decode_random_access_still_frames,
            canceled_playback_cursor_jobs: self.decode_canceled_playback_cursor_jobs,
            canceled_scrub_cursor_jobs: self.decode_canceled_scrub_cursor_jobs,
            canceled_random_access_still_jobs: self.decode_canceled_random_access_still_jobs,
            max_duration_us: self.decode_max_duration_us,
            last_duration_us: self.decode_last_duration_us,
            total_duration_us: self.decode_total_duration_us,
            slow_frame_budget_us,
            queue_wait_total_us: self.decode_queue_wait_total_us,
            queue_wait_max_us: self.decode_queue_wait_max_us,
            queue_wait_last_us: self.decode_queue_wait_last_us,
            current_queue_wait_max_us: self.decode_current_queue_wait_max_us,
            prefetch_queue_wait_max_us: self.decode_prefetch_queue_wait_max_us,
            expired_queue_wait: self.decode_expired_queue_wait,
            enqueued_jobs: self.enqueued_jobs,
            prefetch_skipped_current_pending: self.prefetch_skipped_current_pending,
            prefetch_skipped_current_work: self.prefetch_skipped_current_work,
            prefetch_skipped_prefetch_backlog: self.prefetch_skipped_prefetch_backlog,
            queue_full_drops: self.queue_full_drops,
            queue_invalid_access_mode_drops: self.queue_invalid_access_mode_drops,
            queue_evicted_prefetch_jobs: self.queue_evicted_prefetch_jobs,
            queue_evicted_still_jobs: self.queue_evicted_still_jobs,
            interactive_cancel_requests: self.interactive_cancel_requests,
            interactive_cancel_scheduler_requests: self.interactive_cancel_scheduler_requests,
            interactive_cancel_queued_jobs: self.interactive_cancel_queued_jobs,
            queue_canceled_jobs: self.queue_canceled_jobs,
            playback_current_stalled_expirations: self.playback_current_stalled_expirations,
            queue_pruned_obsolete_jobs: self.queue_pruned_obsolete_jobs,
            queue_promoted_current_jobs: self.queue_promoted_current_jobs,
            worker_disconnected_drops: self.worker_disconnected_drops,
            worker_queue: self.worker_queue,
            seeked_frames: self.decode_seeked_frames,
            decoded_frame_count: self.decode_decoded_frame_count,
            max_decoded_frame_count: self.decode_max_decoded_frame_count,
            stage_durations,
            max_frame_stage_durations,
            max_frame_queue_wait_us,
            access_mode_profiles: self.decode_access_mode_profiles,
            slowest_access_mode: self.decode_access_mode_profiles.slowest_access_mode(),
            primary_bottleneck,
            scheduler: self.scheduler,
            playback_schedule: self.playback_schedule,
        })
    }

    /// Return structured post-decode viewer render performance evidence.
    pub fn render_performance_summary(
        self,
        slow_frame_budget_us: u64,
    ) -> Option<PreviewRenderPerformanceSummary> {
        if self.render_timed_frames == 0 && self.unavailability.observations == 0 {
            return None;
        }
        let stage_durations = self.render_stage_durations;
        let max_frame_stage_durations = self.render_max_frame_stage_durations;
        Some(PreviewRenderPerformanceSummary {
            timed_frames: self.render_timed_frames,
            max_duration_us: self.render_max_duration_us,
            last_duration_us: self.render_last_duration_us,
            total_duration_us: self.render_total_duration_us,
            slow_frame_budget_us,
            stage_durations,
            max_frame_stage_durations,
            primary_bottleneck: classify_preview_render_bottleneck(max_frame_stage_durations),
            unavailability: self.unavailability,
        })
    }

    /// Return input color-resolution branch counters using the shared timeline model.
    pub fn input_color_resolution_counts(self) -> InputColorResolutionSourceCounts {
        InputColorResolutionSourceCounts {
            override_count: self.input_color_resolution_override,
            data_texture: self.input_color_resolution_data_texture,
            detected_metadata: self.input_color_resolution_detected_metadata,
            missing_assume_rec709: self.input_color_resolution_missing_assume_rec709,
            missing_rejected: self.input_color_resolution_missing_rejected,
        }
    }

    /// Return the renderer-owned composite color-path summary for preview diagnostics.
    pub fn composite_color_path_summary(self) -> TimelineCompositeColorPathSummary {
        TimelineCompositeDiagnostics {
            elements: self.color_composite_elements,
            float_linear_composites: self.color_composite_float_linear,
            legacy_rgba8_composites: self.color_composite_legacy_rgba8,
            legacy_media_blend_mode: self.color_composite_legacy_media_blend_mode,
            legacy_media_transform: self.color_composite_legacy_media_transform,
            legacy_media_effect: self.color_composite_legacy_media_effect,
            legacy_solid_blend_mode: self.color_composite_legacy_solid_blend_mode,
            legacy_solid_transform: self.color_composite_legacy_solid_transform,
            legacy_solid_effect: self.color_composite_legacy_solid_effect,
            legacy_adjustment_blend_mode: self.color_composite_legacy_adjustment_blend_mode,
            legacy_adjustment_effect: self.color_composite_legacy_adjustment_effect,
            blocked_color_domain_composites: self.color_composite_blocked_domains,
            blocked_media_effect_domain: self.color_composite_blocked_media_effect_domain,
            blocked_solid_effect_domain: self.color_composite_blocked_solid_effect_domain,
            blocked_adjustment_effect_domain: self.color_composite_blocked_adjustment_effect_domain,
            ..TimelineCompositeDiagnostics::default()
        }
        .color_path_summary()
    }

    /// Return color-stage scheduling diagnostics using the shared renderer model.
    pub fn color_stage_diagnostics(self) -> RenderColorStageDiagnostics {
        RenderColorStageDiagnostics {
            total_stages: self.color_stage_total_stages,
            cpu_input_stages: self.color_stage_cpu_input_stages,
            cpu_output_stages: self.color_stage_cpu_output_stages,
            gpu_color_stages: self.color_stage_gpu_color_stages,
            upload_stages: self.color_stage_upload_stages,
            readback_stages: self.color_stage_readback_stages,
            gpu_blockers: self.color_stage_gpu_blockers,
            gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
                shader_module_not_prepared: self.color_stage_gpu_shader_module_blockers,
                ocio_resource_bind_group_not_prepared: self.color_stage_gpu_ocio_resource_blockers,
                fullscreen_wrapper_not_prepared: self.color_stage_gpu_wrapper_blockers,
                render_pipeline_not_prepared: self.color_stage_gpu_render_pipeline_blockers,
                ocio_config_not_loaded: self.color_stage_gpu_ocio_config_blockers,
                ocio_processor_unavailable: self.color_stage_gpu_ocio_processor_blockers,
                ocio_gpu_shader_extraction_failed: self
                    .color_stage_gpu_ocio_shader_extraction_blockers,
            },
            stage_pixels: self.color_stage_pixels,
        }
    }

    /// Return the stable preview color-path health summary when diagnostics have evidence.
    pub fn color_health_summary(self) -> Option<PreviewColorHealthSummary> {
        let counts = self.input_color_resolution_counts();
        let stages = self.color_stage_diagnostics();
        let composite = self.composite_color_path_summary();
        if counts.total() == 0
            && stages.total_stages == 0
            && composite.composite_plans() == 0
            && self.color_rgba8_boundary_calls == 0
            && self.cpu_output_fallback_frames == 0
            && self.preview_gpu_output_blocker_breakdown.total() == 0
        {
            return None;
        }

        Some(PreviewColorHealthSummary {
            composite_plans: composite.composite_plans(),
            detected_metadata: counts.detected_metadata,
            override_count: counts.override_count,
            policy_assumptions: counts.policy_assumptions(),
            data_textures: counts.data_textures(),
            policy_rejections: counts.policy_rejections(),
            explicit_metadata_or_override: counts.explicit_metadata_or_override(),
            cpu_input_stages: stages.cpu_input_stages,
            cpu_output_stages: stages.cpu_output_stages,
            gpu_color_stages: stages.gpu_color_stages,
            gpu_blockers: stages.gpu_blockers,
            gpu_blocker_breakdown: stages.gpu_blocker_breakdown,
            transfer_stages: stages.upload_stages.saturating_add(stages.readback_stages),
            rgba8_boundary_calls: self.color_rgba8_boundary_calls,
            float_linear_composites: composite.float_linear_composites,
            legacy_rgba8_composites: composite.legacy_rgba8_composites,
            legacy_reason_total: composite.legacy_breakdown.total(),
            legacy_breakdown: composite.legacy_breakdown,
            blocked_color_domain_composites: composite.blocked_composites,
            domain_blockers: composite.domain_blockers,
            fully_float_linear: composite.is_fully_float_linear()
                && self.color_composite_plans == composite.composite_plans(),
            gpu_path_ready: stages.gpu_blockers == 0
                && stages.upload_stages == 0
                && stages.readback_stages == 0,
            cpu_output_fallback_frames: self.cpu_output_fallback_frames,
            cpu_output_fallback_pixels: self.cpu_output_fallback_pixels,
            preview_gpu_output_blocker_breakdown: self.preview_gpu_output_blocker_breakdown,
            gpu_compositing: self.gpu_compositing,
        })
    }
}
