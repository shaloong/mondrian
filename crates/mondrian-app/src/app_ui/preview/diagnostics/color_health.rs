//! Versioned Preview color-health report model and deterministic diagnosis.

use super::*;

/// Schema version for preview color health reports.
pub const APP_UI_PREVIEW_COLOR_HEALTH_REPORT_SCHEMA_VERSION: u32 = 2;

/// Versioned preview color health report for UI, telemetry, and perf artifacts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall preview color health verdict.
    pub verdict: AppUiPreviewColorHealthVerdict,
    /// Structured color health summary used as report evidence.
    pub summary: Option<AppUiPreviewColorHealthSummary>,
    /// Structured checks by preview color-pipeline area.
    pub checks: Vec<AppUiPreviewColorHealthCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<AppUiPreviewColorHealthRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<AppUiPreviewColorHealthAction>,
}

/// Overall preview color health verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewColorHealthVerdict {
    /// Preview color path met all fail-closed checks.
    Pass,
    /// Preview color path passed hard checks but has warning evidence.
    Warn,
    /// Preview color path violated a fail-closed check.
    Fail,
}

/// Preview color diagnostic area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewColorHealthArea {
    /// Evidence capture and summary availability.
    CaptureIntegrity,
    /// Input media metadata and policy handling.
    InputColorPolicy,
    /// Renderer color-stage scheduling.
    StageScheduling,
    /// Timeline compositing precision and legacy paths.
    CompositePath,
    /// Explicitly unsupported features (OS ICC, HDR/EDR, GPU compositing).
    UnsupportedFeature,
}

/// Preview color health check severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppUiPreviewColorHealthSeverity {
    /// Check passed.
    Pass,
    /// Check produced warning evidence.
    Warn,
    /// Check failed.
    Fail,
}

/// One preview color health check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthCheck {
    /// Diagnostic area for this check.
    pub area: AppUiPreviewColorHealthArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: AppUiPreviewColorHealthSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One preview color health root cause.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthRootCause {
    /// Diagnostic area for this root cause.
    pub area: AppUiPreviewColorHealthArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: AppUiPreviewColorHealthSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One preview color health action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewColorHealthAction {
    /// Diagnostic area for this action.
    pub area: AppUiPreviewColorHealthArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

impl AppUiPreviewColorHealthSummary {
    /// Build the versioned preview color health report for this summary.
    pub fn health_report(self, profile: impl Into<String>) -> AppUiPreviewColorHealthReport {
        build_preview_color_health_report(Some(self), profile)
    }
}

/// Build a versioned preview color health report from an optional summary.
pub fn build_preview_color_health_report(
    summary: Option<AppUiPreviewColorHealthSummary>,
    profile: impl Into<String>,
) -> AppUiPreviewColorHealthReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();

    push_preview_bool_check(
        &mut checks,
        AppUiPreviewColorHealthArea::CaptureIntegrity,
        "color_health_present",
        summary.is_some(),
    );

    if let Some(summary) = summary {
        push_preview_bool_check(
            &mut checks,
            AppUiPreviewColorHealthArea::CompositePath,
            color_report_vocab::check::FULLY_FLOAT_LINEAR,
            summary.fully_float_linear,
        );
        push_preview_bool_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::GPU_PATH_READY,
            summary.gpu_path_ready,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::GPU_BLOCKERS,
            summary.gpu_blockers,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::TRANSFER_STAGES,
            summary.transfer_stages,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::CompositePath,
            color_report_vocab::check::LEGACY_REASON_TOTAL,
            summary.legacy_reason_total,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::CompositePath,
            color_report_vocab::check::EFFECT_DOMAIN_BLOCKERS,
            summary.domain_blockers.total().max(summary.blocked_color_domain_composites),
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::InputColorPolicy,
            color_report_vocab::check::POLICY_REJECTIONS,
            summary.policy_rejections,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::CPU_OUTPUT_FALLBACK_FRAMES,
            summary.cpu_output_fallback_frames,
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::StageScheduling,
            color_report_vocab::check::GPU_OUTPUT_BLOCKERS,
            summary.preview_gpu_output_blocker_breakdown.total(),
            0,
        );
        push_preview_max_check(
            &mut checks,
            AppUiPreviewColorHealthArea::UnsupportedFeature,
            "unsupported_feature_count",
            summary.preview_gpu_output_blocker_breakdown.unsupported_features,
            0,
        );
        push_preview_root_causes_and_actions(summary, &mut root_causes, &mut actions);
    } else {
        push_preview_root_cause_with_action(
            &mut root_causes,
            &mut actions,
            AppUiPreviewColorHealthArea::CaptureIntegrity,
            "missing_preview_color_evidence",
            "color_health_present=false".to_owned(),
            "inspect_preview_diagnostics",
            "Ensure preview diagnostics record color summaries from the real preview path.",
        );
    }

    let has_failures = checks
        .iter()
        .any(|check| check.severity == AppUiPreviewColorHealthSeverity::Fail);
    let has_warnings = checks
        .iter()
        .any(|check| check.severity == AppUiPreviewColorHealthSeverity::Warn);
    let verdict = if has_failures {
        AppUiPreviewColorHealthVerdict::Fail
    } else if has_warnings {
        AppUiPreviewColorHealthVerdict::Warn
    } else {
        AppUiPreviewColorHealthVerdict::Pass
    };

    AppUiPreviewColorHealthReport {
        schema_version: APP_UI_PREVIEW_COLOR_HEALTH_REPORT_SCHEMA_VERSION,
        profile: profile.into(),
        verdict,
        summary,
        checks,
        root_causes,
        actions,
    }
}

fn push_preview_max_check(
    checks: &mut Vec<AppUiPreviewColorHealthCheck>,
    area: AppUiPreviewColorHealthArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(AppUiPreviewColorHealthCheck {
        area,
        code,
        severity: if observed > limit {
            AppUiPreviewColorHealthSeverity::Fail
        } else {
            AppUiPreviewColorHealthSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_preview_bool_check(
    checks: &mut Vec<AppUiPreviewColorHealthCheck>,
    area: AppUiPreviewColorHealthArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(AppUiPreviewColorHealthCheck {
        area,
        code,
        severity: if passed {
            AppUiPreviewColorHealthSeverity::Pass
        } else {
            AppUiPreviewColorHealthSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_preview_root_causes_and_actions(
    summary: AppUiPreviewColorHealthSummary,
    root_causes: &mut Vec<AppUiPreviewColorHealthRootCause>,
    actions: &mut Vec<AppUiPreviewColorHealthAction>,
) {
    if summary.policy_rejections > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::InputColorPolicy,
            color_report_vocab::root_cause::INPUT_COLOR_POLICY_REJECTED_SOURCE,
            format!("policy_rejections={}", summary.policy_rejections),
            color_report_vocab::action::INSPECT_ASSET_COLOR_DIAGNOSTICS,
            "Inspect active-sequence media color diagnostics and missing-metadata policy.",
        );
    }
    if summary.gpu_blockers > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::StageScheduling,
            "preview_gpu_color_stage_blocked",
            format!(
                "gpu_blockers={} shader={} ocio={} wrapper={} pipeline={}",
                summary.gpu_blockers,
                summary.gpu_blocker_breakdown.shader_module_not_prepared,
                summary
                    .gpu_blocker_breakdown
                    .ocio_resource_bind_group_not_prepared,
                summary.gpu_blocker_breakdown.fullscreen_wrapper_not_prepared,
                summary.gpu_blocker_breakdown.render_pipeline_not_prepared
            ),
            color_report_vocab::action::INSPECT_GPU_BLOCKERS,
            "Inspect renderer GPU color blocker breakdown before relying on preview GPU scheduling.",
        );
    }
    if summary.transfer_stages > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::StageScheduling,
            "preview_transfer_stage_present",
            format!("transfer_stages={}", summary.transfer_stages),
            color_report_vocab::action::REMOVE_TRANSFER_STAGE,
            "Trace why preview color work introduced upload/readback transfer stages.",
        );
    }
    if summary.legacy_rgba8_composites > 0 || summary.legacy_reason_total > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::CompositePath,
            color_report_vocab::root_cause::LEGACY_RGBA8_COMPOSITE_PATH,
            format!(
                "fully_float_linear={} legacy_reason_total={}",
                summary.fully_float_linear, summary.legacy_reason_total
            ),
            color_report_vocab::action::MIGRATE_LEGACY_COMPOSITE_REASON,
            "Use structured legacy RGBA8 reasons to migrate preview composites back to float/linear.",
        );
    }
    if summary.blocked_color_domain_composites > 0 || !summary.domain_blockers.is_empty() {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::CompositePath,
            color_report_vocab::root_cause::EFFECT_DOMAIN_UNRESOLVED,
            format!(
                "blocked_composites={} media={} solid={} adjustment={}",
                summary.blocked_color_domain_composites,
                summary.domain_blockers.media_effect,
                summary.domain_blockers.solid_effect,
                summary.domain_blockers.adjustment_effect
            ),
            color_report_vocab::action::RESOLVE_EFFECT_DOMAIN_TRANSITIONS,
            "Resolve every effect-domain edge through the renderer OCIO planner; never run it as scene-linear or RGBA8.",
        );
    }
    if summary.gpu_compositing.cpu_fallback_composites > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::CompositePath,
            "preview_gpu_compositing_cpu_fallback",
            format!(
                "cpu_fallback_composites={} cpu_composited_pixels={} first_blocker={:?}",
                summary.gpu_compositing.cpu_fallback_composites,
                summary.gpu_compositing.cpu_composited_pixels,
                summary.gpu_compositing.first_blocker
            ),
            "resolve_gpu_compositing_blocker",
            "Inspect GPU compositing blocker and either lower the layer feature to GPU or keep the explicit CPU fallback.",
        );
    }
    if summary.cpu_output_fallback_frames > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::StageScheduling,
            "preview_cpu_output_fallback",
            format!(
                "cpu_output_fallback_frames={} cpu_output_fallback_pixels={}",
                summary.cpu_output_fallback_frames, summary.cpu_output_fallback_pixels
            ),
            "investigate_cpu_fallback",
            "Inspect why preview output fell back to CPU RGBA8 boundary instead of GPU color path.",
        );
    }
    let blocker_breakdown = summary.preview_gpu_output_blocker_breakdown;
    if !blocker_breakdown.is_empty() {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::StageScheduling,
            "preview_gpu_output_blocked",
            format!(
                "total_blockers={} ocio_config={} ocio_processor={} shader_extraction={} \
                 shader={} ocio_resource={} wrapper={} pipeline={} \
                 surface_contract={} display_color={} hdr={} \
                 frame_resident={} legacy_rgba8_boundary={} cpu_fallback={}",
                blocker_breakdown.total(),
                blocker_breakdown.ocio_config_not_loaded,
                blocker_breakdown.ocio_processor_unavailable,
                blocker_breakdown.ocio_gpu_shader_extraction_failed,
                blocker_breakdown.shader_module_not_prepared,
                blocker_breakdown.ocio_resource_bind_group_not_prepared,
                blocker_breakdown.fullscreen_wrapper_not_prepared,
                blocker_breakdown.render_pipeline_not_prepared,
                blocker_breakdown.surface_contract_mismatch,
                blocker_breakdown.unsupported_display_color_space,
                blocker_breakdown.unsupported_hdr_swapchain_or_edr,
                blocker_breakdown.frame_not_gpu_resident,
                blocker_breakdown.legacy_rgba8_composite_boundary,
                blocker_breakdown.cpu_fallback_requested
            ),
            "inspect_preview_gpu_output_blockers",
            "Inspect preview GPU output blocker breakdown to identify the primary blocker.",
        );
    }
    if blocker_breakdown.unsupported_features > 0 {
        push_preview_root_cause_with_action(
            root_causes,
            actions,
            AppUiPreviewColorHealthArea::UnsupportedFeature,
            "preview_unsupported_feature",
            format!(
                "unsupported_features={}",
                blocker_breakdown.unsupported_features
            ),
            "document_unsupported_feature",
            "Document the unsupported feature limitations and track for future implementation.",
        );
    }
}

fn push_preview_root_cause_with_action(
    root_causes: &mut Vec<AppUiPreviewColorHealthRootCause>,
    actions: &mut Vec<AppUiPreviewColorHealthAction>,
    area: AppUiPreviewColorHealthArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(AppUiPreviewColorHealthRootCause {
            area,
            code: root_code,
            severity: AppUiPreviewColorHealthSeverity::Fail,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(AppUiPreviewColorHealthAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}
