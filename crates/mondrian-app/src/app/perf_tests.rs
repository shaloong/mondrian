use super::*;
use crate::app_ui::panels::{ViewerPreviewSource, ViewerPreviewState};
use crate::app_ui::preview::{
    AppUiPreviewColorHealthSummary, AppUiPreviewDiagnostics, AppUiPreviewService,
};
use crate::app_ui::shell::AppUiAppRoot;
use crate::app_ui::viewer_gpu_output_budget::{
    build_health_report, evaluate_jsonl, ViewerGpuOutputBudget, ViewerGpuOutputBudgetSummary,
    ViewerGpuOutputHealthReport,
};
use anyhow::Context;
use serde::Serialize;
use std::cmp;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mondrian_core::types::Rational;
use mondrian_effects::{EffectNode, EffectNodeExt};
use mondrian_media::{VideoColorDiagnostic, VideoColorDiagnosticIssueAggregate};
use mondrian_timeline::track::Track;
use mondrian_ui_core::tree::TreeWalker;
use mondrian_ui_core::types::Rect;
use mondrian_ui_renderer::DrawEncoder;
use mondrian_ui_theme::ThemePreset;

#[derive(Debug, Serialize)]
struct PerfCaseReport {
    case: &'static str,
    iterations: usize,
    samples_ms: Vec<u128>,
    avg_ms: u128,
    max_ms: u128,
    threshold_ms: u128,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct AppUiScaleReport {
    scenario: &'static str,
    assets: usize,
    clips: usize,
    effects: usize,
    resize_iterations: usize,
    playback_frames: usize,
    initial_paint_commands: usize,
    playback_paint_commands_max: usize,
    preview_diagnostics: AppUiPreviewDiagnostics,
    preview_color_health: Option<AppUiPreviewColorHealthSummary>,
    preview_color_path: PreviewColorPathReport,
    preview_playback_diagnostics: AppUiPreviewDiagnostics,
    preview_playback_color_health: Option<AppUiPreviewColorHealthSummary>,
    preview_playback_color_path: PreviewColorPathReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Serialize)]
struct PreviewMediaPerfReport {
    scenario: &'static str,
    frames: usize,
    cache_iterations: usize,
    media_color_issues: VideoColorDiagnosticIssueAggregate,
    preview_diagnostics: AppUiPreviewDiagnostics,
    preview_color_health: Option<AppUiPreviewColorHealthSummary>,
    preview_color_health_budget: PreviewPerfColorHealthBudget,
    preview_color_health_passed: bool,
    preview_color_health_failures: Vec<PreviewPerfColorHealthBudgetFailure>,
    preview_color_path: PreviewColorPathReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Serialize, Default)]
struct PreviewReadinessCounts {
    ready: usize,
    loading: usize,
    stale: usize,
    unavailable: usize,
}

#[derive(Debug, Serialize)]
struct PreviewMediaPlaybackPerfReport {
    scenario: &'static str,
    frames: usize,
    frame_interval_ms: u64,
    readiness: PreviewReadinessCounts,
    media_color_issues: VideoColorDiagnosticIssueAggregate,
    preview_diagnostics: AppUiPreviewDiagnostics,
    preview_color_health: Option<AppUiPreviewColorHealthSummary>,
    preview_color_health_budget: PreviewPerfColorHealthBudget,
    preview_color_health_passed: bool,
    preview_color_health_failures: Vec<PreviewPerfColorHealthBudgetFailure>,
    preview_color_path: PreviewColorPathReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Serialize)]
struct ViewerGpuOutputBudgetSmokeReport {
    scenario: &'static str,
    source_path: String,
    summary: ViewerGpuOutputBudgetSummary,
    health_report: ViewerGpuOutputHealthReport,
}

const VIEWER_GPU_OUTPUT_BUDGET_SCENARIO: &str = "viewer_gpu_output_budget";
const VIEWER_GPU_OUTPUT_DISPLAY_BASELINE_SCENARIO: &str = "viewer_gpu_output_display_baseline";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
struct PreviewPerfColorHealthBudget {
    require_color_health: bool,
    require_fully_float_linear: bool,
    require_gpu_path_ready: bool,
    max_gpu_blockers: u64,
    max_legacy_reason_total: u64,
    max_transfer_stages: u64,
    max_policy_rejections: u64,
}

impl Default for PreviewPerfColorHealthBudget {
    fn default() -> Self {
        Self {
            require_color_health: true,
            require_fully_float_linear: true,
            require_gpu_path_ready: true,
            max_gpu_blockers: 0,
            max_legacy_reason_total: 0,
            max_transfer_stages: 0,
            max_policy_rejections: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PreviewPerfColorHealthBudgetFailure {
    metric: &'static str,
    actual: u64,
    limit: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct PreviewColorPathReport {
    composite_plans: u64,
    composite_elements: u64,
    float_linear_composites: u64,
    legacy_rgba8_composites: u64,
    legacy_reason_total: u64,
    legacy_reason_details: Vec<PreviewLegacyReasonReport>,
    input_color_resolution: PreviewInputColorResolutionReport,
    gpu_color_stages: u64,
    gpu_blockers: u64,
    gpu_blocker_breakdown: PreviewGpuBlockerBreakdownReport,
    rgba8_boundary_calls: u64,
    fully_float_linear: bool,
    gpu_path_ready: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct PreviewInputColorResolutionReport {
    override_count: u64,
    data_texture: u64,
    detected_metadata: u64,
    missing_assume_rec709: u64,
    missing_assume_working: u64,
    missing_rejected: u64,
    policy_assumptions: u64,
    explicit_metadata_or_override: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct PreviewGpuBlockerBreakdownReport {
    shader_module_not_prepared: u64,
    ocio_resource_bind_group_not_prepared: u64,
    fullscreen_wrapper_not_prepared: u64,
    render_pipeline_not_prepared: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct PreviewLegacyReasonReport {
    layer: &'static str,
    reason: &'static str,
    count: u64,
}

impl PreviewColorPathReport {
    fn from_diagnostics(diagnostics: AppUiPreviewDiagnostics) -> Self {
        let composite_summary = diagnostics.composite_color_path_summary();
        let legacy_breakdown = composite_summary.legacy_breakdown;
        let mut legacy_reason_details = Vec::new();
        push_legacy_reason(
            &mut legacy_reason_details,
            "media",
            "blend_mode",
            legacy_breakdown.media_blend_mode,
        );
        push_legacy_reason(
            &mut legacy_reason_details,
            "media",
            "transform",
            legacy_breakdown.media_transform,
        );
        push_legacy_reason(
            &mut legacy_reason_details,
            "media",
            "effect",
            legacy_breakdown.media_effect,
        );
        push_legacy_reason(
            &mut legacy_reason_details,
            "solid",
            "blend_mode",
            legacy_breakdown.solid_blend_mode,
        );
        push_legacy_reason(
            &mut legacy_reason_details,
            "solid",
            "transform",
            legacy_breakdown.solid_transform,
        );
        push_legacy_reason(
            &mut legacy_reason_details,
            "solid",
            "effect",
            legacy_breakdown.solid_effect,
        );
        push_legacy_reason(
            &mut legacy_reason_details,
            "adjustment",
            "blend_mode",
            legacy_breakdown.adjustment_blend_mode,
        );
        push_legacy_reason(
            &mut legacy_reason_details,
            "adjustment",
            "effect",
            legacy_breakdown.adjustment_effect,
        );
        let legacy_reason_total = legacy_breakdown.total();
        let fully_float_linear = composite_summary.is_fully_float_linear()
            && diagnostics.color_composite_plans == composite_summary.composite_plans();
        let gpu_path_ready = diagnostics.color_stage_gpu_blockers == 0
            && diagnostics.color_stage_readback_stages == 0
            && diagnostics.color_stage_upload_stages == 0;
        let input_counts = diagnostics.input_color_resolution_counts();

        Self {
            composite_plans: diagnostics.color_composite_plans,
            composite_elements: composite_summary.elements,
            float_linear_composites: composite_summary.float_linear_composites,
            legacy_rgba8_composites: composite_summary.legacy_rgba8_composites,
            legacy_reason_total,
            legacy_reason_details,
            input_color_resolution: PreviewInputColorResolutionReport {
                override_count: diagnostics.input_color_resolution_override,
                data_texture: diagnostics.input_color_resolution_data_texture,
                detected_metadata: diagnostics.input_color_resolution_detected_metadata,
                missing_assume_rec709: diagnostics.input_color_resolution_missing_assume_rec709,
                missing_assume_working: diagnostics.input_color_resolution_missing_assume_working,
                missing_rejected: diagnostics.input_color_resolution_missing_rejected,
                policy_assumptions: input_counts.policy_assumptions(),
                explicit_metadata_or_override: input_counts.explicit_metadata_or_override(),
            },
            gpu_color_stages: diagnostics.color_stage_gpu_color_stages,
            gpu_blockers: diagnostics.color_stage_gpu_blockers,
            gpu_blocker_breakdown: PreviewGpuBlockerBreakdownReport {
                shader_module_not_prepared: diagnostics.color_stage_gpu_shader_module_blockers,
                ocio_resource_bind_group_not_prepared: diagnostics
                    .color_stage_gpu_ocio_resource_blockers,
                fullscreen_wrapper_not_prepared: diagnostics.color_stage_gpu_wrapper_blockers,
                render_pipeline_not_prepared: diagnostics.color_stage_gpu_render_pipeline_blockers,
            },
            rgba8_boundary_calls: diagnostics.color_rgba8_boundary_calls,
            fully_float_linear,
            gpu_path_ready,
        }
    }
}

fn push_legacy_reason(
    reasons: &mut Vec<PreviewLegacyReasonReport>,
    layer: &'static str,
    reason: &'static str,
    count: u64,
) {
    if count > 0 {
        reasons.push(PreviewLegacyReasonReport { layer, reason, count });
    }
}

fn perf_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn env_usize_clamped(key: &str, default: usize, min: usize, max: usize) -> usize {
    env_usize(key, default).clamp(min, max)
}

fn env_u128(key: &str, default: u128) -> u128 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u128>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    let Some(value) = std::env::var(key).ok() else {
        return default;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn perf_output_path() -> Option<PathBuf> {
    std::env::var_os("MONDRIAN_PERF_OUTPUT").map(PathBuf::from)
}

fn write_report_if_needed(report_json: &str) {
    if let Some(path) = perf_output_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{report_json}");
        }
    }
}

fn viewer_gpu_output_budget_from_env() -> ViewerGpuOutputBudget {
    ViewerGpuOutputBudget {
        min_records: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MIN_RECORDS", 1),
        min_ready: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MIN_READY", 1),
        max_failed: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_FAILED", 0),
        max_blocked: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_BLOCKED", 0),
        max_rejected: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_REJECTED", 0),
        max_degraded: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DEGRADED", 0),
        max_waiting: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_WAITING", u64::MAX),
        max_display_issues: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_ISSUES", 0),
        max_hdr_output_requires_hdr_surface: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_HDR_OUTPUT_REQUIRES_HDR_SURFACE",
            0,
        ),
        max_output_color_space_requires_surface_color_space: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_OUTPUT_COLOR_SPACE_REQUIRES_SURFACE_COLOR_SPACE",
            0,
        ),
        max_reconfigure_blocked_by_payload: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_RECONFIGURE_BLOCKED_BY_PAYLOAD",
            0,
        ),
        max_unsupported_presentation_intent: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNSUPPORTED_PRESENTATION_INTENT",
            0,
        ),
        max_unsupported_surface_contract: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNSUPPORTED_SURFACE_CONTRACT",
            0,
        ),
        max_unknown_display_issues: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNKNOWN_DISPLAY_ISSUES",
            0,
        ),
        max_display_contract_refreshes: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_CONTRACT_REFRESHES",
            u64::MAX,
        ),
        max_display_issue_refresh_correlations: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_ISSUE_REFRESH_CORRELATIONS",
            0,
        ),
        max_display_tone_map_headroom_changes: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_TONE_MAP_HEADROOM_CHANGES",
            0,
        ),
        max_available_surface_format_changes: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_AVAILABLE_SURFACE_FORMAT_CHANGES",
            0,
        ),
        max_format_color_space_changes: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_FORMAT_COLOR_SPACE_CHANGES",
            0,
        ),
        max_present_mode_changes: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PRESENT_MODE_CHANGES", 0),
        max_alpha_mode_changes: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_ALPHA_MODE_CHANGES", 0),
        max_display_payload_blockers: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_PAYLOAD_BLOCKERS",
            0,
        ),
        max_color_rejections: env_u64("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_COLOR_REJECTIONS", 0),
    }
}

fn viewer_gpu_output_display_baseline_budget_from_env() -> ViewerGpuOutputBudget {
    ViewerGpuOutputBudget {
        max_display_contract_refreshes: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_CONTRACT_REFRESHES",
            ViewerGpuOutputBudget::display_baseline().max_display_contract_refreshes,
        ),
        ..viewer_gpu_output_budget_from_env()
    }
}

fn preview_color_health_budget_from_env() -> PreviewPerfColorHealthBudget {
    PreviewPerfColorHealthBudget {
        require_color_health: env_bool("MONDRIAN_PREVIEW_REQUIRE_COLOR_HEALTH", true),
        require_fully_float_linear: env_bool("MONDRIAN_PREVIEW_REQUIRE_FULLY_FLOAT_LINEAR", true),
        require_gpu_path_ready: env_bool("MONDRIAN_PREVIEW_REQUIRE_GPU_PATH_READY", true),
        max_gpu_blockers: env_u64("MONDRIAN_PREVIEW_MAX_GPU_BLOCKERS", 0),
        max_legacy_reason_total: env_u64("MONDRIAN_PREVIEW_MAX_LEGACY_REASONS", 0),
        max_transfer_stages: env_u64("MONDRIAN_PREVIEW_MAX_TRANSFER_STAGES", 0),
        max_policy_rejections: env_u64("MONDRIAN_PREVIEW_MAX_POLICY_REJECTIONS", 0),
    }
}

fn evaluate_preview_color_health_budget(
    color_health: Option<AppUiPreviewColorHealthSummary>,
    budget: PreviewPerfColorHealthBudget,
) -> Vec<PreviewPerfColorHealthBudgetFailure> {
    let Some(color_health) = color_health else {
        return if budget.require_color_health {
            vec![PreviewPerfColorHealthBudgetFailure {
                metric: "color_health_present",
                actual: 0,
                limit: 1,
            }]
        } else {
            Vec::new()
        };
    };

    let mut failures = Vec::new();
    if budget.require_fully_float_linear && !color_health.fully_float_linear {
        failures.push(PreviewPerfColorHealthBudgetFailure {
            metric: "fully_float_linear",
            actual: 0,
            limit: 1,
        });
    }
    if budget.require_gpu_path_ready && !color_health.gpu_path_ready {
        failures.push(PreviewPerfColorHealthBudgetFailure {
            metric: "gpu_path_ready",
            actual: 0,
            limit: 1,
        });
    }
    push_max_preview_color_health_failure(
        &mut failures,
        "gpu_blockers",
        color_health.gpu_blockers,
        budget.max_gpu_blockers,
    );
    push_max_preview_color_health_failure(
        &mut failures,
        "legacy_reason_total",
        color_health.legacy_reason_total,
        budget.max_legacy_reason_total,
    );
    push_max_preview_color_health_failure(
        &mut failures,
        "transfer_stages",
        color_health.transfer_stages,
        budget.max_transfer_stages,
    );
    push_max_preview_color_health_failure(
        &mut failures,
        "policy_rejections",
        color_health.policy_rejections,
        budget.max_policy_rejections,
    );
    failures
}

fn push_max_preview_color_health_failure(
    failures: &mut Vec<PreviewPerfColorHealthBudgetFailure>,
    metric: &'static str,
    actual: u64,
    limit: u64,
) {
    if actual > limit {
        failures.push(PreviewPerfColorHealthBudgetFailure { metric, actual, limit });
    }
}

fn viewer_gpu_output_budget_report_from_jsonl(
    scenario: &'static str,
    source_path: impl Into<String>,
    contents: &str,
    budget: &ViewerGpuOutputBudget,
) -> anyhow::Result<ViewerGpuOutputBudgetSmokeReport> {
    let summary = evaluate_jsonl(contents, budget)?;
    let health_report = build_health_report(summary.clone(), scenario);
    Ok(ViewerGpuOutputBudgetSmokeReport {
        scenario,
        source_path: source_path.into(),
        summary,
        health_report,
    })
}

fn run_case<F>(
    case: &'static str,
    iterations: usize,
    threshold_ms: u128,
    mut f: F,
) -> anyhow::Result<PerfCaseReport>
where
    F: FnMut() -> anyhow::Result<()>,
{
    let mut samples_ms = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started_at = Instant::now();
        f()?;
        samples_ms.push(started_at.elapsed().as_millis());
    }

    let total_ms = samples_ms.iter().copied().sum::<u128>();
    let avg_ms = total_ms / cmp::max(iterations as u128, 1);
    let max_ms = samples_ms.iter().copied().max().unwrap_or(0);
    let passed = max_ms <= threshold_ms;

    Ok(PerfCaseReport {
        case,
        iterations,
        samples_ms,
        avg_ms,
        max_ms,
        threshold_ms,
        passed,
    })
}

#[test]
#[ignore = "development viewer GPU output budget smoke; run after capturing MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT JSONL"]
fn viewer_gpu_output_budget_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf smoke lock");
    let output_path = std::env::var_os("MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT")
        .map(PathBuf::from)
        .context(
            "MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT must point to a viewer GPU output JSONL file",
        )?;
    let contents = std::fs::read_to_string(&output_path).with_context(|| {
        format!(
            "failed to read viewer GPU output JSONL: {}",
            output_path.display()
        )
    })?;
    let budget = viewer_gpu_output_budget_from_env();
    let report = viewer_gpu_output_budget_report_from_jsonl(
        VIEWER_GPU_OUTPUT_BUDGET_SCENARIO,
        output_path.display().to_string(),
        &contents,
        &budget,
    )?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_VIEWER_GPU_OUTPUT_BUDGET_JSON={report_json}");
    write_report_if_needed(&report_json);
    if !report.summary.passed {
        anyhow::bail!("viewer GPU output budget smoke failed: {report_json}");
    }
    Ok(())
}

#[test]
#[ignore = "development baseline viewer display smoke; run after capturing MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT JSONL"]
fn viewer_gpu_output_display_baseline_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf smoke lock");
    let output_path = std::env::var_os("MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT")
        .map(PathBuf::from)
        .context(
            "MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT must point to a viewer GPU output JSONL file",
        )?;
    let contents = std::fs::read_to_string(&output_path).with_context(|| {
        format!(
            "failed to read viewer GPU output JSONL: {}",
            output_path.display()
        )
    })?;
    let budget = viewer_gpu_output_display_baseline_budget_from_env();
    let report = viewer_gpu_output_budget_report_from_jsonl(
        VIEWER_GPU_OUTPUT_DISPLAY_BASELINE_SCENARIO,
        output_path.display().to_string(),
        &contents,
        &budget,
    )?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_VIEWER_GPU_OUTPUT_DISPLAY_BASELINE_JSON={report_json}");
    write_report_if_needed(&report_json);
    if !report.summary.passed {
        anyhow::bail!("viewer GPU output display baseline smoke failed: {report_json}");
    }
    Ok(())
}

#[test]
#[ignore = "development performance smoke test; run manually"]
fn perf_project_lifecycle_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let create_threshold_ms = env_u128("MONDRIAN_PERF_CREATE_MS", 8_000);
    let open_threshold_ms = env_u128("MONDRIAN_PERF_OPEN_MS", 6_000);
    let save_threshold_ms = env_u128("MONDRIAN_PERF_SAVE_MS", 6_000);

    let open_iters = env_usize("MONDRIAN_PERF_OPEN_ITERS", 3);
    let save_iters = env_usize("MONDRIAN_PERF_SAVE_ITERS", 5);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root = std::env::temp_dir().join(format!("mondrian_perf_smoke_{uniq}"));
    fs::create_dir_all(&root)?;

    let result = (|| -> anyhow::Result<Vec<PerfCaseReport>> {
        let project_path = root.join("perf-smoke.mdp");
        let mut state = AppState::new();

        let create_case = run_case("project.create_new_project", 1, create_threshold_ms, || {
            state.create_new_project_at(
                project_path.clone(),
                "perf-smoke",
                1920,
                1080,
                Rational::FPS_25,
            )
        })?;

        let open_case = run_case(
            "project.open_existing",
            open_iters,
            open_threshold_ms,
            || state.open_project_file(project_path.clone()),
        )?;

        let save_case = run_case(
            "project.save_existing",
            save_iters,
            save_threshold_ms,
            || state.save_project_file(),
        )?;

        Ok(vec![create_case, open_case, save_case])
    })();

    let _ = fs::remove_dir_all(&root);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> = report.iter().filter(|c| !c.passed).map(|c| c.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "performance smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }

    Ok(())
}

#[test]
#[ignore = "development app UI scale smoke test; run manually"]
fn app_ui_scale_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let asset_count = env_usize_clamped("MONDRIAN_UI_PERF_ASSETS", 360, 32, 5_000);
    let clip_count = env_usize_clamped("MONDRIAN_UI_PERF_CLIPS", 720, 24, 10_000);
    let effect_count = env_usize_clamped("MONDRIAN_UI_PERF_EFFECTS", 480, 0, 20_000);
    let resize_iterations = env_usize_clamped("MONDRIAN_UI_PERF_RESIZE_ITERS", 40, 4, 500);
    let playback_frames = env_usize_clamped("MONDRIAN_UI_PERF_PLAYBACK_FRAMES", 120, 8, 2_000);
    let refresh_iterations = env_usize_clamped("MONDRIAN_UI_PERF_REFRESH_ITERS", 8, 1, 100);

    let build_threshold_ms = env_u128("MONDRIAN_UI_PERF_BUILD_MS", 10_000);
    let refresh_threshold_ms = env_u128("MONDRIAN_UI_PERF_REFRESH_MS", 2_500);
    let resize_threshold_ms = env_u128("MONDRIAN_UI_PERF_RESIZE_MS", 1_500);
    let paint_threshold_ms = env_u128("MONDRIAN_UI_PERF_PAINT_MS", 1_500);
    let playback_threshold_ms = env_u128("MONDRIAN_UI_PERF_PLAYBACK_MS", 8_000);
    let preview_probe_threshold_ms = env_u128("MONDRIAN_UI_PERF_PREVIEW_PROBE_MS", 1_500);
    let preview_playback_threshold_ms = env_u128("MONDRIAN_UI_PERF_PREVIEW_PLAYBACK_MS", 4_000);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_ui_perf_smoke_{uniq}"));
    fs::create_dir_all(&root_dir)?;

    let result = (|| -> anyhow::Result<AppUiScaleReport> {
        let mut state = build_app_ui_perf_state(&root_dir, asset_count, clip_count, effect_count)?;
        let bounds = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let theme = ThemePreset::Dark.build();

        let build_case = run_case(
            "app_ui.root_build_large_project",
            1,
            build_threshold_ms,
            || {
                let mut root = AppUiAppRoot::from_app_state(&state);
                TreeWalker::layout(&mut root, bounds);
                Ok(())
            },
        )?;

        let mut root = AppUiAppRoot::from_app_state(&state);
        TreeWalker::layout(&mut root, bounds);

        let refresh_case = run_case(
            "app_ui.refresh_large_project",
            refresh_iterations,
            refresh_threshold_ms,
            || {
                root.refresh_from_app_state(&state);
                TreeWalker::layout(&mut root, bounds);
                Ok(())
            },
        )?;

        let resize_case = run_case("app_ui.resize_loop", 1, resize_threshold_ms, || {
            for index in 0..resize_iterations {
                let width = 1280.0 + (index % 9) as f32 * 83.0;
                let height = 720.0 + (index % 7) as f32 * 47.0;
                TreeWalker::layout(&mut root, Rect::new(0.0, 0.0, width, height));
            }
            TreeWalker::layout(&mut root, bounds);
            Ok(())
        })?;

        let mut initial_paint_commands = 0usize;
        let paint_case = run_case("app_ui.paint_large_project", 1, paint_threshold_ms, || {
            initial_paint_commands = paint_command_count(&root, &theme, bounds);
            anyhow::ensure!(
                initial_paint_commands > 0,
                "app UI root emitted no paint commands"
            );
            Ok(())
        })?;

        state.play();
        let mut playback_paint_commands_max = 0usize;
        let playback_case = run_case(
            "app_ui.sustained_playback_refresh",
            1,
            playback_threshold_ms,
            || {
                for frame in 0..playback_frames {
                    state.set_playback_frame_running(frame as i64);
                    root.refresh_playback_frame_from_app_state(&state, None);
                    TreeWalker::layout(&mut root, bounds);
                    if frame % 12 == 0 {
                        playback_paint_commands_max = playback_paint_commands_max
                            .max(paint_command_count(&root, &theme, bounds));
                    }
                }
                Ok(())
            },
        )?;
        state.pause();

        let preview_service = AppUiPreviewService::new();
        let mut preview_diagnostics = preview_service.diagnostics();
        let preview_probe_case = run_case(
            "app_ui.preview_diagnostics_probe",
            1,
            preview_probe_threshold_ms,
            || {
                let _ = preview_service.viewer_preview_for_state(&state);
                let _ = preview_service.gpu_preview_frame_for_state(&state);
                preview_diagnostics = preview_service.diagnostics();
                Ok(())
            },
        )?;

        let preview_playback_service = AppUiPreviewService::new();
        let mut preview_playback_diagnostics = preview_playback_service.diagnostics();
        let preview_playback_case = run_case(
            "app_ui.preview_playback_refresh",
            1,
            preview_playback_threshold_ms,
            || {
                for frame in 0..playback_frames {
                    state.set_playback_frame_running(frame as i64);
                    root.refresh_playback_frame_from_app_state(
                        &state,
                        Some(&preview_playback_service),
                    );
                    TreeWalker::layout(&mut root, bounds);
                }
                preview_playback_diagnostics = preview_playback_service.diagnostics();
                Ok(())
            },
        )?;
        state.pause();

        Ok(AppUiScaleReport {
            scenario: "app_ui_scale",
            assets: asset_count,
            clips: clip_count,
            effects: effect_count,
            resize_iterations,
            playback_frames,
            initial_paint_commands,
            playback_paint_commands_max,
            preview_diagnostics,
            preview_color_health: preview_diagnostics.color_health_summary(),
            preview_color_path: PreviewColorPathReport::from_diagnostics(preview_diagnostics),
            preview_playback_diagnostics,
            preview_playback_color_health: preview_playback_diagnostics.color_health_summary(),
            preview_playback_color_path: PreviewColorPathReport::from_diagnostics(
                preview_playback_diagnostics,
            ),
            cases: vec![
                build_case,
                refresh_case,
                resize_case,
                paint_case,
                playback_case,
                preview_probe_case,
                preview_playback_case,
            ],
        })
    })();

    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> =
        report.cases.iter().filter(|case| !case.passed).map(|case| case.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "app UI scale smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }

    Ok(())
}

#[test]
#[ignore = "development preview media decode/cache smoke test; run manually"]
fn preview_media_decode_cache_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let frame_count = env_usize_clamped("MONDRIAN_PREVIEW_MEDIA_FRAMES", 12, 2, 120);
    let cache_iterations = env_usize_clamped("MONDRIAN_PREVIEW_MEDIA_CACHE_ITERS", 30, 1, 500);
    let first_frame_threshold_ms = env_u128("MONDRIAN_PREVIEW_MEDIA_FIRST_READY_MS", 10_000);
    let cache_threshold_ms = env_u128("MONDRIAN_PREVIEW_MEDIA_CACHE_REFRESH_MS", 1_000);
    let gpu_candidate_threshold_ms = env_u128("MONDRIAN_PREVIEW_MEDIA_GPU_CANDIDATE_MS", 1_000);
    let sequential_threshold_ms = env_u128("MONDRIAN_PREVIEW_MEDIA_SEQUENCE_READY_MS", 8_000);
    let ready_timeout =
        Duration::from_millis(env_u128("MONDRIAN_PREVIEW_MEDIA_READY_TIMEOUT_MS", 10_000) as u64);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_preview_media_perf_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let video_path = root_dir.join("preview-media-smoke.mp4");

    if !generate_preview_media_fixture(&video_path)? {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"preview_media_decode_cache\",\"skipped\":\"ffmpeg CLI unavailable or fixture generation failed\"}}"
        );
        let _ = fs::remove_dir_all(&root_dir);
        return Ok(());
    }

    let result = (|| -> anyhow::Result<PreviewMediaPerfReport> {
        let mut state = build_preview_media_perf_state(&root_dir, &video_path, frame_count)?;
        let preview_service = AppUiPreviewService::new();

        let first_frame_case = run_case(
            "preview_media.first_frame_ready",
            1,
            first_frame_threshold_ms,
            || {
                state.seek(0);
                wait_for_preview_ready(&preview_service, &state, ready_timeout)
            },
        )?;

        let cached_frame_case = run_case(
            "preview_media.cached_frame_refresh",
            1,
            cache_threshold_ms,
            || {
                for _ in 0..cache_iterations {
                    assert_preview_ready(&preview_service, &state)?;
                }
                Ok(())
            },
        )?;

        let sequential_case = run_case(
            "preview_media.sequential_frame_ready_window",
            1,
            sequential_threshold_ms,
            || {
                for frame in 0..frame_count {
                    state.seek(frame as i64);
                    wait_for_preview_ready(&preview_service, &state, ready_timeout)?;
                }
                Ok(())
            },
        )?;

        let gpu_candidate_case = run_case(
            "preview_media.gpu_candidate_ready",
            1,
            gpu_candidate_threshold_ms,
            || {
                let _ = preview_service.gpu_preview_frame_for_state(&state);
                Ok(())
            },
        )?;

        let preview_diagnostics = preview_service.diagnostics();
        let media_color_issues = summarize_active_sequence_media_color_issues(&state)?;
        let preview_color_health = preview_diagnostics.color_health_summary();
        let preview_color_health_budget = preview_color_health_budget_from_env();
        let preview_color_health_failures =
            evaluate_preview_color_health_budget(preview_color_health, preview_color_health_budget);
        let preview_color_health_passed = preview_color_health_failures.is_empty();
        Ok(PreviewMediaPerfReport {
            scenario: "preview_media_decode_cache",
            frames: frame_count,
            cache_iterations,
            media_color_issues,
            preview_diagnostics,
            preview_color_health,
            preview_color_health_budget,
            preview_color_health_passed,
            preview_color_health_failures,
            preview_color_path: PreviewColorPathReport::from_diagnostics(preview_diagnostics),
            cases: vec![
                first_frame_case,
                cached_frame_case,
                sequential_case,
                gpu_candidate_case,
            ],
        })
    })();

    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> =
        report.cases.iter().filter(|case| !case.passed).map(|case| case.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "preview media performance smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }
    if !report.preview_color_health_passed {
        anyhow::bail!("preview media color-health budget failed: {report_json}");
    }

    Ok(())
}

#[test]
#[ignore = "development preview media continuous playback smoke test; run manually"]
fn preview_media_continuous_playback_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let frame_count = env_usize_clamped("MONDRIAN_PREVIEW_PLAYBACK_FRAMES", 60, 8, 60);
    let frame_interval_ms =
        env_usize_clamped("MONDRIAN_PREVIEW_PLAYBACK_FRAME_MS", 33, 1, 250) as u64;
    let playback_threshold_ms = env_u128("MONDRIAN_PREVIEW_PLAYBACK_WINDOW_MS", 8_000);
    let gpu_candidate_threshold_ms = env_u128("MONDRIAN_PREVIEW_PLAYBACK_GPU_CANDIDATE_MS", 1_000);
    let ready_timeout = Duration::from_millis(env_u128(
        "MONDRIAN_PREVIEW_PLAYBACK_READY_TIMEOUT_MS",
        10_000,
    ) as u64);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_preview_playback_perf_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let video_path = root_dir.join("preview-playback-smoke.mp4");

    if !generate_preview_media_fixture(&video_path)? {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"preview_media_continuous_playback\",\"skipped\":\"ffmpeg CLI unavailable or fixture generation failed\"}}"
        );
        let _ = fs::remove_dir_all(&root_dir);
        return Ok(());
    }

    let result = (|| -> anyhow::Result<PreviewMediaPlaybackPerfReport> {
        let mut state = build_preview_media_perf_state(&root_dir, &video_path, frame_count)?;
        let preview_service = AppUiPreviewService::new();
        let mut readiness = PreviewReadinessCounts::default();

        state.seek(0);
        wait_for_preview_ready(&preview_service, &state, ready_timeout)?;
        state.play();
        let playback_case = run_case(
            "preview_media.continuous_playback_readiness",
            1,
            playback_threshold_ms,
            || {
                for frame in 0..frame_count {
                    state.set_playback_frame_running(frame as i64);
                    let _ = preview_service.poll_finished();
                    record_preview_readiness(
                        &mut readiness,
                        preview_service.viewer_preview_for_state(&state),
                    );
                    let _ = preview_service.poll_finished();
                    thread::sleep(Duration::from_millis(frame_interval_ms));
                }
                let _ = preview_service.poll_finished();
                Ok(())
            },
        )?;
        state.pause();

        let gpu_candidate_case = run_case(
            "preview_media.playback_gpu_candidate_ready",
            1,
            gpu_candidate_threshold_ms,
            || {
                let _ = preview_service.gpu_preview_frame_for_state(&state);
                Ok(())
            },
        )?;

        anyhow::ensure!(
            readiness.unavailable == 0,
            "continuous playback returned unavailable frames: {:?}; diagnostics: {:?}",
            readiness,
            preview_service.diagnostics()
        );
        anyhow::ensure!(
            readiness.ready + readiness.stale >= frame_count.saturating_sub(2),
            "continuous playback did not keep enough frames visible: {:?}; diagnostics: {:?}",
            readiness,
            preview_service.diagnostics()
        );

        let preview_diagnostics = preview_service.diagnostics();
        let media_color_issues = summarize_active_sequence_media_color_issues(&state)?;
        let preview_color_health = preview_diagnostics.color_health_summary();
        let preview_color_health_budget = preview_color_health_budget_from_env();
        let preview_color_health_failures =
            evaluate_preview_color_health_budget(preview_color_health, preview_color_health_budget);
        let preview_color_health_passed = preview_color_health_failures.is_empty();
        Ok(PreviewMediaPlaybackPerfReport {
            scenario: "preview_media_continuous_playback",
            frames: frame_count,
            frame_interval_ms,
            readiness,
            media_color_issues,
            preview_diagnostics,
            preview_color_health,
            preview_color_health_budget,
            preview_color_health_passed,
            preview_color_health_failures,
            preview_color_path: PreviewColorPathReport::from_diagnostics(preview_diagnostics),
            cases: vec![playback_case, gpu_candidate_case],
        })
    })();

    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> =
        report.cases.iter().filter(|case| !case.passed).map(|case| case.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "preview media continuous playback smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }
    if !report.preview_color_health_passed {
        anyhow::bail!("preview media playback color-health budget failed: {report_json}");
    }

    Ok(())
}

fn build_app_ui_perf_state(
    root_dir: &Path,
    asset_count: usize,
    clip_count: usize,
    effect_count: usize,
) -> anyhow::Result<AppState> {
    let library = AssetLibrary::open(root_dir.join("library"))?;
    let mut asset_ids = Vec::with_capacity(asset_count);
    for index in 0..asset_count {
        let id = if index % 5 == 0 {
            library.create_adjustment_layer_asset(Some(&format!("Adjustment {index:04}")))?
        } else {
            library.create_solid_color_asset(Some(&format!("Solid {index:04}")))?
        };
        asset_ids.push(id);
    }

    let mut sequence = Sequence::new("App UI perf");
    sequence.video_tracks.clear();
    sequence.audio_tracks.clear();

    let video_track_count = 8usize;
    let audio_track_count = 6usize;
    for index in 0..video_track_count {
        sequence.video_tracks.push(Track::new_video(format!("V{}", index + 1)));
    }
    for index in 0..audio_track_count {
        sequence.audio_tracks.push(Track::new_audio(format!("A{}", index + 1)));
    }

    let tb = sequence.time_base();
    let effect_types = [
        EffectType::GaussianBlur,
        EffectType::Sharpen,
        EffectType::BasicCorrection,
        EffectType::Vignette,
        EffectType::ChromaKey,
        EffectType::LumaKey,
    ];
    let mut remaining_effects = effect_count;
    let mut first_clip = None;
    let mut first_effect = None;

    for index in 0..clip_count {
        let track_index = index % video_track_count;
        let lane_index = index / video_track_count;
        let position = TimeCode::new((lane_index as i64) * 96, tb);
        let duration = TimeCode::new(72 + (index % 5) as i64 * 6, tb);
        let asset_id = asset_ids[index % asset_ids.len()];
        let mut clip = if index % 5 == 0 {
            Clip::new_adjustment_layer(asset_id, position, duration)
        } else {
            let color = Color::from_hex(0x244C7A + ((index as u32 * 997) & 0x003F3F));
            Clip::new_solid_color(asset_id, color, position, duration)
        };
        clip.label = Some(format!("Clip {index:04}"));

        let local_effects = remaining_effects.min(if index % 3 == 0 { 3 } else { 1 });
        for effect_offset in 0..local_effects {
            let effect_type = effect_types[(index + effect_offset) % effect_types.len()].clone();
            let effect_id = clip.add_effect_node(EffectNode::with_defaults(effect_type));
            first_effect.get_or_insert(effect_id);
            remaining_effects -= 1;
        }

        first_clip.get_or_insert((sequence.video_tracks[track_index].id, clip.id));
        sequence.video_tracks[track_index].add_clip(clip)?;
    }

    for index in 0..(clip_count / 3).max(audio_track_count) {
        let track_index = index % audio_track_count;
        let lane_index = index / audio_track_count;
        let position = TimeCode::new((lane_index as i64) * 120, tb);
        let duration = TimeCode::new(96, tb);
        let mut clip = Clip::new(asset_ids[index % asset_ids.len()], position, duration);
        clip.label = Some(format!("Audio {index:04}"));
        sequence.audio_tracks[track_index].add_clip(clip)?;
    }

    sequence.playhead = TimeCode::new(0, tb);
    sequence.mark_out(sequence.total_duration().frame.max(1));
    let sequence_id = sequence.id;

    let mut state = AppState::new();
    state.asset_library = Some(library);
    state.active_sequence_id = Some(sequence_id);
    state.default_sequence_id = Some(sequence_id);
    state.sequences = vec![sequence.clone()];
    state.sequence = Some(sequence);

    if let Some((track_id, clip_id)) = first_clip {
        state.replace_clip_selection(vec![SelectedClipRef {
            track_id,
            is_video_track: true,
            clip_id,
        }]);
        if let Some(effect_id) = first_effect {
            let _ = state.select_effect_by_id(clip_id, effect_id);
        }
    }

    Ok(state)
}

fn generate_preview_media_fixture(path: &Path) -> anyhow::Result<bool> {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return Ok(false);
    }

    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc2=size=320x180:rate=30:duration=2")
        .arg("-an")
        .arg("-c:v")
        .arg("mpeg4")
        .arg("-q:v")
        .arg("5")
        .arg(path)
        .status()?;

    Ok(status.success() && path.exists())
}

fn build_preview_media_perf_state(
    root_dir: &Path,
    video_path: &Path,
    frame_count: usize,
) -> anyhow::Result<AppState> {
    let library = AssetLibrary::open(root_dir.join("library"))?;
    let asset_id = library.import_media_file(video_path)?;

    let mut sequence = Sequence::new("Preview media perf");
    sequence.settings.frame_rate = Rational::FPS_30;
    let tb = sequence.time_base();
    let duration = TimeCode::new(frame_count as i64, tb);
    sequence.video_tracks[0].add_clip(Clip::new(asset_id, TimeCode::new(0, tb), duration))?;
    sequence.playhead = TimeCode::new(0, tb);
    sequence.mark_out(frame_count as i64);
    let sequence_id = sequence.id;

    let mut state = AppState::new();
    state.asset_library = Some(library);
    state.active_sequence_id = Some(sequence_id);
    state.default_sequence_id = Some(sequence_id);
    state.sequences = vec![sequence.clone()];
    state.sequence = Some(sequence);
    Ok(state)
}

fn summarize_active_sequence_media_color_issues(
    state: &AppState,
) -> anyhow::Result<VideoColorDiagnosticIssueAggregate> {
    let Some(sequence) = state.sequence.as_ref() else {
        return Ok(VideoColorDiagnosticIssueAggregate::default());
    };
    let Some(library) = state.asset_library.as_ref() else {
        return Ok(VideoColorDiagnosticIssueAggregate::default());
    };

    let mut asset_ids = std::collections::HashSet::new();
    for track in &sequence.video_tracks {
        for clip in &track.clips {
            asset_ids.insert(clip.asset_id);
        }
    }

    let mut aggregate = VideoColorDiagnosticIssueAggregate::default();
    for asset_id in asset_ids {
        let Some(asset) = library.get_asset(asset_id)? else {
            continue;
        };
        let Some(video) = asset.media_info.primary_video() else {
            continue;
        };
        aggregate.observe(&VideoColorDiagnostic::from_stream(video));
    }

    Ok(aggregate)
}

fn wait_for_preview_ready(
    preview_service: &AppUiPreviewService,
    state: &AppState,
    timeout: Duration,
) -> anyhow::Result<()> {
    let started_at = Instant::now();
    loop {
        match preview_service.viewer_preview_for_state(state) {
            ViewerPreviewState::Ready(_) => return Ok(()),
            ViewerPreviewState::Loading | ViewerPreviewState::Stale(_) => {
                let _ = preview_service.poll_finished();
            }
            ViewerPreviewState::Unavailable => {
                let _ = preview_service.poll_finished();
            }
        }
        if started_at.elapsed() > timeout {
            anyhow::bail!(
                "preview frame did not become ready within {} ms; diagnostics: {:?}",
                timeout.as_millis(),
                preview_service.diagnostics()
            );
        }
        thread::sleep(Duration::from_millis(8));
    }
}

fn assert_preview_ready(
    preview_service: &AppUiPreviewService,
    state: &AppState,
) -> anyhow::Result<()> {
    match preview_service.viewer_preview_for_state(state) {
        ViewerPreviewState::Ready(_) => Ok(()),
        other => anyhow::bail!(
            "expected ready preview frame, got {:?}; diagnostics: {:?}",
            other,
            preview_service.diagnostics()
        ),
    }
}

fn record_preview_readiness(counts: &mut PreviewReadinessCounts, state: ViewerPreviewState) {
    match state {
        ViewerPreviewState::Ready(_) => counts.ready += 1,
        ViewerPreviewState::Loading => counts.loading += 1,
        ViewerPreviewState::Stale(_) => counts.stale += 1,
        ViewerPreviewState::Unavailable => counts.unavailable += 1,
    }
}

#[test]
fn preview_color_path_report_summarizes_legacy_and_gpu_blockers() {
    let diagnostics = AppUiPreviewDiagnostics {
        color_composite_plans: 3,
        color_composite_elements: 7,
        color_composite_float_linear: 2,
        color_composite_legacy_rgba8: 1,
        color_composite_legacy_media_transform: 1,
        color_composite_legacy_solid_effect: 2,
        color_stage_gpu_color_stages: 4,
        color_stage_gpu_blockers: 1,
        color_stage_gpu_render_pipeline_blockers: 1,
        color_rgba8_boundary_calls: 3,
        input_color_resolution_override: 2,
        input_color_resolution_data_texture: 13,
        input_color_resolution_detected_metadata: 3,
        input_color_resolution_missing_assume_rec709: 5,
        input_color_resolution_missing_assume_working: 7,
        input_color_resolution_missing_rejected: 11,
        ..AppUiPreviewDiagnostics::default()
    };

    let report = PreviewColorPathReport::from_diagnostics(diagnostics);

    assert_eq!(report.composite_plans, 3);
    assert_eq!(report.composite_elements, 7);
    assert_eq!(report.float_linear_composites, 2);
    assert_eq!(report.legacy_rgba8_composites, 1);
    assert_eq!(report.legacy_reason_total, 3);
    assert_eq!(
        report.legacy_reason_details,
        vec![
            PreviewLegacyReasonReport { layer: "media", reason: "transform", count: 1 },
            PreviewLegacyReasonReport { layer: "solid", reason: "effect", count: 2 },
        ]
    );
    assert_eq!(
        report.input_color_resolution,
        PreviewInputColorResolutionReport {
            override_count: 2,
            data_texture: 13,
            detected_metadata: 3,
            missing_assume_rec709: 5,
            missing_assume_working: 7,
            missing_rejected: 11,
            policy_assumptions: 12,
            explicit_metadata_or_override: 5,
        }
    );
    assert_eq!(report.gpu_color_stages, 4);
    assert_eq!(report.gpu_blockers, 1);
    assert_eq!(
        report.gpu_blocker_breakdown,
        PreviewGpuBlockerBreakdownReport {
            shader_module_not_prepared: 0,
            ocio_resource_bind_group_not_prepared: 0,
            fullscreen_wrapper_not_prepared: 0,
            render_pipeline_not_prepared: 1,
        }
    );
    assert_eq!(report.rgba8_boundary_calls, 3);
    assert!(!report.fully_float_linear);
    assert!(!report.gpu_path_ready);
}

#[test]
fn preview_color_path_report_marks_clean_float_linear_path() {
    let diagnostics = AppUiPreviewDiagnostics {
        color_composite_plans: 2,
        color_composite_elements: 2,
        color_composite_float_linear: 2,
        ..AppUiPreviewDiagnostics::default()
    };

    let report = PreviewColorPathReport::from_diagnostics(diagnostics);

    assert_eq!(report.legacy_reason_total, 0);
    assert!(report.legacy_reason_details.is_empty());
    assert!(report.fully_float_linear);
    assert!(report.gpu_path_ready);
}

#[test]
fn preview_perf_report_serializes_color_health_summary() {
    let diagnostics = AppUiPreviewDiagnostics {
        color_composite_plans: 1,
        color_composite_elements: 1,
        color_composite_float_linear: 1,
        color_stage_total_stages: 1,
        color_stage_cpu_output_stages: 1,
        ..AppUiPreviewDiagnostics::default()
    };
    let preview_color_health = diagnostics.color_health_summary();
    let preview_color_health_budget = PreviewPerfColorHealthBudget::default();
    let preview_color_health_failures =
        evaluate_preview_color_health_budget(preview_color_health, preview_color_health_budget);
    let report = PreviewMediaPerfReport {
        scenario: "preview-color-health-test",
        frames: 1,
        cache_iterations: 1,
        media_color_issues: VideoColorDiagnosticIssueAggregate {
            diagnostics: 1,
            diagnostics_with_detected_color_space: 1,
            method_cicp_tags: 1,
            confidence_high: 1,
            diagnostics_with_raw_cicp_metadata: 1,
            ..VideoColorDiagnosticIssueAggregate::default()
        },
        preview_diagnostics: diagnostics,
        preview_color_health,
        preview_color_health_budget,
        preview_color_health_passed: preview_color_health_failures.is_empty(),
        preview_color_health_failures,
        preview_color_path: PreviewColorPathReport::from_diagnostics(diagnostics),
        cases: Vec::new(),
    };

    let json = serde_json::to_value(&report).expect("serialize report");

    assert_eq!(
        json["preview_color_health"]["fully_float_linear"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        json["preview_color_health"]["gpu_path_ready"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        json["preview_color_health"]["gpu_blocker_breakdown"]["render_pipeline_not_prepared"],
        0
    );
    assert_eq!(
        json["preview_color_health"]["legacy_breakdown"]["media_transform"],
        0
    );
    assert_eq!(json["media_color_issues"]["diagnostics"], 1);
    assert_eq!(json["media_color_issues"]["method_cicp_tags"], 1);
    assert_eq!(json["preview_color_health_passed"], true);
    assert_eq!(
        json["preview_color_health_budget"]["max_legacy_reason_total"],
        0
    );
    assert!(json["preview_color_health_failures"]
        .as_array()
        .expect("preview color-health failures array")
        .is_empty());
}

#[test]
fn preview_color_health_budget_reports_failures() {
    let failures = evaluate_preview_color_health_budget(
        Some(AppUiPreviewColorHealthSummary {
            policy_rejections: 1,
            gpu_blockers: 2,
            transfer_stages: 3,
            legacy_reason_total: 4,
            fully_float_linear: false,
            gpu_path_ready: false,
            ..AppUiPreviewColorHealthSummary::default()
        }),
        PreviewPerfColorHealthBudget::default(),
    );

    assert_eq!(
        failures,
        vec![
            PreviewPerfColorHealthBudgetFailure {
                metric: "fully_float_linear",
                actual: 0,
                limit: 1,
            },
            PreviewPerfColorHealthBudgetFailure { metric: "gpu_path_ready", actual: 0, limit: 1 },
            PreviewPerfColorHealthBudgetFailure { metric: "gpu_blockers", actual: 2, limit: 0 },
            PreviewPerfColorHealthBudgetFailure {
                metric: "legacy_reason_total",
                actual: 4,
                limit: 0,
            },
            PreviewPerfColorHealthBudgetFailure { metric: "transfer_stages", actual: 3, limit: 0 },
            PreviewPerfColorHealthBudgetFailure {
                metric: "policy_rejections",
                actual: 1,
                limit: 0,
            },
        ]
    );

    assert_eq!(
        evaluate_preview_color_health_budget(None, PreviewPerfColorHealthBudget::default()),
        vec![PreviewPerfColorHealthBudgetFailure {
            metric: "color_health_present",
            actual: 0,
            limit: 1,
        }]
    );
}

#[test]
fn viewer_gpu_output_budget_smoke_report_serializes_summary() {
    let jsonl = r#"
{"health":{"status":"Waiting"},"health_counts":{"no_invocation":0,"waiting":1,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":0}}
{"health":{"status":"Ready"},"health_counts":{"no_invocation":0,"waiting":1,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"last_frame_context":{"sequence_id":"seq","frame":12,"width":1280,"height":720,"external_texture_key":"key","output_target":"Display","output_color_space":"Srgb","tone_map":false}}
"#;

    let report = viewer_gpu_output_budget_report_from_jsonl(
        VIEWER_GPU_OUTPUT_BUDGET_SCENARIO,
        "target/perf/viewer-gpu-output.jsonl",
        jsonl,
        &ViewerGpuOutputBudget::default(),
    )
    .expect("viewer GPU output budget report");
    let json = serde_json::to_value(&report).expect("serialize viewer GPU output budget report");

    assert_eq!(json["scenario"], "viewer_gpu_output_budget");
    assert_eq!(json["health_report"]["schema_version"], 1);
    assert_eq!(json["health_report"]["profile"], "viewer_gpu_output_budget");
    assert_eq!(json["health_report"]["verdict"], "Pass");
    assert_eq!(json["summary"]["passed"], true);
    assert_eq!(json["summary"]["records"], 2);
    assert_eq!(json["summary"]["budget"]["min_records"], 1);
    assert_eq!(json["summary"]["counts"]["ready"], 1);
    assert_eq!(json["summary"]["counts"]["waiting"], 1);
    assert_eq!(json["summary"]["display_issues"]["total"], 0);
    assert_eq!(json["summary"]["display_contract_refreshes"]["total"], 0);
    assert_eq!(
        json["summary"]["display_issue_refresh_correlations"]["total"],
        0
    );
    assert_eq!(
        json["summary"]["budget"]["max_display_contract_refreshes"],
        u64::MAX
    );
    assert_eq!(
        json["health_report"]["summary"]["budget"]["max_display_contract_refreshes"],
        u64::MAX
    );
    assert_eq!(
        json["summary"]["budget"]["max_hdr_output_requires_hdr_surface"],
        0
    );
    assert_eq!(json["summary"]["budget"]["max_display_payload_blockers"], 0);
    assert_eq!(json["summary"]["budget"]["max_color_rejections"], 0);
    assert_eq!(json["summary"]["color_rejections"], 0);
    assert_eq!(json["summary"]["reported_counts_match_replay"], true);
    assert_eq!(json["summary"]["last_frame_context"]["frame"], 12);
}

#[test]
fn viewer_gpu_output_display_baseline_budget_defaults_to_refresh_headroom() {
    let _lock = perf_lock().lock().expect("perf lock");
    let key = "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_CONTRACT_REFRESHES";
    let previous = std::env::var(key).ok();

    unsafe {
        std::env::remove_var(key);
    }

    let budget = viewer_gpu_output_display_baseline_budget_from_env();
    assert_eq!(budget.max_display_contract_refreshes, 2);
    assert_eq!(budget.max_display_issue_refresh_correlations, 0);
    assert_eq!(budget.max_display_tone_map_headroom_changes, 0);
    assert_eq!(budget.max_available_surface_format_changes, 0);
    assert_eq!(budget.max_format_color_space_changes, 0);
    assert_eq!(budget.max_present_mode_changes, 0);
    assert_eq!(budget.max_alpha_mode_changes, 0);

    match previous {
        Some(value) => unsafe {
            std::env::set_var(key, value);
        },
        None => unsafe {
            std::env::remove_var(key);
        },
    }
}

#[test]
fn viewer_gpu_output_budget_from_env_reads_reason_thresholds() {
    let _lock = perf_lock().lock().expect("perf lock");
    let keys = [
        "MONDRIAN_VIEWER_GPU_OUTPUT_MIN_RECORDS",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MIN_READY",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_FAILED",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_BLOCKED",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_REJECTED",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DEGRADED",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_WAITING",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_ISSUES",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_HDR_OUTPUT_REQUIRES_HDR_SURFACE",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_OUTPUT_COLOR_SPACE_REQUIRES_SURFACE_COLOR_SPACE",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_RECONFIGURE_BLOCKED_BY_PAYLOAD",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNSUPPORTED_PRESENTATION_INTENT",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNSUPPORTED_SURFACE_CONTRACT",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNKNOWN_DISPLAY_ISSUES",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_CONTRACT_REFRESHES",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_ISSUE_REFRESH_CORRELATIONS",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_TONE_MAP_HEADROOM_CHANGES",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_AVAILABLE_SURFACE_FORMAT_CHANGES",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_FORMAT_COLOR_SPACE_CHANGES",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PRESENT_MODE_CHANGES",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_ALPHA_MODE_CHANGES",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_PAYLOAD_BLOCKERS",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_COLOR_REJECTIONS",
    ];
    let previous = keys
        .iter()
        .map(|key| ((*key).to_owned(), std::env::var(key).ok()))
        .collect::<Vec<_>>();

    unsafe {
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MIN_RECORDS", "3");
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MIN_READY", "4");
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_FAILED", "5");
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_BLOCKED", "6");
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_REJECTED", "7");
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DEGRADED", "8");
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_WAITING", "9");
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_ISSUES", "10");
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_HDR_OUTPUT_REQUIRES_HDR_SURFACE",
            "11",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_OUTPUT_COLOR_SPACE_REQUIRES_SURFACE_COLOR_SPACE",
            "12",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_RECONFIGURE_BLOCKED_BY_PAYLOAD",
            "13",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNSUPPORTED_PRESENTATION_INTENT",
            "14",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNSUPPORTED_SURFACE_CONTRACT",
            "15",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNKNOWN_DISPLAY_ISSUES",
            "16",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_CONTRACT_REFRESHES",
            "17",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_ISSUE_REFRESH_CORRELATIONS",
            "18",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_TONE_MAP_HEADROOM_CHANGES",
            "19",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_AVAILABLE_SURFACE_FORMAT_CHANGES",
            "20",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_FORMAT_COLOR_SPACE_CHANGES",
            "21",
        );
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PRESENT_MODE_CHANGES", "22");
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_ALPHA_MODE_CHANGES", "23");
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_PAYLOAD_BLOCKERS",
            "24",
        );
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_COLOR_REJECTIONS", "25");
    }

    let budget = viewer_gpu_output_budget_from_env();

    for (key, value) in previous {
        unsafe {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }

    assert_eq!(
        budget,
        ViewerGpuOutputBudget {
            min_records: 3,
            min_ready: 4,
            max_failed: 5,
            max_blocked: 6,
            max_rejected: 7,
            max_degraded: 8,
            max_waiting: 9,
            max_display_issues: 10,
            max_hdr_output_requires_hdr_surface: 11,
            max_output_color_space_requires_surface_color_space: 12,
            max_reconfigure_blocked_by_payload: 13,
            max_unsupported_presentation_intent: 14,
            max_unsupported_surface_contract: 15,
            max_unknown_display_issues: 16,
            max_display_contract_refreshes: 17,
            max_display_issue_refresh_correlations: 18,
            max_display_tone_map_headroom_changes: 19,
            max_available_surface_format_changes: 20,
            max_format_color_space_changes: 21,
            max_present_mode_changes: 22,
            max_alpha_mode_changes: 23,
            max_display_payload_blockers: 24,
            max_color_rejections: 25,
        }
    );
}

fn paint_command_count(
    root: &AppUiAppRoot,
    theme: &mondrian_ui_theme::Theme,
    bounds: Rect,
) -> usize {
    let mut encoder = DrawEncoder::new();
    TreeWalker::paint_clipped(root, &mut encoder, theme, bounds);
    encoder.finish().len()
}
