//! Budget evaluation for live viewer GPU-output diagnostics JSONL.

use anyhow::Context;
use mondrian_media::{VideoColorDiagnosticIssueAggregate, VideoColorDiagnosticIssueSummary};
use serde::{Deserialize, Serialize};

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
    /// Maximum allowed records carrying an unknown display issue reason.
    pub max_unknown_display_issues: u64,
    /// Maximum allowed records with a display presentation payload blocker.
    pub max_display_payload_blockers: u64,
    /// Maximum allowed records carrying a viewer color rejection.
    pub max_color_rejections: u64,
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
            max_unknown_display_issues: 0,
            max_display_payload_blockers: 0,
            max_color_rejections: 0,
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
    /// Last frame context observed in the stream.
    pub last_frame_context: Option<ViewerGpuOutputFrameContext>,
    /// Last display issue summary observed in the stream.
    pub last_display_issue: Option<ViewerGpuOutputDisplayIssueSummary>,
    /// Last display-contract refresh event observed in the stream.
    pub last_display_contract_refresh: Option<ViewerGpuOutputDisplayContractRefreshEvent>,
    /// Last viewer color rejection observed in the stream.
    pub last_color_rejection: Option<ViewerGpuOutputColorRejectionSummary>,
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
    /// Maximum allowed records with a display presentation payload blocker.
    pub max_display_payload_blockers: u64,
    /// Maximum allowed records carrying a viewer color rejection.
    pub max_color_rejections: u64,
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
            max_display_payload_blockers: budget.max_display_payload_blockers,
            max_color_rejections: budget.max_color_rejections,
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
    let mut last_display_issue = None;
    let mut last_display_contract_refresh = None;
    let mut color_rejections = 0u64;
    let mut media_issues = VideoColorDiagnosticIssueAggregate::default();
    let mut last_color_rejection = None;
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
        "unknown_display_issues",
        display_issues.unknown,
        budget.max_unknown_display_issues,
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

    Ok(ViewerGpuOutputBudgetSummary {
        records,
        counts,
        display_issues,
        display_contract_refreshes,
        display_issue_refresh_correlations,
        color_rejections,
        media_issues,
        reported_counts,
        reported_counts_match_replay: count_mismatches.is_empty(),
        count_mismatches,
        budget: (*budget).into(),
        passed: failures.is_empty(),
        failures,
        last_status,
        last_frame_context,
        last_display_issue,
        last_display_contract_refresh,
        last_color_rejection,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ViewerGpuOutputDiagnosticRecord {
    health: ViewerGpuOutputHealthSummary,
    health_counts: Option<ViewerGpuOutputHealthCounts>,
    last_frame_context: Option<ViewerGpuOutputFrameContext>,
    display_issue_summary: Option<ViewerGpuOutputDisplayIssueSummary>,
    #[serde(default)]
    recent_display_contract_refreshes: Vec<ViewerGpuOutputDisplayContractRefreshEvent>,
    last_display_contract_refresh: Option<ViewerGpuOutputDisplayContractRefreshEvent>,
    last_color_rejection: Option<ViewerGpuOutputColorRejectionSummary>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
struct ViewerGpuOutputHealthSummary {
    status: ViewerGpuOutputHealthStatus,
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
    fn record(&mut self, status: ViewerGpuOutputHealthStatus) {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum ViewerGpuOutputHealthStatus {
    /// No invocation has been observed.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_passes_clean_ready_stream() {
        let jsonl = r#"
{"health":{"status":"Waiting"},"health_counts":{"no_invocation":0,"waiting":1,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":0}}
{"health":{"status":"Ready"},"health_counts":{"no_invocation":0,"waiting":1,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"last_frame_context":{"sequence_id":"seq","frame":7,"width":1920,"height":1080,"external_texture_key":"key","output_target":"Display","output_color_space":"Srgb","tone_map":false}}
"#;

        let summary =
            evaluate_jsonl(jsonl, &ViewerGpuOutputBudget::default()).expect("budget summary");

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
    fn budget_fails_failed_blocked_and_missing_ready_stream() {
        let jsonl = r#"
{"health":{"status":"Failed"}}
{"health":{"status":"Blocked"}}
"#;

        let summary =
            evaluate_jsonl(jsonl, &ViewerGpuOutputBudget::default()).expect("budget summary");

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
        let summary =
            evaluate_jsonl("\n\n", &ViewerGpuOutputBudget::default()).expect("budget summary");

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

        let summary =
            evaluate_jsonl(jsonl, &ViewerGpuOutputBudget::default()).expect("budget summary");

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
{"health":{"status":"Degraded"},"display_issue_summary":{"reason":"ReconfigureBlockedByPayload","output_color_space":"DciP3","payload_blocker":"UiExternalTextureCompositingRequiresSdrSrgb"}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_blocked: 1,
            max_degraded: 1,
            max_display_issues: 2,
            max_hdr_output_requires_hdr_surface: 1,
            max_reconfigure_blocked_by_payload: 1,
            max_display_payload_blockers: 1,
            ..ViewerGpuOutputBudget::default()
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
                output_color_space: Some("DciP3".to_owned()),
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
{"health":{"status":"Blocked"},"display_issue_summary":{"reason":"OutputColorSpaceRequiresSurfaceColorSpace","output_color_space":"DciP3","display_target":{"name":"Reference Monitor","position":[1920,0],"physical_size":[3840,2160],"scale_factor_ppm":1000000,"refresh_rate_millihertz":60000},"current_surface_format":null,"current_surface_color_space":null,"current_surface_encoding":null,"selected_surface_format":"Bgra8UnormSrgb","selected_surface_color_space":"Srgb","selected_surface_encoding":"Srgb","surface_hdr_mode":"SdrOnly","desired_surface_format":null,"desired_surface_color_space":"DisplayP3","desired_surface_encoding":"Srgb","desired_surface_hdr_mode":"SdrOnly","payload_blocker":null,"supported_surface_color_space_count":2,"target_surface_color_space_supported":true}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_blocked: 1,
            max_display_issues: 1,
            max_output_color_space_requires_surface_color_space: 1,
            ..ViewerGpuOutputBudget::default()
        };

        let summary = evaluate_jsonl(jsonl, &budget).expect("budget summary");

        assert!(summary.passed);
        assert_eq!(
            summary.last_display_issue,
            Some(ViewerGpuOutputDisplayIssueSummary {
                reason: "OutputColorSpaceRequiresSurfaceColorSpace".to_owned(),
                output_color_space: Some("DciP3".to_owned()),
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
            ..ViewerGpuOutputBudget::default()
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
            ..ViewerGpuOutputBudget::default()
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
    fn budget_replays_viewer_color_rejections_into_media_issue_summary() {
        let jsonl = r#"
{"health":{"status":"Waiting"},"last_color_rejection":{"asset_id":"asset-a","path":"E:/media/missing.mov","missing_metadata_policy":"RejectMedia","source":"MissingPolicyRejectMedia","override_color_space":null,"detected_color_space":null,"working_color_space":"Rec2020","diagnostic_summary":"source=MissingMetadata,warnings=missing_or_unsupported_cicp","diagnostic_issue_summary":{"detected_color_space":null,"confidence":"None","source":"MissingMetadata","method":"MissingMetadata","has_raw_cicp_metadata":false,"metadata_hint_count":0,"evidence_count":0,"warning_count":1,"multiple_metadata_hints":0,"ignored_metadata_hints":0,"metadata_hint_overrides_cicp_tags":0,"partial_cicp_tags":0,"missing_or_unsupported_cicp_tags":1,"decoder_unavailable":0,"hdr_side_data_count":0,"has_mastering_display_metadata":false,"has_content_light_metadata":false,"has_dynamic_hdr10_plus":false,"has_dolby_vision_config":false,"has_icc_profile":false,"has_user_visible_warnings":true}}}
{"health":{"status":"Ready"},"last_color_rejection":{"asset_id":"asset-b","path":"E:/media/offline.mov","missing_metadata_policy":"RejectMedia","source":"MissingPolicyRejectMedia","override_color_space":null,"detected_color_space":null,"working_color_space":"Rec2020","diagnostic_summary":"source=DecoderUnavailable,warnings=decoder_unavailable","diagnostic_issue_summary":{"detected_color_space":null,"confidence":"None","source":"DecoderUnavailable","method":"DecoderUnavailable","has_raw_cicp_metadata":false,"metadata_hint_count":0,"evidence_count":1,"warning_count":1,"multiple_metadata_hints":0,"ignored_metadata_hints":0,"metadata_hint_overrides_cicp_tags":0,"partial_cicp_tags":0,"missing_or_unsupported_cicp_tags":0,"decoder_unavailable":1,"hdr_side_data_count":0,"has_mastering_display_metadata":false,"has_content_light_metadata":false,"has_dynamic_hdr10_plus":false,"has_dolby_vision_config":false,"has_icc_profile":false,"has_user_visible_warnings":true}}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 1,
            max_waiting: 1,
            max_color_rejections: 2,
            ..ViewerGpuOutputBudget::default()
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
                missing_or_unsupported_cicp_tags: 1,
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
                    partial_cicp_tags: 0,
                    missing_or_unsupported_cicp_tags: 0,
                    decoder_unavailable: 1,
                    hdr_side_data_count: 0,
                    has_mastering_display_metadata: false,
                    has_content_light_metadata: false,
                    has_dynamic_hdr10_plus: false,
                    has_dolby_vision_config: false,
                    has_icc_profile: false,
                    has_user_visible_warnings: true,
                },
            })
        );
    }

    #[test]
    fn budget_fails_display_issue_thresholds() {
        let jsonl = r#"
{"health":{"status":"Degraded"},"display_issue_summary":{"reason":"ReconfigureBlockedByPayload","output_color_space":"DciP3","payload_blocker":"UiExternalTextureCompositingRequiresSdrSrgb"}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_degraded: 1,
            max_display_issues: 0,
            max_reconfigure_blocked_by_payload: 0,
            max_display_payload_blockers: 0,
            ..ViewerGpuOutputBudget::default()
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
{"health":{"status":"Ready"},"last_color_rejection":{"asset_id":"asset-a","path":"E:/media/missing.mov","missing_metadata_policy":"RejectMedia","source":"MissingPolicyRejectMedia","override_color_space":null,"detected_color_space":null,"working_color_space":"Rec2020","diagnostic_summary":"source=MissingMetadata,warnings=missing_or_unsupported_cicp","diagnostic_issue_summary":{"detected_color_space":null,"confidence":"None","source":"MissingMetadata","method":"MissingMetadata","has_raw_cicp_metadata":false,"metadata_hint_count":0,"evidence_count":0,"warning_count":1,"multiple_metadata_hints":0,"ignored_metadata_hints":0,"metadata_hint_overrides_cicp_tags":0,"partial_cicp_tags":0,"missing_or_unsupported_cicp_tags":1,"decoder_unavailable":0,"hdr_side_data_count":0,"has_mastering_display_metadata":false,"has_content_light_metadata":false,"has_dynamic_hdr10_plus":false,"has_dolby_vision_config":false,"has_icc_profile":false,"has_user_visible_warnings":true}}}
"#;
        let budget = ViewerGpuOutputBudget {
            max_color_rejections: 0,
            ..ViewerGpuOutputBudget::default()
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
            ..ViewerGpuOutputBudget::default()
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
    fn budget_fails_unknown_display_issue_reason_threshold() {
        let jsonl = r#"
{"health":{"status":"Degraded"},"display_issue_summary":{"reason":"FutureDisplayReason","output_color_space":"DisplayP3","payload_blocker":null}}
"#;
        let budget = ViewerGpuOutputBudget {
            min_ready: 0,
            max_degraded: 1,
            max_display_issues: 1,
            max_unknown_display_issues: 0,
            ..ViewerGpuOutputBudget::default()
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
