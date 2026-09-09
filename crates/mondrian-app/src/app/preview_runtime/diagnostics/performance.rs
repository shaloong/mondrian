//! Versioned Preview decode/render performance report derivation and bottleneck classification.

use super::*;

impl PreviewDecodePerformanceSummary {
    /// Build the versioned preview decode performance report for this summary.
    pub fn performance_report(self, profile: impl Into<String>) -> PreviewDecodePerformanceReport {
        build_preview_decode_performance_report(
            Some(self),
            profile,
            PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
        )
    }
}

impl PreviewRenderPerformanceSummary {
    /// Build the versioned preview render performance report for this summary.
    pub fn performance_report(self, profile: impl Into<String>) -> PreviewRenderPerformanceReport {
        build_preview_render_performance_report(
            Some(self),
            profile,
            PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
        )
    }
}

/// Build a versioned preview render performance report from an optional summary.
pub fn build_preview_render_performance_report(
    summary: Option<PreviewRenderPerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
) -> PreviewRenderPerformanceReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();

    push_render_bool_check(
        &mut checks,
        PreviewRenderPerformanceArea::CaptureIntegrity,
        "preview_render_evidence_present",
        summary.map(|summary| summary.timed_frames > 0).unwrap_or(false),
    );

    if let Some(mut summary) = summary {
        summary.slow_frame_budget_us = slow_frame_budget_us;
        summary.primary_bottleneck =
            classify_preview_render_bottleneck(summary.max_frame_stage_durations);
        push_render_max_check(
            &mut checks,
            PreviewRenderPerformanceArea::LatencyBudget,
            "preview_render_max_frame_us",
            summary.max_duration_us,
            slow_frame_budget_us,
        );
        push_render_max_check(
            &mut checks,
            PreviewRenderPerformanceArea::CaptureIntegrity,
            "preview_render_blocked_outputs",
            summary.unavailability.blocked,
            0,
        );
        push_render_max_check(
            &mut checks,
            PreviewRenderPerformanceArea::CaptureIntegrity,
            "preview_render_failed_outputs",
            summary.unavailability.failed,
            0,
        );
        push_preview_render_root_causes_and_actions(summary, &mut root_causes, &mut actions);

        let verdict = preview_render_verdict(&checks);
        return PreviewRenderPerformanceReport {
            schema_version: PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION,
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
        PreviewRenderPerformanceArea::CaptureIntegrity,
        "missing_preview_render_evidence",
        "preview_render_evidence_present=false".to_owned(),
        "capture_preview_render_stage_durations",
        "Ensure viewer preview records post-decode render stage timings from the real playback path.",
    );

    let verdict = preview_render_verdict(&checks);
    PreviewRenderPerformanceReport {
        schema_version: PREVIEW_RENDER_PERFORMANCE_REPORT_SCHEMA_VERSION,
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
    summary: Option<PreviewDecodePerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
) -> PreviewDecodePerformanceReport {
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
/// actually reached the media decode boundary. General product diagnostics should
/// use [`build_preview_decode_performance_report`] so idle profiles do not fail
/// merely because they did not exercise every mode.
pub fn build_preview_decode_performance_report_with_required_access_modes(
    summary: Option<PreviewDecodePerformanceSummary>,
    profile: impl Into<String>,
    slow_frame_budget_us: u64,
    required_access_modes: &[PreviewDecodeAccessMode],
) -> PreviewDecodePerformanceReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();
    let required_access_modes = required_access_modes.to_vec();
    let policy = preview_decode_performance_policy(slow_frame_budget_us, &required_access_modes);

    push_decode_bool_check(
        &mut checks,
        PreviewDecodePerformanceArea::CaptureIntegrity,
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
        push_decode_exact_check(
            &mut checks,
            PreviewDecodePerformanceArea::CaptureIntegrity,
            "preview_decode_access_mode_success_accounting",
            summary.access_mode_profiles.total_frames(),
            summary.decode_successes.saturating_sub(summary.startup_preroll_frames),
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::CaptureIntegrity,
            "preview_decode_startup_preroll_success_accounting",
            summary.startup_preroll_frames,
            summary.decode_successes,
        );
        push_preview_decode_access_mode_coverage_checks(
            &mut checks,
            summary,
            &required_access_modes,
        );
        push_preview_decode_work_class_checks(&mut checks, summary, &policy);
        push_preview_decode_session_churn_checks(&mut checks, summary, &policy);
        push_preview_decode_access_mode_operational_checks(
            &mut checks,
            summary,
            policy.queue_wait_budget_us,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::LatencyBudget,
            "preview_decode_timeout_failures",
            summary.decode_timeout_failures,
            0,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_forward_budget_exhausted_failures",
            summary.decode_budget_exhausted_failures,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_max_decoded_frame_count",
            summary.max_decoded_frame_count,
            1,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_seeked_frames",
            summary.seeked_frames,
            0,
        );
        push_decode_warn_min_check(
            &mut checks,
            PreviewDecodePerformanceArea::ProxyCache,
            "preview_decode_cache_hit_frames",
            summary.playback_session_ring_hit_frames,
            1,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_wait_max_us",
            summary.queue_wait_max_us,
            slow_frame_budget_us,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_expired_playback_current_queue",
            (summary.worker_queue.queued_expired_playback_current_jobs as u64)
                .saturating_add(summary.worker_queue.dropped_expired_playback_current_jobs),
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_current_stall_expirations",
            summary.playback_current_stalled_expirations,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_sustained_pressure_events",
            summary.playback_schedule.sustained_pressure_events,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_hardware_decode_admission_gated",
            u64::from(summary.hardware_decode_admission.playback_native_import_gated()),
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_playback_hardware_fallback_not_engaged",
            summary
                .access_mode_profiles
                .playback_cursor
                .hardware_decode_fallback_not_engaged_frames(),
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_native_import_unavailable_playback_frames",
            summary.playback_schedule.current_native_import_unavailable_decisions,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_hardware_fallback_not_engaged_decisions",
            summary.playback_schedule.current_hardware_fallback_not_engaged_decisions,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_deadline_cancellations",
            summary.canceled_prefetch_deadline_jobs,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_preempted_by_current_cancellations",
            summary.canceled_prefetch_preempted_jobs,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_deadline_invalid_frame_rate",
            summary.playback_schedule.current_deadline_missing_frame_rate,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_window_invalid_frame_rate",
            summary.playback_schedule.forward_prefetch_invalid_frame_rate,
            0,
        );
        push_decode_warn_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancellation_gate",
            cancellation_gate.passed,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_logical_cancel_observation_max_us",
            summary.cancellation.all.request_to_logical_cancellation.max_us,
            cancellation_policy.max_request_to_logical_cancellation.as_micros() as u64,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancel_unknown_causes",
            summary.cancellation.all.unknown,
            0,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancel_missing_request_evidence",
            summary
                .cancellation
                .all
                .cancellations
                .saturating_sub(summary.cancellation.all.unknown)
                .saturating_sub(summary.cancellation.all.request_to_logical_cancellation.samples),
            0,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_playback_cancel_return_latency_max_us",
            summary.cancellation.playback.logical_cancellation_to_return.max_us,
            cancellation_policy.max_playback_logical_cancellation_to_return.as_micros() as u64,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_interactive_cancel_return_latency_max_us",
            summary.cancellation.interactive.logical_cancellation_to_return.max_us,
            cancellation_policy.max_interactive_logical_cancellation_to_return.as_micros() as u64,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_still_cancel_return_latency_max_us",
            summary.cancellation.still.logical_cancellation_to_return.max_us,
            cancellation_policy.max_still_logical_cancellation_to_return.as_micros() as u64,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_queue_full_drops",
            summary.queue_full_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_invalid_access_mode_drops",
            summary.queue_invalid_access_mode_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_disconnected_drops",
            summary.worker_disconnected_drops,
            0,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_invalid_access_mode_requests",
            summary.scheduler.dropped_invalid_access_mode_requests,
            0,
        );
        push_decode_max_check(
            &mut checks,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_broker_clock_regressions",
            summary.scheduler.clock_regressions,
            0,
        );

        push_preview_decode_root_causes_and_actions(
            summary,
            &policy,
            &required_access_modes,
            &mut root_causes,
            &mut actions,
        );

        let verdict = preview_decode_verdict(&checks);
        return PreviewDecodePerformanceReport {
            schema_version: PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION,
            profile: profile.into(),
            verdict,
            required_access_modes,
            policy,
            summary: Some(summary),
            checks,
            root_causes,
            actions,
        };
    }

    push_decode_root_cause_with_action(
        &mut root_causes,
        &mut actions,
        PreviewDecodePerformanceArea::CaptureIntegrity,
        "missing_preview_decode_evidence",
        "preview_decode_evidence_present=false".to_owned(),
        "capture_preview_decode_diagnostics",
        "Ensure preview media jobs record decode diagnostics from the real playback path.",
        PreviewDecodePerformanceSeverity::Fail,
    );

    let verdict = preview_decode_verdict(&checks);
    PreviewDecodePerformanceReport {
        schema_version: PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION,
        profile: profile.into(),
        verdict,
        required_access_modes,
        policy,
        summary: None,
        checks,
        root_causes,
        actions,
    }
}

fn preview_decode_verdict(
    checks: &[PreviewDecodePerformanceCheck],
) -> PreviewDecodePerformanceVerdict {
    if checks
        .iter()
        .any(|check| check.severity == PreviewDecodePerformanceSeverity::Fail)
    {
        PreviewDecodePerformanceVerdict::Fail
    } else if checks
        .iter()
        .any(|check| check.severity == PreviewDecodePerformanceSeverity::Warn)
    {
        PreviewDecodePerformanceVerdict::Warn
    } else {
        PreviewDecodePerformanceVerdict::Pass
    }
}

fn preview_render_verdict(
    checks: &[PreviewRenderPerformanceCheck],
) -> PreviewRenderPerformanceVerdict {
    if checks
        .iter()
        .any(|check| check.severity == PreviewRenderPerformanceSeverity::Fail)
    {
        PreviewRenderPerformanceVerdict::Fail
    } else {
        PreviewRenderPerformanceVerdict::Pass
    }
}

fn push_render_max_check(
    checks: &mut Vec<PreviewRenderPerformanceCheck>,
    area: PreviewRenderPerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(PreviewRenderPerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            PreviewRenderPerformanceSeverity::Fail
        } else {
            PreviewRenderPerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_render_bool_check(
    checks: &mut Vec<PreviewRenderPerformanceCheck>,
    area: PreviewRenderPerformanceArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(PreviewRenderPerformanceCheck {
        area,
        code,
        severity: if passed {
            PreviewRenderPerformanceSeverity::Pass
        } else {
            PreviewRenderPerformanceSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_decode_max_check(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    area: PreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(PreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            PreviewDecodePerformanceSeverity::Fail
        } else {
            PreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_min_check(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    area: PreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(PreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed < limit {
            PreviewDecodePerformanceSeverity::Fail
        } else {
            PreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_exact_check(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    area: PreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    expected: u64,
) {
    checks.push(PreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed == expected {
            PreviewDecodePerformanceSeverity::Pass
        } else {
            PreviewDecodePerformanceSeverity::Fail
        },
        observed,
        limit: Some(expected),
    });
}

fn push_decode_warn_max_check(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    area: PreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(PreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed > limit {
            PreviewDecodePerformanceSeverity::Warn
        } else {
            PreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_warn_min_check(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    area: PreviewDecodePerformanceArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(PreviewDecodePerformanceCheck {
        area,
        code,
        severity: if observed < limit {
            PreviewDecodePerformanceSeverity::Warn
        } else {
            PreviewDecodePerformanceSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_decode_bool_check(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    area: PreviewDecodePerformanceArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(PreviewDecodePerformanceCheck {
        area,
        code,
        severity: if passed {
            PreviewDecodePerformanceSeverity::Pass
        } else {
            PreviewDecodePerformanceSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn preview_decode_performance_policy(
    slow_frame_budget_us: u64,
    required_access_modes: &[PreviewDecodeAccessMode],
) -> PreviewDecodePerformancePolicy {
    const ACCESS_MODES: [PreviewDecodeAccessMode; 3] = [
        PreviewDecodeAccessMode::PlaybackCursor,
        PreviewDecodeAccessMode::ScrubCursor,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
    ];
    const WORK_CLASSES: [PreviewDecodeWorkClass; 7] = [
        PreviewDecodeWorkClass::CacheHit,
        PreviewDecodeWorkClass::SessionOpened,
        PreviewDecodeWorkClass::SessionReplaced,
        PreviewDecodeWorkClass::ForwardSteady,
        PreviewDecodeWorkClass::ReusedSeek,
        PreviewDecodeWorkClass::ReusedOther,
        PreviewDecodeWorkClass::Unclassified,
    ];
    const REUSED_SEEK_BUDGET_US: u64 = 500_000;
    const PLAYBACK_FORWARD_STEADY_MAX_BUDGET_MULTIPLIER: u64 = 5;

    let mut work_budgets = Vec::with_capacity(ACCESS_MODES.len() * WORK_CLASSES.len());
    for access_mode in ACCESS_MODES {
        let required = required_access_modes.contains(&access_mode);
        for work_class in WORK_CLASSES {
            let min_samples = match (required, access_mode, work_class) {
                (
                    true,
                    PreviewDecodeAccessMode::PlaybackCursor,
                    PreviewDecodeWorkClass::ForwardSteady,
                ) => 4,
                (
                    true,
                    PreviewDecodeAccessMode::ScrubCursor
                    | PreviewDecodeAccessMode::RandomAccessStillFrame,
                    PreviewDecodeWorkClass::ReusedSeek,
                ) => 1,
                _ => 0,
            };
            let (max_worker_execution_us, p95_worker_execution_us) = match work_class {
                PreviewDecodeWorkClass::SessionOpened | PreviewDecodeWorkClass::SessionReplaced => {
                    (PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US, None)
                }
                PreviewDecodeWorkClass::ReusedSeek => {
                    (REUSED_SEEK_BUDGET_US, Some(REUSED_SEEK_BUDGET_US))
                }
                PreviewDecodeWorkClass::Unclassified => (0, None),
                PreviewDecodeWorkClass::ForwardSteady
                    if access_mode == PreviewDecodeAccessMode::PlaybackCursor =>
                {
                    // Playback executes bounded lookahead specifically so a
                    // rare source/GOP or OS-scheduling spike can be absorbed
                    // without disturbing cadence. Keep the cadence budget on
                    // p95 while retaining a finite five-frame hard bound for
                    // individual prefetched work.
                    (
                        slow_frame_budget_us
                            .saturating_mul(PLAYBACK_FORWARD_STEADY_MAX_BUDGET_MULTIPLIER),
                        Some(slow_frame_budget_us),
                    )
                }
                PreviewDecodeWorkClass::CacheHit
                | PreviewDecodeWorkClass::ForwardSteady
                | PreviewDecodeWorkClass::ReusedOther => {
                    (slow_frame_budget_us, Some(slow_frame_budget_us))
                }
            };
            work_budgets.push(PreviewDecodeWorkBudget {
                access_mode,
                work_class,
                min_samples,
                max_worker_execution_us,
                p95_worker_execution_us,
            });
        }
    }

    PreviewDecodePerformancePolicy {
        work_budgets,
        queue_wait_budget_us: slow_frame_budget_us,
        session_churn_grace_frames: 2,
        max_session_churn_basis_points: 2_500,
    }
}

fn push_preview_decode_work_class_checks(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    summary: PreviewDecodePerformanceSummary,
    policy: &PreviewDecodePerformancePolicy,
) {
    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        if profile.frames > 0 {
            push_decode_exact_check(
                checks,
                PreviewDecodePerformanceArea::CaptureIntegrity,
                preview_decode_work_class_accounting_code(access_mode),
                profile.work_classes.total_frames(),
                profile.frames,
            );
            push_decode_exact_check(
                checks,
                PreviewDecodePerformanceArea::CaptureIntegrity,
                preview_decode_lifecycle_accounting_code(access_mode),
                profile.successful_lifecycle_frames(),
                profile.frames,
            );
            push_decode_min_check(
                checks,
                PreviewDecodePerformanceArea::CaptureIntegrity,
                preview_decode_queue_wait_coverage_code(access_mode),
                profile.queue_wait_samples,
                profile.frames,
            );
        }
        push_decode_exact_check(
            checks,
            PreviewDecodePerformanceArea::CaptureIntegrity,
            preview_decode_queue_wait_histogram_accounting_code(access_mode),
            profile.queue_wait_buckets.total(),
            profile.queue_wait_samples,
        );
        push_decode_exact_check(
            checks,
            PreviewDecodePerformanceArea::CaptureIntegrity,
            preview_decode_expired_queue_wait_histogram_accounting_code(access_mode),
            profile.expired_queue_wait.buckets.total(),
            profile.expired_queue_wait.samples,
        );
        push_decode_exact_check(
            checks,
            PreviewDecodePerformanceArea::CaptureIntegrity,
            preview_decode_canceled_session_open_accounting_code(access_mode),
            profile
                .canceled_session_opened_attempts
                .saturating_add(profile.canceled_session_replaced_attempts),
            profile.canceled_session_open_attempts,
        );
    }
    push_decode_exact_check(
        checks,
        PreviewDecodePerformanceArea::CaptureIntegrity,
        "preview_decode_expired_queue_wait_histogram_samples",
        summary.expired_queue_wait.buckets.total(),
        summary.expired_queue_wait.samples,
    );
    push_decode_exact_check(
        checks,
        PreviewDecodePerformanceArea::CaptureIntegrity,
        "preview_decode_expired_queue_wait_access_mode_samples",
        summary.access_mode_profiles.total_expired_queue_wait_samples(),
        summary.expired_queue_wait.samples,
    );

    for budget in &policy.work_budgets {
        let access_profile = summary.access_mode_profiles.profile_for(budget.access_mode);
        let profile = access_profile.work_classes.profile(budget.work_class);
        let codes = preview_decode_work_check_codes(budget.access_mode, budget.work_class);
        push_decode_exact_check(
            checks,
            PreviewDecodePerformanceArea::CaptureIntegrity,
            codes.histogram_samples,
            profile.latency_buckets.total(),
            profile.frames,
        );

        if budget.work_class == PreviewDecodeWorkClass::Unclassified {
            push_decode_max_check(
                checks,
                PreviewDecodePerformanceArea::CaptureIntegrity,
                codes.samples,
                profile.frames,
                0,
            );
        } else if budget.min_samples > 0 {
            push_decode_min_check(
                checks,
                PreviewDecodePerformanceArea::CaptureIntegrity,
                codes.samples,
                profile.frames,
                budget.min_samples,
            );
        }
        if profile.frames == 0 {
            continue;
        }

        push_decode_max_check(
            checks,
            PreviewDecodePerformanceArea::AccessMode,
            codes.max_worker_execution,
            profile.max_duration_us,
            budget.max_worker_execution_us,
        );
        if let Some(p95_budget_us) = budget.p95_worker_execution_us {
            let p95_upper_bound_us = profile.latency_buckets.p95_upper_bound_us();
            checks.push(PreviewDecodePerformanceCheck {
                area: PreviewDecodePerformanceArea::AccessMode,
                code: codes.p95_worker_execution,
                severity: match p95_upper_bound_us {
                    Some(observed) if observed <= p95_budget_us => {
                        PreviewDecodePerformanceSeverity::Pass
                    }
                    Some(_) | None => PreviewDecodePerformanceSeverity::Fail,
                },
                // `None` means the p95 rank is in the open >5 s bucket. The
                // finite maximum is still real evidence and never masquerades
                // as an upper bound for that quantile.
                observed: p95_upper_bound_us.unwrap_or(profile.max_duration_us),
                limit: Some(p95_budget_us),
            });
        }
    }
}

fn push_preview_decode_session_churn_checks(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    summary: PreviewDecodePerformanceSummary,
    policy: &PreviewDecodePerformancePolicy,
) {
    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        let attempts = profile.session_churn_attempts();
        if attempts == 0 {
            continue;
        }
        let ratio_allowance = attempts
            .saturating_mul(policy.max_session_churn_basis_points)
            .saturating_add(9_999)
            / 10_000;
        let allowed = policy.session_churn_grace_frames.max(ratio_allowance);
        push_decode_max_check(
            checks,
            PreviewDecodePerformanceArea::AccessMode,
            preview_decode_session_churn_code(access_mode),
            profile.session_churn_events(),
            allowed,
        );
        push_decode_max_check(
            checks,
            PreviewDecodePerformanceArea::AccessMode,
            preview_decode_canceled_session_open_max_code(access_mode),
            profile.canceled_session_open_max_duration_us,
            PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US,
        );
    }
}

fn push_preview_decode_access_mode_operational_checks(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    summary: PreviewDecodePerformanceSummary,
    queue_wait_budget_us: u64,
) {
    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        if profile.frames == 0 && profile.queue_wait_samples == 0 {
            continue;
        }
        if profile.frames > 0 && access_mode == PreviewDecodeAccessMode::ScrubCursor {
            checks.push(PreviewDecodePerformanceCheck {
                area: PreviewDecodePerformanceArea::AccessMode,
                code: "preview_decode_scrub_cursor_bounded_any_seek_strategy",
                severity: if profile.bounded_any_seek_strategy_frames == profile.frames {
                    PreviewDecodePerformanceSeverity::Pass
                } else {
                    PreviewDecodePerformanceSeverity::Fail
                },
                observed: profile.bounded_any_seek_strategy_frames,
                limit: Some(profile.frames),
            });
            checks.push(PreviewDecodePerformanceCheck {
                area: PreviewDecodePerformanceArea::AccessMode,
                code: "preview_decode_scrub_cursor_any_seek_window_ms",
                severity: if profile.any_seek_window_ms_max > 0 {
                    PreviewDecodePerformanceSeverity::Pass
                } else {
                    PreviewDecodePerformanceSeverity::Fail
                },
                observed: profile.any_seek_window_ms_max,
                limit: Some(1),
            });
        }
        checks.push(PreviewDecodePerformanceCheck {
            area: PreviewDecodePerformanceArea::AccessMode,
            code: preview_decode_access_mode_queue_wait_budget_code(access_mode),
            severity: if profile.queue_wait_max_us > queue_wait_budget_us {
                PreviewDecodePerformanceSeverity::Warn
            } else {
                PreviewDecodePerformanceSeverity::Pass
            },
            observed: profile.queue_wait_max_us,
            limit: Some(queue_wait_budget_us),
        });
        let queue_wait_p95_upper_bound_us =
            profile.queue_wait_buckets.estimated_p95_upper_bound_us();
        checks.push(PreviewDecodePerformanceCheck {
            area: PreviewDecodePerformanceArea::AccessMode,
            code: preview_decode_access_mode_queue_wait_p95_budget_code(access_mode),
            severity: match queue_wait_p95_upper_bound_us {
                Some(observed) if observed <= queue_wait_budget_us => {
                    PreviewDecodePerformanceSeverity::Pass
                }
                Some(_) | None => PreviewDecodePerformanceSeverity::Warn,
            },
            // If the p95 rank falls into the open >80 ms interval, use the
            // measured finite maximum as evidence; it is not described as a
            // quantile upper bound.
            observed: queue_wait_p95_upper_bound_us.unwrap_or(profile.queue_wait_max_us),
            limit: Some(queue_wait_budget_us),
        });
    }
}

fn push_preview_decode_access_mode_coverage_checks(
    checks: &mut Vec<PreviewDecodePerformanceCheck>,
    summary: PreviewDecodePerformanceSummary,
    required_access_modes: &[PreviewDecodeAccessMode],
) {
    for access_mode in required_access_modes {
        let profile = summary.access_mode_profiles.profile_for(*access_mode);
        push_decode_bool_check(
            checks,
            PreviewDecodePerformanceArea::CaptureIntegrity,
            preview_decode_access_mode_coverage_code(*access_mode),
            profile.frames > 0,
        );
        if profile.frames > 0 {
            push_decode_bool_check(
                checks,
                PreviewDecodePerformanceArea::CaptureIntegrity,
                preview_decode_access_mode_local_coverage_code(*access_mode),
                profile.mode_local_evidence_frames() > 0,
            );
        }
    }
}

#[derive(Clone, Copy)]
struct PreviewDecodeWorkCheckCodes {
    samples: &'static str,
    histogram_samples: &'static str,
    max_worker_execution: &'static str,
    p95_worker_execution: &'static str,
}

macro_rules! preview_decode_work_check_codes {
    ($mode:literal, $class:literal) => {
        PreviewDecodeWorkCheckCodes {
            samples: concat!("preview_decode_", $mode, "_", $class, "_samples"),
            histogram_samples: concat!("preview_decode_", $mode, "_", $class, "_histogram_samples"),
            max_worker_execution: concat!(
                "preview_decode_",
                $mode,
                "_",
                $class,
                "_max_worker_execution_us"
            ),
            p95_worker_execution: concat!(
                "preview_decode_",
                $mode,
                "_",
                $class,
                "_p95_worker_execution_us"
            ),
        }
    };
}

fn preview_decode_work_check_codes(
    access_mode: PreviewDecodeAccessMode,
    work_class: PreviewDecodeWorkClass,
) -> PreviewDecodeWorkCheckCodes {
    match (access_mode, work_class) {
        (PreviewDecodeAccessMode::PlaybackCursor, PreviewDecodeWorkClass::CacheHit) => {
            preview_decode_work_check_codes!("playback_cursor", "cache_hit")
        }
        (PreviewDecodeAccessMode::PlaybackCursor, PreviewDecodeWorkClass::SessionOpened) => {
            preview_decode_work_check_codes!("playback_cursor", "session_opened")
        }
        (PreviewDecodeAccessMode::PlaybackCursor, PreviewDecodeWorkClass::SessionReplaced) => {
            preview_decode_work_check_codes!("playback_cursor", "session_replaced")
        }
        (PreviewDecodeAccessMode::PlaybackCursor, PreviewDecodeWorkClass::ForwardSteady) => {
            preview_decode_work_check_codes!("playback_cursor", "forward_steady")
        }
        (PreviewDecodeAccessMode::PlaybackCursor, PreviewDecodeWorkClass::ReusedSeek) => {
            preview_decode_work_check_codes!("playback_cursor", "reused_seek")
        }
        (PreviewDecodeAccessMode::PlaybackCursor, PreviewDecodeWorkClass::ReusedOther) => {
            preview_decode_work_check_codes!("playback_cursor", "reused_other")
        }
        (PreviewDecodeAccessMode::PlaybackCursor, PreviewDecodeWorkClass::Unclassified) => {
            preview_decode_work_check_codes!("playback_cursor", "unclassified")
        }
        (PreviewDecodeAccessMode::ScrubCursor, PreviewDecodeWorkClass::CacheHit) => {
            preview_decode_work_check_codes!("scrub_cursor", "cache_hit")
        }
        (PreviewDecodeAccessMode::ScrubCursor, PreviewDecodeWorkClass::SessionOpened) => {
            preview_decode_work_check_codes!("scrub_cursor", "session_opened")
        }
        (PreviewDecodeAccessMode::ScrubCursor, PreviewDecodeWorkClass::SessionReplaced) => {
            preview_decode_work_check_codes!("scrub_cursor", "session_replaced")
        }
        (PreviewDecodeAccessMode::ScrubCursor, PreviewDecodeWorkClass::ForwardSteady) => {
            preview_decode_work_check_codes!("scrub_cursor", "forward_steady")
        }
        (PreviewDecodeAccessMode::ScrubCursor, PreviewDecodeWorkClass::ReusedSeek) => {
            preview_decode_work_check_codes!("scrub_cursor", "reused_seek")
        }
        (PreviewDecodeAccessMode::ScrubCursor, PreviewDecodeWorkClass::ReusedOther) => {
            preview_decode_work_check_codes!("scrub_cursor", "reused_other")
        }
        (PreviewDecodeAccessMode::ScrubCursor, PreviewDecodeWorkClass::Unclassified) => {
            preview_decode_work_check_codes!("scrub_cursor", "unclassified")
        }
        (PreviewDecodeAccessMode::RandomAccessStillFrame, PreviewDecodeWorkClass::CacheHit) => {
            preview_decode_work_check_codes!("random_access_still", "cache_hit")
        }
        (
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            PreviewDecodeWorkClass::SessionOpened,
        ) => preview_decode_work_check_codes!("random_access_still", "session_opened"),
        (
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            PreviewDecodeWorkClass::SessionReplaced,
        ) => preview_decode_work_check_codes!("random_access_still", "session_replaced"),
        (
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            PreviewDecodeWorkClass::ForwardSteady,
        ) => preview_decode_work_check_codes!("random_access_still", "forward_steady"),
        (PreviewDecodeAccessMode::RandomAccessStillFrame, PreviewDecodeWorkClass::ReusedSeek) => {
            preview_decode_work_check_codes!("random_access_still", "reused_seek")
        }
        (PreviewDecodeAccessMode::RandomAccessStillFrame, PreviewDecodeWorkClass::ReusedOther) => {
            preview_decode_work_check_codes!("random_access_still", "reused_other")
        }
        (PreviewDecodeAccessMode::RandomAccessStillFrame, PreviewDecodeWorkClass::Unclassified) => {
            preview_decode_work_check_codes!("random_access_still", "unclassified")
        }
    }
}

fn preview_decode_session_churn_code(access_mode: PreviewDecodeAccessMode) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_session_churn_frames"
        }
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_session_churn_frames",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_session_churn_frames"
        }
    }
}

fn preview_decode_work_class_accounting_code(access_mode: PreviewDecodeAccessMode) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_work_class_accounted_frames"
        }
        PreviewDecodeAccessMode::ScrubCursor => {
            "preview_decode_scrub_cursor_work_class_accounted_frames"
        }
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_work_class_accounted_frames"
        }
    }
}

fn preview_decode_lifecycle_accounting_code(access_mode: PreviewDecodeAccessMode) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_lifecycle_accounted_frames"
        }
        PreviewDecodeAccessMode::ScrubCursor => {
            "preview_decode_scrub_cursor_lifecycle_accounted_frames"
        }
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_lifecycle_accounted_frames"
        }
    }
}

fn preview_decode_queue_wait_coverage_code(access_mode: PreviewDecodeAccessMode) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_queue_wait_samples"
        }
        PreviewDecodeAccessMode::ScrubCursor => "preview_decode_scrub_cursor_queue_wait_samples",
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_queue_wait_samples"
        }
    }
}

fn preview_decode_queue_wait_histogram_accounting_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_queue_wait_histogram_samples"
        }
        PreviewDecodeAccessMode::ScrubCursor => {
            "preview_decode_scrub_cursor_queue_wait_histogram_samples"
        }
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_queue_wait_histogram_samples"
        }
    }
}

fn preview_decode_expired_queue_wait_histogram_accounting_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_expired_queue_wait_histogram_samples"
        }
        PreviewDecodeAccessMode::ScrubCursor => {
            "preview_decode_scrub_cursor_expired_queue_wait_histogram_samples"
        }
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_expired_queue_wait_histogram_samples"
        }
    }
}

fn preview_decode_canceled_session_open_accounting_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_canceled_session_open_accounted_attempts"
        }
        PreviewDecodeAccessMode::ScrubCursor => {
            "preview_decode_scrub_cursor_canceled_session_open_accounted_attempts"
        }
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_canceled_session_open_accounted_attempts"
        }
    }
}

fn preview_decode_canceled_session_open_max_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_canceled_session_open_max_us"
        }
        PreviewDecodeAccessMode::ScrubCursor => {
            "preview_decode_scrub_cursor_canceled_session_open_max_us"
        }
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_canceled_session_open_max_us"
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
    summary: PreviewDecodePerformanceSummary,
    policy: &PreviewDecodePerformancePolicy,
    required_access_modes: &[PreviewDecodeAccessMode],
    root_causes: &mut Vec<PreviewDecodePerformanceRootCause>,
    actions: &mut Vec<PreviewDecodePerformanceAction>,
) {
    for access_mode in required_access_modes {
        let profile = summary.access_mode_profiles.profile_for(*access_mode);
        if profile.frames > 0 {
            continue;
        }
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::CaptureIntegrity,
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
            PreviewDecodePerformanceSeverity::Fail,
        );
    }

    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        let accounted_frames = profile.work_classes.total_frames();
        if profile.frames == accounted_frames {
            continue;
        }
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::CaptureIntegrity,
            "preview_decode_work_class_accounting_mismatch",
            format!(
                "access_mode={} successful_frames={} accounted_work_class_frames={} lifecycle_opened_frames={} lifecycle_replaced_frames={} lifecycle_reused_frames={} lifecycle_bypassed_cache_frames={} lifecycle_unclassified_frames={}",
                access_mode.as_str(),
                profile.frames,
                accounted_frames,
                profile.session_opened_frames,
                profile.session_replaced_frames,
                profile.session_reused_frames,
                profile.session_bypassed_cache_frames,
                profile.session_unclassified_frames
            ),
            "restore_preview_decode_work_class_accounting",
            "Classify every successful result exactly once; aggregate frame totals are not a substitute for lifecycle and work evidence.",
            PreviewDecodePerformanceSeverity::Fail,
        );
    }

    for budget in &policy.work_budgets {
        let access_profile = summary.access_mode_profiles.profile_for(budget.access_mode);
        let profile = access_profile.work_classes.profile(budget.work_class);
        if budget.min_samples > 0 && profile.frames < budget.min_samples {
            push_decode_root_cause_with_action(
                root_causes,
                actions,
                PreviewDecodePerformanceArea::CaptureIntegrity,
                "preview_decode_required_work_class_missing",
                format!(
                    "access_mode={} work_class={} samples={} required_samples={}",
                    budget.access_mode.as_str(),
                    budget.work_class.as_str(),
                    profile.frames,
                    budget.min_samples
                ),
                "exercise_required_preview_work_class",
                "Exercise the required access-mode/work-class cell; do not substitute cold opens, cache hits, or another access mode.",
                PreviewDecodePerformanceSeverity::Fail,
            );
        }
        if budget.work_class == PreviewDecodeWorkClass::Unclassified && profile.frames > 0 {
            push_decode_root_cause_with_action(
                root_causes,
                actions,
                PreviewDecodePerformanceArea::CaptureIntegrity,
                "preview_decode_unclassified_work",
                format!(
                    "access_mode={} unclassified_frames={} lifecycle_unclassified_frames={}",
                    budget.access_mode.as_str(),
                    profile.frames,
                    access_profile.session_unclassified_frames
                ),
                "restore_preview_decode_lifecycle_evidence",
                "Make every successful media result attest whether its decoder Session was opened, replaced, reused, or bypassed through a cache.",
                PreviewDecodePerformanceSeverity::Fail,
            );
            continue;
        }
        if profile.frames == 0 {
            continue;
        }
        let p95 = profile.latency_buckets.p95_upper_bound_us();
        let p95_over_budget = budget
            .p95_worker_execution_us
            .is_some_and(|limit| p95.is_none_or(|observed| observed > limit));
        if profile.max_duration_us > budget.max_worker_execution_us || p95_over_budget {
            push_decode_root_cause_with_action(
                root_causes,
                actions,
                PreviewDecodePerformanceArea::AccessMode,
                "preview_decode_work_class_over_budget",
                format!(
                    "access_mode={} work_class={} frames={} max_worker_execution_us={} max_budget_us={} p95_upper_bound_us={:?} p95_budget_us={:?} max_frame_queue_wait_us={} max_frame_bottleneck={:?} primary_bottleneck={:?} latency_buckets={:?}",
                    budget.access_mode.as_str(),
                    budget.work_class.as_str(),
                    profile.frames,
                    profile.max_duration_us,
                    budget.max_worker_execution_us,
                    p95,
                    budget.p95_worker_execution_us,
                    access_profile.max_frame_queue_wait_us,
                    access_profile.max_frame_bottleneck,
                    summary.primary_bottleneck,
                    profile.latency_buckets
                ),
                "inspect_preview_decode_work_class",
                "Inspect this exact access-mode/work-class cell and its media-stage timings; do not weaken unrelated cold-start or steady-state obligations.",
                PreviewDecodePerformanceSeverity::Fail,
            );
        }
    }

    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        let attempts = profile.session_churn_attempts();
        if attempts == 0 {
            continue;
        }
        let ratio_allowance = attempts
            .saturating_mul(policy.max_session_churn_basis_points)
            .saturating_add(9_999)
            / 10_000;
        let allowed = policy.session_churn_grace_frames.max(ratio_allowance);
        let observed = profile.session_churn_events();
        if observed > allowed {
            push_decode_root_cause_with_action(
                root_causes,
                actions,
                PreviewDecodePerformanceArea::AccessMode,
                "preview_decode_session_churn",
                format!(
                    "access_mode={} attempts={} successful_frames={} session_opened_frames={} session_replaced_frames={} canceled_session_open_attempts={} canceled_session_opened_attempts={} canceled_session_replaced_attempts={} canceled_session_open_max_duration_us={} allowed_churn_frames={} max_session_churn_basis_points={}",
                    access_mode.as_str(),
                    attempts,
                    profile.frames,
                    profile.session_opened_frames,
                    profile.session_replaced_frames,
                    profile.canceled_session_open_attempts,
                    profile.canceled_session_opened_attempts,
                    profile.canceled_session_replaced_attempts,
                    profile.canceled_session_open_max_duration_us,
                    allowed,
                    policy.max_session_churn_basis_points
                ),
                "preserve_preview_decode_session",
                "Preserve compatible decoder Sessions across requests; repeated bounded cold opens are still a locality failure.",
                PreviewDecodePerformanceSeverity::Fail,
            );
        }
    }

    let scrub_profile = summary.access_mode_profiles.scrub_cursor;
    if scrub_profile.frames > 0
        && scrub_profile.bounded_any_seek_strategy_frames < scrub_profile.frames
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::AccessMode,
            "preview_decode_scrub_cursor_not_using_low_latency_seek",
            format!(
                "scrub_frames={} bounded_any_seek_strategy_frames={} keyframe_seek_strategy_frames={}",
                scrub_profile.frames,
                scrub_profile.bounded_any_seek_strategy_frames,
                scrub_profile.keyframe_seek_strategy_frames
            ),
            "route_scrub_decode_through_bounded_any_seek",
            "Ensure ScrubCursor decode uses the low-latency bounded-any seek strategy instead of playback/still exact seek semantics.",
            PreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scrub_profile.frames > 0 && scrub_profile.any_seek_window_ms_max == 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::AccessMode,
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
            PreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scrub_profile.frames > 0
        && scrub_profile.work_classes.reused_seek.max_duration_us > summary.slow_frame_budget_us
        && scrub_profile.seeked_frames > 0
        && scrub_profile.seek_index_available_frames == 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::AccessMode,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }

    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        let steady = profile.work_classes.forward_steady;
        if steady.frames == 0 || steady.max_duration_us <= summary.slow_frame_budget_us {
            continue;
        }
        let p95_upper_bound_us = steady.latency_buckets.p95_upper_bound_us();
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::AccessMode,
            "preview_decode_access_mode_over_budget",
            format!(
                "access_mode={} steady_frames={} steady_max_duration_us={} steady_p95_upper_bound_us={:?} steady_total_duration_us={} queue_wait_max_us={} queue_wait_total_us={} max_frame_queue_wait_us={} max_frame_bottleneck={:?} seeked_frames={} keyframe_seek_strategy_frames={} bounded_any_seek_strategy_frames={} forward_reuse_frame_window_max={} forward_decode_budget_frames_max={} any_seek_window_ms_max={} session_reused_frames={} session_opened_frames={} forward_reused_frames={} seek_index_available_frames={} seek_index_used_frames={} seek_index_keyframes_max={} seek_index_observed_packets_max={} seek_index_probe_backed_frames={} seek_index_session_observed_frames={} hardware_decode_active_frames={} zero_copy_active_frames={} gpu_texture_resident_frames={} decoded_nv12_surface_frames={} decoded_p010_surface_frames={} hardware_decode_texture_residency_blocker_frames={} hardware_decode_auto_requested_frames={} hardware_decode_prefer_hardware_requested_frames={} hardware_decode_prefer_gpu_requested_frames={} hardware_decode_require_gpu_requested_frames={} hardware_decode_cpu_not_requested_frames={} hardware_decode_cpu_unavailable_frames={} hardware_decode_backend_unavailable_frames={} hardware_decode_codec_unsupported_frames={} hardware_decode_device_context_attempted_frames={} hardware_decode_device_context_created_frames={} hardware_decode_device_context_unavailable_frames={} hardware_decode_cpu_transfer_frames={} hardware_decode_cpu_transfer_configured_frames={} hardware_decode_cpu_transfer_observed_frames={} hardware_decode_cpu_transfer_setup_failed_frames={} hardware_decode_cpu_transfer_decoder_open_failed_frames={} hardware_decode_cpu_transfer_awaiting_frame_frames={} hardware_decode_backend_boundary_frames={} hardware_decode_gpu_resident_native_frames={} hardware_decode_candidate_d3d12va_frames={} hardware_decode_candidate_d3d11va_frames={} hardware_decode_candidate_dxva2_frames={} hardware_decode_candidate_videotoolbox_frames={} hardware_decode_candidate_vaapi_frames={} hardware_decode_candidate_vdpau_frames={} hardware_decode_candidate_cuda_frames={} hardware_decode_adapter_unavailable_frames={} decoded_frame_count={} max_decoded_frame_count={} slowest_aggregate_session_open_us={} slowest_aggregate_output_lease_wait_us={} slowest_aggregate_cache_lookup_us={} slowest_aggregate_seek_us={} slowest_aggregate_packet_decode_us={} slowest_aggregate_hardware_transfer_us={} slowest_aggregate_swscale_us={} slowest_aggregate_rgba_copy_us={} slowest_aggregate_external_process_us={} cache_hit_frames={} playback_session_ring_hit_frames={} steady_latency_buckets={:?}",
                access_mode.as_str(),
                steady.frames,
                steady.max_duration_us,
                p95_upper_bound_us,
                steady.total_duration_us,
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
                profile.max_frame_stage_durations.output_lease_wait_us,
                profile.max_frame_stage_durations.cache_lookup_us,
                profile.max_frame_stage_durations.seek_us,
                profile.max_frame_stage_durations.packet_decode_us,
                profile.max_frame_stage_durations.hardware_transfer_us,
                profile.max_frame_stage_durations.swscale_us,
                profile.max_frame_stage_durations.rgba_copy_us,
                profile.max_frame_stage_durations.external_process_us,
                profile.cache_hit_frames,
                profile.playback_session_ring_hit_frames,
                steady.latency_buckets
            ),
            "inspect_preview_decode_access_mode_profile",
            "Inspect the per-access-mode decode profile before changing global decode concurrency or color/render code.",
            PreviewDecodePerformanceSeverity::Fail,
        );
    }
    for (access_mode, profile) in summary.access_mode_profiles.named_profiles() {
        let session_open = profile.work_classes.session_opened;
        if session_open.frames == 0
            || session_open.max_duration_us <= PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US
        {
            continue;
        }
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::AccessMode,
            "preview_decode_access_mode_session_open_over_budget",
            format!(
                "access_mode={} session_open_frames={} session_open_max_duration_us={} session_open_total_duration_us={} session_open_budget_us={} session_open_latency_buckets={:?}",
                access_mode.as_str(),
                session_open.frames,
                session_open.max_duration_us,
                session_open.total_duration_us,
                PREVIEW_DECODE_DEFAULT_SESSION_OPEN_BUDGET_US,
                session_open.latency_buckets
            ),
            "inspect_preview_decode_access_mode_session_open",
            "Inspect access-mode-local decoder construction and session churn independently from steady frame cadence.",
            PreviewDecodePerformanceSeverity::Fail,
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
            PreviewDecodePerformanceArea::AccessMode,
            "preview_decode_access_mode_queue_wait_bound",
            format!(
                "access_mode={} queue_wait_max_us={} queue_wait_p95_upper_bound_us={:?} queue_wait_total_us={} queue_wait_last_us={} frames={} slow_frame_budget_us={} queue_wait_buckets={:?}",
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
            PreviewDecodePerformanceSeverity::Warn,
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
            PreviewDecodePerformanceArea::CodecDecode,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    let hardware_fallback_not_engaged =
        playback_profile.hardware_decode_fallback_not_engaged_frames();
    if hardware_fallback_not_engaged > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::CodecDecode,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    let playback_session_local_frames = playback_profile
        .session_reused_frames
        .saturating_add(playback_profile.session_bypassed_cache_frames);
    if playback_profile.frames > 1 && playback_session_local_frames == 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::AccessMode,
            "preview_decode_playback_session_not_reused",
            format!(
                "access_mode=PlaybackCursor frames={} session_opened_frames={} session_replaced_frames={} session_reused_frames=0 session_bypassed_cache_frames=0 max_duration_us={} session_open_us={}",
                playback_profile.frames,
                playback_profile.session_opened_frames,
                playback_profile.session_replaced_frames,
                playback_profile.max_duration_us,
                playback_profile.max_frame_stage_durations.session_open_us
            ),
            "preserve_playback_decode_session",
            "Keep the playback cursor decode session stable across adjacent playback frames before increasing worker count.",
            PreviewDecodePerformanceSeverity::Warn,
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
            PreviewDecodePerformanceArea::AccessMode,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.worker_queue.queued_expired_playback_current_jobs > 0
        || summary.worker_queue.dropped_expired_playback_current_jobs > 0
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_current_stalled_expirations > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_schedule.sustained_pressure_events > 0
        || summary.playback_schedule.sustained_pressure_active
    {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_schedule.current_native_import_unavailable_decisions > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.playback_schedule.current_hardware_fallback_not_engaged_decisions > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }

    if summary.hardware_decode_admission.playback_native_import_gated() {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_hardware_decode_admission_gated",
            format!(
                "playback_hardware_decode_request={:?} admission_blocker={:?} renderer_native_import_support_known={} renderer_native_import_ready={} renderer_import_mode={:?} renderer_supported_handle_kinds={} renderer_supported_source_texture_formats={} native_import_admission_ready={} playback_frames={} current_decode_decisions={} current_drop_late_decisions={}",
                summary.hardware_decode_admission.playback_request,
                summary.hardware_decode_admission.admission_blocker,
                summary
                    .hardware_decode_admission
                    .renderer_native_import_support_known,
                summary.hardware_decode_admission.renderer_native_import_ready,
                summary.hardware_decode_admission.renderer_import_mode,
                summary
                    .hardware_decode_admission
                    .renderer_supported_handle_kinds,
                summary
                    .hardware_decode_admission
                    .renderer_supported_source_texture_formats,
                summary.hardware_decode_admission.native_import_admission_ready,
                summary.playback_cursor_frames,
                summary.playback_schedule.current_decode_decisions,
                summary.playback_schedule.current_drop_late_decisions
            ),
            "connect_native_import_before_enabling_hardware_decode_admission",
            "Keep GPU-resident playback decode disabled until the exact renderer-device native import Adapter is ready; otherwise retain the hardware CPU-transfer or software fallback.",
            PreviewDecodePerformanceSeverity::Warn,
        );
    }

    match summary.primary_bottleneck {
        PreviewDecodeBottleneck::QueueWait => push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        ),
        PreviewDecodeBottleneck::PacketDecode => push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_codec_or_gop_bound",
            format!(
                "packet_decode_us={} decoded_frame_count={} max_decoded_frame_count={}",
                summary.max_frame_stage_durations.packet_decode_us,
                summary.decoded_frame_count,
                summary.max_decoded_frame_count
            ),
            "enable_proxy_or_hardware_decode",
            "Prefer fresh proxy playback or implement hardware decode residency for this source.",
            PreviewDecodePerformanceSeverity::Warn,
        ),
        PreviewDecodeBottleneck::HardwareTransfer => push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::CodecDecode,
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
            PreviewDecodePerformanceSeverity::Warn,
        ),
        PreviewDecodeBottleneck::Seek => push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::RandomAccess,
            "preview_decode_seek_bound",
            format!(
                "seek_us={} seeked_frames={}",
                summary.max_frame_stage_durations.seek_us, summary.seeked_frames
            ),
            "generate_proxy_or_improve_random_access",
            "Generate playback proxies or improve random-access/indexing strategy for this media.",
            PreviewDecodePerformanceSeverity::Warn,
        ),
        PreviewDecodeBottleneck::CpuRgbaBoundary => push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::CpuRgbaBoundary,
            "preview_decode_cpu_rgba_boundary_bound",
            format!(
                "swscale_us={} rgba_copy_us={}",
                summary.max_frame_stage_durations.swscale_us,
                summary.max_frame_stage_durations.rgba_copy_us
            ),
            "remove_cpu_rgba_decode_boundary",
            "Move toward high-bit-depth or GPU-resident decode frames instead of CPU RGBA8 preview payloads.",
            PreviewDecodePerformanceSeverity::Warn,
        ),
        PreviewDecodeBottleneck::ExternalProcess => push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::ExternalProcess,
            "preview_decode_external_process_bound",
            format!(
                "external_process_us={}",
                summary.max_frame_stage_durations.external_process_us
            ),
            "avoid_external_ffmpeg_preview_path",
            "Use in-process decode or a real hardware-resident adapter instead of rawvideo over stdout.",
            PreviewDecodePerformanceSeverity::Warn,
        ),
        PreviewDecodeBottleneck::SessionOpen => push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::CodecDecode,
            "preview_decode_session_open_bound",
            format!(
                "session_open_us={}",
                summary.max_frame_stage_durations.session_open_us
            ),
            "preserve_decode_session_locality",
            "Keep decode sessions alive across adjacent playback requests and avoid path/size churn.",
            PreviewDecodePerformanceSeverity::Warn,
        ),
        PreviewDecodeBottleneck::OutputLease => push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_output_lease_bound",
            format!(
                "output_lease_wait_us={}",
                summary.max_frame_stage_durations.output_lease_wait_us
            ),
            "retire_native_output_leases",
            "Pump Preview completion and renderer copy-fence retirement so bounded decoder slots can be reused without entering the codec while their prior native outputs remain owned.",
            PreviewDecodePerformanceSeverity::Warn,
        ),
        PreviewDecodeBottleneck::CacheLookup | PreviewDecodeBottleneck::None => {}
    }

    if summary.canceled_prefetch_deadline_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.playback_schedule.current_deadline_missing_frame_rate > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.playback_schedule.forward_prefetch_invalid_frame_rate > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_prefetch_window_invalid_frame_rate",
            format!(
                "forward_prefetch_invalid_frame_rate={} forward_prefetch_window_evaluations={} last_forward_prefetch_window_frames={:?} forward_prefetch_horizon_us={} forward_prefetch_min_frames={} forward_prefetch_max_frames={} steady_prefetch_reservation_limit={}",
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
                summary.playback_schedule.forward_prefetch_max_frames,
                summary
                    .playback_schedule
                    .steady_prefetch_reservation_limit
            ),
            "fix_sequence_prefetch_frame_rate_contract",
            "Ensure playback prefetch derives its window from a valid sequence frame rate instead of silently disabling cache warming.",
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_playback_deadline_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::AccessMode,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_prefetch_preempted_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.canceled_still_preempted_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.decode_timeout_failures > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::LatencyBudget,
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
            PreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.decode_budget_exhausted_failures > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::RandomAccess,
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
            PreviewDecodePerformanceSeverity::Fail,
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
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_cancellation_gate_failed",
            format!(
                "failures={:?} playback={:?} interactive={:?} still={:?}",
                cancellation_gate.failures,
                summary.cancellation.playback,
                summary.cancellation.interactive,
                summary.cancellation.still
            ),
            "inspect_preview_decode_cancellation_points",
            "Inspect logical cancellation authority timing separately from concrete FFmpeg open/seek/decode/copy checkpoints; realtime work must observe and return within the playback-owned policy.",
            PreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.canceled_obsolete_jobs > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            "Coalesce preview requests before decode when consumer intent changes faster than workers can consume jobs.",
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    if summary.queue_full_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
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
            PreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.queue_invalid_access_mode_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_queue_invalid_access_mode_drop",
            format!(
                "queue_invalid_access_mode_drops={} enqueued_jobs={} scheduler_dropped_invalid_access_mode_requests={}",
                summary.queue_invalid_access_mode_drops,
                summary.enqueued_jobs,
                summary.scheduler.dropped_invalid_access_mode_requests
            ),
            "fix_preview_access_mode_admission",
            "Ensure invalid priority/access-mode pairs are rejected before worker-queue transport.",
            PreviewDecodePerformanceSeverity::Fail,
        );
    }
    if summary.worker_disconnected_drops > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_worker_disconnected_drops",
            format!(
                "worker_disconnected_drops={} enqueued_jobs={} queue_full_drops={}",
                summary.worker_disconnected_drops, summary.enqueued_jobs, summary.queue_full_drops
            ),
            "restore_preview_worker_lifecycle",
            "Ensure preview workers are running before accepting media preview jobs and close the queue only during service shutdown.",
            PreviewDecodePerformanceSeverity::Fail,
        );
    }

    let scheduler = summary.scheduler;
    if scheduler.dropped_invalid_access_mode_requests > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_invalid_access_mode_request",
            format!(
                "dropped_invalid_access_mode_requests={} scheduled_requests={} pending_requests={}",
                scheduler.dropped_invalid_access_mode_requests,
                scheduler.scheduled_requests,
                scheduler.pending_requests
            ),
            "fix_preview_access_mode_admission",
            "Route speculative media work through PlaybackCursor prefetch only; scrub and still-frame requests must be current-frame work.",
            PreviewDecodePerformanceSeverity::Fail,
        );
    }
    if scheduler.clock_regressions > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_broker_clock_regression",
            format!("clock_regressions={}", scheduler.clock_regressions),
            "inspect_preview_monotonic_clock_adapter",
            "Inspect the Monotonic Runtime Clock Adapter; the Frame Work Broker clamps regressions but cannot accept their timing evidence.",
            PreviewDecodePerformanceSeverity::Fail,
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
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_access_mode_mismatch",
            format!(
                "skipped_decode_access_mode_mismatch={} completed_cache_only_access_mode_mismatch={} completed_stale_access_mode_mismatch={}",
                scheduler.skipped_decode_access_mode_mismatch,
                scheduler.completed_cache_only_access_mode_mismatch,
                scheduler.completed_stale_access_mode_mismatch
            ),
            "inspect_preview_access_mode_transitions",
            "Inspect playback/scrub/still request transitions and ensure older jobs cannot complete newer access-mode work.",
            PreviewDecodePerformanceSeverity::Warn,
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
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_obsolete_generation_churn",
            format!(
                "skipped_decode_obsolete_generation={} completed_stale_obsolete_generation={} pruned_obsolete_requests={}",
                scheduler.skipped_decode_obsolete_generation,
                scheduler.completed_stale_obsolete_generation,
                scheduler.pruned_obsolete_requests
            ),
            "reduce_preview_generation_churn",
            "Reduce duplicate preview requests per foreground tick or coalesce obsolete generations before they reach workers.",
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
    if scheduler.dropped_pending_window_requests > 0 {
        push_decode_root_cause_with_action(
            root_causes,
            actions,
            PreviewDecodePerformanceArea::Scheduling,
            "preview_decode_pending_window_backpressure",
            format!(
                "dropped_pending_window_requests={} pending_requests={}",
                scheduler.dropped_pending_window_requests, scheduler.pending_requests
            ),
            "bound_preview_pending_window_by_access_mode",
            "Inspect current/prefetch admission policy and keep visible current-frame work latest-wins.",
            PreviewDecodePerformanceSeverity::Warn,
        );
    }
}

fn push_preview_render_root_causes_and_actions(
    summary: PreviewRenderPerformanceSummary,
    root_causes: &mut Vec<PreviewRenderPerformanceRootCause>,
    actions: &mut Vec<PreviewRenderPerformanceAction>,
) {
    if summary.unavailability.blocked > 0 {
        push_render_root_cause_with_action(
            root_causes,
            actions,
            PreviewRenderPerformanceArea::CaptureIntegrity,
            "preview_render_output_blocked",
            format!(
                "blocked={} stages={:?} last_stage={:?}",
                summary.unavailability.blocked,
                summary.unavailability.stages,
                summary.unavailability.last_stage
            ),
            "inspect_preview_unavailability",
            "Resolve the typed Preview correctness or dependency blocker before measuring performance.",
        );
    }
    if summary.unavailability.failed > 0 {
        push_render_root_cause_with_action(
            root_causes,
            actions,
            PreviewRenderPerformanceArea::CaptureIntegrity,
            "preview_render_execution_failed",
            format!(
                "failed={} stages={:?} last_stage={:?}",
                summary.unavailability.failed,
                summary.unavailability.stages,
                summary.unavailability.last_stage
            ),
            "inspect_preview_unavailability",
            "Inspect the typed Preview execution stage and detailed terminal reason before tuning latency.",
        );
    }
    if summary.max_duration_us <= summary.slow_frame_budget_us {
        return;
    }

    push_render_root_cause_with_action(
        root_causes,
        actions,
        PreviewRenderPerformanceArea::LatencyBudget,
        "preview_render_frame_over_budget",
        format!(
            "max_duration_us={} slow_frame_budget_us={} primary_bottleneck={:?}",
            summary.max_duration_us, summary.slow_frame_budget_us, summary.primary_bottleneck
        ),
        "inspect_preview_render_stage_durations",
        "Inspect post-decode viewer render stage timings before changing decode code.",
    );

    match summary.primary_bottleneck {
        PreviewRenderBottleneck::Resolve => push_render_root_cause_with_action(
            root_causes,
            actions,
            PreviewRenderPerformanceArea::Resolve,
            "preview_render_resolve_bound",
            format!("resolve_us={}", summary.max_frame_stage_durations.resolve_us),
            "profile_preview_plan_resolution",
            "Profile sequence resolution, media-key construction, and readiness checks.",
        ),
        PreviewRenderBottleneck::FinalCacheLookup => push_render_root_cause_with_action(
            root_causes,
            actions,
            PreviewRenderPerformanceArea::FinalCacheLookup,
            "preview_render_final_cache_lookup_bound",
            format!(
                "final_cache_lookup_us={}",
                summary.max_frame_stage_durations.final_cache_lookup_us
            ),
            "profile_viewer_frame_cache",
            "Profile final viewer frame cache lookup and external texture identity checks.",
        ),
        PreviewRenderBottleneck::WorkingPreparation => push_render_root_cause_with_action(
            root_causes,
            actions,
            PreviewRenderPerformanceArea::WorkingPreparation,
            "preview_render_working_prepare_bound",
            format!(
                "working_prepare_us={}",
                summary.max_frame_stage_durations.working_prepare_us
            ),
            "reduce_working_frame_preparation",
            "Reduce working-frame extraction/copy work before timeline compositing.",
        ),
        PreviewRenderBottleneck::CpuComposite => push_render_root_cause_with_action(
            root_causes,
            actions,
            PreviewRenderPerformanceArea::CpuComposite,
            "preview_render_cpu_composite_bound",
            format!(
                "cpu_composite_us={}",
                summary.max_frame_stage_durations.cpu_composite_us
            ),
            "move_preview_composite_to_gpu",
            "Keep common blend, transform, and effect paths on GPU or improve CPU composite tiling.",
        ),
        PreviewRenderBottleneck::CpuOutputBoundary => push_render_root_cause_with_action(
            root_causes,
            actions,
            PreviewRenderPerformanceArea::CpuOutputBoundary,
            "preview_render_cpu_output_boundary_bound",
            format!(
                "cpu_output_boundary_us={}",
                summary.max_frame_stage_durations.cpu_output_boundary_us
            ),
            "profile_cpu_output_and_validate_gpu_route",
            "Profile the CPU color processor, allocation, and memory bandwidth while preserving required CPU working-frame cache publication. Timing alone does not establish GPU output eligibility: check the typed route, color/alpha contract, readback requirements, and device admission before selecting an existing GPU or hybrid route.",
        ),
        PreviewRenderBottleneck::FramePackaging => push_render_root_cause_with_action(
            root_causes,
            actions,
            PreviewRenderPerformanceArea::FramePackaging,
            "preview_render_frame_packaging_bound",
            format!(
                "frame_packaging_us={}",
                summary.max_frame_stage_durations.frame_packaging_us
            ),
            "avoid_raster_frame_packaging",
            "Prefer GPU-resident viewer frames or reduce final raster hashing/copying.",
        ),
        PreviewRenderBottleneck::None => {}
    }
}

fn push_render_root_cause_with_action(
    root_causes: &mut Vec<PreviewRenderPerformanceRootCause>,
    actions: &mut Vec<PreviewRenderPerformanceAction>,
    area: PreviewRenderPerformanceArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(PreviewRenderPerformanceRootCause {
            area,
            code: root_code,
            severity: PreviewRenderPerformanceSeverity::Fail,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(PreviewRenderPerformanceAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

fn push_decode_root_cause_with_action(
    root_causes: &mut Vec<PreviewDecodePerformanceRootCause>,
    actions: &mut Vec<PreviewDecodePerformanceAction>,
    area: PreviewDecodePerformanceArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
    severity: PreviewDecodePerformanceSeverity,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(PreviewDecodePerformanceRootCause {
            area,
            code: root_code,
            severity,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(PreviewDecodePerformanceAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

pub(in super::super) fn classify_preview_decode_bottleneck(
    durations: PreviewDecodeStageDurations,
    queue_wait_us: u64,
) -> PreviewDecodeBottleneck {
    let candidates = [
        (PreviewDecodeBottleneck::QueueWait, queue_wait_us),
        (
            PreviewDecodeBottleneck::SessionOpen,
            durations.session_open_us,
        ),
        (
            PreviewDecodeBottleneck::OutputLease,
            durations.output_lease_wait_us,
        ),
        (
            PreviewDecodeBottleneck::CacheLookup,
            durations.cache_lookup_us,
        ),
        (PreviewDecodeBottleneck::Seek, durations.seek_us),
        (
            PreviewDecodeBottleneck::PacketDecode,
            durations.packet_decode_us,
        ),
        (
            PreviewDecodeBottleneck::HardwareTransfer,
            durations.hardware_transfer_us,
        ),
        (
            PreviewDecodeBottleneck::CpuRgbaBoundary,
            durations.swscale_us.saturating_add(durations.rgba_copy_us),
        ),
        (
            PreviewDecodeBottleneck::ExternalProcess,
            durations.external_process_us,
        ),
    ];

    candidates
        .into_iter()
        .max_by_key(|(_, duration)| *duration)
        .filter(|(_, duration)| *duration > 0)
        .map(|(bottleneck, _)| bottleneck)
        .unwrap_or(PreviewDecodeBottleneck::None)
}

pub(super) fn classify_preview_render_bottleneck(
    durations: PreviewRenderStageDurations,
) -> PreviewRenderBottleneck {
    let candidates = [
        (PreviewRenderBottleneck::Resolve, durations.resolve_us),
        (
            PreviewRenderBottleneck::FinalCacheLookup,
            durations.final_cache_lookup_us,
        ),
        (
            PreviewRenderBottleneck::WorkingPreparation,
            durations.working_prepare_us,
        ),
        (
            PreviewRenderBottleneck::CpuComposite,
            durations.cpu_composite_us,
        ),
        (
            PreviewRenderBottleneck::CpuOutputBoundary,
            durations.cpu_output_boundary_us,
        ),
        (
            PreviewRenderBottleneck::FramePackaging,
            durations.frame_packaging_us,
        ),
    ];

    candidates
        .into_iter()
        .max_by_key(|(_, duration)| *duration)
        .filter(|(_, duration)| *duration > 0)
        .map(|(bottleneck, _)| bottleneck)
        .unwrap_or(PreviewRenderBottleneck::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lease_wait_is_reported_as_the_decode_bottleneck() {
        let durations = PreviewDecodeStageDurations {
            session_open_us: 10,
            output_lease_wait_us: 20,
            packet_decode_us: 15,
            ..PreviewDecodeStageDurations::default()
        };

        assert_eq!(
            classify_preview_decode_bottleneck(durations, 5),
            PreviewDecodeBottleneck::OutputLease
        );
    }
}
