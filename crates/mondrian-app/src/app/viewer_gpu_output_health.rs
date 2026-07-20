//! Headless health evaluation for live viewer GPU-output diagnostics JSONL.

use anyhow::Context;
use mondrian_media::{VideoColorDiagnosticIssueAggregate, VideoColorDiagnosticIssueSummary};
use mondrian_renderer::{
    RenderColorStageDiagnostics, RenderGpuOutputRuntimeDiagnosticsReport,
    RenderGpuOutputStageDiagnosticsReport,
};
use serde::{Deserialize, Serialize};

/// Schema version for the high-level viewer GPU-output health report.
pub const VIEWER_GPU_OUTPUT_HEALTH_REPORT_SCHEMA_VERSION: u32 = 1;

/// Thresholds for evaluating a viewer GPU-output diagnostics JSONL stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerGpuOutputBudget {
    /// Minimum number of non-empty JSONL records.
    pub min_records: u64,
    /// Minimum number of ready viewer GPU-output attempts.
    pub min_ready: u64,
    /// Maximum allowed failed attempts.
    pub max_failed: u64,
    /// Maximum allowed display-contract-blocked attempts.
    pub max_blocked: u64,
    /// Maximum allowed external texture registration rejections.
    pub max_rejected: u64,
    /// Maximum allowed degraded attempts.
    pub max_degraded: u64,
    /// Maximum allowed waiting attempts.
    pub max_waiting: u64,
    /// Maximum allowed records with a display issue summary.
    pub max_display_issues: u64,
    /// Maximum allowed records blocked by missing HDR presentation support.
    pub max_hdr_output_requires_hdr_surface: u64,
    /// Maximum allowed records requiring a different surface color space.
    pub max_output_color_space_requires_surface_color_space: u64,
    /// Maximum allowed records blocked by the viewer/UI payload contract.
    pub max_reconfigure_blocked_by_payload: u64,
    /// Maximum allowed records targeting a non-presentation output intent.
    pub max_unsupported_presentation_intent: u64,
    /// Maximum allowed records lacking a supported surface contract.
    pub max_unsupported_surface_contract: u64,
    /// Maximum allowed records where OS display profile is unsupported.
    pub max_os_display_profile_unsupported: u64,
    /// Maximum allowed records carrying an unknown display issue reason.
    pub max_unknown_display_issues: u64,
    /// Maximum allowed display-contract refresh events.
    pub max_display_contract_refreshes: u64,
    /// Maximum allowed display issues correlated to a preceding refresh event.
    pub max_display_issue_refresh_correlations: u64,
    /// Maximum allowed refreshes that changed tone-map headroom evidence.
    pub max_display_tone_map_headroom_changes: u64,
    /// Maximum allowed refreshes that changed available surface formats.
    pub max_available_surface_format_changes: u64,
    /// Maximum allowed refreshes that changed per-format color-space capabilities.
    pub max_format_color_space_changes: u64,
    /// Maximum allowed refreshes that changed present modes.
    pub max_present_mode_changes: u64,
    /// Maximum allowed refreshes that changed alpha modes.
    pub max_alpha_mode_changes: u64,
    /// Maximum allowed records with a display presentation payload blocker.
    pub max_display_payload_blockers: u64,
    /// Maximum allowed records carrying a viewer color rejection.
    pub max_color_rejections: u64,
    /// Maximum allowed records missing renderer-owned runtime report snapshots.
    pub max_missing_runtime_reports: u64,
    /// Maximum allowed records missing renderer-owned stage report snapshots.
    pub max_missing_stage_reports: u64,
    /// Maximum allowed ready records without structured preview candidate context.
    pub max_ready_records_missing_preview_candidate_context: u64,
    /// Maximum allowed preview-candidate id regressions across stream.
    pub max_preview_candidate_id_regressions: u64,
}

impl Default for ViewerGpuOutputBudget {
    fn default() -> Self {
        Self {
            min_records: 1,
            min_ready: 1,
            max_failed: 0,
            max_blocked: 0,
            max_rejected: 0,
            max_degraded: 0,
            max_waiting: u64::MAX,
            max_display_issues: 0,
            max_hdr_output_requires_hdr_surface: 0,
            max_output_color_space_requires_surface_color_space: 0,
            max_reconfigure_blocked_by_payload: 0,
            max_unsupported_presentation_intent: 0,
            max_unsupported_surface_contract: 0,
            max_os_display_profile_unsupported: 0,
            max_unknown_display_issues: 0,
            max_display_contract_refreshes: u64::MAX,
            max_display_issue_refresh_correlations: 0,
            max_display_tone_map_headroom_changes: 0,
            max_available_surface_format_changes: 0,
            max_format_color_space_changes: 0,
            max_present_mode_changes: 0,
            max_alpha_mode_changes: 0,
            max_display_payload_blockers: 0,
            max_color_rejections: 0,
            max_missing_runtime_reports: 0,
            max_missing_stage_reports: 0,
            max_ready_records_missing_preview_candidate_context: 0,
            max_preview_candidate_id_regressions: 0,
        }
    }
}

impl ViewerGpuOutputBudget {
    /// Fail-closed viewer display baseline budget with limited startup refresh headroom.
    pub fn display_baseline() -> Self {
        Self {
            max_display_contract_refreshes: 2,
            ..Self::default()
        }
    }
}

/// Structured result of evaluating a viewer GPU-output diagnostics JSONL stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputBudgetSummary {
    /// Number of non-empty JSONL records consumed.
    pub records: u64,
    /// Counts replayed from each record's health status.
    pub counts: ViewerGpuOutputHealthCounts,
    /// Counts replayed from structured display issue summaries.
    pub display_issues: ViewerGpuOutputDisplayIssueCounts,
    /// Counts replayed from structured display-contract refresh events.
    pub display_contract_refreshes: ViewerGpuOutputDisplayContractRefreshCounts,
    /// Counts replayed from display issues correlated to a preceding refresh event.
    pub display_issue_refresh_correlations: ViewerGpuOutputDisplayIssueRefreshCorrelationCounts,
    /// Latest cumulative color-stage/runtime counters observed in the stream.
    pub stage: ViewerGpuOutputStageCounts,
    /// Last renderer GPU-output runtime snapshot observed in the stream.
    pub last_runtime_report: Option<RenderGpuOutputRuntimeDiagnosticsReport>,
    /// Records carrying a structured viewer color rejection.
    pub color_rejections: u64,
    /// Aggregated machine-readable media issue summaries from color rejections.
    pub media_issues: VideoColorDiagnosticIssueAggregate,
    /// Last cumulative counts reported by the JSONL stream, when present.
    pub reported_counts: Option<ViewerGpuOutputHealthCounts>,
    /// Whether reported cumulative counts match status replay.
    pub reported_counts_match_replay: bool,
    /// Line-level mismatches between reported and replayed counts.
    pub count_mismatches: Vec<ViewerGpuOutputCountMismatch>,
    /// Budget thresholds used for evaluation.
    pub budget: ViewerGpuOutputBudgetReport,
    /// Whether the stream satisfied the budget and consistency checks.
    pub passed: bool,
    /// Structured budget failures.
    pub failures: Vec<ViewerGpuOutputBudgetFailure>,
    /// Last health status observed in the stream.
    pub last_status: Option<ViewerGpuOutputHealthStatus>,
    /// Last full health summary observed in the stream.
    pub last_health: Option<ViewerGpuOutputHealthSummary>,
    /// Last frame context observed in the stream.
    pub last_frame_context: Option<ViewerGpuOutputFrameContext>,
    /// Last display issue summary observed in the stream.
    pub last_display_issue: Option<ViewerGpuOutputDisplayIssueSummary>,
    /// Last display-contract refresh event observed in the stream.
    pub last_display_contract_refresh: Option<ViewerGpuOutputDisplayContractRefreshEvent>,
    /// Last viewer color rejection observed in the stream.
    pub last_color_rejection: Option<ViewerGpuOutputColorRejectionSummary>,
    /// Number of records carrying a structured runtime report.
    pub runtime_reports: u64,
    /// Number of records carrying a structured stage report.
    pub stage_reports: u64,
    /// Number of records missing structured runtime report.
    pub missing_runtime_reports: u64,
    /// Number of records missing structured stage report.
    pub missing_stage_reports: u64,
    /// Number of ready records missing structured preview candidate context.
    pub ready_records_missing_preview_candidate_context: u64,
    /// Number of preview-candidate id regressions across stream.
    pub preview_candidate_id_regressions: u64,
    /// Last preview-candidate id observed in a ready attempt.
    pub last_preview_candidate_id: Option<u64>,
}

/// High-level, stable health report for CI, perf walls, and human diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputHealthReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied reporting profile or preset.
    pub profile: String,
    /// Source diagnostics stream path, when the report came from a persisted JSONL file.
    pub source_path: Option<String>,
    /// Overall diagnostic verdict.
    pub verdict: ViewerGpuOutputHealthVerdict,
    /// Budget/evaluator summary used as the evidence base.
    pub summary: ViewerGpuOutputBudgetSummary,
    /// Layered checks grouped by industrial color-pipeline responsibility.
    pub checks: Vec<ViewerGpuOutputHealthCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<ViewerGpuOutputHealthRootCause>,
    /// Suggested next diagnostic or engineering actions.
    pub actions: Vec<ViewerGpuOutputHealthAction>,
    /// Compact evidence pointers for dashboards and issue templates.
    pub evidence: ViewerGpuOutputHealthEvidence,
}

/// Overall verdict for a viewer GPU-output health report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ViewerGpuOutputHealthVerdict {
    /// The stream satisfied the budget and produced no warning-level diagnostic checks.
    Pass,
    /// The stream satisfied the budget, but one or more diagnostic checks need attention.
    Warn,
    /// The stream violated the budget or a fail-closed diagnostic check.
    Fail,
}

/// Diagnostic area used by high-level viewer GPU-output checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ViewerGpuOutputDiagnosticArea {
    /// JSONL capture completeness and replay consistency.
    CaptureIntegrity,
    /// Viewer output state machine health.
    ViewerOutput,
    /// Renderer color-stage and native GPU path health.
    GpuColorPath,
    /// Renderer GPU backend/runtime preparation health.
    BackendRuntime,
    /// Display boundary and presentation contract health.
    DisplayContract,
    /// Monitor/surface capability drift across refresh events.
    DisplayCapabilityDrift,
    /// Media metadata and color-policy rejections.
    MediaColorPolicy,
}

/// Severity for an individual health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ViewerGpuOutputHealthSeverity {
    /// The check passed.
    Pass,
    /// The check is not a budget failure, but should be tracked.
    Warn,
    /// The check is a blocking diagnostic failure.
    Fail,
}

/// One layered diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputHealthCheck {
    /// Pipeline area the check belongs to.
    pub area: ViewerGpuOutputDiagnosticArea,
    /// Stable machine-readable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: ViewerGpuOutputHealthSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional threshold or target value.
    pub limit: Option<u64>,
}

/// One prioritized root cause inferred from the summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputHealthRootCause {
    /// Pipeline area the root cause belongs to.
    pub area: ViewerGpuOutputDiagnosticArea,
    /// Stable machine-readable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: ViewerGpuOutputHealthSeverity,
    /// Compact evidence string for dashboards.
    pub evidence: String,
}

/// One suggested follow-up action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputHealthAction {
    /// Pipeline area the action belongs to.
    pub area: ViewerGpuOutputDiagnosticArea,
    /// Stable machine-readable action code.
    pub code: &'static str,
    /// Human-readable action text.
    pub description: &'static str,
}

/// Compact evidence pointers for a health report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputHealthEvidence {
    /// Last health status observed in the stream.
    pub last_status: Option<ViewerGpuOutputHealthStatus>,
    /// Last full health summary observed in the stream.
    pub last_health: Option<ViewerGpuOutputHealthSummary>,
    /// Last frame context observed in the stream.
    pub last_frame_context: Option<ViewerGpuOutputFrameContext>,
    /// Last display issue observed in the stream.
    pub last_display_issue: Option<ViewerGpuOutputDisplayIssueSummary>,
    /// Last display-contract refresh observed in the stream.
    pub last_display_contract_refresh: Option<ViewerGpuOutputDisplayContractRefreshEvent>,
    /// Last media color rejection observed in the stream.
    pub last_color_rejection: Option<ViewerGpuOutputColorRejectionSummary>,
    /// Last renderer runtime snapshot observed in the stream.
    pub last_runtime_report: Option<RenderGpuOutputRuntimeDiagnosticsReport>,
    /// Budget failures that drove a fail verdict.
    pub budget_failures: Vec<ViewerGpuOutputBudgetFailure>,
}

/// Serializable representation of the applied budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputBudgetReport {
    /// Minimum number of non-empty JSONL records.
    pub min_records: u64,
    /// Minimum number of ready viewer GPU-output attempts.
    pub min_ready: u64,
    /// Maximum allowed failed attempts.
    pub max_failed: u64,
    /// Maximum allowed display-contract-blocked attempts.
    pub max_blocked: u64,
    /// Maximum allowed external texture registration rejections.
    pub max_rejected: u64,
    /// Maximum allowed degraded attempts.
    pub max_degraded: u64,
    /// Maximum allowed waiting attempts.
    pub max_waiting: u64,
    /// Maximum allowed records with a display issue summary.
    pub max_display_issues: u64,
    /// Maximum allowed records blocked by missing HDR presentation support.
    pub max_hdr_output_requires_hdr_surface: u64,
    /// Maximum allowed records requiring a different surface color space.
    pub max_output_color_space_requires_surface_color_space: u64,
    /// Maximum allowed records blocked by the viewer/UI payload contract.
    pub max_reconfigure_blocked_by_payload: u64,
    /// Maximum allowed records targeting a non-presentation output intent.
    pub max_unsupported_presentation_intent: u64,
    /// Maximum allowed records lacking a supported surface contract.
    pub max_unsupported_surface_contract: u64,
    /// Maximum allowed records carrying an unknown display issue reason.
    pub max_unknown_display_issues: u64,
    /// Maximum allowed display-contract refresh events.
    pub max_display_contract_refreshes: u64,
    /// Maximum allowed display issues correlated to a preceding refresh event.
    pub max_display_issue_refresh_correlations: u64,
    /// Maximum allowed refreshes that changed tone-map headroom evidence.
    pub max_display_tone_map_headroom_changes: u64,
    /// Maximum allowed refreshes that changed available surface formats.
    pub max_available_surface_format_changes: u64,
    /// Maximum allowed refreshes that changed per-format color-space capabilities.
    pub max_format_color_space_changes: u64,
    /// Maximum allowed refreshes that changed present modes.
    pub max_present_mode_changes: u64,
    /// Maximum allowed refreshes that changed alpha modes.
    pub max_alpha_mode_changes: u64,
    /// Maximum allowed records with a display presentation payload blocker.
    pub max_display_payload_blockers: u64,
    /// Maximum allowed records carrying a viewer color rejection.
    pub max_color_rejections: u64,
    /// Maximum allowed records missing renderer-owned runtime snapshots.
    pub max_missing_runtime_reports: u64,
    /// Maximum allowed records missing renderer-owned stage snapshots.
    pub max_missing_stage_reports: u64,
    /// Maximum allowed ready records without structured preview-candidate context.
    pub max_ready_records_missing_preview_candidate_context: u64,
    /// Maximum allowed preview-candidate id regressions.
    pub max_preview_candidate_id_regressions: u64,
}

impl From<ViewerGpuOutputBudget> for ViewerGpuOutputBudgetReport {
    fn from(budget: ViewerGpuOutputBudget) -> Self {
        Self {
            min_records: budget.min_records,
            min_ready: budget.min_ready,
            max_failed: budget.max_failed,
            max_blocked: budget.max_blocked,
            max_rejected: budget.max_rejected,
            max_degraded: budget.max_degraded,
            max_waiting: budget.max_waiting,
            max_display_issues: budget.max_display_issues,
            max_hdr_output_requires_hdr_surface: budget.max_hdr_output_requires_hdr_surface,
            max_output_color_space_requires_surface_color_space: budget
                .max_output_color_space_requires_surface_color_space,
            max_reconfigure_blocked_by_payload: budget.max_reconfigure_blocked_by_payload,
            max_unsupported_presentation_intent: budget.max_unsupported_presentation_intent,
            max_unsupported_surface_contract: budget.max_unsupported_surface_contract,
            max_unknown_display_issues: budget.max_unknown_display_issues,
            max_display_contract_refreshes: budget.max_display_contract_refreshes,
            max_display_issue_refresh_correlations: budget.max_display_issue_refresh_correlations,
            max_display_tone_map_headroom_changes: budget.max_display_tone_map_headroom_changes,
            max_available_surface_format_changes: budget.max_available_surface_format_changes,
            max_format_color_space_changes: budget.max_format_color_space_changes,
            max_present_mode_changes: budget.max_present_mode_changes,
            max_alpha_mode_changes: budget.max_alpha_mode_changes,
            max_display_payload_blockers: budget.max_display_payload_blockers,
            max_color_rejections: budget.max_color_rejections,
            max_missing_runtime_reports: budget.max_missing_runtime_reports,
            max_missing_stage_reports: budget.max_missing_stage_reports,
            max_ready_records_missing_preview_candidate_context: budget
                .max_ready_records_missing_preview_candidate_context,
            max_preview_candidate_id_regressions: budget.max_preview_candidate_id_regressions,
        }
    }
}

/// One budget failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputBudgetFailure {
    /// Metric that violated the budget.
    pub metric: &'static str,
    /// Actual metric value.
    pub actual: u64,
    /// Configured limit.
    pub limit: u64,
}

/// One mismatch between replayed and reported cumulative health counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewerGpuOutputCountMismatch {
    /// 1-based JSONL line number.
    pub line: u64,
    /// Counts replayed from health statuses up to this line.
    pub replayed: ViewerGpuOutputHealthCounts,
    /// Counts reported by the record at this line.
    pub reported: ViewerGpuOutputHealthCounts,
}

/// Evaluate a viewer GPU-output diagnostics JSONL stream.
pub fn evaluate_jsonl(
    contents: &str,
    budget: &ViewerGpuOutputBudget,
) -> anyhow::Result<ViewerGpuOutputBudgetSummary> {
    let mut counts = ViewerGpuOutputHealthCounts::default();
    let mut records = 0u64;
    let mut last_status = None;
    let mut last_frame_context = None;
    let mut display_issues = ViewerGpuOutputDisplayIssueCounts::default();
    let mut display_contract_refreshes = ViewerGpuOutputDisplayContractRefreshCounts::default();
    let mut display_issue_refresh_correlations =
        ViewerGpuOutputDisplayIssueRefreshCorrelationCounts::default();
    let mut stage = ViewerGpuOutputStageCounts::default();
    let mut last_health = None;
    let mut last_display_issue = None;
    let mut last_display_contract_refresh = None;
    let mut color_rejections = 0u64;
    let mut media_issues = VideoColorDiagnosticIssueAggregate::default();
    let mut last_color_rejection = None;
    let mut last_runtime_report = None;
    let mut runtime_reports = 0u64;
    let mut stage_reports = 0u64;
    let mut ready_records_missing_preview_candidate_context = 0u64;
    let mut preview_candidate_id_regressions = 0u64;
    let mut last_ready_preview_candidate_id = None;
    let mut reported_counts = None;
    let mut count_mismatches = Vec::new();

    for (line_index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: ViewerGpuOutputDiagnosticRecord = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSONL record at line {}", line_index + 1))?;
        records = records.saturating_add(1);
        counts.record(record.health.status);
        stage.merge_max(record.stage_counts());
        if record.runtime_report.is_some() {
            runtime_reports = runtime_reports.saturating_add(1);
            last_runtime_report = record.runtime_report;
        }
        if record.accumulated_stage_report.is_some() {
            stage_reports = stage_reports.saturating_add(1);
        }
        if record.health.status == ViewerGpuOutputHealthStatus::Ready {
            match record.preview_candidate_id() {
                None => {
                    ready_records_missing_preview_candidate_context =
                        ready_records_missing_preview_candidate_context.saturating_add(1)
                }
                Some(candidate_id) => {
                    if let Some(last_candidate_id) = last_ready_preview_candidate_id {
                        if candidate_id < last_candidate_id {
                            preview_candidate_id_regressions =
                                preview_candidate_id_regressions.saturating_add(1);
                        }
                    }
                    last_ready_preview_candidate_id = Some(candidate_id);
                }
            }
        }
        if let Some(record_counts) = record.health_counts {
            reported_counts = Some(record_counts);
            if record_counts != counts {
                count_mismatches.push(ViewerGpuOutputCountMismatch {
                    line: (line_index + 1) as u64,
                    replayed: counts,
                    reported: record_counts,
                });
            }
        }
        if let Some(issue) = record.display_issue_summary {
            display_issues.record(&issue);
            display_issue_refresh_correlations.record(&issue);
            last_display_issue = Some(issue);
        }
        for refresh in record.recent_display_contract_refreshes {
            display_contract_refreshes.record(&refresh);
            last_display_contract_refresh = Some(refresh);
        }
        if let Some(refresh) = record.last_display_contract_refresh {
            let needs_record = last_display_contract_refresh
                .as_ref()
                .map(|last| last != &refresh)
                .unwrap_or(true);
            if needs_record {
                display_contract_refreshes.record(&refresh);
            }
            last_display_contract_refresh = Some(refresh);
        }
        if let Some(rejection) = record.last_color_rejection {
            color_rejections = color_rejections.saturating_add(1);
            media_issues.observe_summary(rejection.diagnostic_issue_summary);
            last_color_rejection = Some(rejection);
        }
        last_status = Some(record.health.status);
        last_health = Some(record.health);
        last_frame_context = record.last_frame_context;
    }

    let mut failures = Vec::new();
    if !count_mismatches.is_empty() {
        failures.push(ViewerGpuOutputBudgetFailure {
            metric: "health_counts_match_replay",
            actual: count_mismatches.len() as u64,
            limit: 0,
        });
    }
    if records < budget.min_records {
        failures.push(ViewerGpuOutputBudgetFailure {
            metric: "records",
            actual: records,
            limit: budget.min_records,
        });
    }
    if counts.ready < budget.min_ready {
        failures.push(ViewerGpuOutputBudgetFailure {
            metric: "ready",
            actual: counts.ready,
            limit: budget.min_ready,
        });
    }
    push_max_failure(&mut failures, "failed", counts.failed, budget.max_failed);
    push_max_failure(&mut failures, "blocked", counts.blocked, budget.max_blocked);
    push_max_failure(
        &mut failures,
        "rejected",
        counts.rejected,
        budget.max_rejected,
    );
    push_max_failure(
        &mut failures,
        "degraded",
        counts.degraded,
        budget.max_degraded,
    );
    push_max_failure(&mut failures, "waiting", counts.waiting, budget.max_waiting);
    push_max_failure(
        &mut failures,
        "display_issues",
        display_issues.total,
        budget.max_display_issues,
    );
    push_max_failure(
        &mut failures,
        "hdr_output_requires_hdr_surface",
        display_issues.hdr_output_requires_hdr_surface,
        budget.max_hdr_output_requires_hdr_surface,
    );
    push_max_failure(
        &mut failures,
        "output_color_space_requires_surface_color_space",
        display_issues.output_color_space_requires_surface_color_space,
        budget.max_output_color_space_requires_surface_color_space,
    );
    push_max_failure(
        &mut failures,
        "reconfigure_blocked_by_payload",
        display_issues.reconfigure_blocked_by_payload,
        budget.max_reconfigure_blocked_by_payload,
    );
    push_max_failure(
        &mut failures,
        "unsupported_presentation_intent",
        display_issues.unsupported_presentation_intent,
        budget.max_unsupported_presentation_intent,
    );
    push_max_failure(
        &mut failures,
        "unsupported_surface_contract",
        display_issues.unsupported_surface_contract,
        budget.max_unsupported_surface_contract,
    );
    push_max_failure(
        &mut failures,
        "os_display_profile_unsupported",
        display_issues.os_display_profile_unsupported,
        budget.max_os_display_profile_unsupported,
    );
    push_max_failure(
        &mut failures,
        "unknown_display_issues",
        display_issues.unknown,
        budget.max_unknown_display_issues,
    );
    push_max_failure(
        &mut failures,
        "display_contract_refreshes",
        display_contract_refreshes.total,
        budget.max_display_contract_refreshes,
    );
    push_max_failure(
        &mut failures,
        "display_issue_refresh_correlations",
        display_issue_refresh_correlations.total,
        budget.max_display_issue_refresh_correlations,
    );
    push_max_failure(
        &mut failures,
        "display_tone_map_headroom_changes",
        display_contract_refreshes.display_tone_map_headroom_changed,
        budget.max_display_tone_map_headroom_changes,
    );
    push_max_failure(
        &mut failures,
        "available_surface_format_changes",
        display_contract_refreshes.available_surface_formats_changed,
        budget.max_available_surface_format_changes,
    );
    push_max_failure(
        &mut failures,
        "format_color_space_changes",
        display_contract_refreshes.format_color_spaces_changed,
        budget.max_format_color_space_changes,
    );
    push_max_failure(
        &mut failures,
        "present_mode_changes",
        display_contract_refreshes.present_modes_changed,
        budget.max_present_mode_changes,
    );
    push_max_failure(
        &mut failures,
        "alpha_mode_changes",
        display_contract_refreshes.alpha_modes_changed,
        budget.max_alpha_mode_changes,
    );
    push_max_failure(
        &mut failures,
        "display_payload_blockers",
        display_issues.payload_blockers,
        budget.max_display_payload_blockers,
    );
    push_max_failure(
        &mut failures,
        "color_rejections",
        color_rejections,
        budget.max_color_rejections,
    );
    push_max_failure(
        &mut failures,
        "missing_runtime_reports",
        records.saturating_sub(runtime_reports),
        budget.max_missing_runtime_reports,
    );
    push_max_failure(
        &mut failures,
        "missing_stage_reports",
        records.saturating_sub(stage_reports),
        budget.max_missing_stage_reports,
    );
    push_max_failure(
        &mut failures,
        "ready_records_missing_preview_candidate_context",
        ready_records_missing_preview_candidate_context,
        budget.max_ready_records_missing_preview_candidate_context,
    );
    push_max_failure(
        &mut failures,
        "preview_candidate_id_regressions",
        preview_candidate_id_regressions,
        budget.max_preview_candidate_id_regressions,
    );

    Ok(ViewerGpuOutputBudgetSummary {
        records,
        counts,
        display_issues,
        display_contract_refreshes,
        display_issue_refresh_correlations,
        stage,
        color_rejections,
        media_issues,
        reported_counts,
        reported_counts_match_replay: count_mismatches.is_empty(),
        count_mismatches,
        budget: (*budget).into(),
        passed: failures.is_empty(),
        failures,
        last_status,
        last_health,
        last_frame_context,
        last_display_issue,
        last_display_contract_refresh,
        last_color_rejection,
        runtime_reports,
        stage_reports,
        missing_runtime_reports: records.saturating_sub(runtime_reports),
        missing_stage_reports: records.saturating_sub(stage_reports),
        ready_records_missing_preview_candidate_context,
        preview_candidate_id_regressions,
        last_preview_candidate_id: last_ready_preview_candidate_id,
        last_runtime_report,
    })
}

fn push_max_failure(
    failures: &mut Vec<ViewerGpuOutputBudgetFailure>,
    metric: &'static str,
    actual: u64,
    limit: u64,
) {
    if actual > limit {
        failures.push(ViewerGpuOutputBudgetFailure { metric, actual, limit });
    }
}

/// Build the high-level diagnostic report from a budget summary.
pub fn build_health_report(
    summary: ViewerGpuOutputBudgetSummary,
    profile: impl Into<String>,
    source_path: Option<String>,
) -> ViewerGpuOutputHealthReport {
    let mut checks = Vec::new();
    let mut root_causes = Vec::new();
    let mut actions = Vec::new();

    push_min_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::CaptureIntegrity,
        "records_present",
        summary.records,
        summary.budget.min_records,
    );
    push_bool_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::CaptureIntegrity,
        "health_counts_match_replay",
        summary.reported_counts_match_replay,
    );
    push_min_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::ViewerOutput,
        "ready_frames",
        summary.counts.ready,
        summary.budget.min_ready,
    );
    push_max_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::ViewerOutput,
        "failed_frames",
        summary.counts.failed,
        summary.budget.max_failed,
    );
    push_max_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::ViewerOutput,
        "blocked_frames",
        summary.counts.blocked,
        summary.budget.max_blocked,
    );
    push_max_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::ViewerOutput,
        "rejected_frames",
        summary.counts.rejected,
        summary.budget.max_rejected,
    );
    push_max_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::ViewerOutput,
        "degraded_frames",
        summary.counts.degraded,
        summary.budget.max_degraded,
    );
    push_max_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::ViewerOutput,
        "ready_records_missing_preview_candidate_context",
        summary.ready_records_missing_preview_candidate_context,
        summary.budget.max_ready_records_missing_preview_candidate_context,
    );
    push_max_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::ViewerOutput,
        "preview_candidate_id_regressions",
        summary.preview_candidate_id_regressions,
        summary.budget.max_preview_candidate_id_regressions,
    );
    push_gpu_color_checks(&mut checks, &summary);
    push_backend_runtime_checks(&mut checks, &summary);
    push_display_contract_checks(&mut checks, &summary);
    push_display_drift_checks(&mut checks, &summary);
    push_max_check(
        &mut checks,
        ViewerGpuOutputDiagnosticArea::MediaColorPolicy,
        "color_rejections",
        summary.color_rejections,
        summary.budget.max_color_rejections,
    );
    if summary.media_issues.diagnostics_with_warnings > 0 {
        checks.push(ViewerGpuOutputHealthCheck {
            area: ViewerGpuOutputDiagnosticArea::MediaColorPolicy,
            code: "media_warnings",
            severity: ViewerGpuOutputHealthSeverity::Warn,
            observed: summary.media_issues.diagnostics_with_warnings,
            limit: Some(0),
        });
    }

    push_root_causes_and_actions(&summary, &mut root_causes, &mut actions);

    let has_failures = !summary.passed
        || checks.iter().any(|check| check.severity == ViewerGpuOutputHealthSeverity::Fail);
    let has_warnings =
        checks.iter().any(|check| check.severity == ViewerGpuOutputHealthSeverity::Warn);
    let verdict = if has_failures {
        ViewerGpuOutputHealthVerdict::Fail
    } else if has_warnings {
        ViewerGpuOutputHealthVerdict::Warn
    } else {
        ViewerGpuOutputHealthVerdict::Pass
    };

    let evidence = ViewerGpuOutputHealthEvidence {
        last_status: summary.last_status,
        last_health: summary.last_health,
        last_frame_context: summary.last_frame_context.clone(),
        last_display_issue: summary.last_display_issue.clone(),
        last_display_contract_refresh: summary.last_display_contract_refresh.clone(),
        last_color_rejection: summary.last_color_rejection.clone(),
        last_runtime_report: summary.last_runtime_report,
        budget_failures: summary.failures.clone(),
    };

    ViewerGpuOutputHealthReport {
        schema_version: VIEWER_GPU_OUTPUT_HEALTH_REPORT_SCHEMA_VERSION,
        profile: profile.into(),
        source_path,
        verdict,
        summary,
        checks,
        root_causes,
        actions,
        evidence,
    }
}

fn push_min_check(
    checks: &mut Vec<ViewerGpuOutputHealthCheck>,
    area: ViewerGpuOutputDiagnosticArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(ViewerGpuOutputHealthCheck {
        area,
        code,
        severity: if observed < limit {
            ViewerGpuOutputHealthSeverity::Fail
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_max_check(
    checks: &mut Vec<ViewerGpuOutputHealthCheck>,
    area: ViewerGpuOutputDiagnosticArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(ViewerGpuOutputHealthCheck {
        area,
        code,
        severity: if observed > limit {
            ViewerGpuOutputHealthSeverity::Fail
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_bool_check(
    checks: &mut Vec<ViewerGpuOutputHealthCheck>,
    area: ViewerGpuOutputDiagnosticArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(ViewerGpuOutputHealthCheck {
        area,
        code,
        severity: if passed {
            ViewerGpuOutputHealthSeverity::Pass
        } else {
            ViewerGpuOutputHealthSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_warn_check(
    checks: &mut Vec<ViewerGpuOutputHealthCheck>,
    area: ViewerGpuOutputDiagnosticArea,
    code: &'static str,
    observed: u64,
) {
    checks.push(ViewerGpuOutputHealthCheck {
        area,
        code,
        severity: if observed > 0 {
            ViewerGpuOutputHealthSeverity::Warn
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed,
        limit: Some(0),
    });
}

fn push_gpu_color_checks(
    checks: &mut Vec<ViewerGpuOutputHealthCheck>,
    summary: &ViewerGpuOutputBudgetSummary,
) {
    push_warn_check(
        checks,
        ViewerGpuOutputDiagnosticArea::GpuColorPath,
        "upload_stages",
        summary.stage.upload_stages,
    );
    push_warn_check(
        checks,
        ViewerGpuOutputDiagnosticArea::GpuColorPath,
        "readback_stages",
        summary.stage.readback_stages,
    );
    checks.push(ViewerGpuOutputHealthCheck {
        area: ViewerGpuOutputDiagnosticArea::GpuColorPath,
        code: "gpu_color_stages_present",
        severity: if summary.stage.total_stages > 0 && summary.stage.gpu_color_stages == 0 {
            ViewerGpuOutputHealthSeverity::Warn
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed: summary.stage.gpu_color_stages,
        limit: None,
    });
    checks.push(ViewerGpuOutputHealthCheck {
        area: ViewerGpuOutputDiagnosticArea::GpuColorPath,
        code: "gpu_blockers",
        severity: if summary.stage.gpu_blockers > 0 {
            ViewerGpuOutputHealthSeverity::Fail
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed: summary.stage.gpu_blockers,
        limit: Some(0),
    });
    checks.push(ViewerGpuOutputHealthCheck {
        area: ViewerGpuOutputDiagnosticArea::GpuColorPath,
        code: "stage_report_coverage",
        severity: if summary.missing_stage_reports > summary.budget.max_missing_stage_reports {
            ViewerGpuOutputHealthSeverity::Fail
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed: summary.missing_stage_reports,
        limit: Some(summary.budget.max_missing_stage_reports),
    });
}

fn push_backend_runtime_checks(
    checks: &mut Vec<ViewerGpuOutputHealthCheck>,
    summary: &ViewerGpuOutputBudgetSummary,
) {
    checks.push(ViewerGpuOutputHealthCheck {
        area: ViewerGpuOutputDiagnosticArea::BackendRuntime,
        code: "runtime_report_coverage",
        severity: if summary.missing_runtime_reports > summary.budget.max_missing_runtime_reports {
            ViewerGpuOutputHealthSeverity::Fail
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed: summary.missing_runtime_reports,
        limit: Some(summary.budget.max_missing_runtime_reports),
    });
    let Some(runtime) = summary.last_runtime_report else {
        return;
    };

    checks.push(ViewerGpuOutputHealthCheck {
        area: ViewerGpuOutputDiagnosticArea::BackendRuntime,
        code: "shader_cache_extraction_failures",
        severity: if runtime.shader_cache_extraction_failures > 0 {
            ViewerGpuOutputHealthSeverity::Fail
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed: runtime.shader_cache_extraction_failures,
        limit: Some(0),
    });
    checks.push(ViewerGpuOutputHealthCheck {
        area: ViewerGpuOutputDiagnosticArea::BackendRuntime,
        code: "backend_object_failures",
        severity: if runtime.backend_object_failures > 0 {
            ViewerGpuOutputHealthSeverity::Fail
        } else {
            ViewerGpuOutputHealthSeverity::Pass
        },
        observed: runtime.backend_object_failures,
        limit: Some(0),
    });
}

fn push_display_contract_checks(
    checks: &mut Vec<ViewerGpuOutputHealthCheck>,
    summary: &ViewerGpuOutputBudgetSummary,
) {
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayContract,
        "display_issues",
        summary.display_issues.total,
        summary.budget.max_display_issues,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayContract,
        "hdr_surface_blockers",
        summary.display_issues.hdr_output_requires_hdr_surface,
        summary.budget.max_hdr_output_requires_hdr_surface,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayContract,
        "surface_color_space_blockers",
        summary.display_issues.output_color_space_requires_surface_color_space,
        summary.budget.max_output_color_space_requires_surface_color_space,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayContract,
        "payload_blockers",
        summary.display_issues.payload_blockers,
        summary.budget.max_display_payload_blockers,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayContract,
        "unknown_display_issues",
        summary.display_issues.unknown,
        summary.budget.max_unknown_display_issues,
    );
}

fn push_display_drift_checks(
    checks: &mut Vec<ViewerGpuOutputHealthCheck>,
    summary: &ViewerGpuOutputBudgetSummary,
) {
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
        "display_contract_refreshes",
        summary.display_contract_refreshes.total,
        summary.budget.max_display_contract_refreshes,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
        "display_issue_refresh_correlations",
        summary.display_issue_refresh_correlations.total,
        summary.budget.max_display_issue_refresh_correlations,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
        "tone_map_headroom_changes",
        summary.display_contract_refreshes.display_tone_map_headroom_changed,
        summary.budget.max_display_tone_map_headroom_changes,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
        "available_surface_format_changes",
        summary.display_contract_refreshes.available_surface_formats_changed,
        summary.budget.max_available_surface_format_changes,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
        "format_color_space_changes",
        summary.display_contract_refreshes.format_color_spaces_changed,
        summary.budget.max_format_color_space_changes,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
        "present_mode_changes",
        summary.display_contract_refreshes.present_modes_changed,
        summary.budget.max_present_mode_changes,
    );
    push_max_check(
        checks,
        ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
        "alpha_mode_changes",
        summary.display_contract_refreshes.alpha_modes_changed,
        summary.budget.max_alpha_mode_changes,
    );
}

fn push_root_causes_and_actions(
    summary: &ViewerGpuOutputBudgetSummary,
    root_causes: &mut Vec<ViewerGpuOutputHealthRootCause>,
    actions: &mut Vec<ViewerGpuOutputHealthAction>,
) {
    for failure in &summary.failures {
        match failure.metric {
            "records" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::CaptureIntegrity,
                "jsonl_capture_missing",
                format!("records={} required={}", failure.actual, failure.limit),
                "capture_viewer_jsonl",
                "Capture a live viewer GPU-output JSONL stream before evaluating the budget.",
            ),
            "health_counts_match_replay" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::CaptureIntegrity,
                "health_count_replay_mismatch",
                format!("mismatches={}", failure.actual),
                "inspect_jsonl_writer",
                "Inspect cumulative health-count emission; replayed statuses must match reported counters.",
            ),
            "ready" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::ViewerOutput,
                "no_ready_viewer_frames",
                format!("ready={} required={}", failure.actual, failure.limit),
                "drive_viewer_until_ready",
                "Drive playback or scrubbing until at least one viewer GPU-output frame reaches Ready.",
            ),
            "missing_runtime_reports" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::BackendRuntime,
                "missing_runtime_report",
                format!(
                    "missing_runtime_reports={} limit={}",
                    failure.actual, failure.limit
                ),
                "restore_viewer_runtime_reporting",
                "Emit per-record renderer-owned runtime reports in viewer GPU-output JSONL.",
            ),
            "missing_stage_reports" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::GpuColorPath,
                "missing_stage_report",
                format!("missing_stage_reports={} limit={}", failure.actual, failure.limit),
                "restore_viewer_stage_reporting",
                "Emit per-record renderer-owned stage report snapshots in viewer GPU-output JSONL.",
            ),
            "ready_records_missing_preview_candidate_context" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::ViewerOutput,
                "missing_preview_candidate_context",
                format!(
                    "ready_records_missing_preview_candidate_context={} limit={}",
                    failure.actual, failure.limit
                ),
                "write_viewer_preview_candidate_context",
                "Persist preview-candidate identity/state fields with each ready viewer output record.",
            ),
            "preview_candidate_id_regressions" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::ViewerOutput,
                "preview_candidate_id_regression",
                format!(
                    "preview_candidate_id_regressions={} limit={}",
                    failure.actual, failure.limit
                ),
                "investigate_preview_candidate_id_sequence",
                "Inspect preview candidate ID generation and ensure IDs are monotonic across output attempts.",
            ),
            "failed" | "blocked" | "rejected" | "degraded" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::ViewerOutput,
                "viewer_output_not_healthy",
                format!("{}={} limit={}", failure.metric, failure.actual, failure.limit),
                "inspect_viewer_outcome",
                "Inspect last_outcome, health flags, and frame context for the failing viewer state.",
            ),
            "color_rejections" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::MediaColorPolicy,
                "media_color_policy_rejected_source",
                format!("color_rejections={} limit={}", failure.actual, failure.limit),
                "inspect_media_color_metadata",
                "Inspect the last color rejection and source metadata evidence before relaxing policy.",
            ),
            "display_contract_refreshes" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
                "display_contract_refresh_churn",
                display_refresh_churn_evidence(summary, failure.actual, failure.limit),
                "inspect_display_refresh_history",
                "Inspect display-contract refresh history and identify whether monitor, format, or HDR contract churn is expected.",
            ),
            "display_issue_refresh_correlations" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
                "display_issue_correlated_with_contract_refresh",
                display_issue_refresh_correlation_evidence(summary, failure.actual, failure.limit),
                "inspect_display_issue_correlation",
                "Inspect the preceding refresh attached to the display issue and confirm whether the blocker began after monitor or surface reconfiguration.",
            ),
            "display_tone_map_headroom_changes" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
                "display_tone_map_headroom_drift",
                display_tone_map_headroom_evidence(summary, failure.actual, failure.limit),
                "inspect_display_tone_map_headroom",
                "Inspect previous and next HDR headroom evidence to confirm whether monitor HDR reporting drifted.",
            ),
            "available_surface_format_changes" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
                "surface_format_capability_drift",
                display_surface_format_drift_evidence(summary, failure.actual, failure.limit),
                "inspect_surface_format_capabilities",
                "Inspect previous and next available surface format sets for monitor or backend capability drift.",
            ),
            "format_color_space_changes" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
                "surface_color_space_capability_drift",
                display_format_color_space_drift_evidence(summary, failure.actual, failure.limit),
                "inspect_surface_color_space_capabilities",
                "Inspect per-format surface color-space capability diffs before trusting display promotion decisions.",
            ),
            "present_mode_changes" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
                "present_mode_capability_drift",
                display_present_mode_drift_evidence(summary, failure.actual, failure.limit),
                "inspect_present_mode_capabilities",
                "Inspect present-mode capability changes reported by the OS/backend around the refresh event.",
            ),
            "alpha_mode_changes" => push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::DisplayCapabilityDrift,
                "alpha_mode_capability_drift",
                display_alpha_mode_drift_evidence(summary, failure.actual, failure.limit),
                "inspect_alpha_mode_capabilities",
                "Inspect alpha-mode capability changes reported by the OS/backend around the refresh event.",
            ),
            _ => push_display_contract_root_cause(summary, failure, root_causes, actions),
        }
    }

    if summary.stage.gpu_blockers > 0 {
        push_root_cause_with_action(
            root_causes,
            actions,
            ViewerGpuOutputDiagnosticArea::GpuColorPath,
            "gpu_color_stage_blocked",
            format!(
                "gpu_blockers={} shader={} ocio={} wrapper={} pipeline={}",
                summary.stage.gpu_blockers,
                summary.stage.gpu_shader_module_blockers,
                summary.stage.gpu_ocio_resource_blockers,
                summary.stage.gpu_wrapper_blockers,
                summary.stage.gpu_render_pipeline_blockers
            ),
            "inspect_gpu_blocker_breakdown",
            "Inspect shader module, OCIO resource, fullscreen wrapper, and render pipeline preparation.",
        );
    }
    if summary.stage.upload_stages > 0 || summary.stage.readback_stages > 0 {
        push_root_cause_with_action(
            root_causes,
            actions,
            ViewerGpuOutputDiagnosticArea::GpuColorPath,
            "cpu_gpu_transfer_stage_present",
            format!(
                "upload_stages={} readback_stages={}",
                summary.stage.upload_stages, summary.stage.readback_stages
            ),
            "remove_transfer_stage",
            "Trace why the viewer output path left the native GPU color path and introduced transfer stages.",
        );
    }
    if let Some(runtime) = summary.last_runtime_report {
        if runtime.shader_cache_extraction_failures > 0 {
            push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::BackendRuntime,
                "shader_cache_extraction_failed",
                format!(
                    "shader_cache_entries={} hits={} misses={} extraction_failures={}",
                    runtime.shader_cache_entries,
                    runtime.shader_cache_hits,
                    runtime.shader_cache_misses,
                    runtime.shader_cache_extraction_failures
                ),
                "inspect_shader_cache_extraction",
                "Inspect renderer shader-cache extraction failures before trusting viewer GPU readiness.",
            );
        }
        if runtime.backend_object_failures > 0 {
            push_root_cause_with_action(
                root_causes,
                actions,
                ViewerGpuOutputDiagnosticArea::BackendRuntime,
                "backend_object_runtime_failed",
                format!(
                    "backend_object_entries={} hits={} misses={} failures={}",
                    runtime.backend_object_entries,
                    runtime.backend_object_hits,
                    runtime.backend_object_misses,
                    runtime.backend_object_failures
                ),
                "inspect_backend_object_runtime",
                "Inspect renderer backend-object preparation failures before trusting viewer GPU readiness.",
            );
        }
    }
}

fn push_display_contract_root_cause(
    summary: &ViewerGpuOutputBudgetSummary,
    failure: &ViewerGpuOutputBudgetFailure,
    root_causes: &mut Vec<ViewerGpuOutputHealthRootCause>,
    actions: &mut Vec<ViewerGpuOutputHealthAction>,
) {
    let (root_code, action_code, action_description) = match failure.metric {
        "hdr_output_requires_hdr_surface" => (
            "hdr_output_requires_hdr_surface",
            "inspect_hdr_surface_contract",
            "Inspect the selected surface HDR mode and desired HDR surface contract for the blocked output.",
        ),
        "output_color_space_requires_surface_color_space" => (
            "output_color_space_requires_surface_color_space",
            "inspect_surface_color_space_contract",
            "Inspect the selected and desired surface color spaces for the blocked output.",
        ),
        "reconfigure_blocked_by_payload" | "display_payload_blockers" => (
            "display_payload_contract_blocked",
            "inspect_display_payload_blocker",
            "Inspect the viewer payload blocker that prevented surface reconfiguration or promotion.",
        ),
        "unsupported_presentation_intent" => (
            "unsupported_presentation_intent",
            "inspect_unsupported_presentation_intent",
            "Inspect the requested viewer output intent and confirm it targets a real presentation space.",
        ),
        "unsupported_surface_contract" => (
            "unsupported_surface_contract",
            "inspect_unsupported_surface_contract",
            "Inspect the desired output contract and confirm the current monitor/surface can present it.",
        ),
        "os_display_profile_unsupported" => (
            "os_display_profile_unsupported",
            "inspect_os_display_profile_support",
            "OS-level ICC profile, EDR, or HDR behavior is not supported. Display management cannot guarantee correct color presentation on this platform.",
        ),
        "unknown_display_issues" => (
            "unknown_display_issue_reason",
            "inspect_unknown_display_issue_reason",
            "Inspect the raw display issue reason and extend the evaluator with an explicit classification.",
        ),
        "display_issues" => (
            "display_issue_summary_present",
            "inspect_display_issue_summary",
            "Inspect the last structured display issue summary and its desired surface contract evidence.",
        ),
        _ => (
            "display_contract_blocked_output",
            "inspect_display_contract",
            "Inspect display issue reason, desired surface contract, and payload blocker evidence.",
        ),
    };
    push_root_cause_with_action(
        root_causes,
        actions,
        ViewerGpuOutputDiagnosticArea::DisplayContract,
        root_code,
        display_issue_evidence(summary, failure.metric, failure.actual, failure.limit),
        action_code,
        action_description,
    );
}

fn display_issue_evidence(
    summary: &ViewerGpuOutputBudgetSummary,
    metric: &str,
    actual: u64,
    limit: u64,
) -> String {
    let issue = summary.last_display_issue.as_ref();
    format!(
        "{}={} limit={} reason={} output={} desired={} selected={} payload={} target_supported={} display={}",
        metric,
        actual,
        limit,
        issue.map(|i| i.reason.as_str()).unwrap_or("unknown"),
        issue
            .and_then(|i| i.output_color_space.as_deref())
            .unwrap_or("unknown"),
        issue
            .and_then(|i| i.desired_surface_color_space.as_deref())
            .unwrap_or("unknown"),
        issue
            .and_then(|i| i.selected_surface_color_space.as_deref())
            .unwrap_or("unknown"),
        issue
            .and_then(|i| i.payload_blocker.as_deref())
            .unwrap_or("none"),
        issue
            .and_then(|i| i.target_surface_color_space_supported)
            .map(|supported| if supported { "true" } else { "false" })
            .unwrap_or("unknown"),
        issue
            .and_then(|i| i.display_target.as_ref())
            .map(format_display_target)
            .unwrap_or_else(|| "unknown".to_owned()),
    )
}

fn display_refresh_churn_evidence(
    summary: &ViewerGpuOutputBudgetSummary,
    actual: u64,
    limit: u64,
) -> String {
    let refreshes = summary.display_contract_refreshes;
    let last_reason = summary
        .last_display_contract_refresh
        .as_ref()
        .map(|refresh| refresh.reason.as_str())
        .unwrap_or("unknown");
    format!(
        "display_contract_refreshes={} limit={} resize={} scale_factor_changed={} window_moved={} renderer_rebuilt={} display_target_changed={} last_reason={}",
        actual,
        limit,
        refreshes.resize,
        refreshes.scale_factor_changed,
        refreshes.window_moved,
        refreshes.renderer_rebuilt,
        refreshes.display_target_changed,
        last_reason,
    )
}

fn display_issue_refresh_correlation_evidence(
    summary: &ViewerGpuOutputBudgetSummary,
    actual: u64,
    limit: u64,
) -> String {
    let correlations = summary.display_issue_refresh_correlations;
    let preceding_reason = summary
        .last_display_issue
        .as_ref()
        .and_then(|issue| issue.preceding_display_contract_refresh.as_ref())
        .map(|refresh| refresh.reason.as_str())
        .unwrap_or("unknown");
    format!(
        "display_issue_refresh_correlations={} limit={} after_resize={} after_scale_factor_changed={} after_window_moved={} preceding_reason={}",
        actual,
        limit,
        correlations.after_resize,
        correlations.after_scale_factor_changed,
        correlations.after_window_moved,
        preceding_reason,
    )
}

fn display_tone_map_headroom_evidence(
    summary: &ViewerGpuOutputBudgetSummary,
    actual: u64,
    limit: u64,
) -> String {
    let refresh = summary.last_display_contract_refresh.as_ref();
    format!(
        "display_tone_map_headroom_changes={} limit={} previous_headroom_ppm={} next_headroom_ppm={} last_reason={}",
        actual,
        limit,
        refresh
            .and_then(|refresh| refresh.previous.display_tone_map_headroom_ppm)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_owned()),
        refresh
            .and_then(|refresh| refresh.next.display_tone_map_headroom_ppm)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_owned()),
        refresh
            .map(|refresh| refresh.reason.as_str())
            .unwrap_or("unknown"),
    )
}

fn display_surface_format_drift_evidence(
    summary: &ViewerGpuOutputBudgetSummary,
    actual: u64,
    limit: u64,
) -> String {
    let refresh = summary.last_display_contract_refresh.as_ref();
    format!(
        "available_surface_format_changes={} limit={} previous_formats={} next_formats={} last_reason={}",
        actual,
        limit,
        refresh
            .map(|refresh| refresh.previous.available_surface_formats.join(","))
            .filter(|formats| !formats.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        refresh
            .map(|refresh| refresh.next.available_surface_formats.join(","))
            .filter(|formats| !formats.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        refresh
            .map(|refresh| refresh.reason.as_str())
            .unwrap_or("unknown"),
    )
}

fn display_format_color_space_drift_evidence(
    summary: &ViewerGpuOutputBudgetSummary,
    actual: u64,
    limit: u64,
) -> String {
    let refresh = summary.last_display_contract_refresh.as_ref();
    format!(
        "format_color_space_changes={} limit={} previous_formats={} next_formats={} last_reason={}",
        actual,
        limit,
        refresh
            .map(|refresh| summarize_format_color_spaces(&refresh.previous.format_color_spaces))
            .filter(|formats| !formats.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        refresh
            .map(|refresh| summarize_format_color_spaces(&refresh.next.format_color_spaces))
            .filter(|formats| !formats.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        refresh.map(|refresh| refresh.reason.as_str()).unwrap_or("unknown"),
    )
}

fn display_present_mode_drift_evidence(
    summary: &ViewerGpuOutputBudgetSummary,
    actual: u64,
    limit: u64,
) -> String {
    let refresh = summary.last_display_contract_refresh.as_ref();
    format!(
        "present_mode_changes={} limit={} previous_present_modes={} next_present_modes={} last_reason={}",
        actual,
        limit,
        refresh
            .map(|refresh| refresh.previous.present_modes.join(","))
            .filter(|modes| !modes.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        refresh
            .map(|refresh| refresh.next.present_modes.join(","))
            .filter(|modes| !modes.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        refresh
            .map(|refresh| refresh.reason.as_str())
            .unwrap_or("unknown"),
    )
}

fn display_alpha_mode_drift_evidence(
    summary: &ViewerGpuOutputBudgetSummary,
    actual: u64,
    limit: u64,
) -> String {
    let refresh = summary.last_display_contract_refresh.as_ref();
    format!(
        "alpha_mode_changes={} limit={} previous_alpha_modes={} next_alpha_modes={} last_reason={}",
        actual,
        limit,
        refresh
            .map(|refresh| refresh.previous.alpha_modes.join(","))
            .filter(|modes| !modes.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        refresh
            .map(|refresh| refresh.next.alpha_modes.join(","))
            .filter(|modes| !modes.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        refresh.map(|refresh| refresh.reason.as_str()).unwrap_or("unknown"),
    )
}

fn summarize_format_color_spaces(formats: &[ViewerGpuOutputSurfaceFormatColorSpaces]) -> String {
    formats
        .iter()
        .map(|format| {
            let mut supported = Vec::new();
            if format.srgb {
                supported.push("Srgb");
            }
            if format.extended_srgb_linear {
                supported.push("ExtendedSrgbLinear");
            }
            if format.display_p3 {
                supported.push("DisplayP3");
            }
            if format.bt2100_pq {
                supported.push("Bt2100Pq");
            }
            if format.bt2100_hlg {
                supported.push("Bt2100Hlg");
            }
            if format.extended_srgb {
                supported.push("ExtendedSrgb");
            }
            if format.extended_display_p3 {
                supported.push("ExtendedDisplayP3");
            }
            format!("{}:[{}]", format.format, supported.join("|"))
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn format_display_target(display_target: &ViewerGpuOutputDisplayTarget) -> String {
    format!(
        "{}@{},{} {}x{} scale_ppm={} refresh_mhz={}",
        display_target.name.as_deref().unwrap_or("unknown"),
        display_target.position.0,
        display_target.position.1,
        display_target.physical_size.0,
        display_target.physical_size.1,
        display_target.scale_factor_ppm,
        display_target
            .refresh_rate_millihertz
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
    )
}

fn push_root_cause_with_action(
    root_causes: &mut Vec<ViewerGpuOutputHealthRootCause>,
    actions: &mut Vec<ViewerGpuOutputHealthAction>,
    area: ViewerGpuOutputDiagnosticArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(ViewerGpuOutputHealthRootCause {
            area,
            code: root_code,
            severity: ViewerGpuOutputHealthSeverity::Fail,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(ViewerGpuOutputHealthAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ViewerGpuOutputDiagnosticRecord {
    health: ViewerGpuOutputHealthSummary,
    health_counts: Option<ViewerGpuOutputHealthCounts>,
    accumulated_stage_report: Option<RenderGpuOutputStageDiagnosticsReport>,
    last_stage_report: Option<RenderGpuOutputStageDiagnosticsReport>,
    runtime_report: Option<RenderGpuOutputRuntimeDiagnosticsReport>,
    #[serde(default)]
    stage_total_stages: u64,
    #[serde(default)]
    stage_upload_stages: u64,
    #[serde(default)]
    stage_gpu_color_stages: u64,
    #[serde(default)]
    stage_readback_stages: u64,
    #[serde(default)]
    stage_gpu_blockers: u64,
    #[serde(default)]
    stage_gpu_shader_module_blockers: u64,
    #[serde(default)]
    stage_gpu_ocio_resource_blockers: u64,
    #[serde(default)]
    stage_gpu_wrapper_blockers: u64,
    #[serde(default)]
    stage_gpu_render_pipeline_blockers: u64,
    #[serde(default)]
    stage_pixels: u64,
    last_frame_context: Option<ViewerGpuOutputFrameContext>,
    display_issue_summary: Option<ViewerGpuOutputDisplayIssueSummary>,
    #[serde(default)]
    recent_display_contract_refreshes: Vec<ViewerGpuOutputDisplayContractRefreshEvent>,
    last_display_contract_refresh: Option<ViewerGpuOutputDisplayContractRefreshEvent>,
    last_color_rejection: Option<ViewerGpuOutputColorRejectionSummary>,
    #[serde(default)]
    last_preview_candidate_id: Option<u64>,
    #[serde(default)]
    last_preview_candidate_state: Option<ViewerGpuOutputPreviewCandidateState>,
    #[serde(default)]
    preview_candidate_id: Option<u64>,
}

impl ViewerGpuOutputDiagnosticRecord {
    fn preview_candidate_id(&self) -> Option<u64> {
        self.last_preview_candidate_id.or(self.preview_candidate_id).or_else(|| {
            self.last_frame_context
                .as_ref()
                .and_then(|context| context.preview_candidate_id)
        })
    }

    fn stage_counts(&self) -> ViewerGpuOutputStageCounts {
        if let Some(report) = self.accumulated_stage_report {
            return report.into();
        }
        ViewerGpuOutputStageCounts {
            total_stages: self.stage_total_stages,
            upload_stages: self.stage_upload_stages,
            gpu_color_stages: self.stage_gpu_color_stages,
            readback_stages: self.stage_readback_stages,
            gpu_blockers: self.stage_gpu_blockers,
            gpu_shader_module_blockers: self.stage_gpu_shader_module_blockers,
            gpu_ocio_resource_blockers: self.stage_gpu_ocio_resource_blockers,
            gpu_wrapper_blockers: self.stage_gpu_wrapper_blockers,
            gpu_render_pipeline_blockers: self.stage_gpu_render_pipeline_blockers,
            pixels: self.stage_pixels,
        }
    }
}

/// Viewer color rejection summary consumed from viewer GPU-output JSONL records.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputColorRejectionSummary {
    /// Rejected asset id.
    pub asset_id: String,
    /// Rejected media path.
    pub path: String,
    /// Missing-metadata policy active during rejection.
    pub missing_metadata_policy: String,
    /// Input color-resolution source.
    pub source: String,
    /// Override color space, when present.
    pub override_color_space: Option<String>,
    /// Detected media color space, when present.
    pub detected_color_space: Option<String>,
    /// Sequence working color space.
    pub working_color_space: String,
    /// Compact human-readable diagnostic summary.
    pub diagnostic_summary: String,
    /// Machine-readable media issue summary.
    pub diagnostic_issue_summary: VideoColorDiagnosticIssueSummary,
}

/// Full health flags consumed from viewer GPU-output JSONL records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputHealthSummary {
    /// Coarse health status for the current record.
    pub status: ViewerGpuOutputHealthStatus,
    /// Viewer output is registered and ready.
    #[serde(default)]
    pub viewer_output_ready: bool,
    /// Native GPU output boundary is ready.
    #[serde(default)]
    pub native_gpu_boundary_ready: bool,
    /// Display boundary accepted the output request.
    #[serde(default)]
    pub display_boundary_ready: bool,
    /// Presentation path is ready for the requested output.
    #[serde(default)]
    pub presentation_ready: bool,
    /// Color-stage sequence has been recorded.
    #[serde(default)]
    pub stage_sequence_ready: bool,
    /// No GPU-stage blockers were reported.
    #[serde(default)]
    pub no_gpu_blockers: bool,
    /// Output texture was available to the viewer.
    #[serde(default)]
    pub output_texture_available: bool,
    /// External texture registration succeeded.
    #[serde(default)]
    pub external_texture_registered: bool,
}

/// Terminal or transient outcome observed by one Viewer GPU-output attempt.
///
/// Presentation adapters report facts using this vocabulary; the shared
/// classifier below is the only owner of the resulting health policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ViewerGpuOutputAttemptOutcome {
    /// The product is not currently showing a workspace.
    NonWorkspace,
    /// The already-registered output is still current.
    Current,
    /// The requested preview is still loading.
    Loading,
    /// No preview output is available for the request.
    Unavailable,
    /// The presentation adapter received an invalid external-texture key.
    InvalidTextureKey,
    /// The display contract rejected the requested output.
    DisplayContractBlocked,
    /// Renderer output recording failed.
    RecordFailed,
    /// Renderer recording succeeded without producing the promised texture.
    OutputTextureMissing,
    /// The output texture was registered with the presentation adapter.
    Registered,
    /// The presentation adapter rejected the external frame.
    ExternalFrameRejected,
}

/// Classify one Viewer GPU-output attempt from adapter facts.
///
/// This function deliberately contains no Window, Widget, WGPU-surface, or
/// environment access, so production presentation and Headless gates cannot
/// drift on the meaning of `Ready` or `Degraded`.
pub(crate) fn classify_viewer_gpu_output_health(
    outcome: Option<ViewerGpuOutputAttemptOutcome>,
    presentation_ready: bool,
    last_stage_diagnostics: Option<RenderColorStageDiagnostics>,
) -> ViewerGpuOutputHealthSummary {
    let Some(outcome) = outcome else {
        return ViewerGpuOutputHealthSummary::default();
    };
    let display_boundary_ready = outcome != ViewerGpuOutputAttemptOutcome::DisplayContractBlocked;
    let stage_sequence_ready = last_stage_diagnostics
        .map(|diagnostics| {
            diagnostics.total_stages == 2
                && diagnostics.upload_stages == 1
                && diagnostics.gpu_color_stages == 1
                && diagnostics.readback_stages == 0
        })
        .unwrap_or(false);
    let no_gpu_blockers = last_stage_diagnostics
        .map(|diagnostics| {
            diagnostics.gpu_blockers == 0 && diagnostics.gpu_blocker_breakdown.total() == 0
        })
        .unwrap_or(false);
    let output_texture_available = outcome != ViewerGpuOutputAttemptOutcome::OutputTextureMissing
        && last_stage_diagnostics.is_some();
    let external_texture_registered = outcome == ViewerGpuOutputAttemptOutcome::Registered;
    let native_gpu_boundary_ready =
        stage_sequence_ready && no_gpu_blockers && output_texture_available;
    let viewer_output_ready = native_gpu_boundary_ready
        && display_boundary_ready
        && presentation_ready
        && external_texture_registered;
    let status = match outcome {
        ViewerGpuOutputAttemptOutcome::NonWorkspace
        | ViewerGpuOutputAttemptOutcome::Current
        | ViewerGpuOutputAttemptOutcome::Loading
        | ViewerGpuOutputAttemptOutcome::Unavailable
        | ViewerGpuOutputAttemptOutcome::InvalidTextureKey => ViewerGpuOutputHealthStatus::Waiting,
        ViewerGpuOutputAttemptOutcome::DisplayContractBlocked => {
            ViewerGpuOutputHealthStatus::Blocked
        }
        ViewerGpuOutputAttemptOutcome::RecordFailed
        | ViewerGpuOutputAttemptOutcome::OutputTextureMissing => {
            ViewerGpuOutputHealthStatus::Failed
        }
        ViewerGpuOutputAttemptOutcome::ExternalFrameRejected => {
            ViewerGpuOutputHealthStatus::Rejected
        }
        ViewerGpuOutputAttemptOutcome::Registered if viewer_output_ready => {
            ViewerGpuOutputHealthStatus::Ready
        }
        ViewerGpuOutputAttemptOutcome::Registered => ViewerGpuOutputHealthStatus::Degraded,
    };

    ViewerGpuOutputHealthSummary {
        status,
        viewer_output_ready,
        native_gpu_boundary_ready,
        display_boundary_ready,
        presentation_ready,
        stage_sequence_ready,
        no_gpu_blockers,
        output_texture_available,
        external_texture_registered,
    }
}

/// Cumulative color-stage counters observed in viewer GPU-output diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputStageCounts {
    /// Total color stages scheduled.
    pub total_stages: u64,
    /// Upload stages scheduled.
    pub upload_stages: u64,
    /// Native GPU color stages scheduled.
    pub gpu_color_stages: u64,
    /// Readback stages scheduled.
    pub readback_stages: u64,
    /// Total GPU blockers.
    pub gpu_blockers: u64,
    /// Shader module preparation blockers.
    pub gpu_shader_module_blockers: u64,
    /// OCIO resource bind group preparation blockers.
    pub gpu_ocio_resource_blockers: u64,
    /// Fullscreen wrapper preparation blockers.
    pub gpu_wrapper_blockers: u64,
    /// Render pipeline preparation blockers.
    pub gpu_render_pipeline_blockers: u64,
    /// Pixels processed by recorded stages.
    pub pixels: u64,
}

impl ViewerGpuOutputStageCounts {
    fn merge_max(&mut self, other: Self) {
        self.total_stages = self.total_stages.max(other.total_stages);
        self.upload_stages = self.upload_stages.max(other.upload_stages);
        self.gpu_color_stages = self.gpu_color_stages.max(other.gpu_color_stages);
        self.readback_stages = self.readback_stages.max(other.readback_stages);
        self.gpu_blockers = self.gpu_blockers.max(other.gpu_blockers);
        self.gpu_shader_module_blockers =
            self.gpu_shader_module_blockers.max(other.gpu_shader_module_blockers);
        self.gpu_ocio_resource_blockers =
            self.gpu_ocio_resource_blockers.max(other.gpu_ocio_resource_blockers);
        self.gpu_wrapper_blockers = self.gpu_wrapper_blockers.max(other.gpu_wrapper_blockers);
        self.gpu_render_pipeline_blockers =
            self.gpu_render_pipeline_blockers.max(other.gpu_render_pipeline_blockers);
        self.pixels = self.pixels.max(other.pixels);
    }
}

impl From<RenderGpuOutputStageDiagnosticsReport> for ViewerGpuOutputStageCounts {
    fn from(report: RenderGpuOutputStageDiagnosticsReport) -> Self {
        Self {
            total_stages: report.total_stages,
            upload_stages: report.upload_stages,
            gpu_color_stages: report.gpu_color_stages,
            readback_stages: report.readback_stages,
            gpu_blockers: report.gpu_blockers,
            gpu_shader_module_blockers: report.gpu_blocker_breakdown.shader_module_not_prepared,
            gpu_ocio_resource_blockers: report
                .gpu_blocker_breakdown
                .ocio_resource_bind_group_not_prepared,
            gpu_wrapper_blockers: report.gpu_blocker_breakdown.fullscreen_wrapper_not_prepared,
            gpu_render_pipeline_blockers: report.gpu_blocker_breakdown.render_pipeline_not_prepared,
            pixels: report.stage_pixels,
        }
    }
}

/// Health-status counts for viewer GPU-output attempts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputHealthCounts {
    /// No invocation status count.
    pub no_invocation: u64,
    /// Waiting status count.
    pub waiting: u64,
    /// Blocked status count.
    pub blocked: u64,
    /// Failed status count.
    pub failed: u64,
    /// Rejected status count.
    pub rejected: u64,
    /// Degraded status count.
    pub degraded: u64,
    /// Ready status count.
    pub ready: u64,
}

/// Counts replayed from viewer GPU-output display issue summaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputDisplayIssueCounts {
    /// Total records carrying a display issue summary.
    pub total: u64,
    /// HDR output requested on a surface that cannot prove HDR presentation.
    pub hdr_output_requires_hdr_surface: u64,
    /// Output color space requires a different surface color space.
    pub output_color_space_requires_surface_color_space: u64,
    /// Surface reconfiguration is blocked by the viewer/UI payload contract.
    pub reconfigure_blocked_by_payload: u64,
    /// Requested output is not a presentation color space.
    pub unsupported_presentation_intent: u64,
    /// No supported surface contract exists for the requested output.
    pub unsupported_surface_contract: u64,
    /// OS display profile is unsupported on this platform.
    pub os_display_profile_unsupported: u64,
    /// Records whose display issue includes a payload blocker.
    pub payload_blockers: u64,
    /// Unknown reason strings from newer diagnostics.
    pub unknown: u64,
}

/// Counts replayed from viewer GPU-output display-contract refresh events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputDisplayContractRefreshCounts {
    /// Total refresh events replayed from the JSONL stream.
    pub total: u64,
    /// Refreshes caused by surface resize.
    pub resize: u64,
    /// Refreshes caused by scale-factor changes.
    pub scale_factor_changed: u64,
    /// Refreshes caused by window monitor changes/moves.
    pub window_moved: u64,
    /// Refreshes that rebuilt the frame renderer.
    pub renderer_rebuilt: u64,
    /// Refreshes that changed the active display target.
    pub display_target_changed: u64,
    /// Refreshes that changed the active surface format.
    pub surface_format_changed: u64,
    /// Refreshes that changed the active surface color space.
    pub surface_color_space_changed: u64,
    /// Refreshes that changed the active HDR mode.
    pub surface_hdr_mode_changed: u64,
    /// Refreshes that changed tone-map headroom evidence.
    pub display_tone_map_headroom_changed: u64,
    /// Refreshes that changed the available surface format set.
    pub available_surface_formats_changed: u64,
    /// Refreshes that changed per-format color-space capabilities.
    pub format_color_spaces_changed: u64,
    /// Refreshes that changed available present modes.
    pub present_modes_changed: u64,
    /// Refreshes that changed available alpha modes.
    pub alpha_modes_changed: u64,
}

/// Counts replayed from display issues that followed a specific refresh event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputDisplayIssueRefreshCorrelationCounts {
    /// Display issues carrying a correlated preceding refresh event.
    pub total: u64,
    /// Display issues correlated to a resize refresh.
    pub after_resize: u64,
    /// Display issues correlated to a scale-factor change refresh.
    pub after_scale_factor_changed: u64,
    /// Display issues correlated to a window-move refresh.
    pub after_window_moved: u64,
}

impl ViewerGpuOutputDisplayContractRefreshCounts {
    fn record(&mut self, refresh: &ViewerGpuOutputDisplayContractRefreshEvent) {
        self.total = self.total.saturating_add(1);
        match refresh.reason.as_str() {
            "Resize" => self.resize = self.resize.saturating_add(1),
            "ScaleFactorChanged" => {
                self.scale_factor_changed = self.scale_factor_changed.saturating_add(1);
            }
            "WindowMoved" => self.window_moved = self.window_moved.saturating_add(1),
            _ => {}
        }
        if refresh.renderer_rebuilt {
            self.renderer_rebuilt = self.renderer_rebuilt.saturating_add(1);
        }
        if refresh.display_target_changed {
            self.display_target_changed = self.display_target_changed.saturating_add(1);
        }
        if refresh.surface_format_changed {
            self.surface_format_changed = self.surface_format_changed.saturating_add(1);
        }
        if refresh.surface_color_space_changed {
            self.surface_color_space_changed = self.surface_color_space_changed.saturating_add(1);
        }
        if refresh.surface_hdr_mode_changed {
            self.surface_hdr_mode_changed = self.surface_hdr_mode_changed.saturating_add(1);
        }
        if refresh.display_tone_map_headroom_changed {
            self.display_tone_map_headroom_changed =
                self.display_tone_map_headroom_changed.saturating_add(1);
        }
        if refresh.available_surface_formats_changed {
            self.available_surface_formats_changed =
                self.available_surface_formats_changed.saturating_add(1);
        }
        if refresh.format_color_spaces_changed {
            self.format_color_spaces_changed = self.format_color_spaces_changed.saturating_add(1);
        }
        if refresh.present_modes_changed {
            self.present_modes_changed = self.present_modes_changed.saturating_add(1);
        }
        if refresh.alpha_modes_changed {
            self.alpha_modes_changed = self.alpha_modes_changed.saturating_add(1);
        }
    }
}

impl ViewerGpuOutputDisplayIssueRefreshCorrelationCounts {
    fn record(&mut self, issue: &ViewerGpuOutputDisplayIssueSummary) {
        let Some(refresh) = issue.preceding_display_contract_refresh.as_ref() else {
            return;
        };
        self.total = self.total.saturating_add(1);
        match refresh.reason.as_str() {
            "Resize" => self.after_resize = self.after_resize.saturating_add(1),
            "ScaleFactorChanged" => {
                self.after_scale_factor_changed = self.after_scale_factor_changed.saturating_add(1);
            }
            "WindowMoved" => {
                self.after_window_moved = self.after_window_moved.saturating_add(1);
            }
            _ => {}
        }
    }
}

impl ViewerGpuOutputDisplayIssueCounts {
    fn record(&mut self, issue: &ViewerGpuOutputDisplayIssueSummary) {
        self.total = self.total.saturating_add(1);
        match issue.reason.as_str() {
            "HdrOutputRequiresHdrSurface" => {
                self.hdr_output_requires_hdr_surface =
                    self.hdr_output_requires_hdr_surface.saturating_add(1);
            }
            "OutputColorSpaceRequiresSurfaceColorSpace" => {
                self.output_color_space_requires_surface_color_space =
                    self.output_color_space_requires_surface_color_space.saturating_add(1);
            }
            "ReconfigureBlockedByPayload" => {
                self.reconfigure_blocked_by_payload =
                    self.reconfigure_blocked_by_payload.saturating_add(1);
            }
            "UnsupportedPresentationIntent" => {
                self.unsupported_presentation_intent =
                    self.unsupported_presentation_intent.saturating_add(1);
            }
            "UnsupportedSurfaceContract" => {
                self.unsupported_surface_contract =
                    self.unsupported_surface_contract.saturating_add(1);
            }
            "OsDisplayProfileUnsupported" => {
                self.os_display_profile_unsupported =
                    self.os_display_profile_unsupported.saturating_add(1);
            }
            _ => {
                self.unknown = self.unknown.saturating_add(1);
            }
        }
        if issue.payload_blocker.is_some() {
            self.payload_blockers = self.payload_blockers.saturating_add(1);
        }
    }
}

/// Display issue summary consumed from viewer GPU-output JSONL records.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputDisplayIssueSummary {
    /// Stable display issue reason emitted by the app window.
    pub reason: String,
    /// Output color space requested by the viewer boundary.
    pub output_color_space: Option<String>,
    /// Most recent display-contract refresh correlated to this issue, when present.
    pub preceding_display_contract_refresh: Option<ViewerGpuOutputDisplayContractRefreshEvent>,
    /// Display target active when the issue was recorded.
    pub display_target: Option<ViewerGpuOutputDisplayTarget>,
    /// Current surface format, when presentation is already configured.
    pub current_surface_format: Option<String>,
    /// Current surface color space, when presentation is already configured.
    pub current_surface_color_space: Option<String>,
    /// Current surface encoding, when presentation is already configured.
    pub current_surface_encoding: Option<String>,
    /// Selected surface format, when the blocker is tied to the current swapchain choice.
    pub selected_surface_format: Option<String>,
    /// Selected surface color space, when the blocker is tied to the current swapchain choice.
    pub selected_surface_color_space: Option<String>,
    /// Selected surface encoding, when the blocker is tied to the current swapchain choice.
    pub selected_surface_encoding: Option<String>,
    /// Surface HDR mode active when the issue was recorded.
    pub surface_hdr_mode: Option<String>,
    /// Desired surface format for satisfying the requested output.
    pub desired_surface_format: Option<String>,
    /// Desired surface color space for satisfying the requested output.
    pub desired_surface_color_space: Option<String>,
    /// Desired surface encoding for satisfying the requested output.
    pub desired_surface_encoding: Option<String>,
    /// Desired surface HDR mode for satisfying the requested output.
    pub desired_surface_hdr_mode: Option<String>,
    /// Viewer/UI payload blocker, when the surface could be promoted but the
    /// frame payload path cannot yet present that contract.
    pub payload_blocker: Option<String>,
    /// Number of supported surface color spaces on the selected format, when relevant.
    pub supported_surface_color_space_count: Option<u8>,
    /// Whether the target surface color space is supported on the selected format.
    pub target_surface_color_space_supported: Option<bool>,
}

/// Stable monitor/display-target fingerprint consumed from viewer GPU-output diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputDisplayTarget {
    /// Display name reported by the OS, when available.
    pub name: Option<String>,
    /// Display origin in the virtual desktop space.
    pub position: (i32, i32),
    /// Physical display size in pixels.
    pub physical_size: (u32, u32),
    /// Display scale factor in parts per million.
    pub scale_factor_ppm: u32,
    /// Display refresh rate in millihertz, when available.
    pub refresh_rate_millihertz: Option<u32>,
}

/// Display-contract refresh event consumed from viewer GPU-output diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputDisplayContractRefreshEvent {
    /// Stable refresh reason emitted by the app window.
    pub reason: String,
    /// The previous display-output contract snapshot.
    pub previous: ViewerGpuOutputDisplayContractSnapshot,
    /// The next display-output contract snapshot.
    pub next: ViewerGpuOutputDisplayContractSnapshot,
    /// Whether the refresh rebuilt the frame renderer.
    pub renderer_rebuilt: bool,
    /// Whether the refresh changed the active display target.
    pub display_target_changed: bool,
    /// Whether the refresh changed the active surface format.
    pub surface_format_changed: bool,
    /// Whether the refresh changed the active surface color space.
    pub surface_color_space_changed: bool,
    /// Whether the refresh changed the active HDR mode.
    pub surface_hdr_mode_changed: bool,
    /// Whether the refresh changed reported display tone-map headroom.
    #[serde(default)]
    pub display_tone_map_headroom_changed: bool,
    /// Whether the refresh changed the available surface format set.
    #[serde(default)]
    pub available_surface_formats_changed: bool,
    /// Whether the refresh changed per-format color-space capabilities.
    #[serde(default)]
    pub format_color_spaces_changed: bool,
    /// Whether the refresh changed the available present modes.
    #[serde(default)]
    pub present_modes_changed: bool,
    /// Whether the refresh changed the available alpha modes.
    #[serde(default)]
    pub alpha_modes_changed: bool,
}

/// Display-output contract snapshot consumed from viewer GPU-output diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputDisplayContractSnapshot {
    /// Display target active for the snapshot.
    pub display_target: ViewerGpuOutputDisplayTarget,
    /// Surface format chosen for the snapshot.
    pub surface_format: String,
    /// Surface color space chosen for the snapshot.
    pub surface_color_space: String,
    /// Surface encoding chosen for the snapshot.
    pub surface_encoding: String,
    /// Surface HDR mode chosen for the snapshot.
    pub surface_hdr_mode: String,
    /// Reported tone-map headroom in parts per million, when available.
    #[serde(default)]
    pub display_tone_map_headroom_ppm: Option<u32>,
    /// Available surface formats for the snapshot.
    #[serde(default)]
    pub available_surface_formats: Vec<String>,
    /// Per-format surface color-space capabilities for the snapshot.
    #[serde(default)]
    pub format_color_spaces: Vec<ViewerGpuOutputSurfaceFormatColorSpaces>,
    /// Available present modes for the snapshot.
    #[serde(default)]
    pub present_modes: Vec<String>,
    /// Available alpha modes for the snapshot.
    #[serde(default)]
    pub alpha_modes: Vec<String>,
}

/// Per-format surface color-space capability snapshot consumed from diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputSurfaceFormatColorSpaces {
    /// Surface format.
    pub format: String,
    /// Supports SRGB presentation.
    pub srgb: bool,
    /// Supports extended linear SRGB presentation.
    pub extended_srgb_linear: bool,
    /// Supports Display P3 presentation.
    pub display_p3: bool,
    /// Supports BT.2100 PQ presentation.
    pub bt2100_pq: bool,
    /// Supports BT.2100 HLG presentation.
    pub bt2100_hlg: bool,
    /// Supports extended SRGB presentation.
    pub extended_srgb: bool,
    /// Supports extended Display P3 presentation.
    pub extended_display_p3: bool,
}

impl ViewerGpuOutputHealthCounts {
    pub(crate) fn record(&mut self, status: ViewerGpuOutputHealthStatus) {
        match status {
            ViewerGpuOutputHealthStatus::NoInvocation => {
                self.no_invocation = self.no_invocation.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Waiting => {
                self.waiting = self.waiting.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Blocked => {
                self.blocked = self.blocked.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Failed => {
                self.failed = self.failed.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Rejected => {
                self.rejected = self.rejected.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Degraded => {
                self.degraded = self.degraded.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Ready => {
                self.ready = self.ready.saturating_add(1);
            }
        }
    }
}

/// Viewer GPU-output health status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum ViewerGpuOutputHealthStatus {
    /// No invocation has been observed.
    #[default]
    NoInvocation,
    /// The viewer output path is waiting for app state, media, or a valid key.
    Waiting,
    /// Display contract blocked the output request.
    Blocked,
    /// Recording or output texture lookup failed.
    Failed,
    /// External texture registration was rejected.
    Rejected,
    /// Output registered, but presentation or native-boundary health is degraded.
    Degraded,
    /// Output registered through the native GPU boundary and is presentation-ready.
    Ready,
}

/// Frame context associated with a viewer GPU-output diagnostic record.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ViewerGpuOutputFrameContext {
    /// Sequence id that produced the frame.
    pub sequence_id: String,
    /// Timeline frame number.
    pub frame: i64,
    /// Preview frame width.
    pub width: u32,
    /// Preview frame height.
    pub height: u32,
    /// External texture key used by the app UI renderer.
    pub external_texture_key: String,
    /// Output target.
    pub output_target: String,
    /// Output color space.
    pub output_color_space: String,
    /// Whether tone mapping was requested.
    pub tone_map: bool,
    /// Preview candidate identifier associated with this ready record, when available.
    #[serde(default)]
    pub preview_candidate_id: Option<u64>,
    /// Preview candidate state for this record.
    #[serde(default)]
    pub preview_candidate_state: Option<ViewerGpuOutputPreviewCandidateState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum ViewerGpuOutputPreviewCandidateState {
    Current,
    Loading,
    Unavailable,
    Ready,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_stage_diagnostics() -> RenderColorStageDiagnostics {
        RenderColorStageDiagnostics {
            total_stages: 2,
            upload_stages: 1,
            gpu_color_stages: 1,
            ..RenderColorStageDiagnostics::default()
        }
    }

    #[test]
    fn shared_attempt_classifier_requires_registration_presentation_and_native_boundary() {
        let ready = classify_viewer_gpu_output_health(
            Some(ViewerGpuOutputAttemptOutcome::Registered),
            true,
            Some(ready_stage_diagnostics()),
        );
        assert_eq!(ready.status, ViewerGpuOutputHealthStatus::Ready);
        assert!(ready.viewer_output_ready);

        let presentation_not_ready = classify_viewer_gpu_output_health(
            Some(ViewerGpuOutputAttemptOutcome::Registered),
            false,
            Some(ready_stage_diagnostics()),
        );
        assert_eq!(
            presentation_not_ready.status,
            ViewerGpuOutputHealthStatus::Degraded
        );
        assert!(presentation_not_ready.native_gpu_boundary_ready);
        assert!(!presentation_not_ready.viewer_output_ready);

        let no_stage_evidence = classify_viewer_gpu_output_health(
            Some(ViewerGpuOutputAttemptOutcome::Registered),
            true,
            None,
        );
        assert_eq!(
            no_stage_evidence.status,
            ViewerGpuOutputHealthStatus::Degraded
        );
        assert!(!no_stage_evidence.native_gpu_boundary_ready);
    }

    #[test]
    fn shared_attempt_classifier_preserves_terminal_failure_class() {
        for (outcome, expected) in [
            (
                ViewerGpuOutputAttemptOutcome::DisplayContractBlocked,
                ViewerGpuOutputHealthStatus::Blocked,
            ),
            (
                ViewerGpuOutputAttemptOutcome::RecordFailed,
                ViewerGpuOutputHealthStatus::Failed,
            ),
            (
                ViewerGpuOutputAttemptOutcome::OutputTextureMissing,
                ViewerGpuOutputHealthStatus::Failed,
            ),
            (
                ViewerGpuOutputAttemptOutcome::ExternalFrameRejected,
                ViewerGpuOutputHealthStatus::Rejected,
            ),
        ] {
            let health = classify_viewer_gpu_output_health(
                Some(outcome),
                true,
                Some(ready_stage_diagnostics()),
            );
            assert_eq!(health.status, expected);
        }
    }

    fn viewer_gpu_output_budget_allow_missing() -> ViewerGpuOutputBudget {
        ViewerGpuOutputBudget {
            max_missing_runtime_reports: u64::MAX,
            max_missing_stage_reports: u64::MAX,
            max_ready_records_missing_preview_candidate_context: u64::MAX,
            max_preview_candidate_id_regressions: u64::MAX,
            ..Default::default()
        }
    }

    #[test]
    fn budget_passes_clean_ready_stream() {
        let jsonl = r#"
{"health":{"status":"Waiting"},"health_counts":{"no_invocation":0,"waiting":1,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":0}}
{"health":{"status":"Ready"},"health_counts":{"no_invocation":0,"waiting":1,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"last_frame_context":{"sequence_id":"seq","frame":7,"width":1920,"height":1080,"external_texture_key":"key","output_target":"Display","output_color_space":"Srgb","tone_map":false}}
"#;

        let summary = evaluate_jsonl(jsonl, &viewer_gpu_output_budget_allow_missing())
            .expect("budget summary");

        assert!(summary.passed);
        assert_eq!(summary.records, 2);
        assert_eq!(summary.counts.waiting, 1);
        assert_eq!(summary.counts.ready, 1);
        assert_eq!(summary.color_rejections, 0);
        assert_eq!(
            summary.media_issues,
            VideoColorDiagnosticIssueAggregate::default()
        );
        assert_eq!(
            summary.display_issues,
            ViewerGpuOutputDisplayIssueCounts::default()
        );
        assert!(summary.reported_counts_match_replay);
        assert_eq!(summary.reported_counts, Some(summary.counts));
        assert!(summary.count_mismatches.is_empty());
        assert_eq!(
            summary.last_status,
            Some(ViewerGpuOutputHealthStatus::Ready)
        );
        assert_eq!(summary.last_frame_context.expect("frame context").frame, 7);
    }

    #[test]
    fn budget_fails_when_runtime_and_stage_reports_missing_by_default() {
        let jsonl = r#"
{"health":{"status":"Ready"}}
"#;

        let summary =
            evaluate_jsonl(jsonl, &ViewerGpuOutputBudget::default()).expect("budget summary");

        assert!(!summary.passed);
        assert!(summary
            .failures
            .iter()
            .any(|failure| failure.metric == "missing_runtime_reports"));
        assert!(summary.failures.iter().any(|failure| failure.metric == "missing_stage_reports"));
        assert_eq!(summary.missing_runtime_reports, 1);
        assert_eq!(summary.missing_stage_reports, 1);
    }

    #[test]
    fn budget_fails_when_ready_record_missing_preview_candidate_context() {
        let jsonl = r#"
{"health":{"status":"Ready"},"health_counts":{"no_invocation":0,"waiting":0,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"last_frame_context":{"sequence_id":"seq","frame":10,"width":1920,"height":1080,"external_texture_key":"key","output_target":"Display","output_color_space":"Srgb","tone_map":false}}
"#;

        let summary = evaluate_jsonl(
            jsonl,
            &ViewerGpuOutputBudget {
                max_missing_runtime_reports: u64::MAX,
                max_missing_stage_reports: u64::MAX,
                ..ViewerGpuOutputBudget::default()
            },
        )
        .expect("budget summary");

        assert!(!summary.passed);
        assert!(summary
            .failures
            .iter()
            .any(|failure| failure.metric == "ready_records_missing_preview_candidate_context"));
        assert_eq!(summary.ready_records_missing_preview_candidate_context, 1);
    }

    #[test]
    fn budget_fails_when_preview_candidate_id_regresses() {
        let jsonl = r#"
{"health":{"status":"Ready"},"health_counts":{"no_invocation":0,"waiting":0,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"last_frame_context":{"sequence_id":"seq","frame":10,"width":1920,"height":1080,"external_texture_key":"key-1","output_target":"Display","output_color_space":"Srgb","tone_map":false,"preview_candidate_id":2}}
{"health":{"status":"Ready"},"health_counts":{"no_invocation":0,"waiting":0,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":2},"last_frame_context":{"sequence_id":"seq","frame":11,"width":1920,"height":1080,"external_texture_key":"key-2","output_target":"Display","output_color_space":"Srgb","tone_map":false,"preview_candidate_id":1}}
"#;

        let summary = evaluate_jsonl(
            jsonl,
            &ViewerGpuOutputBudget {
                max_missing_runtime_reports: u64::MAX,
                max_missing_stage_reports: u64::MAX,
                ..ViewerGpuOutputBudget::default()
            },
        )
        .expect("budget summary");

        assert!(!summary.passed);
        assert!(summary
            .failures
            .iter()
            .any(|failure| failure.metric == "preview_candidate_id_regressions"));
        assert_eq!(summary.preview_candidate_id_regressions, 1);
        assert_eq!(summary.last_preview_candidate_id, Some(1));
    }

    #[test]
    fn budget_replays_stage_and_health_flags() {
        let jsonl = r#"
{"health":{"status":"Ready","viewer_output_ready":true,"native_gpu_boundary_ready":true,"display_boundary_ready":true,"presentation_ready":true,"stage_sequence_ready":true,"no_gpu_blockers":true,"output_texture_available":true,"external_texture_registered":true},"health_counts":{"no_invocation":0,"waiting":0,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"stage_total_stages":99,"stage_gpu_color_stages":99,"stage_pixels":1,"accumulated_stage_report":{"total_stages":2,"upload_stages":0,"gpu_color_stages":1,"readback_stages":0,"gpu_blockers":0,"gpu_blocker_breakdown":{"shader_module_not_prepared":0,"ocio_resource_bind_group_not_prepared":0,"fullscreen_wrapper_not_prepared":0,"render_pipeline_not_prepared":0,"ocio_config_not_loaded":0,"ocio_processor_unavailable":0,"ocio_gpu_shader_extraction_failed":0},"stage_pixels":2073600},"runtime_report":{"shader_cache_entries":1,"shader_cache_hits":2,"shader_cache_misses":3,"shader_cache_extraction_failures":0,"backend_prep_resource_entries":4,"static_pipeline_entries":10,"static_pipeline_hits":11,"static_pipeline_misses":12,"backend_object_entries":5,"backend_object_hits":6,"backend_object_misses":7,"backend_object_failures":0,"frame_table_entries":8,"next_frame_id":9,"wrapper_input_bind_group_creations":0,"wrapper_input_bind_group_cache_hits":0}}
"#;

        let summary = evaluate_jsonl(jsonl, &viewer_gpu_output_budget_allow_missing())
            .expect("budget summary");

        assert!(summary.passed);
        assert_eq!(
            summary.stage,
            ViewerGpuOutputStageCounts {
                total_stages: 2,
                gpu_color_stages: 1,
                pixels: 2_073_600,
                ..ViewerGpuOutputStageCounts::default()
            }
        );
        assert_eq!(
            summary.last_runtime_report,
            Some(RenderGpuOutputRuntimeDiagnosticsReport {
                shader_cache_entries: 1,
                shader_cache_hits: 2,
                shader_cache_misses: 3,
                shader_cache_extraction_failures: 0,
                backend_prep_resource_entries: 4,
                static_pipeline_entries: 10,
                static_pipeline_hits: 11,
                static_pipeline_misses: 12,
                backend_object_entries: 5,
                backend_object_hits: 6,
                backend_object_misses: 7,
                backend_object_failures: 0,
                wrapper_input_bind_group_creations: 0,
                wrapper_input_bind_group_cache_hits: 0,
                frame_table_entries: 8,
                next_frame_id: 9,
            })
        );
        assert_eq!(
            summary.last_health,
            Some(ViewerGpuOutputHealthSummary {
                status: ViewerGpuOutputHealthStatus::Ready,
                viewer_output_ready: true,
                native_gpu_boundary_ready: true,
                display_boundary_ready: true,
                presentation_ready: true,
                stage_sequence_ready: true,
                no_gpu_blockers: true,
                output_texture_available: true,
                external_texture_registered: true,
            })
        );
    }

    #[test]
    fn health_report_surfaces_backend_runtime_failures() {
        let jsonl = r#"
{"health":{"status":"Failed","viewer_output_ready":false,"native_gpu_boundary_ready":false,"display_boundary_ready":true,"presentation_ready":true,"stage_sequence_ready":true,"no_gpu_blockers":true,"output_texture_available":true,"external_texture_registered":false},"health_counts":{"no_invocation":0,"waiting":0,"blocked":0,"failed":1,"rejected":0,"degraded":0,"ready":0},"accumulated_stage_report":{"total_stages":2,"upload_stages":0,"gpu_color_stages":1,"readback_stages":0,"gpu_blockers":0,"gpu_blocker_breakdown":{"shader_module_not_prepared":0,"ocio_resource_bind_group_not_prepared":0,"fullscreen_wrapper_not_prepared":0,"render_pipeline_not_prepared":0,"ocio_config_not_loaded":0,"ocio_processor_unavailable":0,"ocio_gpu_shader_extraction_failed":0},"stage_pixels":4096},"runtime_report":{"shader_cache_entries":1,"shader_cache_hits":0,"shader_cache_misses":1,"shader_cache_extraction_failures":1,"backend_prep_resource_entries":1,"static_pipeline_entries":1,"static_pipeline_hits":0,"static_pipeline_misses":1,"backend_object_entries":1,"backend_object_hits":0,"backend_object_misses":1,"backend_object_failures":1,"frame_table_entries":2,"next_frame_id":3,"wrapper_input_bind_group_creations":0,"wrapper_input_bind_group_cache_hits":0}}
"#;

        let summary = evaluate_jsonl(jsonl, &viewer_gpu_output_budget_allow_missing())
            .expect("budget summary");
        let report = build_health_report(summary, "display-baseline", None);

        assert_eq!(report.verdict, ViewerGpuOutputHealthVerdict::Fail);
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "shader_cache_extraction_failures"
                && check.severity == ViewerGpuOutputHealthSeverity::Fail));
        assert!(
            report.checks.iter().any(|check| check.code == "backend_object_failures"
                && check.severity == ViewerGpuOutputHealthSeverity::Fail)
        );
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "shader_cache_extraction_failed"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "backend_object_runtime_failed"));
        assert_eq!(
            report.evidence.last_runtime_report,
            Some(RenderGpuOutputRuntimeDiagnosticsReport {
                shader_cache_entries: 1,
                shader_cache_hits: 0,
                shader_cache_misses: 1,
                shader_cache_extraction_failures: 1,
                backend_prep_resource_entries: 1,
                static_pipeline_entries: 1,
                static_pipeline_hits: 0,
                static_pipeline_misses: 1,
                backend_object_entries: 1,
                backend_object_hits: 0,
                backend_object_misses: 1,
                backend_object_failures: 1,
                wrapper_input_bind_group_creations: 0,
                wrapper_input_bind_group_cache_hits: 0,
                frame_table_entries: 2,
                next_frame_id: 3,
            })
        );
    }

    #[test]
    fn health_report_passes_clean_native_gpu_stream() {
        let jsonl = r#"
{"health":{"status":"Ready","viewer_output_ready":true,"native_gpu_boundary_ready":true,"display_boundary_ready":true,"presentation_ready":true,"stage_sequence_ready":true,"no_gpu_blockers":true,"output_texture_available":true,"external_texture_registered":true},"health_counts":{"no_invocation":0,"waiting":0,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"stage_total_stages":1,"stage_gpu_color_stages":1,"stage_pixels":4096}
"#;

        let summary = evaluate_jsonl(jsonl, &viewer_gpu_output_budget_allow_missing())
            .expect("budget summary");
        let report = build_health_report(
            summary,
            "display-baseline",
            Some("target/perf/viewer-gpu-output.jsonl".to_owned()),
        );

        assert_eq!(
            report.schema_version,
            VIEWER_GPU_OUTPUT_HEALTH_REPORT_SCHEMA_VERSION
        );
        assert_eq!(report.profile, "display-baseline");
        assert_eq!(
            report.source_path,
            Some("target/perf/viewer-gpu-output.jsonl".to_owned())
        );
        assert_eq!(report.verdict, ViewerGpuOutputHealthVerdict::Pass);
        assert!(report.root_causes.is_empty());
        assert!(report.actions.is_empty());
        assert!(
            report.checks.iter().any(|check| check.code == "gpu_color_stages_present"
                && check.severity == ViewerGpuOutputHealthSeverity::Pass)
        );
    }

    #[test]
    fn health_report_reports_gpu_and_display_root_causes() {
        let jsonl = r#"
{"health":{"status":"Blocked","viewer_output_ready":false,"native_gpu_boundary_ready":false,"display_boundary_ready":false,"presentation_ready":false,"stage_sequence_ready":true,"no_gpu_blockers":false,"output_texture_available":true,"external_texture_registered":false},"health_counts":{"no_invocation":0,"waiting":0,"blocked":1,"failed":0,"rejected":0,"degraded":0,"ready":0},"stage_total_stages":3,"stage_upload_stages":1,"stage_gpu_color_stages":1,"stage_readback_stages":1,"stage_gpu_blockers":1,"stage_gpu_render_pipeline_blockers":1,"display_issue_summary":{"reason":"HdrOutputRequiresHdrSurface","output_color_space":"Rec2100Pq","payload_blocker":null}}
"#;

        let summary = evaluate_jsonl(jsonl, &viewer_gpu_output_budget_allow_missing())
            .expect("budget summary");
        let report = build_health_report(summary, "display-baseline", None);

        assert_eq!(report.verdict, ViewerGpuOutputHealthVerdict::Fail);
        assert!(report.root_causes.iter().any(|root| root.code == "viewer_output_not_healthy"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "hdr_output_requires_hdr_surface"));
        assert!(report.root_causes.iter().any(|root| root.code == "gpu_color_stage_blocked"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "cpu_gpu_transfer_stage_present"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_gpu_blocker_breakdown"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_hdr_surface_contract"));
    }

    #[test]
    fn health_report_splits_display_drift_root_causes() {
        let jsonl = r#"
{"health":{"status":"Ready","viewer_output_ready":true,"native_gpu_boundary_ready":true,"display_boundary_ready":true,"presentation_ready":true,"stage_sequence_ready":true,"no_gpu_blockers":true,"output_texture_available":true,"external_texture_registered":true},"health_counts":{"no_invocation":0,"waiting":0,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"display_issue_summary":{"reason":"OutputColorSpaceRequiresSurfaceColorSpace","output_color_space":"DisplayP3","preceding_display_contract_refresh":{"reason":"WindowMoved","previous":{"display_target":{"name":"Panel A","position":[0,0],"physical_size":[2560,1440],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"surface_format":"Bgra8UnormSrgb","surface_color_space":"Srgb","surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","display_tone_map_headroom_ppm":500000,"available_surface_formats":["Bgra8UnormSrgb"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo"],"alpha_modes":["Auto"]},"next":{"display_target":{"name":"HDR Monitor","position":[2560,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":120000},"surface_format":"Rgba16Float","surface_color_space":"DisplayP3","surface_encoding":"Srgb","surface_hdr_mode":"HdrPq","display_tone_map_headroom_ppm":850000,"available_surface_formats":["Bgra8UnormSrgb","Rgba16Float"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false},{"format":"Rgba16Float","srgb":false,"extended_srgb_linear":false,"display_p3":true,"bt2100_pq":true,"bt2100_hlg":true,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo","Immediate"],"alpha_modes":["Auto","Opaque"]},"renderer_rebuilt":true,"display_target_changed":true,"surface_format_changed":true,"surface_color_space_changed":true,"surface_hdr_mode_changed":true,"display_tone_map_headroom_changed":true,"available_surface_formats_changed":true,"format_color_spaces_changed":true,"present_modes_changed":true,"alpha_modes_changed":true},"display_target":{"name":"HDR Monitor","position":[2560,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":120000},"current_surface_format":"Rgba16Float","current_surface_color_space":"DisplayP3","current_surface_encoding":"Srgb","selected_surface_format":"Rgba16Float","selected_surface_color_space":"DisplayP3","selected_surface_encoding":"Srgb","surface_hdr_mode":"HdrPq","desired_surface_format":"Rgba16Float","desired_surface_color_space":"DisplayP3","desired_surface_encoding":"Srgb","desired_surface_hdr_mode":"HdrPq","payload_blocker":null,"supported_surface_color_space_count":2,"target_surface_color_space_supported":true},"last_display_contract_refresh":{"reason":"WindowMoved","previous":{"display_target":{"name":"Panel A","position":[0,0],"physical_size":[2560,1440],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"surface_format":"Bgra8UnormSrgb","surface_color_space":"Srgb","surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","display_tone_map_headroom_ppm":500000,"available_surface_formats":["Bgra8UnormSrgb"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo"],"alpha_modes":["Auto"]},"next":{"display_target":{"name":"HDR Monitor","position":[2560,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":120000},"surface_format":"Rgba16Float","surface_color_space":"DisplayP3","surface_encoding":"Srgb","surface_hdr_mode":"HdrPq","display_tone_map_headroom_ppm":850000,"available_surface_formats":["Bgra8UnormSrgb","Rgba16Float"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false},{"format":"Rgba16Float","srgb":false,"extended_srgb_linear":false,"display_p3":true,"bt2100_pq":true,"bt2100_hlg":true,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo","Immediate"],"alpha_modes":["Auto","Opaque"]},"renderer_rebuilt":true,"display_target_changed":true,"surface_format_changed":true,"surface_color_space_changed":true,"surface_hdr_mode_changed":true,"display_tone_map_headroom_changed":true,"available_surface_formats_changed":true,"format_color_spaces_changed":true,"present_modes_changed":true,"alpha_modes_changed":true}}
"#;
        let budget = ViewerGpuOutputBudget {
            max_display_contract_refreshes: 0,
            max_display_issue_refresh_correlations: 0,
            max_display_tone_map_headroom_changes: 0,
            max_available_surface_format_changes: 0,
            max_format_color_space_changes: 0,
            max_present_mode_changes: 0,
            max_alpha_mode_changes: 0,
            max_display_issues: 1,
            max_output_color_space_requires_surface_color_space: 1,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");
        let report = build_health_report(summary, "display-baseline", None);

        assert_eq!(report.verdict, ViewerGpuOutputHealthVerdict::Fail);
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "display_contract_refresh_churn"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "display_issue_correlated_with_contract_refresh"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "display_tone_map_headroom_drift"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "surface_format_capability_drift"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "surface_color_space_capability_drift"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "present_mode_capability_drift"));
        assert!(report.root_causes.iter().any(|root| root.code == "alpha_mode_capability_drift"));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "surface_format_capability_drift"
                && root.evidence.contains("Bgra8UnormSrgb")
                && root.evidence.contains("Rgba16Float")
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "display_tone_map_headroom_drift"
                && root.evidence.contains("previous_headroom_ppm=500000")
                && root.evidence.contains("next_headroom_ppm=850000")
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_display_refresh_history"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_display_issue_correlation"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_surface_color_space_capabilities"));
    }

    #[test]
    fn budget_fails_failed_blocked_and_missing_ready_stream() {
        let jsonl = r#"
{"health":{"status":"Failed"}}
{"health":{"status":"Blocked"}}
"#;

        let summary = evaluate_jsonl(jsonl, &viewer_gpu_output_budget_allow_missing())
            .expect("budget summary");

        assert!(!summary.passed);
        assert_eq!(summary.counts.failed, 1);
        assert_eq!(summary.counts.blocked, 1);
        assert_eq!(
            summary.failures,
            vec![
                ViewerGpuOutputBudgetFailure { metric: "ready", actual: 0, limit: 1 },
                ViewerGpuOutputBudgetFailure { metric: "failed", actual: 1, limit: 0 },
                ViewerGpuOutputBudgetFailure { metric: "blocked", actual: 1, limit: 0 },
            ]
        );
    }

    #[test]
    fn budget_fails_empty_stream_with_record_failure() {
        let summary = evaluate_jsonl("\n\n", &viewer_gpu_output_budget_allow_missing())
            .expect("budget summary");

        assert!(!summary.passed);
        assert_eq!(summary.records, 0);
        assert_eq!(
            summary.failures,
            vec![
                ViewerGpuOutputBudgetFailure { metric: "records", actual: 0, limit: 1 },
                ViewerGpuOutputBudgetFailure { metric: "ready", actual: 0, limit: 1 },
            ]
        );
    }

    #[test]
    fn budget_fails_when_reported_counts_do_not_match_replay() {
        let jsonl = r#"
{"health":{"status":"Ready"},"health_counts":{"no_invocation":0,"waiting":0,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":2}}
"#;

        let summary = evaluate_jsonl(jsonl, &viewer_gpu_output_budget_allow_missing())
            .expect("budget summary");

        assert!(!summary.passed);
        assert!(!summary.reported_counts_match_replay);
        assert_eq!(summary.counts.ready, 1);
        assert_eq!(
            summary.count_mismatches,
            vec![ViewerGpuOutputCountMismatch {
                line: 2,
                replayed: ViewerGpuOutputHealthCounts {
                    ready: 1,
                    ..ViewerGpuOutputHealthCounts::default()
                },
                reported: ViewerGpuOutputHealthCounts {
                    ready: 2,
                    ..ViewerGpuOutputHealthCounts::default()
                },
            }]
        );
        assert_eq!(
            summary.failures,
            vec![ViewerGpuOutputBudgetFailure {
                metric: "health_counts_match_replay",
                actual: 1,
                limit: 0,
            }]
        );
    }

    #[test]
    fn budget_counts_display_issue_reasons() {
        let jsonl = r#"
{"health":{"status":"Blocked"},"display_issue_summary":{"reason":"HdrOutputRequiresHdrSurface","output_color_space":"Rec2100Pq","payload_blocker":null}}
{"health":{"status":"Degraded"},"display_issue_summary":{"reason":"ReconfigureBlockedByPayload","output_color_space":"DisplayP3","payload_blocker":"UiExternalTextureCompositingRequiresSdrSrgb"}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_blocked: 1,
            max_degraded: 1,
            max_display_issues: 2,
            max_hdr_output_requires_hdr_surface: 1,
            max_reconfigure_blocked_by_payload: 1,
            max_display_payload_blockers: 1,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(summary.passed);
        assert_eq!(
            summary.display_issues,
            ViewerGpuOutputDisplayIssueCounts {
                total: 2,
                hdr_output_requires_hdr_surface: 1,
                reconfigure_blocked_by_payload: 1,
                payload_blockers: 1,
                ..ViewerGpuOutputDisplayIssueCounts::default()
            }
        );
        assert_eq!(
            summary.last_display_issue,
            Some(ViewerGpuOutputDisplayIssueSummary {
                reason: "ReconfigureBlockedByPayload".to_owned(),
                output_color_space: Some("DisplayP3".to_owned()),
                preceding_display_contract_refresh: None,
                display_target: None,
                current_surface_format: None,
                current_surface_color_space: None,
                current_surface_encoding: None,
                selected_surface_format: None,
                selected_surface_color_space: None,
                selected_surface_encoding: None,
                surface_hdr_mode: None,
                desired_surface_format: None,
                desired_surface_color_space: None,
                desired_surface_encoding: None,
                desired_surface_hdr_mode: None,
                payload_blocker: Some("UiExternalTextureCompositingRequiresSdrSrgb".to_owned()),
                supported_surface_color_space_count: None,
                target_surface_color_space_supported: None,
            })
        );
    }

    #[test]
    fn budget_preserves_structured_display_issue_evidence() {
        let jsonl = r#"
{"health":{"status":"Blocked"},"display_issue_summary":{"reason":"OutputColorSpaceRequiresSurfaceColorSpace","output_color_space":"DisplayP3","display_target":{"name":"Reference Monitor","position":[1920,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"current_surface_format":null,"current_surface_color_space":null,"current_surface_encoding":null,"selected_surface_format":"Bgra8UnormSrgb","selected_surface_color_space":"Srgb","selected_surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","desired_surface_format":null,"desired_surface_color_space":"DisplayP3","desired_surface_encoding":"Srgb","desired_surface_hdr_mode":"SdrOnly","payload_blocker":null,"supported_surface_color_space_count":2,"target_surface_color_space_supported":true}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_blocked: 1,
            max_display_issues: 1,
            max_output_color_space_requires_surface_color_space: 1,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(summary.passed);
        assert_eq!(
            summary.last_display_issue,
            Some(ViewerGpuOutputDisplayIssueSummary {
                reason: "OutputColorSpaceRequiresSurfaceColorSpace".to_owned(),
                output_color_space: Some("DisplayP3".to_owned()),
                preceding_display_contract_refresh: None,
                display_target: Some(ViewerGpuOutputDisplayTarget {
                    name: Some("Reference Monitor".to_owned()),
                    position: (1920, 0),
                    physical_size: (3840, 2160),
                    scale_factor_ppm: 1_000_000,
                    refresh_rate_millihertz: Some(60_000),
                }),
                current_surface_format: None,
                current_surface_color_space: None,
                current_surface_encoding: None,
                selected_surface_format: Some("Bgra8UnormSrgb".to_owned()),
                selected_surface_color_space: Some("Srgb".to_owned()),
                selected_surface_encoding: Some("Srgb".to_owned()),
                surface_hdr_mode: Some("SdrOnly".to_owned()),
                desired_surface_format: None,
                desired_surface_color_space: Some("DisplayP3".to_owned()),
                desired_surface_encoding: Some("Srgb".to_owned()),
                desired_surface_hdr_mode: Some("SdrOnly".to_owned()),
                payload_blocker: None,
                supported_surface_color_space_count: Some(2),
                target_surface_color_space_supported: Some(true),
            })
        );
    }

    #[test]
    fn budget_correlates_display_issue_with_preceding_refresh() {
        let jsonl = r#"
{"health":{"status":"Blocked"},"display_issue_summary":{"reason":"HdrOutputRequiresHdrSurface","output_color_space":"Rec2100Pq","preceding_display_contract_refresh":{"reason":"WindowMoved","previous":{"display_target":{"name":"Office SDR","position":[0,0],"physical_size":[2560,1440],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"surface_format":"Bgra8UnormSrgb","surface_color_space":"Srgb","surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly"},"next":{"display_target":{"name":"HDR Monitor","position":[2560,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":120000},"surface_format":"Bgra8UnormSrgb","surface_color_space":"Srgb","surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly"},"renderer_rebuilt":false,"display_target_changed":true,"surface_format_changed":false,"surface_color_space_changed":false,"surface_hdr_mode_changed":false},"display_target":{"name":"HDR Monitor","position":[2560,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":120000},"current_surface_format":null,"current_surface_color_space":null,"current_surface_encoding":null,"selected_surface_format":"Bgra8UnormSrgb","selected_surface_color_space":"Srgb","selected_surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","desired_surface_format":null,"desired_surface_color_space":"Bt2100Pq","desired_surface_encoding":"Pq","desired_surface_hdr_mode":"HdrPq","payload_blocker":null,"supported_surface_color_space_count":1,"target_surface_color_space_supported":false}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_blocked: 1,
            max_display_issues: 1,
            max_hdr_output_requires_hdr_surface: 1,
            max_display_issue_refresh_correlations: 1,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(summary.passed);
        assert_eq!(
            summary.display_issue_refresh_correlations,
            ViewerGpuOutputDisplayIssueRefreshCorrelationCounts {
                total: 1,
                after_window_moved: 1,
                ..ViewerGpuOutputDisplayIssueRefreshCorrelationCounts::default()
            }
        );
        assert_eq!(
            summary
                .last_display_issue
                .expect("last display issue")
                .preceding_display_contract_refresh
                .expect("preceding refresh")
                .reason,
            "WindowMoved"
        );
    }

    #[test]
    fn budget_replays_display_contract_refresh_history() {
        let jsonl = r#"
{"health":{"status":"Waiting"},"recent_display_contract_refreshes":[{"reason":"Resize","previous":{"display_target":{"name":"Panel A","position":[0,0],"physical_size":[1920,1080],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"surface_format":"Bgra8UnormSrgb","surface_color_space":"Srgb","surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","display_tone_map_headroom_ppm":null,"available_surface_formats":["Bgra8UnormSrgb"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo"],"alpha_modes":["Auto"]},"next":{"display_target":{"name":"Panel A","position":[0,0],"physical_size":[2560,1440],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"surface_format":"Bgra8UnormSrgb","surface_color_space":"Srgb","surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","display_tone_map_headroom_ppm":null,"available_surface_formats":["Bgra8UnormSrgb"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo"],"alpha_modes":["Auto"]},"renderer_rebuilt":false,"display_target_changed":true,"surface_format_changed":false,"surface_color_space_changed":false,"surface_hdr_mode_changed":false,"display_tone_map_headroom_changed":false,"available_surface_formats_changed":false,"format_color_spaces_changed":false,"present_modes_changed":false,"alpha_modes_changed":false}]}
{"health":{"status":"Blocked"},"last_display_contract_refresh":{"reason":"WindowMoved","previous":{"display_target":{"name":"Panel A","position":[0,0],"physical_size":[2560,1440],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"surface_format":"Bgra8UnormSrgb","surface_color_space":"Srgb","surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","display_tone_map_headroom_ppm":null,"available_surface_formats":["Bgra8UnormSrgb"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo"],"alpha_modes":["Auto"]},"next":{"display_target":{"name":"HDR Monitor","position":[2560,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":120000},"surface_format":"Rgba16Float","surface_color_space":"Bt2100Pq","surface_encoding":"Pq","surface_hdr_mode":"HdrPq","display_tone_map_headroom_ppm":null,"available_surface_formats":["Bgra8UnormSrgb","Rgba16Float"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false},{"format":"Rgba16Float","srgb":false,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":true,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo","Immediate"],"alpha_modes":["Auto","Opaque"]},"renderer_rebuilt":true,"display_target_changed":true,"surface_format_changed":true,"surface_color_space_changed":true,"surface_hdr_mode_changed":true,"display_tone_map_headroom_changed":false,"available_surface_formats_changed":true,"format_color_spaces_changed":true,"present_modes_changed":true,"alpha_modes_changed":true}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_waiting: 1,
            max_blocked: 1,
            max_display_contract_refreshes: 2,
            max_available_surface_format_changes: 1,
            max_format_color_space_changes: 1,
            max_present_mode_changes: 1,
            max_alpha_mode_changes: 1,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(summary.passed);
        assert_eq!(
            summary.display_contract_refreshes,
            ViewerGpuOutputDisplayContractRefreshCounts {
                total: 2,
                resize: 1,
                window_moved: 1,
                renderer_rebuilt: 1,
                display_target_changed: 2,
                surface_format_changed: 1,
                surface_color_space_changed: 1,
                surface_hdr_mode_changed: 1,
                available_surface_formats_changed: 1,
                format_color_spaces_changed: 1,
                present_modes_changed: 1,
                alpha_modes_changed: 1,
                ..ViewerGpuOutputDisplayContractRefreshCounts::default()
            }
        );
        assert_eq!(
            summary.last_display_contract_refresh,
            Some(ViewerGpuOutputDisplayContractRefreshEvent {
                reason: "WindowMoved".to_owned(),
                previous: ViewerGpuOutputDisplayContractSnapshot {
                    display_target: ViewerGpuOutputDisplayTarget {
                        name: Some("Panel A".to_owned()),
                        position: (0, 0),
                        physical_size: (2560, 1440),
                        scale_factor_ppm: 1_000_000,
                        refresh_rate_millihertz: Some(60_000),
                    },
                    surface_format: "Bgra8UnormSrgb".to_owned(),
                    surface_color_space: "Srgb".to_owned(),
                    surface_encoding: "Srgb".to_owned(),
                    surface_hdr_mode: "SdrOnly".to_owned(),
                    display_tone_map_headroom_ppm: None,
                    available_surface_formats: vec!["Bgra8UnormSrgb".to_owned()],
                    format_color_spaces: vec![ViewerGpuOutputSurfaceFormatColorSpaces {
                        format: "Bgra8UnormSrgb".to_owned(),
                        srgb: true,
                        extended_srgb_linear: false,
                        display_p3: false,
                        bt2100_pq: false,
                        bt2100_hlg: false,
                        extended_srgb: false,
                        extended_display_p3: false,
                    }],
                    present_modes: vec!["Fifo".to_owned()],
                    alpha_modes: vec!["Auto".to_owned()],
                },
                next: ViewerGpuOutputDisplayContractSnapshot {
                    display_target: ViewerGpuOutputDisplayTarget {
                        name: Some("HDR Monitor".to_owned()),
                        position: (2560, 0),
                        physical_size: (3840, 2160),
                        scale_factor_ppm: 1_000_000,
                        refresh_rate_millihertz: Some(120_000),
                    },
                    surface_format: "Rgba16Float".to_owned(),
                    surface_color_space: "Bt2100Pq".to_owned(),
                    surface_encoding: "Pq".to_owned(),
                    surface_hdr_mode: "HdrPq".to_owned(),
                    display_tone_map_headroom_ppm: None,
                    available_surface_formats: vec![
                        "Bgra8UnormSrgb".to_owned(),
                        "Rgba16Float".to_owned(),
                    ],
                    format_color_spaces: vec![
                        ViewerGpuOutputSurfaceFormatColorSpaces {
                            format: "Bgra8UnormSrgb".to_owned(),
                            srgb: true,
                            extended_srgb_linear: false,
                            display_p3: false,
                            bt2100_pq: false,
                            bt2100_hlg: false,
                            extended_srgb: false,
                            extended_display_p3: false,
                        },
                        ViewerGpuOutputSurfaceFormatColorSpaces {
                            format: "Rgba16Float".to_owned(),
                            srgb: false,
                            extended_srgb_linear: false,
                            display_p3: false,
                            bt2100_pq: true,
                            bt2100_hlg: false,
                            extended_srgb: false,
                            extended_display_p3: false,
                        },
                    ],
                    present_modes: vec!["Fifo".to_owned(), "Immediate".to_owned()],
                    alpha_modes: vec!["Auto".to_owned(), "Opaque".to_owned()],
                },
                renderer_rebuilt: true,
                display_target_changed: true,
                surface_format_changed: true,
                surface_color_space_changed: true,
                surface_hdr_mode_changed: true,
                display_tone_map_headroom_changed: false,
                available_surface_formats_changed: true,
                format_color_spaces_changed: true,
                present_modes_changed: true,
                alpha_modes_changed: true,
            })
        );
    }

    #[test]
    fn budget_fails_display_capability_drift_thresholds() {
        let jsonl = r#"
{"health":{"status":"Ready"},"last_display_contract_refresh":{"reason":"WindowMoved","previous":{"display_target":{"name":"Panel A","position":[0,0],"physical_size":[2560,1440],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"surface_format":"Bgra8UnormSrgb","surface_color_space":"Srgb","surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","display_tone_map_headroom_ppm":null,"available_surface_formats":["Bgra8UnormSrgb"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo"],"alpha_modes":["Auto"]},"next":{"display_target":{"name":"HDR Monitor","position":[2560,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":120000},"surface_format":"Rgba16Float","surface_color_space":"Bt2100Pq","surface_encoding":"Pq","surface_hdr_mode":"HdrPq","display_tone_map_headroom_ppm":null,"available_surface_formats":["Bgra8UnormSrgb","Rgba16Float"],"format_color_spaces":[{"format":"Bgra8UnormSrgb","srgb":true,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":false,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false},{"format":"Rgba16Float","srgb":false,"extended_srgb_linear":false,"display_p3":false,"bt2100_pq":true,"bt2100_hlg":false,"extended_srgb":false,"extended_display_p3":false}],"present_modes":["Fifo","Immediate"],"alpha_modes":["Auto","Opaque"]},"renderer_rebuilt":true,"display_target_changed":true,"surface_format_changed":true,"surface_color_space_changed":true,"surface_hdr_mode_changed":true,"display_tone_map_headroom_changed":false,"available_surface_formats_changed":true,"format_color_spaces_changed":true,"present_modes_changed":true,"alpha_modes_changed":true}}
"#;
        let budget = ViewerGpuOutputBudget {
            max_display_contract_refreshes: 0,
            max_available_surface_format_changes: 0,
            max_format_color_space_changes: 0,
            max_present_mode_changes: 0,
            max_alpha_mode_changes: 0,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(!summary.passed);
        assert!(summary.failures.contains(&ViewerGpuOutputBudgetFailure {
            metric: "display_contract_refreshes",
            actual: 1,
            limit: 0,
        }));
        assert!(summary.failures.contains(&ViewerGpuOutputBudgetFailure {
            metric: "available_surface_format_changes",
            actual: 1,
            limit: 0,
        }));
        assert!(summary.failures.contains(&ViewerGpuOutputBudgetFailure {
            metric: "format_color_space_changes",
            actual: 1,
            limit: 0,
        }));
        assert!(summary.failures.contains(&ViewerGpuOutputBudgetFailure {
            metric: "present_mode_changes",
            actual: 1,
            limit: 0,
        }));
        assert!(summary.failures.contains(&ViewerGpuOutputBudgetFailure {
            metric: "alpha_mode_changes",
            actual: 1,
            limit: 0,
        }));
    }

    #[test]
    fn budget_replays_viewer_color_rejections_into_media_issue_summary() {
        let jsonl = r#"
{"health":{"status":"Waiting"},"last_color_rejection":{"asset_id":"asset-a","path":"E:/media/missing.mov","missing_metadata_policy":"RejectMedia","source":"MissingPolicyRejectMedia","override_color_space":null,"detected_color_space":null,"working_color_space":"Rec2020","diagnostic_summary":"source=MissingMetadata,warnings=missing_cicp","diagnostic_issue_summary":{"detected_color_space":null,"confidence":"None","source":"MissingMetadata","method":"MissingMetadata","has_raw_cicp_metadata":false,"metadata_hint_count":0,"evidence_count":0,"warning_count":1,"multiple_metadata_hints":0,"ignored_metadata_hints":0,"metadata_hint_overrides_cicp_tags":0,"lower_priority_metadata_hints":0,"ignored_lower_priority_metadata_hints":0,"partial_cicp_tags":0,"missing_cicp_tags":1,"unsupported_cicp_tags":0,"decoder_unavailable":0,"hdr_side_data_count":0,"has_mastering_display_metadata":false,"has_content_light_metadata":false,"has_dynamic_hdr10_plus":false,"has_dolby_vision_config":false,"has_icc_profile":false,"has_user_visible_warnings":true}}}
{"health":{"status":"Ready"},"last_color_rejection":{"asset_id":"asset-b","path":"E:/media/offline.mov","missing_metadata_policy":"RejectMedia","source":"MissingPolicyRejectMedia","override_color_space":null,"detected_color_space":null,"working_color_space":"Rec2020","diagnostic_summary":"source=DecoderUnavailable,warnings=decoder_unavailable","diagnostic_issue_summary":{"detected_color_space":null,"confidence":"None","source":"DecoderUnavailable","method":"DecoderUnavailable","has_raw_cicp_metadata":false,"metadata_hint_count":0,"evidence_count":1,"warning_count":1,"multiple_metadata_hints":0,"ignored_metadata_hints":0,"metadata_hint_overrides_cicp_tags":0,"lower_priority_metadata_hints":0,"ignored_lower_priority_metadata_hints":0,"partial_cicp_tags":0,"missing_cicp_tags":0,"unsupported_cicp_tags":0,"decoder_unavailable":1,"hdr_side_data_count":0,"has_mastering_display_metadata":false,"has_content_light_metadata":false,"has_dynamic_hdr10_plus":false,"has_dolby_vision_config":false,"has_icc_profile":false,"has_user_visible_warnings":true}}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 1,
            max_waiting: 1,
            max_color_rejections: 2,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(summary.passed);
        assert_eq!(summary.color_rejections, 2);
        assert_eq!(
            summary.media_issues,
            VideoColorDiagnosticIssueAggregate {
                diagnostics: 2,
                diagnostics_with_warnings: 2,
                method_missing_metadata: 1,
                method_decoder_unavailable: 1,
                confidence_none: 2,
                evidence_count: 1,
                warning_count: 2,
                missing_cicp_tags: 1,
                unsupported_cicp_tags: 0,
                decoder_unavailable: 1,
                ..VideoColorDiagnosticIssueAggregate::default()
            }
        );
        assert_eq!(
            summary.last_color_rejection,
            Some(ViewerGpuOutputColorRejectionSummary {
                asset_id: "asset-b".to_owned(),
                path: "E:/media/offline.mov".to_owned(),
                missing_metadata_policy: "RejectMedia".to_owned(),
                source: "MissingPolicyRejectMedia".to_owned(),
                override_color_space: None,
                detected_color_space: None,
                working_color_space: "Rec2020".to_owned(),
                diagnostic_summary: "source=DecoderUnavailable,warnings=decoder_unavailable"
                    .to_owned(),
                diagnostic_issue_summary: VideoColorDiagnosticIssueSummary {
                    detected_color_space: None,
                    confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                    source: mondrian_media::VideoColorSpaceSource::DecoderUnavailable,
                    method: mondrian_media::VideoColorDetectionMethod::DecoderUnavailable,
                    has_raw_cicp_metadata: false,
                    metadata_hint_count: 0,
                    evidence_count: 1,
                    warning_count: 1,
                    multiple_metadata_hints: 0,
                    ignored_metadata_hints: 0,
                    metadata_hint_overrides_cicp_tags: 0,
                    lower_priority_metadata_hints: 0,
                    ignored_lower_priority_metadata_hints: 0,
                    partial_cicp_tags: 0,
                    missing_cicp_tags: 0,
                    unsupported_cicp_tags: 0,
                    decoder_unavailable: 1,
                    hdr_side_data_count: 0,
                    has_mastering_display_metadata: false,
                    has_content_light_metadata: false,
                    has_dynamic_hdr10_plus: false,
                    has_dolby_vision_config: false,
                    has_icc_profile: false,
                    icc_cicp_mismatch: 0,
                    icc_profile_unmapped: 0,
                    has_user_visible_warnings: true,
                },
            })
        );
    }

    #[test]
    fn budget_fails_display_issue_thresholds() {
        let jsonl = r#"
{"health":{"status":"Degraded"},"display_issue_summary":{"reason":"ReconfigureBlockedByPayload","output_color_space":"DisplayP3","payload_blocker":"UiExternalTextureCompositingRequiresSdrSrgb"}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_degraded: 1,
            max_display_issues: 0,
            max_reconfigure_blocked_by_payload: 0,
            max_display_payload_blockers: 0,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(!summary.passed);
        assert_eq!(
            summary.failures,
            vec![
                ViewerGpuOutputBudgetFailure { metric: "display_issues", actual: 1, limit: 0 },
                ViewerGpuOutputBudgetFailure {
                    metric: "reconfigure_blocked_by_payload",
                    actual: 1,
                    limit: 0,
                },
                ViewerGpuOutputBudgetFailure {
                    metric: "display_payload_blockers",
                    actual: 1,
                    limit: 0,
                },
            ]
        );
    }

    #[test]
    fn budget_fails_color_rejection_threshold() {
        let jsonl = r#"
{"health":{"status":"Ready"},"last_color_rejection":{"asset_id":"asset-a","path":"E:/media/missing.mov","missing_metadata_policy":"RejectMedia","source":"MissingPolicyRejectMedia","override_color_space":null,"detected_color_space":null,"working_color_space":"Rec2020","diagnostic_summary":"source=MissingMetadata,warnings=missing_cicp","diagnostic_issue_summary":{"detected_color_space":null,"confidence":"None","source":"MissingMetadata","method":"MissingMetadata","has_raw_cicp_metadata":false,"metadata_hint_count":0,"evidence_count":0,"warning_count":1,"multiple_metadata_hints":0,"ignored_metadata_hints":0,"metadata_hint_overrides_cicp_tags":0,"lower_priority_metadata_hints":0,"ignored_lower_priority_metadata_hints":0,"partial_cicp_tags":0,"missing_cicp_tags":1,"unsupported_cicp_tags":0,"decoder_unavailable":0,"hdr_side_data_count":0,"has_mastering_display_metadata":false,"has_content_light_metadata":false,"has_dynamic_hdr10_plus":false,"has_dolby_vision_config":false,"has_icc_profile":false,"has_user_visible_warnings":true}}}
"#;
        let budget = ViewerGpuOutputBudget {
            max_color_rejections: 0,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(!summary.passed);
        assert!(summary.failures.contains(&ViewerGpuOutputBudgetFailure {
            metric: "color_rejections",
            actual: 1,
            limit: 0,
        }));
    }

    #[test]
    fn budget_fails_specific_display_issue_reason_thresholds() {
        let jsonl = r#"
{"health":{"status":"Blocked"},"display_issue_summary":{"reason":"HdrOutputRequiresHdrSurface","output_color_space":"Rec2100Pq","payload_blocker":null}}
{"health":{"status":"Blocked"},"display_issue_summary":{"reason":"UnsupportedSurfaceContract","output_color_space":"DisplayP3","payload_blocker":null}}
{"health":{"status":"Blocked"},"display_issue_summary":{"reason":"UnsupportedPresentationIntent","output_color_space":"SceneLinear","payload_blocker":null}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_blocked: 3,
            max_display_issues: 3,
            max_hdr_output_requires_hdr_surface: 0,
            max_unsupported_presentation_intent: 0,
            max_unsupported_surface_contract: 0,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(!summary.passed);
        assert_eq!(
            summary.failures,
            vec![
                ViewerGpuOutputBudgetFailure {
                    metric: "hdr_output_requires_hdr_surface",
                    actual: 1,
                    limit: 0,
                },
                ViewerGpuOutputBudgetFailure {
                    metric: "unsupported_presentation_intent",
                    actual: 1,
                    limit: 0,
                },
                ViewerGpuOutputBudgetFailure {
                    metric: "unsupported_surface_contract",
                    actual: 1,
                    limit: 0,
                },
            ]
        );
    }

    #[test]
    fn budget_fails_os_display_profile_unsupported_threshold() {
        let jsonl = r#"
{"health":{"status":"Blocked"},"display_issue_summary":{"reason":"OsDisplayProfileUnsupported","output_color_space":"DisplayP3","payload_blocker":null}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_blocked: 1,
            max_display_issues: 1,
            max_os_display_profile_unsupported: 0,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(!summary.passed);
        assert_eq!(
            summary.failures,
            vec![ViewerGpuOutputBudgetFailure {
                metric: "os_display_profile_unsupported",
                actual: 1,
                limit: 0,
            }]
        );
    }

    #[test]
    fn budget_fails_unknown_display_issue_reason_threshold() {
        let jsonl = r#"
{"health":{"status":"Degraded"},"display_issue_summary":{"reason":"FutureDisplayReason","output_color_space":"DisplayP3","payload_blocker":null}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_degraded: 1,
            max_display_issues: 1,
            max_unknown_display_issues: 0,
            ..viewer_gpu_output_budget_allow_missing()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(!summary.passed);
        assert_eq!(
            summary.failures,
            vec![ViewerGpuOutputBudgetFailure {
                metric: "unknown_display_issues",
                actual: 1,
                limit: 0,
            },]
        );
    }
}
