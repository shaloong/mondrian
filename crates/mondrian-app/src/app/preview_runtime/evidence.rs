//! Mutable Preview execution-evidence accumulation and immutable snapshot projection.

use super::*;

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Clone read-only worker progress authority for independent acceptance sampling.
    #[cfg(test)]
    pub(crate) fn decode_execution_watch(&self) -> PreviewDecodeWorkerExecutionWatch {
        self.decode_execution_watch.clone()
    }

    /// Return a point-in-time snapshot of preview scheduling and cache health.
    pub fn diagnostics(&self) -> PreviewDiagnostics {
        let scheduler = self.scheduler.diagnostics();
        let frame_store = self.frame_store.borrow().diagnostics();
        let decode_cancellation = self.metrics.decode_cancellation.borrow().report();
        let cancellation = decode_cancellation.all;
        let decode_residency = self.decode_residency.diagnostics();
        let decode_worker_execution = self.decode_execution_watch.snapshot();
        let mut decode_access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        decode_access_mode_profiles.apply_cancellation(decode_cancellation);
        PreviewDiagnostics {
            render_requests: self.metrics.render_requests.get(),
            ready_frames: self.metrics.ready_frames.get(),
            loading_frames: self.metrics.loading_frames.get(),
            stale_frames: self.metrics.stale_frames.get(),
            unavailable_frames: self.metrics.unavailable_frames.get(),
            unavailability: self.unavailability_evidence.borrow().snapshot(),
            playback_current_stalled_expirations: self
                .metrics
                .playback_current_stalled_expirations
                .get(),
            gpu_preview_candidate_requests: self.metrics.gpu_preview_candidate_requests.get(),
            gpu_preview_candidate_ready: self.metrics.gpu_preview_candidate_ready.get(),
            gpu_preview_candidate_current: self.metrics.gpu_preview_candidate_current.get(),
            gpu_preview_candidate_loading: self.metrics.gpu_preview_candidate_loading.get(),
            gpu_preview_candidate_unavailable: self.metrics.gpu_preview_candidate_unavailable.get(),
            gpu_preview_candidate_pixels: self.metrics.gpu_preview_candidate_pixels.get(),
            gpu_preview_external_frames_registered: self
                .metrics
                .gpu_preview_external_frames_registered
                .get(),
            gpu_preview_external_frames_rejected: self
                .metrics
                .gpu_preview_external_frames_rejected
                .get(),
            gpu_preview_external_frames_cleared: self
                .metrics
                .gpu_preview_external_frames_cleared
                .get(),
            input_color_resolution_override: self.metrics.input_color_resolution_override.get(),
            input_color_resolution_detected_metadata: self
                .metrics
                .input_color_resolution_detected_metadata
                .get(),
            input_color_resolution_missing_assume_rec709: self
                .metrics
                .input_color_resolution_missing_assume_rec709
                .get(),
            input_color_resolution_missing_rejected: self
                .metrics
                .input_color_resolution_missing_rejected
                .get(),
            input_color_resolution_data_texture: self
                .metrics
                .input_color_resolution_data_texture
                .get(),
            media_proxy_path_hits: self.metrics.media_proxy_path_hits.get(),
            media_proxy_path_misses: self.metrics.media_proxy_path_misses.get(),
            media_proxy_path_stale: self.metrics.media_proxy_path_stale.get(),
            media_proxy_path_bypasses: self.metrics.media_proxy_path_bypasses.get(),
            media_proxy_generation_requests: self.metrics.media_proxy_generation_requests.get(),
            media_proxy_generation_request_dedupes: self
                .metrics
                .media_proxy_generation_request_dedupes
                .get(),
            scrub_adaptive_normal_requests: self.metrics.scrub_adaptive_normal_requests.get(),
            scrub_adaptive_hot_region_requests: self
                .metrics
                .scrub_adaptive_hot_region_requests
                .get(),
            scrub_adaptive_slow_latency_requests: self
                .metrics
                .scrub_adaptive_slow_latency_requests
                .get(),
            scrub_adaptive_recovery_requests: self.metrics.scrub_adaptive_recovery_requests.get(),
            media_cache_hits: self.metrics.media_cache_hits.get(),
            media_cache_misses: self.metrics.media_cache_misses.get(),
            media_failure_hits: self.metrics.media_failure_hits.get(),
            decode_cpu_budget: self.decode_cpu_budget,
            decode_worker_count: self.decode_worker_count,
            decode_worker_execution,
            decode_residency_revision: decode_residency.revision,
            decode_residency_family: decode_residency.active_family.map(|family| match family {
                PreviewDecodeResidencyFamily::Playback => "playback",
                PreviewDecodeResidencyFamily::Interactive => "interactive",
            }),
            decode_residency_transitions: decode_residency.transitions,
            decode_residency_blocked_admissions: decode_residency.blocked_admissions,
            decode_residency_required_acknowledgements: decode_residency.required_acknowledgements,
            decode_residency_acknowledged_retirements: decode_residency.acknowledged_retirements,
            hardware_decode_admission: self.hardware_decode_admission_diagnostics(),
            decode_successes: self.metrics.decode_successes.get(),
            decode_startup_preroll_frames: self.metrics.decode_startup_preroll_frames.get(),
            decode_startup_preroll_total_duration_us: self
                .metrics
                .decode_startup_preroll_total_duration_us
                .get(),
            decode_startup_preroll_max_duration_us: self
                .metrics
                .decode_startup_preroll_max_duration_us
                .get(),
            decode_startup_preroll_queue_wait_total_us: self
                .metrics
                .decode_startup_preroll_queue_wait_total_us
                .get(),
            decode_startup_preroll_queue_wait_max_us: self
                .metrics
                .decode_startup_preroll_queue_wait_max_us
                .get(),
            decode_failures: self.metrics.decode_failures.get(),
            decode_timeout_failures: self.metrics.decode_timeout_failures.get(),
            decode_budget_exhausted_failures: self.metrics.decode_budget_exhausted_failures.get(),
            decode_cancellation,
            decode_cancellation_checkpoints: self.metrics.decode_cancellation_checkpoints.get(),
            decode_canceled_jobs: cancellation.cancellations,
            decode_canceled_shutdown_jobs: cancellation.shutdown,
            decode_canceled_obsolete_jobs: cancellation.superseded,
            decode_canceled_prefetch_deadline_jobs: cancellation.prefetch_deadline,
            decode_canceled_playback_deadline_jobs: cancellation.playback_deadline,
            decode_canceled_prefetch_preempted_jobs: cancellation.prefetch_preempted_by_current,
            decode_canceled_still_preempted_jobs: cancellation.still_preempted_by_realtime_current,
            decode_canceled_unknown_jobs: cancellation.unknown,
            decode_canceled_total_duration_us: cancellation.execution.total_us,
            decode_canceled_max_duration_us: cancellation.execution.max_us,
            decode_canceled_last_duration_us: cancellation.execution.last_us,
            decode_cancel_observation_samples: cancellation.request_to_checkpoint.samples,
            decode_cancel_observation_total_us: cancellation.request_to_checkpoint.total_us,
            decode_cancel_observation_max_us: cancellation.request_to_checkpoint.max_us,
            decode_cancel_observation_last_us: cancellation.request_to_checkpoint.last_us,
            decode_canceled_return_latency_total_us: cancellation.checkpoint_to_return.total_us,
            decode_canceled_return_latency_max_us: cancellation.checkpoint_to_return.max_us,
            decode_canceled_return_latency_last_us: cancellation.checkpoint_to_return.last_us,
            decode_in_process_cpu_frames: self.metrics.decode_in_process_cpu_frames.get(),
            decode_external_ffmpeg_cpu_rgba_frames: self
                .metrics
                .decode_external_ffmpeg_cpu_rgba_frames
                .get(),
            decode_playback_session_ring_hit_frames: self
                .metrics
                .decode_playback_session_ring_hit_frames
                .get(),
            decode_cache_hit_frames: self.metrics.decode_cache_hit_frames.get(),
            decode_playback_cursor_frames: self.metrics.decode_playback_cursor_frames.get(),
            decode_scrub_cursor_frames: self.metrics.decode_scrub_cursor_frames.get(),
            decode_random_access_still_frames: self.metrics.decode_random_access_still_frames.get(),
            decode_canceled_playback_cursor_jobs: decode_cancellation.playback.cancellations,
            decode_canceled_scrub_cursor_jobs: decode_cancellation.interactive.cancellations,
            decode_canceled_random_access_still_jobs: decode_cancellation.still.cancellations,
            decode_total_duration_us: self.metrics.decode_total_duration_us.get(),
            decode_max_duration_us: self.metrics.decode_max_duration_us.get(),
            decode_last_duration_us: self.metrics.decode_last_duration_us.get(),
            decode_queue_wait_total_us: self.metrics.decode_queue_wait_total_us.get(),
            decode_queue_wait_max_us: self.metrics.decode_queue_wait_max_us.get(),
            decode_queue_wait_last_us: self.metrics.decode_queue_wait_last_us.get(),
            decode_current_queue_wait_max_us: self.metrics.decode_current_queue_wait_max_us.get(),
            decode_prefetch_queue_wait_max_us: self.metrics.decode_prefetch_queue_wait_max_us.get(),
            decode_seeked_frames: self.metrics.decode_seeked_frames.get(),
            decode_decoded_frame_count: self.metrics.decode_decoded_frame_count.get(),
            decode_max_decoded_frame_count: self.metrics.decode_max_decoded_frame_count.get(),
            decode_threading_none_frames: self.metrics.decode_threading_none_frames.get(),
            decode_threading_frame_frames: self.metrics.decode_threading_frame_frames.get(),
            decode_threading_slice_frames: self.metrics.decode_threading_slice_frames.get(),
            decode_last_threading_count: self.metrics.decode_last_threading_count.get(),
            decode_max_threading_count: self.metrics.decode_max_threading_count.get(),
            decode_stage_durations: self.metrics.decode_stage_durations.get(),
            decode_max_frame_stage_durations: self.metrics.decode_max_frame_stage_durations.get(),
            decode_max_frame_queue_wait_us: self.metrics.decode_max_frame_queue_wait_us.get(),
            decode_max_frame_bottleneck: self.metrics.decode_max_frame_bottleneck.get(),
            decode_access_mode_profiles,
            render_timed_frames: self.metrics.render_timed_frames.get(),
            render_total_duration_us: self.metrics.render_total_duration_us.get(),
            render_max_duration_us: self.metrics.render_max_duration_us.get(),
            render_last_duration_us: self.metrics.render_last_duration_us.get(),
            render_stage_durations: self.metrics.render_stage_durations.get(),
            render_max_frame_stage_durations: self.metrics.render_max_frame_stage_durations.get(),
            completion_poll_calls: self.metrics.completion_poll_calls.get(),
            completion_poll_results: self.metrics.completion_poll_results.get(),
            completion_poll_total_duration_us: self.metrics.completion_poll_total_duration_us.get(),
            completion_poll_max_duration_us: self.metrics.completion_poll_max_duration_us.get(),
            completion_poll_last_duration_us: self.metrics.completion_poll_last_duration_us.get(),
            completion_poll_max_results_per_poll: self
                .metrics
                .completion_poll_max_results_per_poll
                .get(),
            completion_poll_count_budget_exhaustions: self
                .metrics
                .completion_poll_count_budget_exhaustions
                .get(),
            completion_poll_time_budget_exhaustions: self
                .metrics
                .completion_poll_time_budget_exhaustions
                .get(),
            enqueued_jobs: self.metrics.enqueued_jobs.get(),
            prefetch_skipped_current_pending: self.metrics.prefetch_skipped_current_pending.get(),
            prefetch_skipped_current_work: self.metrics.prefetch_skipped_current_work.get(),
            prefetch_skipped_prefetch_backlog: self.metrics.prefetch_skipped_prefetch_backlog.get(),
            queue_full_drops: self.metrics.queue_full_drops.get(),
            queue_invalid_access_mode_drops: self.metrics.queue_invalid_access_mode_drops.get(),
            queue_evicted_prefetch_jobs: self.metrics.queue_evicted_prefetch_jobs.get(),
            queue_evicted_still_jobs: self.metrics.queue_evicted_still_jobs.get(),
            interactive_cancel_requests: self.metrics.interactive_cancel_requests.get(),
            interactive_cancel_scheduler_requests: self
                .metrics
                .interactive_cancel_scheduler_requests
                .get(),
            interactive_cancel_queued_jobs: self.metrics.interactive_cancel_queued_jobs.get(),
            queue_canceled_jobs: self.metrics.queue_canceled_jobs.get(),
            queue_pruned_obsolete_jobs: self.metrics.queue_pruned_obsolete_jobs.get(),
            queue_promoted_current_jobs: self.metrics.queue_promoted_current_jobs.get(),
            worker_disconnected_drops: self.metrics.worker_disconnected_drops.get(),
            worker_queue: self.jobs.diagnostics(),
            scheduler,
            playback_schedule: self.playback_schedule_diagnostics(),
            viewer_frame_cache_hits: self.metrics.viewer_frame_cache_hits.get(),
            viewer_frame_cache_misses: self.metrics.viewer_frame_cache_misses.get(),
            viewer_frame_cache_entries: frame_store.viewer_entries,
            media_cache_entries: frame_store.media_entries,
            media_failure_entries: frame_store.failure_entries,
            media_cache_reserved_bytes: frame_store.media_reserved_bytes,
            media_cache_byte_budget: frame_store.media_byte_budget,
            media_cache_resource_units: frame_store.media_resource_units,
            media_cache_resource_unit_budget: frame_store.media_resource_unit_budget,
            media_cache_evictions: frame_store.media_evictions,
            media_cache_oversize_rejections: frame_store.media_oversize_rejections,
            viewer_frame_cache_reserved_bytes: frame_store.viewer_reserved_bytes,
            viewer_frame_cache_byte_budget: frame_store.viewer_byte_budget,
            viewer_frame_cache_evictions: frame_store.viewer_evictions,
            viewer_frame_cache_oversize_rejections: frame_store.viewer_oversize_rejections,
            pinned_viewer_frame_bytes: frame_store.pinned_viewer_bytes,
            pinned_media_frame_bytes: frame_store.pinned_media_bytes,
            media_failure_evictions: frame_store.failure_evictions,
            color_input_transform_calls: self.metrics.color_input_transform_calls.get(),
            color_input_transform_pixels: self.metrics.color_input_transform_pixels.get(),
            color_output_transform_calls: self.metrics.color_output_transform_calls.get(),
            color_output_transform_pixels: self.metrics.color_output_transform_pixels.get(),
            color_intermediate_transform_calls: self
                .metrics
                .color_intermediate_transform_calls
                .get(),
            color_intermediate_transform_pixels: self
                .metrics
                .color_intermediate_transform_pixels
                .get(),
            color_rgba8_boundary_calls: self.metrics.color_rgba8_boundary_calls.get(),
            color_stage_plans: self.metrics.color_stage_plans.get(),
            color_stage_total_stages: self.metrics.color_stage_total_stages.get(),
            color_stage_cpu_input_stages: self.metrics.color_stage_cpu_input_stages.get(),
            color_stage_cpu_output_stages: self.metrics.color_stage_cpu_output_stages.get(),
            color_stage_gpu_color_stages: self.metrics.color_stage_gpu_color_stages.get(),
            color_stage_upload_stages: self.metrics.color_stage_upload_stages.get(),
            color_stage_readback_stages: self.metrics.color_stage_readback_stages.get(),
            color_stage_gpu_blockers: self.metrics.color_stage_gpu_blockers.get(),
            color_stage_gpu_shader_module_blockers: self
                .metrics
                .color_stage_gpu_shader_module_blockers
                .get(),
            color_stage_gpu_ocio_resource_blockers: self
                .metrics
                .color_stage_gpu_ocio_resource_blockers
                .get(),
            color_stage_gpu_wrapper_blockers: self.metrics.color_stage_gpu_wrapper_blockers.get(),
            color_stage_gpu_render_pipeline_blockers: self
                .metrics
                .color_stage_gpu_render_pipeline_blockers
                .get(),
            color_stage_gpu_ocio_config_blockers: self
                .metrics
                .color_stage_gpu_ocio_config_blockers
                .get(),
            color_stage_gpu_ocio_processor_blockers: self
                .metrics
                .color_stage_gpu_ocio_processor_blockers
                .get(),
            color_stage_gpu_ocio_shader_extraction_blockers: self
                .metrics
                .color_stage_gpu_ocio_shader_extraction_blockers
                .get(),
            color_stage_pixels: self.metrics.color_stage_pixels.get(),
            color_composite_plans: self.metrics.color_composite_plans.get(),
            color_composite_elements: self.metrics.color_composite_elements.get(),
            color_composite_float_linear: self.metrics.color_composite_float_linear.get(),
            color_composite_legacy_rgba8: self.metrics.color_composite_legacy_rgba8.get(),
            color_composite_legacy_media_blend_mode: self
                .metrics
                .color_composite_legacy_media_blend_mode
                .get(),
            color_composite_legacy_media_transform: self
                .metrics
                .color_composite_legacy_media_transform
                .get(),
            color_composite_legacy_media_effect: self
                .metrics
                .color_composite_legacy_media_effect
                .get(),
            color_composite_legacy_solid_blend_mode: self
                .metrics
                .color_composite_legacy_solid_blend_mode
                .get(),
            color_composite_legacy_solid_transform: self
                .metrics
                .color_composite_legacy_solid_transform
                .get(),
            color_composite_legacy_solid_effect: self
                .metrics
                .color_composite_legacy_solid_effect
                .get(),
            color_composite_legacy_adjustment_blend_mode: self
                .metrics
                .color_composite_legacy_adjustment_blend_mode
                .get(),
            color_composite_legacy_adjustment_effect: self
                .metrics
                .color_composite_legacy_adjustment_effect
                .get(),
            color_composite_blocked_domains: self.metrics.color_composite_blocked_domains.get(),
            color_composite_blocked_media_effect_domain: self
                .metrics
                .color_composite_blocked_media_effect_domain
                .get(),
            color_composite_blocked_solid_effect_domain: self
                .metrics
                .color_composite_blocked_solid_effect_domain
                .get(),
            color_composite_blocked_adjustment_effect_domain: self
                .metrics
                .color_composite_blocked_adjustment_effect_domain
                .get(),
            cpu_output_fallback_frames: self.metrics.cpu_output_fallback_frames.get(),
            cpu_output_fallback_pixels: self.metrics.cpu_output_fallback_pixels.get(),
            preview_gpu_output_blocker_frames: self.metrics.preview_gpu_output_blocker_frames.get(),
            preview_gpu_output_blocker_breakdown: *self
                .metrics
                .preview_gpu_output_blocker_breakdown
                .borrow(),
            gpu_compositing: *self.metrics.gpu_compositing.borrow(),
        }
    }

    /// Return the latest color-management rejection captured for the current viewer request.
    pub fn last_color_rejection(&self) -> Option<PreviewColorRejection> {
        self.last_color_rejection.borrow().clone()
    }

    /// Return the latest typed reason a Preview candidate or presentation was unavailable.
    pub fn last_unavailability(&self) -> Option<PreviewUnavailability> {
        self.unavailability_evidence.borrow().last()
    }

    pub(super) fn record_input_color_resolution(&self, source: InputColorResolutionSource) {
        match source {
            InputColorResolutionSource::Override => {
                bump(&self.metrics.input_color_resolution_override);
            }
            InputColorResolutionSource::DataTexture => {
                bump(&self.metrics.input_color_resolution_data_texture);
            }
            InputColorResolutionSource::DetectedMetadata => {
                bump(&self.metrics.input_color_resolution_detected_metadata);
            }
            InputColorResolutionSource::MissingPolicyAssumeRec709 => {
                bump(&self.metrics.input_color_resolution_missing_assume_rec709);
            }
            InputColorResolutionSource::MissingPolicyRejectMedia => {
                bump(&self.metrics.input_color_resolution_missing_rejected);
            }
        }
    }

    pub(super) fn record_color_transform(&self, diagnostics: RenderColorTransformDiagnostics) {
        match diagnostics.direction {
            RenderColorTransformDirection::InputToWorking => {
                bump(&self.metrics.color_input_transform_calls);
                add_cell(
                    &self.metrics.color_input_transform_pixels,
                    diagnostics.pixel_count as u64,
                );
            }
            RenderColorTransformDirection::WorkingToOutput => {
                bump(&self.metrics.color_output_transform_calls);
                add_cell(
                    &self.metrics.color_output_transform_pixels,
                    diagnostics.pixel_count as u64,
                );
            }
            RenderColorTransformDirection::Intermediate => {
                bump(&self.metrics.color_intermediate_transform_calls);
                add_cell(
                    &self.metrics.color_intermediate_transform_pixels,
                    diagnostics.pixel_count as u64,
                );
            }
        }
        if diagnostics.used_rgba8_boundary {
            bump(&self.metrics.color_rgba8_boundary_calls);
        }
    }

    pub(super) fn record_color_stage(&self, diagnostics: RenderColorStageDiagnostics) {
        bump(&self.metrics.color_stage_plans);
        add_cell(
            &self.metrics.color_stage_total_stages,
            diagnostics.total_stages,
        );
        add_cell(
            &self.metrics.color_stage_cpu_input_stages,
            diagnostics.cpu_input_stages,
        );
        add_cell(
            &self.metrics.color_stage_cpu_output_stages,
            diagnostics.cpu_output_stages,
        );
        add_cell(
            &self.metrics.color_stage_gpu_color_stages,
            diagnostics.gpu_color_stages,
        );
        add_cell(
            &self.metrics.color_stage_upload_stages,
            diagnostics.upload_stages,
        );
        add_cell(
            &self.metrics.color_stage_readback_stages,
            diagnostics.readback_stages,
        );
        add_cell(
            &self.metrics.color_stage_gpu_blockers,
            diagnostics.gpu_blockers,
        );
        add_cell(
            &self.metrics.color_stage_gpu_shader_module_blockers,
            diagnostics.gpu_blocker_breakdown.shader_module_not_prepared,
        );
        add_cell(
            &self.metrics.color_stage_gpu_ocio_resource_blockers,
            diagnostics.gpu_blocker_breakdown.ocio_resource_bind_group_not_prepared,
        );
        add_cell(
            &self.metrics.color_stage_gpu_wrapper_blockers,
            diagnostics.gpu_blocker_breakdown.fullscreen_wrapper_not_prepared,
        );
        add_cell(
            &self.metrics.color_stage_gpu_render_pipeline_blockers,
            diagnostics.gpu_blocker_breakdown.render_pipeline_not_prepared,
        );
        add_cell(
            &self.metrics.color_stage_gpu_ocio_config_blockers,
            diagnostics.gpu_blocker_breakdown.ocio_config_not_loaded,
        );
        add_cell(
            &self.metrics.color_stage_gpu_ocio_processor_blockers,
            diagnostics.gpu_blocker_breakdown.ocio_processor_unavailable,
        );
        add_cell(
            &self.metrics.color_stage_gpu_ocio_shader_extraction_blockers,
            diagnostics.gpu_blocker_breakdown.ocio_gpu_shader_extraction_failed,
        );
        add_cell(&self.metrics.color_stage_pixels, diagnostics.stage_pixels);
    }

    pub(super) fn record_preview_decode(
        &self,
        diagnostics: PreviewDecodeDiagnostics,
        priority: MediaPreviewRequestPriority,
        queue_wait_us: u64,
        count_playback_current_success: bool,
    ) {
        if count_playback_current_success {
            self.record_playback_current_success(priority, diagnostics.access_mode);
        }
        self.record_playback_current_hardware_recovery(priority, &diagnostics);
        match diagnostics.path {
            PreviewDecodePath::InProcessFfmpegCpuRgba
            | PreviewDecodePath::InProcessFfmpegCpuFloat => {
                bump(&self.metrics.decode_in_process_cpu_frames);
            }
            PreviewDecodePath::InProcessFfmpegNative => {}
            PreviewDecodePath::ExternalFfmpegCpuRgba => {
                bump(&self.metrics.decode_external_ffmpeg_cpu_rgba_frames);
            }
            PreviewDecodePath::PlaybackSessionRingHit => {
                bump(&self.metrics.decode_playback_session_ring_hit_frames);
            }
            PreviewDecodePath::PreviewCacheHit => {
                bump(&self.metrics.decode_cache_hit_frames);
            }
        }
        match diagnostics.access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                bump(&self.metrics.decode_playback_cursor_frames);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                bump(&self.metrics.decode_scrub_cursor_frames);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                bump(&self.metrics.decode_random_access_still_frames);
            }
        }
        add_cell(
            &self.metrics.decode_total_duration_us,
            diagnostics.elapsed_us,
        );
        if diagnostics.elapsed_us >= self.metrics.decode_max_duration_us.get() {
            self.metrics.decode_max_duration_us.set(diagnostics.elapsed_us);
            self.metrics.decode_max_frame_stage_durations.set(diagnostics.stage_durations);
            self.metrics.decode_max_frame_queue_wait_us.set(queue_wait_us);
            self.metrics.decode_max_frame_bottleneck.set(classify_preview_decode_bottleneck(
                diagnostics.stage_durations,
                queue_wait_us,
            ));
        }
        self.metrics.decode_last_duration_us.set(diagnostics.elapsed_us);
        if diagnostics.seek_performed {
            bump(&self.metrics.decode_seeked_frames);
        }
        add_cell(
            &self.metrics.decode_decoded_frame_count,
            u64::from(diagnostics.decoded_frame_count),
        );
        self.metrics.decode_max_decoded_frame_count.set(
            self.metrics
                .decode_max_decoded_frame_count
                .get()
                .max(u64::from(diagnostics.decoded_frame_count)),
        );
        match diagnostics.threading_kind {
            PreviewDecodeThreadingKind::None => bump(&self.metrics.decode_threading_none_frames),
            PreviewDecodeThreadingKind::Frame => bump(&self.metrics.decode_threading_frame_frames),
            PreviewDecodeThreadingKind::Slice => bump(&self.metrics.decode_threading_slice_frames),
        }
        let threading_count = u64::from(diagnostics.threading_count);
        self.metrics.decode_last_threading_count.set(threading_count);
        self.metrics
            .decode_max_threading_count
            .set(self.metrics.decode_max_threading_count.get().max(threading_count));
        let mut stage_durations = self.metrics.decode_stage_durations.get();
        stage_durations.accumulate(diagnostics.stage_durations);
        self.metrics.decode_stage_durations.set(stage_durations);
        let mut access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        access_mode_profiles.record(diagnostics, queue_wait_us);
        self.metrics.decode_access_mode_profiles.set(access_mode_profiles);
    }

    pub(super) fn record_startup_preroll_decode(
        &self,
        diagnostics: PreviewDecodeDiagnostics,
        queue_wait_us: u64,
    ) {
        bump(&self.metrics.decode_startup_preroll_frames);
        add_cell(
            &self.metrics.decode_startup_preroll_total_duration_us,
            diagnostics.elapsed_us,
        );
        self.metrics.decode_startup_preroll_max_duration_us.set(
            self.metrics
                .decode_startup_preroll_max_duration_us
                .get()
                .max(diagnostics.elapsed_us),
        );
        add_cell(
            &self.metrics.decode_startup_preroll_queue_wait_total_us,
            queue_wait_us,
        );
        self.metrics
            .decode_startup_preroll_queue_wait_max_us
            .set(self.metrics.decode_startup_preroll_queue_wait_max_us.get().max(queue_wait_us));
    }

    pub(super) fn record_preview_decode_cancel(
        &self,
        access_mode: PreviewDecodeAccessMode,
        reason: Option<MediaPreviewCancelReason>,
        decode_cancellation: Option<mondrian_media::PreviewDecodeCancellation>,
        elapsed_us: u64,
        observed_elapsed_us: Option<u64>,
        request_to_observed_us: Option<u64>,
        owns_pending_playback_demand: bool,
    ) {
        let reason = reason.unwrap_or(MediaPreviewCancelReason::Unknown);
        if reason == MediaPreviewCancelReason::PlaybackDeadline && owns_pending_playback_demand {
            self.record_playback_current_late_drop(1);
        }
        if let Some(decode_cancellation) = decode_cancellation {
            let mut evidence = self.metrics.decode_cancellation_checkpoints.get();
            evidence.observe(decode_cancellation);
            self.metrics.decode_cancellation_checkpoints.set(evidence);
        }
        self.metrics.decode_cancellation.borrow_mut().observe(
            mondrian_playback::FrameCancellationObservation {
                work_class: media_preview_frame_work_class(access_mode),
                cause: reason.playback_cause(),
                execution_duration: Duration::from_micros(elapsed_us),
                execution_to_checkpoint: observed_elapsed_us.map(Duration::from_micros),
                request_to_checkpoint: request_to_observed_us.map(Duration::from_micros),
            },
        );
    }

    pub(super) fn record_preview_decode_failure(
        &self,
        access_mode: PreviewDecodeAccessMode,
        reason: Option<MediaPreviewFailureReason>,
    ) {
        let reason = reason.unwrap_or(MediaPreviewFailureReason::DecodeError);
        bump(&self.metrics.decode_failures);
        match reason {
            MediaPreviewFailureReason::Timeout => {
                bump(&self.metrics.decode_timeout_failures);
            }
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted => {
                bump(&self.metrics.decode_budget_exhausted_failures);
            }
            MediaPreviewFailureReason::DecodeError => {}
        }
        let mut access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        access_mode_profiles.record_failure(access_mode, reason);
        self.metrics.decode_access_mode_profiles.set(access_mode_profiles);
    }

    pub(super) fn record_preview_decode_queue_wait(
        &self,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        queue_wait_us: u64,
    ) {
        add_cell(&self.metrics.decode_queue_wait_total_us, queue_wait_us);
        self.metrics
            .decode_queue_wait_max_us
            .set(self.metrics.decode_queue_wait_max_us.get().max(queue_wait_us));
        self.metrics.decode_queue_wait_last_us.set(queue_wait_us);
        match priority {
            MediaPreviewRequestPriority::Current => {
                self.metrics
                    .decode_current_queue_wait_max_us
                    .set(self.metrics.decode_current_queue_wait_max_us.get().max(queue_wait_us));
            }
            MediaPreviewRequestPriority::Prefetch => {
                self.metrics
                    .decode_prefetch_queue_wait_max_us
                    .set(self.metrics.decode_prefetch_queue_wait_max_us.get().max(queue_wait_us));
            }
        }
        let mut access_mode_profiles = self.metrics.decode_access_mode_profiles.get();
        access_mode_profiles.record_queue_wait(access_mode, queue_wait_us);
        self.metrics.decode_access_mode_profiles.set(access_mode_profiles);
    }

    pub(super) fn playback_schedule_diagnostics(&self) -> PreviewPlaybackScheduleDiagnostics {
        PreviewPlaybackScheduleDiagnostics {
            last_current_deadline_budget_us: self.metrics.playback_current_deadline_budget_us.get(),
            current_deadline_assignments: self.metrics.playback_current_deadline_assignments.get(),
            current_deadline_missing_frame_rate: self
                .metrics
                .playback_current_deadline_missing_frame_rate
                .get(),
            current_decode_decisions: self.metrics.playback_current_decode_decisions.get(),
            current_drop_late_decisions: self.metrics.playback_current_drop_late_decisions.get(),
            current_sustained_pressure_skips: self
                .metrics
                .playback_current_sustained_pressure_skips
                .get(),
            current_proxy_or_hardware_recommended_decisions: self
                .metrics
                .playback_current_proxy_or_hardware_recommended_decisions
                .get(),
            current_native_import_unavailable_decisions: self
                .metrics
                .playback_current_native_import_unavailable_decisions
                .get(),
            current_hardware_fallback_not_engaged_decisions: self
                .metrics
                .playback_current_hardware_fallback_not_engaged_decisions
                .get(),
            current_proxy_generation_requests: self.metrics.media_proxy_generation_requests.get(),
            current_proxy_generation_request_dedupes: self
                .metrics
                .media_proxy_generation_request_dedupes
                .get(),
            current_late_streak: self.playback_pressure.get().late_streak(),
            sustained_pressure_active: self.playback_sustained_pressure_active(),
            sustained_pressure_events: self.metrics.playback_sustained_pressure_events.get(),
            sustained_pressure_recoveries: self
                .metrics
                .playback_sustained_pressure_recoveries
                .get(),
            prefetch_skipped_sustained_pressure: self
                .metrics
                .playback_prefetch_skipped_sustained_pressure
                .get(),
            forward_prefetch_horizon_us: MEDIA_PREVIEW_FORWARD_PREFETCH_HORIZON_US,
            last_forward_prefetch_window_frames: self
                .metrics
                .playback_forward_prefetch_window_frames
                .get(),
            forward_prefetch_min_frames: MEDIA_PREVIEW_FORWARD_PREFETCH_MIN_FRAMES,
            forward_prefetch_max_frames: MEDIA_PREVIEW_FORWARD_PREFETCH_MAX_FRAMES,
            forward_prefetch_window_evaluations: self
                .metrics
                .playback_forward_prefetch_window_evaluations
                .get(),
            forward_prefetch_invalid_frame_rate: self
                .metrics
                .playback_forward_prefetch_invalid_frame_rate
                .get(),
        }
    }

    pub(super) fn record_playback_current_deadline_budget(&self, budget_us: Option<u64>) {
        match budget_us {
            Some(budget_us) => {
                self.metrics.playback_current_deadline_budget_us.set(Some(budget_us));
                bump(&self.metrics.playback_current_deadline_assignments);
                bump(&self.metrics.playback_current_decode_decisions);
            }
            None => {
                bump(&self.metrics.playback_current_deadline_missing_frame_rate);
                bump(&self.metrics.playback_current_proxy_or_hardware_recommended_decisions);
            }
        }
    }

    pub(super) fn record_playback_current_late_drop(&self, count: u64) {
        if count == 0 {
            return;
        }
        add_cell(&self.metrics.playback_current_drop_late_decisions, count);
        add_cell(
            &self.metrics.playback_current_proxy_or_hardware_recommended_decisions,
            count,
        );
        let mut pressure = self.playback_pressure.get();
        let transition = pressure.observe_late(count);
        self.playback_pressure.set(pressure);
        if transition == PlaybackPressureTransition::Entered {
            bump(&self.metrics.playback_sustained_pressure_events);
        }
    }

    pub(super) fn record_playback_current_sustained_pressure_skip(&self) {
        bump(&self.metrics.playback_current_sustained_pressure_skips);
        bump(&self.metrics.playback_current_proxy_or_hardware_recommended_decisions);
    }

    pub(super) fn playback_realtime_work_pending(&self) -> bool {
        let queue = self.jobs.diagnostics();
        queue.queued_current_jobs > 0
            || queue.in_flight_current_jobs > 0
            || queue.queued_playback_cursor_jobs > 0
            || queue.in_flight_playback_cursor_jobs > 0
    }

    pub(super) fn record_playback_current_success(
        &self,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) {
        let mut pressure = self.playback_pressure.get();
        let transition = pressure.observe_success(priority, access_mode);
        self.playback_pressure.set(pressure);
        if transition == PlaybackPressureTransition::Recovered {
            bump(&self.metrics.playback_sustained_pressure_recoveries);
        }
    }

    pub(super) fn record_playback_current_hardware_recovery(
        &self,
        priority: MediaPreviewRequestPriority,
        diagnostics: &PreviewDecodeDiagnostics,
    ) {
        let admission = self.hardware_decode_admission_diagnostics();
        let signals = playback_hardware_recovery_signals(
            priority,
            admission.playback_request,
            admission.native_import_admission_ready,
            PlaybackDecodeExecution::from(diagnostics),
        );
        if signals.native_import_unavailable {
            bump(&self.metrics.playback_current_native_import_unavailable_decisions);
        }
        if signals.hardware_fallback_not_engaged {
            bump(&self.metrics.playback_current_hardware_fallback_not_engaged_decisions);
        }
        if signals.recovery_recommended() {
            bump(&self.metrics.playback_current_proxy_or_hardware_recommended_decisions);
        }
    }

    pub(super) fn playback_sustained_pressure_active(&self) -> bool {
        self.playback_pressure.get().is_active()
    }

    pub(super) fn record_playback_forward_prefetch_window(&self, window_frames: Option<usize>) {
        match window_frames {
            Some(window_frames) => {
                self.metrics.playback_forward_prefetch_window_frames.set(Some(window_frames));
                bump(&self.metrics.playback_forward_prefetch_window_evaluations);
            }
            None => bump(&self.metrics.playback_forward_prefetch_invalid_frame_rate),
        }
    }

    pub(super) fn record_render_stage_durations(
        &self,
        total_duration_us: u64,
        durations: PreviewRenderStageDurations,
    ) {
        bump(&self.metrics.render_timed_frames);
        add_cell(&self.metrics.render_total_duration_us, total_duration_us);
        if total_duration_us >= self.metrics.render_max_duration_us.get() {
            self.metrics.render_max_duration_us.set(total_duration_us);
            self.metrics.render_max_frame_stage_durations.set(durations);
        }
        self.metrics.render_last_duration_us.set(total_duration_us);
        let mut stage_durations = self.metrics.render_stage_durations.get();
        stage_durations.accumulate(durations);
        self.metrics.render_stage_durations.set(stage_durations);
    }

    pub(super) fn record_cpu_execution_evidence(&self, output: &PreviewCompositeOutput) {
        for diagnostics in &output.input_color_diagnostics {
            self.record_color_transform(*diagnostics);
        }
        if output.input_color_stage_diagnostics != RenderColorStageDiagnostics::default() {
            self.record_color_stage(output.input_color_stage_diagnostics);
        }
        if output.composite_diagnostics.legacy_rgba8_composites > 0 {
            self.record_preview_gpu_output_blocker(
                &PreviewGpuOutputBlocker::LegacyRgba8CompositeBoundary {
                    legacy_composites: output.composite_diagnostics.legacy_rgba8_composites,
                },
            );
        }
    }

    pub(super) fn record_composite(&self, diagnostics: TimelineCompositeDiagnostics) {
        bump(&self.metrics.color_composite_plans);
        add_cell(&self.metrics.color_composite_elements, diagnostics.elements);
        add_cell(
            &self.metrics.color_composite_float_linear,
            diagnostics.float_linear_composites,
        );
        add_cell(
            &self.metrics.color_composite_legacy_rgba8,
            diagnostics.legacy_rgba8_composites,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_blend_mode,
            diagnostics.legacy_media_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_transform,
            diagnostics.legacy_media_transform,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_effect,
            diagnostics.legacy_media_effect,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_blend_mode,
            diagnostics.legacy_solid_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_transform,
            diagnostics.legacy_solid_transform,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_effect,
            diagnostics.legacy_solid_effect,
        );
        add_cell(
            &self.metrics.color_composite_legacy_adjustment_blend_mode,
            diagnostics.legacy_adjustment_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_adjustment_effect,
            diagnostics.legacy_adjustment_effect,
        );
        add_cell(
            &self.metrics.color_composite_blocked_domains,
            diagnostics.blocked_color_domain_composites,
        );
        add_cell(
            &self.metrics.color_composite_blocked_media_effect_domain,
            diagnostics.blocked_media_effect_domain,
        );
        add_cell(
            &self.metrics.color_composite_blocked_solid_effect_domain,
            diagnostics.blocked_solid_effect_domain,
        );
        add_cell(
            &self.metrics.color_composite_blocked_adjustment_effect_domain,
            diagnostics.blocked_adjustment_effect_domain,
        );
    }

    pub(crate) fn record_cpu_output_fallback(&self, width: u32, height: u32) {
        bump(&self.metrics.cpu_output_fallback_frames);
        add_cell(
            &self.metrics.cpu_output_fallback_pixels,
            (width as u64).saturating_mul(height as u64),
        );
    }

    pub(crate) fn record_preview_gpu_output_blocker(
        &self,
        blocker: &crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker,
    ) {
        bump(&self.metrics.preview_gpu_output_blocker_frames);
        self.metrics.preview_gpu_output_blocker_breakdown.borrow_mut().record(blocker);
    }

    pub(crate) fn record_preview_gpu_output_blocker_breakdown(
        &self,
        breakdown: crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    ) {
        if breakdown.is_empty() {
            return;
        }
        bump(&self.metrics.preview_gpu_output_blocker_frames);
        self.metrics
            .preview_gpu_output_blocker_breakdown
            .borrow_mut()
            .accumulate(breakdown);
    }

    pub(crate) fn record_gpu_compositing(&self, diagnostics: GpuCompositingDiagnostics) {
        if diagnostics == GpuCompositingDiagnostics::default() {
            return;
        }
        self.metrics.gpu_compositing.borrow_mut().accumulate(diagnostics);
    }
}
