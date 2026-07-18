//! Structured preview diagnostics and fail-closed performance/color reports.

mod color_health;
pub use color_health::*;

use super::*;
use std::path::PathBuf;

/// Aggregated CPU-side viewer render stage timings after media decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderStageDurations {
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

impl AppUiPreviewRenderStageDurations {
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
pub struct AppUiPreviewPlaybackScheduleDiagnostics {
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
pub struct AppUiPreviewHardwareDecodeAdmissionDiagnostics {
    /// Request that will be attached to playback decode jobs.
    pub playback_request: PreviewHardwareDecodeRequest,
    /// Whether renderer native decoded-frame import support reached preview scheduling.
    pub renderer_native_import_support_known: bool,
    /// Whether the renderer reports native decoded-frame import support.
    pub renderer_native_import_ready: bool,
    /// Whether the platform reports native texture import support for a renderer-supported handle.
    pub platform_native_import_ready: bool,
    /// Whether playback is allowed to request GPU-resident decode.
    pub native_import_admission_ready: bool,
    /// Stable reason playback cannot request GPU-resident decode, when gated.
    pub admission_blocker: Option<AppUiPreviewHardwareDecodeAdmissionBlocker>,
    /// Whether the platform native texture import probe is available.
    pub platform_discovery_available: bool,
    /// Whether the platform reports a zero-copy native texture path.
    pub platform_zero_copy_supported: bool,
    /// Whether the platform reports a declared low-copy fallback path.
    pub platform_low_copy_fallback_supported: bool,
    /// Renderer-supported native decoder handle-kind count.
    pub renderer_supported_handle_kinds: u8,
    /// Renderer-supported decoded source texture-format count.
    pub renderer_supported_source_texture_formats: u8,
}

impl AppUiPreviewHardwareDecodeAdmissionDiagnostics {
    fn playback_native_import_gated(self) -> bool {
        self.renderer_native_import_support_known
            && !self.native_import_admission_ready
            && self.admission_blocker.is_some()
    }
}

/// Stable hardware-decode admission blocker reported by the app scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewHardwareDecodeAdmissionBlocker {
    /// Renderer runtime has not reported native decoded-frame import support yet.
    RendererSupportUnknown,
    /// Renderer backend has no native decoded-frame import implementation.
    RendererImportUnavailable,
    /// Renderer reports native import but no decoder handle family.
    RendererHandleSupportMissing,
    /// Renderer reports native import but no decoded source texture format.
    RendererSourceTextureFormatSupportMissing,
    /// Platform native texture import probe is unavailable.
    PlatformDiscoveryUnavailable,
    /// Platform can be probed, but neither zero-copy nor low-copy import is declared.
    PlatformCopyPathUnavailable,
    /// Platform import exists but cannot consume any renderer-supported decoder handle.
    PlatformHandleUnsupported,
}

/// Point-in-time preview service counters for local performance diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDiagnostics {
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
    /// Playback current-frame requests expired so buffering cannot hold the shell indefinitely.
    pub playback_current_stalled_expirations: u64,
    /// Requests for a CPU working-frame candidate for the app-window GPU output path.
    pub gpu_preview_candidate_requests: u64,
    /// GPU preview candidate requests that produced a working-frame candidate.
    pub gpu_preview_candidate_ready: u64,
    /// GPU preview candidate requests skipped because the matching external texture is current.
    pub gpu_preview_candidate_current: u64,
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
    /// Coordinated CPU budget used for preview workers and FFmpeg decoder threads.
    pub decode_cpu_budget: PreviewDecodeCpuBudget,
    /// Preview decode workers successfully started for this service.
    pub decode_worker_count: usize,
    /// App-level playback hardware-decode admission state.
    pub hardware_decode_admission: AppUiPreviewHardwareDecodeAdmissionDiagnostics,
    /// Successful background media decodes received by the UI service.
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
    /// Failed background media decodes received by the UI service.
    pub decode_failures: u64,
    /// Failed background media decodes caused by a structured decode timeout.
    pub decode_timeout_failures: u64,
    /// Failed background media decodes caused by access-mode forward-scan budget exhaustion.
    pub decode_budget_exhausted_failures: u64,
    /// Playback-owned cancellation evidence from all semantic frame-work classes.
    pub decode_cancellation: mondrian_playback::FrameCancellationEvidenceReport,
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
    /// Canceled jobs with an attributable external request-to-checkpoint timestamp.
    pub decode_cancel_observation_samples: u64,
    /// Total latency from external cancellation request to the first cooperative checkpoint.
    pub decode_cancel_observation_total_us: u64,
    /// Slowest latency from external cancellation request to the first cooperative checkpoint.
    pub decode_cancel_observation_max_us: u64,
    /// Most recent latency from external cancellation request to the first cooperative checkpoint.
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
    /// Successful decodes served from the preview frame cache.
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
    /// Total time decoded jobs spent waiting in the preview worker queue.
    pub decode_queue_wait_total_us: u64,
    /// Slowest decoded job queue wait.
    pub decode_queue_wait_max_us: u64,
    /// Most recent decoded job queue wait.
    pub decode_queue_wait_last_us: u64,
    /// Slowest current-frame decode queue wait.
    pub decode_current_queue_wait_max_us: u64,
    /// Slowest prefetch decode queue wait.
    pub decode_prefetch_queue_wait_max_us: u64,
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
    pub decode_max_frame_bottleneck: AppUiPreviewDecodeBottleneck,
    /// Decode profile split by playback, scrub, and random-access still modes.
    pub decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles,
    /// Viewer render requests with post-decode stage timing evidence.
    pub render_timed_frames: u64,
    /// Total post-decode viewer render duration in microseconds.
    pub render_total_duration_us: u64,
    /// Slowest post-decode viewer render duration in microseconds.
    pub render_max_duration_us: u64,
    /// Most recent post-decode viewer render duration in microseconds.
    pub render_last_duration_us: u64,
    /// Aggregated CPU-side viewer render stage timings after media decode.
    pub render_stage_durations: AppUiPreviewRenderStageDurations,
    /// CPU-side viewer render stage timings from the slowest post-decode frame.
    pub render_max_frame_stage_durations: AppUiPreviewRenderStageDurations,
    /// UI-thread completion polling passes for decoded preview results.
    pub completion_poll_calls: u64,
    /// Decoded preview results processed by UI-thread completion polling.
    pub completion_poll_results: u64,
    /// Total UI-thread completion polling duration in microseconds.
    pub completion_poll_total_duration_us: u64,
    /// Slowest UI-thread completion polling pass in microseconds.
    pub completion_poll_max_duration_us: u64,
    /// Most recent UI-thread completion polling pass in microseconds.
    pub completion_poll_last_duration_us: u64,
    /// Largest configured completion-result count budget observed by diagnostics.
    pub completion_poll_max_results_per_poll: u64,
    /// Completion polling passes that stopped at the result-count budget.
    pub completion_poll_count_budget_exhaustions: u64,
    /// Completion polling passes that yielded after the UI-thread time budget.
    pub completion_poll_time_budget_exhaustions: u64,
    /// Media preview jobs accepted by the worker queue.
    pub enqueued_jobs: u64,
    /// Playback prefetch passes skipped because visible current-frame media was pending.
    pub prefetch_skipped_current_pending: u64,
    /// Playback prefetch passes skipped because current-frame work was queued or running.
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
    pub playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics,
    /// Final viewer preview frame cache hits.
    pub viewer_frame_cache_hits: u64,
    /// Final viewer preview frame cache misses.
    pub viewer_frame_cache_misses: u64,
    /// Current number of final viewer preview frames in the bounded cache.
    pub viewer_frame_cache_entries: usize,
    /// Current number of frames in the media preview LRU cache.
    pub media_cache_entries: usize,
    /// Current number of keys in the media preview failure LRU cache.
    pub media_failure_entries: usize,
    /// CPU pixel bytes reserved by decoded/media cache entries.
    pub media_cache_reserved_bytes: usize,
    /// Maximum CPU pixel bytes allowed for decoded/media cache entries.
    pub media_cache_byte_budget: usize,
    /// Decoder/GPU resource leases retained by decoded-media cache entries.
    pub media_cache_resource_units: usize,
    /// Maximum decoder/GPU resource leases retained by decoded-media entries.
    pub media_cache_resource_unit_budget: usize,
    /// Decoded/media cache entries evicted by count or byte pressure.
    pub media_cache_evictions: u64,
    /// Decoded/media payloads refused because one entry exceeded the byte budget.
    pub media_cache_oversize_rejections: u64,
    /// Encoded CPU pixel bytes reserved by final Viewer raster cache entries.
    pub viewer_frame_cache_reserved_bytes: usize,
    /// Maximum encoded CPU pixel bytes allowed for final Viewer raster cache entries.
    pub viewer_frame_cache_byte_budget: usize,
    /// Final Viewer raster entries evicted by count or byte pressure.
    pub viewer_frame_cache_evictions: u64,
    /// Final Viewer raster payloads refused because one entry exceeded the byte budget.
    pub viewer_frame_cache_oversize_rejections: u64,
    /// Encoded bytes retained by the non-evictable current/stale Viewer frame.
    pub pinned_viewer_frame_bytes: usize,
    /// CPU pixel bytes retained by an oversize current decoded/media frame.
    pub pinned_media_frame_bytes: usize,
    /// Remembered failure keys evicted by their bounded LRU policy.
    pub media_failure_evictions: u64,
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
        crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    /// GPU compositing capability diagnostics.
    pub gpu_compositing: mondrian_renderer::GpuCompositingDiagnostics,
}

/// Structured color-management rejection captured from the viewer preview path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorRejection {
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
    /// Explicitly detected media color space, if any.
    pub detected_color_space: Option<ColorSpace>,
    /// Sequence working color space active during the decision.
    pub working_color_space: WorkingColorSpace,
    /// Compact media color diagnostic summary from `mondrian-media`.
    pub diagnostic_summary: String,
    /// Machine-readable media color diagnostic issue summary.
    pub diagnostic_issue_summary: VideoColorDiagnosticIssueSummary,
}

/// Stable preview color-path health summary for perf JSONL and diagnostics tooling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthSummary {
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
        crate::app_ui::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    /// GPU compositing capability diagnostics.
    pub gpu_compositing: mondrian_renderer::GpuCompositingDiagnostics,
}

/// Fixed latency buckets for compact preview decode distribution diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodeLatencyBuckets {
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

impl AppUiPreviewDecodeLatencyBuckets {
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

    fn estimated_p95_upper_bound_us(self) -> u64 {
        self.estimated_quantile_upper_bound_us(95)
    }

    fn estimated_quantile_upper_bound_us(self, percentile: u64) -> u64 {
        let total = self.total();
        if total == 0 {
            return 0;
        }
        let rank = total.saturating_mul(percentile.min(100)).saturating_add(99) / 100;
        let mut cumulative = 0_u64;
        for (count, upper_bound_us) in [
            (self.le_10ms, 10_000),
            (self.le_16ms, 16_000),
            (self.le_25ms, 25_000),
            (self.le_40ms, 40_000),
            (self.le_50ms, 50_000),
            (self.le_80ms, 80_000),
            (self.gt_80ms, 80_001),
        ] {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= rank {
                return upper_bound_us;
            }
        }
        80_001
    }
}

/// Preview decode profile for one access mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodeAccessModeProfile {
    /// Successful decode/cache results for this access mode.
    pub frames: u64,
    /// Successful in-process CPU RGBA8/f32 results for this access mode.
    pub in_process_cpu_frames: u64,
    /// Successful external ffmpeg CPU RGBA results for this access mode.
    pub external_ffmpeg_cpu_rgba_frames: u64,
    /// Successful playback ring hits for this access mode.
    pub playback_session_ring_hit_frames: u64,
    /// Successful preview cache hits for this access mode.
    pub cache_hit_frames: u64,
    /// Total end-to-end decode duration for this access mode.
    pub total_duration_us: u64,
    /// Slowest end-to-end decode duration for this access mode.
    pub max_duration_us: u64,
    /// Most recent end-to-end decode duration for this access mode.
    pub last_duration_us: u64,
    /// Fixed distribution buckets for end-to-end decode duration.
    pub latency_buckets: AppUiPreviewDecodeLatencyBuckets,
    /// Total worker-queue wait before decode started for this access mode.
    pub queue_wait_total_us: u64,
    /// Slowest worker-queue wait before decode started for this access mode.
    pub queue_wait_max_us: u64,
    /// Most recent worker-queue wait before decode started for this access mode.
    pub queue_wait_last_us: u64,
    /// Fixed distribution buckets for worker-queue wait.
    pub queue_wait_buckets: AppUiPreviewDecodeLatencyBuckets,
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
    /// Total worker execution time spent in canceled decode jobs for this access mode.
    pub canceled_total_duration_us: u64,
    /// Slowest worker execution time for a canceled decode job in this access mode.
    pub canceled_max_duration_us: u64,
    /// Most recent canceled decode worker execution time in this access mode.
    pub canceled_last_duration_us: u64,
    /// Canceled jobs with attributable request-to-checkpoint timing in this access mode.
    pub cancel_observation_samples: u64,
    /// Total external cancellation request-to-checkpoint latency for this access mode.
    pub cancel_observation_total_us: u64,
    /// Slowest external cancellation request-to-checkpoint latency for this access mode.
    pub cancel_observation_max_us: u64,
    /// Most recent external cancellation request-to-checkpoint latency for this access mode.
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
    /// Decode requests for this access mode that required a seek.
    pub seeked_frames: u64,
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
    /// Decode results that opened or replaced the access-mode-local session.
    pub session_opened_frames: u64,
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
    pub max_frame_bottleneck: AppUiPreviewDecodeBottleneck,
}

impl AppUiPreviewDecodeAccessModeProfile {
    fn mode_local_evidence_frames(self) -> u64 {
        self.frames.saturating_sub(self.cache_hit_frames)
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
            }
            PreviewDecodePath::PreviewCacheHit => {
                self.cache_hit_frames = self.cache_hit_frames.saturating_add(1);
            }
        }
        self.total_duration_us = self.total_duration_us.saturating_add(diagnostics.elapsed_us);
        self.latency_buckets.record(diagnostics.elapsed_us);
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
        if diagnostics.session_reused {
            self.session_reused_frames = self.session_reused_frames.saturating_add(1);
        } else {
            self.session_opened_frames = self.session_opened_frames.saturating_add(1);
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
        self.queue_wait_total_us = self.queue_wait_total_us.saturating_add(queue_wait_us);
        self.queue_wait_max_us = self.queue_wait_max_us.max(queue_wait_us);
        self.queue_wait_last_us = queue_wait_us;
        self.queue_wait_buckets.record(queue_wait_us);
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
        self.cancel_observation_samples = profile.request_to_checkpoint.samples;
        self.cancel_observation_total_us = profile.request_to_checkpoint.total_us;
        self.cancel_observation_max_us = profile.request_to_checkpoint.max_us;
        self.cancel_observation_last_us = profile.request_to_checkpoint.last_us;
        self.canceled_return_latency_total_us = profile.checkpoint_to_return.total_us;
        self.canceled_return_latency_max_us = profile.checkpoint_to_return.max_us;
        self.canceled_return_latency_last_us = profile.checkpoint_to_return.last_us;
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
            MediaPreviewFailureReason::DecodeError => {}
        }
    }
}

/// Preview decode profiles split by access mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodeAccessModeProfiles {
    /// Sustained playback and forward-prefetch decode profile.
    pub playback_cursor: AppUiPreviewDecodeAccessModeProfile,
    /// Latest-wins interactive scrub decode profile.
    pub scrub_cursor: AppUiPreviewDecodeAccessModeProfile,
    /// Deterministic still-frame/random-access decode profile.
    pub random_access_still: AppUiPreviewDecodeAccessModeProfile,
}

impl AppUiPreviewDecodeAccessModeProfiles {
    fn profile_for(
        self,
        access_mode: PreviewDecodeAccessMode,
    ) -> AppUiPreviewDecodeAccessModeProfile {
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

    fn named_profiles(self) -> [(PreviewDecodeAccessMode, AppUiPreviewDecodeAccessModeProfile); 3] {
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
pub struct AppUiPreviewDecodePerformanceSummary {
    /// Coordinated CPU budget used for preview workers and FFmpeg decoder threads.
    pub cpu_budget: PreviewDecodeCpuBudget,
    /// App-level playback hardware-decode admission state.
    pub hardware_decode_admission: AppUiPreviewHardwareDecodeAdmissionDiagnostics,
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
    /// Canceled jobs with an attributable external request-to-checkpoint timestamp.
    pub cancel_observation_samples: u64,
    /// Total latency from external cancellation request to the first cooperative checkpoint.
    pub cancel_observation_total_us: u64,
    /// Slowest latency from external cancellation request to the first cooperative checkpoint.
    pub cancel_observation_max_us: u64,
    /// Most recent latency from external cancellation request to the first cooperative checkpoint.
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
    /// Successful decodes served from preview cache.
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
    /// Total time decoded jobs spent waiting in the preview worker queue.
    pub queue_wait_total_us: u64,
    /// Slowest decoded job queue wait.
    pub queue_wait_max_us: u64,
    /// Most recent decoded job queue wait.
    pub queue_wait_last_us: u64,
    /// Slowest current-frame decode queue wait.
    pub current_queue_wait_max_us: u64,
    /// Slowest prefetch decode queue wait.
    pub prefetch_queue_wait_max_us: u64,
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
    pub access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles,
    /// Access mode that produced the slowest successful decode frame.
    pub slowest_access_mode: Option<PreviewDecodeAccessMode>,
    /// Dominant stage inferred from the slowest successful decode frame.
    pub primary_bottleneck: AppUiPreviewDecodeBottleneck,
    /// Scheduler-side access-mode/drop/stale diagnostics captured with decode evidence.
    pub scheduler: MediaPreviewSchedulerDiagnostics,
    /// Playback-clock deadline and forward-prefetch scheduling contract.
    pub playback_schedule: AppUiPreviewPlaybackScheduleDiagnostics,
}

/// Dominant preview decode bottleneck inferred from stage diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewDecodeBottleneck {
    /// No decode evidence was captured.
    #[default]
    None,
    /// Opening or reconfiguring the decode session dominated.
    SessionOpen,
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
pub const APP_UI_PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION: u32 = 31;

/// Default preview slow-frame budget: one frame should complete in tens of ms.
pub const APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US: u64 = 50_000;

/// Versioned preview decode performance report for UI, telemetry, and perf artifacts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall preview decode performance verdict.
    pub verdict: AppUiPreviewDecodePerformanceVerdict,
    /// Access modes this report profile required to be sampled.
    pub required_access_modes: Vec<PreviewDecodeAccessMode>,
    /// Structured decode performance summary used as report evidence.
    pub summary: Option<AppUiPreviewDecodePerformanceSummary>,
    /// Structured checks by preview decode area.
    pub checks: Vec<AppUiPreviewDecodePerformanceCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<AppUiPreviewDecodePerformanceRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<AppUiPreviewDecodePerformanceAction>,
}

/// Stable preview render performance summary for post-decode viewer work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceSummary {
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
    pub stage_durations: AppUiPreviewRenderStageDurations,
    /// Stage timings from the slowest post-decode render frame.
    pub max_frame_stage_durations: AppUiPreviewRenderStageDurations,
    /// Dominant post-decode render bottleneck inferred from the slowest-frame timings.
    pub primary_bottleneck: AppUiPreviewRenderBottleneck,
}

/// Dominant post-decode preview render bottleneck.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewRenderBottleneck {
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
pub const APP_UI_PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION: u32 = 1;

/// Default post-decode viewer render budget: one frame should complete in tens of ms.
pub const APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US: u64 = 50_000;

/// Versioned preview render performance report for UI, telemetry, and perf artifacts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall post-decode render performance verdict.
    pub verdict: AppUiPreviewRenderPerformanceVerdict,
    /// Structured render performance summary used as report evidence.
    pub summary: Option<AppUiPreviewRenderPerformanceSummary>,
    /// Structured checks by preview render area.
    pub checks: Vec<AppUiPreviewRenderPerformanceCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<AppUiPreviewRenderPerformanceRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<AppUiPreviewRenderPerformanceAction>,
}

/// Overall post-decode preview render performance verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewRenderPerformanceVerdict {
    /// Preview render met the applied performance budget.
    Pass,
    /// Preview render violated the budget or had no evidence.
    Fail,
}

/// Preview render performance diagnostic area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewRenderPerformanceArea {
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
pub enum AppUiPreviewRenderPerformanceSeverity {
    /// Check passed.
    Pass,
    /// Check failed.
    Fail,
}

/// One preview render performance check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceCheck {
    /// Diagnostic area for this check.
    pub area: AppUiPreviewRenderPerformanceArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: AppUiPreviewRenderPerformanceSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One preview render performance root cause.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceRootCause {
    /// Diagnostic area for this root cause.
    pub area: AppUiPreviewRenderPerformanceArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: AppUiPreviewRenderPerformanceSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One preview render performance action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewRenderPerformanceAction {
    /// Diagnostic area for this action.
    pub area: AppUiPreviewRenderPerformanceArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

/// Overall preview decode performance verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewDecodePerformanceVerdict {
    /// Preview decode met the applied performance budget.
    Pass,
    /// Preview decode has warning evidence but no hard budget failure.
    Warn,
    /// Preview decode violated the applied performance budget or had no evidence.
    Fail,
}

/// Preview decode performance diagnostic area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewDecodePerformanceArea {
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
pub enum AppUiPreviewDecodePerformanceSeverity {
    /// Check passed.
    Pass,
    /// Check produced warning evidence.
    Warn,
    /// Check failed.
    Fail,
}

/// One preview decode performance check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceCheck {
    /// Diagnostic area for this check.
    pub area: AppUiPreviewDecodePerformanceArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: AppUiPreviewDecodePerformanceSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One preview decode performance root cause.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceRootCause {
    /// Diagnostic area for this root cause.
    pub area: AppUiPreviewDecodePerformanceArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: AppUiPreviewDecodePerformanceSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One preview decode performance action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDecodePerformanceAction {
    /// Diagnostic area for this action.
    pub area: AppUiPreviewDecodePerformanceArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

impl AppUiPreviewDecodePerformanceSummary {
    /// Build the versioned preview decode performance report for this summary.
    pub fn performance_report(
        self,
        profile: impl Into<String>,
    ) -> AppUiPreviewDecodePerformanceReport {
        build_preview_decode_performance_report(
            Some(self),
            profile,
            APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
        )
    }
}

impl AppUiPreviewRenderPerformanceSummary {
    /// Build the versioned preview render performance report for this summary.
    pub fn performance_report(
        self,
        profile: impl Into<String>,
    ) -> AppUiPreviewRenderPerformanceReport {
        build_preview_render_performance_report(
            Some(self),
            profile,
            APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
        )
    }
}

/// Build a versioned preview render performance report from an optional summary.
pub fn build_preview_render_performance_report(
    summary: Option<AppUiPreviewRenderPerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
) -> AppUiPreviewRenderPerformanceReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();

    push_render_bool_check(
        &mut checks,
        AppUiPreviewRenderPerformanceArea::CaptureIntegrity,
        "preview_render_evidence_present",
        summary.map(|summary| summary.timed_frames > 0).unwrap_or(false),
    );

    if let Some(mut summary) = summary {
        summary.slow_frame_budget_us = slow_frame_budget_us;
        summary.primary_bottleneck =
            classify_preview_render_bottleneck(summary.max_frame_stage_durations);
        push_render_max_check(
            &mut checks,
            AppUiPreviewRenderPerformanceArea::LatencyBudget,
            "preview_render_max_frame_us",
            summary.max_duration_us,
            slow_frame_budget_us,
        );
        push_preview_render_root_causes_and_actions(summary, &mut root_causes, &mut actions);

        let verdict = preview_render_verdict(&checks);
        return AppUiPreviewRenderPerformanceReport {
            schema_version: APP_UI_PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION,
            profile: profile.into(),
            verdict,
            summary: Some(summary),
            checks,
            root_causes,
            actions,
        };
    }

    push_render_root_cause_with_action(
        &mut root_causes,
        &mut actions,
        AppUiPreviewRenderPerformanceArea::CaptureIntegrity,
        "missing_preview_render_evidence",
        "preview_render_evidence_present=false".to_owned(),
        "capture_preview_render_stage_durations",
        "Ensure viewer preview records post-decode render stage timings from the real playback path.",
    );

    let verdict = preview_render_verdict(&checks);
    AppUiPreviewRenderPerformanceReport {
        schema_version: APP_UI_PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION,
        profile: profile.into(),
        verdict,
        summary: None,
        checks,
        root_causes,
        actions,
    }
}

/// Build a versioned preview decode performance report from an optional summary.
pub fn build_preview_decode_performance_report(
    summary: Option<AppUiPreviewDecodePerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
) -> AppUiPreviewDecodePerformanceReport {
    build_preview_decode_performance_report_with_required_access_modes(
        summary,
        profile,
        slow_frame_budget_us,
        &[],
    )
}

/// Build a preview decode report with an explicit access-mode coverage contract.
///
/// Perf smokes use this when a scenario is only valid if selected access modes
/// actually reached the media decode boundary. General UI diagnostics should
/// use [`build_preview_decode_performance_report`] so idle profiles do not fail
/// merely because they did not exercise every mode.
pub fn build_preview_decode_performance_report_with_required_access_modes(
    summary: Option<AppUiPreviewDecodePerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
    required_access_modes: &[PreviewDecodeAccessMode],
) -> AppUiPreviewDecodePerformanceReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();
    let required_access_modes = required_access_modes.to_vec();

    push_decode_bool_check(
        &mut checks,
        AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
        "preview_decode_evidence_present",
        summary
            .map(|summary| {
                summary
                    .decode_successes
                    .saturating_add(summary.decode_failures)
                    .saturating_add(summary.canceled_jobs)
                    .saturating_add(summary.playback_current_stalled_expirations)
                    > 0
            })
            .unwrap_or(false),
    );

    if let Some(mut summary) = summary {
        summary.slow_frame_budget_us = slow_frame_budget_us;
        summary.primary_bottleneck = classify_preview_decode_bottleneck(
            summary.max_frame_stage_durations,
            summary.max_frame_queue_wait_us,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_max_frame_us",
            summary.max_duration_us,
            slow_frame_budget_us,
        );
        push_preview_decode_access_mode_coverage_checks(
            &mut checks,
            summary,
            &required_access_modes,
        );
        push_preview_decode_access_mode_checks(&mut checks, summary, slow_frame_budget_us);
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_timeout_failures",
            summary.decode_timeout_failures,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_forward_budget_exhausted_failures",
            summary.decode_budget_exhausted_failures,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_max_decoded_frame_count",
            summary.max_decoded_frame_count,
            1,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_seeked_frames",
            summary.seeked_frames,
            0,
        );
        push_decode_warn_min_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::ProxyCache,
            "preview_decode_cache_hit_frames",
            summary
                .cache_hit_frames
                .saturating_add(summary.playback_session_ring_hit_frames),
            1,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_wait_max_us",
            summary.queue_wait_max_us,
            slow_frame_budget_us,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_expired_playback_current_queue",
            (summary.worker_queue.queued_expired_playback_current_jobs as u64)
                .saturating_add(summary.worker_queue.dropped_expired_playback_current_jobs),
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_current_stall_expirations",
            summary.playback_current_stalled_expirations,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_sustained_pressure_events",
            summary.playback_schedule.sustained_pressure_events,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_hardware_decode_admission_gated",
            u64::from(summary.hardware_decode_admission.playback_native_import_gated()),
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_playback_hardware_fallback_not_engaged",
            summary
                .access_mode_profiles
                .playback_cursor
                .hardware_decode_fallback_not_engaged_frames(),
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_native_import_unavailable_playback_frames",
            summary.playback_schedule.current_native_import_unavailable_decisions,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_hardware_fallback_not_engaged_decisions",
            summary.playback_schedule.current_hardware_fallback_not_engaged_decisions,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_deadline_cancellations",
            summary.canceled_prefetch_deadline_jobs,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_preempted_by_current_cancellations",
            summary.canceled_prefetch_preempted_jobs,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_deadline_invalid_frame_rate",
            summary.playback_schedule.current_deadline_missing_frame_rate,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_window_invalid_frame_rate",
            summary.playback_schedule.forward_prefetch_invalid_frame_rate,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_still_preempted_by_realtime_cancellations",
            summary.canceled_still_preempted_jobs,
            0,
        );
        let cancellation_policy = mondrian_playback::FrameCancellationPolicy::default();
        let cancellation_gate = mondrian_playback::evaluate_frame_cancellation(
            summary.cancellation,
            cancellation_policy,
        );
        push_decode_bool_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancellation_gate",
            cancellation_gate.passed,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancel_observation_max_us",
            summary.cancellation.all.request_to_checkpoint.max_us,
            cancellation_policy.max_request_to_checkpoint.as_micros() as u64,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancel_unknown_causes",
            summary.cancellation.all.unknown,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancel_missing_request_evidence",
            summary
                .cancellation
                .all
                .cancellations
                .saturating_sub(summary.cancellation.all.unknown)
                .saturating_sub(summary.cancellation.all.request_to_checkpoint.samples),
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_cancel_return_latency_max_us",
            summary.cancellation.playback.checkpoint_to_return.max_us,
            cancellation_policy.max_playback_return.as_micros() as u64,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_interactive_cancel_return_latency_max_us",
            summary.cancellation.interactive.checkpoint_to_return.max_us,
            cancellation_policy.max_interactive_return.as_micros() as u64,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_still_cancel_return_latency_max_us",
            summary.cancellation.still.checkpoint_to_return.max_us,
            cancellation_policy.max_still_return.as_micros() as u64,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_queue_full_drops",
            summary.queue_full_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_invalid_access_mode_drops",
            summary.queue_invalid_access_mode_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_disconnected_drops",
            summary.worker_disconnected_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_invalid_access_mode_requests",
            summary.scheduler.dropped_invalid_access_mode_requests,
            0,
        );
        push_decode_max_check(
            &mut checks,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_broker_clock_regressions",
            summary.scheduler.clock_regressions,
            0,
        );

        push_preview_decode_root_causes_and_actions(
            summary,
            &required_access_modes,
            &mut root_causes,
            &mut actions,
        );

        let verdict = preview_decode_verdict(&checks);
        return AppUiPreviewDecodePerformanceReport {
            schema_version: APP_UI_PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION,
            profile: profile.into(),
            verdict,
            required_access_modes,
            summary: Some(summary),
            checks,
            root_causes,
            actions,
        };
    }

    push_decode_root_cause_with_action(
        &mut root_causes,
        &mut actions,
        AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
        "missing_preview_decode_evidence",
        "preview_decode_evidence_present=false".to_owned(),
        "capture_preview_decode_diagnostics",
        "Ensure preview media jobs record decode diagnostics from the real playback path.",
        AppUiPreviewDecodePerformanceSeverity::Fail,
    );

    let verdict = preview_decode_verdict(&checks);
    AppUiPreviewDecodePerformanceReport {
        schema_version: APP_UI_PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION,
        profile: profile.into(),
        verdict,
        required_access_modes,
        summary: None,
        checks,
        root_causes,
        actions,
    }
}

fn preview_decode_verdict(
    checks: &[AppUiPreviewDecodePerformanceCheck],
) -> AppUiPreviewDecodePerformanceVerdict {
    if checks
        .iter()
        .any(|check| check.severity == AppUiPreviewDecodePerformanceSeverity::Fail)
    {
        AppUiPreviewDecodePerformanceVerdict::Fail
    } else if checks
        .iter()
        .any(|check| check.severity == AppUiPreviewDecodePerformanceSeverity::Warn)
    {
        AppUiPreviewDecodePerformanceVerdict::Warn
    } else {
        AppUiPreviewDecodePerformanceVerdict::Pass
    }
}

fn preview_render_verdict(
    checks: &[AppUiPreviewRenderPerformanceCheck],
) -> AppUiPreviewRenderPerformanceVerdict {
    if checks
        .iter()
        .any(|check| check.severity == AppUiPreviewRenderPerformanceSeverity::Fail)
    {
        AppUiPreviewRenderPerformanceVerdict::Fail
    } else {
        AppUiPreviewRenderPerformanceVerdict::Pass
    }
}

fn push_render_max_check(
    checks: &mut Vec<AppUiPreviewRenderPerformanceCheck>,
    area: AppUiPreviewRenderPerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewRenderPerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            AppUiPreviewRenderPerformanceSeverity::Fail
        } else {
            AppUiPreviewRenderPerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_render_bool_check(
    checks: &mut Vec<AppUiPreviewRenderPerformanceCheck>,
    area: AppUiPreviewRenderPerformanceArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(AppUiPreviewRenderPerformanceCheck {
        area,
        code,
        severity: if passed {
            AppUiPreviewRenderPerformanceSeverity::Pass
        } else {
            AppUiPreviewRenderPerformanceSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_decode_max_check(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    area: AppUiPreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            AppUiPreviewDecodePerformanceSeverity::Fail
        } else {
            AppUiPreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_warn_max_check(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    area: AppUiPreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            AppUiPreviewDecodePerformanceSeverity::Warn
        } else {
            AppUiPreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_warn_min_check(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    area: AppUiPreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed < limit {
            AppUiPreviewDecodePerformanceSeverity::Warn
        } else {
            AppUiPreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_bool_check(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    area: AppUiPreviewDecodePerformanceArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(AppUiPreviewDecodePerformanceCheck {
        area,
        code,
        severity: if passed {
            AppUiPreviewDecodePerformanceSeverity::Pass
        } else {
            AppUiPreviewDecodePerformanceSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_preview_decode_access_mode_checks(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    summary: AppUiPreviewDecodePerformanceSummary,
    slow_frame_budget_us: u64,
) {
    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        if profile.frames == 0 && profile.queue_wait_max_us == 0 {
            continue;
        }
        if profile.frames > 0 {
            let p95_upper_bound_us = profile.latency_buckets.estimated_p95_upper_bound_us();
            checks.push(AppUiPreviewDecodePerformanceCheck {
                area: AppUiPreviewDecodePerformanceArea::AccessMode,
                code: preview_decode_access_mode_budget_code(access_mode),
                severity: if profile.max_duration_us > slow_frame_budget_us {
                    AppUiPreviewDecodePerformanceSeverity::Fail
                } else {
                    AppUiPreviewDecodePerformanceSeverity::Pass
                },
                observed: profile.max_duration_us,
                limit: Some(slow_frame_budget_us),
            });
            checks.push(AppUiPreviewDecodePerformanceCheck {
                area: AppUiPreviewDecodePerformanceArea::AccessMode,
                code: preview_decode_access_mode_p95_budget_code(access_mode),
                severity: if p95_upper_bound_us > slow_frame_budget_us {
                    AppUiPreviewDecodePerformanceSeverity::Fail
                } else {
                    AppUiPreviewDecodePerformanceSeverity::Pass
                },
                observed: p95_upper_bound_us,
                limit: Some(slow_frame_budget_us),
            });
            if access_mode == PreviewDecodeAccessMode::ScrubCursor {
                checks.push(AppUiPreviewDecodePerformanceCheck {
                    area: AppUiPreviewDecodePerformanceArea::AccessMode,
                    code: "preview_decode_scrub_cursor_bounded_any_seek_strategy",
                    severity: if profile.bounded_any_seek_strategy_frames == profile.frames {
                        AppUiPreviewDecodePerformanceSeverity::Pass
                    } else {
                        AppUiPreviewDecodePerformanceSeverity::Fail
                    },
                    observed: profile.bounded_any_seek_strategy_frames,
                    limit: Some(profile.frames),
                });
                checks.push(AppUiPreviewDecodePerformanceCheck {
                    area: AppUiPreviewDecodePerformanceArea::AccessMode,
                    code: "preview_decode_scrub_cursor_any_seek_window_ms",
                    severity: if profile.any_seek_window_ms_max > 0 {
                        AppUiPreviewDecodePerformanceSeverity::Pass
                    } else {
                        AppUiPreviewDecodePerformanceSeverity::Fail
                    },
                    observed: profile.any_seek_window_ms_max,
                    limit: Some(1),
                });
            }
        }
        let queue_wait_p95_upper_bound_us =
            profile.queue_wait_buckets.estimated_p95_upper_bound_us();
        checks.push(AppUiPreviewDecodePerformanceCheck {
            area: AppUiPreviewDecodePerformanceArea::AccessMode,
            code: preview_decode_access_mode_queue_wait_budget_code(access_mode),
            severity: if profile.queue_wait_max_us > slow_frame_budget_us {
                AppUiPreviewDecodePerformanceSeverity::Warn
            } else {
                AppUiPreviewDecodePerformanceSeverity::Pass
            },
            observed: profile.queue_wait_max_us,
            limit: Some(slow_frame_budget_us),
        });
        checks.push(AppUiPreviewDecodePerformanceCheck {
            area: AppUiPreviewDecodePerformanceArea::AccessMode,
            code: preview_decode_access_mode_queue_wait_p95_budget_code(access_mode),
            severity: if queue_wait_p95_upper_bound_us > slow_frame_budget_us {
                AppUiPreviewDecodePerformanceSeverity::Warn
            } else {
                AppUiPreviewDecodePerformanceSeverity::Pass
            },
            observed: queue_wait_p95_upper_bound_us,
            limit: Some(slow_frame_budget_us),
        });
    }
}

fn push_preview_decode_access_mode_coverage_checks(
    checks: &mut Vec<AppUiPreviewDecodePerformanceCheck>,
    summary: AppUiPreviewDecodePerformanceSummary,
    required_access_modes: &[PreviewDecodeAccessMode],
) {
    for access_mode in required_access_modes {
        let profile = summary.access_mode_profiles.profile_for(*access_mode);
        push_decode_bool_check(
            checks,
            AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
            preview_decode_access_mode_coverage_code(*access_mode),
            profile.frames > 0,
        );
        if profile.frames > 0 {
            push_decode_bool_check(
                checks,
                AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
                preview_decode_access_mode_local_coverage_code(*access_mode),
                profile.mode_local_evidence_frames() > 0,
            );
        }
    }
}

fn preview_decode_access_mode_budget_code(access_mode: PreviewDecodeAccessMode) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => "preview_decode_playback_cursor_max_frame_us",
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_max_frame_us",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_max_frame_us"
        }
    }
}

fn preview_decode_access_mode_queue_wait_budget_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_queue_wait_max_us"
        }
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_queue_wait_max_us",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_queue_wait_max_us"
        }
    }
}

fn preview_decode_access_mode_p95_budget_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => "preview_decode_playback_cursor_p95_frame_us",
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_p95_frame_us",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_p95_frame_us"
        }
    }
}

fn preview_decode_access_mode_queue_wait_p95_budget_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_queue_wait_p95_us"
        }
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_queue_wait_p95_us",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_queue_wait_p95_us"
        }
    }
}

fn preview_decode_access_mode_coverage_code(access_mode: PreviewDecodeAccessMode) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => "preview_decode_playback_cursor_sampled",
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_sampled",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_sampled"
        }
    }
}

fn preview_decode_access_mode_local_coverage_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_mode_local_sampled"
        }
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_mode_local_sampled",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_mode_local_sampled"
        }
    }
}

fn push_preview_decode_root_causes_and_actions(
    summary: AppUiPreviewDecodePerformanceSummary,
    required_access_modes: &[PreviewDecodeAccessMode],
    root_causes: &mut Vec<AppUiPreviewDecodePerformanceRootCause>,
    actions: &mut Vec<AppUiPreviewDecodePerformanceAction>,
) {
    for access_mode in required_access_modes {
        let profile = summary.access_mode_profiles.profile_for(*access_mode);
        if profile.frames > 0 {
            continue;
        }
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
            "preview_decode_required_access_mode_missing",
            format!(
                "access_mode={} required_access_modes={}",
                access_mode.as_str(),
                required_access_modes
                    .iter()
                    .map(|mode| mode.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            "exercise_required_preview_access_modes",
            "Drive this perf profile through every required preview access mode before treating the report as representative.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    for access_mode in required_access_modes {
        let profile = summary.access_mode_profiles.profile_for(*access_mode);
        if profile.frames == 0 || profile.mode_local_evidence_frames() > 0 {
            continue;
        }
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CaptureIntegrity,
            "preview_decode_required_access_mode_cache_only",
            format!(
                "access_mode={} frames={} cache_hit_frames={} mode_local_evidence_frames=0",
                access_mode.as_str(),
                profile.frames,
                profile.cache_hit_frames
            ),
            "exercise_required_preview_access_modes_without_global_cache",
            "Drive required preview access modes through mode-local decode evidence; process-global cache hits alone do not prove the access-mode contract.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }

    if summary.max_duration_us > summary.slow_frame_budget_us {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_frame_over_budget",
            format!(
                "max_duration_us={} slow_frame_budget_us={} max_frame_queue_wait_us={} primary_bottleneck={:?} slowest_access_mode={}",
                summary.max_duration_us,
                summary.slow_frame_budget_us,
                summary.max_frame_queue_wait_us,
                summary.primary_bottleneck,
                summary
                    .slowest_access_mode
                    .map(PreviewDecodeAccessMode::as_str)
                    .unwrap_or("None")
            ),
            "inspect_preview_decode_stage_durations",
            "Inspect preview decode stage timings before changing color or render code.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }

    let scrub_profile = summary.access_mode_profiles.scrub_cursor;
    if scrub_profile.frames > 0
        && scrub_profile.bounded_any_seek_strategy_frames < scrub_profile.frames
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_scrub_cursor_not_using_low_latency_seek",
            format!(
                "scrub_frames={} bounded_any_seek_strategy_frames={} keyframe_seek_strategy_frames={}",
                scrub_profile.frames,
                scrub_profile.bounded_any_seek_strategy_frames,
                scrub_profile.keyframe_seek_strategy_frames
            ),
            "route_scrub_decode_through_bounded_any_seek",
            "Ensure ScrubCursor decode uses the low-latency bounded-any seek strategy instead of playback/still exact seek semantics.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scrub_profile.frames > 0 && scrub_profile.any_seek_window_ms_max == 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_scrub_cursor_missing_any_seek_window",
            format!(
                "scrub_frames={} bounded_any_seek_strategy_frames={} any_seek_window_ms_max=0 forward_reuse_frame_window_max={} forward_decode_budget_frames_max={}",
                scrub_profile.frames,
                scrub_profile.bounded_any_seek_strategy_frames,
                scrub_profile.forward_reuse_frame_window_max,
                scrub_profile.forward_decode_budget_frames_max
            ),
            "restore_scrub_bounded_any_seek_window",
            "Ensure ScrubCursor decode keeps a non-zero bounded-any seek window so active scrubbing does not fall back to exact still-frame seeking.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scrub_profile.frames > 0
        && scrub_profile.max_duration_us > summary.slow_frame_budget_us
        && scrub_profile.seeked_frames > 0
        && scrub_profile.seek_index_available_frames == 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_scrub_cursor_without_seek_index_evidence",
            format!(
                "scrub_frames={} seeked_frames={} max_duration_us={} slow_frame_budget_us={} seek_index_available_frames=0 seek_index_used_frames={} seek_index_keyframes_max={} seek_index_observed_packets_max={} seek_index_probe_backed_frames={} seek_index_session_observed_frames={}",
                scrub_profile.frames,
                scrub_profile.seeked_frames,
                scrub_profile.max_duration_us,
                summary.slow_frame_budget_us,
                scrub_profile.seek_index_used_frames,
                scrub_profile.seek_index_keyframes_max,
                scrub_profile.seek_index_observed_packets_max,
                scrub_profile.seek_index_probe_backed_frames,
                scrub_profile.seek_index_session_observed_frames
            ),
            "build_preview_seek_index_evidence",
            "Collect keyframe/GOP seek evidence for scrub sessions before widening seek windows or adding decode workers.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        if profile.frames == 0 || profile.max_duration_us <= summary.slow_frame_budget_us {
            continue;
        }
        let p95_upper_bound_us = profile.latency_buckets.estimated_p95_upper_bound_us();
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_access_mode_over_budget",
            format!(
                "access_mode={} frames={} max_duration_us={} p95_upper_bound_us={} total_duration_us={} queue_wait_max_us={} queue_wait_total_us={} max_frame_queue_wait_us={} max_frame_bottleneck={:?} seeked_frames={} keyframe_seek_strategy_frames={} bounded_any_seek_strategy_frames={} forward_reuse_frame_window_max={} forward_decode_budget_frames_max={} any_seek_window_ms_max={} session_reused_frames={} session_opened_frames={} forward_reused_frames={} seek_index_available_frames={} seek_index_used_frames={} seek_index_keyframes_max={} seek_index_observed_packets_max={} seek_index_probe_backed_frames={} seek_index_session_observed_frames={} hardware_decode_active_frames={} zero_copy_active_frames={} gpu_texture_resident_frames={} decoded_nv12_surface_frames={} decoded_p010_surface_frames={} hardware_decode_texture_residency_blocker_frames={} hardware_decode_auto_requested_frames={} hardware_decode_prefer_hardware_requested_frames={} hardware_decode_prefer_gpu_requested_frames={} hardware_decode_require_gpu_requested_frames={} hardware_decode_cpu_not_requested_frames={} hardware_decode_cpu_unavailable_frames={} hardware_decode_backend_unavailable_frames={} hardware_decode_codec_unsupported_frames={} hardware_decode_device_context_attempted_frames={} hardware_decode_device_context_created_frames={} hardware_decode_device_context_unavailable_frames={} hardware_decode_cpu_transfer_frames={} hardware_decode_cpu_transfer_configured_frames={} hardware_decode_cpu_transfer_observed_frames={} hardware_decode_cpu_transfer_setup_failed_frames={} hardware_decode_cpu_transfer_decoder_open_failed_frames={} hardware_decode_cpu_transfer_awaiting_frame_frames={} hardware_decode_backend_boundary_frames={} hardware_decode_gpu_resident_native_frames={} hardware_decode_candidate_d3d12va_frames={} hardware_decode_candidate_d3d11va_frames={} hardware_decode_candidate_dxva2_frames={} hardware_decode_candidate_videotoolbox_frames={} hardware_decode_candidate_vaapi_frames={} hardware_decode_candidate_vdpau_frames={} hardware_decode_candidate_cuda_frames={} hardware_decode_adapter_unavailable_frames={} decoded_frame_count={} max_decoded_frame_count={} session_open_us={} cache_lookup_us={} seek_us={} packet_decode_us={} hardware_transfer_us={} swscale_us={} rgba_copy_us={} external_process_us={} cache_hit_frames={} playback_session_ring_hit_frames={} latency_buckets={:?}",
                access_mode.as_str(),
                profile.frames,
                profile.max_duration_us,
                p95_upper_bound_us,
                profile.total_duration_us,
                profile.queue_wait_max_us,
                profile.queue_wait_total_us,
                profile.max_frame_queue_wait_us,
                profile.max_frame_bottleneck,
                profile.seeked_frames,
                profile.keyframe_seek_strategy_frames,
                profile.bounded_any_seek_strategy_frames,
                profile.forward_reuse_frame_window_max,
                profile.forward_decode_budget_frames_max,
                profile.any_seek_window_ms_max,
                profile.session_reused_frames,
                profile.session_opened_frames,
                profile.forward_reused_frames,
                profile.seek_index_available_frames,
                profile.seek_index_used_frames,
                profile.seek_index_keyframes_max,
                profile.seek_index_observed_packets_max,
                profile.seek_index_probe_backed_frames,
                profile.seek_index_session_observed_frames,
                profile.hardware_decode_active_frames,
                profile.zero_copy_active_frames,
                profile.gpu_texture_resident_frames,
                profile.decoded_nv12_surface_frames,
                profile.decoded_p010_surface_frames,
                profile.hardware_decode_texture_residency_blocker_frames,
                profile.hardware_decode_auto_requested_frames,
                profile.hardware_decode_prefer_hardware_requested_frames,
                profile.hardware_decode_prefer_gpu_requested_frames,
                profile.hardware_decode_require_gpu_requested_frames,
                profile.hardware_decode_cpu_not_requested_frames,
                profile.hardware_decode_cpu_unavailable_frames,
                profile.hardware_decode_backend_unavailable_frames,
                profile.hardware_decode_codec_unsupported_frames,
                profile.hardware_decode_device_context_attempted_frames,
                profile.hardware_decode_device_context_created_frames,
                profile.hardware_decode_device_context_unavailable_frames,
                profile.hardware_decode_cpu_transfer_frames,
                profile.hardware_decode_cpu_transfer_configured_frames,
                profile.hardware_decode_cpu_transfer_observed_frames,
                profile.hardware_decode_cpu_transfer_setup_failed_frames,
                profile.hardware_decode_cpu_transfer_decoder_open_failed_frames,
                profile.hardware_decode_cpu_transfer_awaiting_frame_frames,
                profile.hardware_decode_backend_boundary_frames,
                profile.hardware_decode_gpu_resident_native_frames,
                profile.hardware_decode_candidate_d3d12va_frames,
                profile.hardware_decode_candidate_d3d11va_frames,
                profile.hardware_decode_candidate_dxva2_frames,
                profile.hardware_decode_candidate_videotoolbox_frames,
                profile.hardware_decode_candidate_vaapi_frames,
                profile.hardware_decode_candidate_vdpau_frames,
                profile.hardware_decode_candidate_cuda_frames,
                profile.hardware_decode_adapter_unavailable_frames,
                profile.decoded_frame_count,
                profile.max_decoded_frame_count,
                profile.max_frame_stage_durations.session_open_us,
                profile.max_frame_stage_durations.cache_lookup_us,
                profile.max_frame_stage_durations.seek_us,
                profile.max_frame_stage_durations.packet_decode_us,
                profile.max_frame_stage_durations.hardware_transfer_us,
                profile.max_frame_stage_durations.swscale_us,
                profile.max_frame_stage_durations.rgba_copy_us,
                profile.max_frame_stage_durations.external_process_us,
                profile.cache_hit_frames,
                profile.playback_session_ring_hit_frames,
                profile.latency_buckets
            ),
            "inspect_preview_decode_access_mode_profile",
            "Inspect the per-access-mode decode profile before changing global decode concurrency or color/render code.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }

    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        if profile.queue_wait_max_us <= summary.slow_frame_budget_us {
            continue;
        }
        let queue_wait_p95_upper_bound_us =
            profile.queue_wait_buckets.estimated_p95_upper_bound_us();
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_access_mode_queue_wait_bound",
            format!(
                "access_mode={} queue_wait_max_us={} queue_wait_p95_upper_bound_us={} queue_wait_total_us={} queue_wait_last_us={} frames={} slow_frame_budget_us={} queue_wait_buckets={:?}",
                access_mode.as_str(),
                profile.queue_wait_max_us,
                queue_wait_p95_upper_bound_us,
                profile.queue_wait_total_us,
                profile.queue_wait_last_us,
                profile.frames,
                summary.slow_frame_budget_us,
                profile.queue_wait_buckets
            ),
            "inspect_preview_access_mode_queue",
            "Inspect per-access-mode worker lane pressure so playback, scrub, and still-frame requests cannot hide each other's queue latency.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    let playback_profile = summary.access_mode_profiles.playback_cursor;
    let hardware_cpu_transfer_setup_failures = playback_profile
        .hardware_decode_cpu_transfer_setup_failed_frames
        .saturating_add(playback_profile.hardware_decode_cpu_transfer_decoder_open_failed_frames);
    if hardware_cpu_transfer_setup_failures > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_hardware_cpu_transfer_setup_failed",
            format!(
                "access_mode=PlaybackCursor setup_failed_frames={} decoder_open_failed_frames={} device_context_attempted_frames={} device_context_created_frames={} codec_unsupported_frames={} backend_unavailable_frames={} cpu_transfer_configured_frames={} cpu_transfer_observed_frames={}",
                playback_profile.hardware_decode_cpu_transfer_setup_failed_frames,
                playback_profile.hardware_decode_cpu_transfer_decoder_open_failed_frames,
                playback_profile.hardware_decode_device_context_attempted_frames,
                playback_profile.hardware_decode_device_context_created_frames,
                playback_profile.hardware_decode_codec_unsupported_frames,
                playback_profile.hardware_decode_backend_unavailable_frames,
                playback_profile.hardware_decode_cpu_transfer_configured_frames,
                playback_profile.hardware_decode_cpu_transfer_observed_frames
            ),
            "diagnose_ffmpeg_hardware_decode_setup",
            "Inspect FFmpeg hardware device setup and decoder-open diagnostics before treating CPU soft decode as the only fallback path.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    let hardware_fallback_not_engaged =
        playback_profile.hardware_decode_fallback_not_engaged_frames();
    if hardware_fallback_not_engaged > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_playback_hardware_fallback_not_engaged",
            format!(
                "access_mode=PlaybackCursor requested_frames={} effective_frames={} not_engaged_frames={} prefer_hardware_requested_frames={} prefer_gpu_requested_frames={} require_gpu_requested_frames={} cpu_transfer_frames={} cpu_transfer_observed_frames={} gpu_resident_native_frames={} active_frames={} backend_unavailable_frames={} codec_unsupported_frames={} device_context_attempted_frames={} device_context_created_frames={} device_context_unavailable_frames={} setup_failed_frames={} decoder_open_failed_frames={} awaiting_frame_frames={} backend_boundary_frames={} adapter_unavailable_frames={} candidate_d3d12va_frames={} candidate_d3d11va_frames={} candidate_dxva2_frames={} candidate_videotoolbox_frames={} candidate_vaapi_frames={} candidate_vdpau_frames={} candidate_cuda_frames={}",
                playback_profile.hardware_decode_requested_frames(),
                playback_profile.hardware_decode_effective_frames(),
                hardware_fallback_not_engaged,
                playback_profile.hardware_decode_prefer_hardware_requested_frames,
                playback_profile.hardware_decode_prefer_gpu_requested_frames,
                playback_profile.hardware_decode_require_gpu_requested_frames,
                playback_profile.hardware_decode_cpu_transfer_frames,
                playback_profile.hardware_decode_cpu_transfer_observed_frames,
                playback_profile.hardware_decode_gpu_resident_native_frames,
                playback_profile.hardware_decode_active_frames,
                playback_profile.hardware_decode_backend_unavailable_frames,
                playback_profile.hardware_decode_codec_unsupported_frames,
                playback_profile.hardware_decode_device_context_attempted_frames,
                playback_profile.hardware_decode_device_context_created_frames,
                playback_profile.hardware_decode_device_context_unavailable_frames,
                playback_profile.hardware_decode_cpu_transfer_setup_failed_frames,
                playback_profile.hardware_decode_cpu_transfer_decoder_open_failed_frames,
                playback_profile.hardware_decode_cpu_transfer_awaiting_frame_frames,
                playback_profile.hardware_decode_backend_boundary_frames,
                playback_profile.hardware_decode_adapter_unavailable_frames,
                playback_profile.hardware_decode_candidate_d3d12va_frames,
                playback_profile.hardware_decode_candidate_d3d11va_frames,
                playback_profile.hardware_decode_candidate_dxva2_frames,
                playback_profile.hardware_decode_candidate_videotoolbox_frames,
                playback_profile.hardware_decode_candidate_vaapi_frames,
                playback_profile.hardware_decode_candidate_vdpau_frames,
                playback_profile.hardware_decode_candidate_cuda_frames
            ),
            "recover_playback_hardware_decode_fallback",
            "Treat requested-but-unengaged playback hardware decode as a recovery event: fix decoder backend setup or switch to proxy/optimized media instead of continuing slow soft decode.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if playback_profile.frames > 1 && playback_profile.session_reused_frames == 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_playback_session_not_reused",
            format!(
                "access_mode=PlaybackCursor frames={} session_opened_frames={} session_reused_frames=0 max_duration_us={} session_open_us={}",
                playback_profile.frames,
                playback_profile.session_opened_frames,
                playback_profile.max_duration_us,
                playback_profile.max_frame_stage_durations.session_open_us
            ),
            "preserve_playback_decode_session",
            "Keep the playback cursor decode session stable across adjacent playback frames before increasing worker count.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    let playback_source_decode_frames = playback_profile
        .in_process_cpu_frames
        .saturating_add(playback_profile.external_ffmpeg_cpu_rgba_frames);
    if playback_source_decode_frames > 0
        && playback_profile.forward_reused_frames == 0
        && playback_profile.playback_session_ring_hit_frames == 0
        && playback_profile.cache_hit_frames == 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_playback_without_locality",
            format!(
                "access_mode=PlaybackCursor source_decode_frames={} forward_reused_frames=0 playback_session_ring_hit_frames=0 cache_hit_frames=0 seeked_frames={} decoded_frame_count={} max_decoded_frame_count={}",
                playback_source_decode_frames,
                playback_profile.seeked_frames,
                playback_profile.decoded_frame_count,
                playback_profile.max_decoded_frame_count
            ),
            "improve_playback_decoder_residency",
            "Inspect playback cursor sequencing, proxy readiness, and decoder residency because playback is behaving like repeated random access.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.worker_queue.queued_expired_playback_current_jobs > 0
        || summary.worker_queue.dropped_expired_playback_current_jobs > 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_expired_playback_current_queue",
            format!(
                "queued_expired_playback_current_jobs={} dropped_expired_playback_current_jobs={} queued_jobs={} queued_current_jobs={} queued_playback_cursor_jobs={} queued_prefetch_jobs={} in_flight_jobs={} in_flight_playback_cursor_jobs={} canceled_playback_deadline_jobs={} current_deadline_assignments={} current_decode_decisions={} current_drop_late_decisions={} current_proxy_or_hardware_recommended_decisions={}",
                summary.worker_queue.queued_expired_playback_current_jobs,
                summary.worker_queue.dropped_expired_playback_current_jobs,
                summary.worker_queue.queued_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_playback_cursor_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_queue.in_flight_jobs,
                summary.worker_queue.in_flight_playback_cursor_jobs,
                summary.canceled_playback_deadline_jobs,
                summary.playback_schedule.current_deadline_assignments,
                summary.playback_schedule.current_decode_decisions,
                summary.playback_schedule.current_drop_late_decisions,
                summary
                    .playback_schedule
                    .current_proxy_or_hardware_recommended_decisions
            ),
            "drop_expired_playback_queue_work",
            "Drop or reprioritize expired playback-current work before it waits in the worker queue; playback must make clock-driven decode/drop/proxy decisions instead of decoding stale visible frames.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_current_stalled_expirations > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_current_stall_expirations",
            format!(
                "playback_current_stalled_expirations={} queue_canceled_jobs={} scheduler_canceled_requests={} queued_jobs={} queued_current_jobs={} in_flight_jobs={} in_flight_current_jobs={} canceled_jobs={} canceled_obsolete_jobs={} canceled_playback_deadline_jobs={}",
                summary.playback_current_stalled_expirations,
                summary.queue_canceled_jobs,
                summary.scheduler.canceled_requests,
                summary.worker_queue.queued_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.in_flight_jobs,
                summary.worker_queue.in_flight_current_jobs,
                summary.canceled_jobs,
                summary.canceled_obsolete_jobs,
                summary.canceled_playback_deadline_jobs
            ),
            "diagnose_preview_current_frame_stalls",
            "Inspect realtime current-frame decode residency, worker queue pressure, and cooperative cancellation because playback buffering had to be released without a current preview frame.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_schedule.sustained_pressure_events > 0
        || summary.playback_schedule.sustained_pressure_active
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_sustained_pressure",
            format!(
                "sustained_pressure_active={} sustained_pressure_events={} sustained_pressure_recoveries={} current_late_streak={} current_drop_late_decisions={} current_proxy_or_hardware_recommended_decisions={} prefetch_skipped_sustained_pressure={} prefetch_skipped_current_pending={} prefetch_skipped_current_work={} queued_current_jobs={} queued_prefetch_jobs={} in_flight_current_jobs={} in_flight_prefetch_jobs={}",
                summary.playback_schedule.sustained_pressure_active,
                summary.playback_schedule.sustained_pressure_events,
                summary.playback_schedule.sustained_pressure_recoveries,
                summary.playback_schedule.current_late_streak,
                summary.playback_schedule.current_drop_late_decisions,
                summary
                    .playback_schedule
                    .current_proxy_or_hardware_recommended_decisions,
                summary
                    .playback_schedule
                    .prefetch_skipped_sustained_pressure,
                summary.prefetch_skipped_current_pending,
                summary.prefetch_skipped_current_work,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_queue.in_flight_current_jobs,
                summary.worker_queue.in_flight_prefetch_jobs
            ),
            "recover_playback_scheduler_pressure",
            "Suppress forward prefetch while sustained playback pressure is active, then resume only after a current playback frame succeeds; use proxy or hardware decode when late frames continue.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_schedule.current_native_import_unavailable_decisions > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_native_import_unavailable_playback_frames",
            format!(
                "current_native_import_unavailable_decisions={} current_proxy_or_hardware_recommended_decisions={} playback_gpu_resident_native_frames={} playback_hardware_cpu_transfer_frames={} playback_zero_copy_active_frames={} playback_gpu_texture_resident_frames={}",
                summary
                    .playback_schedule
                    .current_native_import_unavailable_decisions,
                summary
                    .playback_schedule
                    .current_proxy_or_hardware_recommended_decisions,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_gpu_resident_native_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_cpu_transfer_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .zero_copy_active_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .gpu_texture_resident_frames
            ),
            "enable_renderer_native_video_import",
            "Connect the renderer native video import path for playback before treating hardware decode as GPU-resident; CPU transfer fallback is not the production playback path.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_schedule.current_hardware_fallback_not_engaged_decisions > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_hardware_fallback_recovery_decisions",
            format!(
                "current_hardware_fallback_not_engaged_decisions={} current_proxy_or_hardware_recommended_decisions={} current_drop_late_decisions={} current_proxy_generation_requests={} current_proxy_generation_request_dedupes={} playback_requested_frames={} playback_effective_hardware_frames={} playback_cpu_transfer_observed_frames={} playback_gpu_resident_native_frames={} playback_backend_unavailable_frames={} playback_codec_unsupported_frames={} playback_device_context_unavailable_frames={} playback_setup_failed_frames={} playback_decoder_open_failed_frames={}",
                summary
                    .playback_schedule
                    .current_hardware_fallback_not_engaged_decisions,
                summary
                    .playback_schedule
                    .current_proxy_or_hardware_recommended_decisions,
                summary.playback_schedule.current_drop_late_decisions,
                summary.playback_schedule.current_proxy_generation_requests,
                summary
                    .playback_schedule
                    .current_proxy_generation_request_dedupes,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_requested_frames(),
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_effective_frames(),
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_cpu_transfer_observed_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_gpu_resident_native_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_backend_unavailable_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_codec_unsupported_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_device_context_unavailable_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_cpu_transfer_setup_failed_frames,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_cpu_transfer_decoder_open_failed_frames
            ),
            "recover_playback_hardware_decode_fallback",
            "Route current playback hardware-decode misses into recovery policy: repair backend setup when possible, otherwise use existing proxy/optimized media policy instead of continuing slow soft decode.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.hardware_decode_admission.playback_native_import_gated() {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_hardware_decode_admission_gated",
            format!(
                "playback_hardware_decode_request={:?} admission_blocker={:?} renderer_native_import_support_known={} renderer_native_import_ready={} renderer_supported_handle_kinds={} renderer_supported_source_texture_formats={} platform_discovery_available={} platform_zero_copy_supported={} platform_low_copy_fallback_supported={} platform_native_import_ready={} native_import_admission_ready={} playback_frames={} current_decode_decisions={} current_drop_late_decisions={}",
                summary.hardware_decode_admission.playback_request,
                summary.hardware_decode_admission.admission_blocker,
                summary
                    .hardware_decode_admission
                    .renderer_native_import_support_known,
                summary.hardware_decode_admission.renderer_native_import_ready,
                summary
                    .hardware_decode_admission
                    .renderer_supported_handle_kinds,
                summary
                    .hardware_decode_admission
                    .renderer_supported_source_texture_formats,
                summary
                    .hardware_decode_admission
                    .platform_discovery_available,
                summary
                    .hardware_decode_admission
                    .platform_zero_copy_supported,
                summary
                    .hardware_decode_admission
                    .platform_low_copy_fallback_supported,
                summary.hardware_decode_admission.platform_native_import_ready,
                summary.hardware_decode_admission.native_import_admission_ready,
                summary.playback_cursor_frames,
                summary.playback_schedule.current_decode_decisions,
                summary.playback_schedule.current_drop_late_decisions
            ),
            "connect_native_import_before_enabling_hardware_decode_admission",
            "Keep playback hardware decode admission disabled until renderer and platform native video import are ready; then allow GPU-resident playback jobs instead of hardware CPU-transfer fallback.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    match summary.primary_bottleneck {
        AppUiPreviewDecodeBottleneck::QueueWait => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_wait_bound",
            format!(
            "queue_wait_max_us={} current_queue_wait_max_us={} prefetch_queue_wait_max_us={} enqueued_jobs={} prefetch_skipped_current_pending={} prefetch_skipped_current_work={} prefetch_skipped_prefetch_backlog={} queued_jobs={} queued_current_jobs={} queued_prefetch_jobs={} queued_playback_cursor_jobs={} queued_expired_playback_current_jobs={} queued_scrub_cursor_jobs={} queued_random_access_still_jobs={} queued_any_lane_eligible_jobs={} queued_playback_lane_eligible_jobs={} queued_scrub_lane_eligible_jobs={} queued_still_lane_eligible_jobs={} queued_non_playback_lane_eligible_jobs={} in_flight_jobs={} in_flight_current_jobs={} in_flight_prefetch_jobs={} in_flight_playback_cursor_jobs={} in_flight_scrub_cursor_jobs={} in_flight_random_access_still_jobs={} in_flight_playback_lane_jobs={} in_flight_scrub_lane_jobs={} in_flight_still_lane_jobs={} in_flight_non_playback_lane_jobs={} in_flight_cross_lane_current_jobs={} queue_full_drops={} queue_evicted_prefetch_jobs={} queue_evicted_still_jobs={} interactive_cancel_requests={} interactive_cancel_scheduler_requests={} interactive_cancel_queued_jobs={} queue_canceled_jobs={} queue_pruned_obsolete_jobs={} queue_promoted_current_jobs={}",
                summary.queue_wait_max_us,
                summary.current_queue_wait_max_us,
                summary.prefetch_queue_wait_max_us,
                summary.enqueued_jobs,
                summary.prefetch_skipped_current_pending,
                summary.prefetch_skipped_current_work,
                summary.prefetch_skipped_prefetch_backlog,
                summary.worker_queue.queued_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_queue.queued_playback_cursor_jobs,
                summary.worker_queue.queued_expired_playback_current_jobs,
                summary.worker_queue.queued_scrub_cursor_jobs,
                summary.worker_queue.queued_random_access_still_jobs,
                summary.worker_queue.queued_any_lane_eligible_jobs,
                summary.worker_queue.queued_playback_lane_eligible_jobs,
                summary.worker_queue.queued_scrub_lane_eligible_jobs,
                summary.worker_queue.queued_still_lane_eligible_jobs,
                summary.worker_queue.queued_non_playback_lane_eligible_jobs,
                summary.worker_queue.in_flight_jobs,
                summary.worker_queue.in_flight_current_jobs,
                summary.worker_queue.in_flight_prefetch_jobs,
                summary.worker_queue.in_flight_playback_cursor_jobs,
                summary.worker_queue.in_flight_scrub_cursor_jobs,
                summary.worker_queue.in_flight_random_access_still_jobs,
                summary.worker_queue.in_flight_playback_lane_jobs,
                summary.worker_queue.in_flight_scrub_lane_jobs,
                summary.worker_queue.in_flight_still_lane_jobs,
                summary.worker_queue.in_flight_non_playback_lane_jobs,
                summary.worker_queue.in_flight_cross_lane_current_jobs,
                summary.queue_full_drops,
                summary.queue_evicted_prefetch_jobs,
                summary.queue_evicted_still_jobs,
                summary.interactive_cancel_requests,
                summary.interactive_cancel_scheduler_requests,
                summary.interactive_cancel_queued_jobs,
                summary.queue_canceled_jobs,
                summary.queue_pruned_obsolete_jobs,
                summary.queue_promoted_current_jobs
            ),
            "prioritize_current_preview_decode",
            "Reduce worker queue wait by canceling stale prefetch work or adding a cancellable decode session.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::PacketDecode => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_codec_or_gop_bound",
            format!(
                "packet_decode_us={} decoded_frame_count={} max_decoded_frame_count={}",
                summary.max_frame_stage_durations.packet_decode_us,
                summary.decoded_frame_count,
                summary.max_decoded_frame_count
            ),
            "enable_proxy_or_hardware_decode",
            "Prefer fresh proxy playback or implement hardware decode residency for this source.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::HardwareTransfer => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_hardware_transfer_bound",
            format!(
                "hardware_transfer_us={} hardware_decode_cpu_transfer_observed_frames={}",
                summary.max_frame_stage_durations.hardware_transfer_us,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .hardware_decode_cpu_transfer_observed_frames
            ),
            "connect_native_decoder_surface_import",
            "Avoid hardware-frame transfer back to CPU by importing native decoder surfaces into the renderer.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::Seek => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_seek_bound",
            format!(
                "seek_us={} seeked_frames={}",
                summary.max_frame_stage_durations.seek_us, summary.seeked_frames
            ),
            "generate_proxy_or_improve_random_access",
            "Generate playback proxies or improve random-access/indexing strategy for this media.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::CpuRgbaBoundary => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CpuRgbaBoundary,
            "preview_decode_cpu_rgba_boundary_bound",
            format!(
                "swscale_us={} rgba_copy_us={}",
                summary.max_frame_stage_durations.swscale_us,
                summary.max_frame_stage_durations.rgba_copy_us
            ),
            "remove_cpu_rgba_decode_boundary",
            "Move toward high-bit-depth or GPU-resident decode frames instead of CPU RGBA8 preview payloads.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::ExternalProcess => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::ExternalProcess,
            "preview_decode_external_process_bound",
            format!(
                "external_process_us={}",
                summary.max_frame_stage_durations.external_process_us
            ),
            "avoid_external_ffmpeg_preview_path",
            "Use in-process decode or a real hardware-resident adapter instead of rawvideo over stdout.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::SessionOpen => push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_session_open_bound",
            format!(
                "session_open_us={}",
                summary.max_frame_stage_durations.session_open_us
            ),
            "preserve_decode_session_locality",
            "Keep decode sessions alive across adjacent playback requests and avoid path/size churn.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        ),
        AppUiPreviewDecodeBottleneck::CacheLookup | AppUiPreviewDecodeBottleneck::None => {}
    }

    if summary.cache_hit_frames == 0
        && summary
            .in_process_cpu_frames
            .saturating_add(summary.external_ffmpeg_cpu_rgba_frames)
            > 0
        && summary.playback_session_ring_hit_frames == 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::ProxyCache,
            "preview_decode_source_path_without_cache_hits",
            format!(
                "source_decode_frames={} cache_hit_frames=0",
                summary
                    .in_process_cpu_frames
                    .saturating_add(summary.external_ffmpeg_cpu_rgba_frames)
            ),
            "warm_preview_cache_or_proxy",
            "Warm preview cache or generate fresh playback proxies before interactive playback.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.canceled_prefetch_deadline_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_deadline_cancellations",
            format!(
                "canceled_prefetch_deadline_jobs={} playback_prefetch_deadline_jobs={} scrub_prefetch_deadline_jobs={} random_access_still_prefetch_deadline_jobs={}",
                summary.canceled_prefetch_deadline_jobs,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .canceled_prefetch_deadline_jobs,
                summary
                    .access_mode_profiles
                    .scrub_cursor
                    .canceled_prefetch_deadline_jobs,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_prefetch_deadline_jobs
            ),
            "tune_preview_prefetch_deadline_or_proxy",
            "Inspect prefetch cancellation pressure, proxy readiness, and playback decode locality before increasing decode concurrency.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.playback_schedule.current_deadline_missing_frame_rate > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_deadline_invalid_frame_rate",
            format!(
                "current_deadline_missing_frame_rate={} current_deadline_assignments={} last_current_deadline_budget_us={:?}",
                summary
                    .playback_schedule
                    .current_deadline_missing_frame_rate,
                summary.playback_schedule.current_deadline_assignments,
                summary.playback_schedule.last_current_deadline_budget_us
            ),
            "fix_sequence_playback_frame_rate_contract",
            "Ensure playback sequences expose a valid frame rate so current-frame decode receives a display deadline.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.playback_schedule.forward_prefetch_invalid_frame_rate > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_window_invalid_frame_rate",
            format!(
                "forward_prefetch_invalid_frame_rate={} forward_prefetch_window_evaluations={} last_forward_prefetch_window_frames={:?} forward_prefetch_horizon_us={} forward_prefetch_min_frames={} forward_prefetch_max_frames={}",
                summary
                    .playback_schedule
                    .forward_prefetch_invalid_frame_rate,
                summary
                    .playback_schedule
                    .forward_prefetch_window_evaluations,
                summary
                    .playback_schedule
                    .last_forward_prefetch_window_frames,
                summary.playback_schedule.forward_prefetch_horizon_us,
                summary.playback_schedule.forward_prefetch_min_frames,
                summary.playback_schedule.forward_prefetch_max_frames
            ),
            "fix_sequence_prefetch_frame_rate_contract",
            "Ensure playback prefetch derives its window from a valid sequence frame rate instead of silently disabling cache warming.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_playback_deadline_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::AccessMode,
            "preview_decode_playback_deadline_cancellations",
            format!(
                "canceled_playback_deadline_jobs={} playback_cursor_deadline_jobs={} playback_frames={} playback_queue_wait_max_us={} playback_max_duration_us={} current_decode_decisions={} current_drop_late_decisions={} current_proxy_or_hardware_recommended_decisions={}",
                summary.canceled_playback_deadline_jobs,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .canceled_playback_deadline_jobs,
                summary.playback_cursor_frames,
                summary.access_mode_profiles.playback_cursor.queue_wait_max_us,
                summary.access_mode_profiles.playback_cursor.max_duration_us,
                summary.playback_schedule.current_decode_decisions,
                summary.playback_schedule.current_drop_late_decisions,
                summary
                    .playback_schedule
                    .current_proxy_or_hardware_recommended_decisions
            ),
            "drop_late_playback_frames_or_use_proxy_hardware_decode",
            "Playback current frames that miss their display deadline should be dropped or served by proxy/hardware decode rather than decoded after they are obsolete.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_prefetch_preempted_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_preempted_by_current",
            format!(
                "canceled_prefetch_preempted_jobs={} playback_prefetch_preempted_jobs={} scrub_prefetch_preempted_jobs={} random_access_still_prefetch_preempted_jobs={} queued_current_jobs={} queued_prefetch_jobs={} in_flight_jobs={} in_flight_prefetch_jobs={} in_flight_playback_cursor_jobs={}",
                summary.canceled_prefetch_preempted_jobs,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .canceled_prefetch_preempted_jobs,
                summary
                    .access_mode_profiles
                    .scrub_cursor
                    .canceled_prefetch_preempted_jobs,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_prefetch_preempted_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_queue.in_flight_jobs,
                summary.worker_queue.in_flight_prefetch_jobs,
                summary.worker_queue.in_flight_playback_cursor_jobs
            ),
            "reduce_speculative_prefetch_pressure",
            "Throttle playback prefetch when visible current-frame work is waiting; prefetch should yield before it consumes the only interactive decode opportunity.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_still_preempted_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_still_preempted_by_realtime_current",
            format!(
                "canceled_still_preempted_jobs={} random_access_still_preempted_jobs={} queued_current_jobs={} queued_scrub_cursor_jobs={} queued_playback_cursor_jobs={} queued_random_access_still_jobs={} in_flight_jobs={} in_flight_current_jobs={} in_flight_scrub_cursor_jobs={} in_flight_playback_cursor_jobs={} in_flight_random_access_still_jobs={}",
                summary.canceled_still_preempted_jobs,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_still_preempted_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_scrub_cursor_jobs,
                summary.worker_queue.queued_playback_cursor_jobs,
                summary.worker_queue.queued_random_access_still_jobs,
                summary.worker_queue.in_flight_jobs,
                summary.worker_queue.in_flight_current_jobs,
                summary.worker_queue.in_flight_scrub_cursor_jobs,
                summary.worker_queue.in_flight_playback_cursor_jobs,
                summary.worker_queue.in_flight_random_access_still_jobs
            ),
            "preempt_still_decode_for_realtime_preview",
            "Keep random-access still decode from occupying the only interactive lane when playback or scrub current-frame work is waiting.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.decode_timeout_failures > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_timeout_failures",
            format!(
                "decode_timeout_failures={} playback_timeout_failures={} scrub_timeout_failures={} random_access_still_timeout_failures={}",
                summary.decode_timeout_failures,
                summary.access_mode_profiles.playback_cursor.timeout_failures,
                summary.access_mode_profiles.scrub_cursor.timeout_failures,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .timeout_failures
            ),
            "inspect_access_mode_decode_timeout_budget",
            "Inspect access-mode decode strategy, hardware decode residency, proxy readiness, and timeout budget before widening worker concurrency.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.decode_budget_exhausted_failures > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_forward_budget_exhausted",
            format!(
                "decode_budget_exhausted_failures={} playback_budget_exhausted_failures={} scrub_budget_exhausted_failures={} random_access_still_budget_exhausted_failures={} playback_forward_decode_budget_frames_max={} scrub_forward_decode_budget_frames_max={} random_access_still_forward_decode_budget_frames_max={}",
                summary.decode_budget_exhausted_failures,
                summary.access_mode_profiles.playback_cursor.budget_exhausted_failures,
                summary.access_mode_profiles.scrub_cursor.budget_exhausted_failures,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .budget_exhausted_failures,
                summary
                    .access_mode_profiles
                    .playback_cursor
                    .forward_decode_budget_frames_max,
                summary
                    .access_mode_profiles
                    .scrub_cursor
                    .forward_decode_budget_frames_max,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .forward_decode_budget_frames_max
            ),
            "inspect_access_mode_forward_decode_budget",
            "Inspect GOP length, proxy readiness, hardware decode residency, and access-mode forward decode budgets before widening CPU fallback work.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    let cancellation_gate = mondrian_playback::evaluate_frame_cancellation(
        summary.cancellation,
        mondrian_playback::FrameCancellationPolicy::default(),
    );
    if !cancellation_gate.passed {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancellation_gate_failed",
            format!(
                "failures={:?} playback={:?} interactive={:?} still={:?}",
                cancellation_gate.failures,
                summary.cancellation.playback,
                summary.cancellation.interactive,
                summary.cancellation.still
            ),
            "inspect_preview_decode_cancellation_points",
            "Inspect cancellation authority attribution and FFmpeg open/seek/decode/copy checkpoints; realtime work must observe and return within the playback-owned policy.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.canceled_obsolete_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_obsolete_cancellations",
            format!(
                "canceled_obsolete_jobs={} canceled_jobs={} playback_obsolete_jobs={} scrub_obsolete_jobs={} random_access_still_obsolete_jobs={}",
                summary.canceled_obsolete_jobs,
                summary.canceled_jobs,
                summary.access_mode_profiles.playback_cursor.canceled_obsolete_jobs,
                summary.access_mode_profiles.scrub_cursor.canceled_obsolete_jobs,
                summary
                    .access_mode_profiles
                    .random_access_still
                    .canceled_obsolete_jobs
            ),
            "coalesce_obsolete_preview_requests",
            "Coalesce preview requests before decode when UI state changes faster than workers can consume jobs.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.queue_full_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_queue_full_drops",
            format!(
                "queue_full_drops={} enqueued_jobs={} prefetch_skipped_current_pending={} prefetch_skipped_current_work={} prefetch_skipped_prefetch_backlog={} queued_jobs={} queued_current_jobs={} queued_prefetch_jobs={} queued_playback_cursor_jobs={} queued_expired_playback_current_jobs={} queued_scrub_cursor_jobs={} queued_random_access_still_jobs={} queued_any_lane_eligible_jobs={} queued_playback_lane_eligible_jobs={} queued_scrub_lane_eligible_jobs={} queued_still_lane_eligible_jobs={} queued_non_playback_lane_eligible_jobs={} in_flight_jobs={} in_flight_current_jobs={} in_flight_prefetch_jobs={} in_flight_playback_cursor_jobs={} in_flight_scrub_cursor_jobs={} in_flight_random_access_still_jobs={} queue_evicted_prefetch_jobs={} queue_evicted_still_jobs={} interactive_cancel_requests={} interactive_cancel_scheduler_requests={} interactive_cancel_queued_jobs={} queue_canceled_jobs={} queue_pruned_obsolete_jobs={} queue_promoted_current_jobs={} scheduler_dropped_pending_window_requests={} scheduler_evicted_still_requests={}",
                summary.queue_full_drops,
                summary.enqueued_jobs,
                summary.prefetch_skipped_current_pending,
                summary.prefetch_skipped_current_work,
                summary.prefetch_skipped_prefetch_backlog,
                summary.worker_queue.queued_jobs,
                summary.worker_queue.queued_current_jobs,
                summary.worker_queue.queued_prefetch_jobs,
                summary.worker_queue.queued_playback_cursor_jobs,
                summary.worker_queue.queued_expired_playback_current_jobs,
                summary.worker_queue.queued_scrub_cursor_jobs,
                summary.worker_queue.queued_random_access_still_jobs,
                summary.worker_queue.queued_any_lane_eligible_jobs,
                summary.worker_queue.queued_playback_lane_eligible_jobs,
                summary.worker_queue.queued_scrub_lane_eligible_jobs,
                summary.worker_queue.queued_still_lane_eligible_jobs,
                summary.worker_queue.queued_non_playback_lane_eligible_jobs,
                summary.worker_queue.in_flight_jobs,
                summary.worker_queue.in_flight_current_jobs,
                summary.worker_queue.in_flight_prefetch_jobs,
                summary.worker_queue.in_flight_playback_cursor_jobs,
                summary.worker_queue.in_flight_scrub_cursor_jobs,
                summary.worker_queue.in_flight_random_access_still_jobs,
                summary.queue_evicted_prefetch_jobs,
                summary.queue_evicted_still_jobs,
                summary.interactive_cancel_requests,
                summary.interactive_cancel_scheduler_requests,
                summary.interactive_cancel_queued_jobs,
                summary.queue_canceled_jobs,
                summary.queue_pruned_obsolete_jobs,
                summary.queue_promoted_current_jobs,
                summary.scheduler.dropped_pending_window_requests,
                summary.scheduler.evicted_still_requests
            ),
            "reduce_preview_worker_transport_backpressure",
            "Fix preview worker transport backpressure so scheduler-accepted current-frame work cannot be dropped after admission.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.queue_invalid_access_mode_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_invalid_access_mode_drop",
            format!(
                "queue_invalid_access_mode_drops={} enqueued_jobs={} scheduler_dropped_invalid_access_mode_requests={}",
                summary.queue_invalid_access_mode_drops,
                summary.enqueued_jobs,
                summary.scheduler.dropped_invalid_access_mode_requests
            ),
            "fix_preview_access_mode_admission",
            "Ensure invalid priority/access-mode pairs are rejected before worker-queue transport.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.worker_disconnected_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_disconnected_drops",
            format!(
                "worker_disconnected_drops={} enqueued_jobs={} queue_full_drops={}",
                summary.worker_disconnected_drops, summary.enqueued_jobs, summary.queue_full_drops
            ),
            "restore_preview_worker_lifecycle",
            "Ensure preview workers are running before accepting media preview jobs and close the queue only during service shutdown.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }

    let scheduler = summary.scheduler;
    if scheduler.dropped_invalid_access_mode_requests > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_invalid_access_mode_request",
            format!(
                "dropped_invalid_access_mode_requests={} scheduled_requests={} pending_requests={}",
                scheduler.dropped_invalid_access_mode_requests,
                scheduler.scheduled_requests,
                scheduler.pending_requests
            ),
            "fix_preview_access_mode_admission",
            "Route speculative media work through PlaybackCursor prefetch only; scrub and still-frame requests must be current-frame work.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scheduler.clock_regressions > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_broker_clock_regression",
            format!("clock_regressions={}", scheduler.clock_regressions),
            "inspect_preview_monotonic_clock_adapter",
            "Inspect the Monotonic Runtime Clock Adapter; the Frame Work Broker clamps regressions but cannot accept their timing evidence.",
            AppUiPreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scheduler
        .skipped_decode_access_mode_mismatch
        .saturating_add(scheduler.completed_cache_only_access_mode_mismatch)
        .saturating_add(scheduler.completed_stale_access_mode_mismatch)
        > 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_access_mode_mismatch",
            format!(
                "skipped_decode_access_mode_mismatch={} completed_cache_only_access_mode_mismatch={} completed_stale_access_mode_mismatch={}",
                scheduler.skipped_decode_access_mode_mismatch,
                scheduler.completed_cache_only_access_mode_mismatch,
                scheduler.completed_stale_access_mode_mismatch
            ),
            "inspect_preview_access_mode_transitions",
            "Inspect playback/scrub/still request transitions and ensure older jobs cannot complete newer access-mode work.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if scheduler
        .skipped_decode_obsolete_generation
        .saturating_add(scheduler.completed_stale_obsolete_generation)
        .saturating_add(scheduler.pruned_obsolete_requests)
        > 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_obsolete_generation_churn",
            format!(
                "skipped_decode_obsolete_generation={} completed_stale_obsolete_generation={} pruned_obsolete_requests={}",
                scheduler.skipped_decode_obsolete_generation,
                scheduler.completed_stale_obsolete_generation,
                scheduler.pruned_obsolete_requests
            ),
            "reduce_preview_generation_churn",
            "Reduce duplicate preview requests per UI tick or coalesce obsolete generations before they reach workers.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
    if scheduler.dropped_pending_window_requests > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewDecodePerformanceArea::Scheduling,
            "preview_decode_pending_window_backpressure",
            format!(
                "dropped_pending_window_requests={} pending_requests={}",
                scheduler.dropped_pending_window_requests, scheduler.pending_requests
            ),
            "bound_preview_pending_window_by_access_mode",
            "Inspect current/prefetch admission policy and keep visible current-frame work latest-wins.",
            AppUiPreviewDecodePerformanceSeverity::Warn,
        );
    }
}

fn push_preview_render_root_causes_and_actions(
    summary: AppUiPreviewRenderPerformanceSummary,
    root_causes: &mut Vec<AppUiPreviewRenderPerformanceRootCause>,
    actions: &mut Vec<AppUiPreviewRenderPerformanceAction>,
) {
    if summary.max_duration_us <= summary.slow_frame_budget_us {
        return;
    }

    push_render_root_cause_with_action(
        root_causes,
        actions,
        AppUiPreviewRenderPerformanceArea::LatencyBudget,
        "preview_render_frame_over_budget",
        format!(
            "max_duration_us={} slow_frame_budget_us={} primary_bottleneck={:?}",
            summary.max_duration_us, summary.slow_frame_budget_us, summary.primary_bottleneck
        ),
        "inspect_preview_render_stage_durations",
        "Inspect post-decode viewer render stage timings before changing decode code.",
    );

    match summary.primary_bottleneck {
        AppUiPreviewRenderBottleneck::Resolve => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::Resolve,
            "preview_render_resolve_bound",
            format!("resolve_us={}", summary.max_frame_stage_durations.resolve_us),
            "profile_preview_plan_resolution",
            "Profile sequence resolution, media-key construction, and readiness checks.",
        ),
        AppUiPreviewRenderBottleneck::FinalCacheLookup => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::FinalCacheLookup,
            "preview_render_final_cache_lookup_bound",
            format!(
                "final_cache_lookup_us={}",
                summary.max_frame_stage_durations.final_cache_lookup_us
            ),
            "profile_viewer_frame_cache",
            "Profile final viewer frame cache lookup and external texture identity checks.",
        ),
        AppUiPreviewRenderBottleneck::WorkingPreparation => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::WorkingPreparation,
            "preview_render_working_prepare_bound",
            format!(
                "working_prepare_us={}",
                summary.max_frame_stage_durations.working_prepare_us
            ),
            "reduce_working_frame_preparation",
            "Reduce working-frame extraction/copy work before timeline compositing.",
        ),
        AppUiPreviewRenderBottleneck::CpuComposite => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::CpuComposite,
            "preview_render_cpu_composite_bound",
            format!(
                "cpu_composite_us={}",
                summary.max_frame_stage_durations.cpu_composite_us
            ),
            "move_preview_composite_to_gpu",
            "Keep common blend, transform, and effect paths on GPU or improve CPU composite tiling.",
        ),
        AppUiPreviewRenderBottleneck::CpuOutputBoundary => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::CpuOutputBoundary,
            "preview_render_cpu_output_boundary_bound",
            format!(
                "cpu_output_boundary_us={}",
                summary.max_frame_stage_durations.cpu_output_boundary_us
            ),
            "move_preview_output_boundary_to_gpu",
            "Route viewer output color/display transforms through the GPU output boundary.",
        ),
        AppUiPreviewRenderBottleneck::FramePackaging => push_render_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewRenderPerformanceArea::FramePackaging,
            "preview_render_frame_packaging_bound",
            format!(
                "frame_packaging_us={}",
                summary.max_frame_stage_durations.frame_packaging_us
            ),
            "avoid_raster_frame_packaging",
            "Prefer GPU-resident viewer frames or reduce final raster hashing/copying.",
        ),
        AppUiPreviewRenderBottleneck::None => {}
    }
}

fn push_render_root_cause_with_action(
    root_causes: &mut Vec<AppUiPreviewRenderPerformanceRootCause>,
    actions: &mut Vec<AppUiPreviewRenderPerformanceAction>,
    area: AppUiPreviewRenderPerformanceArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(AppUiPreviewRenderPerformanceRootCause {
            area,
            code: root_code,
            severity: AppUiPreviewRenderPerformanceSeverity::Fail,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(AppUiPreviewRenderPerformanceAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

fn push_decode_root_cause_with_action(
    root_causes: &mut Vec<AppUiPreviewDecodePerformanceRootCause>,
    actions: &mut Vec<AppUiPreviewDecodePerformanceAction>,
    area: AppUiPreviewDecodePerformanceArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
    severity: AppUiPreviewDecodePerformanceSeverity,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(AppUiPreviewDecodePerformanceRootCause {
            area,
            code: root_code,
            severity,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(AppUiPreviewDecodePerformanceAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

pub(super) fn classify_preview_decode_bottleneck(
    durations: PreviewDecodeStageDurations,
    queue_wait_us: u64,
) -> AppUiPreviewDecodeBottleneck {
    let candidates = [
        (AppUiPreviewDecodeBottleneck::QueueWait, queue_wait_us),
        (
            AppUiPreviewDecodeBottleneck::SessionOpen,
            durations.session_open_us,
        ),
        (
            AppUiPreviewDecodeBottleneck::CacheLookup,
            durations.cache_lookup_us,
        ),
        (AppUiPreviewDecodeBottleneck::Seek, durations.seek_us),
        (
            AppUiPreviewDecodeBottleneck::PacketDecode,
            durations.packet_decode_us,
        ),
        (
            AppUiPreviewDecodeBottleneck::HardwareTransfer,
            durations.hardware_transfer_us,
        ),
        (
            AppUiPreviewDecodeBottleneck::CpuRgbaBoundary,
            durations.swscale_us.saturating_add(durations.rgba_copy_us),
        ),
        (
            AppUiPreviewDecodeBottleneck::ExternalProcess,
            durations.external_process_us,
        ),
    ];

    candidates
        .into_iter()
        .max_by_key(|(_, duration)| *duration)
        .filter(|(_, duration)| *duration > 0)
        .map(|(bottleneck, _)| bottleneck)
        .unwrap_or(AppUiPreviewDecodeBottleneck::None)
}

fn classify_preview_render_bottleneck(
    durations: AppUiPreviewRenderStageDurations,
) -> AppUiPreviewRenderBottleneck {
    let candidates = [
        (AppUiPreviewRenderBottleneck::Resolve, durations.resolve_us),
        (
            AppUiPreviewRenderBottleneck::FinalCacheLookup,
            durations.final_cache_lookup_us,
        ),
        (
            AppUiPreviewRenderBottleneck::WorkingPreparation,
            durations.working_prepare_us,
        ),
        (
            AppUiPreviewRenderBottleneck::CpuComposite,
            durations.cpu_composite_us,
        ),
        (
            AppUiPreviewRenderBottleneck::CpuOutputBoundary,
            durations.cpu_output_boundary_us,
        ),
        (
            AppUiPreviewRenderBottleneck::FramePackaging,
            durations.frame_packaging_us,
        ),
    ];

    candidates
        .into_iter()
        .max_by_key(|(_, duration)| *duration)
        .filter(|(_, duration)| *duration > 0)
        .map(|(bottleneck, _)| bottleneck)
        .unwrap_or(AppUiPreviewRenderBottleneck::None)
}

impl AppUiPreviewColorRejection {
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
            detected_color_space: resolution.detected_color_space,
            working_color_space: resolution.working_color_space,
            diagnostic_summary,
            diagnostic_issue_summary,
        }
    }
}

impl AppUiPreviewDiagnostics {
    /// Return structured preview decode performance evidence when decode activity exists.
    pub fn decode_performance_summary(
        self,
        slow_frame_budget_us: u64,
    ) -> Option<AppUiPreviewDecodePerformanceSummary> {
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
        if primary_bottleneck == AppUiPreviewDecodeBottleneck::None {
            primary_bottleneck = classify_preview_decode_bottleneck(
                max_frame_stage_durations,
                max_frame_queue_wait_us,
            );
        }
        Some(AppUiPreviewDecodePerformanceSummary {
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
    ) -> Option<AppUiPreviewRenderPerformanceSummary> {
        if self.render_timed_frames == 0 {
            return None;
        }
        let stage_durations = self.render_stage_durations;
        let max_frame_stage_durations = self.render_max_frame_stage_durations;
        Some(AppUiPreviewRenderPerformanceSummary {
            timed_frames: self.render_timed_frames,
            max_duration_us: self.render_max_duration_us,
            last_duration_us: self.render_last_duration_us,
            total_duration_us: self.render_total_duration_us,
            slow_frame_budget_us,
            stage_durations,
            max_frame_stage_durations,
            primary_bottleneck: classify_preview_render_bottleneck(max_frame_stage_durations),
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
    pub fn color_health_summary(self) -> Option<AppUiPreviewColorHealthSummary> {
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

        Some(AppUiPreviewColorHealthSummary {
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
