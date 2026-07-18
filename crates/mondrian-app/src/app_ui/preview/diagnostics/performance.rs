//! Versioned Preview decode/render performance report derivation and bottleneck classification.

use super::*;

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

pub(in super::super) fn classify_preview_decode_bottleneck(
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

pub(super) fn classify_preview_render_bottleneck(
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
