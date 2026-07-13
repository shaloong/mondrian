use super::playback_acceptance::{
    evaluate_professional_playback, PreviewPlaybackMediaProbeReport,
    PreviewProfessionalPlaybackGateReport, ProfessionalPlaybackObservation,
    PROFESSIONAL_MIN_ACCURATE_SEEKS, PROFESSIONAL_MIN_OBSERVED_DURATION_US,
    PROFESSIONAL_MIN_SUPERSEDED_SEEKS, PROFESSIONAL_MIN_WARM_SEEKS,
};
use super::*;
use crate::app::ui_actions::TimelineSeekSource;
use crate::app_ui::native_video_import::resolve_playback_hardware_decode_admission;
use crate::app_ui::panels::{ViewerPreviewSource, ViewerPreviewState};
use crate::app_ui::preview::{
    build_preview_color_health_report, build_preview_decode_performance_report,
    build_preview_decode_performance_report_with_required_access_modes,
    build_preview_render_performance_report, AppUiPreviewColorHealthReport,
    AppUiPreviewColorHealthSummary, AppUiPreviewColorHealthVerdict,
    AppUiPreviewDecodeAccessModeProfile, AppUiPreviewDecodeAccessModeProfiles,
    AppUiPreviewDecodeExecutionSummary, AppUiPreviewDecodePerformanceArea,
    AppUiPreviewDecodePerformanceCheck, AppUiPreviewDecodePerformanceReport,
    AppUiPreviewDecodePerformanceSeverity, AppUiPreviewDecodePerformanceVerdict,
    AppUiPreviewDiagnostics, AppUiPreviewRenderPerformanceReport,
    AppUiPreviewRenderPerformanceSeverity, AppUiPreviewRenderPerformanceVerdict,
    AppUiPreviewService, APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
    APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
};
use crate::app_ui::shell::AppUiAppRoot;
use crate::app_ui::viewer_gpu_output_budget::{
    build_health_report, evaluate_jsonl, ViewerGpuOutputBudget, ViewerGpuOutputHealthReport,
    ViewerGpuOutputHealthVerdict,
};
use crate::app_ui::viewer_gpu_preview_headless::{
    HeadlessViewerGpuAdapter, HeadlessViewerGpuAdapterInfo, HeadlessViewerGpuExecution,
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
use mondrian_media::{
    MediaInfo, PreviewDecodeAccessMode, PreviewDecodeStageDurations, VideoColorDiagnostic,
    VideoColorDiagnosticIssueAggregate,
};
use mondrian_platform::{NativeVideoTextureImportProbe, SystemPlatformService};
use mondrian_renderer::{
    GpuCompositingDiagnostics, GpuViewerSpatialRuntimeDiagnostics, NativeVideoImportCpuTimings,
    RenderColorStageDiagnostics, ViewerGpuExecutionCpuStageTimings,
};
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
    preview_color_report: AppUiPreviewColorHealthReport,
    preview_playback_diagnostics: AppUiPreviewDiagnostics,
    preview_playback_color_report: AppUiPreviewColorHealthReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Serialize)]
struct PreviewMediaPerfReport {
    scenario: &'static str,
    frames: usize,
    cache_iterations: usize,
    media_color_issues: VideoColorDiagnosticIssueAggregate,
    preview_diagnostics: AppUiPreviewDiagnostics,
    preview_color_report: AppUiPreviewColorHealthReport,
    decode_failure_codes: Vec<&'static str>,
    render_failure_codes: Vec<&'static str>,
    preview_decode_report: AppUiPreviewDecodePerformanceReport,
    preview_render_report: AppUiPreviewRenderPerformanceReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Serialize, Default)]
struct PreviewReadinessCounts {
    ready: usize,
    loading: usize,
    stale: usize,
    unavailable: usize,
}

#[derive(Debug, Serialize, Default, PartialEq, Eq)]
struct HeadlessViewerGpuExtent {
    width: u32,
    height: u32,
}

#[derive(Debug, Serialize, Default)]
struct HeadlessViewerGpuExecutionSummary {
    adapter: Option<HeadlessViewerGpuAdapterInfo>,
    rendered_frames: usize,
    cached_frames: usize,
    output_extents: Vec<HeadlessViewerGpuExtent>,
    wall_duration_samples_us: Vec<u64>,
    record_submit_samples_us: Vec<u64>,
    completion_wait_samples_us: Vec<u64>,
    #[serde(skip)]
    input_prepare_samples_us: Vec<u64>,
    #[serde(skip)]
    native_video_import_samples: Vec<NativeVideoImportCpuTimings>,
    #[serde(skip)]
    working_composite_samples_us: Vec<u64>,
    #[serde(skip)]
    spatial_samples_us: Vec<u64>,
    #[serde(skip)]
    output_boundary_samples_us: Vec<u64>,
    #[serde(skip)]
    display_calibration_samples_us: Vec<u64>,
    gpu_duration_samples_us: Vec<u64>,
    #[serde(skip)]
    gpu_timestamp_tokens: Vec<u64>,
    missing_gpu_timestamp_frames: usize,
    discarded_gpu_timestamp_frames: u64,
    fallback_count: usize,
    fallback_reasons: Vec<String>,
    stage_diagnostics: RenderColorStageDiagnostics,
    compositing_diagnostics: GpuCompositingDiagnostics,
    spatial_diagnostics: Option<GpuViewerSpatialRuntimeDiagnostics>,
    rendered_decode_execution: AppUiPreviewDecodeExecutionSummary,
}

impl HeadlessViewerGpuExecutionSummary {
    fn record(&mut self, execution: HeadlessViewerGpuExecution) {
        let output_extent = HeadlessViewerGpuExtent {
            width: execution.output_width,
            height: execution.output_height,
        };
        if !self.output_extents.contains(&output_extent) {
            self.output_extents.push(output_extent);
        }
        if execution.cached {
            self.cached_frames = self.cached_frames.saturating_add(1);
        } else {
            self.rendered_frames = self.rendered_frames.saturating_add(1);
            self.rendered_decode_execution.accumulate(execution.decode_execution);
            self.wall_duration_samples_us.push(execution.duration_us);
            self.record_submit_samples_us.push(execution.record_submit_us);
            self.completion_wait_samples_us.push(execution.completion_wait_us);
            if let Some(timings) = execution.cpu_stage_timings {
                self.input_prepare_samples_us.push(timings.input_prepare_us);
                self.native_video_import_samples.push(timings.native_video_import);
                self.working_composite_samples_us.push(timings.working_composite_us);
                self.spatial_samples_us.push(timings.spatial_us);
                self.output_boundary_samples_us.push(timings.output_boundary_us);
                self.display_calibration_samples_us.push(timings.display_calibration_us);
            }
            if let Some(token) = execution.gpu_timestamp_token {
                self.gpu_timestamp_tokens.push(token);
            } else {
                self.missing_gpu_timestamp_frames =
                    self.missing_gpu_timestamp_frames.saturating_add(1);
            }
        }
        self.fallback_count = self.fallback_count.saturating_add(execution.fallback_reasons.len());
        self.fallback_reasons.extend(execution.fallback_reasons);
        if let Some(diagnostics) = execution.stage_diagnostics {
            self.stage_diagnostics.accumulate(diagnostics);
        }
        if let Some(diagnostics) = execution.compositing_diagnostics {
            self.compositing_diagnostics.accumulate(diagnostics);
        }
        if let Some(diagnostics) = execution.spatial_diagnostics {
            self.spatial_diagnostics = Some(diagnostics);
        }
    }

    fn record_gpu_timings(&mut self, timings: &[(u64, u64)]) {
        self.gpu_duration_samples_us.extend(
            timings
                .iter()
                .filter(|(token, _)| self.gpu_timestamp_tokens.contains(token))
                .map(|(_, duration_us)| *duration_us),
        );
    }

    fn p95_duration_us(&self) -> u64 {
        p95_sample_us(&self.gpu_duration_samples_us)
    }

    fn p95_record_submit_us(&self) -> u64 {
        p95_sample_us(&self.record_submit_samples_us)
    }

    fn p95_completion_wait_us(&self) -> u64 {
        p95_sample_us(&self.completion_wait_samples_us)
    }

    fn p95_wall_duration_us(&self) -> u64 {
        p95_sample_us(&self.wall_duration_samples_us)
    }

    fn p95_cpu_stages(&self) -> ViewerGpuExecutionCpuStageTimings {
        ViewerGpuExecutionCpuStageTimings {
            input_prepare_us: p95_sample_us(&self.input_prepare_samples_us),
            native_video_import: p95_native_video_import(&self.native_video_import_samples),
            working_composite_us: p95_sample_us(&self.working_composite_samples_us),
            spatial_us: p95_sample_us(&self.spatial_samples_us),
            output_boundary_us: p95_sample_us(&self.output_boundary_samples_us),
            display_calibration_us: p95_sample_us(&self.display_calibration_samples_us),
        }
    }
}

fn p95_native_video_import(samples: &[NativeVideoImportCpuTimings]) -> NativeVideoImportCpuTimings {
    fn field(
        samples: &[NativeVideoImportCpuTimings],
        read: impl Fn(&NativeVideoImportCpuTimings) -> u64,
    ) -> u64 {
        p95_sample_us(&samples.iter().map(read).collect::<Vec<_>>())
    }

    NativeVideoImportCpuTimings {
        source_validation_us: field(samples, |sample| sample.source_validation_us),
        bridge_acquire_us: field(samples, |sample| sample.bridge_acquire_us),
        pipeline_prepare_us: field(samples, |sample| sample.pipeline_prepare_us),
        yuv_record_us: field(samples, |sample| sample.yuv_record_us),
        color_stage_us: field(samples, |sample| sample.color_stage_us),
        resource_extract_us: field(samples, |sample| sample.resource_extract_us),
        submit_us: field(samples, |sample| sample.submit_us),
        total_us: field(samples, |sample| sample.total_us),
    }
}

fn p95_sample_us(samples: &[u64]) -> u64 {
    let mut samples = samples.to_vec();
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let rank = samples.len().saturating_mul(95).saturating_add(99) / 100;
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

#[test]
fn headless_gpu_summary_records_distinct_executed_extents() {
    let mut summary = HeadlessViewerGpuExecutionSummary::default();
    for (width, height) in [(960, 540), (960, 540), (480, 270)] {
        summary.record(HeadlessViewerGpuExecution {
            output_width: width,
            output_height: height,
            cached: false,
            duration_us: 1,
            record_submit_us: 1,
            completion_wait_us: 1,
            gpu_timestamp_token: Some(u64::from(width) << 32 | u64::from(height)),
            cpu_stage_timings: Some(ViewerGpuExecutionCpuStageTimings::default()),
            compositing_diagnostics: None,
            spatial_diagnostics: None,
            stage_diagnostics: None,
            fallback_reasons: Vec::new(),
            decode_execution: AppUiPreviewDecodeExecutionSummary::default(),
        });
    }

    assert_eq!(
        summary.output_extents,
        vec![
            HeadlessViewerGpuExtent { width: 960, height: 540 },
            HeadlessViewerGpuExtent { width: 480, height: 270 },
        ]
    );
}

#[test]
fn professional_frame_count_covers_full_duration_after_initial_observation() {
    assert_eq!(
        professional_min_frame_count_for_interval(40_000_000).expect("25 fps interval"),
        45_001
    );
}

#[derive(Debug, Serialize)]
struct PreviewMediaPlaybackPerfReport {
    scenario: &'static str,
    frames: usize,
    frame_interval_ns: u64,
    media_probe: PreviewPlaybackMediaProbeReport,
    readiness: PreviewReadinessCounts,
    headless_gpu_preroll: HeadlessViewerGpuExecutionSummary,
    headless_gpu: HeadlessViewerGpuExecutionSummary,
    real_media_gates: Option<PreviewExternalPlaybackGateReport>,
    professional_media_gates: Option<PreviewProfessionalPlaybackGateReport>,
    media_color_issues: VideoColorDiagnosticIssueAggregate,
    preview_diagnostics: AppUiPreviewDiagnostics,
    preview_color_report: AppUiPreviewColorHealthReport,
    decode_failure_codes: Vec<&'static str>,
    render_failure_codes: Vec<&'static str>,
    preview_decode_report: AppUiPreviewDecodePerformanceReport,
    preview_render_report: Option<AppUiPreviewRenderPerformanceReport>,
    playback_evidence: PlaybackEvidenceReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Clone, Serialize)]
struct PreviewExternalPlaybackGateReport {
    enabled: bool,
    playback_decode_p95_limit_us: u64,
    playback_decode_p95_observed_us: u64,
    playback_queue_wait_p95_limit_us: u64,
    playback_queue_wait_p95_observed_us: u64,
    min_visible_frames: usize,
    visible_frames: usize,
    min_ready_frames: usize,
    ready_frames: usize,
    min_ready_basis_points: usize,
    ready_basis_points: usize,
    gpu_rendered_frames: usize,
    gpu_cached_frames: usize,
    gpu_timestamped_frames: usize,
    gpu_missing_timestamp_frames: usize,
    gpu_discarded_timestamp_frames: u64,
    gpu_execution_p95_limit_us: u64,
    gpu_execution_p95_observed_us: u64,
    gpu_record_submit_p95_us: u64,
    gpu_completion_wait_p95_us: u64,
    gpu_wall_duration_p95_us: u64,
    gpu_cpu_stage_p95_us: ViewerGpuExecutionCpuStageTimings,
    gpu_readback_stages: u64,
    gpu_blockers: u64,
    gpu_fallback_count: usize,
    delivery_clock_drift_limit_us: u64,
    delivery_clock_drift_observed_us: u64,
    audio_underrun_recoveries: u64,
    dropped_playback_evidence_events: u64,
    cpu_frame_store_within_budget: bool,
    decoder_resource_store_within_budget: bool,
    cpu_frame_store_oversize_rejections: u64,
    passed: bool,
    failures: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct ViewerGpuOutputHealthSmokeReport {
    scenario: &'static str,
    report: ViewerGpuOutputHealthReport,
}

const VIEWER_GPU_OUTPUT_BUDGET_SCENARIO: &str = "viewer_gpu_output_budget";
const VIEWER_GPU_OUTPUT_DISPLAY_BASELINE_SCENARIO: &str = "viewer_gpu_output_display_baseline";

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

fn preview_decode_hard_failures(report: &AppUiPreviewDecodePerformanceReport) -> Vec<&'static str> {
    let mut failures = Vec::new();
    if report.verdict != AppUiPreviewDecodePerformanceVerdict::Fail {
        return failures;
    }
    failures.extend(
        report
            .checks
            .iter()
            .filter(|check| {
                check.severity
                    == crate::app_ui::preview::AppUiPreviewDecodePerformanceSeverity::Fail
            })
            .map(|check| check.code),
    );
    failures.extend(report.root_causes.iter().map(|root| root.code));
    failures.push("preview_decode_report_failed");
    failures.sort_unstable();
    failures.dedup();
    failures
}

fn preview_render_hard_failures(report: &AppUiPreviewRenderPerformanceReport) -> Vec<&'static str> {
    let mut failures = Vec::new();
    if report.verdict != AppUiPreviewRenderPerformanceVerdict::Fail {
        return failures;
    }
    failures.extend(
        report
            .checks
            .iter()
            .filter(|check| check.severity == AppUiPreviewRenderPerformanceSeverity::Fail)
            .map(|check| check.code),
    );
    failures.extend(
        report
            .root_causes
            .iter()
            .filter(|root| root.severity == AppUiPreviewRenderPerformanceSeverity::Fail)
            .map(|root| root.code),
    );
    failures.push("preview_render_report_failed");
    failures.sort_unstable();
    failures.dedup();
    failures
}

fn preview_decode_required_access_mode_failures(
    report: &AppUiPreviewDecodePerformanceReport,
) -> Vec<&'static str> {
    if report.summary.is_none() {
        return vec!["preview_decode_report_missing_summary"];
    };

    let mut failures = Vec::new();
    for check in &report.checks {
        if check.severity != crate::app_ui::preview::AppUiPreviewDecodePerformanceSeverity::Fail {
            continue;
        }
        match check.code {
            "preview_decode_playback_cursor_sampled" => {
                failures.push("preview_decode_playback_cursor_not_sampled");
            }
            "preview_decode_playback_cursor_mode_local_sampled" => {
                failures.push("preview_decode_playback_cursor_cache_only");
            }
            "preview_decode_scrub_cursor_sampled" => {
                failures.push("preview_decode_scrub_cursor_not_sampled");
            }
            "preview_decode_scrub_cursor_mode_local_sampled" => {
                failures.push("preview_decode_scrub_cursor_cache_only");
            }
            "preview_decode_random_access_still_sampled" => {
                failures.push("preview_decode_random_access_still_not_sampled");
            }
            "preview_decode_random_access_still_mode_local_sampled" => {
                failures.push("preview_decode_random_access_still_cache_only");
            }
            _ => {}
        }
    }
    failures
}

fn preview_decode_access_mode_queue_wait_failures(
    report: &AppUiPreviewDecodePerformanceReport,
    access_modes: &[PreviewDecodeAccessMode],
) -> Vec<&'static str> {
    let Some(summary) = report.summary.as_ref() else {
        return vec!["preview_decode_report_missing_summary"];
    };

    let mut failures = Vec::new();
    for access_mode in access_modes {
        let profile =
            preview_decode_profile_for_access_mode(&summary.access_mode_profiles, *access_mode);
        if profile.queue_wait_max_us <= summary.slow_frame_budget_us {
            continue;
        }
        failures.push(preview_decode_access_mode_queue_wait_failure_code(
            *access_mode,
        ));
    }
    failures
}

fn preview_decode_profile_for_access_mode(
    profiles: &AppUiPreviewDecodeAccessModeProfiles,
    access_mode: PreviewDecodeAccessMode,
) -> AppUiPreviewDecodeAccessModeProfile {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => profiles.playback_cursor,
        PreviewDecodeAccessMode::ScrubCursor => profiles.scrub_cursor,
        PreviewDecodeAccessMode::RandomAccessStillFrame => profiles.random_access_still,
    }
}

fn preview_decode_access_mode_queue_wait_failure_code(
    access_mode: PreviewDecodeAccessMode,
) -> &'static str {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => {
            "preview_decode_playback_cursor_queue_wait_over_budget"
        }
        PreviewDecodeAccessMode::ScrubCursor => {
            "preview_decode_scrub_cursor_queue_wait_over_budget"
        }
        PreviewDecodeAccessMode::RandomAccessStillFrame => {
            "preview_decode_random_access_still_queue_wait_over_budget"
        }
    }
}

fn preview_playback_decode_failures(
    report: &AppUiPreviewDecodePerformanceReport,
) -> Vec<&'static str> {
    let mut failures = preview_decode_required_access_mode_failures(report);
    let mut scoped_fail_check = !failures.is_empty();
    for check in &report.checks {
        if check.severity != AppUiPreviewDecodePerformanceSeverity::Fail {
            continue;
        }
        let playback_scoped = check.code.starts_with("preview_decode_playback_")
            || matches!(
                check.code,
                "preview_decode_timeout_failures"
                    | "preview_decode_forward_budget_exhausted_failures"
                    | "preview_decode_worker_queue_full_drops"
                    | "preview_decode_worker_disconnected_drops"
                    | "preview_decode_queue_invalid_access_mode_drops"
                    | "preview_decode_invalid_access_mode_requests"
            );
        if playback_scoped {
            failures.push(check.code);
            scoped_fail_check = true;
        }
    }
    failures.extend(preview_decode_access_mode_queue_wait_failures(
        report,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    ));
    for root in &report.root_causes {
        match root.code {
            "preview_decode_playback_session_not_reused"
            | "preview_decode_playback_without_locality"
            | "preview_decode_playback_sustained_pressure" => failures.push(root.code),
            _ => {}
        }
    }
    if scoped_fail_check && report.verdict == AppUiPreviewDecodePerformanceVerdict::Fail {
        failures.push("preview_decode_report_failed");
    }
    failures.sort_unstable();
    failures.dedup();
    failures
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
        max_os_display_profile_unsupported: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_OS_DISPLAY_PROFILE_UNSUPPORTED",
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
        max_missing_runtime_reports: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_RUNTIME_REPORTS",
            0,
        ),
        max_missing_stage_reports: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_STAGE_REPORTS",
            0,
        ),
        max_ready_records_missing_preview_candidate_context: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_READY_RECORDS_MISSING_PREVIEW_CANDIDATE_CONTEXT",
            u64::MAX,
        ),
        max_preview_candidate_id_regressions: env_u64(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PREVIEW_CANDIDATE_ID_REGRESSIONS",
            u64::MAX,
        ),
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

fn viewer_gpu_output_budget_report_from_jsonl(
    scenario: &'static str,
    source_path: impl Into<String>,
    contents: &str,
    budget: &ViewerGpuOutputBudget,
) -> anyhow::Result<ViewerGpuOutputHealthSmokeReport> {
    let summary = evaluate_jsonl(contents, budget)?;
    let source_path = source_path.into();
    let report = build_health_report(summary, scenario, Some(source_path));
    Ok(ViewerGpuOutputHealthSmokeReport { scenario, report })
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
    eprintln!("MONDRIAN_VIEWER_GPU_OUTPUT_HEALTH_REPORT_JSON={report_json}");
    write_report_if_needed(&report_json);
    if report.report.verdict == ViewerGpuOutputHealthVerdict::Fail {
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
    eprintln!("MONDRIAN_VIEWER_GPU_OUTPUT_DISPLAY_BASELINE_HEALTH_REPORT_JSON={report_json}");
    write_report_if_needed(&report_json);
    if report.report.verdict == ViewerGpuOutputHealthVerdict::Fail {
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
            preview_color_report: build_preview_color_health_report(
                preview_diagnostics.color_health_summary(),
                "app_ui_scale_preview",
            ),
            preview_playback_diagnostics,
            preview_playback_color_report: build_preview_color_health_report(
                preview_playback_diagnostics.color_health_summary(),
                "app_ui_scale_preview_playback",
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
    let scrub_threshold_ms = env_u128("MONDRIAN_PREVIEW_MEDIA_SCRUB_READY_MS", 8_000);
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

    let result = run_preview_media_access_mode_probe(
        &root_dir,
        &video_path,
        "preview_media_decode_cache",
        frame_count,
        cache_iterations,
        first_frame_threshold_ms,
        cache_threshold_ms,
        gpu_candidate_threshold_ms,
        sequential_threshold_ms,
        scrub_threshold_ms,
        ready_timeout,
        None,
    );

    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    validate_preview_media_access_mode_report(&report, &report_json)?;

    Ok(())
}

fn run_preview_media_access_mode_probe(
    root_dir: &Path,
    video_path: &Path,
    scenario: &'static str,
    frame_count: usize,
    cache_iterations: usize,
    first_frame_threshold_ms: u128,
    cache_threshold_ms: u128,
    gpu_candidate_threshold_ms: u128,
    sequential_threshold_ms: u128,
    scrub_threshold_ms: u128,
    ready_timeout: Duration,
    overall_deadline: Option<Instant>,
) -> anyhow::Result<PreviewMediaPerfReport> {
    run_preview_media_access_mode_probe_with_media_info(
        root_dir,
        video_path,
        None,
        scenario,
        frame_count,
        cache_iterations,
        first_frame_threshold_ms,
        cache_threshold_ms,
        gpu_candidate_threshold_ms,
        sequential_threshold_ms,
        scrub_threshold_ms,
        ready_timeout,
        overall_deadline,
    )
}

fn run_preview_media_access_mode_probe_with_media_info(
    root_dir: &Path,
    video_path: &Path,
    media_info: Option<MediaInfo>,
    scenario: &'static str,
    frame_count: usize,
    cache_iterations: usize,
    first_frame_threshold_ms: u128,
    cache_threshold_ms: u128,
    gpu_candidate_threshold_ms: u128,
    sequential_threshold_ms: u128,
    scrub_threshold_ms: u128,
    ready_timeout: Duration,
    overall_deadline: Option<Instant>,
) -> anyhow::Result<PreviewMediaPerfReport> {
    ensure_preview_media_access_mode_deadline(overall_deadline, scenario, None)?;
    let mut state = match media_info {
        Some(media_info) => build_preview_media_perf_state_with_media_info(
            root_dir,
            video_path,
            Some(media_info),
            frame_count,
        )?,
        None => build_preview_media_perf_state(root_dir, video_path, frame_count)?,
    };
    let preview_service = AppUiPreviewService::new();
    ensure_preview_media_access_mode_deadline(overall_deadline, scenario, Some(&preview_service))?;

    let first_frame_case = run_case(
        "preview_media.first_frame_ready",
        1,
        first_frame_threshold_ms,
        || {
            state.seek(0);
            wait_for_preview_ready_until(
                &preview_service,
                &state,
                ready_timeout,
                overall_deadline,
                scenario,
            )
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

    let scrub_case = run_case(
        "preview_media.active_scrub_ready_window",
        1,
        scrub_threshold_ms,
        || {
            for frame in 0..frame_count {
                state.seek_with_source(frame as i64, TimelineSeekSource::PointerDrag);
                wait_for_preview_ready_until(
                    &preview_service,
                    &state,
                    ready_timeout,
                    overall_deadline,
                    scenario,
                )?;
            }
            state.seek(frame_count.saturating_sub(1) as i64);
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
                wait_for_preview_ready_until(
                    &preview_service,
                    &state,
                    ready_timeout,
                    overall_deadline,
                    scenario,
                )?;
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
    let preview_color_report =
        build_preview_color_health_report(preview_diagnostics.color_health_summary(), scenario);
    let preview_decode_report = build_preview_decode_performance_report_with_required_access_modes(
        preview_diagnostics
            .decode_performance_summary(APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US),
        scenario,
        APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
        &[
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ],
    );
    let preview_render_report = build_preview_render_performance_report(
        preview_diagnostics
            .render_performance_summary(APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US),
        scenario,
        APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
    );
    let decode_failure_codes = preview_decode_hard_failures(&preview_decode_report);
    let render_failure_codes = preview_render_hard_failures(&preview_render_report);
    Ok(PreviewMediaPerfReport {
        scenario,
        frames: frame_count,
        cache_iterations,
        media_color_issues,
        preview_diagnostics,
        preview_color_report,
        decode_failure_codes,
        render_failure_codes,
        preview_decode_report,
        preview_render_report,
        cases: vec![
            first_frame_case,
            cached_frame_case,
            scrub_case,
            sequential_case,
            gpu_candidate_case,
        ],
    })
}

fn ensure_preview_media_access_mode_deadline(
    overall_deadline: Option<Instant>,
    scenario: &str,
    preview_service: Option<&AppUiPreviewService>,
) -> anyhow::Result<()> {
    if overall_deadline.is_some_and(|deadline| Instant::now() > deadline) {
        if let Some(preview_service) = preview_service {
            anyhow::bail!(
                "preview access-mode probe exceeded total timeout in scenario {scenario}; diagnostics: {:?}",
                preview_service.diagnostics()
            );
        }
        anyhow::bail!(
            "preview access-mode probe exceeded total timeout in scenario {scenario} before preview diagnostics were available"
        );
    }
    Ok(())
}

fn validate_preview_media_access_mode_report(
    report: &PreviewMediaPerfReport,
    report_json: &str,
) -> anyhow::Result<()> {
    let failed_cases: Vec<_> =
        report.cases.iter().filter(|case| !case.passed).map(|case| case.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "preview media performance smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }
    if report.preview_color_report.verdict == AppUiPreviewColorHealthVerdict::Fail {
        anyhow::bail!("preview media color report failed: {report_json}");
    }
    let access_mode_coverage_failures =
        preview_decode_required_access_mode_failures(&report.preview_decode_report);
    if !access_mode_coverage_failures.is_empty() {
        anyhow::bail!(
            "preview media decode access-mode coverage failed: {:?}; report: {}",
            access_mode_coverage_failures,
            report_json
        );
    }
    if !report.decode_failure_codes.is_empty() {
        anyhow::bail!(
            "preview media decode report failed: {:?}; report: {}",
            report.decode_failure_codes,
            report_json
        );
    }
    if !report.render_failure_codes.is_empty() {
        anyhow::bail!(
            "preview media render report failed: {:?}; report: {}",
            report.render_failure_codes,
            report_json
        );
    }
    let queue_wait_failures = preview_decode_access_mode_queue_wait_failures(
        &report.preview_decode_report,
        &[
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ],
    );
    if !queue_wait_failures.is_empty() {
        anyhow::bail!(
            "preview media interactive decode queue wait failed: {:?}; report: {}",
            queue_wait_failures,
            report_json
        );
    }
    Ok(())
}

#[test]
#[ignore = "development preview media access-mode smoke for a real external media file; run manually"]
fn preview_media_external_access_mode_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let Some(video_path) =
        std::env::var_os("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH").map(std::path::PathBuf::from)
    else {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"preview_media_external_access_mode\",\"skipped\":\"MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH not set\"}}"
        );
        return Ok(());
    };
    anyhow::ensure!(
        video_path.exists(),
        "MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH does not exist: {}",
        video_path.display()
    );

    let frame_count = env_usize_clamped("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_FRAMES", 24, 2, 300);
    let cache_iterations =
        env_usize_clamped("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_CACHE_ITERS", 6, 1, 120);
    let first_frame_threshold_ms =
        env_u128("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_FIRST_READY_MS", 30_000);
    let cache_threshold_ms = env_u128("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_CACHE_REFRESH_MS", 2_000);
    let gpu_candidate_threshold_ms =
        env_u128("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_GPU_CANDIDATE_MS", 2_000);
    let sequential_threshold_ms =
        env_u128("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_SEQUENCE_READY_MS", 30_000);
    let scrub_threshold_ms = env_u128("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_SCRUB_READY_MS", 30_000);
    let ready_timeout = Duration::from_millis(env_u128(
        "MONDRIAN_PREVIEW_EXTERNAL_MEDIA_READY_TIMEOUT_MS",
        30_000,
    ) as u64);
    let overall_timeout = Duration::from_millis(env_u128(
        "MONDRIAN_PREVIEW_EXTERNAL_MEDIA_TOTAL_TIMEOUT_MS",
        120_000,
    ) as u64);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_preview_external_media_{uniq}"));
    fs::create_dir_all(&root_dir)?;

    let overall_deadline = Instant::now() + overall_timeout;
    let media_info = probe_external_preview_media_info(&video_path)?;

    let result = run_preview_media_access_mode_probe_with_media_info(
        &root_dir,
        &video_path,
        Some(media_info),
        "preview_media_external_access_mode",
        frame_count,
        cache_iterations,
        first_frame_threshold_ms,
        cache_threshold_ms,
        gpu_candidate_threshold_ms,
        sequential_threshold_ms,
        scrub_threshold_ms,
        ready_timeout,
        Some(overall_deadline),
    );
    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);
    validate_preview_media_access_mode_report(&report, &report_json)?;

    Ok(())
}

#[test]
#[ignore = "development preview media continuous playback smoke test; run manually"]
fn preview_media_continuous_playback_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let frame_count = env_usize_clamped("MONDRIAN_PREVIEW_PLAYBACK_FRAMES", 60, 8, 60);
    let frame_interval_ns = (env_usize_clamped("MONDRIAN_PREVIEW_PLAYBACK_FRAME_MS", 33, 1, 250)
        as u64)
        .saturating_mul(1_000_000);
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

    let result = run_preview_media_continuous_playback_probe(
        &root_dir,
        &video_path,
        None,
        "preview_media_continuous_playback",
        frame_count,
        frame_interval_ns,
        playback_threshold_ms,
        gpu_candidate_threshold_ms,
        ready_timeout,
        0,
    );

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
    if report.preview_color_report.verdict == AppUiPreviewColorHealthVerdict::Fail {
        anyhow::bail!("preview media playback color report failed: {report_json}");
    }
    if !report.decode_failure_codes.is_empty() {
        anyhow::bail!(
            "preview media continuous playback decode report failed: {:?}; report: {}",
            report.decode_failure_codes,
            report_json
        );
    }
    if !report.render_failure_codes.is_empty() {
        anyhow::bail!(
            "preview media continuous playback render report failed: {:?}; report: {}",
            report.render_failure_codes,
            report_json
        );
    }

    Ok(())
}

#[test]
#[ignore = "development preview media continuous playback smoke for a real external media file; run manually"]
fn preview_media_external_continuous_playback_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let Some(video_path) = std::env::var_os("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_MEDIA_PATH")
        .or_else(|| std::env::var_os("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH"))
        .map(std::path::PathBuf::from)
    else {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"preview_media_external_continuous_playback\",\"skipped\":\"MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_MEDIA_PATH not set\"}}"
        );
        return Ok(());
    };
    run_external_continuous_playback_gate(video_path, false)
}

#[test]
#[ignore = "professional 4K25/30 HEVC Main10 hardware playback gate; requires real media and GPU"]
fn preview_media_4k_hevc_main10_hardware_playback_gate() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let video_path = std::env::var_os("MONDRIAN_PREVIEW_4K_HEVC_MAIN10_MEDIA_PATH")
        .map(std::path::PathBuf::from)
        .context("MONDRIAN_PREVIEW_4K_HEVC_MAIN10_MEDIA_PATH is required; this gate never skips")?;
    run_external_continuous_playback_gate(video_path, true)
}

fn run_external_continuous_playback_gate(
    video_path: PathBuf,
    professional: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        video_path.exists(),
        "external playback media path does not exist: {}",
        video_path.display()
    );

    let media_info = probe_external_preview_media_info(&video_path)?;
    let media_probe = PreviewPlaybackMediaProbeReport::from_media_info(&media_info)?;
    let professional_min_frames = professional
        .then(|| professional_min_frame_count(&media_probe))
        .transpose()?
        .unwrap_or(8);
    let default_frame_count = if professional {
        professional_min_frames
    } else {
        60
    };
    let max_frame_count = if professional {
        professional_min_frames.saturating_mul(2).max(professional_min_frames)
    } else {
        1_800
    };
    let frame_count = env_usize_clamped(
        "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_FRAMES",
        default_frame_count,
        professional_min_frames,
        max_frame_count,
    );
    let frame_interval_ns = std::env::var("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_FRAME_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|milliseconds| milliseconds.saturating_mul(1_000_000))
        .unwrap_or(media_probe.frame_interval_ns()?);
    let playback_threshold_ms = env_u128("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_WINDOW_MS", 8_000);
    let gpu_candidate_threshold_ms =
        env_u128("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_GPU_CANDIDATE_MS", 2_000);
    let ready_timeout = Duration::from_millis(env_u128(
        "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_READY_TIMEOUT_MS",
        30_000,
    ) as u64);
    let default_overall_timeout_ms = if professional {
        40 * 60 * 1_000
    } else {
        180_000
    };
    let overall_timeout = Duration::from_millis(env_u128(
        "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_TOTAL_TIMEOUT_MS",
        default_overall_timeout_ms,
    ) as u64);
    let playback_p95_limit_us = env_u64("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_P95_US", 40_000);
    let playback_queue_wait_p95_limit_us = env_u64(
        "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_QUEUE_WAIT_P95_US",
        10_000,
    );
    let min_visible_percent = env_usize_clamped(
        "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_VISIBLE_PERCENT",
        95,
        1,
        100,
    );
    let min_ready_basis_points = env_usize_clamped(
        "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_READY_BASIS_POINTS",
        9_950,
        1,
        10_000,
    );

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_preview_external_playback_{uniq}"));
    fs::create_dir_all(&root_dir)?;

    let deadline = Instant::now() + overall_timeout;
    let scenario = if professional {
        "preview_media_4k_hevc_main10_hardware_playback"
    } else {
        "preview_media_external_continuous_playback"
    };
    let result = run_preview_media_continuous_playback_probe(
        &root_dir,
        &video_path,
        Some(media_info),
        scenario,
        frame_count,
        frame_interval_ns,
        playback_threshold_ms,
        gpu_candidate_threshold_ms,
        ready_timeout,
        if professional {
            PROFESSIONAL_MIN_WARM_SEEKS.saturating_add(PROFESSIONAL_MIN_ACCURATE_SEEKS) as usize
        } else {
            0
        },
    );
    let _ = fs::remove_dir_all(&root_dir);

    anyhow::ensure!(
        Instant::now() <= deadline,
        "external playback smoke exceeded total timeout {:?}",
        overall_timeout
    );
    let mut report = result?;
    let real_media_gates = evaluate_external_playback_gates(
        &report.readiness,
        &report.headless_gpu,
        report.frames,
        report.frame_interval_ns.saturating_add(999) / 1_000,
        &report.preview_decode_report,
        &report.preview_diagnostics,
        &report.playback_evidence,
        playback_p95_limit_us,
        playback_queue_wait_p95_limit_us,
        min_visible_percent,
        min_ready_basis_points,
    );
    report.real_media_gates = Some(real_media_gates);
    if professional {
        let playback_decode = report
            .preview_decode_report
            .summary
            .as_ref()
            .map(|summary| summary.access_mode_profiles.playback_cursor)
            .unwrap_or_default();
        report.professional_media_gates = Some(evaluate_professional_playback(
            ProfessionalPlaybackObservation {
                media: &report.media_probe,
                rendered_decode_execution: report.headless_gpu.rendered_decode_execution,
                viewer_fallback_count: report.headless_gpu.fallback_count,
                viewer_fallback_reasons: &report.headless_gpu.fallback_reasons,
                playback_decode,
                playback_evidence: &report.playback_evidence,
                preview_diagnostics: &report.preview_diagnostics,
                frames: report.frames,
                frame_interval_ns: report.frame_interval_ns,
            },
            90,
        ));
    }
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> =
        report.cases.iter().filter(|case| !case.passed).map(|case| case.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "preview media external continuous playback smoke failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }
    if report.preview_color_report.verdict == AppUiPreviewColorHealthVerdict::Fail {
        anyhow::bail!("preview media external playback color report failed: {report_json}");
    }
    if !report.decode_failure_codes.is_empty() {
        anyhow::bail!(
            "preview media external continuous playback decode report failed: {:?}; report: {}",
            report.decode_failure_codes,
            report_json
        );
    }
    if !report.render_failure_codes.is_empty() {
        anyhow::bail!(
            "preview media external continuous playback render report failed: {:?}; report: {}",
            report.render_failure_codes,
            report_json
        );
    }
    if let Some(gates) = &report.real_media_gates {
        if !gates.passed {
            anyhow::bail!(
                "preview media external continuous playback real-media gates failed: {:?}; report: {}",
                gates.failures,
                report_json
            );
        }
    }
    if let Some(gates) = &report.professional_media_gates {
        if !gates.passed {
            anyhow::bail!(
                "professional 4K HEVC Main10 hardware playback gates failed: {:?}; report: {}",
                gates.failures,
                report_json
            );
        }
    }

    Ok(())
}

fn evaluate_external_playback_gates(
    readiness: &PreviewReadinessCounts,
    headless_gpu: &HeadlessViewerGpuExecutionSummary,
    frames: usize,
    gpu_execution_p95_limit_us: u64,
    decode_report: &AppUiPreviewDecodePerformanceReport,
    preview_diagnostics: &AppUiPreviewDiagnostics,
    playback_evidence: &PlaybackEvidenceReport,
    playback_decode_p95_limit_us: u64,
    playback_queue_wait_p95_limit_us: u64,
    min_visible_percent: usize,
    min_ready_basis_points: usize,
) -> PreviewExternalPlaybackGateReport {
    let playback_decode_p95_observed_us =
        decode_check_observed(decode_report, "preview_decode_playback_cursor_p95_frame_us");
    let playback_queue_wait_p95_observed_us = decode_check_observed(
        decode_report,
        "preview_decode_playback_cursor_queue_wait_p95_us",
    );
    let visible_frames = readiness.ready.saturating_add(readiness.stale);
    let min_visible_frames = frames.saturating_mul(min_visible_percent).saturating_add(99) / 100;
    let min_ready_frames =
        frames.saturating_mul(min_ready_basis_points).saturating_add(9_999) / 10_000;
    let ready_basis_points = readiness.ready.saturating_mul(10_000) / frames.max(1);
    let mut failures = Vec::new();
    if playback_decode_p95_observed_us == 0
        || playback_decode_p95_observed_us > playback_decode_p95_limit_us
    {
        failures.push("playback_decode_p95");
    }
    if playback_queue_wait_p95_observed_us > playback_queue_wait_p95_limit_us {
        failures.push("playback_queue_wait_p95");
    }
    if visible_frames < min_visible_frames {
        failures.push("visible_frame_ratio");
    }
    if readiness.ready < min_ready_frames {
        failures.push("current_ready_ratio");
    }
    if headless_gpu.rendered_frames == 0
        || headless_gpu.rendered_frames.saturating_add(headless_gpu.cached_frames)
            < min_ready_frames
    {
        failures.push("viewer_gpu_execution_coverage");
    }
    if headless_gpu.stage_diagnostics.readback_stages > 0 {
        failures.push("viewer_gpu_readback");
    }
    if headless_gpu.stage_diagnostics.gpu_blockers > 0 {
        failures.push("viewer_gpu_blockers");
    }
    if headless_gpu.missing_gpu_timestamp_frames > 0
        || headless_gpu.gpu_duration_samples_us.len() != headless_gpu.rendered_frames
    {
        failures.push("viewer_gpu_timestamp_coverage");
    }
    let gpu_execution_p95_observed_us = headless_gpu.p95_duration_us();
    if gpu_execution_p95_observed_us == 0
        || gpu_execution_p95_observed_us > gpu_execution_p95_limit_us
    {
        failures.push("viewer_gpu_execution_p95");
    }
    let delivery_clock_drift_limit_us = 20_000;
    let delivery_clock_drift_observed_us = playback_evidence.delivery_clock_drift.max_us;
    if delivery_clock_drift_observed_us > delivery_clock_drift_limit_us {
        failures.push("delivery_clock_drift");
    }
    if playback_evidence.audio_underrun_recoveries > 0 {
        failures.push("audio_underrun_recovery");
    }
    if playback_evidence.dropped_event_count > 0 {
        failures.push("playback_evidence_overflow");
    }
    let cpu_frame_store_within_budget = preview_diagnostics.media_cache_reserved_bytes
        <= preview_diagnostics.media_cache_byte_budget
        && preview_diagnostics.pinned_media_frame_bytes
            <= preview_diagnostics.media_cache_byte_budget
        && preview_diagnostics.viewer_frame_cache_reserved_bytes
            <= preview_diagnostics.viewer_frame_cache_byte_budget
        && preview_diagnostics.pinned_viewer_frame_bytes
            <= preview_diagnostics.viewer_frame_cache_byte_budget;
    if !cpu_frame_store_within_budget {
        failures.push("cpu_frame_store_budget");
    }
    let decoder_resource_store_within_budget = preview_diagnostics.media_cache_resource_units
        <= preview_diagnostics.media_cache_resource_unit_budget;
    if !decoder_resource_store_within_budget {
        failures.push("decoder_resource_store_budget");
    }
    let cpu_frame_store_oversize_rejections = preview_diagnostics
        .media_cache_oversize_rejections
        .saturating_add(preview_diagnostics.viewer_frame_cache_oversize_rejections);
    if cpu_frame_store_oversize_rejections > 0 {
        failures.push("cpu_frame_store_oversize_rejection");
    }

    PreviewExternalPlaybackGateReport {
        enabled: true,
        playback_decode_p95_limit_us,
        playback_decode_p95_observed_us,
        playback_queue_wait_p95_limit_us,
        playback_queue_wait_p95_observed_us,
        min_visible_frames,
        visible_frames,
        min_ready_frames,
        ready_frames: readiness.ready,
        min_ready_basis_points,
        ready_basis_points,
        gpu_rendered_frames: headless_gpu.rendered_frames,
        gpu_cached_frames: headless_gpu.cached_frames,
        gpu_timestamped_frames: headless_gpu.gpu_duration_samples_us.len(),
        gpu_missing_timestamp_frames: headless_gpu.missing_gpu_timestamp_frames,
        gpu_discarded_timestamp_frames: headless_gpu.discarded_gpu_timestamp_frames,
        gpu_execution_p95_limit_us,
        gpu_execution_p95_observed_us,
        gpu_record_submit_p95_us: headless_gpu.p95_record_submit_us(),
        gpu_completion_wait_p95_us: headless_gpu.p95_completion_wait_us(),
        gpu_wall_duration_p95_us: headless_gpu.p95_wall_duration_us(),
        gpu_cpu_stage_p95_us: headless_gpu.p95_cpu_stages(),
        gpu_readback_stages: headless_gpu.stage_diagnostics.readback_stages,
        gpu_blockers: headless_gpu.stage_diagnostics.gpu_blockers,
        gpu_fallback_count: headless_gpu.fallback_count,
        delivery_clock_drift_limit_us,
        delivery_clock_drift_observed_us,
        audio_underrun_recoveries: playback_evidence.audio_underrun_recoveries,
        dropped_playback_evidence_events: playback_evidence.dropped_event_count,
        cpu_frame_store_within_budget,
        decoder_resource_store_within_budget,
        cpu_frame_store_oversize_rejections,
        passed: failures.is_empty(),
        failures,
    }
}

fn decode_check_observed(report: &AppUiPreviewDecodePerformanceReport, code: &'static str) -> u64 {
    report
        .checks
        .iter()
        .find(|check| check.code == code)
        .map(|check| check.observed)
        .unwrap_or(0)
}

fn run_preview_media_continuous_playback_probe(
    root_dir: &Path,
    video_path: &Path,
    media_info: Option<MediaInfo>,
    scenario: &'static str,
    frame_count: usize,
    frame_interval_ns: u64,
    playback_threshold_ms: u128,
    gpu_candidate_threshold_ms: u128,
    ready_timeout: Duration,
    seek_probe_count: usize,
) -> anyhow::Result<PreviewMediaPlaybackPerfReport> {
    let media_info = match media_info {
        Some(media_info) => media_info,
        None => MediaInfo::probe(video_path)
            .with_context(|| format!("probe playback media {}", video_path.display()))?,
    };
    let media_probe = PreviewPlaybackMediaProbeReport::from_media_info(&media_info)?;
    let mut state = build_preview_media_perf_state_with_media_info(
        root_dir,
        video_path,
        Some(media_info),
        frame_count,
    )?;
    state.begin_playback_evidence_run(mondrian_playback::PlaybackEvidenceConfig {
        event_capacity: frame_count
            .saturating_mul(6)
            .saturating_add(seek_probe_count.saturating_mul(8))
            .saturating_add(1_024),
        sample_capacity: frame_count.saturating_add(seek_probe_count).saturating_add(1_024),
    })?;
    let preview_service = AppUiPreviewService::new();
    let mut gpu_adapter =
        HeadlessViewerGpuAdapter::new().context("create real headless Viewer GPU Adapter")?;
    configure_headless_gpu_decode_admission(&preview_service, &gpu_adapter);
    let mut readiness = PreviewReadinessCounts::default();
    let mut headless_gpu_preroll = HeadlessViewerGpuExecutionSummary::default();
    let mut headless_gpu = HeadlessViewerGpuExecutionSummary::default();
    headless_gpu_preroll.adapter = Some(gpu_adapter.adapter_info().clone());
    headless_gpu.adapter = Some(gpu_adapter.adapter_info().clone());

    state.seek(0);
    wait_for_headless_gpu_ready(
        &preview_service,
        &mut state,
        &mut gpu_adapter,
        &mut headless_gpu_preroll,
        ready_timeout,
    )?;
    state.play();
    anyhow::ensure!(
        execute_headless_gpu_candidate(
            &preview_service,
            &mut state,
            &mut gpu_adapter,
            &mut headless_gpu,
        )? == HeadlessGpuCandidateStatus::Ready,
        "headless GPU pre-roll did not satisfy the initial playback Frame Demand"
    );
    wait_for_headless_playback_preroll(&preview_service, &mut state, ready_timeout)?;
    let playback_case = run_case(
        "preview_media.continuous_playback_readiness",
        1,
        playback_threshold_ms,
        || {
            for _ in 0..frame_count {
                state.advance_playback_clock(Duration::from_nanos(frame_interval_ns));
                let sample = run_headless_preview_interval(
                    &preview_service,
                    &mut state,
                    &mut gpu_adapter,
                    &mut headless_gpu,
                    Duration::from_nanos(frame_interval_ns),
                )?;
                record_headless_preview_readiness(&mut readiness, sample);
            }
            apply_headless_preview_outcome(&preview_service, &mut state);
            Ok(())
        },
    )?;
    let seek_case = (seek_probe_count > 0)
        .then(|| {
            run_case(
                "preview_media.cross_region_seek_readiness",
                1,
                u128::from(seek_probe_count as u64).saturating_mul(1_000),
                || {
                    run_headless_cross_region_seeks(
                        &preview_service,
                        &mut state,
                        &mut gpu_adapter,
                        &mut headless_gpu,
                        frame_count,
                        seek_probe_count,
                        ready_timeout,
                    )
                },
            )
        })
        .transpose()?;
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
    let gpu_timings = gpu_adapter
        .finish_gpu_timings()
        .context("finish deferred headless Viewer GPU timestamp maps")?;
    headless_gpu_preroll.record_gpu_timings(&gpu_timings);
    headless_gpu.record_gpu_timings(&gpu_timings);
    headless_gpu.discarded_gpu_timestamp_frames = gpu_adapter.discarded_gpu_timings();

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
    anyhow::ensure!(
        headless_gpu.rendered_frames > 0,
        "continuous playback produced no real headless GPU executions"
    );
    anyhow::ensure!(
        headless_gpu.stage_diagnostics.readback_stages == 0,
        "headless Viewer GPU execution introduced readback stages: {:?}",
        headless_gpu.stage_diagnostics
    );
    anyhow::ensure!(
        headless_gpu.stage_diagnostics.gpu_blockers == 0,
        "headless Viewer GPU execution reported blockers: {:?}",
        headless_gpu.stage_diagnostics
    );

    let preview_diagnostics = preview_service.diagnostics();
    let media_color_issues = summarize_active_sequence_media_color_issues(&state)?;
    let preview_color_report =
        build_preview_color_health_report(preview_diagnostics.color_health_summary(), scenario);
    let preview_decode_report = build_preview_decode_performance_report_with_required_access_modes(
        preview_diagnostics
            .decode_performance_summary(APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US),
        scenario,
        APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    );
    let preview_render_report = preview_diagnostics
        .render_performance_summary(APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US)
        .map(|summary| {
            build_preview_render_performance_report(
                Some(summary),
                scenario,
                APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
            )
        });
    let decode_failure_codes = preview_playback_decode_failures(&preview_decode_report);
    let render_failure_codes = preview_render_report
        .as_ref()
        .map(preview_render_hard_failures)
        .unwrap_or_default();
    let playback_evidence = state.playback_evidence_report();
    let report = PreviewMediaPlaybackPerfReport {
        scenario,
        frames: frame_count,
        frame_interval_ns,
        media_probe,
        readiness,
        headless_gpu_preroll,
        headless_gpu,
        real_media_gates: None,
        professional_media_gates: None,
        media_color_issues,
        preview_diagnostics,
        preview_color_report,
        decode_failure_codes,
        render_failure_codes,
        preview_decode_report,
        preview_render_report,
        playback_evidence,
        cases: std::iter::once(playback_case)
            .chain(seek_case)
            .chain(std::iter::once(gpu_candidate_case))
            .collect(),
    };
    validate_executed_adaptive_scaling(&report)?;
    Ok(report)
}

fn professional_min_frame_count(media: &PreviewPlaybackMediaProbeReport) -> anyhow::Result<usize> {
    professional_min_frame_count_for_interval(media.frame_interval_ns()?)
}

fn professional_min_frame_count_for_interval(interval_ns: u64) -> anyhow::Result<usize> {
    anyhow::ensure!(
        interval_ns > 0,
        "professional playback interval must be positive"
    );
    let required_ns = u128::from(PROFESSIONAL_MIN_OBSERVED_DURATION_US).saturating_mul(1_000);
    let frames = required_ns
        .saturating_add(u128::from(interval_ns).saturating_sub(1))
        .checked_div(u128::from(interval_ns))
        .unwrap_or(u128::MAX)
        .saturating_add(1);
    usize::try_from(frames).context("professional playback frame count exceeds usize")
}

fn run_headless_cross_region_seeks(
    preview_service: &AppUiPreviewService,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    frame_count: usize,
    seek_count: usize,
    timeout_per_seek: Duration,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        frame_count > seek_count,
        "seek fixture has too few distinct regions"
    );
    let target_span = frame_count.saturating_sub(1);
    for index in 0..seek_count {
        let target = index
            .saturating_add(1)
            .saturating_mul(target_span)
            .checked_div(seek_count.saturating_add(1))
            .unwrap_or(0);
        let source = if index % 2 == 0 {
            TimelineSeekSource::PointerDrag
        } else {
            TimelineSeekSource::Settled
        };
        state.seek_with_source(target as i64, source);
        wait_for_headless_gpu_ready(
            preview_service,
            state,
            gpu_adapter,
            gpu_summary,
            timeout_per_seek,
        )?;
    }

    let supersession_count = usize::try_from(PROFESSIONAL_MIN_SUPERSEDED_SEEKS)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    anyhow::ensure!(
        frame_count > supersession_count,
        "seek fixture has too few latest-wins regions"
    );
    for index in 0..supersession_count {
        let target = supersession_count
            .saturating_sub(index)
            .saturating_mul(target_span)
            .checked_div(supersession_count.saturating_add(1))
            .unwrap_or(0);
        let source = if index % 2 == 0 {
            TimelineSeekSource::PointerDrag
        } else {
            TimelineSeekSource::Settled
        };
        state.seek_with_source(target as i64, source);
        let _ = preview_service.gpu_preview_frame_for_state(state);
        thread::yield_now();
    }
    wait_for_headless_gpu_ready(
        preview_service,
        state,
        gpu_adapter,
        gpu_summary,
        timeout_per_seek,
    )?;
    wait_for_preview_work_quiescence(preview_service, state, timeout_per_seek)?;
    Ok(())
}

fn wait_for_preview_work_quiescence(
    preview_service: &AppUiPreviewService,
    state: &mut AppState,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        apply_headless_preview_outcome(preview_service, state);
        let diagnostics = preview_service.diagnostics();
        if diagnostics.scheduler.pending_requests == 0
            && diagnostics.worker_queue.queued_jobs == 0
            && diagnostics.worker_queue.in_flight_jobs == 0
        {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "preview work did not return to zero residency after latest-wins seek burst: {:?}",
            diagnostics
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn validate_executed_adaptive_scaling(
    report: &PreviewMediaPlaybackPerfReport,
) -> anyhow::Result<()> {
    let Some(summary) = report.preview_decode_report.summary.as_ref() else {
        return Ok(());
    };
    let profile = summary.access_mode_profiles.playback_cursor;
    let requested = profile
        .hardware_decode_prefer_hardware_requested_frames
        .saturating_add(profile.hardware_decode_prefer_gpu_requested_frames)
        .saturating_add(profile.hardware_decode_require_gpu_requested_frames);
    let effective = profile
        .hardware_decode_cpu_transfer_observed_frames
        .saturating_add(profile.hardware_decode_gpu_resident_native_frames);
    let not_engaged = requested.saturating_sub(effective);
    let pressure_threshold = mondrian_playback::PlaybackPolicy::default().pressure_threshold as u64;
    let quarter_evidence_threshold = pressure_threshold.saturating_mul(2);
    if not_engaged < quarter_evidence_threshold {
        return Ok(());
    }

    anyhow::ensure!(
        report.playback_evidence.deliveries.degraded >= quarter_evidence_threshold,
        "hardware fallback execution did not reach Playback Quality Policy: requested={requested}, effective={effective}, degraded={}",
        report.playback_evidence.deliveries.degraded
    );
    let full = report
        .headless_gpu
        .output_extents
        .iter()
        .max_by_key(|extent| u64::from(extent.width).saturating_mul(u64::from(extent.height)))
        .context("adaptive playback report contains no executed GPU extent")?;
    let expected_half = HeadlessViewerGpuExtent {
        width: full.width.div_ceil(2),
        height: full.height.div_ceil(2),
    };
    let expected_quarter = HeadlessViewerGpuExtent {
        width: full.width.div_ceil(4),
        height: full.height.div_ceil(4),
    };
    anyhow::ensure!(
        report.headless_gpu.output_extents.contains(&expected_half)
            && report.headless_gpu.output_extents.contains(&expected_quarter),
        "hardware fallback changed policy state without executing Half/Quarter GPU extents: {:?}",
        report.headless_gpu.output_extents
    );
    Ok(())
}

fn configure_headless_gpu_decode_admission(
    preview_service: &AppUiPreviewService,
    gpu_adapter: &HeadlessViewerGpuAdapter,
) {
    let admission = resolve_playback_hardware_decode_admission(
        &gpu_adapter.native_import_support(),
        &SystemPlatformService.native_video_texture_import(),
    );
    preview_service.set_playback_hardware_decode_admission(
        admission.request,
        admission.hardware_decode_device_selector,
        admission.renderer_native_import_ready,
        admission.platform_native_import_ready,
        admission.native_import_admission_ready,
        admission.admission_blocker,
        admission.platform_discovery_available,
        admission.platform_zero_copy_supported,
        admission.platform_low_copy_fallback_supported,
        admission.renderer_supported_handle_kinds,
        admission.renderer_supported_source_texture_formats,
    );
}

struct HeadlessPreviewSample {
    current_gpu_ready: bool,
    stale_output_available: bool,
    unavailable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadlessGpuCandidateStatus {
    Ready,
    Loading,
    Unavailable,
}

fn wait_for_headless_gpu_ready(
    preview_service: &AppUiPreviewService,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        apply_headless_preview_outcome(preview_service, state);
        match execute_headless_gpu_candidate(preview_service, state, gpu_adapter, gpu_summary)? {
            HeadlessGpuCandidateStatus::Ready => return Ok(()),
            HeadlessGpuCandidateStatus::Loading | HeadlessGpuCandidateStatus::Unavailable => {}
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "timed out waiting for a real headless Viewer GPU output; diagnostics: {:?}",
            preview_service.diagnostics()
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_for_headless_playback_preroll(
    preview_service: &AppUiPreviewService,
    state: &mut AppState,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last_tick = Instant::now();
    while state.is_playback_priming() {
        apply_headless_preview_outcome(preview_service, state);
        let now = Instant::now();
        state.advance_playback_clock(now.saturating_duration_since(last_tick));
        last_tick = now;
        anyhow::ensure!(
            now < deadline,
            "timed out waiting for bounded playback video preroll; diagnostics: {:?}",
            preview_service.diagnostics()
        );
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn run_headless_preview_interval(
    preview_service: &AppUiPreviewService,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    interval: Duration,
) -> anyhow::Result<HeadlessPreviewSample> {
    let deadline = Instant::now() + interval;
    let mut candidate_status = HeadlessGpuCandidateStatus::Loading;

    loop {
        apply_headless_preview_outcome(preview_service, state);
        if candidate_status != HeadlessGpuCandidateStatus::Ready {
            candidate_status =
                execute_headless_gpu_candidate(preview_service, state, gpu_adapter, gpu_summary)?;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(1)));
    }

    Ok(HeadlessPreviewSample {
        current_gpu_ready: candidate_status == HeadlessGpuCandidateStatus::Ready,
        stale_output_available: gpu_adapter.has_presented_output(),
        unavailable: candidate_status == HeadlessGpuCandidateStatus::Unavailable,
    })
}

fn execute_headless_gpu_candidate(
    preview_service: &AppUiPreviewService,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
) -> anyhow::Result<HeadlessGpuCandidateStatus> {
    match preview_service.gpu_preview_frame_for_state(state) {
        crate::app_ui::preview::AppUiGpuPreviewFrameState::Ready(frame) => {
            let execution = gpu_adapter
                .execute(&frame)
                .context("execute current Viewer frame on the real headless GPU Adapter")?;
            gpu_summary.record(execution);
            if let Some(ticket) = frame.presentation_ticket() {
                state.complete_frame_presentation(ticket, Instant::now());
            }
            observe_headless_video_preroll(preview_service, state);
            Ok(HeadlessGpuCandidateStatus::Ready)
        }
        crate::app_ui::preview::AppUiGpuPreviewFrameState::Current => {
            if let Some(ticket) = preview_service.playback_presentation_ticket(state) {
                state.complete_frame_presentation(ticket, Instant::now());
            }
            observe_headless_video_preroll(preview_service, state);
            Ok(HeadlessGpuCandidateStatus::Ready)
        }
        crate::app_ui::preview::AppUiGpuPreviewFrameState::Loading => {
            Ok(HeadlessGpuCandidateStatus::Loading)
        }
        crate::app_ui::preview::AppUiGpuPreviewFrameState::Unavailable => {
            Ok(HeadlessGpuCandidateStatus::Unavailable)
        }
    }
}

fn record_headless_preview_readiness(
    counts: &mut PreviewReadinessCounts,
    sample: HeadlessPreviewSample,
) {
    if sample.current_gpu_ready {
        counts.ready = counts.ready.saturating_add(1);
    } else if sample.stale_output_available {
        counts.stale = counts.stale.saturating_add(1);
    } else if sample.unavailable {
        counts.unavailable = counts.unavailable.saturating_add(1);
    } else {
        counts.loading = counts.loading.saturating_add(1);
    }
}
fn apply_headless_preview_outcome(
    preview_service: &AppUiPreviewService,
    state: &mut AppState,
) -> bool {
    let mut outcome = preview_service.poll_finished_outcome();
    outcome.merge(preview_service.expire_stalled_realtime_current());
    for delivery in outcome.frame_deliveries.iter().copied() {
        state.observe_frame_delivery(delivery);
    }
    observe_headless_video_preroll(preview_service, state);
    outcome.visible_change
}

fn observe_headless_video_preroll(
    preview_service: &AppUiPreviewService,
    state: &mut AppState,
) -> bool {
    preview_service
        .playback_video_preroll_readiness(state)
        .is_some_and(|readiness| {
            state.observe_video_preroll(
                readiness.ready_media_frames,
                readiness.available_media_frames,
            )
        })
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
    sequence.audio_program = mondrian_timeline::AudioProgram::for_tracks(
        sequence.audio_tracks.iter().map(|track| track.id),
    );

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
        let position = tt((lane_index as i64) * 96, tb);
        let duration = tt(72 + (index % 5) as i64 * 6, tb);
        let asset_id = asset_ids[index % asset_ids.len()];
        let mut clip = if index % 5 == 0 {
            Clip::new_adjustment_layer(asset_id, position, duration).expect("valid clip")
        } else {
            let color = Color::from_hex(0x244C7A + ((index as u32 * 997) & 0x003F3F));
            Clip::new_solid_color(asset_id, color, position, duration).expect("valid clip")
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
        let position = tt((lane_index as i64) * 120, tb);
        let duration = tt(96, tb);
        let mut clip =
            Clip::new(asset_ids[index % asset_ids.len()], position, duration).expect("valid clip");
        clip.label = Some(format!("Audio {index:04}"));
        sequence.audio_tracks[track_index].add_clip(clip)?;
    }

    sequence.playhead = tt(0, tb);
    sequence.mark_out(sequence.total_duration().expect("valid duration"));
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
        .arg("-color_range")
        .arg("tv")
        .arg("-colorspace")
        .arg("bt709")
        .arg("-color_primaries")
        .arg("bt709")
        .arg("-color_trc")
        .arg("bt709")
        .arg(path)
        .status()?;

    Ok(status.success() && path.exists())
}

fn build_preview_media_perf_state(
    root_dir: &Path,
    video_path: &Path,
    frame_count: usize,
) -> anyhow::Result<AppState> {
    build_preview_media_perf_state_with_media_info(root_dir, video_path, None, frame_count)
}

fn build_preview_media_perf_state_with_media_info(
    root_dir: &Path,
    video_path: &Path,
    media_info: Option<MediaInfo>,
    frame_count: usize,
) -> anyhow::Result<AppState> {
    let library = AssetLibrary::open(root_dir.join("library"))?;
    let probed_frame_rate = media_info
        .as_ref()
        .and_then(MediaInfo::primary_video)
        .filter(|video| {
            video.frame_rate_proven && video.frame_rate.num > 0 && video.frame_rate.den > 0
        })
        .map(|video| video.frame_rate.reduce());
    let asset_id = match media_info {
        Some(info) => library.upsert_media_file_with_info(video_path, info)?,
        None => library.import_media_file(video_path)?,
    };

    let mut sequence = Sequence::new("Preview media perf");
    sequence.settings.frame_rate = probed_frame_rate.unwrap_or(Rational::FPS_30);
    let tb = sequence.time_base();
    let duration = tt(frame_count as i64, tb);
    sequence.video_tracks[0]
        .add_clip(Clip::new(asset_id, tt(0, tb), duration).expect("valid clip"))?;
    sequence.playhead = tt(0, tb);
    sequence.mark_out(duration);
    let sequence_id = sequence.id;

    let mut state = AppState::new();
    state.asset_library = Some(library);
    state.active_sequence_id = Some(sequence_id);
    state.default_sequence_id = Some(sequence_id);
    state.sequences = vec![sequence.clone()];
    state.sequence = Some(sequence);
    Ok(state)
}

fn probe_external_preview_media_info(video_path: &Path) -> anyhow::Result<MediaInfo> {
    MediaInfo::probe(video_path)
        .with_context(|| format!("probe external preview media {}", video_path.display()))
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

fn wait_for_preview_ready_until(
    preview_service: &AppUiPreviewService,
    state: &AppState,
    timeout: Duration,
    overall_deadline: Option<Instant>,
    scenario: &str,
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
        if overall_deadline.is_some_and(|deadline| Instant::now() > deadline) {
            anyhow::bail!(
                "preview access-mode probe exceeded total timeout in scenario {scenario}; diagnostics: {:?}",
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

#[test]
fn preview_color_report_summarizes_legacy_and_gpu_blockers() {
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
        input_color_resolution_missing_rejected: 11,
        ..AppUiPreviewDiagnostics::default()
    };

    let report =
        build_preview_color_health_report(diagnostics.color_health_summary(), "preview-test");

    let summary = report.summary.expect("preview color summary");
    assert_eq!(report.verdict, AppUiPreviewColorHealthVerdict::Fail);
    assert_eq!(summary.composite_plans, 3);
    assert_eq!(summary.float_linear_composites, 2);
    assert_eq!(summary.legacy_rgba8_composites, 1);
    assert_eq!(summary.legacy_reason_total, 3);
    assert_eq!(summary.override_count, 2);
    assert_eq!(summary.data_textures, 13);
    assert_eq!(summary.detected_metadata, 3);
    assert_eq!(summary.policy_assumptions, 5);
    assert_eq!(summary.explicit_metadata_or_override, 5);
    assert_eq!(summary.gpu_color_stages, 4);
    assert_eq!(summary.gpu_blockers, 1);
    assert_eq!(
        summary.gpu_blocker_breakdown.render_pipeline_not_prepared,
        1
    );
    assert_eq!(summary.rgba8_boundary_calls, 3);
    assert!(!summary.fully_float_linear);
    assert!(!summary.gpu_path_ready);
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_gpu_color_stage_blocked"));
    assert!(report.root_causes.iter().any(|root| root.code == "legacy_rgba8_composite_path"));
}

#[test]
fn preview_color_report_marks_clean_float_linear_path() {
    let diagnostics = AppUiPreviewDiagnostics {
        color_composite_plans: 2,
        color_composite_elements: 2,
        color_composite_float_linear: 2,
        ..AppUiPreviewDiagnostics::default()
    };

    let report =
        build_preview_color_health_report(diagnostics.color_health_summary(), "preview-clean");
    let summary = report.summary.expect("preview color summary");

    assert_eq!(report.verdict, AppUiPreviewColorHealthVerdict::Pass);
    assert_eq!(summary.legacy_reason_total, 0);
    assert!(summary.fully_float_linear);
    assert!(summary.gpu_path_ready);
}

#[test]
fn preview_decode_hard_failures_include_failed_report() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_rgba_frames: 1,
        decode_total_duration_us: 80_000,
        decode_max_duration_us: 80_000,
        decode_last_duration_us: 80_000,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            random_access_still: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                total_duration_us: 80_000,
                max_duration_us: 80_000,
                last_duration_us: 80_000,
                session_opened_frames: 1,
                stage_durations: PreviewDecodeStageDurations {
                    packet_decode_us: 75_000,
                    ..PreviewDecodeStageDurations::default()
                },
                max_frame_stage_durations: PreviewDecodeStageDurations {
                    packet_decode_us: 75_000,
                    ..PreviewDecodeStageDurations::default()
                },
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-random-access-hard-failure-test",
        50_000,
    );

    assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
    let failures = preview_decode_hard_failures(&report);
    assert!(failures.contains(&"preview_decode_report_failed"));
    assert!(failures.contains(&"preview_decode_max_frame_us"));
    assert!(failures.contains(&"preview_decode_frame_over_budget"));
    assert!(failures.contains(&"preview_decode_access_mode_over_budget"));
}

#[test]
fn preview_render_hard_failures_include_checks_and_root_causes() {
    let diagnostics = AppUiPreviewDiagnostics {
        render_timed_frames: 1,
        render_total_duration_us: 90_000,
        render_max_duration_us: 90_000,
        render_last_duration_us: 90_000,
        render_stage_durations: crate::app_ui::preview::AppUiPreviewRenderStageDurations {
            cpu_output_boundary_us: 80_000,
            ..crate::app_ui::preview::AppUiPreviewRenderStageDurations::default()
        },
        render_max_frame_stage_durations:
            crate::app_ui::preview::AppUiPreviewRenderStageDurations {
                cpu_output_boundary_us: 80_000,
                ..crate::app_ui::preview::AppUiPreviewRenderStageDurations::default()
            },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_render_performance_report(
        diagnostics.render_performance_summary(50_000),
        "preview-render-hard-failure-test",
        50_000,
    );

    let failures = preview_render_hard_failures(&report);

    assert!(failures.contains(&"preview_render_report_failed"));
    assert!(failures.contains(&"preview_render_max_frame_us"));
    assert!(failures.contains(&"preview_render_frame_over_budget"));
    assert!(failures.contains(&"preview_render_cpu_output_boundary_bound"));
}

#[test]
fn preview_media_decode_access_mode_coverage_requires_scrub_and_still_samples() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_rgba_frames: 1,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            random_access_still: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-access-mode-coverage-test",
        50_000,
        &[
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ],
    );

    assert_eq!(
        preview_decode_required_access_mode_failures(&report),
        vec!["preview_decode_scrub_cursor_not_sampled"]
    );
}

#[test]
fn preview_media_decode_access_mode_coverage_passes_with_scrub_and_still_samples() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_rgba_frames: 2,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                bounded_any_seek_strategy_frames: 1,
                any_seek_window_ms_max: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            random_access_still: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-access-mode-coverage-test",
        50_000,
        &[
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ],
    );

    assert!(preview_decode_required_access_mode_failures(&report).is_empty());
}

#[test]
fn preview_media_decode_access_mode_coverage_rejects_cache_only_samples() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 1,
        decode_cache_hit_frames: 1,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                cache_hit_frames: 1,
                bounded_any_seek_strategy_frames: 1,
                any_seek_window_ms_max: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-access-mode-cache-only-coverage-test",
        50_000,
        &[PreviewDecodeAccessMode::ScrubCursor],
    );

    assert_eq!(
        preview_decode_required_access_mode_failures(&report),
        vec!["preview_decode_scrub_cursor_cache_only"]
    );
}

#[test]
fn preview_decode_access_mode_queue_wait_failures_are_scoped_by_mode() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_rgba_frames: 2,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                queue_wait_total_us: 70_000,
                queue_wait_max_us: 70_000,
                queue_wait_last_us: 70_000,
                bounded_any_seek_strategy_frames: 1,
                any_seek_window_ms_max: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            random_access_still: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                queue_wait_total_us: 10_000,
                queue_wait_max_us: 10_000,
                queue_wait_last_us: 10_000,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-interactive-queue-wait-test",
        50_000,
    );

    assert_eq!(
        preview_decode_access_mode_queue_wait_failures(
            &report,
            &[
                PreviewDecodeAccessMode::ScrubCursor,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ],
        ),
        vec!["preview_decode_scrub_cursor_queue_wait_over_budget"]
    );
    assert!(preview_decode_access_mode_queue_wait_failures(
        &report,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    )
    .is_empty());
}

#[test]
fn preview_playback_decode_failures_include_required_playback_coverage() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_rgba_frames: 1,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            random_access_still: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-coverage-test",
        50_000,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    );

    let failures = preview_playback_decode_failures(&report);

    assert!(failures.contains(&"preview_decode_playback_cursor_not_sampled"));
    assert!(failures.contains(&"preview_decode_report_failed"));
}

#[test]
fn preview_playback_decode_failures_include_playback_queue_wait_regressions() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_rgba_frames: 1,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                queue_wait_total_us: 85_000,
                queue_wait_max_us: 85_000,
                queue_wait_last_us: 85_000,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                queue_wait_total_us: 90_000,
                queue_wait_max_us: 90_000,
                queue_wait_last_us: 90_000,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-queue-wait-test",
        50_000,
    );

    let failures = preview_playback_decode_failures(&report);

    assert!(failures.contains(&"preview_decode_playback_cursor_queue_wait_over_budget"));
    assert!(!failures.contains(&"preview_decode_scrub_cursor_queue_wait_over_budget"));
}

#[test]
fn preview_playback_decode_failures_include_sustained_pressure() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_rgba_frames: 1,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        playback_schedule: crate::app_ui::preview::AppUiPreviewPlaybackScheduleDiagnostics {
            sustained_pressure_active: true,
            sustained_pressure_events: 1,
            current_late_streak: 2,
            ..crate::app_ui::preview::AppUiPreviewPlaybackScheduleDiagnostics::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-pressure-test",
        50_000,
    );

    let failures = preview_playback_decode_failures(&report);

    assert!(failures.contains(&"preview_decode_playback_sustained_pressure"));
    assert!(!failures.contains(&"preview_decode_report_failed"));
}

#[test]
fn preview_playback_decode_failures_include_locality_regressions() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_rgba_frames: 2,
        decode_total_duration_us: 80_000,
        decode_max_duration_us: 45_000,
        decode_last_duration_us: 35_000,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                frames: 2,
                in_process_cpu_rgba_frames: 2,
                total_duration_us: 80_000,
                max_duration_us: 45_000,
                last_duration_us: 35_000,
                seeked_frames: 2,
                session_opened_frames: 2,
                decoded_frame_count: 96,
                max_decoded_frame_count: 48,
                stage_durations: PreviewDecodeStageDurations {
                    seek_us: 20_000,
                    packet_decode_us: 55_000,
                    ..PreviewDecodeStageDurations::default()
                },
                max_frame_stage_durations: PreviewDecodeStageDurations {
                    seek_us: 10_000,
                    packet_decode_us: 30_000,
                    ..PreviewDecodeStageDurations::default()
                },
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-locality-test",
        50_000,
    );

    let failures = preview_playback_decode_failures(&report);

    assert!(failures.contains(&"preview_decode_playback_session_not_reused"));
    assert!(failures.contains(&"preview_decode_playback_without_locality"));
    assert!(!failures.contains(&"preview_decode_report_failed"));
}

#[test]
fn preview_playback_decode_failures_allow_non_playback_warnings() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_rgba_frames: 1,
        decode_total_duration_us: 12_000,
        decode_max_duration_us: 12_000,
        decode_last_duration_us: 12_000,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            scrub_cursor: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                total_duration_us: 12_000,
                max_duration_us: 12_000,
                last_duration_us: 12_000,
                session_opened_frames: 1,
                bounded_any_seek_strategy_frames: 1,
                any_seek_window_ms_max: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-non-playback-warn-test",
        50_000,
    );

    assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Warn);
    assert!(preview_playback_decode_failures(&report).is_empty());
}

#[test]
fn preview_playback_decode_failures_ignore_slow_random_still_startup() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_rgba_frames: 2,
        decode_total_duration_us: 130_000,
        decode_max_duration_us: 120_000,
        decode_last_duration_us: 10_000,
        decode_access_mode_profiles: AppUiPreviewDecodeAccessModeProfiles {
            playback_cursor: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                total_duration_us: 10_000,
                max_duration_us: 10_000,
                last_duration_us: 10_000,
                session_reused_frames: 1,
                forward_reused_frames: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            random_access_still: AppUiPreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_rgba_frames: 1,
                total_duration_us: 120_000,
                max_duration_us: 120_000,
                last_duration_us: 120_000,
                session_opened_frames: 1,
                ..AppUiPreviewDecodeAccessModeProfile::default()
            },
            ..AppUiPreviewDecodeAccessModeProfiles::default()
        },
        ..AppUiPreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-scenario-scope-test",
        50_000,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    );

    assert_eq!(report.verdict, AppUiPreviewDecodePerformanceVerdict::Fail);
    assert!(preview_playback_decode_failures(&report).is_empty());
}

#[test]
fn preview_perf_report_serializes_color_report() {
    let diagnostics = AppUiPreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_rgba_frames: 1,
        decode_total_duration_us: 80_000,
        decode_max_duration_us: 80_000,
        decode_last_duration_us: 80_000,
        decode_seeked_frames: 1,
        decode_decoded_frame_count: 24,
        decode_max_decoded_frame_count: 24,
        decode_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 70_000,
            seek_us: 5_000,
            swscale_us: 4_000,
            rgba_copy_us: 1_000,
            ..PreviewDecodeStageDurations::default()
        },
        decode_max_frame_stage_durations: PreviewDecodeStageDurations {
            packet_decode_us: 70_000,
            seek_us: 5_000,
            swscale_us: 4_000,
            rgba_copy_us: 1_000,
            ..PreviewDecodeStageDurations::default()
        },
        render_timed_frames: 1,
        render_total_duration_us: 90_000,
        render_max_duration_us: 90_000,
        render_last_duration_us: 90_000,
        render_stage_durations: crate::app_ui::preview::AppUiPreviewRenderStageDurations {
            resolve_us: 2_000,
            final_cache_lookup_us: 100,
            working_prepare_us: 5_000,
            cpu_composite_us: 20_000,
            cpu_output_boundary_us: 60_000,
            frame_packaging_us: 2_900,
        },
        render_max_frame_stage_durations:
            crate::app_ui::preview::AppUiPreviewRenderStageDurations {
                resolve_us: 2_000,
                final_cache_lookup_us: 100,
                working_prepare_us: 5_000,
                cpu_composite_us: 20_000,
                cpu_output_boundary_us: 60_000,
                frame_packaging_us: 2_900,
            },
        color_composite_plans: 1,
        color_composite_elements: 1,
        color_composite_float_linear: 1,
        color_stage_total_stages: 1,
        color_stage_cpu_output_stages: 1,
        ..AppUiPreviewDiagnostics::default()
    };
    let preview_decode_report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US),
        "preview-color-health-test",
        APP_UI_PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
    );
    let preview_render_report = build_preview_render_performance_report(
        diagnostics.render_performance_summary(APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US),
        "preview-color-health-test",
        APP_UI_PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
    );
    let decode_failure_codes = preview_decode_hard_failures(&preview_decode_report);
    let render_failure_codes = preview_render_hard_failures(&preview_render_report);
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
        preview_color_report: build_preview_color_health_report(
            diagnostics.color_health_summary(),
            "preview-color-health-test",
        ),
        decode_failure_codes,
        render_failure_codes,
        preview_decode_report,
        preview_render_report,
        cases: Vec::new(),
    };

    let json = serde_json::to_value(&report).expect("serialize report");

    assert_eq!(
        json["preview_color_report"]["summary"]["fully_float_linear"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        json["preview_color_report"]["summary"]["gpu_path_ready"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        json["preview_color_report"]["summary"]["gpu_blocker_breakdown"]
            ["render_pipeline_not_prepared"],
        0
    );
    assert_eq!(
        json["preview_color_report"]["summary"]["legacy_breakdown"]["media_transform"],
        0
    );
    assert_eq!(json["media_color_issues"]["diagnostics"], 1);
    assert_eq!(json["media_color_issues"]["method_cicp_tags"], 1);
    assert_eq!(json["preview_color_report"]["verdict"], "Pass");
    assert!(json["decode_failure_codes"]
        .as_array()
        .expect("decode failure codes")
        .iter()
        .any(|code| code == "preview_decode_codec_or_gop_bound"));
    assert!(json["render_failure_codes"]
        .as_array()
        .expect("render failure codes")
        .iter()
        .any(|code| code == "preview_render_cpu_output_boundary_bound"));
    assert_eq!(json["preview_decode_report"]["verdict"], "Fail");
    assert_eq!(
        json["preview_decode_report"]["summary"]["primary_bottleneck"],
        "PacketDecode"
    );
    assert_eq!(
        json["preview_decode_report"]["summary"]["stage_durations"]["packet_decode_us"],
        70_000
    );
    assert!(json["preview_decode_report"]["root_causes"]
        .as_array()
        .expect("root causes")
        .iter()
        .any(|root| root["code"] == "preview_decode_codec_or_gop_bound"));
    assert_eq!(json["preview_render_report"]["verdict"], "Fail");
    assert_eq!(
        json["preview_render_report"]["summary"]["primary_bottleneck"],
        "CpuOutputBoundary"
    );
    assert_eq!(
        json["preview_render_report"]["summary"]["stage_durations"]["cpu_output_boundary_us"],
        60_000
    );
    assert!(json["preview_render_report"]["root_causes"]
        .as_array()
        .expect("render root causes")
        .iter()
        .any(|root| root["code"] == "preview_render_cpu_output_boundary_bound"));
    assert!(json.get("preview_color_health").is_none());
    assert!(json.get("preview_color_health_budget").is_none());
    assert!(json.get("preview_color_health_passed").is_none());
    assert!(json.get("preview_color_health_failures").is_none());
}

#[test]
fn preview_color_report_reports_failures() {
    let report = build_preview_color_health_report(
        Some(AppUiPreviewColorHealthSummary {
            policy_rejections: 1,
            gpu_blockers: 2,
            transfer_stages: 3,
            legacy_reason_total: 4,
            fully_float_linear: false,
            gpu_path_ready: false,
            ..AppUiPreviewColorHealthSummary::default()
        }),
        "preview-ci",
    );

    assert_eq!(report.verdict, AppUiPreviewColorHealthVerdict::Fail);
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "input_color_policy_rejected_source"));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_gpu_color_stage_blocked"));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "preview_transfer_stage_present"));
    assert!(report.root_causes.iter().any(|root| root.code == "legacy_rgba8_composite_path"));

    let missing = build_preview_color_health_report(None, "preview-missing");
    assert_eq!(missing.verdict, AppUiPreviewColorHealthVerdict::Fail);
    assert!(missing
        .root_causes
        .iter()
        .any(|root| root.code == "missing_preview_color_evidence"));
}

#[test]
fn viewer_gpu_output_budget_smoke_report_serializes_health_report() {
    let jsonl = r#"
{"health":{"status":"Waiting"},"health_counts":{"no_invocation":0,"waiting":1,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":0}}
{"health":{"status":"Ready"},"health_counts":{"no_invocation":0,"waiting":1,"blocked":0,"failed":0,"rejected":0,"degraded":0,"ready":1},"last_frame_context":{"sequence_id":"seq","frame":12,"width":1280,"height":720,"external_texture_key":"key","output_target":"Display","output_color_space":"Srgb","tone_map":false}}
"#;

    let report = viewer_gpu_output_budget_report_from_jsonl(
        VIEWER_GPU_OUTPUT_BUDGET_SCENARIO,
        "target/perf/viewer-gpu-output.jsonl",
        jsonl,
        &ViewerGpuOutputBudget {
            max_missing_runtime_reports: u64::MAX,
            max_missing_stage_reports: u64::MAX,
            max_ready_records_missing_preview_candidate_context: u64::MAX,
            max_preview_candidate_id_regressions: u64::MAX,
            ..ViewerGpuOutputBudget::default()
        },
    )
    .expect("viewer GPU output budget report");
    let json = serde_json::to_value(&report).expect("serialize viewer GPU output budget report");

    assert_eq!(json["scenario"], "viewer_gpu_output_budget");
    assert_eq!(json["report"]["schema_version"], 1);
    assert_eq!(json["report"]["profile"], "viewer_gpu_output_budget");
    assert_eq!(
        json["report"]["source_path"],
        "target/perf/viewer-gpu-output.jsonl"
    );
    assert_eq!(json["report"]["verdict"], "Pass");
    assert!(json.get("summary").is_none());
    assert!(json.get("health_report").is_none());
    assert!(json.get("source_path").is_none());
    assert_eq!(json["report"]["summary"]["passed"], true);
    assert_eq!(json["report"]["summary"]["records"], 2);
    assert_eq!(json["report"]["summary"]["budget"]["min_records"], 1);
    assert_eq!(json["report"]["summary"]["counts"]["ready"], 1);
    assert_eq!(json["report"]["summary"]["counts"]["waiting"], 1);
    assert_eq!(json["report"]["summary"]["display_issues"]["total"], 0);
    assert_eq!(
        json["report"]["summary"]["display_contract_refreshes"]["total"],
        0
    );
    assert_eq!(
        json["report"]["summary"]["display_issue_refresh_correlations"]["total"],
        0
    );
    assert_eq!(
        json["report"]["summary"]["budget"]["max_display_contract_refreshes"],
        u64::MAX
    );
    assert_eq!(
        json["report"]["summary"]["budget"]["max_hdr_output_requires_hdr_surface"],
        0
    );
    assert_eq!(
        json["report"]["summary"]["budget"]["max_display_payload_blockers"],
        0
    );
    assert_eq!(
        json["report"]["summary"]["budget"]["max_color_rejections"],
        0
    );
    assert_eq!(json["report"]["summary"]["color_rejections"], 0);
    assert_eq!(
        json["report"]["summary"]["reported_counts_match_replay"],
        true
    );
    assert_eq!(json["report"]["summary"]["last_frame_context"]["frame"], 12);
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
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_RUNTIME_REPORTS",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_STAGE_REPORTS",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_READY_RECORDS_MISSING_PREVIEW_CANDIDATE_CONTEXT",
        "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PREVIEW_CANDIDATE_ID_REGRESSIONS",
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
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_OS_DISPLAY_PROFILE_UNSUPPORTED",
            "30",
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
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_RUNTIME_REPORTS",
            "26",
        );
        std::env::set_var("MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_STAGE_REPORTS", "27");
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_READY_RECORDS_MISSING_PREVIEW_CANDIDATE_CONTEXT",
            "28",
        );
        std::env::set_var(
            "MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PREVIEW_CANDIDATE_ID_REGRESSIONS",
            "29",
        );
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
            max_os_display_profile_unsupported: 30,
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
            max_missing_runtime_reports: 26,
            max_missing_stage_reports: 27,
            max_ready_records_missing_preview_candidate_context: 28,
            max_preview_candidate_id_regressions: 29,
        }
    );
}

#[test]
fn external_playback_gates_fail_on_decode_queue_or_visibility_regression() {
    let readiness = PreviewReadinessCounts { ready: 10, stale: 4, loading: 2, unavailable: 4 };
    let report = preview_decode_report_with_playback_p95(80_000, 12_000);

    let evidence = PlaybackEvidenceCollector::default().report();
    let diagnostics = AppUiPreviewDiagnostics::default();
    let gpu = passing_headless_gpu_summary(20);
    let gates = evaluate_external_playback_gates(
        &readiness,
        &gpu,
        20,
        33_000,
        &report,
        &diagnostics,
        &evidence,
        40_000,
        10_000,
        95,
        9_000,
    );

    assert!(!gates.passed);
    assert_eq!(gates.visible_frames, 14);
    assert_eq!(gates.min_visible_frames, 19);
    assert_eq!(
        gates.failures,
        vec![
            "playback_decode_p95",
            "playback_queue_wait_p95",
            "visible_frame_ratio",
            "current_ready_ratio"
        ]
    );
}

#[test]
fn external_playback_gates_pass_when_real_media_thresholds_hold() {
    let readiness = PreviewReadinessCounts { ready: 18, stale: 1, loading: 1, unavailable: 0 };
    let report = preview_decode_report_with_playback_p95(25_000, 4_000);

    let evidence = PlaybackEvidenceCollector::default().report();
    let diagnostics = AppUiPreviewDiagnostics::default();
    let gpu = passing_headless_gpu_summary(20);
    let gates = evaluate_external_playback_gates(
        &readiness,
        &gpu,
        20,
        33_000,
        &report,
        &diagnostics,
        &evidence,
        40_000,
        10_000,
        95,
        9_000,
    );

    assert!(gates.passed);
    assert!(gates.failures.is_empty());
}

#[test]
fn external_playback_gates_do_not_treat_repeated_stale_frames_as_current_ready() {
    let readiness = PreviewReadinessCounts { ready: 2, stale: 18, loading: 0, unavailable: 0 };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = AppUiPreviewDiagnostics::default();
    let evidence = PlaybackEvidenceCollector::default().report();
    let gpu = passing_headless_gpu_summary(20);

    let gates = evaluate_external_playback_gates(
        &readiness,
        &gpu,
        20,
        33_000,
        &decode,
        &diagnostics,
        &evidence,
        40_000,
        10_000,
        95,
        9_000,
    );

    assert_eq!(gates.visible_frames, 20);
    assert_eq!(gates.ready_frames, 2);
    assert_eq!(gates.min_ready_frames, 18);
    assert_eq!(gates.failures, vec!["current_ready_ratio"]);
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_require_real_gpu_execution_without_readback_or_blockers() {
    let readiness = PreviewReadinessCounts { ready: 20, stale: 0, loading: 0, unavailable: 0 };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = AppUiPreviewDiagnostics::default();
    let evidence = PlaybackEvidenceCollector::default().report();
    let gpu = HeadlessViewerGpuExecutionSummary {
        stage_diagnostics: RenderColorStageDiagnostics {
            readback_stages: 1,
            gpu_blockers: 1,
            ..RenderColorStageDiagnostics::default()
        },
        ..HeadlessViewerGpuExecutionSummary::default()
    };

    let gates = evaluate_external_playback_gates(
        &readiness,
        &gpu,
        20,
        33_000,
        &decode,
        &diagnostics,
        &evidence,
        40_000,
        10_000,
        95,
        9_000,
    );

    assert_eq!(
        gates.failures,
        vec![
            "viewer_gpu_execution_coverage",
            "viewer_gpu_readback",
            "viewer_gpu_blockers",
            "viewer_gpu_execution_p95"
        ]
    );
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_fail_on_clock_audio_or_evidence_integrity() {
    let readiness = PreviewReadinessCounts { ready: 20, stale: 0, loading: 0, unavailable: 0 };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let mut evidence = PlaybackEvidenceCollector::default().report();
    evidence.delivery_clock_drift.max_us = 25_000;
    evidence.audio_underrun_recoveries = 1;
    evidence.dropped_event_count = 1;

    let diagnostics = AppUiPreviewDiagnostics::default();
    let gpu = passing_headless_gpu_summary(20);
    let gates = evaluate_external_playback_gates(
        &readiness,
        &gpu,
        20,
        33_000,
        &decode,
        &diagnostics,
        &evidence,
        40_000,
        10_000,
        95,
        9_000,
    );

    assert_eq!(
        gates.failures,
        vec![
            "delivery_clock_drift",
            "audio_underrun_recovery",
            "playback_evidence_overflow",
        ]
    );
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_fail_on_cpu_frame_store_budget_or_admission() {
    let readiness = PreviewReadinessCounts { ready: 20, stale: 0, loading: 0, unavailable: 0 };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let evidence = PlaybackEvidenceCollector::default().report();
    let diagnostics = AppUiPreviewDiagnostics {
        media_cache_reserved_bytes: 101,
        media_cache_byte_budget: 100,
        pinned_media_frame_bytes: 120,
        viewer_frame_cache_reserved_bytes: 80,
        viewer_frame_cache_byte_budget: 100,
        pinned_viewer_frame_bytes: 120,
        media_cache_oversize_rejections: 1,
        ..AppUiPreviewDiagnostics::default()
    };
    let gpu = passing_headless_gpu_summary(20);

    let gates = evaluate_external_playback_gates(
        &readiness,
        &gpu,
        20,
        33_000,
        &decode,
        &diagnostics,
        &evidence,
        40_000,
        10_000,
        95,
        9_000,
    );

    assert_eq!(
        gates.failures,
        vec![
            "cpu_frame_store_budget",
            "cpu_frame_store_oversize_rejection"
        ]
    );
    assert!(!gates.cpu_frame_store_within_budget);
    assert_eq!(gates.cpu_frame_store_oversize_rejections, 1);
    assert!(!gates.passed);
}

fn passing_headless_gpu_summary(frames: usize) -> HeadlessViewerGpuExecutionSummary {
    HeadlessViewerGpuExecutionSummary {
        rendered_frames: frames,
        wall_duration_samples_us: vec![1_000; frames],
        record_submit_samples_us: vec![400; frames],
        completion_wait_samples_us: vec![600; frames],
        gpu_duration_samples_us: vec![500; frames],
        gpu_timestamp_tokens: (0..frames as u64).collect(),
        stage_diagnostics: RenderColorStageDiagnostics {
            total_stages: frames as u64,
            gpu_color_stages: frames as u64,
            ..RenderColorStageDiagnostics::default()
        },
        ..HeadlessViewerGpuExecutionSummary::default()
    }
}

fn preview_decode_report_with_playback_p95(
    playback_decode_p95_us: u64,
    playback_queue_wait_p95_us: u64,
) -> AppUiPreviewDecodePerformanceReport {
    AppUiPreviewDecodePerformanceReport {
        schema_version: 26,
        profile: "test".to_owned(),
        verdict: AppUiPreviewDecodePerformanceVerdict::Pass,
        required_access_modes: vec![PreviewDecodeAccessMode::PlaybackCursor],
        summary: None,
        checks: vec![
            AppUiPreviewDecodePerformanceCheck {
                area: AppUiPreviewDecodePerformanceArea::AccessMode,
                code: "preview_decode_playback_cursor_p95_frame_us",
                severity: AppUiPreviewDecodePerformanceSeverity::Pass,
                observed: playback_decode_p95_us,
                limit: Some(40_000),
            },
            AppUiPreviewDecodePerformanceCheck {
                area: AppUiPreviewDecodePerformanceArea::AccessMode,
                code: "preview_decode_playback_cursor_queue_wait_p95_us",
                severity: AppUiPreviewDecodePerformanceSeverity::Pass,
                observed: playback_queue_wait_p95_us,
                limit: Some(10_000),
            },
        ],
        root_causes: Vec::new(),
        actions: Vec::new(),
    }
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
