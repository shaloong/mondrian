#[cfg(feature = "validation")]
use super::audio_playback_acceptance::{
    evaluate_professional_audio_playback, AudioPlaybackMediaProbeReport,
    ProfessionalAudioPlaybackObservation, ProfessionalAudioRecoveryObservation,
    ProfessionalVideoReadinessObservation,
};
use super::playback_acceptance::{
    evaluate_playback_qualification, evaluate_professional_playback,
    PlaybackDecodeExecutionEvidence, PresentedDecodeExecutionEvidence,
    PreviewCancellationRecoveryEvidence, PreviewPlaybackMediaProbeReport,
    PreviewPlaybackQualificationGateReport, PreviewPlaybackQualificationObservation,
    PreviewProcessMemoryEvidenceCollector, PreviewProcessMemoryEvidenceReport,
    PreviewProfessionalPlaybackGateReport, PreviewRuntimeAcceptanceEvidence,
    ProfessionalNativeVideoGpuTimingEvidence, ProfessionalPlaybackObservation,
    PROFESSIONAL_GPU_CANDIDATE_LIMIT_MS, PROFESSIONAL_MIN_ACCURATE_SEEKS,
    PROFESSIONAL_MIN_OBSERVED_DURATION_US, PROFESSIONAL_MIN_READY_BASIS_POINTS,
    PROFESSIONAL_MIN_VISIBLE_PERCENT, PROFESSIONAL_MIN_WARM_SEEKS,
    PROFESSIONAL_PLAYBACK_DECODE_P95_LIMIT_US, PROFESSIONAL_PLAYBACK_QUEUE_WAIT_P95_LIMIT_US,
    PROFESSIONAL_READY_TIMEOUT_MS, PROFESSIONAL_TOTAL_TIMEOUT_MS,
};
use super::playback_preview::{pump_playback_preview, PlaybackPreviewPumpOutcome};
use super::*;
#[path = "authoring_perf.rs"]
mod authoring_perf;
#[path = "perf_decode_progress.rs"]
mod perf_decode_progress;
#[path = "perf_process_memory.rs"]
mod perf_process_memory;
use crate::app::headless_preview_presentation::{
    prepare_headless_preview_successor, stage_headless_preview_lookahead, HeadlessPreviewRuntime,
};
use crate::app::headless_realtime_playback::*;
use crate::app::headless_viewer_gpu::{
    HeadlessGpuCompletionDeadline, HeadlessNativeVideoImportGpuTimingFinalEvidence,
    HeadlessViewerGpuAdapter, HeadlessViewerGpuAdapterInfo, HeadlessViewerGpuExecution,
    HeadlessViewerGpuOutput,
};
use crate::app::preview_execution::{
    PreviewDecodeExecutionSummary, PREVIEW_GPU_CPU_STAGING_CAPACITY,
};
use crate::app::preview_runtime::{
    build_preview_color_health_report, build_preview_decode_performance_report,
    build_preview_decode_performance_report_with_required_access_modes,
    build_preview_render_performance_report, PreviewColorHealthReport, PreviewColorHealthSummary,
    PreviewColorHealthVerdict, PreviewDecodeAccessModeProfile, PreviewDecodeAccessModeProfiles,
    PreviewDecodeLatencyBuckets, PreviewDecodePerformanceArea, PreviewDecodePerformanceCheck,
    PreviewDecodePerformancePolicy, PreviewDecodePerformanceReport,
    PreviewDecodePerformanceSeverity, PreviewDecodePerformanceVerdict,
    PreviewDecodeWorkClassProfiles, PreviewDecodeWorkLatencyBuckets,
    PreviewDecodeWorkLatencyProfile, PreviewDiagnostics, PreviewRenderPerformanceReport,
    PreviewRenderPerformanceSeverity, PreviewRenderPerformanceVerdict,
    PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US, PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION,
    PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
};
use crate::app::ui_actions::TimelineSeekSource;
use crate::app::viewer_gpu_output_health::{
    build_health_report, evaluate_jsonl, ViewerGpuOutputBudget, ViewerGpuOutputHealthReport,
    ViewerGpuOutputHealthVerdict,
};
use crate::app_ui::panels::ViewerPreviewSource;
use crate::app_ui::preview::WindowPreviewAdapter;
use crate::app_ui::shell::AppUiAppRoot;
use anyhow::Context;
use mondrian_audio::AudioRuntimeResourceGrant;
use perf_decode_progress::PreviewDecodeExecutionJournal;
use perf_process_memory::ProfessionalProcessMemorySampler;
use serde::Serialize;
use std::cmp;
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mondrian_core::types::{Rational, Resolution};
use mondrian_effects::{EffectNode, EffectNodeExt};
#[cfg(feature = "validation")]
use mondrian_media::AudioPlaybackSnapshot;
use mondrian_media::{
    probe_media_info, MediaInfo, PreviewDecodeAccessMode, PreviewDecodeExecutionStage,
    PreviewDecodeStageDurations, VideoColorDiagnostic, VideoColorDiagnosticIssueAggregate,
};
use mondrian_platform::{ProcessMemoryProbe, SystemPlatformService};
use mondrian_playback::{PlaybackClockPhaseErrorSummary, PlaybackEvidenceReport};
use mondrian_renderer::profile::{GpuTimestampSample, GpuTimestampStageDurations};
use mondrian_renderer::{
    color::RenderColorStageDiagnostics, GpuCompositingDiagnostics,
    GpuCompositorTextureBindingDiagnostics, GpuCompositorUniformArenaDiagnostics,
    GpuViewerSpatialRuntimeDiagnostics, NativeVideoImportCpuTimings,
    NativeVideoImportGpuTimingDiagnostics, NativeVideoImportGpuTimingPolicy,
    ViewerGpuExecutionCpuStageTimings, NATIVE_VIDEO_IMPORT_GPU_TIMING_MAX_CAPACITY,
};
use mondrian_timeline::track::Track;
use mondrian_ui_core::tree::TreeWalker;
use mondrian_ui_core::types::Rect;
use mondrian_ui_renderer::DrawEncoder;
use mondrian_ui_theme::ThemePreset;

const PROFESSIONAL_NATIVE_VIDEO_GPU_IMPORTS_PER_CANDIDATE_BUDGET: usize = 4;
const PROFESSIONAL_NATIVE_VIDEO_GPU_CANDIDATE_OVERHEAD: usize = 128;
const PROFESSIONAL_NATIVE_VIDEO_GPU_OBSERVATION_CAPACITY_LIMIT: usize = 1_000_000;

fn commit_perf_media_probe(
    library: &AssetLibrary,
    path: &Path,
    media_info: MediaInfo,
) -> anyhow::Result<AssetId> {
    let canonical_path = std::fs::canonicalize(path)
        .with_context(|| format!("canonicalize performance media {}", path.display()))?;
    let source_fingerprint = mondrian_media::MediaFileFingerprint::capture(&canonical_path);
    let candidate = mondrian_assets::AssetMediaProbeCandidate::new(
        canonical_path,
        source_fingerprint,
        media_info,
    )?;
    Ok(library.commit_media_probe(candidate, None)?)
}

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
    preview_diagnostics: PreviewDiagnostics,
    preview_color_report: PreviewColorHealthReport,
    preview_playback_diagnostics: PreviewDiagnostics,
    preview_playback_color_report: PreviewColorHealthReport,
    cases: Vec<PerfCaseReport>,
}

fn app_ui_scale_color_gate_failures(
    preview: &PreviewColorHealthReport,
    playback: &PreviewColorHealthReport,
) -> Vec<&'static str> {
    [("preview", preview), ("playback", playback)]
        .into_iter()
        .filter_map(|(name, report)| {
            (report.verdict == PreviewColorHealthVerdict::Fail).then_some(name)
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct PreviewMediaPerfReport {
    scenario: &'static str,
    frames: usize,
    cache_iterations: usize,
    headless_gpu: HeadlessViewerGpuExecutionSummary,
    media_color_issues: VideoColorDiagnosticIssueAggregate,
    preview_diagnostics: PreviewDiagnostics,
    preview_color_report: PreviewColorHealthReport,
    decode_failure_codes: Vec<&'static str>,
    render_failure_codes: Vec<&'static str>,
    preview_decode_report: PreviewDecodePerformanceReport,
    preview_render_report: Option<PreviewRenderPerformanceReport>,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Clone, Copy, Serialize, Default)]
struct PreviewReadinessCounts {
    ready: usize,
    loading: usize,
    stale: usize,
    unavailable: usize,
    /// Display-clock opportunities that elapsed before the harness could
    /// classify that exact authored frame.
    missed_deadline: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Default, PartialEq, Eq)]
struct HeadlessViewerGpuExtent {
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NativeVideoGpuTimingCandidateKey {
    session_id: u64,
    candidate_token: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NativeVideoGpuTimingCandidateRecord {
    key: NativeVideoGpuTimingCandidateKey,
    submitted_imports: u64,
    scheduled_samples: u64,
    missing_samples: u64,
    dropped_samples: u64,
    published: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NativeVideoGpuTimingSampleKey {
    session_id: u64,
    import_token: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NativeVideoGpuTimingSampleRecord {
    key: NativeVideoGpuTimingSampleKey,
    candidate: NativeVideoGpuTimingCandidateKey,
    yuv_decode_marker_bracket_us: u64,
    input_color_marker_bracket_us: u64,
    decode_fence_ready_at_admission: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct NativeVideoGpuMarkerTimingStatistics {
    samples: u64,
    p50_us: u64,
    p95_us: u64,
    mean_us: u64,
    max_us: u64,
}

impl NativeVideoGpuMarkerTimingStatistics {
    fn from_samples(samples: &[u64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let sum = samples.iter().fold(0_u128, |total, sample| {
            total.saturating_add(u128::from(*sample))
        });
        Self {
            samples: samples.len() as u64,
            p50_us: percentile_sample_us(samples, 50),
            p95_us: percentile_sample_us(samples, 95),
            mean_us: (sum / samples.len() as u128).min(u128::from(u64::MAX)) as u64,
            max_us: samples.iter().copied().max().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct NativeVideoGpuFenceReadinessStatistics {
    ready: u64,
    not_ready: u64,
    unknown: u64,
}

impl NativeVideoGpuFenceReadinessStatistics {
    fn record(&mut self, ready: Option<bool>) {
        match ready {
            Some(true) => self.ready = self.ready.saturating_add(1),
            Some(false) => self.not_ready = self.not_ready.saturating_add(1),
            None => self.unknown = self.unknown.saturating_add(1),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct NativeVideoGpuTimingAdapterReport {
    session_id: u64,
    candidate_receipts: u64,
    observation_capacity: usize,
    buffered_samples: usize,
    drained_samples: u64,
    adapter_overflow_samples: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct ProfessionalNativeVideoGpuTimingReport {
    adapter: NativeVideoGpuTimingAdapterReport,
    evidence: ProfessionalNativeVideoGpuTimingEvidence,
    yuv_decode_marker_bracket: NativeVideoGpuMarkerTimingStatistics,
    input_color_marker_bracket: NativeVideoGpuMarkerTimingStatistics,
    decode_fence_ready_at_admission: NativeVideoGpuFenceReadinessStatistics,
}

#[derive(Debug, Serialize, Default)]
struct HeadlessViewerGpuExecutionSummary {
    adapter: Option<HeadlessViewerGpuAdapterInfo>,
    /// Newly rendered GPU submissions whose completion was observed.
    rendered_frames: usize,
    gpu_completion_observed_frames: usize,
    /// Newly rendered executions published as the exact current output.
    published_rendered_frames: usize,
    /// Completed executions released because their visual lifecycle was stale.
    released_rendered_frames: usize,
    /// Completed ticketless executions retained as immediate successors.
    prepared_successor_frames: usize,
    /// Bounded attempts to establish or observe the immediate successor.
    successor_preparation_attempts: usize,
    /// Attempts that found an already-prepared successor or retained one.
    successor_preparation_ready: usize,
    /// Completed executions rejected by an exact terminal Frame Delivery.
    terminal_rejected_rendered_frames: usize,
    /// Terminal-rejected rendered executions classified specifically as Late.
    late_rejected_rendered_frames: usize,
    /// Exact-current observations that reused an already-completed GPU output.
    published_cached_output_observations: usize,
    /// Cached executions released before publication.
    released_cached_frames: usize,
    /// Cached executions rejected by an exact terminal Frame Delivery.
    terminal_rejected_cached_frames: usize,
    /// Terminal-rejected cached executions classified specifically as Late.
    late_rejected_cached_frames: usize,
    /// Successful GPU-backed current-output observations, including repeated
    /// observation of a retained exact output.
    published_output_observations: usize,
    /// Successful GPU-backed publications that consumed an exact Frame Demand.
    presented_demand_completions: usize,
    /// Unique `(epoch, target_frame)` publication coverage. Reissued quality
    /// or handoff demands for the same frame cannot inflate this value.
    presented_unique_frame_completions: usize,
    #[serde(skip)]
    presented_frame_bindings: HashSet<(mondrian_playback::PlaybackEpoch, i64)>,
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
    program_output_boundary_samples_us: Vec<u64>,
    #[serde(skip)]
    program_scopes_samples_us: Vec<u64>,
    #[serde(skip)]
    signal_monitoring_samples_us: Vec<u64>,
    #[serde(skip)]
    monitor_adaptation_samples_us: Vec<u64>,
    #[serde(skip)]
    display_calibration_samples_us: Vec<u64>,
    gpu_duration_samples_us: Vec<u64>,
    #[serde(skip)]
    gpu_stage_samples: Vec<GpuTimestampStageDurations>,
    #[serde(skip)]
    expected_gpu_timestamp_tokens: HashSet<u64>,
    #[serde(skip)]
    recorded_gpu_timestamp_tokens: HashSet<u64>,
    duplicate_expected_gpu_timestamp_tokens: usize,
    unmatched_gpu_timestamp_samples: usize,
    duplicate_gpu_timestamp_samples: usize,
    duplicate_gpu_timestamp_ownership: usize,
    missing_gpu_timestamp_frames: usize,
    discarded_gpu_timestamp_frames: u64,
    fallback_count: usize,
    fallback_reasons: Vec<String>,
    stage_diagnostics: RenderColorStageDiagnostics,
    compositing_diagnostics: GpuCompositingDiagnostics,
    compositor_uniform_arena: Option<GpuCompositorUniformArenaDiagnostics>,
    compositor_texture_bindings: Option<GpuCompositorTextureBindingDiagnostics>,
    compositor_creative_luts: Option<mondrian_renderer::GpuCreativeLutCacheDiagnostics>,
    spatial_diagnostics: Option<GpuViewerSpatialRuntimeDiagnostics>,
    resource_pool_samples: Vec<mondrian_renderer::GpuColorFrameWgpuResourcePoolDiagnostics>,
    rendered_decode_execution: PreviewDecodeExecutionSummary,
    /// Decode execution carried only by newly rendered outputs that actually
    /// became the exact current Viewer output.
    published_rendered_decode_execution: PreviewDecodeExecutionSummary,
    native_import_contract_pools_peak: usize,
    native_import_bridge_entries_peak: usize,
    /// Sources retained after a completion while a later pipelined owner is
    /// still active. This is expected bounded residency, not a leak.
    native_import_pipelined_retained_sources_peak: usize,
    /// Sources still retained after the exact final active submission
    /// completed. Any non-zero value is an ownership leak.
    native_import_retained_sources_peak: usize,
    #[serde(skip)]
    native_video_gpu_timing_candidates: Vec<NativeVideoGpuTimingCandidateRecord>,
}

impl HeadlessViewerGpuExecutionSummary {
    fn record(
        &mut self,
        execution: HeadlessViewerGpuExecution,
        disposition: HeadlessGpuExecutionPublication,
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) {
        self.native_import_contract_pools_peak = self
            .native_import_contract_pools_peak
            .max(execution.native_import_contract_pools);
        self.native_import_bridge_entries_peak = self
            .native_import_bridge_entries_peak
            .max(execution.native_import_bridge_entries);
        if execution.remaining_submissions_after_completion == 0 {
            self.native_import_retained_sources_peak = self
                .native_import_retained_sources_peak
                .max(execution.native_import_retained_sources);
        } else {
            self.native_import_pipelined_retained_sources_peak = self
                .native_import_pipelined_retained_sources_peak
                .max(execution.native_import_retained_sources);
        }
        if let Some(receipt) = execution.native_import_gpu_timing_receipt {
            self.native_video_gpu_timing_candidates
                .push(NativeVideoGpuTimingCandidateRecord {
                    key: NativeVideoGpuTimingCandidateKey {
                        session_id: receipt.session_id.get(),
                        candidate_token: receipt.candidate_token,
                    },
                    submitted_imports: receipt.submitted_imports,
                    scheduled_samples: receipt.scheduled_samples,
                    missing_samples: receipt.missing_samples,
                    dropped_samples: receipt.dropped_samples,
                    published: disposition == HeadlessGpuExecutionPublication::PublishedCurrent,
                });
        }
        let output_extent = HeadlessViewerGpuExtent {
            width: execution.output_width,
            height: execution.output_height,
        };
        if !self.output_extents.contains(&output_extent) {
            self.output_extents.push(output_extent);
        }
        self.rendered_frames = self.rendered_frames.saturating_add(1);
        match disposition {
            HeadlessGpuExecutionPublication::PublishedCurrent => {
                self.published_rendered_frames = self.published_rendered_frames.saturating_add(1);
            }
            HeadlessGpuExecutionPublication::Released => {
                self.released_rendered_frames = self.released_rendered_frames.saturating_add(1);
            }
            HeadlessGpuExecutionPublication::PreparedSuccessor => {
                self.prepared_successor_frames = self.prepared_successor_frames.saturating_add(1);
            }
            HeadlessGpuExecutionPublication::TerminalRejected(kind) => {
                self.terminal_rejected_rendered_frames =
                    self.terminal_rejected_rendered_frames.saturating_add(1);
                if kind == mondrian_playback::FrameDeliveryKind::Late {
                    self.late_rejected_rendered_frames =
                        self.late_rejected_rendered_frames.saturating_add(1);
                }
            }
        }
        if execution.gpu_completion_observed {
            self.gpu_completion_observed_frames =
                self.gpu_completion_observed_frames.saturating_add(1);
        }
        self.rendered_decode_execution.accumulate(execution.decode_execution);
        if disposition == HeadlessGpuExecutionPublication::PublishedCurrent {
            self.published_rendered_decode_execution.accumulate(execution.decode_execution);
        }
        self.wall_duration_samples_us.push(execution.duration_us);
        self.record_submit_samples_us.push(execution.record_submit_us);
        self.completion_wait_samples_us.push(execution.completion_wait_us);
        if let Some(timings) = execution.cpu_stage_timings {
            self.input_prepare_samples_us.push(timings.input_prepare_us);
            self.native_video_import_samples.push(timings.native_video_import);
            self.working_composite_samples_us.push(timings.working_composite_us);
            self.spatial_samples_us.push(timings.spatial_us);
            self.program_output_boundary_samples_us.push(timings.program_output_boundary_us);
            self.program_scopes_samples_us.push(timings.program_scopes_us);
            self.signal_monitoring_samples_us.push(timings.signal_monitoring_us);
            self.monitor_adaptation_samples_us.push(timings.monitor_adaptation_us);
            self.display_calibration_samples_us.push(timings.display_calibration_us);
        }
        if let Some(token) = execution.gpu_timestamp_token {
            if !self.expected_gpu_timestamp_tokens.insert(token) {
                self.duplicate_expected_gpu_timestamp_tokens =
                    self.duplicate_expected_gpu_timestamp_tokens.saturating_add(1);
            }
        } else {
            self.missing_gpu_timestamp_frames = self.missing_gpu_timestamp_frames.saturating_add(1);
        }
        self.fallback_count = self.fallback_count.saturating_add(execution.fallback_reasons.len());
        self.fallback_reasons.extend(execution.fallback_reasons);
        if let Some(diagnostics) = execution.stage_diagnostics {
            self.stage_diagnostics.accumulate(diagnostics);
        }
        if let Some(diagnostics) = execution.compositing_diagnostics {
            self.compositing_diagnostics.accumulate(diagnostics);
        }
        if let Some(diagnostics) = execution.compositor_uniform_arena {
            self.compositor_uniform_arena = Some(diagnostics);
        }
        if let Some(diagnostics) = execution.compositor_texture_bindings {
            self.compositor_texture_bindings = Some(diagnostics);
        }
        if let Some(diagnostics) = execution.compositor_creative_luts {
            self.compositor_creative_luts = Some(diagnostics);
        }
        if let Some(diagnostics) = execution.spatial_diagnostics {
            self.spatial_diagnostics = Some(diagnostics);
        }
        self.resource_pool_samples.push(execution.resource_pool_diagnostics);
        if disposition == HeadlessGpuExecutionPublication::PublishedCurrent {
            self.record_published_output_observation(completed_demand);
        }
    }

    fn record_gpu_timings(&mut self, timings: &[GpuTimestampSample]) {
        for sample in timings {
            let token = sample.token.id();
            if !self.accept_gpu_timestamp_sample(token) {
                continue;
            }
            self.gpu_duration_samples_us.push(sample.elapsed_us);
            self.gpu_stage_samples.push(sample.stages);
        }
        self.finish_gpu_timestamp_reconciliation();
    }

    fn record_owned_gpu_timestamp_sample(&mut self, sample: &GpuTimestampSample) {
        if self.accept_gpu_timestamp_sample(sample.token.id()) {
            self.gpu_duration_samples_us.push(sample.elapsed_us);
            self.gpu_stage_samples.push(sample.stages);
        }
    }

    fn accept_gpu_timestamp_sample(&mut self, token: u64) -> bool {
        if self.recorded_gpu_timestamp_tokens.contains(&token) {
            self.duplicate_gpu_timestamp_samples =
                self.duplicate_gpu_timestamp_samples.saturating_add(1);
            return false;
        }
        if !self.expected_gpu_timestamp_tokens.remove(&token) {
            self.unmatched_gpu_timestamp_samples =
                self.unmatched_gpu_timestamp_samples.saturating_add(1);
            return false;
        }
        self.recorded_gpu_timestamp_tokens.insert(token);
        true
    }

    fn finish_gpu_timestamp_reconciliation(&mut self) {
        self.missing_gpu_timestamp_frames = self
            .missing_gpu_timestamp_frames
            .saturating_add(self.expected_gpu_timestamp_tokens.len());
        self.expected_gpu_timestamp_tokens.clear();
    }

    fn record_current_output_presentation(
        &mut self,
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) {
        self.published_cached_output_observations =
            self.published_cached_output_observations.saturating_add(1);
        self.record_published_output_observation(completed_demand);
    }

    fn record_published_output_observation(
        &mut self,
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) {
        self.published_output_observations = self.published_output_observations.saturating_add(1);
        if let Some(identity) = completed_demand {
            self.presented_demand_completions = self.presented_demand_completions.saturating_add(1);
            if self.presented_frame_bindings.insert((identity.epoch, identity.target_frame)) {
                self.presented_unique_frame_completions =
                    self.presented_unique_frame_completions.saturating_add(1);
            }
        }
    }

    fn p95_duration_us(&self) -> u64 {
        p95_sample_us(&self.gpu_duration_samples_us)
    }

    fn p95_gpu_stages(&self) -> GpuTimestampStageDurations {
        fn field(
            samples: &[GpuTimestampStageDurations],
            read: impl Fn(&GpuTimestampStageDurations) -> u64,
        ) -> u64 {
            p95_sample_us(&samples.iter().map(read).collect::<Vec<_>>())
        }

        GpuTimestampStageDurations {
            through_working_composite_us: field(&self.gpu_stage_samples, |sample| {
                sample.through_working_composite_us
            }),
            spatial_us: field(&self.gpu_stage_samples, |sample| sample.spatial_us),
            program_output_boundary_us: field(&self.gpu_stage_samples, |sample| {
                sample.program_output_boundary_us
            }),
            program_scopes_us: field(&self.gpu_stage_samples, |sample| sample.program_scopes_us),
            signal_monitoring_us: field(&self.gpu_stage_samples, |sample| {
                sample.signal_monitoring_us
            }),
            monitor_adaptation_us: field(&self.gpu_stage_samples, |sample| {
                sample.monitor_adaptation_us
            }),
            display_calibration_us: field(&self.gpu_stage_samples, |sample| {
                sample.display_calibration_us
            }),
        }
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
            program_output_boundary_us: p95_sample_us(&self.program_output_boundary_samples_us),
            program_scopes_us: p95_sample_us(&self.program_scopes_samples_us),
            signal_monitoring_us: p95_sample_us(&self.signal_monitoring_samples_us),
            monitor_adaptation_us: p95_sample_us(&self.monitor_adaptation_samples_us),
            display_calibration_us: p95_sample_us(&self.display_calibration_samples_us),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuTimestampSampleOwner {
    Unmatched,
    Exact(usize),
    Duplicate,
}

fn gpu_timestamp_sample_owner<'a>(
    token: u64,
    summaries: impl Iterator<Item = (usize, &'a HeadlessViewerGpuExecutionSummary)>,
) -> GpuTimestampSampleOwner {
    let mut owner = None;
    for (index, summary) in summaries {
        if !summary.expected_gpu_timestamp_tokens.contains(&token) {
            continue;
        }
        if owner.replace(index).is_some() {
            return GpuTimestampSampleOwner::Duplicate;
        }
    }
    owner.map_or(
        GpuTimestampSampleOwner::Unmatched,
        GpuTimestampSampleOwner::Exact,
    )
}

fn distribute_gpu_timestamp_samples(
    timings: &[GpuTimestampSample],
    summaries: &mut [&mut HeadlessViewerGpuExecutionSummary],
    evidence_owner: usize,
) {
    for sample in timings {
        let owner = gpu_timestamp_sample_owner(
            sample.token.id(),
            summaries.iter().enumerate().map(|(index, summary)| (index, &**summary)),
        );
        match owner {
            GpuTimestampSampleOwner::Exact(index) => {
                summaries[index].record_owned_gpu_timestamp_sample(sample);
            }
            GpuTimestampSampleOwner::Unmatched => {
                summaries[evidence_owner].unmatched_gpu_timestamp_samples =
                    summaries[evidence_owner].unmatched_gpu_timestamp_samples.saturating_add(1);
            }
            GpuTimestampSampleOwner::Duplicate => {
                summaries[evidence_owner].duplicate_gpu_timestamp_ownership =
                    summaries[evidence_owner].duplicate_gpu_timestamp_ownership.saturating_add(1);
            }
        }
    }
    for summary in summaries {
        summary.finish_gpu_timestamp_reconciliation();
    }
}

fn reconcile_native_video_gpu_timings(
    session_id: u64,
    renderer: &NativeVideoImportGpuTimingDiagnostics,
    candidates: impl IntoIterator<Item = NativeVideoGpuTimingCandidateRecord>,
    samples: impl IntoIterator<Item = NativeVideoGpuTimingSampleRecord>,
    adapter_dropped_samples: u64,
) -> ProfessionalNativeVideoGpuTimingReport {
    let mut candidate_records =
        HashMap::<NativeVideoGpuTimingCandidateKey, Vec<NativeVideoGpuTimingCandidateRecord>>::new(
        );
    for candidate in candidates {
        candidate_records.entry(candidate.key).or_default().push(candidate);
    }

    let mut evidence = ProfessionalNativeVideoGpuTimingEvidence {
        capability_supported: renderer.capability_supported,
        activated: renderer.activated,
        inactive_reason: renderer.inactive_reason.clone(),
        renderer_submitted_imports: renderer.submitted_imports,
        renderer_samples: renderer.samples,
        renderer_pending_samples: renderer.pending,
        renderer_missing_samples: renderer.missing,
        renderer_dropped_samples: renderer.dropped,
        adapter_dropped_samples,
        ..ProfessionalNativeVideoGpuTimingEvidence::default()
    };
    let mut accepted_candidates =
        HashMap::<NativeVideoGpuTimingCandidateKey, NativeVideoGpuTimingCandidateRecord>::new();
    let mut ambiguous_candidates = HashSet::new();
    for (key, records) in candidate_records {
        if records.len() > 1 {
            ambiguous_candidates.insert(key);
            evidence.duplicate_candidate_receipts = evidence
                .duplicate_candidate_receipts
                .saturating_add(records.len().saturating_sub(1) as u64);
        }
        if records.len() != 1 {
            evidence.orphan_candidate_receipts =
                evidence.orphan_candidate_receipts.saturating_add(records.len() as u64);
            continue;
        }
        let record = records[0];
        if record.key.session_id != session_id
            || checked_evidence_sum(&[
                record.scheduled_samples,
                record.missing_samples,
                record.dropped_samples,
            ]) != Some(record.submitted_imports)
        {
            evidence.orphan_candidate_receipts =
                evidence.orphan_candidate_receipts.saturating_add(1);
            continue;
        }
        evidence.receipt_candidates = evidence.receipt_candidates.saturating_add(1);
        evidence.receipt_submitted_imports =
            evidence.receipt_submitted_imports.saturating_add(record.submitted_imports);
        evidence.receipt_scheduled_samples =
            evidence.receipt_scheduled_samples.saturating_add(record.scheduled_samples);
        evidence.receipt_missing_samples =
            evidence.receipt_missing_samples.saturating_add(record.missing_samples);
        evidence.receipt_dropped_samples =
            evidence.receipt_dropped_samples.saturating_add(record.dropped_samples);
        if record.published && record.submitted_imports > 0 {
            evidence.published_native_candidates =
                evidence.published_native_candidates.saturating_add(1);
        }
        accepted_candidates.insert(key, record);
    }

    let mut observed_sample_keys = HashSet::new();
    let mut yuv_samples = Vec::new();
    let mut input_color_samples = Vec::new();
    let mut fence_readiness = NativeVideoGpuFenceReadinessStatistics::default();
    let mut candidate_sample_counts = HashMap::<NativeVideoGpuTimingCandidateKey, u64>::new();
    for sample in samples {
        if !observed_sample_keys.insert(sample.key) {
            evidence.duplicate_samples = evidence.duplicate_samples.saturating_add(1);
            continue;
        }
        evidence.observed_samples = evidence.observed_samples.saturating_add(1);
        if sample.key.session_id != session_id || sample.candidate.session_id != session_id {
            evidence.unmatched_samples = evidence.unmatched_samples.saturating_add(1);
            continue;
        }
        let Some(candidate) = accepted_candidates.get(&sample.candidate) else {
            if ambiguous_candidates.contains(&sample.candidate) {
                evidence.duplicate_sample_ownership =
                    evidence.duplicate_sample_ownership.saturating_add(1);
            } else {
                evidence.unmatched_samples = evidence.unmatched_samples.saturating_add(1);
            }
            continue;
        };
        if candidate.published && candidate.submitted_imports > 0 {
            evidence.published_native_samples = evidence.published_native_samples.saturating_add(1);
        }
        candidate_sample_counts
            .entry(sample.candidate)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        yuv_samples.push(sample.yuv_decode_marker_bracket_us);
        input_color_samples.push(sample.input_color_marker_bracket_us);
        fence_readiness.record(sample.decode_fence_ready_at_admission);
    }
    for (key, candidate) in &accepted_candidates {
        if candidate_sample_counts.get(key).copied().unwrap_or_default()
            != candidate.scheduled_samples
        {
            evidence.candidate_sample_count_mismatches =
                evidence.candidate_sample_count_mismatches.saturating_add(1);
        }
    }

    ProfessionalNativeVideoGpuTimingReport {
        adapter: NativeVideoGpuTimingAdapterReport::default(),
        evidence,
        yuv_decode_marker_bracket: NativeVideoGpuMarkerTimingStatistics::from_samples(&yuv_samples),
        input_color_marker_bracket: NativeVideoGpuMarkerTimingStatistics::from_samples(
            &input_color_samples,
        ),
        decode_fence_ready_at_admission: fence_readiness,
    }
}

impl HeadlessGpuExecutionObserver for HeadlessViewerGpuExecutionSummary {
    fn execution_completed(
        &mut self,
        execution: HeadlessViewerGpuExecution,
        disposition: HeadlessGpuExecutionDisposition,
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) {
        self.record(execution, disposition, completed_demand);
    }

    fn current_output_presented(
        &mut self,
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) {
        self.record_current_output_presentation(completed_demand);
    }

    fn successor_preparation(&mut self, ready: bool) {
        self.successor_preparation_attempts = self.successor_preparation_attempts.saturating_add(1);
        if ready {
            self.successor_preparation_ready = self.successor_preparation_ready.saturating_add(1);
        }
    }
}

fn checked_evidence_sum(values: &[u64]) -> Option<u64> {
    values.iter().try_fold(0_u64, |total, value| total.checked_add(*value))
}

fn build_native_video_gpu_timing_report(
    final_evidence: HeadlessNativeVideoImportGpuTimingFinalEvidence,
    summaries: &[&HeadlessViewerGpuExecutionSummary],
) -> ProfessionalNativeVideoGpuTimingReport {
    let HeadlessNativeVideoImportGpuTimingFinalEvidence { diagnostics, samples: raw_samples } =
        final_evidence;
    let session_id = diagnostics.session_id.get();
    let candidate_count = summaries.iter().fold(0_u64, |count, summary| {
        count.saturating_add(
            u64::try_from(summary.native_video_gpu_timing_candidates.len()).unwrap_or(u64::MAX),
        )
    });
    let candidates = summaries
        .iter()
        .flat_map(|summary| summary.native_video_gpu_timing_candidates.iter().copied());
    let sample_count = u64::try_from(raw_samples.len()).unwrap_or(u64::MAX);
    let samples = raw_samples.into_iter().map(|sample| NativeVideoGpuTimingSampleRecord {
        key: NativeVideoGpuTimingSampleKey {
            session_id: sample.session_id.get(),
            import_token: sample.import_token,
        },
        candidate: NativeVideoGpuTimingCandidateKey {
            session_id: sample.session_id.get(),
            candidate_token: sample.candidate_token,
        },
        yuv_decode_marker_bracket_us: sample.yuv_decode_marker_bracket_us,
        input_color_marker_bracket_us: sample.input_color_marker_bracket_us,
        decode_fence_ready_at_admission: sample.decode_fence_ready_at_admission,
    });
    let mut report = reconcile_native_video_gpu_timings(
        session_id,
        &diagnostics.renderer,
        candidates,
        samples,
        diagnostics.adapter_overflow_samples,
    );
    report.evidence.orphan_candidate_receipts = report
        .evidence
        .orphan_candidate_receipts
        .saturating_add(candidate_count.abs_diff(diagnostics.candidate_receipts));
    report.evidence.unmatched_samples = report
        .evidence
        .unmatched_samples
        .saturating_add(sample_count.abs_diff(diagnostics.drained_samples))
        .saturating_add(u64::try_from(diagnostics.buffered_samples).unwrap_or(u64::MAX));
    report.adapter = NativeVideoGpuTimingAdapterReport {
        session_id,
        candidate_receipts: diagnostics.candidate_receipts,
        observation_capacity: diagnostics.observation_capacity,
        buffered_samples: diagnostics.buffered_samples,
        drained_samples: diagnostics.drained_samples,
        adapter_overflow_samples: diagnostics.adapter_overflow_samples,
    };
    report
}

type HeadlessGpuExecutionPublication = HeadlessGpuExecutionDisposition;

fn headless_test_demand_identity(frame: i64) -> mondrian_playback::FrameDemandIdentity {
    let mut state = AppState::new();
    state.set_playback_frame_running(frame);
    state.pending_playback_frame_demand_identity().expect("test Frame Demand")
}

#[test]
fn current_headless_gpu_output_counts_as_a_cached_presentation() {
    let mut summary = HeadlessViewerGpuExecutionSummary::default();

    summary.record_current_output_presentation(None);

    assert_eq!(summary.rendered_frames, 0);
    assert_eq!(summary.published_cached_output_observations, 1);
    assert_eq!(summary.published_output_observations, 1);
    assert_eq!(summary.presented_demand_completions, 0);
}

#[test]
fn repeated_demands_for_one_epoch_frame_count_as_one_publication_coverage_unit() {
    let policy = mondrian_playback::PlaybackPolicy {
        pressure_window: 1,
        pressure_threshold: 1,
        ..mondrian_playback::PlaybackPolicy::default()
    };
    let mut engine =
        mondrian_playback::PlaybackEngine::new(Rational::new(1, 25), policy).expect("engine");
    let binding =
        mondrian_playback::PlaybackTimelineBinding::new(None, 1, Rational::new(1, 25), 100)
            .expect("timeline binding");
    engine
        .play_timeline(
            binding,
            FramePosition::new(17, Rational::new(1, 25)),
            mondrian_playback::MonotonicTimestamp::ZERO,
        )
        .expect("play");
    engine
        .complete_priming(
            mondrian_playback::ClockMaster::Synthetic,
            mondrian_playback::MonotonicTimestamp::ZERO,
        )
        .expect("initial priming");
    let first = engine.pending_frame_demand().expect("first demand").identity();
    let application = engine
        .observe_frame_delivery(
            mondrian_playback::FrameDeliveryCandidate::for_demand(
                first,
                mondrian_playback::FrameDeliveryKind::Late,
            )
            .complete_at(mondrian_playback::MonotonicTimestamp::ZERO),
        )
        .expect("pressure delivery");
    assert!(application.accepted());
    let second = engine.pending_frame_demand().expect("reissued demand").identity();
    assert_ne!(first.sequence, second.sequence);
    assert_eq!(
        (first.epoch, first.target_frame),
        (second.epoch, second.target_frame)
    );

    let mut summary = HeadlessViewerGpuExecutionSummary::default();
    summary.record_current_output_presentation(Some(first));
    summary.record_current_output_presentation(Some(second));

    assert_eq!(summary.presented_demand_completions, 2);
    assert_eq!(summary.presented_unique_frame_completions, 1);
}

#[test]
fn gpu_timestamp_reconciliation_is_linear_and_preserves_evidence_classes() {
    let mut summary = HeadlessViewerGpuExecutionSummary::default();
    summary.expected_gpu_timestamp_tokens.extend([11, 12]);

    assert!(summary.accept_gpu_timestamp_sample(11));
    assert!(!summary.accept_gpu_timestamp_sample(11));
    assert!(!summary.accept_gpu_timestamp_sample(99));
    summary.finish_gpu_timestamp_reconciliation();

    assert_eq!(summary.duplicate_gpu_timestamp_samples, 1);
    assert_eq!(summary.unmatched_gpu_timestamp_samples, 1);
    assert_eq!(summary.missing_gpu_timestamp_frames, 1);
    assert!(summary.expected_gpu_timestamp_tokens.is_empty());
}

#[test]
fn gpu_timestamp_distribution_requires_exactly_one_phase_owner() {
    let mut preroll = HeadlessViewerGpuExecutionSummary::default();
    let mut main = HeadlessViewerGpuExecutionSummary::default();
    preroll.expected_gpu_timestamp_tokens.extend([1, 2]);
    main.expected_gpu_timestamp_tokens.extend([2, 3]);
    let summaries = [&preroll, &main];
    let owner = |token| {
        gpu_timestamp_sample_owner(
            token,
            summaries.iter().enumerate().map(|(index, summary)| (index, *summary)),
        )
    };

    assert_eq!(owner(1), GpuTimestampSampleOwner::Exact(0));
    assert_eq!(owner(3), GpuTimestampSampleOwner::Exact(1));
    assert_eq!(owner(9), GpuTimestampSampleOwner::Unmatched);
    assert_eq!(owner(2), GpuTimestampSampleOwner::Duplicate);
}

#[test]
fn native_video_gpu_timing_reconciliation_preserves_release_and_publication_semantics() {
    let session_id = 7;
    let published = NativeVideoGpuTimingCandidateRecord {
        key: NativeVideoGpuTimingCandidateKey { session_id, candidate_token: 11 },
        submitted_imports: 2,
        scheduled_samples: 2,
        missing_samples: 0,
        dropped_samples: 0,
        published: true,
    };
    let released = NativeVideoGpuTimingCandidateRecord {
        key: NativeVideoGpuTimingCandidateKey { session_id, candidate_token: 12 },
        submitted_imports: 1,
        scheduled_samples: 1,
        missing_samples: 0,
        dropped_samples: 0,
        published: false,
    };
    let sample =
        |import_token, candidate_token, yuv, color, fence| NativeVideoGpuTimingSampleRecord {
            key: NativeVideoGpuTimingSampleKey { session_id, import_token },
            candidate: NativeVideoGpuTimingCandidateKey { session_id, candidate_token },
            yuv_decode_marker_bracket_us: yuv,
            input_color_marker_bracket_us: color,
            decode_fence_ready_at_admission: fence,
        };
    let samples = [
        sample(101, 11, 10, 5, Some(true)),
        sample(102, 11, 30, 15, Some(false)),
        sample(103, 12, 20, 10, None),
    ];
    let renderer = NativeVideoImportGpuTimingDiagnostics {
        schema_version: 1,
        capability_supported: true,
        activated: true,
        inactive_reason: None,
        submitted_imports: 3,
        samples: 3,
        pending: 0,
        missing: 0,
        dropped: 0,
    };

    let report = reconcile_native_video_gpu_timings(
        session_id,
        &renderer,
        [published, released],
        samples,
        0,
    );

    assert_eq!(report.evidence.receipt_candidates, 2);
    assert_eq!(report.evidence.receipt_submitted_imports, 3);
    assert_eq!(report.evidence.observed_samples, 3);
    assert_eq!(report.evidence.published_native_candidates, 1);
    assert_eq!(report.evidence.published_native_samples, 2);
    assert_eq!(
        report.yuv_decode_marker_bracket,
        NativeVideoGpuMarkerTimingStatistics {
            samples: 3,
            p50_us: 20,
            p95_us: 30,
            mean_us: 20,
            max_us: 30,
        }
    );
    assert_eq!(
        report.input_color_marker_bracket,
        NativeVideoGpuMarkerTimingStatistics {
            samples: 3,
            p50_us: 10,
            p95_us: 15,
            mean_us: 10,
            max_us: 15,
        }
    );
    assert_eq!(
        report.decode_fence_ready_at_admission,
        NativeVideoGpuFenceReadinessStatistics { ready: 1, not_ready: 1, unknown: 1 }
    );
}

#[test]
fn native_video_gpu_timing_reconciliation_rejects_cross_candidate_count_swaps() {
    let session_id = 7;
    let candidate = |candidate_token, scheduled_samples| NativeVideoGpuTimingCandidateRecord {
        key: NativeVideoGpuTimingCandidateKey { session_id, candidate_token },
        submitted_imports: scheduled_samples,
        scheduled_samples,
        missing_samples: 0,
        dropped_samples: 0,
        published: true,
    };
    let sample = |import_token| NativeVideoGpuTimingSampleRecord {
        key: NativeVideoGpuTimingSampleKey { session_id, import_token },
        candidate: NativeVideoGpuTimingCandidateKey { session_id, candidate_token: 11 },
        yuv_decode_marker_bracket_us: 10,
        input_color_marker_bracket_us: 5,
        decode_fence_ready_at_admission: Some(true),
    };
    let renderer = NativeVideoImportGpuTimingDiagnostics {
        schema_version: 1,
        capability_supported: true,
        activated: true,
        inactive_reason: None,
        submitted_imports: 3,
        samples: 3,
        pending: 0,
        missing: 0,
        dropped: 0,
    };

    let report = reconcile_native_video_gpu_timings(
        session_id,
        &renderer,
        [candidate(11, 2), candidate(12, 1)],
        [sample(101), sample(102), sample(103)],
        0,
    );

    assert_eq!(
        report.evidence.renderer_samples,
        report.evidence.observed_samples
    );
    assert_eq!(
        report.evidence.receipt_scheduled_samples,
        report.evidence.observed_samples
    );
    assert_eq!(report.evidence.unmatched_samples, 0);
    assert_eq!(report.evidence.candidate_sample_count_mismatches, 2);
}

#[test]
fn native_video_gpu_timing_reconciliation_rejects_ambiguous_foreign_and_duplicate_evidence() {
    let session_id = 7;
    let duplicate = NativeVideoGpuTimingCandidateRecord {
        key: NativeVideoGpuTimingCandidateKey { session_id, candidate_token: 11 },
        submitted_imports: 1,
        scheduled_samples: 1,
        missing_samples: 0,
        dropped_samples: 0,
        published: true,
    };
    let foreign = NativeVideoGpuTimingCandidateRecord {
        key: NativeVideoGpuTimingCandidateKey { session_id: 8, candidate_token: 12 },
        ..duplicate
    };
    let invalid = NativeVideoGpuTimingCandidateRecord {
        key: NativeVideoGpuTimingCandidateKey { session_id, candidate_token: 13 },
        submitted_imports: 2,
        ..duplicate
    };
    let sample = |sample_session, import_token, candidate_session, candidate_token| {
        NativeVideoGpuTimingSampleRecord {
            key: NativeVideoGpuTimingSampleKey { session_id: sample_session, import_token },
            candidate: NativeVideoGpuTimingCandidateKey {
                session_id: candidate_session,
                candidate_token,
            },
            yuv_decode_marker_bracket_us: 10,
            input_color_marker_bracket_us: 5,
            decode_fence_ready_at_admission: Some(true),
        }
    };
    let samples = [
        sample(7, 101, 7, 11),
        sample(7, 101, 7, 11),
        sample(8, 102, 8, 12),
        sample(7, 103, 7, 99),
    ];
    let renderer = NativeVideoImportGpuTimingDiagnostics {
        schema_version: 1,
        capability_supported: true,
        activated: true,
        inactive_reason: None,
        submitted_imports: 3,
        samples: 3,
        pending: 0,
        missing: 0,
        dropped: 0,
    };

    let report = reconcile_native_video_gpu_timings(
        session_id,
        &renderer,
        [duplicate, duplicate, foreign, invalid],
        samples,
        2,
    );

    assert_eq!(report.evidence.receipt_candidates, 0);
    assert_eq!(report.evidence.duplicate_candidate_receipts, 1);
    assert_eq!(report.evidence.orphan_candidate_receipts, 4);
    assert_eq!(report.evidence.observed_samples, 3);
    assert_eq!(report.evidence.duplicate_samples, 1);
    assert_eq!(report.evidence.duplicate_sample_ownership, 1);
    assert_eq!(report.evidence.unmatched_samples, 2);
    assert_eq!(report.evidence.adapter_dropped_samples, 2);
    assert_eq!(report.yuv_decode_marker_bracket.samples, 0);
}

#[test]
fn headless_gpu_summary_separates_execution_publication_and_terminal_rejection() {
    let execution = |label: &str, token: u64, decode_execution: PreviewDecodeExecutionSummary| {
        HeadlessViewerGpuExecution {
            submission_id: token,
            output: HeadlessViewerGpuOutput {
                resource_key: label.to_owned(),
                width: 960,
                height: 540,
            },
            output_width: 960,
            output_height: 540,
            duration_us: 1,
            record_submit_us: 1,
            completion_wait_us: 1,
            gpu_completion_observed: true,
            gpu_timestamp_token: Some(token),
            cpu_stage_timings: Some(ViewerGpuExecutionCpuStageTimings::default()),
            native_import_gpu_timing_receipt: None,
            compositing_diagnostics: None,
            compositor_uniform_arena: None,
            compositor_texture_bindings: None,
            compositor_creative_luts: None,
            spatial_diagnostics: None,
            resource_pool_diagnostics: Default::default(),
            stage_diagnostics: None,
            fallback_reasons: Vec::new(),
            decode_execution,
            native_import_contract_pools: 0,
            native_import_bridge_entries: 0,
            native_import_retained_sources: 0,
            remaining_submissions_after_completion: 0,
        }
    };
    let native_decode = PreviewDecodeExecutionSummary {
        media_layers: 1,
        hardware_native_layers: 1,
        p010_10_bit_hardware_layers: 1,
        ..PreviewDecodeExecutionSummary::default()
    };
    let mut summary = HeadlessViewerGpuExecutionSummary::default();

    summary.record(
        execution("published", 1, native_decode),
        HeadlessGpuExecutionPublication::PublishedCurrent,
        Some(headless_test_demand_identity(7)),
    );
    summary.record(
        execution("released", 2, native_decode),
        HeadlessGpuExecutionPublication::Released,
        None,
    );
    summary.record(
        execution("prepared", 3, native_decode),
        HeadlessGpuExecutionPublication::PreparedSuccessor,
        None,
    );
    summary.record(
        execution("late", 4, native_decode),
        HeadlessGpuExecutionPublication::TerminalRejected(
            mondrian_playback::FrameDeliveryKind::Late,
        ),
        None,
    );
    assert_eq!(summary.rendered_frames, 4);
    assert_eq!(summary.published_rendered_frames, 1);
    assert_eq!(summary.prepared_successor_frames, 1);
    assert_eq!(summary.released_rendered_frames, 1);
    assert_eq!(summary.terminal_rejected_rendered_frames, 1);
    assert_eq!(summary.late_rejected_rendered_frames, 1);
    assert_eq!(summary.published_output_observations, 1);
    assert_eq!(summary.presented_demand_completions, 1);
    assert_eq!(summary.rendered_decode_execution.hardware_native_layers, 4);
    assert_eq!(
        summary.published_rendered_decode_execution.hardware_native_layers, 1,
        "prepared, released, or Late executions cannot become presented decode evidence"
    );
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
    percentile_sample_us(samples, 95)
}

fn percentile_sample_us(samples: &[u64], percentile: usize) -> u64 {
    debug_assert!((1..=100).contains(&percentile));
    let mut samples = samples.to_vec();
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let rank = samples.len().saturating_mul(percentile).saturating_add(99) / 100;
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

#[test]
fn headless_gpu_summary_records_distinct_executed_extents() {
    let mut summary = HeadlessViewerGpuExecutionSummary::default();
    for (width, height) in [(960, 540), (960, 540), (480, 270)] {
        summary.record(
            HeadlessViewerGpuExecution {
                submission_id: u64::from(width) << 32 | u64::from(height),
                output: HeadlessViewerGpuOutput {
                    resource_key: format!("test-{width}x{height}"),
                    width,
                    height,
                },
                output_width: width,
                output_height: height,
                duration_us: 1,
                record_submit_us: 1,
                completion_wait_us: 1,
                gpu_completion_observed: true,
                gpu_timestamp_token: Some(u64::from(width) << 32 | u64::from(height)),
                cpu_stage_timings: Some(ViewerGpuExecutionCpuStageTimings::default()),
                native_import_gpu_timing_receipt: None,
                compositing_diagnostics: None,
                compositor_uniform_arena: None,
                compositor_texture_bindings: None,
                compositor_creative_luts: None,
                spatial_diagnostics: None,
                resource_pool_diagnostics: Default::default(),
                stage_diagnostics: None,
                fallback_reasons: Vec::new(),
                decode_execution: PreviewDecodeExecutionSummary::default(),
                native_import_contract_pools: 0,
                native_import_bridge_entries: 0,
                native_import_retained_sources: 0,
                remaining_submissions_after_completion: 0,
            },
            HeadlessGpuExecutionPublication::PublishedCurrent,
            Some(headless_test_demand_identity(i64::from(width))),
        );
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
fn professional_frame_count_covers_duration_terminal_and_measurement_guard() {
    let frame_count =
        professional_min_frame_count_for_interval(40_000_000).expect("25 fps interval");
    let plan =
        continuous_playback_observation_plan(frame_count).expect("positive observation count");

    assert_eq!(frame_count, 45_002);
    assert_eq!(plan.advancing_intervals, 45_001);
    assert_eq!(plan.terminal_observations, 1);
    assert_eq!(
        plan.advancing_intervals + plan.terminal_observations,
        frame_count
    );
}

#[test]
fn continuous_playback_observation_plan_rejects_an_empty_window() {
    assert!(continuous_playback_observation_plan(0).is_err());
}

#[test]
fn continuous_playback_opportunity_ledger_classifies_clock_gaps_once() {
    let epoch = headless_candidate_test_intent(0).epoch;
    let mut ledger =
        ContinuousPlaybackOpportunityLedger::new(epoch, 10, 4).expect("opportunity ledger");
    let mut readiness = PreviewReadinessCounts::default();

    ledger
        .record(
            epoch,
            10,
            HeadlessPreviewSample {
                current_gpu_ready: true,
                stale_output_available: false,
                unavailable: false,
            },
            &mut readiness,
        )
        .expect("first opportunity");
    ledger
        .record(
            epoch,
            12,
            HeadlessPreviewSample {
                current_gpu_ready: false,
                stale_output_available: true,
                unavailable: false,
            },
            &mut readiness,
        )
        .expect("post-gap opportunity");
    ledger
        .record(
            epoch,
            13,
            HeadlessPreviewSample {
                current_gpu_ready: true,
                stale_output_available: false,
                unavailable: false,
            },
            &mut readiness,
        )
        .expect("terminal opportunity");

    assert_eq!(ledger.observed_opportunities(), 3);
    assert_eq!(ledger.missed_opportunities(), 1);
    assert_eq!(ledger.classified_opportunities(), 4);
    assert_eq!(readiness.ready, 2);
    assert_eq!(readiness.stale, 1);
    assert_eq!(readiness.missed_deadline, 1);
    assert!(
        ledger
            .record(
                epoch,
                13,
                HeadlessPreviewSample {
                    current_gpu_ready: true,
                    stale_output_available: false,
                    unavailable: false,
                },
                &mut readiness,
            )
            .is_err(),
        "an already classified opportunity must not be sampled twice"
    );
}

#[test]
fn continuous_playback_opportunity_ledger_caps_a_gap_at_the_window_boundary() {
    let epoch = headless_candidate_test_intent(0).epoch;
    let mut ledger =
        ContinuousPlaybackOpportunityLedger::new(epoch, 10, 4).expect("opportunity ledger");
    let mut readiness = PreviewReadinessCounts::default();

    ledger
        .record(
            epoch,
            15,
            HeadlessPreviewSample {
                current_gpu_ready: true,
                stale_output_available: false,
                unavailable: false,
            },
            &mut readiness,
        )
        .expect("gap crossing the terminal boundary");

    assert!(ledger.is_complete());
    assert_eq!(ledger.classified_opportunities(), 4);
    assert_eq!(ledger.observed_opportunities(), 0);
    assert_eq!(ledger.missed_opportunities(), 4);
    assert_eq!(readiness.ready, 0);
    assert_eq!(readiness.missed_deadline, 4);
}

#[test]
fn continuous_playback_window_accepts_authored_end_with_one_missing_frame_opportunity() {
    let mut playback = PlaybackEvidenceCollector::default().report();
    playback.first_epoch = Some(7);
    playback.latest_epoch = Some(7);
    playback.observed_duration_us = 1_000_000;
    playback.clock_frame_advances.advanced_frames = 59;
    playback.clock_frame_advances.multi_frame_advances = 1;
    playback.clock_frame_advances.skipped_intermediate_frames = 1;
    let window = ContinuousPlaybackWindowEvidence {
        target_observations: 60,
        observed_observations: 59,
        required_duration_us: 1_000_000,
        start_frame: 0,
        terminal_frame: 59,
        planned_terminal_frame: 59,
        reached_natural_end: true,
        wall_duration_us: 1_000_000,
        playback,
    };
    let readiness = PreviewReadinessCounts {
        ready: 59,
        missed_deadline: 1,
        ..PreviewReadinessCounts::default()
    };

    let gate = evaluate_continuous_playback_window(&window, &readiness);

    assert!(gate.passed, "{:?}", gate.failures);
    assert!(gate.failures.is_empty());
}

#[test]
fn continuous_window_duration_and_startup_headroom_respect_frame_phase() {
    let frame_interval_ns = 16_683_333;

    assert_eq!(
        minimum_continuous_window_duration_us(7, frame_interval_ns),
        100_099
    );
    assert_eq!(
        startup_headroom_frames(Duration::from_secs(10), frame_interval_ns),
        600
    );
}

#[test]
fn source_full_playback_probe_authors_the_source_extent_at_unit_scale() {
    let mut sequence = Sequence::new("Source Full playback probe");
    let source_resolution = Resolution { width: 3840, height: 2160 };

    configure_preview_media_authored_output(
        &mut sequence,
        PreviewMediaAuthoredOutput::SourceFull,
        source_resolution,
    )
    .expect("configure source-Full authored output");

    assert_eq!(sequence.settings.resolution, source_resolution);
    assert_eq!(sequence.settings.preview.resolution_scale, 1.0);
}

#[test]
fn real_media_probe_rate_maps_to_the_nearest_supported_sequence_grid() {
    assert_eq!(
        canonical_preview_probe_sequence_frame_rate(Rational::new(724_800, 12_097))
            .expect("map the observed 59.916 fps cadence"),
        Rational::FPS_5994
    );
    assert!(canonical_preview_probe_sequence_frame_rate(Rational::new(48, 1)).is_err());
}

#[test]
fn pause_seek_resume_gate_requires_bounded_presentation_continuity() {
    let passing = evaluate_pause_seek_resume(
        12,
        12,
        100,
        112,
        Some(9),
        Some(9),
        3,
        1,
        PreviewReadinessCounts { ready: 12, ..PreviewReadinessCounts::default() },
    );
    assert!(passing.passed, "{:?}", passing.failures);

    let isolated_stale = evaluate_pause_seek_resume(
        12,
        12,
        100,
        112,
        Some(9),
        Some(9),
        3,
        1,
        PreviewReadinessCounts {
            ready: 11,
            stale: 1,
            ..PreviewReadinessCounts::default()
        },
    );
    assert!(isolated_stale.passed, "{:?}", isolated_stale.failures);

    let stale_burst = evaluate_pause_seek_resume(
        12,
        12,
        100,
        112,
        Some(9),
        Some(9),
        3,
        2,
        PreviewReadinessCounts {
            ready: 10,
            stale: 2,
            ..PreviewReadinessCounts::default()
        },
    );
    assert!(!stale_burst.passed);
    assert_eq!(stale_burst.failures, vec!["resume_presentation_continuity"]);
}

#[test]
fn playback_resize_gate_requires_geometry_change_and_bounded_presentation_continuity() {
    let authored_full_extent = HeadlessViewerGpuExtent { width: 3840, height: 2160 };
    let passing = evaluate_playback_resize(
        8,
        8,
        20,
        28,
        Some(17),
        Some(17),
        4,
        1,
        4,
        true,
        true,
        PreviewReadinessCounts {
            ready: 7,
            stale: 1,
            ..PreviewReadinessCounts::default()
        },
        &authored_full_extent,
        &[HeadlessViewerGpuExtent { width: 3840, height: 2160 }],
    );
    assert!(passing.passed, "{:?}", passing.failures);

    let stale_burst = evaluate_playback_resize(
        8,
        8,
        20,
        28,
        Some(17),
        Some(17),
        4,
        2,
        4,
        true,
        true,
        PreviewReadinessCounts {
            ready: 6,
            stale: 2,
            ..PreviewReadinessCounts::default()
        },
        &authored_full_extent,
        &[HeadlessViewerGpuExtent { width: 3840, height: 2160 }],
    );
    assert_eq!(stale_burst.failures, vec!["resize_presentation_continuity"]);

    let excessive_displacement = evaluate_playback_resize(
        8,
        8,
        20,
        33,
        Some(17),
        Some(17),
        (PREVIEW_GPU_CPU_STAGING_CAPACITY + 1) as u64,
        0,
        4,
        true,
        true,
        PreviewReadinessCounts { ready: 8, ..PreviewReadinessCounts::default() },
        &authored_full_extent,
        &[HeadlessViewerGpuExtent { width: 3840, height: 2160 }],
    );
    assert_eq!(
        excessive_displacement.failures,
        vec!["resize_frame_displacement_exceeded"]
    );

    let unchanged_geometry = evaluate_playback_resize(
        8,
        8,
        20,
        28,
        Some(17),
        Some(17),
        1,
        0,
        1,
        true,
        true,
        PreviewReadinessCounts { ready: 8, ..PreviewReadinessCounts::default() },
        &authored_full_extent,
        &[HeadlessViewerGpuExtent { width: 3840, height: 2160 }],
    );
    assert!(!unchanged_geometry.passed);
    assert_eq!(
        unchanged_geometry.failures,
        vec!["viewer_geometry_unchanged"]
    );

    let scaled_gpu_output = evaluate_playback_resize(
        8,
        8,
        20,
        28,
        Some(17),
        Some(17),
        4,
        0,
        4,
        true,
        true,
        PreviewReadinessCounts { ready: 8, ..PreviewReadinessCounts::default() },
        &authored_full_extent,
        &[HeadlessViewerGpuExtent { width: 1920, height: 1080 }],
    );
    assert!(!scaled_gpu_output.passed);
    assert_eq!(
        scaled_gpu_output.failures,
        vec!["authored_full_gpu_extent_changed"]
    );
}

#[test]
fn dual_video_gate_requires_two_layers_and_real_gpu_composite() {
    let full_extent = HeadlessViewerGpuExtent { width: 3840, height: 2160 };
    let passing = evaluate_multilayer_playback(
        2,
        10,
        20,
        0,
        10,
        &[HeadlessViewerGpuExtent { width: 3840, height: 2160 }],
        &full_extent,
    );
    assert!(passing.passed, "{:?}", passing.failures);

    let passthrough = evaluate_multilayer_playback(
        2,
        10,
        10,
        10,
        0,
        &[HeadlessViewerGpuExtent { width: 3840, height: 2160 }],
        &full_extent,
    );
    assert!(!passthrough.passed);
    assert_eq!(
        passthrough.failures,
        vec![
            "multilayer_media_execution_incomplete",
            "multilayer_used_passthrough",
            "multilayer_native_composite_incomplete",
        ]
    );
}

#[test]
fn continuous_playback_window_reports_early_end_and_short_duration_without_bailing() {
    let mut playback = PlaybackEvidenceCollector::default().report();
    playback.first_epoch = Some(7);
    playback.latest_epoch = Some(7);
    playback.observed_duration_us = 900_000;
    playback.clock_frame_advances.advanced_frames = 58;
    let window = ContinuousPlaybackWindowEvidence {
        target_observations: 60,
        observed_observations: 59,
        required_duration_us: 1_000_000,
        start_frame: 0,
        terminal_frame: 58,
        planned_terminal_frame: 59,
        reached_natural_end: true,
        wall_duration_us: 900_000,
        playback,
    };
    let readiness = PreviewReadinessCounts { ready: 59, ..PreviewReadinessCounts::default() };

    let gate = evaluate_continuous_playback_window(&window, &readiness);

    assert!(!gate.passed);
    assert_eq!(
        gate.failures,
        vec![
            "wall_duration_below_window",
            "playback_evidence_duration_below_window",
            "natural_end_outside_authored_boundary",
            "observation_window_incomplete",
        ]
    );
}

#[test]
fn professional_playback_case_budget_includes_observation_and_bounded_overhead() {
    assert_eq!(
        professional_playback_case_budget_ms(45_001, 40_000_000),
        2_100_040
    );
}

#[test]
fn professional_native_video_gpu_timing_capacity_is_explicit_and_bounded() {
    assert_eq!(
        professional_native_video_gpu_timing_observation_capacity(45_001, 100)
            .expect("30-minute timing capacity"),
        (45_001 + 100 + PROFESSIONAL_NATIVE_VIDEO_GPU_CANDIDATE_OVERHEAD)
            * PROFESSIONAL_NATIVE_VIDEO_GPU_IMPORTS_PER_CANDIDATE_BUDGET
    );
    assert!(professional_native_video_gpu_timing_observation_capacity(
        PROFESSIONAL_NATIVE_VIDEO_GPU_OBSERVATION_CAPACITY_LIMIT,
        0,
    )
    .is_err());
}

#[derive(Debug, Serialize)]
struct PreviewMediaPlaybackPerfReport {
    scenario: &'static str,
    frames: usize,
    frame_interval_ns: u64,
    media_probe: PreviewPlaybackMediaProbeReport,
    authored_output: PreviewMediaAuthoredOutputEvidence,
    pause_seek_resume_probe: Option<PreviewPauseSeekResumeEvidence>,
    playback_resize_probe: Option<PreviewPlaybackResizeEvidence>,
    multilayer_playback: Option<PreviewMultilayerPlaybackEvidence>,
    readiness: PreviewReadinessCounts,
    headless_gpu_preroll: HeadlessViewerGpuExecutionSummary,
    /// GPU work owned by the uninterrupted playback window only.
    headless_gpu: HeadlessViewerGpuExecutionSummary,
    /// Settled seek/cancellation/final-candidate work after the window froze.
    headless_gpu_post_window: HeadlessViewerGpuExecutionSummary,
    /// Real GPU work performed while the production Viewer layout changes size.
    headless_gpu_resize: HeadlessViewerGpuExecutionSummary,
    cancellation_recovery_probe: Option<PreviewCancellationRecoveryEvidence>,
    real_media_gates: Option<PreviewExternalPlaybackGateReport>,
    qualification_media_gates: Option<PreviewPlaybackQualificationGateReport>,
    professional_media_gates: Option<PreviewProfessionalPlaybackGateReport>,
    media_color_issues: VideoColorDiagnosticIssueAggregate,
    continuous_preview_diagnostics: PreviewDiagnostics,
    preview_diagnostics: PreviewDiagnostics,
    preview_color_report: PreviewColorHealthReport,
    decode_failure_codes: Vec<&'static str>,
    render_failure_codes: Vec<&'static str>,
    preview_decode_report: PreviewDecodePerformanceReport,
    preview_render_report: Option<PreviewRenderPerformanceReport>,
    continuous_playback_window: ContinuousPlaybackWindowEvidence,
    playback_evidence: PlaybackEvidenceReport,
    process_memory_evidence: PreviewProcessMemoryEvidenceReport,
    native_video_gpu_timing: ProfessionalNativeVideoGpuTimingReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PreviewMediaAuthoredOutput {
    SequenceDefault,
    SourceFull,
}

#[derive(Debug, Serialize)]
struct PreviewMediaAuthoredOutputEvidence {
    mode: PreviewMediaAuthoredOutput,
    resolution: Resolution,
    resolution_scale: f32,
    full_extent: HeadlessViewerGpuExtent,
}

#[derive(Debug, Serialize)]
struct PreviewPauseSeekResumeEvidence {
    requested_observations: usize,
    observed_observations: usize,
    start_frame: i64,
    end_frame: i64,
    first_epoch: Option<u64>,
    last_epoch: Option<u64>,
    maximum_frame_advance: u64,
    maximum_consecutive_stale: u64,
    allowed_stale_observations: usize,
    readiness: PreviewReadinessCounts,
    passed: bool,
    failures: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct PreviewPlaybackResizeEvidence {
    requested_observations: usize,
    observed_observations: usize,
    start_frame: i64,
    end_frame: i64,
    first_epoch: Option<u64>,
    last_epoch: Option<u64>,
    maximum_frame_advance: u64,
    maximum_consecutive_stale: u64,
    allowed_stale_observations: usize,
    unique_presentation_extents: usize,
    presentation_geometry_valid: bool,
    authored_output_unchanged: bool,
    readiness: PreviewReadinessCounts,
    authored_full_gpu_extent_exact: bool,
    passed: bool,
    failures: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct PreviewMultilayerPlaybackEvidence {
    expected_layers_per_frame: u32,
    rendered_frames: usize,
    expected_media_layer_executions: u64,
    observed_media_layer_executions: u64,
    gpu_passthrough_frames: u64,
    gpu_native_composites: u64,
    authored_full_gpu_extent_exact: bool,
    passed: bool,
    failures: Vec<&'static str>,
}

fn evaluate_pause_seek_resume(
    requested_observations: usize,
    observed_observations: usize,
    start_frame: i64,
    end_frame: i64,
    first_epoch: Option<u64>,
    last_epoch: Option<u64>,
    maximum_frame_advance: u64,
    maximum_consecutive_stale: u64,
    readiness: PreviewReadinessCounts,
) -> PreviewPauseSeekResumeEvidence {
    let mut failures = Vec::new();
    if observed_observations != requested_observations {
        failures.push("resume_observation_window_incomplete");
    }
    if end_frame <= start_frame {
        failures.push("resume_no_forward_progress");
    }
    if first_epoch.is_none() || first_epoch != last_epoch {
        failures.push("resume_epoch_changed");
    }
    if maximum_frame_advance == 0 || maximum_frame_advance > PREVIEW_GPU_CPU_STAGING_CAPACITY as u64
    {
        failures.push("resume_frame_displacement_exceeded");
    }
    let allowed_stale_observations = requested_observations.div_ceil(12);
    if readiness.ready.saturating_add(readiness.stale) != requested_observations
        || readiness.stale > allowed_stale_observations
        || maximum_consecutive_stale > 1
        || readiness.loading > 0
        || readiness.unavailable > 0
        || readiness.missed_deadline > 0
    {
        failures.push("resume_presentation_continuity");
    }
    PreviewPauseSeekResumeEvidence {
        requested_observations,
        observed_observations,
        start_frame,
        end_frame,
        first_epoch,
        last_epoch,
        maximum_frame_advance,
        maximum_consecutive_stale,
        allowed_stale_observations,
        readiness,
        passed: failures.is_empty(),
        failures,
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_playback_resize(
    requested_observations: usize,
    observed_observations: usize,
    start_frame: i64,
    end_frame: i64,
    first_epoch: Option<u64>,
    last_epoch: Option<u64>,
    maximum_frame_advance: u64,
    maximum_consecutive_stale: u64,
    unique_presentation_extents: usize,
    presentation_geometry_valid: bool,
    authored_output_unchanged: bool,
    readiness: PreviewReadinessCounts,
    authored_full_extent: &HeadlessViewerGpuExtent,
    gpu_output_extents: &[HeadlessViewerGpuExtent],
) -> PreviewPlaybackResizeEvidence {
    let mut failures = Vec::new();
    if observed_observations != requested_observations {
        failures.push("resize_observation_window_incomplete");
    }
    if end_frame <= start_frame {
        failures.push("resize_no_forward_progress");
    }
    if maximum_frame_advance == 0 || maximum_frame_advance > PREVIEW_GPU_CPU_STAGING_CAPACITY as u64
    {
        failures.push("resize_frame_displacement_exceeded");
    }
    if first_epoch.is_none() || first_epoch != last_epoch {
        failures.push("resize_epoch_changed");
    }
    if unique_presentation_extents < 2 {
        failures.push("viewer_geometry_unchanged");
    }
    if !presentation_geometry_valid {
        failures.push("viewer_geometry_invalid");
    }
    if !authored_output_unchanged {
        failures.push("authored_output_changed_during_resize");
    }
    let allowed_stale_observations = requested_observations.div_ceil(8);
    if readiness.ready.saturating_add(readiness.stale) != requested_observations
        || readiness.stale > allowed_stale_observations
        || maximum_consecutive_stale > 1
        || readiness.loading > 0
        || readiness.unavailable > 0
        || readiness.missed_deadline > 0
    {
        failures.push("resize_presentation_continuity");
    }
    let authored_full_gpu_extent_exact = !gpu_output_extents.is_empty()
        && gpu_output_extents.iter().all(|extent| extent == authored_full_extent);
    if !authored_full_gpu_extent_exact {
        failures.push("authored_full_gpu_extent_changed");
    }
    PreviewPlaybackResizeEvidence {
        requested_observations,
        observed_observations,
        start_frame,
        end_frame,
        first_epoch,
        last_epoch,
        maximum_frame_advance,
        maximum_consecutive_stale,
        allowed_stale_observations,
        unique_presentation_extents,
        presentation_geometry_valid,
        authored_output_unchanged,
        readiness,
        authored_full_gpu_extent_exact,
        passed: failures.is_empty(),
        failures,
    }
}

fn evaluate_multilayer_playback(
    expected_layers_per_frame: u32,
    rendered_frames: usize,
    observed_media_layer_executions: u64,
    gpu_passthrough_frames: u64,
    gpu_native_composites: u64,
    gpu_output_extents: &[HeadlessViewerGpuExtent],
    authored_full_extent: &HeadlessViewerGpuExtent,
) -> PreviewMultilayerPlaybackEvidence {
    let expected_media_layer_executions =
        (rendered_frames as u64).saturating_mul(u64::from(expected_layers_per_frame));
    let mut failures = Vec::new();
    if rendered_frames == 0 {
        failures.push("multilayer_no_gpu_frames");
    }
    if expected_layers_per_frame < 2
        || observed_media_layer_executions != expected_media_layer_executions
    {
        failures.push("multilayer_media_execution_incomplete");
    }
    if gpu_passthrough_frames > 0 {
        failures.push("multilayer_used_passthrough");
    }
    if gpu_native_composites < rendered_frames as u64 {
        failures.push("multilayer_native_composite_incomplete");
    }
    let authored_full_gpu_extent_exact = !gpu_output_extents.is_empty()
        && gpu_output_extents.iter().all(|extent| extent == authored_full_extent);
    if !authored_full_gpu_extent_exact {
        failures.push("multilayer_authored_full_gpu_extent_changed");
    }
    PreviewMultilayerPlaybackEvidence {
        expected_layers_per_frame,
        rendered_frames,
        expected_media_layer_executions,
        observed_media_layer_executions,
        gpu_passthrough_frames,
        gpu_native_composites,
        authored_full_gpu_extent_exact,
        passed: failures.is_empty(),
        failures,
    }
}

#[derive(Debug, Clone, Serialize)]
struct ContinuousPlaybackWindowEvidence {
    target_observations: usize,
    observed_observations: usize,
    required_duration_us: u64,
    start_frame: i64,
    terminal_frame: i64,
    planned_terminal_frame: i64,
    reached_natural_end: bool,
    wall_duration_us: u64,
    playback: PlaybackEvidenceReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewNativeVideoGpuTimingPolicy {
    Disabled,
    Strict { observation_capacity: usize },
}

impl PreviewNativeVideoGpuTimingPolicy {
    const fn renderer_policy(self) -> NativeVideoImportGpuTimingPolicy {
        match self {
            Self::Disabled => NativeVideoImportGpuTimingPolicy::Disabled,
            Self::Strict { .. } => NativeVideoImportGpuTimingPolicy::Enabled {
                capacity: NATIVE_VIDEO_IMPORT_GPU_TIMING_MAX_CAPACITY,
            },
        }
    }

    const fn observation_capacity(self) -> usize {
        match self {
            Self::Disabled => 0,
            Self::Strict { observation_capacity } => observation_capacity,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PreviewMediaPlaybackProbeConfig {
    scenario: &'static str,
    frame_count: usize,
    sequence_frame_count: usize,
    frame_interval_ns: u64,
    playback_threshold_ms: u128,
    gpu_candidate_threshold_ms: u128,
    /// Scenario-specific recurring FFmpeg worker-execution bound.
    ///
    /// This must be the same authority used by the outer real-media gate;
    /// otherwise one report can pass 60 ms while its nested report silently
    /// evaluates the same samples against the unrelated 50 ms default.
    decode_slow_frame_budget_us: u64,
    ready_timeout: Duration,
    seek_probe_count: usize,
    seek_threshold_per_settled_ms: u128,
    resume_probe_frames: usize,
    resize_probe_frames: usize,
    video_layer_count: u32,
    probe_cancellation_recovery: bool,
    native_video_gpu_timing: PreviewNativeVideoGpuTimingPolicy,
    absolute_deadline: Option<Instant>,
    authored_output: PreviewMediaAuthoredOutput,
}

#[derive(Debug, Clone, Serialize)]
struct PreviewExternalPlaybackGateReport {
    enabled: bool,
    continuous_window: Option<ContinuousPlaybackWindowGateReport>,
    playback_decode_p95_limit_us: u64,
    playback_decode_p95_observed_us: u64,
    playback_current_queue_wait_limit_us: u64,
    playback_current_queue_wait_observed_us: u64,
    min_visible_frames: usize,
    visible_frames: usize,
    min_ready_frames: usize,
    ready_frames: usize,
    min_ready_basis_points: usize,
    ready_basis_points: usize,
    /// Newly rendered GPU submissions whose completion was observed, whether
    /// or not their artifact retained publication authority.
    gpu_rendered_frames: usize,
    gpu_completion_observed_frames: usize,
    /// Newly rendered executions published as the exact current output.
    gpu_published_rendered_frames: usize,
    /// Completed executions retained under exact immediate-successor authority.
    gpu_prepared_successor_frames: usize,
    /// Completed executions released after their visual lifecycle became stale.
    gpu_released_rendered_frames: usize,
    /// Completed executions rejected by an exact terminal Frame Delivery.
    gpu_terminal_rejected_rendered_frames: usize,
    /// Terminal-rejected rendered executions classified specifically as Late.
    gpu_late_rejected_rendered_frames: usize,
    /// Exact-current observations that reused an already-completed GPU output.
    gpu_published_cached_output_observations: usize,
    /// Cached executions released before publication.
    gpu_released_cached_frames: usize,
    /// Cached executions rejected by an exact terminal Frame Delivery.
    gpu_terminal_rejected_cached_frames: usize,
    /// Terminal-rejected cached executions classified specifically as Late.
    gpu_late_rejected_cached_frames: usize,
    /// Successful exact-current GPU-backed output observations.
    gpu_published_output_observations: usize,
    /// Exact-current GPU-backed publications that consumed a Frame Demand.
    gpu_presented_demand_completions: usize,
    /// Unique `(epoch, target_frame)` coverage used by the publication gate.
    gpu_presented_unique_frame_completions: usize,
    gpu_timestamped_frames: usize,
    gpu_missing_timestamp_frames: usize,
    gpu_discarded_timestamp_frames: u64,
    gpu_execution_p95_limit_us: u64,
    gpu_execution_p95_observed_us: u64,
    gpu_stage_p95_us: GpuTimestampStageDurations,
    gpu_record_submit_p95_us: u64,
    gpu_completion_wait_p95_us: u64,
    gpu_wall_duration_p95_us: u64,
    gpu_cpu_stage_p95_us: ViewerGpuExecutionCpuStageTimings,
    gpu_readback_stages: u64,
    gpu_blockers: u64,
    gpu_fallback_count: usize,
    delivery_phase_error_p95_limit_us: u64,
    audio_device_delivery_phase: PlaybackClockPhaseErrorSummary,
    synthetic_delivery_phase: PlaybackClockPhaseErrorSummary,
    unproven_presentable_deliveries: u64,
    phase_not_applicable_deliveries: u64,
    audio_underrun_recoveries: u64,
    playback_temporal_approximation_frames: u64,
    playback_temporal_mismatch_failures: u64,
    clock_advanced_frames: u64,
    max_clock_skipped_intermediate_frames: u64,
    clock_skipped_intermediate_frames: u64,
    evicted_playback_evidence_events: u64,
    cpu_frame_store_within_budget: bool,
    decoder_resource_store_within_budget: bool,
    cpu_frame_store_oversize_rejections: u64,
    passed: bool,
    failures: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
struct ContinuousPlaybackWindowGateReport {
    target_observations: usize,
    observed_observations: usize,
    missed_observations: usize,
    classified_readiness_observations: usize,
    required_duration_us: u64,
    wall_duration_us: u64,
    playback_evidence_duration_us: u64,
    start_frame: i64,
    terminal_frame: i64,
    planned_terminal_frame: i64,
    clock_advanced_frames: u64,
    reached_natural_end: bool,
    passed: bool,
    failures: Vec<&'static str>,
}

fn evaluate_continuous_playback_window(
    window: &ContinuousPlaybackWindowEvidence,
    readiness: &PreviewReadinessCounts,
) -> ContinuousPlaybackWindowGateReport {
    let classified_readiness_observations = readiness
        .ready
        .saturating_add(readiness.stale)
        .saturating_add(readiness.loading)
        .saturating_add(readiness.unavailable)
        .saturating_add(readiness.missed_deadline);
    let frame_displacement = window
        .terminal_frame
        .checked_sub(window.start_frame)
        .and_then(|value| u64::try_from(value).ok());
    let mut failures = Vec::new();
    if classified_readiness_observations > window.target_observations {
        failures.push("observation_count_exceeded_target");
    }
    if window.observed_observations.saturating_add(readiness.missed_deadline)
        != classified_readiness_observations
    {
        failures.push("readiness_opportunity_classification");
    }
    if window.wall_duration_us < window.required_duration_us {
        failures.push("wall_duration_below_window");
    }
    if window.playback.observed_duration_us < window.required_duration_us {
        failures.push("playback_evidence_duration_below_window");
    }
    if frame_displacement != Some(window.playback.clock_frame_advances.advanced_frames) {
        failures.push("clock_frame_displacement_mismatch");
    }
    if window.reached_natural_end && window.terminal_frame != window.planned_terminal_frame {
        failures.push("natural_end_outside_authored_boundary");
    }
    if classified_readiness_observations < window.target_observations {
        failures.push("observation_window_incomplete");
    }
    let same_epoch = window.playback.first_epoch.is_some()
        && window.playback.first_epoch == window.playback.latest_epoch;
    if !same_epoch {
        failures.push("playback_epoch_changed_within_window");
    }

    ContinuousPlaybackWindowGateReport {
        target_observations: window.target_observations,
        observed_observations: window.observed_observations,
        missed_observations: readiness.missed_deadline,
        classified_readiness_observations,
        required_duration_us: window.required_duration_us,
        wall_duration_us: window.wall_duration_us,
        playback_evidence_duration_us: window.playback.observed_duration_us,
        start_frame: window.start_frame,
        terminal_frame: window.terminal_frame,
        planned_terminal_frame: window.planned_terminal_frame,
        clock_advanced_frames: window.playback.clock_frame_advances.advanced_frames,
        reached_natural_end: window.reached_natural_end,
        passed: failures.is_empty(),
        failures,
    }
}

fn minimum_continuous_window_duration_us(
    advancing_intervals: usize,
    frame_interval_ns: u64,
) -> u64 {
    // The first observation can begin at any phase inside its current frame.
    // Crossing N frame boundaries therefore has a strict lower bound of
    // (N - 1) complete frame periods; the two exact frame coordinates and the
    // Engine displacement evidence prove the remaining boundary crossing.
    (advancing_intervals.saturating_sub(1) as u128)
        .saturating_mul(u128::from(frame_interval_ns))
        .checked_div(1_000)
        .unwrap_or(u128::MAX)
        .min(u128::from(u64::MAX)) as u64
}

fn startup_headroom_frames(ready_timeout: Duration, frame_interval_ns: u64) -> usize {
    if frame_interval_ns == 0 {
        return 0;
    }
    ready_timeout
        .as_nanos()
        .saturating_add(u128::from(frame_interval_ns).saturating_sub(1))
        .checked_div(u128::from(frame_interval_ns))
        .unwrap_or(u128::MAX)
        .min(usize::MAX as u128) as usize
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

struct FreshJsonlEvidenceWriter {
    path: PathBuf,
    file: std::fs::File,
}

impl FreshJsonlEvidenceWriter {
    fn create(path: PathBuf) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !path.as_os_str().is_empty(),
            "performance evidence output path must not be empty"
        );
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "create performance evidence output directory {}",
                    parent.display()
                )
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .with_context(|| {
                format!("open fresh performance evidence output {}", path.display())
            })?;
        Ok(Self { path, file })
    }

    fn write_json_line(&mut self, record_json: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !record_json.contains('\r') && !record_json.contains('\n'),
            "performance evidence record must occupy exactly one JSONL line"
        );
        self.file.write_all(record_json.as_bytes()).with_context(|| {
            format!(
                "write performance evidence record to {}",
                self.path.display()
            )
        })?;
        self.file.write_all(b"\n").with_context(|| {
            format!(
                "terminate performance evidence record in {}",
                self.path.display()
            )
        })?;
        self.file.flush().with_context(|| {
            format!(
                "flush performance evidence record to {}",
                self.path.display()
            )
        })
    }

    fn finish(mut self) -> anyhow::Result<()> {
        self.file.flush().with_context(|| {
            format!("flush performance evidence output {}", self.path.display())
        })?;
        self.file
            .sync_all()
            .with_context(|| format!("sync performance evidence output {}", self.path.display()))
    }
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

#[test]
fn fresh_jsonl_evidence_writer_replaces_stale_records_and_flushes() -> anyhow::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "mondrian-fresh-jsonl-writer-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let path = root.join("evidence.jsonl");
    fs::create_dir_all(&root)?;
    fs::write(&path, b"{\"stale\":true}\n")?;

    let mut writer = FreshJsonlEvidenceWriter::create(path.clone())?;
    writer.write_json_line("{\"run_id\":\"new-run\",\"ordinal\":1}")?;
    writer.finish()?;

    assert_eq!(
        fs::read_to_string(&path)?,
        "{\"run_id\":\"new-run\",\"ordinal\":1}\n"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn fresh_jsonl_evidence_writer_propagates_invalid_output_errors() -> anyhow::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "mondrian-fresh-jsonl-writer-error-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir_all(&root)?;

    let error = match FreshJsonlEvidenceWriter::create(root.clone()) {
        Ok(_) => anyhow::bail!("opening a directory as JSONL evidence unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("open fresh performance evidence output"));
    fs::remove_dir_all(root)?;
    Ok(())
}

fn preview_decode_hard_failures(report: &PreviewDecodePerformanceReport) -> Vec<&'static str> {
    let mut failures = Vec::new();
    if report.verdict != PreviewDecodePerformanceVerdict::Fail {
        return failures;
    }
    failures.extend(
        report
            .checks
            .iter()
            .filter(|check| {
                check.severity
                    == crate::app::preview_runtime::PreviewDecodePerformanceSeverity::Fail
            })
            .map(|check| check.code),
    );
    failures.extend(report.root_causes.iter().map(|root| root.code));
    failures.push("preview_decode_report_failed");
    failures.sort_unstable();
    failures.dedup();
    failures
}

fn preview_render_hard_failures(report: &PreviewRenderPerformanceReport) -> Vec<&'static str> {
    let mut failures = Vec::new();
    if report.verdict != PreviewRenderPerformanceVerdict::Fail {
        return failures;
    }
    failures.extend(
        report
            .checks
            .iter()
            .filter(|check| check.severity == PreviewRenderPerformanceSeverity::Fail)
            .map(|check| check.code),
    );
    failures.extend(
        report
            .root_causes
            .iter()
            .filter(|root| root.severity == PreviewRenderPerformanceSeverity::Fail)
            .map(|root| root.code),
    );
    failures.push("preview_render_report_failed");
    failures.sort_unstable();
    failures.dedup();
    failures
}

fn preview_decode_required_access_mode_failures(
    report: &PreviewDecodePerformanceReport,
) -> Vec<&'static str> {
    if report.summary.is_none() {
        return vec!["preview_decode_report_missing_summary"];
    };

    let mut failures = Vec::new();
    for check in &report.checks {
        if check.severity != crate::app::preview_runtime::PreviewDecodePerformanceSeverity::Fail {
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
    report: &PreviewDecodePerformanceReport,
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
    profiles: &PreviewDecodeAccessModeProfiles,
    access_mode: PreviewDecodeAccessMode,
) -> PreviewDecodeAccessModeProfile {
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

fn preview_playback_decode_failures(report: &PreviewDecodePerformanceReport) -> Vec<&'static str> {
    let mut failures = preview_decode_required_access_mode_failures(report);
    let mut scoped_fail_check = !failures.is_empty();
    for check in &report.checks {
        if check.severity != PreviewDecodePerformanceSeverity::Fail {
            continue;
        }
        let playback_scoped = check.code.starts_with("preview_decode_playback_")
            || matches!(
                check.code,
                "preview_decode_timeout_failures"
                    | "preview_decode_forward_budget_exhausted_failures"
                    | "preview_decode_cancellation_gate"
                    | "preview_decode_broker_clock_regressions"
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
    if let Some(summary) = report.summary.as_ref()
        && summary.current_queue_wait_max_us > summary.slow_frame_budget_us
    {
        // PlaybackCursor prefetch is intentionally allowed to remain queued
        // behind current work. Only Current queue latency can make the
        // realtime playback gate fail; aggregate access-mode queue latency
        // would incorrectly turn healthy lookahead residency into pressure.
        failures.push("preview_decode_playback_cursor_queue_wait_over_budget");
    }
    for root in &report.root_causes {
        match root.code {
            "preview_decode_playback_session_not_reused"
            | "preview_decode_playback_without_locality"
            | "preview_decode_playback_sustained_pressure" => failures.push(root.code),
            _ => {}
        }
    }
    if scoped_fail_check && report.verdict == PreviewDecodePerformanceVerdict::Fail {
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

        state.play()?;
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
        state.pause()?;

        let preview_service = WindowPreviewAdapter::new();
        let mut preview_diagnostics = preview_service.diagnostics();
        let preview_probe_case = run_case(
            "app_ui.preview_diagnostics_probe",
            1,
            preview_probe_threshold_ms,
            || {
                let _ = preview_service.viewer_preview_for_state(&state);
                let _ = preview_service
                    .gpu_preview_frame(state.preview_frame_execution_request(Instant::now()));
                preview_diagnostics = preview_service.diagnostics();
                Ok(())
            },
        )?;

        let preview_playback_service = WindowPreviewAdapter::new();
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
        state.pause()?;

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

    let failed_color_gates = app_ui_scale_color_gate_failures(
        &report.preview_color_report,
        &report.preview_playback_color_report,
    );
    if !failed_color_gates.is_empty() {
        anyhow::bail!(
            "app UI scale color-health gate failed: {:?}; report: {}",
            failed_color_gates,
            report_json
        );
    }

    anyhow::ensure!(
        report.preview_playback_diagnostics.unavailability.stages.timeline_evaluation == 0,
        "app UI scale success fixture produced TimelineEvaluation blockers; report: {}",
        report_json
    );
    let visual_cache = report.preview_playback_diagnostics.visual_program_cache;
    anyhow::ensure!(
        visual_cache.author_fingerprint_evaluations == 1
            && visual_cache.author_snapshot_binding_misses == 1
            && visual_cache.rejected_residency == 0,
        "app UI scale Preview did not retain one exact author-generation visual binding: {:?}; report: {}",
        visual_cache,
        report_json
    );

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

    let frame_count = env_usize_clamped("MONDRIAN_PREVIEW_MEDIA_FRAMES", 12, 3, 120);
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
    anyhow::ensure!(
        frame_count >= 3,
        "preview access-mode probe requires at least three frames to prove a cold Session followed by a real reused backward seek"
    );
    // Scrub and still-frame phases deliberately use disjoint timeline ranges.
    // Otherwise the Frame Store can satisfy the second phase and falsely claim
    // access-mode coverage without exercising its media Session.
    let sequence_frame_count = frame_count.saturating_mul(2).max(2);
    let mut state = match media_info {
        Some(media_info) => build_preview_media_perf_state_with_media_info(
            root_dir,
            video_path,
            Some(media_info),
            sequence_frame_count,
        )?,
        None => build_preview_media_perf_state(root_dir, video_path, sequence_frame_count)?,
    };
    let preview_service = HeadlessPreviewRuntime::new();
    let mut gpu_adapter =
        HeadlessViewerGpuAdapter::new().context("create real headless Viewer GPU Adapter")?;
    configure_headless_gpu_decode_admission(&preview_service, &mut gpu_adapter)?;
    let mut headless_gpu = HeadlessViewerGpuExecutionSummary {
        adapter: Some(gpu_adapter.adapter_info().clone()),
        ..HeadlessViewerGpuExecutionSummary::default()
    };
    ensure_preview_media_access_mode_deadline(overall_deadline, scenario, Some(&preview_service))?;

    let first_frame_case = run_case(
        "preview_media.first_frame_ready",
        1,
        first_frame_threshold_ms,
        || {
            state.seek(0)?;
            wait_for_headless_gpu_ready(
                &preview_service,
                &mut state,
                &mut gpu_adapter,
                &mut headless_gpu,
                ready_timeout,
            )
        },
    )?;

    let cached_frame_case = run_case(
        "preview_media.cached_frame_refresh",
        1,
        cache_threshold_ms,
        || {
            for _ in 0..cache_iterations {
                // A cache lookup may concurrently schedule fresher media work.
                // The retained frame is deliberately reported as Stale while
                // that replacement is pending, so keep it visible and wait for
                // the exact requested frame instead of treating Stale as a
                // render failure.
                ensure_preview_media_access_mode_deadline(
                    overall_deadline,
                    scenario,
                    Some(&preview_service),
                )?;
                wait_for_headless_gpu_ready(
                    &preview_service,
                    &mut state,
                    &mut gpu_adapter,
                    &mut headless_gpu,
                    ready_timeout,
                )?;
            }
            Ok(())
        },
    )?;

    let scrub_case = run_case(
        "preview_media.active_scrub_ready_window",
        1,
        scrub_threshold_ms,
        || {
            for sample in 0..frame_count {
                let frame = session_warm_then_backward_seek_frame(sample, frame_count);
                state.seek_with_source(frame as i64, TimelineSeekSource::PointerDrag)?;
                ensure_preview_media_access_mode_deadline(
                    overall_deadline,
                    scenario,
                    Some(&preview_service),
                )?;
                wait_for_headless_gpu_ready(
                    &preview_service,
                    &mut state,
                    &mut gpu_adapter,
                    &mut headless_gpu,
                    ready_timeout,
                )?;
            }
            state.seek(frame_count.saturating_sub(1) as i64)?;
            Ok(())
        },
    )?;

    let still_case = run_case(
        "preview_media.random_access_still_ready_window",
        1,
        sequential_threshold_ms,
        || {
            for sample in 0..frame_count {
                let frame = frame_count
                    .saturating_add(session_warm_then_backward_seek_frame(sample, frame_count));
                state.seek(frame as i64)?;
                ensure_preview_media_access_mode_deadline(
                    overall_deadline,
                    scenario,
                    Some(&preview_service),
                )?;
                wait_for_headless_gpu_ready(
                    &preview_service,
                    &mut state,
                    &mut gpu_adapter,
                    &mut headless_gpu,
                    ready_timeout,
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
            wait_for_headless_gpu_ready(
                &preview_service,
                &mut state,
                &mut gpu_adapter,
                &mut headless_gpu,
                ready_timeout,
            )
        },
    )?;
    settle_headless_preview_and_release_transport_media(
        &preview_service,
        &mut state,
        &mut gpu_adapter,
        &mut headless_gpu,
        ready_timeout,
    )?;
    let gpu_timings = gpu_adapter
        .finish_gpu_timings()
        .context("finish deferred headless Viewer GPU timestamp maps")?;
    headless_gpu.record_gpu_timings(&gpu_timings);
    headless_gpu.discarded_gpu_timestamp_frames = gpu_adapter.discarded_gpu_timings();

    let preview_diagnostics = preview_service.diagnostics();
    let media_color_issues = summarize_active_sequence_media_color_issues(&state)?;
    let preview_color_report =
        build_preview_color_health_report(preview_diagnostics.color_health_summary(), scenario);
    let preview_decode_report = build_preview_decode_performance_report_with_required_access_modes(
        preview_diagnostics.decode_performance_summary(PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US),
        scenario,
        PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
        &[
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ],
    );
    let preview_render_report = preview_diagnostics
        .render_performance_summary(PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US)
        .map(|summary| {
            build_preview_render_performance_report(
                Some(summary),
                scenario,
                PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
            )
        });
    let decode_failure_codes = preview_decode_hard_failures(&preview_decode_report);
    let render_failure_codes = preview_render_report
        .as_ref()
        .map(preview_render_hard_failures)
        .unwrap_or_default();
    Ok(PreviewMediaPerfReport {
        scenario,
        frames: frame_count,
        cache_iterations,
        headless_gpu,
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
            still_case,
            gpu_candidate_case,
        ],
    })
}

fn session_warm_then_backward_seek_frame(sample: usize, frame_count: usize) -> usize {
    match sample {
        0 => frame_count.saturating_sub(1),
        1 => 1,
        2 => 0,
        _ => sample.saturating_sub(1),
    }
}

#[test]
fn access_mode_probe_warms_then_forces_a_real_backward_seek() {
    assert_eq!(
        (0..6)
            .map(|sample| session_warm_then_backward_seek_frame(sample, 6))
            .collect::<Vec<_>>(),
        vec![5, 1, 0, 2, 3, 4]
    );
}

fn ensure_preview_media_access_mode_deadline(
    overall_deadline: Option<Instant>,
    scenario: &str,
    preview_service: Option<&HeadlessPreviewRuntime>,
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
    if report.preview_color_report.verdict == PreviewColorHealthVerdict::Fail {
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
    if report.headless_gpu.rendered_frames == 0
        || report.headless_gpu.gpu_completion_observed_frames == 0
    {
        anyhow::bail!(
            "preview media gate produced no completed production Headless GPU execution; report: {}",
            report_json
        );
    }
    if report.headless_gpu.stage_diagnostics.readback_stages != 0
        || report.headless_gpu.stage_diagnostics.gpu_blockers != 0
        || report.headless_gpu.fallback_count != 0
    {
        anyhow::bail!(
            "preview media Headless GPU execution used a blocker, fallback, or readback path; report: {}",
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

    let frame_count = env_usize_clamped("MONDRIAN_PREVIEW_EXTERNAL_MEDIA_FRAMES", 24, 3, 300);
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
        PreviewMediaPlaybackProbeConfig {
            scenario: "preview_media_continuous_playback",
            frame_count,
            sequence_frame_count: frame_count,
            frame_interval_ns,
            playback_threshold_ms,
            gpu_candidate_threshold_ms,
            decode_slow_frame_budget_us: PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
            ready_timeout,
            seek_probe_count: 0,
            seek_threshold_per_settled_ms: 1_000,
            resume_probe_frames: 0,
            resize_probe_frames: 0,
            video_layer_count: 1,
            probe_cancellation_recovery: false,
            native_video_gpu_timing: PreviewNativeVideoGpuTimingPolicy::Disabled,
            absolute_deadline: None,
            authored_output: PreviewMediaAuthoredOutput::SequenceDefault,
        },
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
    if report.preview_color_report.verdict == PreviewColorHealthVerdict::Fail {
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
    run_external_continuous_playback_gate(video_path, false, 1)
}

#[test]
#[ignore = "development Source-Full dual-video playback/composite smoke; run manually"]
fn preview_media_external_dual_video_playback_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let Some(video_path) = std::env::var_os("MONDRIAN_PREVIEW_EXTERNAL_DUAL_VIDEO_MEDIA_PATH")
        .or_else(|| std::env::var_os("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_MEDIA_PATH"))
        .map(std::path::PathBuf::from)
    else {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"preview_media_external_dual_video_playback\",\"skipped\":\"MONDRIAN_PREVIEW_EXTERNAL_DUAL_VIDEO_MEDIA_PATH not set\"}}"
        );
        return Ok(());
    };
    run_external_continuous_playback_gate(video_path, false, 2)
}

#[test]
#[ignore = "sealed realtime 4K60 Main10 dual-layer playback gate; requires generated media and GPU"]
fn preview_media_realtime_4k60_dual_video_gate() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let video_path = std::env::var_os("MONDRIAN_REALTIME_4K60_MEDIA_PATH")
        .map(std::path::PathBuf::from)
        .context(
            "MONDRIAN_REALTIME_4K60_MEDIA_PATH is required; the sealed realtime gate never skips",
        )?;
    run_external_continuous_playback_gate(video_path, false, 2)
}

#[test]
#[ignore = "development preview media resolution-scale decode stability smoke; run manually"]
fn preview_media_resolution_scale_decode_stability_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let ready_timeout = Duration::from_millis(env_u128(
        "MONDRIAN_PREVIEW_RESOLUTION_SCALE_READY_TIMEOUT_MS",
        30_000,
    ) as u64);
    let seek_frames_per_scale =
        env_usize_clamped("MONDRIAN_PREVIEW_RESOLUTION_SCALE_SEEKS", 8, 2, 24);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_preview_resolution_scale_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let video_path = root_dir.join("preview-resolution-scale-smoke.mp4");

    if !generate_preview_media_fixture_with_size(&video_path, 640, 360, 2)? {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"preview_media_resolution_scale_decode_stability\",\"skipped\":\"ffmpeg CLI unavailable or fixture generation failed\"}}"
        );
        let _ = fs::remove_dir_all(&root_dir);
        return Ok(());
    }

    let result = run_preview_media_resolution_scale_decode_stability_probe(
        &root_dir,
        &video_path,
        seek_frames_per_scale,
        ready_timeout,
    );
    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    anyhow::ensure!(
        report.passed,
        "preview media resolution-scale decode stability smoke failed: {report_json}"
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct PreviewMediaResolutionScalePhaseEvidence {
    resolution_scale: f32,
    output_width: u32,
    output_height: u32,
    presented_frames: usize,
    decode_decoded_frame_count: u64,
    decode_random_access_still_frames: u64,
    decode_seeked_frames: u64,
    decoded_frame_delta_from_warm: u64,
}

#[derive(Debug, Clone, Serialize)]
struct PreviewMediaResolutionScaleDecodeStabilityReport {
    scenario: &'static str,
    source_width: u32,
    source_height: u32,
    sequence_frame_count: usize,
    seek_frames_per_scale: usize,
    phases: Vec<PreviewMediaResolutionScalePhaseEvidence>,
    passed: bool,
}

/// Stage-2/3 regression probe for the DecodeRepresentation contract: with real
/// media, switching the authored preview resolution scale (output extent) must
/// only change the composition/render target. The decode identity and the
/// decoded-frame residency are source-representation state, so replaying the
/// same frames at 1.0 -> 0.5 -> 0.25 must not consume one more decoded frame.
fn run_preview_media_resolution_scale_decode_stability_probe(
    root_dir: &Path,
    video_path: &Path,
    seek_frames_per_scale: usize,
    ready_timeout: Duration,
) -> anyhow::Result<PreviewMediaResolutionScaleDecodeStabilityReport> {
    let media_info = probe_media_info(video_path)
        .with_context(|| format!("probe resolution-scale media {}", video_path.display()))?;
    let source_video = media_info
        .primary_video()
        .context("resolution-scale fixture has no primary video stream")?;
    let source_width = source_video.width;
    let source_height = source_video.height;
    let sequence_frame_count = source_video
        .total_frames
        .and_then(|frames| usize::try_from(frames).ok())
        .unwrap_or(60)
        .clamp(seek_frames_per_scale.saturating_add(2), 60);
    let mut state = build_preview_media_perf_state_with_media_info(
        root_dir,
        video_path,
        Some(media_info),
        sequence_frame_count,
    )?;
    let preview_service = HeadlessPreviewRuntime::new();
    let mut gpu_adapter =
        HeadlessViewerGpuAdapter::new_with_native_import_gpu_timing_policy_and_observation_capacity(
            PreviewNativeVideoGpuTimingPolicy::Disabled.renderer_policy(),
            0,
        )
        .context("create real headless Viewer GPU Adapter")?;
    configure_headless_gpu_decode_admission(&preview_service, &mut gpu_adapter)?;
    let mut gpu_summary = HeadlessViewerGpuExecutionSummary::default();
    let sequence_id = state
        .active_sequence_id()
        .context("resolution-scale probe has no active Sequence")?;

    let scales = [1.0_f32, 0.5, 0.25];
    let target_span = sequence_frame_count.saturating_sub(1);
    let mut phases = Vec::with_capacity(scales.len());
    let mut warm_decoded_frames = None;

    state.seek(0)?;
    for scale in scales {
        state.commit_sequence_edit(sequence_id, "修改预览分辨率", |sequence| {
            let mut settings = sequence.settings.clone();
            settings.preview.resolution_scale = scale;
            sequence.apply_settings(settings)
        })?;
        let output = crate::app::preview_quality::preview_execution_resolution(
            state
                .active_sequence()
                .context("resolution-scale probe lost its active Sequence")?
                .settings
                .resolution,
            scale,
            mondrian_playback::PreviewResolutionScale::Full,
        );
        for index in 0..seek_frames_per_scale {
            let target = index
                .saturating_add(1)
                .saturating_mul(target_span)
                .checked_div(seek_frames_per_scale.saturating_add(1))
                .unwrap_or(0);
            let source = if index % 2 == 0 {
                TimelineSeekSource::PointerDrag
            } else {
                TimelineSeekSource::Settled
            };
            state.seek_with_source(target as i64, source)?;
            wait_for_headless_gpu_ready(
                &preview_service,
                &mut state,
                &mut gpu_adapter,
                &mut gpu_summary,
                ready_timeout,
            )?;
        }
        wait_for_preview_work_quiescence(&preview_service, &mut state, ready_timeout)?;
        let diagnostics = preview_service.diagnostics();
        let decoded = diagnostics.decode_decoded_frame_count;
        let warm = *warm_decoded_frames.get_or_insert(decoded);
        phases.push(PreviewMediaResolutionScalePhaseEvidence {
            resolution_scale: scale,
            output_width: output.width,
            output_height: output.height,
            presented_frames: seek_frames_per_scale,
            decode_decoded_frame_count: decoded,
            decode_random_access_still_frames: diagnostics.decode_random_access_still_frames,
            decode_seeked_frames: diagnostics.decode_seeked_frames,
            decoded_frame_delta_from_warm: decoded.saturating_sub(warm),
        });
    }

    let warm_decoded =
        warm_decoded_frames.context("resolution-scale probe recorded no warm phase")?;
    let extents_changed = phases.windows(2).all(|pair| {
        (pair[0].output_width, pair[0].output_height)
            != (pair[1].output_width, pair[1].output_height)
    });
    let decode_stable = phases.iter().skip(1).all(|phase| phase.decoded_frame_delta_from_warm == 0);
    let passed = warm_decoded > 0 && extents_changed && decode_stable;
    Ok(PreviewMediaResolutionScaleDecodeStabilityReport {
        scenario: "preview_media_resolution_scale_decode_stability",
        source_width,
        source_height,
        sequence_frame_count,
        seek_frames_per_scale,
        phases,
        passed,
    })
}

#[test]
#[ignore = "manual bounded product-audio source smoke; requires real external media"]
fn audio_bounded_source_external_render_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let Some(media_path) =
        std::env::var_os("MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH").map(std::path::PathBuf::from)
    else {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"audio_bounded_source_external_render\",\"skipped\":\"MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH not set\"}}"
        );
        return Ok(());
    };
    let media_info = probe_media_info(&media_path)?;
    anyhow::ensure!(
        !media_info.audio_streams.is_empty(),
        "external bounded-audio smoke source has no audio stream"
    );
    anyhow::ensure!(
        media_info
            .primary_audio()
            .and_then(|stream| stream.duration)
            .is_some_and(|duration| duration.as_micros() >= 22_000_000),
        "external bounded-audio smoke requires at least 22 seconds of proven primary audio"
    );
    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_audio_source_smoke_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let result = (|| {
        let library = AssetLibrary::open(root_dir.join("library"))?;
        let asset_id = commit_perf_media_probe(&library, &media_path, media_info)?;
        let mut sequence = Sequence::new("Bounded audio source smoke");
        let duration = TimelineTime::new(22, 1)?;
        let track_id = sequence.audio_tracks[0].id;
        sequence.add_media_audio_clip(
            track_id,
            Clip::new(asset_id, TimelineTime::ZERO, duration)?,
            AudioSourceComponentId::primary(),
        )?;
        let cache = Arc::new(AudioSourceCache::new(48_000));
        let renderer = TimelineAudioPcmRenderer::new(
            sequence.clone(),
            vec![sequence],
            library,
            Arc::clone(&cache),
            AudioRuntimeResourceGrant::new(64, 768 * 1024 * 1024, 128 * 1024 * 1024),
            mondrian_audio::AudioAuditionOverlay::default(),
            48_000,
            AudioChannelLayout::Stereo,
        )?;
        let cancellation = mondrian_core::ExecutionCancellationToken::new();
        for (generation, start_sample) in
            [0, 24_000, 96_000, 480_000, 960_000].into_iter().enumerate()
        {
            let rendered = renderer.render(
                AudioPcmRenderRequest {
                    start_sample,
                    frame_count: 2_048,
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(
                        generation as u64 + 1,
                    )),
                },
                &cancellation,
            )?;
            anyhow::ensure!(
                rendered.frame_count() == 2_048,
                "audio render extent shifted"
            );
            anyhow::ensure!(
                rendered.samples.iter().all(|sample| sample.is_finite()),
                "audio render produced non-finite PCM"
            );
        }
        let diagnostics = cache.diagnostics();
        anyhow::ensure!(
            diagnostics.entries == 3,
            "unexpected source residency: {diagnostics:?}"
        );
        anyhow::ensure!(
            diagnostics.reserved_bytes <= diagnostics.byte_budget,
            "audio source cache exceeded byte budget: {diagnostics:?}"
        );
        anyhow::ensure!(diagnostics.decode_successes == 3, "{diagnostics:?}");
        anyhow::ensure!(diagnostics.hits >= 2, "{diagnostics:?}");
        anyhow::ensure!(diagnostics.decoder_session_opens == 1, "{diagnostics:?}");
        anyhow::ensure!(
            diagnostics.decoder_sequential_reuses == 2,
            "{diagnostics:?}"
        );
        anyhow::ensure!(
            diagnostics.decoder_random_seek_restarts == 0,
            "{diagnostics:?}"
        );
        eprintln!(
            "MONDRIAN_PERF_JSON={}",
            serde_json::json!({
                "scenario": "audio_bounded_source_external_render",
                "entries": diagnostics.entries,
                "reserved_bytes": diagnostics.reserved_bytes,
                "byte_budget": diagnostics.byte_budget,
                "hits": diagnostics.hits,
                "misses": diagnostics.misses,
                "decode_successes": diagnostics.decode_successes,
                "decode_max_duration_us": diagnostics.decode_max_duration_us,
                "decoder_sessions": diagnostics.decoder_sessions,
                "decoder_session_capacity": diagnostics.decoder_session_capacity,
                "decoder_session_opens": diagnostics.decoder_session_opens,
                "decoder_sequential_reuses": diagnostics.decoder_sequential_reuses,
                "decoder_random_seek_restarts": diagnostics.decoder_random_seek_restarts,
                "decoder_cold_window_max_duration_us": diagnostics.decoder_cold_window_max_duration_us,
                "decoder_sequential_window_max_duration_us": diagnostics.decoder_sequential_window_max_duration_us,
                "evictions": diagnostics.evictions,
                "passed": true,
            })
        );
        Ok(())
    })();
    let _ = fs::remove_dir_all(&root_dir);
    result
}

#[test]
#[ignore = "development production CPAL + headless Viewer smoke; requires real external audio and an output device"]
fn playback_cpal_av_external_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let Some(media_path) =
        std::env::var_os("MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH").map(PathBuf::from)
    else {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"playback_cpal_av_external\",\"skipped\":\"MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH not set\"}}"
        );
        return Ok(());
    };
    let media_info = probe_media_info(&media_path)?;
    let audio_stream_duration = media_info
        .primary_audio()
        .and_then(|audio| audio.duration)
        .context("CPAL smoke requires a proven primary-audio stream duration")?;
    let frame_count = env_usize_clamped("MONDRIAN_AUDIO_EXTERNAL_SMOKE_FRAMES", 8, 8, 3_600);
    let frame_interval_ns = 1_000_000_000u64
        .saturating_mul(1_001)
        .checked_div(30_000)
        .context("resolve 30000/1001 frame interval")?;
    let required_duration =
        Duration::from_nanos((frame_count as u64).saturating_mul(frame_interval_ns));
    anyhow::ensure!(
        audio_stream_duration >= required_duration,
        "CPAL smoke audio is too short: {audio_stream_duration:?} < {required_duration:?}"
    );
    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_cpal_av_smoke_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let result = (|| {
        let sequence_frame_count = audio_stream_duration
            .as_nanos()
            .checked_div(u128::from(frame_interval_ns))
            .and_then(|frames| usize::try_from(frames).ok())
            .unwrap_or(frame_count)
            .saturating_sub(1)
            .max(frame_count.saturating_add(2));
        let mut state = build_professional_cpal_av_state(
            &root_dir,
            &media_path,
            media_info,
            sequence_frame_count,
        )?;
        let mut realtime = HeadlessRealtimePlaybackSession::new()?;
        let mut gpu_summary = HeadlessViewerGpuExecutionSummary::default();
        state.seek(0)?;
        {
            let (preview_service, gpu_adapter) = realtime.bound_resources()?;
            wait_for_headless_gpu_ready(
                preview_service,
                &mut state,
                gpu_adapter,
                &mut gpu_summary,
                Duration::from_secs(30),
            )?;
        }
        state.play()?;
        let stream_generation = {
            let (preview_service, gpu_adapter) = realtime.bound_resources()?;
            wait_for_production_av_qualification(
                preview_service,
                &mut state,
                gpu_adapter,
                &mut gpu_summary,
                Duration::from_secs(30),
            )?
        };
        state.begin_playback_evidence_run(mondrian_playback::PlaybackEvidenceConfig::default())?;
        let observation_started = Instant::now();
        let process_memory_sampler = ProfessionalProcessMemorySampler::start(observation_started)?;
        let mut process_memory_evidence = PreviewProcessMemoryEvidenceCollector::default();
        let mut readiness = PreviewReadinessCounts::default();
        realtime.begin_realtime(&state, None)?;
        for _ in 0..frame_count {
            let sample = realtime.run_production_av_interval(
                &mut state,
                &mut gpu_summary,
                Duration::from_secs(30),
            )?;
            record_headless_preview_readiness(&mut readiness, sample);
        }
        process_memory_evidence.observe_playback_duration(
            observation_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        );
        for sample in process_memory_sampler.finish()? {
            process_memory_evidence.observe_playback(sample.observed_at_us, sample.sample);
        }
        process_memory_evidence
            .observe_post_stress(SystemPlatformService.product_process_tree_memory());
        let process_memory = process_memory_evidence.report();
        state.pump_audio_output()?;
        let audio = state.audio_playback_snapshot();
        let output = audio.output.context("CPAL smoke produced no output snapshot")?;
        anyhow::ensure!(
            audio.state == mondrian_media::AudioPlaybackState::Active,
            "{audio:?}"
        );
        anyhow::ensure!(
            output.stream_generation == stream_generation
                && output.active_callback_consumed_frames > 0
                && !output.stream_failed,
            "{output:?}"
        );
        anyhow::ensure!(
            state.playback_clock_master() == Some(mondrian_playback::ClockMaster::AudioDevice),
            "CPAL smoke did not retain Audio Device Clock Master"
        );
        anyhow::ensure!(
            gpu_summary.presented_unique_frame_completions > 0,
            "CPAL smoke completed no exact-current headless GPU Frame Demand"
        );
        anyhow::ensure!(
            gpu_summary.successor_preparation_ready > 0,
            "CPAL smoke established no exact immediate-successor preparation"
        );
        let evidence = state.playback_evidence_report();
        anyhow::ensure!(
            evidence.deliveries.rejected == 0,
            "CPAL smoke observed rejected terminal deliveries: {:?}",
            evidence.deliveries
        );
        state.pause()?;
        let coordinator_timing = realtime.finish_realtime()?;
        eprintln!(
            "MONDRIAN_PERF_JSON={}",
            serde_json::json!({
                "scenario": "playback_cpal_av_external",
                "stream_generation": stream_generation,
                "active_callback_consumed_frames": output.active_callback_consumed_frames,
                "callback_count": output.callback_count,
                "underrun_frames": output.underrun_frames,
                "gpu_output_observations": gpu_summary.published_output_observations,
                "gpu_presented_demand_completions": gpu_summary.presented_demand_completions,
                "gpu_presented_unique_frame_completions":
                    gpu_summary.presented_unique_frame_completions,
                "successor_preparation_attempts":
                    gpu_summary.successor_preparation_attempts,
                "successor_preparation_ready": gpu_summary.successor_preparation_ready,
                "prepared_successor_gpu_completions": gpu_summary.prepared_successor_frames,
                "delivery_phase_error": evidence.delivery_phase_error,
                "deliveries": evidence.deliveries,
                "video_readiness": readiness,
                "coordinator_timing": coordinator_timing,
                "process_memory": process_memory,
                "passed": true,
            })
        );
        Ok(())
    })();
    let _ = fs::remove_dir_all(&root_dir);
    result
}

#[test]
#[cfg(feature = "validation")]
#[ignore = "professional 30-minute production CPAL + headless Viewer A/V gate; requires long real audio and an output device"]
fn playback_professional_cpal_av_gate() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let media_path = std::env::var_os("MONDRIAN_PLAYBACK_PROFESSIONAL_AUDIO_MEDIA_PATH")
        .map(PathBuf::from)
        .context(
            "MONDRIAN_PLAYBACK_PROFESSIONAL_AUDIO_MEDIA_PATH is required; this gate never skips",
        )?;
    anyhow::ensure!(
        media_path.exists(),
        "professional audio media path does not exist: {}",
        media_path.display()
    );
    let media_info = probe_media_info(&media_path)
        .with_context(|| format!("probe professional audio media {}", media_path.display()))?;
    let media_probe = AudioPlaybackMediaProbeReport::from_media_info(&media_info)?;
    let frame_interval_ns = Rational::FPS_2997
        .den
        .saturating_mul(1_000_000_000)
        .checked_div(Rational::FPS_2997.num)
        .and_then(|value| u64::try_from(value).ok())
        .context("resolve 30000/1001 frame interval")?;
    let frame_count = professional_min_frame_count_for_interval(frame_interval_ns)?;
    const QUALIFICATION_GUARD_SECONDS: u64 = 30;
    let qualification_guard_frames = usize::try_from(
        30_000u64
            .saturating_mul(QUALIFICATION_GUARD_SECONDS)
            .saturating_add(1_000)
            .checked_div(1_001)
            .unwrap_or(u64::MAX),
    )
    .unwrap_or(usize::MAX);
    let sequence_frame_count =
        frame_count.saturating_add(qualification_guard_frames).saturating_add(2);
    let required_source_duration_us = (sequence_frame_count as u128)
        .saturating_mul(u128::from(frame_interval_ns))
        .saturating_add(999)
        .checked_div(1_000)
        .unwrap_or(u128::MAX)
        .min(u128::from(u64::MAX)) as u64;
    media_probe.ensure_observation_coverage(required_source_duration_us)?;
    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_cpal_av_gate_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let result = run_professional_cpal_av_probe(
        &root_dir,
        &media_path,
        media_info,
        media_probe,
        frame_count,
        sequence_frame_count,
        frame_interval_ns,
    );
    let _ = fs::remove_dir_all(&root_dir);
    result
}

#[cfg(feature = "validation")]
fn run_professional_cpal_av_probe(
    root_dir: &Path,
    media_path: &Path,
    media_info: MediaInfo,
    media_probe: AudioPlaybackMediaProbeReport,
    frame_count: usize,
    sequence_frame_count: usize,
    frame_interval_ns: u64,
) -> anyhow::Result<()> {
    let mut state =
        build_professional_cpal_av_state(root_dir, media_path, media_info, sequence_frame_count)?;
    let mut realtime = HeadlessRealtimePlaybackSession::new()?;
    let mut gpu_summary = HeadlessViewerGpuExecutionSummary {
        adapter: Some(realtime.gpu()?.adapter_info().clone()),
        ..HeadlessViewerGpuExecutionSummary::default()
    };
    let ready_timeout = Duration::from_secs(30);

    state.seek(0)?;
    {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        wait_for_headless_gpu_ready(
            preview_service,
            &mut state,
            gpu_adapter,
            &mut gpu_summary,
            ready_timeout,
        )?;
    }
    state.play()?;
    let initial_stream_generation = {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        wait_for_production_av_qualification(
            preview_service,
            &mut state,
            gpu_adapter,
            &mut gpu_summary,
            ready_timeout,
        )?
    };
    let initial_audio = state.audio_playback_snapshot();

    state.begin_playback_evidence_run(mondrian_playback::PlaybackEvidenceConfig::default())?;
    let mut process_memory_evidence = PreviewProcessMemoryEvidenceCollector::default();
    let process_memory_probe = SystemPlatformService;
    // Memory evidence covers the same complete production run as transport
    // evidence, including the controlled device-loss/recovery interval. Starting
    // after recovery made a valid 30-minute run appear about one second short.
    let observation_started = Instant::now();
    let process_memory_sampler = ProfessionalProcessMemorySampler::start(observation_started)?;
    let recovery_started = Instant::now();
    state.request_controlled_audio_output_recycle(initial_stream_generation)?;
    let recovery = {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        wait_for_production_av_recovery(
            preview_service,
            &mut state,
            gpu_adapter,
            &mut gpu_summary,
            initial_audio,
            recovery_started,
        )?
    };
    gpu_summary = HeadlessViewerGpuExecutionSummary {
        adapter: Some(realtime.gpu()?.adapter_info().clone()),
        ..HeadlessViewerGpuExecutionSummary::default()
    };
    let mut readiness = PreviewReadinessCounts::default();
    realtime.begin_realtime(&state, None)?;
    // Qualification proves a healthy starting point, while this bounded tail
    // guarantees that a short startup reactivation cannot shorten the required
    // uninterrupted callback interval. The evaluator still requires a complete
    // 30-minute active interval; the extension grants no missing evidence.
    const MAX_EVIDENCE_EXTENSION_SECONDS: u64 = 30;
    let max_frame_count = frame_count.saturating_add(
        usize::try_from(
            MAX_EVIDENCE_EXTENSION_SECONDS
                .saturating_mul(1_000_000_000)
                .div_ceil(frame_interval_ns),
        )
        .unwrap_or(usize::MAX),
    );
    let mut observed_frame_count = 0usize;

    for frame_index in 0..max_frame_count {
        let sample =
            realtime.run_production_av_interval(&mut state, &mut gpu_summary, ready_timeout)?;
        record_headless_preview_readiness(&mut readiness, sample);
        observed_frame_count = frame_index.saturating_add(1);
        anyhow::ensure!(
            state.is_playing(),
            "production A/V transport ended before the 30-minute observation completed at frame {frame_index}"
        );
        if observed_frame_count >= frame_count {
            let playback_duration_ready = state.playback_evidence_report().observed_duration_us
                >= PROFESSIONAL_MIN_OBSERVED_DURATION_US;
            let callback_duration_ready = state
                .audio_playback_snapshot()
                .output
                .and_then(|output| output.active_duration)
                .is_some_and(|duration| {
                    duration.as_micros() >= u128::from(PROFESSIONAL_MIN_OBSERVED_DURATION_US)
                });
            if playback_duration_ready && callback_duration_ready {
                break;
            }
        }
    }
    process_memory_evidence.observe_playback_duration(
        observation_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
    );
    for sample in process_memory_sampler.finish()? {
        process_memory_evidence.observe_playback(sample.observed_at_us, sample.sample);
    }
    state.pump_audio_output()?;
    let _ = realtime.pump_preview_completion(&mut state)?;
    let audio_snapshot = state.audio_playback_snapshot();
    let playback_evidence = state.playback_evidence_report();
    let source_cache = state.audio_source_cache_diagnostics();
    state.pause()?;
    let coordinator_timing = realtime.finish_realtime()?;
    {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        settle_headless_preview_and_release_transport_media(
            preview_service,
            &mut state,
            gpu_adapter,
            &mut gpu_summary,
            ready_timeout,
        )?;
    }
    let gpu_timings = realtime
        .gpu_mut()?
        .finish_gpu_timings()
        .context("finish deferred headless Viewer GPU timestamp maps")?;
    gpu_summary.record_gpu_timings(&gpu_timings);
    gpu_summary.discarded_gpu_timestamp_frames = realtime.gpu()?.discarded_gpu_timings();
    process_memory_evidence.observe_post_stress(process_memory_probe.product_process_tree_memory());
    let process_memory_evidence = process_memory_evidence.report();

    anyhow::ensure!(
        readiness
            .ready
            .saturating_add(readiness.loading)
            .saturating_add(readiness.stale)
            .saturating_add(readiness.unavailable)
            .saturating_add(readiness.missed_deadline)
            == observed_frame_count,
        "professional CPAL video-readiness accounting did not close: readiness={readiness:?}, observed={observed_frame_count}"
    );

    let report = evaluate_professional_audio_playback(ProfessionalAudioPlaybackObservation {
        media: &media_probe,
        recovery,
        audio: audio_snapshot,
        source_cache,
        playback_evidence: &playback_evidence,
        process_memory: &process_memory_evidence,
        video_readiness: ProfessionalVideoReadinessObservation {
            ready: readiness.ready as u64,
            loading: readiness.loading as u64,
            stale: readiness.stale as u64,
            unavailable: readiness.unavailable as u64,
            missed_deadline: readiness.missed_deadline as u64,
        },
        video_coordinator: coordinator_timing.professional_observation(),
        gpu_presented_frames: gpu_summary.presented_unique_frame_completions as u64,
    });
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);
    anyhow::ensure!(
        report.passed,
        "professional production CPAL A/V gate failed: {:?}; report: {report_json}",
        report.failures
    );
    Ok(())
}

fn wait_for_production_av_qualification(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<u64> {
    let deadline = Instant::now() + timeout;
    let work_watch = preview_service.work_watch();
    let mut candidate_binding = None;
    let mut candidate_status = HeadlessGpuCandidateStatus::Loading;
    let mut stable_qualification: Option<(u64, Instant)> = None;
    const QUALIFICATION_STABILITY: Duration = Duration::from_secs(1);
    loop {
        let drain_target_revision = work_watch.revision();
        state.pump_audio_output()?;
        let now = Instant::now();
        state.advance_playback_clock_at(now);
        let pump_outcome = apply_headless_preview_outcome(preview_service, state);
        let current_intent = HeadlessGpuCandidateIntent::from_state(state);
        if should_attempt_headless_gpu_candidate(
            candidate_status,
            candidate_binding,
            current_intent,
            pump_outcome,
        ) {
            let attempt = execute_headless_gpu_candidate(
                preview_service,
                state,
                gpu_adapter,
                gpu_summary,
                HeadlessGpuCompletionDeadline::at(deadline),
            )?;
            candidate_status = attempt.status;
            apply_headless_candidate_binding(
                &mut candidate_binding,
                current_intent,
                attempt.binding,
            );
        }
        let audio = state.audio_playback_snapshot();
        let qualified_output = (!state.is_playback_priming()
            && state.playback_clock_master() == Some(mondrian_playback::ClockMaster::AudioDevice)
            && audio.state == mondrian_media::AudioPlaybackState::Active)
            .then_some(audio.output)
            .flatten()
            .filter(|output| {
                output.active
                    && !output.stream_failed
                    && output.active_callback_consumed_frames > 0
                    && output.last_callback_age.is_some_and(|age| age <= Duration::from_millis(100))
            });
        if let Some(output) = qualified_output {
            match stable_qualification {
                Some((generation, qualified_at))
                    if generation == output.stream_generation
                        && now.saturating_duration_since(qualified_at)
                            >= QUALIFICATION_STABILITY =>
                {
                    return Ok(output.stream_generation);
                }
                Some((generation, _)) if generation == output.stream_generation => {}
                _ => stable_qualification = Some((output.stream_generation, now)),
            }
        } else {
            stable_qualification = None;
        }
        anyhow::ensure!(
            now < deadline,
            "timed out qualifying real CPAL callback consumption and headless video presentation; audio={audio:?}, clock={:?}, preview={:?}",
            state.playback_clock_master(),
            preview_service.diagnostics()
        );
        wait_for_headless_preview_revision(
            &work_watch,
            drain_target_revision,
            deadline,
            pump_outcome.needs_follow_up_poll && !candidate_status.requires_bounded_wait(),
        );
    }
}

#[cfg(feature = "validation")]
fn wait_for_production_av_recovery(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    initial_audio: AudioPlaybackSnapshot,
    requested_at: Instant,
) -> anyhow::Result<ProfessionalAudioRecoveryObservation> {
    const HANDOFF_LIMIT: Duration = Duration::from_secs(5);
    const STABILITY: Duration = Duration::from_secs(1);
    let deadline = requested_at
        .checked_add(HANDOFF_LIMIT + STABILITY)
        .context("derive controlled audio recovery deadline")?;
    let initial_output = initial_audio
        .output
        .context("controlled recycle requires a qualified initial CPAL output")?;
    let initial_lifecycle = initial_audio.output_lifecycle;
    let work_watch = preview_service.work_watch();
    let mut candidate_binding = None;
    let mut candidate_status = HeadlessGpuCandidateStatus::Loading;
    let mut request_to_loss_us = None;
    let mut request_to_synthetic_us = None;
    let mut request_to_reopen_us = None;
    let mut stable_recovery: Option<(u64, Instant)> = None;

    loop {
        let drain_target_revision = work_watch.revision();
        state.pump_audio_output()?;
        let now = Instant::now();
        let elapsed_us = now
            .saturating_duration_since(requested_at)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        state.advance_playback_clock_at(now);
        let pump_outcome = apply_headless_preview_outcome(preview_service, state);
        let current_intent = HeadlessGpuCandidateIntent::from_state(state);
        if should_attempt_headless_gpu_candidate(
            candidate_status,
            candidate_binding,
            current_intent,
            pump_outcome,
        ) {
            let attempt = execute_headless_gpu_candidate(
                preview_service,
                state,
                gpu_adapter,
                gpu_summary,
                HeadlessGpuCompletionDeadline::at(deadline),
            )?;
            candidate_status = attempt.status;
            apply_headless_candidate_binding(
                &mut candidate_binding,
                current_intent,
                attempt.binding,
            );
        }

        let audio = state.audio_playback_snapshot();
        let lifecycle = audio.output_lifecycle;
        if request_to_loss_us.is_none() && lifecycle.lost_count > initial_lifecycle.lost_count {
            request_to_loss_us = Some(elapsed_us);
        }
        if request_to_synthetic_us.is_none()
            && state.playback_clock_master() == Some(mondrian_playback::ClockMaster::Synthetic)
        {
            request_to_synthetic_us = Some(elapsed_us);
        }
        if request_to_reopen_us.is_none()
            && lifecycle.opened_count > initial_lifecycle.opened_count
            && lifecycle
                .last_opened_generation
                .is_some_and(|generation| generation > initial_output.stream_generation)
        {
            request_to_reopen_us = Some(elapsed_us);
        }
        let qualified_output = (!state.is_playback_priming()
            && state.playback_clock_master() == Some(mondrian_playback::ClockMaster::AudioDevice)
            && audio.state == mondrian_media::AudioPlaybackState::Active)
            .then_some(audio.output)
            .flatten()
            .filter(|output| {
                output.stream_generation > initial_output.stream_generation
                    && output.active
                    && !output.stream_failed
                    && output.active_callback_consumed_frames > 0
                    && output.last_callback_age.is_some_and(|age| age <= Duration::from_millis(100))
            });
        if let Some(output) = qualified_output {
            match stable_recovery {
                Some((generation, stable_at))
                    if generation == output.stream_generation
                        && now.saturating_duration_since(stable_at) >= STABILITY =>
                {
                    let request_to_recovered_us = stable_at
                        .saturating_duration_since(requested_at)
                        .as_micros()
                        .min(u128::from(u64::MAX))
                        as u64;
                    return Ok(ProfessionalAudioRecoveryObservation {
                        initial_audio,
                        initial_audio_device_stable_us: STABILITY.as_micros() as u64,
                        request_to_loss_us,
                        request_to_synthetic_us,
                        request_to_reopen_us,
                        request_to_recovered_us: Some(request_to_recovered_us),
                        final_audio_device_stable_us: STABILITY.as_micros() as u64,
                    });
                }
                Some((generation, _)) if generation == output.stream_generation => {}
                _ => stable_recovery = Some((output.stream_generation, now)),
            }
        } else {
            stable_recovery = None;
        }
        anyhow::ensure!(
            now < deadline,
            "controlled CPAL recovery did not reach one-second stable Audio Device Clock/Active residency; audio={audio:?}, clock={:?}",
            state.playback_clock_master()
        );
        wait_for_headless_preview_revision(
            &work_watch,
            drain_target_revision,
            deadline,
            pump_outcome.needs_follow_up_poll && !candidate_status.requires_bounded_wait(),
        );
    }
}

fn build_professional_cpal_av_state(
    root_dir: &Path,
    media_path: &Path,
    media_info: MediaInfo,
    frame_count: usize,
) -> anyhow::Result<AppState> {
    let library = AssetLibrary::open(root_dir.join("library"))?;
    let audio_asset_id = commit_perf_media_probe(&library, media_path, media_info)?;
    let solid_asset_id = library.create_solid_color_asset(Some("CPAL A/V gate picture"))?;
    let mut sequence = Sequence::new("Professional CPAL A/V gate");
    sequence.settings.frame_rate = Rational::FPS_2997;
    let time_base = sequence.time_base();
    let duration = tt(frame_count as i64, time_base);
    sequence.video_tracks[0].add_clip(Clip::new_solid_color(
        solid_asset_id,
        mondrian_core::Color::from_rgba8(18, 18, 18, 255),
        TimelineTime::ZERO,
        duration,
    )?)?;
    let audio_track_id = sequence.audio_tracks[0].id;
    sequence.add_media_audio_clip(
        audio_track_id,
        Clip::new(audio_asset_id, TimelineTime::ZERO, duration)?,
        AudioSourceComponentId::primary(),
    )?;
    sequence.playhead = TimelineTime::ZERO;
    sequence.mark_out(duration);
    let sequence_id = sequence.id;
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));
    state.test_set_active_sequence(sequence_id);
    state.test_set_default_sequence(sequence_id);
    state.test_set_sequences(vec![sequence.clone()]);
    state.test_set_sequence(Some(sequence));
    Ok(state)
}

#[test]
#[ignore = "professional 4K HEVC Main10 hardware playback gate; requires real media and GPU"]
fn preview_media_professional_4k_hevc_main10_hardware_playback_gate() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let video_path =
        std::env::var_os("MONDRIAN_PREVIEW_PROFESSIONAL_4K_HEVC_MAIN10_MEDIA_PATH")
            .map(std::path::PathBuf::from)
            .context(
                "MONDRIAN_PREVIEW_PROFESSIONAL_4K_HEVC_MAIN10_MEDIA_PATH is required; this gate never skips",
            )?;
    run_external_continuous_playback_gate(video_path, true, 1)
}

#[test]
#[ignore = "short production Main10 demux cancellation/recovery qualification; requires real media and GPU"]
fn preview_media_professional_4k_hevc_main10_isolated_demux_qualification_gate(
) -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let video_path =
        std::env::var_os("MONDRIAN_PREVIEW_PROFESSIONAL_4K_HEVC_MAIN10_MEDIA_PATH")
            .map(PathBuf::from)
            .context(
                "MONDRIAN_PREVIEW_PROFESSIONAL_4K_HEVC_MAIN10_MEDIA_PATH is required; this gate never skips",
            )?;
    run_external_isolated_demux_qualification_gate(video_path)
}

fn run_external_isolated_demux_qualification_gate(video_path: PathBuf) -> anyhow::Result<()> {
    anyhow::ensure!(
        video_path.exists(),
        "qualification media path does not exist: {}",
        video_path.display()
    );
    let media_info = probe_external_preview_media_info(&video_path)?;
    let media_probe = PreviewPlaybackMediaProbeReport::from_media_info(&media_info)?;
    let frame_interval_ns = media_probe.frame_interval_ns()?;
    let sequence_frame_count = professional_min_frame_count(&media_probe)?;
    media_probe.ensure_observation_coverage(sequence_frame_count, frame_interval_ns)?;
    const QUALIFICATION_PLAYBACK_FRAMES: usize = 25;
    const QUALIFICATION_SEEK_PROBES: usize = 4;
    let playback_threshold_ms = (QUALIFICATION_PLAYBACK_FRAMES as u128)
        .saturating_mul(u128::from(frame_interval_ns))
        .saturating_add(999_999)
        .saturating_div(1_000_000)
        .saturating_add(30_000);
    let ready_timeout = Duration::from_millis(PROFESSIONAL_READY_TIMEOUT_MS);
    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir =
        std::env::temp_dir().join(format!("mondrian_preview_main10_qualification_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let result = run_preview_media_continuous_playback_probe(
        &root_dir,
        &video_path,
        Some(media_info),
        PreviewMediaPlaybackProbeConfig {
            scenario: "preview_media_professional_4k_hevc_main10_isolated_demux_qualification",
            frame_count: QUALIFICATION_PLAYBACK_FRAMES,
            sequence_frame_count,
            frame_interval_ns,
            playback_threshold_ms,
            gpu_candidate_threshold_ms: PROFESSIONAL_GPU_CANDIDATE_LIMIT_MS,
            decode_slow_frame_budget_us: PROFESSIONAL_PLAYBACK_DECODE_P95_LIMIT_US,
            ready_timeout,
            seek_probe_count: QUALIFICATION_SEEK_PROBES,
            seek_threshold_per_settled_ms: 1_000,
            resume_probe_frames: 12,
            resize_probe_frames: 8,
            video_layer_count: 1,
            probe_cancellation_recovery: true,
            native_video_gpu_timing: PreviewNativeVideoGpuTimingPolicy::Disabled,
            absolute_deadline: None,
            authored_output: PreviewMediaAuthoredOutput::SourceFull,
        },
    );
    let _ = fs::remove_dir_all(&root_dir);
    let mut report = result?;
    let cancellation_recovery = report
        .cancellation_recovery_probe
        .context("short Main10 qualification omitted cancellation-recovery evidence")?;
    let runtime_evidence = professional_runtime_acceptance_evidence(
        &report.continuous_preview_diagnostics,
        &report.preview_diagnostics,
    );
    let qualification = evaluate_playback_qualification(PreviewPlaybackQualificationObservation {
        media: &report.media_probe,
        required_source_frames: sequence_frame_count,
        frame_interval_ns: report.frame_interval_ns,
        rendered_decode_execution: professional_presented_decode_evidence(
            report.headless_gpu.published_rendered_decode_execution,
        ),
        viewer_fallback_count: report.headless_gpu.fallback_count,
        viewer_fallback_reasons: &report.headless_gpu.fallback_reasons,
        playback_evidence: &report.playback_evidence,
        preview_diagnostics: &runtime_evidence,
        cancellation_recovery,
    });
    let qualification_passed = qualification.passed;
    report.qualification_media_gates = Some(qualification);
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);
    anyhow::ensure!(
        qualification_passed,
        "short Main10 production qualification failed; report: {report_json}"
    );
    Ok(())
}

#[test]
#[ignore = "accelerated native-surface endurance probe; requires real media and GPU"]
fn preview_media_external_accelerated_native_surface_endurance_probe() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let video_path = std::env::var_os("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_MEDIA_PATH")
        .map(PathBuf::from)
        .context("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_MEDIA_PATH is required")?;
    anyhow::ensure!(
        video_path.exists(),
        "external playback media path does not exist: {}",
        video_path.display()
    );
    let frame_count = env_usize_clamped(
        "MONDRIAN_PREVIEW_ACCELERATED_ENDURANCE_FRAMES",
        20_000,
        1,
        100_000,
    );
    let start_frame = env_usize_clamped(
        "MONDRIAN_PREVIEW_ACCELERATED_ENDURANCE_START_FRAME",
        0,
        0,
        1_000_000,
    );
    let seek_probe_count = env_usize_clamped(
        "MONDRIAN_PREVIEW_ACCELERATED_ENDURANCE_SEEK_PROBES",
        0,
        0,
        200,
    );
    let sequence_frame_count = start_frame.saturating_add(frame_count).saturating_add(2);
    let media_info = probe_external_preview_media_info(&video_path)?;
    let media_probe = PreviewPlaybackMediaProbeReport::from_media_info(&media_info)?;
    let frame_interval = Duration::from_nanos(media_probe.frame_interval_ns()?);
    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir =
        std::env::temp_dir().join(format!("mondrian_preview_accelerated_endurance_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let mut state = build_preview_media_perf_state_with_media_info(
        &root_dir,
        &video_path,
        Some(media_info),
        sequence_frame_count,
    )?;
    let preview_service = HeadlessPreviewRuntime::new();
    let decode_execution_journal = PreviewDecodeExecutionJournal::start_from_env(
        preview_service.decode_execution_watch(),
        "preview_media_external_accelerated_native_surface_endurance",
    )?;
    let mut gpu_adapter =
        HeadlessViewerGpuAdapter::new().context("create real headless Viewer GPU Adapter")?;
    configure_headless_gpu_decode_admission(&preview_service, &mut gpu_adapter)?;
    let mut gpu_summary = HeadlessViewerGpuExecutionSummary {
        adapter: Some(gpu_adapter.adapter_info().clone()),
        ..HeadlessViewerGpuExecutionSummary::default()
    };

    state.seek(start_frame as i64)?;
    wait_for_headless_gpu_ready(
        &preview_service,
        &mut state,
        &mut gpu_adapter,
        &mut gpu_summary,
        Duration::from_secs(30),
    )?;
    state.play()?;
    wait_for_headless_gpu_ready(
        &preview_service,
        &mut state,
        &mut gpu_adapter,
        &mut gpu_summary,
        Duration::from_secs(30),
    )?;
    wait_for_headless_playback_preroll(
        &preview_service,
        &mut state,
        &mut gpu_adapter,
        &mut gpu_summary,
        Duration::from_secs(30),
    )?;

    for _ in 0..frame_count {
        state.advance_playback_clock(frame_interval);
        wait_for_headless_gpu_ready(
            &preview_service,
            &mut state,
            &mut gpu_adapter,
            &mut gpu_summary,
            Duration::from_secs(30),
        )?;
    }

    state.pause()?;
    wait_for_headless_gpu_ready(
        &preview_service,
        &mut state,
        &mut gpu_adapter,
        &mut gpu_summary,
        Duration::from_secs(30),
    )?;
    if seek_probe_count > 0 {
        run_headless_cross_region_seeks(
            &preview_service,
            &mut state,
            &mut gpu_adapter,
            &mut gpu_summary,
            sequence_frame_count,
            seek_probe_count,
            Duration::from_secs(30),
        )?;
    }
    wait_for_preview_idle_residency_release(&preview_service, &mut state, Duration::from_secs(30))?;
    let diagnostics = preview_service.diagnostics();
    anyhow::ensure!(
        diagnostics.worker_queue.in_flight_jobs == 0
            && diagnostics.frame_store.media_aggregate_resource_units == 0
            && gpu_summary.native_import_retained_sources_peak == 0,
        "accelerated endurance left native resources resident: {diagnostics:?}"
    );
    println!(
        "MONDRIAN_PERF_JSON={}",
        serde_json::json!({
            "scenario": "preview_media_external_accelerated_native_surface_endurance",
            "start_frame": start_frame,
            "frames": frame_count,
            "seek_probes": seek_probe_count,
            "decode_successes": diagnostics.decode_successes,
            "playback_decode_frames": diagnostics.decode_playback_cursor_frames,
            "external_frames_registered": diagnostics.gpu_preview_external_frames_registered,
            "media_cache_evictions": diagnostics.frame_store.media_evictions,
            "native_import_retained_sources_peak": gpu_summary.native_import_retained_sources_peak,
        })
    );
    if let Some(journal) = decode_execution_journal {
        journal.finish()?;
    }
    drop(preview_service);
    drop(state);
    let _ = fs::remove_dir_all(&root_dir);
    Ok(())
}

fn run_external_continuous_playback_gate(
    video_path: PathBuf,
    professional: bool,
    video_layer_count: u32,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(video_layer_count, 1 | 2),
        "external playback gate supports one or two video layers"
    );
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
    let probed_frame_interval_ns = media_probe.frame_interval_ns()?;
    let frame_interval_ns = if professional {
        probed_frame_interval_ns
    } else {
        std::env::var("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_FRAME_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .map(|milliseconds| milliseconds.saturating_mul(1_000_000))
            .unwrap_or(probed_frame_interval_ns)
    };
    if professional {
        media_probe.ensure_observation_coverage(frame_count, frame_interval_ns)?;
    }
    let default_playback_threshold_ms = if professional {
        professional_playback_case_budget_ms(frame_count, frame_interval_ns)
    } else {
        (frame_count as u128)
            .saturating_mul(u128::from(frame_interval_ns))
            .saturating_add(999_999)
            .saturating_div(1_000_000)
            .saturating_add(5_000)
    };
    let playback_threshold_ms = if professional {
        default_playback_threshold_ms
    } else {
        env_u128(
            "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_WINDOW_MS",
            default_playback_threshold_ms,
        )
    };
    let gpu_candidate_threshold_ms = if professional {
        PROFESSIONAL_GPU_CANDIDATE_LIMIT_MS
    } else {
        env_u128("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_GPU_CANDIDATE_MS", 2_000)
    };
    let ready_timeout = Duration::from_millis(if professional {
        PROFESSIONAL_READY_TIMEOUT_MS
    } else {
        env_u128(
            "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_READY_TIMEOUT_MS",
            30_000,
        ) as u64
    });
    let default_overall_timeout_ms = if professional {
        u128::from(PROFESSIONAL_TOTAL_TIMEOUT_MS)
    } else {
        180_000
    };
    let overall_timeout = Duration::from_millis(if professional {
        PROFESSIONAL_TOTAL_TIMEOUT_MS
    } else {
        env_u128(
            "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_TOTAL_TIMEOUT_MS",
            default_overall_timeout_ms,
        ) as u64
    });
    let playback_p95_limit_us = if professional {
        PROFESSIONAL_PLAYBACK_DECODE_P95_LIMIT_US
    } else {
        env_u64("MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_P95_US", 60_000)
    };
    let playback_queue_wait_p95_limit_us = if professional {
        PROFESSIONAL_PLAYBACK_QUEUE_WAIT_P95_LIMIT_US
    } else {
        env_u64(
            "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_QUEUE_WAIT_P95_US",
            10_000,
        )
    };
    let min_visible_percent = if professional {
        PROFESSIONAL_MIN_VISIBLE_PERCENT
    } else {
        env_usize_clamped(
            "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_VISIBLE_PERCENT",
            95,
            1,
            100,
        )
    };
    let min_ready_basis_points = if professional {
        PROFESSIONAL_MIN_READY_BASIS_POINTS
    } else {
        env_usize_clamped(
            "MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_READY_BASIS_POINTS",
            9_950,
            1,
            10_000,
        )
    };

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_preview_external_playback_{uniq}"));
    fs::create_dir_all(&root_dir)?;

    let deadline = Instant::now() + overall_timeout;
    let scenario = if professional {
        "preview_media_professional_4k_hevc_main10_hardware_playback"
    } else if video_layer_count == 2 {
        "preview_media_external_dual_video_playback"
    } else {
        "preview_media_external_continuous_playback"
    };
    let seek_probe_count = if professional {
        PROFESSIONAL_MIN_WARM_SEEKS.saturating_add(PROFESSIONAL_MIN_ACCURATE_SEEKS) as usize
    } else if video_layer_count == 2 {
        std::env::var("MONDRIAN_PREVIEW_EXTERNAL_SEEK_PROBES")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(8)
            .min(200)
    } else {
        std::env::var("MONDRIAN_PREVIEW_EXTERNAL_SEEK_PROBES")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default()
            .min(200)
    };
    let seek_threshold_per_settled_ms = if professional {
        1_000
    } else {
        env_u128("MONDRIAN_PREVIEW_EXTERNAL_SEEK_MS", 3_000)
    };
    let source_frame_count = media_info
        .primary_video()
        .and_then(|video| video.total_frames)
        .and_then(|frames| usize::try_from(frames).ok())
        .map(|frames| frames.saturating_sub((video_layer_count - 1) as usize));
    let resume_probe_frames = if seek_probe_count == 0 {
        0
    } else if professional {
        12
    } else {
        env_usize_clamped("MONDRIAN_PREVIEW_EXTERNAL_RESUME_FRAMES", 12, 4, 60)
    };
    let default_sequence_frame_count = frame_count
        .saturating_add(startup_headroom_frames(ready_timeout, frame_interval_ns))
        .min(source_frame_count.unwrap_or(usize::MAX));
    let sequence_frame_count = if professional {
        default_sequence_frame_count
    } else {
        env_usize_clamped(
            "MONDRIAN_PREVIEW_EXTERNAL_TIMELINE_FRAMES",
            default_sequence_frame_count,
            frame_count,
            200_000,
        )
    };
    let result = run_preview_media_continuous_playback_probe(
        &root_dir,
        &video_path,
        Some(media_info),
        PreviewMediaPlaybackProbeConfig {
            scenario,
            frame_count,
            sequence_frame_count,
            frame_interval_ns,
            playback_threshold_ms,
            gpu_candidate_threshold_ms,
            decode_slow_frame_budget_us: playback_p95_limit_us,
            ready_timeout,
            seek_probe_count,
            seek_threshold_per_settled_ms,
            resume_probe_frames,
            resize_probe_frames: if seek_probe_count == 0 { 0 } else { 8 },
            video_layer_count,
            probe_cancellation_recovery: professional,
            native_video_gpu_timing: if professional {
                PreviewNativeVideoGpuTimingPolicy::Strict {
                    observation_capacity:
                        professional_native_video_gpu_timing_observation_capacity(
                            frame_count,
                            seek_probe_count,
                        )?,
                }
            } else {
                PreviewNativeVideoGpuTimingPolicy::Disabled
            },
            absolute_deadline: Some(deadline),
            authored_output: PreviewMediaAuthoredOutput::SourceFull,
        },
    );
    let _ = fs::remove_dir_all(&root_dir);

    anyhow::ensure!(
        Instant::now() <= deadline,
        "external playback smoke exceeded total timeout {:?}",
        overall_timeout
    );
    let mut report = result?;
    let mut real_media_gates = evaluate_external_playback_gates(
        &report.readiness,
        &report.headless_gpu,
        report.frames,
        report.frame_interval_ns.saturating_add(999) / 1_000,
        &report.preview_decode_report,
        &report.continuous_preview_diagnostics,
        &report.continuous_playback_window.playback,
        playback_p95_limit_us,
        playback_queue_wait_p95_limit_us,
        min_visible_percent,
        min_ready_basis_points,
    );
    let continuous_window =
        evaluate_continuous_playback_window(&report.continuous_playback_window, &report.readiness);
    real_media_gates.failures.extend(continuous_window.failures.iter().copied());
    real_media_gates.passed &= continuous_window.passed;
    real_media_gates.continuous_window = Some(continuous_window);
    report.real_media_gates = Some(real_media_gates);
    if professional {
        let playback_decode = report
            .preview_decode_report
            .summary
            .as_ref()
            .map(|summary| summary.access_mode_profiles.playback_cursor)
            .unwrap_or_default();
        let runtime_evidence = professional_runtime_acceptance_evidence(
            &report.continuous_preview_diagnostics,
            &report.preview_diagnostics,
        );
        report.professional_media_gates = Some(evaluate_professional_playback(
            ProfessionalPlaybackObservation {
                media: &report.media_probe,
                rendered_decode_execution: professional_presented_decode_evidence(
                    report.headless_gpu.published_rendered_decode_execution,
                ),
                viewer_fallback_count: report.headless_gpu.fallback_count,
                viewer_fallback_reasons: &report.headless_gpu.fallback_reasons,
                playback_decode: professional_playback_decode_evidence(playback_decode),
                playback_evidence: &report.playback_evidence,
                continuous_playback_evidence: &report.continuous_playback_window.playback,
                continuous_playback_wall_duration_us: report
                    .continuous_playback_window
                    .wall_duration_us,
                preview_diagnostics: &runtime_evidence,
                process_memory: &report.process_memory_evidence,
                native_video_gpu_timing: &report.native_video_gpu_timing.evidence,
                frames: report.frames,
                frame_interval_ns: report.frame_interval_ns,
            },
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
    if report.preview_color_report.verdict == PreviewColorHealthVerdict::Fail {
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
    if let Some(gates) = &report.real_media_gates
        && !gates.passed
    {
        anyhow::bail!(
            "preview media external continuous playback real-media gates failed: {:?}; report: {}",
            gates.failures,
            report_json
        );
    }
    if let Some(gates) = &report.professional_media_gates
        && !gates.passed
    {
        anyhow::bail!(
            "professional 4K HEVC Main10 hardware playback gates failed: {:?}; report: {}",
            gates.failures,
            report_json
        );
    }

    Ok(())
}

fn evaluate_external_playback_gates(
    readiness: &PreviewReadinessCounts,
    headless_gpu: &HeadlessViewerGpuExecutionSummary,
    frames: usize,
    gpu_execution_p95_limit_us: u64,
    decode_report: &PreviewDecodePerformanceReport,
    preview_diagnostics: &PreviewDiagnostics,
    playback_evidence: &PlaybackEvidenceReport,
    playback_decode_p95_limit_us: u64,
    playback_current_queue_wait_limit_us: u64,
    min_visible_percent: usize,
    min_ready_basis_points: usize,
) -> PreviewExternalPlaybackGateReport {
    let playback_decode_p95_observed = decode_check_observed(
        decode_report,
        "preview_decode_playback_cursor_forward_steady_p95_worker_execution_us",
    );
    let playback_queue_wait_check = decode_check_observed(
        decode_report,
        "preview_decode_playback_cursor_queue_wait_p95_us",
    );
    let playback_decode_p95_observed_us = playback_decode_p95_observed.unwrap_or_default();
    let playback_current_queue_wait_observed_us =
        preview_diagnostics.decode_current_queue_wait_max_us;
    let visible_frames = readiness.ready.saturating_add(readiness.stale);
    let min_visible_frames = frames.saturating_mul(min_visible_percent).saturating_add(99) / 100;
    let min_ready_frames =
        frames.saturating_mul(min_ready_basis_points).saturating_add(9_999) / 10_000;
    let ready_frames = if playback_evidence.demand_count > 0 {
        // The Viewer publication is the final authority for whether an exact
        // current frame became visible. A Frame Demand may be superseded at a
        // clock boundary after its GPU output was already published, so the
        // Engine delivery ledger alone can under-count exact presentation.
        // Keep both independently gated below and use their union's lower-cost
        // cardinality here: both ledgers de-duplicate by epoch/frame.
        usize::try_from(playback_evidence.deliveries.ready)
            .unwrap_or(usize::MAX)
            .max(headless_gpu.presented_unique_frame_completions)
    } else {
        // Deterministic unit fixtures without an Engine event stream retain
        // the direct readiness observation as their evidence source.
        readiness.ready
    };
    let ready_basis_points = ready_frames.saturating_mul(10_000) / frames.max(1);
    let playback_decode_profile = preview_diagnostics.decode_access_mode_profiles.playback_cursor;
    let playback_temporal_approximation_frames =
        playback_decode_profile.temporal_approximation_frames;
    let playback_temporal_mismatch_failures = playback_decode_profile.temporal_mismatch_failures;
    let max_clock_skipped_intermediate_frames = (frames / 1_000) as u64;
    let clock_skipped_intermediate_frames =
        playback_evidence.clock_frame_advances.skipped_intermediate_frames;
    let mut failures = Vec::new();
    if playback_decode_p95_observed
        .is_none_or(|observed| observed == 0 || observed > playback_decode_p95_limit_us)
    {
        failures.push("playback_decode_p95");
    }
    if playback_queue_wait_check.is_none()
        || playback_current_queue_wait_observed_us > playback_current_queue_wait_limit_us
    {
        failures.push("playback_current_queue_wait");
    }
    if visible_frames < min_visible_frames {
        failures.push("visible_frame_ratio");
    }
    if ready_frames < min_ready_frames {
        failures.push("current_ready_ratio");
    }
    if headless_gpu.rendered_frames == 0 {
        failures.push("viewer_gpu_execution_missing");
    }
    let classified_rendered_frames = headless_gpu
        .published_rendered_frames
        .saturating_add(headless_gpu.prepared_successor_frames)
        .saturating_add(headless_gpu.released_rendered_frames)
        .saturating_add(headless_gpu.terminal_rejected_rendered_frames);
    if classified_rendered_frames != headless_gpu.rendered_frames {
        failures.push("viewer_gpu_execution_classification");
    }
    if headless_gpu.presented_unique_frame_completions < min_ready_frames {
        failures.push("viewer_gpu_publication_coverage");
    }
    if headless_gpu.gpu_completion_observed_frames != headless_gpu.rendered_frames {
        failures.push("viewer_gpu_completion_coverage");
    }
    if headless_gpu.stage_diagnostics.readback_stages > 0 {
        failures.push("viewer_gpu_readback");
    }
    if headless_gpu.stage_diagnostics.gpu_blockers > 0 {
        failures.push("viewer_gpu_blockers");
    }
    if headless_gpu.missing_gpu_timestamp_frames > 0
        || headless_gpu.unmatched_gpu_timestamp_samples > 0
        || headless_gpu.duplicate_gpu_timestamp_samples > 0
        || headless_gpu.duplicate_gpu_timestamp_ownership > 0
        || headless_gpu.duplicate_expected_gpu_timestamp_tokens > 0
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
    let delivery_phase_error_p95_limit_us = 20_000;
    let delivery_phase = playback_evidence.delivery_phase_error;
    if delivery_phase.audio_device.proven_error.p95_us > delivery_phase_error_p95_limit_us
        || delivery_phase.synthetic.proven_error.p95_us > delivery_phase_error_p95_limit_us
        || delivery_phase.unproven_presentable > 0
    {
        failures.push("delivery_phase_error");
    }
    if playback_evidence.audio_underrun_recoveries > 0 {
        failures.push("audio_underrun_recovery");
    }
    if playback_temporal_approximation_frames > 0 {
        failures.push("playback_temporal_approximation");
    }
    if playback_temporal_mismatch_failures > 0 {
        failures.push("playback_temporal_mismatch");
    }
    if clock_skipped_intermediate_frames > max_clock_skipped_intermediate_frames {
        failures.push("clock_skipped_intermediate_frames");
    }
    let frame_store = preview_diagnostics.frame_store;
    let cpu_frame_store_within_budget = frame_store.optional_media_cache_within_policy()
        && frame_store.media_aggregate_within_hard_grant()
        && frame_store.media_high_water_within_hard_grant()
        && frame_store.viewer_residency_within_policy();
    if !cpu_frame_store_within_budget {
        failures.push("cpu_frame_store_budget");
    }
    let decoder_resource_store_within_budget = frame_store.media_resource_units
        <= frame_store.media_resource_unit_budget
        && frame_store.media_aggregate_resource_units
            <= frame_store.media_aggregate_hard_resource_unit_limit
        && frame_store.media_aggregate_resource_unit_high_water
            <= frame_store.media_aggregate_hard_resource_unit_limit
        && frame_store.media_current_working_set_resource_unit_high_water
            <= frame_store.current_media_working_set_resource_unit_limit
        && !frame_store.media_capacity_overcommitted
        && !frame_store.media_current_working_set_overcommitted
        && frame_store.media_capacity_overcommit_events == 0
        && frame_store.media_current_working_set_overcommit_events == 0;
    if !decoder_resource_store_within_budget {
        failures.push("decoder_resource_store_budget");
    }
    let cpu_frame_store_oversize_rejections = frame_store.oversize_rejections();
    if cpu_frame_store_oversize_rejections > 0 {
        failures.push("cpu_frame_store_oversize_rejection");
    }

    PreviewExternalPlaybackGateReport {
        enabled: true,
        continuous_window: None,
        playback_decode_p95_limit_us,
        playback_decode_p95_observed_us,
        playback_current_queue_wait_limit_us,
        playback_current_queue_wait_observed_us,
        min_visible_frames,
        visible_frames,
        min_ready_frames,
        ready_frames,
        min_ready_basis_points,
        ready_basis_points,
        gpu_rendered_frames: headless_gpu.rendered_frames,
        gpu_completion_observed_frames: headless_gpu.gpu_completion_observed_frames,
        gpu_published_rendered_frames: headless_gpu.published_rendered_frames,
        gpu_prepared_successor_frames: headless_gpu.prepared_successor_frames,
        gpu_released_rendered_frames: headless_gpu.released_rendered_frames,
        gpu_terminal_rejected_rendered_frames: headless_gpu.terminal_rejected_rendered_frames,
        gpu_late_rejected_rendered_frames: headless_gpu.late_rejected_rendered_frames,
        gpu_published_cached_output_observations: headless_gpu.published_cached_output_observations,
        gpu_released_cached_frames: headless_gpu.released_cached_frames,
        gpu_terminal_rejected_cached_frames: headless_gpu.terminal_rejected_cached_frames,
        gpu_late_rejected_cached_frames: headless_gpu.late_rejected_cached_frames,
        gpu_published_output_observations: headless_gpu.published_output_observations,
        gpu_presented_demand_completions: headless_gpu.presented_demand_completions,
        gpu_presented_unique_frame_completions: headless_gpu.presented_unique_frame_completions,
        gpu_timestamped_frames: headless_gpu.gpu_duration_samples_us.len(),
        gpu_missing_timestamp_frames: headless_gpu.missing_gpu_timestamp_frames,
        gpu_discarded_timestamp_frames: headless_gpu.discarded_gpu_timestamp_frames,
        gpu_execution_p95_limit_us,
        gpu_execution_p95_observed_us,
        gpu_stage_p95_us: headless_gpu.p95_gpu_stages(),
        gpu_record_submit_p95_us: headless_gpu.p95_record_submit_us(),
        gpu_completion_wait_p95_us: headless_gpu.p95_completion_wait_us(),
        gpu_wall_duration_p95_us: headless_gpu.p95_wall_duration_us(),
        gpu_cpu_stage_p95_us: headless_gpu.p95_cpu_stages(),
        gpu_readback_stages: headless_gpu.stage_diagnostics.readback_stages,
        gpu_blockers: headless_gpu.stage_diagnostics.gpu_blockers,
        gpu_fallback_count: headless_gpu.fallback_count,
        delivery_phase_error_p95_limit_us,
        audio_device_delivery_phase: delivery_phase.audio_device,
        synthetic_delivery_phase: delivery_phase.synthetic,
        unproven_presentable_deliveries: delivery_phase.unproven_presentable,
        phase_not_applicable_deliveries: delivery_phase.phase_not_applicable,
        audio_underrun_recoveries: playback_evidence.audio_underrun_recoveries,
        playback_temporal_approximation_frames,
        playback_temporal_mismatch_failures,
        clock_advanced_frames: playback_evidence.clock_frame_advances.advanced_frames,
        max_clock_skipped_intermediate_frames,
        clock_skipped_intermediate_frames,
        evicted_playback_evidence_events: playback_evidence.evicted_event_count,
        cpu_frame_store_within_budget,
        decoder_resource_store_within_budget,
        cpu_frame_store_oversize_rejections,
        passed: failures.is_empty(),
        failures,
    }
}

fn professional_presented_decode_evidence(
    summary: PreviewDecodeExecutionSummary,
) -> PresentedDecodeExecutionEvidence {
    PresentedDecodeExecutionEvidence {
        media_layers: summary.media_layers,
        software_cpu_layers: summary.software_cpu_layers,
        hardware_cpu_transfer_layers: summary.hardware_cpu_transfer_layers,
        hardware_native_layers: summary.hardware_native_layers,
        p010_10_bit_hardware_layers: summary.p010_10_bit_hardware_layers,
    }
}

fn professional_playback_decode_evidence(
    profile: PreviewDecodeAccessModeProfile,
) -> PlaybackDecodeExecutionEvidence {
    PlaybackDecodeExecutionEvidence {
        hardware_decode_prefer_hardware_requested_frames: profile
            .hardware_decode_prefer_hardware_requested_frames,
        hardware_decode_prefer_gpu_requested_frames: profile
            .hardware_decode_prefer_gpu_requested_frames,
        hardware_decode_require_gpu_requested_frames: profile
            .hardware_decode_require_gpu_requested_frames,
        hardware_decode_cpu_not_requested_frames: profile.hardware_decode_cpu_not_requested_frames,
        hardware_decode_cpu_unavailable_frames: profile.hardware_decode_cpu_unavailable_frames,
        hardware_decode_backend_unavailable_frames: profile
            .hardware_decode_backend_unavailable_frames,
        hardware_decode_codec_unsupported_frames: profile.hardware_decode_codec_unsupported_frames,
        hardware_decode_device_context_unavailable_frames: profile
            .hardware_decode_device_context_unavailable_frames,
        hardware_decode_cpu_transfer_setup_failed_frames: profile
            .hardware_decode_cpu_transfer_setup_failed_frames,
        hardware_decode_cpu_transfer_decoder_open_failed_frames: profile
            .hardware_decode_cpu_transfer_decoder_open_failed_frames,
        hardware_decode_cpu_transfer_awaiting_frame_frames: profile
            .hardware_decode_cpu_transfer_awaiting_frame_frames,
        hardware_decode_backend_boundary_frames: profile.hardware_decode_backend_boundary_frames,
        hardware_decode_adapter_unavailable_frames: profile
            .hardware_decode_adapter_unavailable_frames,
    }
}

fn professional_runtime_acceptance_evidence(
    continuous: &PreviewDiagnostics,
    post_window: &PreviewDiagnostics,
) -> PreviewRuntimeAcceptanceEvidence {
    PreviewRuntimeAcceptanceEvidence {
        resource_policy_applications: continuous.resource_decision_applications,
        // All counters and high-water marks are cumulative for the Runtime
        // lifetime, while ownership and helper residency are instantaneous.
        // The final settled snapshot therefore preserves the continuous-window
        // evidence and is the only valid source for post-stress lifecycle
        // qualification.
        scheduler: post_window.scheduler,
        worker_queue: post_window.worker_queue,
        frame_store: post_window.frame_store,
        accurate_seek_temporal_approximation_frames: post_window
            .decode_access_mode_profiles
            .random_access_still
            .temporal_approximation_frames,
        decode_cancellation: post_window.decode_cancellation,
        decode_cancellation_checkpoints: post_window.decode_cancellation_checkpoints,
        decode_worker_execution: post_window.decode_worker_execution,
    }
}

#[test]
fn professional_runtime_evidence_attributes_resource_cadence_to_continuous_window() {
    let continuous = PreviewDiagnostics {
        resource_decision_applications: 45_000,
        ..PreviewDiagnostics::default()
    };
    let post_window = PreviewDiagnostics {
        resource_decision_applications: 45_123,
        ..PreviewDiagnostics::default()
    };

    let evidence = professional_runtime_acceptance_evidence(&continuous, &post_window);

    assert_eq!(evidence.resource_policy_applications, 45_000);
}

fn decode_check_observed(
    report: &PreviewDecodePerformanceReport,
    code: &'static str,
) -> Option<u64> {
    report
        .checks
        .iter()
        .find(|check| check.code == code)
        .map(|check| check.observed)
}

fn run_preview_media_continuous_playback_probe(
    root_dir: &Path,
    video_path: &Path,
    media_info: Option<MediaInfo>,
    config: PreviewMediaPlaybackProbeConfig,
) -> anyhow::Result<PreviewMediaPlaybackPerfReport> {
    anyhow::ensure!(
        config.resume_probe_frames == 0 || config.seek_probe_count > 0,
        "pause-seek-resume observations require at least one seek probe"
    );
    let media_info = match media_info {
        Some(media_info) => media_info,
        None => probe_media_info(video_path)
            .with_context(|| format!("probe playback media {}", video_path.display()))?,
    };
    let media_probe = PreviewPlaybackMediaProbeReport::from_media_info(&media_info)?;
    let source_video = media_info
        .primary_video()
        .context("continuous playback probe found no primary video")?;
    let source_resolution = Resolution {
        width: source_video.width,
        height: source_video.height,
    };
    let mut state = build_preview_media_perf_state_with_media_info(
        root_dir,
        video_path,
        Some(media_info),
        config.sequence_frame_count,
    )?;
    if config.video_layer_count > 1 {
        let sequence_id = state
            .active_sequence_id()
            .context("multilayer playback probe has no active Sequence")?;
        state.commit_sequence_edit(
            sequence_id,
            "配置双视频轨播放探针",
            configure_preview_media_dual_video_layers,
        )?;
    }
    if config.authored_output == PreviewMediaAuthoredOutput::SourceFull {
        let sequence_id = state
            .active_sequence_id()
            .context("source-Full playback probe has no active Sequence")?;
        state.commit_sequence_edit(
            sequence_id,
            "配置源分辨率 Full 预览探针",
            |sequence| {
                configure_preview_media_authored_output(
                    sequence,
                    config.authored_output,
                    source_resolution,
                )
            },
        )?;
    }
    let authored_sequence = state
        .active_sequence()
        .context("continuous playback probe lost its active Sequence")?;
    let authored_resolution = authored_sequence.settings.resolution;
    let authored_resolution_scale = authored_sequence.settings.preview.resolution_scale;
    let authored_full_resolution = crate::app::preview_quality::preview_execution_resolution(
        authored_resolution,
        authored_resolution_scale,
        mondrian_playback::PreviewResolutionScale::Full,
    );
    let authored_output = PreviewMediaAuthoredOutputEvidence {
        mode: config.authored_output,
        resolution: authored_resolution,
        resolution_scale: authored_resolution_scale,
        full_extent: HeadlessViewerGpuExtent {
            width: authored_full_resolution.width,
            height: authored_full_resolution.height,
        },
    };
    let gpu_adapter =
        HeadlessViewerGpuAdapter::new_with_native_import_gpu_timing_policy_and_observation_capacity(
            config.native_video_gpu_timing.renderer_policy(),
            config.native_video_gpu_timing.observation_capacity(),
        )
        .context("create real headless Viewer GPU Adapter")?;
    let mut realtime = HeadlessRealtimePlaybackSession::with_gpu_adapter(gpu_adapter)?;
    let decode_execution_journal = PreviewDecodeExecutionJournal::start_from_env(
        realtime.preview()?.decode_execution_watch(),
        config.scenario,
    )?;
    let mut readiness = PreviewReadinessCounts::default();
    let mut headless_gpu_preroll = HeadlessViewerGpuExecutionSummary::default();
    let mut headless_gpu = HeadlessViewerGpuExecutionSummary::default();
    headless_gpu_preroll.adapter = Some(realtime.gpu()?.adapter_info().clone());
    headless_gpu.adapter = Some(realtime.gpu()?.adapter_info().clone());
    let mut process_memory_evidence = PreviewProcessMemoryEvidenceCollector::default();
    let process_memory_probe = SystemPlatformService;
    let mut process_memory_sampler = None;

    state.seek(0)?;
    {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        wait_for_headless_gpu_ready(
            preview_service,
            &mut state,
            gpu_adapter,
            &mut headless_gpu_preroll,
            config.ready_timeout,
        )?;
    }
    let observation_plan = continuous_playback_observation_plan(config.frame_count)?;
    let mut continuous_playback_window = None;
    let playback_case = run_case(
        "preview_media.continuous_playback_readiness",
        1,
        config.playback_threshold_ms,
        || {
            state.play()?;
            // Startup priming is a bounded hold, not a promise that frame zero
            // remains current indefinitely. If cold decoder/GPU setup misses
            // that hold, the authoritative Synthetic Clock must advance and
            // the adapter establishes the first presentable *current* frame.
            // Freezing the clock here leaves a terminal Late demand with no
            // authority for replacement work.
            let initial_ready_observation = {
                let (preview_service, gpu_adapter) = realtime.bound_resources()?;
                wait_for_headless_gpu_ready_observation(
                    preview_service,
                    &mut state,
                    gpu_adapter,
                    &mut headless_gpu,
                    config.ready_timeout,
                )?
            };
            anyhow::ensure!(
                initial_ready_observation.current_gpu_ready,
                "continuous playback failed to establish its exact starting presentation"
            );
            {
                let (preview_service, gpu_adapter) = realtime.bound_resources()?;
                wait_for_headless_playback_preroll(
                    preview_service,
                    &mut state,
                    gpu_adapter,
                    &mut headless_gpu,
                    config.ready_timeout,
                )?;
            }
            // The declared continuous window begins only after cold-start
            // priming has established a presentable current frame. Startup
            // skips remain observable in Preview diagnostics but must not be
            // charged to the later fixed-size clock-displacement ledger.
            state
                .begin_playback_evidence_run(mondrian_playback::PlaybackEvidenceConfig::default())?;
            let playback_started = Instant::now();
            process_memory_sampler =
                Some(ProfessionalProcessMemorySampler::start(playback_started)?);
            let start_frame = state.current_frame();
            let mut opportunity_ledger = ContinuousPlaybackOpportunityLedger::new(
                state.playback_epoch(),
                start_frame,
                config.frame_count,
            )?;
            let planned_terminal_frame = state
                .last_content_frame()
                .context("resolve continuous playback terminal frame")?;
            realtime.begin_realtime(&state, config.absolute_deadline)?;
            for _ in 0..observation_plan.advancing_intervals {
                if opportunity_ledger.classified_opportunities()
                    >= config.frame_count.saturating_sub(observation_plan.terminal_observations)
                {
                    break;
                }
                let outcome = realtime.run_video_interval(
                    &mut state,
                    &mut headless_gpu,
                    config.ready_timeout,
                )?;
                let HeadlessRealtimeIntervalOutcome::Advanced { epoch, frame, sample } = outcome
                else {
                    break;
                };
                opportunity_ledger.record(epoch, frame, sample, &mut readiness)?;
            }
            let _ = realtime.pump_preview_completion(&mut state)?;
            let terminal_epoch = state.playback_epoch();
            let terminal_frame = state.current_frame();
            if !opportunity_ledger.is_complete() {
                let terminal_sample = realtime.complete_current_video_opportunity(
                    &mut state,
                    &mut headless_gpu,
                    config.ready_timeout,
                )?;
                opportunity_ledger.record(
                    terminal_epoch,
                    terminal_frame,
                    terminal_sample,
                    &mut readiness,
                )?;
            }
            let wall_duration_us =
                playback_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            process_memory_evidence.observe_playback_duration(wall_duration_us);
            let terminal = state.playback_engine.snapshot();
            continuous_playback_window = Some(ContinuousPlaybackWindowEvidence {
                target_observations: config.frame_count,
                observed_observations: opportunity_ledger.observed_opportunities(),
                required_duration_us: minimum_continuous_window_duration_us(
                    observation_plan.advancing_intervals,
                    config.frame_interval_ns,
                ),
                start_frame,
                terminal_frame: terminal.position.frame,
                planned_terminal_frame,
                reached_natural_end: terminal.state == mondrian_playback::TransportState::Ended,
                wall_duration_us,
                playback: state.playback_evidence_report(),
            });
            anyhow::ensure!(
                opportunity_ledger.missed_opportunities() == readiness.missed_deadline,
                "continuous playback opportunity ledger diverged from readiness evidence"
            );
            let _ = realtime.finish_realtime()?;
            Ok(())
        },
    )?;
    let continuous_playback_window = continuous_playback_window
        .context("continuous playback case omitted its bounded window evidence")?;
    let process_memory_sampler = process_memory_sampler
        .take()
        .context("continuous playback omitted its process-memory sampler")?;
    for sample in process_memory_sampler.finish()? {
        process_memory_evidence.observe_playback(sample.observed_at_us, sample.sample);
    }
    {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        drain_headless_gpu_submission(
            preview_service,
            &mut state,
            gpu_adapter,
            &mut headless_gpu,
            config.ready_timeout,
        )?;
    }
    let continuous_preview_diagnostics = realtime.preview()?.diagnostics();
    let preview_decode_report = build_preview_decode_performance_report_with_required_access_modes(
        continuous_preview_diagnostics
            .decode_performance_summary(config.decode_slow_frame_budget_us),
        config.scenario,
        config.decode_slow_frame_budget_us,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    );
    let preview_render_report = continuous_preview_diagnostics
        .render_performance_summary(PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US)
        .map(|summary| {
            build_preview_render_performance_report(
                Some(summary),
                config.scenario,
                PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
            )
        });
    let mut headless_gpu_post_window = HeadlessViewerGpuExecutionSummary {
        adapter: Some(realtime.gpu()?.adapter_info().clone()),
        ..HeadlessViewerGpuExecutionSummary::default()
    };
    let mut headless_gpu_resize = HeadlessViewerGpuExecutionSummary {
        adapter: Some(realtime.gpu()?.adapter_info().clone()),
        ..HeadlessViewerGpuExecutionSummary::default()
    };
    // The measured realtime window is complete. Move to the settled transport
    // family explicitly instead of relying on the cadence loop to land on the
    // Sequence's Ended boundary. The final candidate probe must establish a
    // durable output under that exact family before decoder residency is
    // released.
    state.pause()?;
    let seek_case = (config.seek_probe_count > 0)
        .then(|| {
            run_case(
                "preview_media.cross_region_seek_readiness",
                1,
                u128::from(config.seek_probe_count.saturating_add(1) as u64)
                    .saturating_mul(config.seek_threshold_per_settled_ms),
                || {
                    let (preview_service, gpu_adapter) = realtime.bound_resources()?;
                    run_headless_cross_region_seeks(
                        preview_service,
                        &mut state,
                        gpu_adapter,
                        &mut headless_gpu_post_window,
                        config.sequence_frame_count,
                        config.seek_probe_count,
                        config.ready_timeout,
                    )
                },
            )
        })
        .transpose()?;

    let mut pause_seek_resume_probe = None;
    let pause_seek_resume_case = (config.resume_probe_frames > 0)
        .then(|| {
            run_case(
                "preview_media.pause_seek_resume_readiness",
                1,
                u128::from(config.resume_probe_frames as u64).saturating_mul(1_000),
                || {
                    pause_seek_resume_probe = Some(run_headless_pause_seek_resume_probe(
                        &mut realtime,
                        &mut state,
                        &mut headless_gpu_post_window,
                        config.resume_probe_frames,
                        config.ready_timeout,
                    )?);
                    Ok(())
                },
            )
        })
        .transpose()?;

    let mut playback_resize_probe = None;
    let mut playback_resize_context = (config.resize_probe_frames > 0)
        .then(|| {
            prepare_headless_playback_resize_probe(
                &mut realtime,
                &mut state,
                &mut headless_gpu_resize,
                config.ready_timeout,
            )
        })
        .transpose()?;
    let playback_resize_case = (config.resize_probe_frames > 0)
        .then(|| {
            run_case(
                "preview_media.playback_viewer_resize_readiness",
                1,
                u128::from(config.resize_probe_frames as u64).saturating_mul(1_000),
                || {
                    playback_resize_probe = Some(run_headless_playback_resize_probe(
                        &mut realtime,
                        &mut state,
                        &mut headless_gpu_resize,
                        playback_resize_context
                            .as_mut()
                            .context("playback-resize setup omitted its prepared context")?,
                        config.resize_probe_frames,
                        authored_resolution,
                        authored_resolution_scale,
                        config.ready_timeout,
                    )?);
                    Ok(())
                },
            )
        })
        .transpose()?;

    let mut cancellation_recovery_probe = None;
    let cancellation_recovery_case = config
        .probe_cancellation_recovery
        .then(|| {
            run_case(
                "preview_media.cancellation_recovery",
                1,
                config.ready_timeout.as_millis().saturating_mul(2),
                || {
                    let (preview_service, gpu_adapter) = realtime.bound_resources()?;
                    cancellation_recovery_probe = Some(run_headless_cancellation_recovery_probe(
                        preview_service,
                        &mut state,
                        gpu_adapter,
                        &mut headless_gpu_post_window,
                        config.sequence_frame_count,
                        config.ready_timeout,
                    )?);
                    Ok(())
                },
            )
        })
        .transpose()?;

    let gpu_candidate_case = run_case(
        "preview_media.playback_gpu_candidate_ready",
        1,
        config.gpu_candidate_threshold_ms,
        || {
            let (preview_service, gpu_adapter) = realtime.bound_resources()?;
            wait_for_headless_gpu_ready(
                preview_service,
                &mut state,
                gpu_adapter,
                &mut headless_gpu_post_window,
                config.ready_timeout,
            )
        },
    )?;
    {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        settle_headless_preview_and_release_transport_media(
            preview_service,
            &mut state,
            gpu_adapter,
            &mut headless_gpu_post_window,
            config.ready_timeout,
        )?;
    }
    let gpu_timings = realtime
        .gpu_mut()?
        .finish_gpu_timings()
        .context("finish deferred headless Viewer GPU timestamp maps")?;
    distribute_gpu_timestamp_samples(
        &gpu_timings,
        &mut [
            &mut headless_gpu_preroll,
            &mut headless_gpu,
            &mut headless_gpu_post_window,
            &mut headless_gpu_resize,
        ],
        1,
    );
    headless_gpu_post_window.discarded_gpu_timestamp_frames =
        realtime.gpu()?.discarded_gpu_timings();
    let native_video_gpu_timing = build_native_video_gpu_timing_report(
        realtime
            .gpu_mut()?
            .finish_native_import_gpu_timings()
            .context("finish native-import Viewer GPU timing evidence")?,
        &[
            &headless_gpu_preroll,
            &headless_gpu,
            &headless_gpu_post_window,
            &headless_gpu_resize,
        ],
    );
    process_memory_evidence.observe_post_stress(process_memory_probe.product_process_tree_memory());

    anyhow::ensure!(
        readiness.unavailable == 0,
        "continuous playback returned unavailable frames: {:?}; diagnostics: {:?}",
        readiness,
        realtime.preview()?.diagnostics()
    );
    anyhow::ensure!(
        headless_gpu.rendered_frames > 0,
        "continuous playback produced no real headless GPU executions"
    );
    anyhow::ensure!(
        headless_gpu_preroll.native_import_retained_sources_peak == 0
            && headless_gpu.native_import_retained_sources_peak == 0
            && headless_gpu_post_window.native_import_retained_sources_peak == 0
            && headless_gpu_resize.native_import_retained_sources_peak == 0,
        "completed headless GPU outputs retained decoder sources after copy completion: preroll={}, playback={}, post_window={}, resize={}",
        headless_gpu_preroll.native_import_retained_sources_peak,
        headless_gpu.native_import_retained_sources_peak,
        headless_gpu_post_window.native_import_retained_sources_peak,
        headless_gpu_resize.native_import_retained_sources_peak
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

    let preview_diagnostics = realtime.preview()?.diagnostics();
    let media_color_issues = summarize_active_sequence_media_color_issues(&state)?;
    let preview_color_report = build_preview_color_health_report(
        preview_diagnostics.color_health_summary(),
        config.scenario,
    );
    let decode_failure_codes = preview_playback_decode_failures(&preview_decode_report);
    let render_failure_codes = preview_render_report
        .as_ref()
        .map(preview_render_hard_failures)
        .unwrap_or_default();
    // Professional seek/cancellation qualification runs after the continuous
    // window. The collector report is cumulative, so its final snapshot keeps
    // the complete clock/delivery evidence and also includes those completed
    // warm, accurate, and superseded seek samples. The continuous-window
    // snapshot remains independently embedded in `continuous_playback_window`.
    let playback_evidence = state.playback_evidence_report();
    let multilayer_playback = (config.video_layer_count > 1).then(|| {
        evaluate_multilayer_playback(
            config.video_layer_count,
            headless_gpu.rendered_frames,
            u64::from(headless_gpu.rendered_decode_execution.media_layers),
            headless_gpu.compositing_diagnostics.gpu_passthrough_frames,
            headless_gpu.compositing_diagnostics.gpu_native_composites,
            &headless_gpu.output_extents,
            &authored_output.full_extent,
        )
    });
    if let Some(multilayer) = multilayer_playback.as_ref() {
        anyhow::ensure!(
            multilayer.passed,
            "multilayer playback gate failed: {multilayer:?}"
        );
    }
    let report = PreviewMediaPlaybackPerfReport {
        scenario: config.scenario,
        frames: config.frame_count,
        frame_interval_ns: config.frame_interval_ns,
        media_probe,
        authored_output,
        pause_seek_resume_probe,
        playback_resize_probe,
        multilayer_playback,
        readiness,
        headless_gpu_preroll,
        headless_gpu,
        headless_gpu_post_window,
        headless_gpu_resize,
        cancellation_recovery_probe,
        real_media_gates: None,
        qualification_media_gates: None,
        professional_media_gates: None,
        media_color_issues,
        continuous_preview_diagnostics,
        preview_diagnostics,
        preview_color_report,
        decode_failure_codes,
        render_failure_codes,
        preview_decode_report,
        preview_render_report,
        continuous_playback_window,
        playback_evidence,
        process_memory_evidence: process_memory_evidence.report(),
        native_video_gpu_timing,
        cases: std::iter::once(playback_case)
            .chain(seek_case)
            .chain(pause_seek_resume_case)
            .chain(playback_resize_case)
            .chain(cancellation_recovery_case)
            .chain(std::iter::once(gpu_candidate_case))
            .collect(),
    };
    if config.authored_output == PreviewMediaAuthoredOutput::SourceFull {
        anyhow::ensure!(
            report.headless_gpu.output_extents.contains(&report.authored_output.full_extent),
            "source-Full playback window never executed its authored Full GPU extent {:?}: {:?}",
            report.authored_output.full_extent,
            report.headless_gpu.output_extents
        );
    }
    validate_executed_adaptive_scaling(&report)?;
    if let Some(journal) = decode_execution_journal {
        journal.finish()?;
    }
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
        // One interval closes the mathematical duration. A second, complete
        // source-frame interval is measurement guard so process-wall and
        // memory sampling cannot fail a 30-minute requirement by sub-frame
        // observation jitter.
        .saturating_add(2);
    usize::try_from(frames).context("professional playback frame count exceeds usize")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContinuousPlaybackObservationPlan {
    advancing_intervals: usize,
    terminal_observations: usize,
}

fn continuous_playback_observation_plan(
    total_observations: usize,
) -> anyhow::Result<ContinuousPlaybackObservationPlan> {
    let advancing_intervals = total_observations
        .checked_sub(1)
        .context("continuous playback requires one terminal observation")?;
    Ok(ContinuousPlaybackObservationPlan { advancing_intervals, terminal_observations: 1 })
}

/// Exact forward-playback opportunity accounting for the bounded performance
/// harness. Playback Engine demand identities remain the production authority;
/// this ledger only closes authored-frame gaps that elapsed before the harness
/// could sample them, so a clock skip cannot disappear from the readiness
/// denominator.
#[derive(Debug)]
struct ContinuousPlaybackOpportunityLedger {
    epoch: mondrian_playback::PlaybackEpoch,
    next_frame: i64,
    target_opportunities: usize,
    observed_opportunities: usize,
    missed_opportunities: usize,
}

impl ContinuousPlaybackOpportunityLedger {
    fn new(
        epoch: mondrian_playback::PlaybackEpoch,
        first_frame: i64,
        target_opportunities: usize,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            target_opportunities > 0,
            "continuous playback opportunity ledger requires a non-empty target"
        );
        Ok(Self {
            epoch,
            next_frame: first_frame,
            target_opportunities,
            observed_opportunities: 0,
            missed_opportunities: 0,
        })
    }

    fn record(
        &mut self,
        epoch: mondrian_playback::PlaybackEpoch,
        frame: i64,
        sample: HeadlessPreviewSample,
        readiness: &mut PreviewReadinessCounts,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            epoch == self.epoch,
            "continuous playback opportunity changed epoch: expected {:?}, observed {:?}",
            self.epoch,
            epoch
        );
        anyhow::ensure!(
            frame >= self.next_frame,
            "continuous playback opportunity was sampled twice or out of order: next={}, observed={frame}",
            self.next_frame
        );
        let missed = usize::try_from(frame.saturating_sub(self.next_frame))
            .context("continuous playback frame gap exceeds usize")?;
        let remaining = self.target_opportunities.saturating_sub(self.classified_opportunities());
        let missed_in_window = missed.min(remaining);
        self.missed_opportunities = self.missed_opportunities.saturating_add(missed_in_window);
        readiness.missed_deadline = readiness.missed_deadline.saturating_add(missed_in_window);
        if self.is_complete() {
            self.next_frame = frame.saturating_add(1);
            return Ok(());
        }
        self.observed_opportunities = self.observed_opportunities.saturating_add(1);
        self.next_frame = frame.saturating_add(1);
        record_headless_preview_readiness(readiness, sample);
        Ok(())
    }

    const fn observed_opportunities(&self) -> usize {
        self.observed_opportunities
    }

    const fn missed_opportunities(&self) -> usize {
        self.missed_opportunities
    }

    const fn classified_opportunities(&self) -> usize {
        self.observed_opportunities.saturating_add(self.missed_opportunities)
    }

    const fn is_complete(&self) -> bool {
        self.classified_opportunities() >= self.target_opportunities
    }
}

fn professional_native_video_gpu_timing_observation_capacity(
    frame_count: usize,
    seek_probe_count: usize,
) -> anyhow::Result<usize> {
    let candidate_budget = frame_count
        .checked_add(seek_probe_count)
        .and_then(|value| value.checked_add(PROFESSIONAL_NATIVE_VIDEO_GPU_CANDIDATE_OVERHEAD))
        .context("professional native-video GPU timing candidate budget overflowed")?;
    let observation_capacity = candidate_budget
        .checked_mul(PROFESSIONAL_NATIVE_VIDEO_GPU_IMPORTS_PER_CANDIDATE_BUDGET)
        .context("professional native-video GPU timing observation budget overflowed")?;
    anyhow::ensure!(
        observation_capacity <= PROFESSIONAL_NATIVE_VIDEO_GPU_OBSERVATION_CAPACITY_LIMIT,
        "professional native-video GPU timing observation capacity {observation_capacity} exceeds explicit hard limit {PROFESSIONAL_NATIVE_VIDEO_GPU_OBSERVATION_CAPACITY_LIMIT}"
    );
    Ok(observation_capacity)
}

fn professional_playback_case_budget_ms(frame_count: usize, interval_ns: u64) -> u128 {
    const PROFESSIONAL_CASE_OVERHEAD_MARGIN_MS: u128 = 5 * 60 * 1_000;
    (frame_count as u128)
        .saturating_mul(u128::from(interval_ns))
        .saturating_add(999_999)
        .saturating_div(1_000_000)
        .saturating_add(PROFESSIONAL_CASE_OVERHEAD_MARGIN_MS)
}

fn run_headless_cross_region_seeks(
    preview_service: &HeadlessPreviewRuntime,
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
        state.seek_with_source(target as i64, source)?;
        wait_for_headless_gpu_ready(
            preview_service,
            state,
            gpu_adapter,
            gpu_summary,
            timeout_per_seek,
        )?;
    }

    // Scale the latest-wins gesture burst with the declared gate. A
    // professional gate already requests 100 settled regions and therefore
    // retains the 101-input stress burst. A short local gate must not silently
    // perform that professional workload while budgeting only its one
    // declared seek.
    let supersession_count = seek_count.saturating_add(1);
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
        state.seek_with_source(target as i64, source)?;
        let _ = preview_service
            .gpu_preview_frame(state.preview_frame_execution_request(Instant::now()));
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

fn run_headless_pause_seek_resume_probe(
    realtime: &mut HeadlessRealtimePlaybackSession,
    state: &mut AppState,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    observation_count: usize,
    timeout: Duration,
) -> anyhow::Result<PreviewPauseSeekResumeEvidence> {
    anyhow::ensure!(
        observation_count > 0,
        "pause-seek-resume probe requires at least one observation"
    );
    anyhow::ensure!(
        state.playback_engine.snapshot().state == mondrian_playback::TransportState::Paused,
        "pause-seek-resume probe must begin from Paused transport"
    );
    state.play()?;
    let initial = {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        let initial = wait_for_headless_gpu_ready_observation(
            preview_service,
            state,
            gpu_adapter,
            gpu_summary,
            timeout,
        )?;
        wait_for_headless_playback_preroll(
            preview_service,
            state,
            gpu_adapter,
            gpu_summary,
            timeout,
        )?;
        initial
    };
    anyhow::ensure!(
        initial.current_gpu_ready,
        "resumed playback failed to establish an exact current presentation"
    );

    let start_frame = state.current_frame();
    let mut readiness = PreviewReadinessCounts::default();
    let mut observed_observations = 0usize;
    let mut first_epoch = None;
    let mut last_epoch = None;
    realtime.begin_realtime(state, None)?;
    for _ in 0..observation_count {
        match realtime.run_video_interval(state, gpu_summary, timeout)? {
            HeadlessRealtimeIntervalOutcome::Advanced { epoch, sample, .. } => {
                let epoch = epoch.get();
                first_epoch.get_or_insert(epoch);
                last_epoch = Some(epoch);
                observed_observations = observed_observations.saturating_add(1);
                record_headless_preview_readiness(&mut readiness, sample);
            }
            HeadlessRealtimeIntervalOutcome::NaturalEnd { terminal_frame } => {
                anyhow::bail!(
                    "resumed playback reached natural end before completing its observation window at frame {terminal_frame}"
                );
            }
        }
    }
    let _ = realtime.pump_preview_completion(state)?;
    let end_frame = state.current_frame();
    state.pause()?;
    let coordinator_timing = realtime.finish_realtime()?;
    let evidence = evaluate_pause_seek_resume(
        observation_count,
        observed_observations,
        start_frame,
        end_frame,
        first_epoch,
        last_epoch,
        coordinator_timing.maximum_frame_advance,
        coordinator_timing.max_consecutive_stale,
        readiness,
    );
    anyhow::ensure!(
        evidence.passed,
        "pause-seek-resume gate failed: {evidence:?}; last_preroll={:?}; realtime_timing={:?}; gpu_summary={:?}; preview_diagnostics={:?}",
        realtime.preview()?.last_video_preroll_observation_for_test(),
        coordinator_timing,
        gpu_summary,
        realtime.preview()?.diagnostics(),
    );
    Ok(evidence)
}

struct HeadlessPlaybackResizeProbeContext {
    root: AppUiAppRoot,
    start_frame: i64,
}

fn prepare_headless_playback_resize_probe(
    realtime: &mut HeadlessRealtimePlaybackSession,
    state: &mut AppState,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<HeadlessPlaybackResizeProbeContext> {
    anyhow::ensure!(
        state.playback_engine.snapshot().state == mondrian_playback::TransportState::Paused,
        "playback-resize probe must begin from Paused transport"
    );

    state.seek(0)?;
    {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        wait_for_headless_gpu_ready(preview_service, state, gpu_adapter, gpu_summary, timeout)?;
    }
    let mut root = AppUiAppRoot::from_app_state(state);
    TreeWalker::layout(&mut root, Rect::new(0.0, 0.0, 1280.0, 720.0));
    state.play()?;
    let initial = {
        let (preview_service, gpu_adapter) = realtime.bound_resources()?;
        let initial = wait_for_headless_gpu_ready_observation(
            preview_service,
            state,
            gpu_adapter,
            gpu_summary,
            timeout,
        )?;
        wait_for_headless_playback_preroll(
            preview_service,
            state,
            gpu_adapter,
            gpu_summary,
            timeout,
        )?;
        initial
    };
    anyhow::ensure!(
        initial.current_gpu_ready,
        "resized playback failed to establish an exact current presentation"
    );

    // A user begins resizing an already-running Viewer, not an unobserved
    // transport whose first current-frame binding has never entered the
    // realtime coordinator. Warm that coordinator for one interval, then keep
    // the same driver/bindings for every measured resize interval.
    realtime.begin_realtime(state, None)?;
    match realtime.run_video_interval(state, gpu_summary, timeout)? {
        HeadlessRealtimeIntervalOutcome::Advanced { .. } => {}
        HeadlessRealtimeIntervalOutcome::NaturalEnd { terminal_frame } => {
            anyhow::bail!(
                "resized playback reached natural end during coordinator warmup at frame {terminal_frame}"
            );
        }
    }

    Ok(HeadlessPlaybackResizeProbeContext { root, start_frame: state.current_frame() })
}

#[allow(clippy::too_many_arguments)]
fn run_headless_playback_resize_probe(
    realtime: &mut HeadlessRealtimePlaybackSession,
    state: &mut AppState,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    context: &mut HeadlessPlaybackResizeProbeContext,
    observation_count: usize,
    authored_resolution: Resolution,
    authored_resolution_scale: f32,
    timeout: Duration,
) -> anyhow::Result<PreviewPlaybackResizeEvidence> {
    anyhow::ensure!(
        observation_count > 0,
        "playback-resize probe requires at least one observation"
    );
    anyhow::ensure!(
        state.is_playing(),
        "prepared playback-resize probe lost Playing transport authority"
    );
    const WINDOW_BOUNDS: [(f32, f32); 4] = [
        (1280.0, 720.0),
        (1600.0, 900.0),
        (1024.0, 768.0),
        (1920.0, 1080.0),
    ];
    let start_frame = context.start_frame;
    let mut readiness = PreviewReadinessCounts::default();
    let mut observed_observations = 0usize;
    let mut first_epoch = None;
    let mut last_epoch = None;
    let mut presentation_extents = HashSet::new();
    let mut presentation_geometry_valid = true;
    for index in 0..observation_count {
        let (width, height) = WINDOW_BOUNDS[index % WINDOW_BOUNDS.len()];
        let bounds = Rect::new(0.0, 0.0, width, height);
        context.root.refresh_playback_frame_from_app_state(state, None);
        TreeWalker::layout(&mut context.root, bounds);
        let geometry = crate::app_ui::shell::viewer_presentation_geometry(&context.root)
            .context("resized production UI root omitted Viewer presentation geometry")?;
        presentation_extents.insert((
            geometry.presentation.output_width,
            geometry.presentation.output_height,
        ));
        let visible_right = geometry.visible_rect.x + geometry.visible_rect.width;
        let visible_bottom = geometry.visible_rect.y + geometry.visible_rect.height;
        presentation_geometry_valid &= geometry.visible_rect.width > 0.0
            && geometry.visible_rect.height > 0.0
            && geometry.visible_rect.x >= bounds.x
            && geometry.visible_rect.y >= bounds.y
            && visible_right <= bounds.x + bounds.width
            && visible_bottom <= bounds.y + bounds.height;

        match realtime.run_video_interval(state, gpu_summary, timeout)? {
            HeadlessRealtimeIntervalOutcome::Advanced { epoch, sample, .. } => {
                let epoch = epoch.get();
                first_epoch.get_or_insert(epoch);
                last_epoch = Some(epoch);
                observed_observations = observed_observations.saturating_add(1);
                record_headless_preview_readiness(&mut readiness, sample);
            }
            HeadlessRealtimeIntervalOutcome::NaturalEnd { terminal_frame } => {
                anyhow::bail!(
                    "resized playback reached natural end before completing its observation window at frame {terminal_frame}"
                );
            }
        }
    }
    let _ = realtime.pump_preview_completion(state)?;
    let end_frame = state.current_frame();
    state.pause()?;
    let coordinator_timing = realtime.finish_realtime()?;
    let authored_output_unchanged = state.active_sequence().is_some_and(|sequence| {
        sequence.settings.resolution == authored_resolution
            && sequence.settings.preview.resolution_scale == authored_resolution_scale
    });
    let authored_full_extent = HeadlessViewerGpuExtent {
        width: authored_resolution.width,
        height: authored_resolution.height,
    };
    let evidence = evaluate_playback_resize(
        observation_count,
        observed_observations,
        start_frame,
        end_frame,
        first_epoch,
        last_epoch,
        coordinator_timing.maximum_frame_advance,
        coordinator_timing.max_consecutive_stale,
        presentation_extents.len(),
        presentation_geometry_valid,
        authored_output_unchanged,
        readiness,
        &authored_full_extent,
        &gpu_summary.output_extents,
    );
    anyhow::ensure!(
        evidence.passed,
        "playback-resize gate failed: {evidence:?}; coordinator={:?}",
        coordinator_timing,
    );
    Ok(evidence)
}

fn run_headless_cancellation_recovery_probe(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    frame_count: usize,
    timeout: Duration,
) -> anyhow::Result<PreviewCancellationRecoveryEvidence> {
    anyhow::ensure!(
        frame_count >= 8,
        "cancellation-recovery probe requires at least eight Timeline frames"
    );
    // Start from the complete settled-output boundary. A queue-ordered Ready
    // output may still own its submitted native surface, and the prior
    // Interactive decoder session may retain its family residency after work
    // becomes quiescent. Without both retirements, the next decode can park in
    // OutputLeaseWait instead of entering the isolated Seek/PacketRead call
    // this probe declares.
    settle_headless_preview_and_release_transport_media(
        preview_service,
        state,
        gpu_adapter,
        gpu_summary,
        timeout,
    )?;
    let before = preview_service.diagnostics();
    let worker_before = interactive_decode_progress(before.decode_worker_execution)
        .context("cancellation-recovery probe has no Interactive Preview worker")?;
    let broker_cancellations_before = before.decode_cancellation.all.cancellations;
    let media_cancellation_checkpoints_before = before.decode_cancellation_checkpoints.total;
    let cancellation_terminations_before =
        isolated_demux_cancellation_terminations(before.decode_worker_execution);
    let isolated_checkpoints_before =
        before.decode_cancellation_checkpoints.isolated_demux_termination;
    let superseded_target_frame = frame_count.saturating_mul(3).saturating_div(4);
    let recovery_target_frame = frame_count.saturating_div(4);

    state.seek_with_source(superseded_target_frame as i64, TimelineSeekSource::Settled)?;
    let _ =
        preview_service.gpu_preview_frame(state.preview_frame_execution_request(Instant::now()));
    let stage_deadline = Instant::now() + timeout;
    let stage_before_supersession = loop {
        let snapshot = preview_service.decode_execution_watch().snapshot();
        let progress = interactive_decode_progress(snapshot)
            .context("Interactive Preview worker disappeared during cancellation probe")?;
        let active_demux_call = progress.request_sequence > worker_before.request_sequence
            && progress.isolated_demux.active_sessions > 0
            && matches!(
                progress.stage,
                PreviewDecodeExecutionStage::Seek | PreviewDecodeExecutionStage::PacketRead
            );
        if active_demux_call {
            break progress.stage;
        }
        anyhow::ensure!(
            Instant::now() < stage_deadline,
            "timed out observing a real isolated-demux Seek/PacketRead before supersession; progress={progress:?}"
        );
        thread::sleep(Duration::from_micros(50));
    };

    state.seek_with_source(recovery_target_frame as i64, TimelineSeekSource::Settled)?;
    // Recovery must enter through the production Presentation Coordinator.
    // A raw Runtime request is used above only to place the deliberately
    // superseded decode inside isolated demux. Issuing one here could return
    // and immediately drop an already-prepared move-only GPU candidate before
    // its presentation ticket and execution lease reach the Headless Adapter.
    wait_for_headless_gpu_ready(preview_service, state, gpu_adapter, gpu_summary, timeout)?;
    wait_for_preview_work_quiescence(preview_service, state, timeout)?;
    anyhow::ensure!(
        state.current_frame() == recovery_target_frame as i64,
        "post-cancellation Viewer recovery presented the wrong Timeline frame: expected {recovery_target_frame}, observed {}",
        state.current_frame()
    );

    let after = preview_service.diagnostics();
    let broker_cancellation_delta = after
        .decode_cancellation
        .all
        .cancellations
        .saturating_sub(broker_cancellations_before);
    let media_cancellation_checkpoint_delta = after
        .decode_cancellation_checkpoints
        .total
        .saturating_sub(media_cancellation_checkpoints_before);
    let isolated_termination_delta =
        isolated_demux_cancellation_terminations(after.decode_worker_execution)
            .saturating_sub(cancellation_terminations_before);
    let isolated_checkpoint_delta = after
        .decode_cancellation_checkpoints
        .isolated_demux_termination
        .saturating_sub(isolated_checkpoints_before);
    anyhow::ensure!(
        broker_cancellation_delta > 0 && media_cancellation_checkpoint_delta > 0,
        "superseded real media execution did not produce matched Broker and media cancellation evidence: broker_delta={broker_cancellation_delta}, media_checkpoint_delta={media_cancellation_checkpoint_delta}, diagnostics={after:?}"
    );
    anyhow::ensure!(
        (isolated_termination_delta == 0) == (isolated_checkpoint_delta == 0),
        "isolated-demux termination and media checkpoint evidence disagree: termination_delta={isolated_termination_delta}, checkpoint_delta={isolated_checkpoint_delta}"
    );

    Ok(PreviewCancellationRecoveryEvidence {
        stage_before_supersession,
        superseded_target_frame,
        recovery_target_frame,
        broker_cancellation_delta,
        media_cancellation_checkpoint_delta,
        isolated_termination_delta,
        isolated_checkpoint_delta,
        recovery_presented: true,
    })
}

fn interactive_decode_progress(
    workers: crate::app::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics,
) -> Option<mondrian_media::PreviewDecodeExecutionProgress> {
    workers.non_playback.or(workers.any)
}

fn isolated_demux_cancellation_terminations(
    workers: crate::app::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics,
) -> u64 {
    [workers.any, workers.playback, workers.non_playback]
        .into_iter()
        .flatten()
        .map(|progress| progress.isolated_demux.cancellation_terminations)
        .fold(0, u64::saturating_add)
}

fn wait_for_preview_work_quiescence(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let work_watch = preview_service.work_watch();
    loop {
        let drain_target_revision = work_watch.revision();
        let pump_outcome = apply_headless_preview_outcome(preview_service, state);
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
        wait_for_headless_preview_revision(
            &work_watch,
            drain_target_revision,
            deadline,
            pump_outcome.needs_follow_up_poll,
        );
    }
}

fn wait_for_preview_idle_residency_release(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    timeout: Duration,
) -> anyhow::Result<()> {
    wait_for_preview_work_quiescence(preview_service, state, timeout)?;
    anyhow::ensure!(
        preview_service.try_release_settled_transport_all_media_residency(),
        "Preview output or work state was not settled while releasing decoder-backed media residency"
    );
    let deadline = Instant::now() + timeout;
    let work_watch = preview_service.work_watch();
    loop {
        let drain_target_revision = work_watch.revision();
        let pump_outcome = apply_headless_preview_outcome(preview_service, state);
        let diagnostics = preview_service.diagnostics();
        let work_remains = diagnostics.scheduler.pending_requests > 0
            || diagnostics.worker_queue.queued_jobs > 0
            || diagnostics.worker_queue.in_flight_jobs > 0;
        if !work_remains
            && preview_decode_workers_idle_and_reaped(diagnostics.decode_worker_execution)
        {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "Preview workers did not retire decoder Sessions and reap demux helpers after idle residency release: {diagnostics:?}"
        );
        wait_for_headless_preview_revision(
            &work_watch,
            drain_target_revision,
            deadline,
            pump_outcome.needs_follow_up_poll,
        );
    }
}

fn preview_decode_workers_idle_and_reaped(
    workers: crate::app::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics,
) -> bool {
    [workers.any, workers.playback, workers.non_playback]
        .into_iter()
        .flatten()
        .all(|progress| {
            let demux = progress.isolated_demux;
            progress.stage == PreviewDecodeExecutionStage::Idle
                && demux.active_sessions == 0
                && demux.reaped_sessions() == demux.session_launches
        })
}

#[test]
fn preview_idle_release_requires_worker_idle_and_post_reap_accounting() {
    use mondrian_media::{PreviewDecodeExecutionProgress, PreviewIsolatedDemuxExecutionEvidence};

    let complete = PreviewDecodeExecutionProgress {
        isolated_demux: PreviewIsolatedDemuxExecutionEvidence {
            session_launches: 1,
            clean_closes: 1,
            ..PreviewIsolatedDemuxExecutionEvidence::default()
        },
        ..PreviewDecodeExecutionProgress::default()
    };
    let workers = crate::app::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics {
        playback: Some(complete),
        ..crate::app::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics::default()
    };
    assert!(preview_decode_workers_idle_and_reaped(workers));

    let mut still_retiring = complete;
    still_retiring.stage = PreviewDecodeExecutionStage::SessionRetire;
    assert!(!preview_decode_workers_idle_and_reaped(
        crate::app::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics {
            playback: Some(still_retiring),
            ..workers
        }
    ));

    let mut unreaped = complete;
    unreaped.isolated_demux.clean_closes = 0;
    unreaped.isolated_demux.active_sessions = 1;
    assert!(!preview_decode_workers_idle_and_reaped(
        crate::app::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics {
            playback: Some(unreaped),
            ..workers
        }
    ));
}

fn validate_executed_adaptive_scaling(
    report: &PreviewMediaPlaybackPerfReport,
) -> anyhow::Result<()> {
    validate_executed_adaptive_scaling_for_window(
        report.continuous_playback_window.playback.deliveries.degraded,
        &report.headless_gpu.output_extents,
    )
}

fn validate_executed_adaptive_scaling_for_window(
    degraded: u64,
    output_extents: &[HeadlessViewerGpuExtent],
) -> anyhow::Result<()> {
    let pressure_threshold = mondrian_playback::PlaybackPolicy::default().pressure_threshold as u64;
    let quarter_evidence_threshold = pressure_threshold.saturating_mul(2);
    if degraded < pressure_threshold {
        return Ok(());
    }
    let full = output_extents
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
        output_extents.contains(&expected_half),
        "sustained playback pressure did not execute a Half GPU extent: {:?}",
        output_extents
    );
    if degraded >= quarter_evidence_threshold {
        anyhow::ensure!(
            output_extents.contains(&expected_quarter),
            "continued playback pressure did not execute a Quarter GPU extent: {:?}",
            output_extents
        );
    }
    Ok(())
}

#[test]
fn adaptive_scaling_validation_uses_the_continuous_window_pressure() {
    let policy = mondrian_playback::PlaybackPolicy::default();
    let threshold = u64::try_from(policy.pressure_threshold).expect("pressure threshold fits u64");
    let full = HeadlessViewerGpuExtent { width: 3840, height: 2160 };
    let half = HeadlessViewerGpuExtent { width: 1920, height: 1080 };
    let quarter = HeadlessViewerGpuExtent { width: 960, height: 540 };

    assert!(validate_executed_adaptive_scaling_for_window(0, &[full]).is_ok());
    assert!(validate_executed_adaptive_scaling_for_window(threshold, &[full]).is_err());
    assert!(validate_executed_adaptive_scaling_for_window(threshold, &[full, half]).is_ok());
    assert!(validate_executed_adaptive_scaling_for_window(
        threshold.saturating_mul(2),
        &[full, half],
    )
    .is_err());
    assert!(validate_executed_adaptive_scaling_for_window(
        threshold.saturating_mul(2),
        &[full, half, quarter],
    )
    .is_ok());
}

#[test]
fn submitted_current_frame_can_fill_the_bounded_successor_slot() {
    assert!(headless_candidate_may_prepare_successor(
        HeadlessGpuCandidateStatus::Ready
    ));
    assert!(headless_candidate_may_prepare_successor(
        HeadlessGpuCandidateStatus::QueuedReady
    ));
    assert!(headless_candidate_may_prepare_successor(
        HeadlessGpuCandidateStatus::InFlight
    ));
    for status in [
        HeadlessGpuCandidateStatus::Loading,
        HeadlessGpuCandidateStatus::Backpressured,
        HeadlessGpuCandidateStatus::DroppedLate,
        HeadlessGpuCandidateStatus::Unavailable,
    ] {
        assert!(
            !headless_candidate_may_prepare_successor(status),
            "{status:?} has no submitted current-frame owner for bounded successor work"
        );
    }
}

#[test]
fn headless_driver_always_runs_its_first_candidate_reconciliation() {
    let intent = headless_candidate_test_intent(7);

    assert!(should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Loading,
        None,
        intent,
        PlaybackPreviewPumpOutcome::default(),
    ));
}

#[test]
fn completed_gpu_evidence_reopens_current_candidate_reconciliation() {
    let intent = headless_candidate_test_intent(7);
    let mut binding = None;
    apply_headless_candidate_binding(
        &mut binding,
        intent,
        HeadlessGpuCandidateBindingUpdate::AttemptedIntent,
    );
    assert!(binding.is_some_and(|binding| binding.covers_current(intent)));

    apply_headless_candidate_binding(
        &mut binding,
        intent,
        HeadlessGpuCandidateBindingUpdate::RetryCurrentIntent,
    );

    assert!(binding.is_none());
    assert!(should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Loading,
        binding,
        intent,
        PlaybackPreviewPumpOutcome::default(),
    ));
}

#[test]
fn consumed_demand_remains_satisfied_without_rebinding_to_no_demand() {
    let sampled = headless_candidate_test_intent(7);
    let after_completion = HeadlessGpuCandidateIntent {
        epoch: sampled.epoch,
        quality_revision: sampled.quality_revision,
        frame: sampled.frame,
        pending_demand: None,
    };
    let mut binding = None;

    apply_headless_candidate_binding(
        &mut binding,
        sampled,
        HeadlessGpuCandidateBindingUpdate::SatisfiedIntent,
    );
    apply_headless_candidate_binding(
        &mut binding,
        after_completion,
        HeadlessGpuCandidateBindingUpdate::SatisfiedIntent,
    );

    assert!(binding.is_some_and(|binding| binding.covers_current(after_completion)));
    assert_eq!(
        binding.map(|binding| binding.intent),
        Some(sampled),
        "callback reconciliation must preserve exact completed-demand authority"
    );
    assert!(!should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Ready,
        binding,
        after_completion,
        PlaybackPreviewPumpOutcome::default(),
    ));
    assert!(headless_candidate_is_ready_for_sample(
        HeadlessGpuCandidateStatus::Ready,
        binding,
        sampled,
        true,
    ));
    assert!(headless_candidate_is_ready_for_sample(
        HeadlessGpuCandidateStatus::Ready,
        binding,
        after_completion,
        true,
    ));
}

#[test]
fn consumed_demand_never_covers_a_new_quality_revision_at_the_same_frame() {
    let sampled = headless_candidate_test_intent(7);
    let revised = HeadlessGpuCandidateIntent {
        quality_revision: sampled.quality_revision.saturating_add(1),
        pending_demand: None,
        ..sampled
    };
    let binding = Some(satisfied_headless_candidate_binding(sampled));

    assert!(!binding.is_some_and(|binding| binding.covers_current(revised)));
    assert!(should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Ready,
        binding,
        revised,
        PlaybackPreviewPumpOutcome::default(),
    ));
}

#[test]
fn non_gpu_candidate_replaces_prior_gpu_sample_binding_explicitly() {
    let mut output_binding = Some(HeadlessGpuCandidateOutputBinding::Gpu(
        crate::app::preview_execution::PreviewOutputKey::new(
            mondrian_core::types::SequenceId::new(),
            1,
            1,
            crate::app::preview_execution::PreviewSemanticIdentity::from_test_fingerprint([9; 32]),
        ),
    ));

    apply_headless_candidate_output_binding(
        &mut output_binding,
        HeadlessGpuCandidateOutputBindingUpdate::NonGpu,
    );

    assert_eq!(
        output_binding,
        Some(HeadlessGpuCandidateOutputBinding::NonGpu)
    );
}

#[test]
fn old_ready_binding_never_counts_for_one_or_many_advanced_frames() {
    let sampled = headless_candidate_test_intent(7);
    for current_frame in [8, 19] {
        let current = HeadlessGpuCandidateIntent {
            epoch: sampled.epoch,
            quality_revision: sampled.quality_revision,
            frame: current_frame,
            pending_demand: sampled.pending_demand,
        };
        assert!(!headless_candidate_is_ready_for_sample(
            HeadlessGpuCandidateStatus::Ready,
            Some(satisfied_headless_candidate_binding(sampled)),
            current,
            true,
        ));
    }
}

#[test]
fn gpu_capacity_retry_states_force_a_bounded_wait_even_with_preview_backlog() {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(1);
    for status in [
        HeadlessGpuCandidateStatus::QueuedReady,
        HeadlessGpuCandidateStatus::InFlight,
        HeadlessGpuCandidateStatus::Backpressured,
        HeadlessGpuCandidateStatus::Unavailable,
    ] {
        // Model `needs_follow_up_poll = true`; capacity-bound states must
        // suppress that immediate retry.
        let immediate_follow_up = !status.requires_bounded_wait();
        assert_eq!(
            headless_preview_wait_budget(deadline, now, immediate_follow_up),
            Some(HEADLESS_PREVIEW_CLOCK_TICK_MAX_WAIT),
        );
    }
}

#[test]
fn loading_candidate_does_not_retry_same_frame_and_demand_without_progress() {
    let intent = headless_candidate_test_intent(7);

    assert!(!should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Loading,
        Some(attempted_headless_candidate_binding(intent)),
        intent,
        PlaybackPreviewPumpOutcome::default(),
    ));
}

#[test]
fn loading_candidate_retries_same_intent_after_visible_progress() {
    let intent = headless_candidate_test_intent(7);

    assert!(should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Loading,
        Some(attempted_headless_candidate_binding(intent)),
        intent,
        PlaybackPreviewPumpOutcome {
            visible_change: true,
            ..PlaybackPreviewPumpOutcome::default()
        },
    ));
}

#[test]
fn loading_candidate_consumes_candidate_only_retry_signal() {
    let intent = headless_candidate_test_intent(7);

    assert!(should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Loading,
        Some(attempted_headless_candidate_binding(intent)),
        intent,
        PlaybackPreviewPumpOutcome {
            candidate_retry_required: true,
            ..PlaybackPreviewPumpOutcome::default()
        },
    ));
}

#[test]
fn loading_candidate_retries_same_frame_for_a_new_exact_demand() {
    let mut state = AppState::new();
    state.set_playback_frame_running(7);
    let first = HeadlessGpuCandidateIntent::from_state(&state);
    state.set_playback_frame_running(7);
    let second = HeadlessGpuCandidateIntent::from_state(&state);
    assert_ne!(first.pending_demand, second.pending_demand);

    assert!(should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Ready,
        Some(satisfied_headless_candidate_binding(first)),
        second,
        PlaybackPreviewPumpOutcome::default(),
    ));
}

#[test]
fn loading_candidate_retries_when_the_timeline_frame_changes() {
    let intent = headless_candidate_test_intent(7);
    let next_frame = HeadlessGpuCandidateIntent { frame: 8, ..intent };

    assert!(should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Loading,
        Some(attempted_headless_candidate_binding(intent)),
        next_frame,
        PlaybackPreviewPumpOutcome::default(),
    ));
}

#[test]
fn ready_candidate_is_stable_for_the_same_exact_intent() {
    let intent = headless_candidate_test_intent(7);

    assert!(!should_attempt_headless_gpu_candidate(
        HeadlessGpuCandidateStatus::Ready,
        Some(satisfied_headless_candidate_binding(intent)),
        intent,
        PlaybackPreviewPumpOutcome {
            visible_change: true,
            ..PlaybackPreviewPumpOutcome::default()
        },
    ));
}

fn headless_candidate_test_intent(frame: i64) -> HeadlessGpuCandidateIntent {
    let mut state = AppState::new();
    state.set_playback_frame_running(frame);
    HeadlessGpuCandidateIntent::from_state(&state)
}

fn attempted_headless_candidate_binding(
    intent: HeadlessGpuCandidateIntent,
) -> HeadlessGpuCandidateBinding {
    HeadlessGpuCandidateBinding {
        intent,
        state: HeadlessGpuCandidateBindingState::Attempted,
    }
}

fn satisfied_headless_candidate_binding(
    intent: HeadlessGpuCandidateIntent,
) -> HeadlessGpuCandidateBinding {
    HeadlessGpuCandidateBinding {
        intent,
        state: HeadlessGpuCandidateBindingState::Satisfied,
    }
}

fn drain_headless_gpu_submission(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("derive bounded Headless GPU drain deadline")?;
    let work_watch = preview_service.work_watch();
    while gpu_adapter.has_submission_in_flight() {
        let drain_target_revision = work_watch.revision();
        let pump_outcome = apply_headless_preview_outcome(preview_service, state);
        let attempt = execute_headless_gpu_candidate(
            preview_service,
            state,
            gpu_adapter,
            gpu_summary,
            HeadlessGpuCompletionDeadline::at(deadline),
        )?;
        anyhow::ensure!(
            Instant::now() < deadline,
            "timed out draining exact Headless Viewer GPU submission; next_wake={:?}, diagnostics={:?}",
            gpu_adapter.next_submission_wake(),
            preview_service.diagnostics()
        );
        if gpu_adapter.has_submission_in_flight() {
            wait_for_headless_preview_revision(
                &work_watch,
                drain_target_revision,
                gpu_adapter.next_submission_wake().unwrap_or(deadline).min(deadline),
                pump_outcome.needs_follow_up_poll && !attempt.status.requires_bounded_wait(),
            );
        }
    }
    Ok(())
}

fn settle_headless_preview_and_release_transport_media(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<()> {
    wait_for_headless_gpu_ready(preview_service, state, gpu_adapter, gpu_summary, timeout)?;
    drain_headless_gpu_submission(preview_service, state, gpu_adapter, gpu_summary, timeout)?;
    wait_for_preview_idle_residency_release(preview_service, state, timeout)
}

fn wait_for_headless_gpu_ready(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<()> {
    wait_for_headless_gpu_ready_observation(
        preview_service,
        state,
        gpu_adapter,
        gpu_summary,
        timeout,
    )
    .map(|_| ())
}

fn wait_for_headless_gpu_ready_observation(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<HeadlessPreviewSample> {
    wait_for_headless_gpu_ready_observation_impl(
        preview_service,
        state,
        gpu_adapter,
        gpu_summary,
        timeout,
    )
}

fn wait_for_headless_gpu_ready_observation_impl(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<HeadlessPreviewSample> {
    let deadline = Instant::now() + timeout;
    let work_watch = preview_service.work_watch();
    let mut candidate_binding = None;
    let mut candidate_output_binding = None;
    let mut candidate_status = HeadlessGpuCandidateStatus::Loading;
    loop {
        let drain_target_revision = work_watch.revision();
        let now = Instant::now();
        if state.is_playing() {
            state.advance_playback_clock_at(now);
        }
        let pump_outcome = apply_headless_preview_outcome(preview_service, state);
        let current_intent = HeadlessGpuCandidateIntent::from_state(state);
        let attempt_gate = should_attempt_headless_gpu_candidate(
            candidate_status,
            candidate_binding,
            current_intent,
            pump_outcome,
        );
        if attempt_gate {
            let attempt = execute_headless_gpu_candidate(
                preview_service,
                state,
                gpu_adapter,
                gpu_summary,
                HeadlessGpuCompletionDeadline::at(deadline),
            )?;
            candidate_status = attempt.status;
            apply_headless_candidate_binding(
                &mut candidate_binding,
                current_intent,
                attempt.binding,
            );
            apply_headless_candidate_output_binding(
                &mut candidate_output_binding,
                attempt.output_binding,
            );
        }
        let sampled_intent = HeadlessGpuCandidateIntent::from_state(state);
        let output_binding_matches = headless_candidate_output_binding_matches(
            candidate_output_binding.as_ref(),
            preview_service,
        );
        if headless_candidate_is_ready_for_sample(
            candidate_status,
            candidate_binding,
            sampled_intent,
            output_binding_matches,
        ) {
            return Ok(HeadlessPreviewSample {
                current_gpu_ready: true,
                stale_output_available: preview_service.has_retained_gpu_output(),
                unavailable: false,
            });
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "timed out waiting for a real headless Viewer GPU output; current_intent={sampled_intent:?}, candidate_status={candidate_status:?}, candidate_binding={candidate_binding:?}, candidate_output_binding={candidate_output_binding:?}, output_binding_matches={output_binding_matches}, pending_demand={:?}, transport={:?}, diagnostics={:?}",
            state.pending_playback_frame_demand_identity(),
            state.playback_engine.snapshot(),
            preview_service.diagnostics()
        );
        wait_for_headless_preview_revision(
            &work_watch,
            drain_target_revision,
            deadline,
            pump_outcome.needs_follow_up_poll && !candidate_status.requires_bounded_wait(),
        );
    }
}

#[test]
fn terminal_headless_observation_closes_a_consumed_exact_demand_without_ready() {
    let target = headless_candidate_test_intent(7);
    let target_demand = target.pending_demand.expect("running intent demand");
    let resolved = HeadlessGpuCandidateIntent { pending_demand: None, ..target };

    assert!(!headless_demand_resolved_without_ready(target, target,));
    assert!(headless_demand_resolved_without_ready(target, resolved,));
    assert_ne!(resolved.pending_demand, Some(target_demand));
}

#[test]
fn terminal_headless_observation_retargets_only_a_newer_exact_quality_demand() {
    let target = headless_candidate_test_intent(7);
    let target_demand = target.pending_demand.expect("running intent demand");
    let revised_quality = target.quality_revision.saturating_add(1);
    let revised_demand = mondrian_playback::FrameDemandIdentity {
        quality_revision: revised_quality,
        ..target_demand
    };
    let revised = HeadlessGpuCandidateIntent {
        quality_revision: revised_quality,
        pending_demand: Some(revised_demand),
        ..target
    };

    assert!(headless_terminal_observation_may_retarget_quality(
        target, revised
    ));
    assert!(!headless_terminal_observation_may_retarget_quality(
        target,
        HeadlessGpuCandidateIntent { frame: 8, ..revised }
    ));
    assert!(!headless_terminal_observation_may_retarget_quality(
        target,
        HeadlessGpuCandidateIntent { pending_demand: None, ..revised }
    ));
}

fn wait_for_headless_playback_preroll(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    gpu_summary: &mut HeadlessViewerGpuExecutionSummary,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("derive bounded Headless playback-preroll deadline")?;
    let work_watch = preview_service.work_watch();
    while state.is_playback_priming() {
        let drain_target_revision = work_watch.revision();
        let pump_outcome = apply_headless_preview_outcome(preview_service, state);
        let now = Instant::now();
        state.advance_playback_clock_at(now);
        let attempt = execute_headless_gpu_candidate(
            preview_service,
            state,
            gpu_adapter,
            gpu_summary,
            HeadlessGpuCompletionDeadline::at(deadline),
        )?;
        if state.is_playback_priming()
            && matches!(
                attempt.status,
                HeadlessGpuCandidateStatus::Ready | HeadlessGpuCandidateStatus::QueuedReady
            )
        {
            let _ = prepare_headless_preview_successor(
                preview_service,
                state,
                gpu_adapter,
                HeadlessGpuCompletionDeadline::at(deadline),
            )?;
            stage_headless_preview_lookahead(preview_service, state, gpu_adapter)?;
        }
        anyhow::ensure!(
            now < deadline,
            "timed out waiting for bounded playback video preroll; diagnostics: {:?}",
            preview_service.diagnostics()
        );
        wait_for_headless_preview_revision(
            &work_watch,
            drain_target_revision,
            deadline,
            pump_outcome.needs_follow_up_poll && !attempt.status.requires_bounded_wait(),
        );
    }
    Ok(())
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
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
) -> super::playback_preview::PlaybackPreviewPumpOutcome {
    pump_playback_preview(state, preview_service)
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
        EffectType::ChromaticAberration,
        EffectType::Grain,
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
    state.test_set_asset_library(Some(library));
    state.test_set_active_sequence(sequence_id);
    state.test_set_default_sequence(sequence_id);
    state.test_set_sequences(vec![sequence.clone()]);
    state.test_set_sequence(Some(sequence));

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
    generate_preview_media_fixture_with_size(path, 320, 180, 2)
}

fn generate_preview_media_fixture_with_size(
    path: &Path,
    width: u32,
    height: u32,
    duration_secs: u32,
) -> anyhow::Result<bool> {
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
        .arg(format!(
            "testsrc2=size={width}x{height}:rate=30:duration={duration_secs}"
        ))
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
    let media_info = match media_info {
        Some(media_info) => media_info,
        None => probe_media_info(video_path)
            .with_context(|| format!("probe playback media {}", video_path.display()))?,
    };
    let probed_frame_rate = media_info
        .primary_video()
        .filter(|video| {
            video.frame_rate_proven && video.frame_rate.num > 0 && video.frame_rate.den > 0
        })
        .map(|video| canonical_preview_probe_sequence_frame_rate(video.frame_rate))
        .transpose()?;
    let asset_id = commit_perf_media_probe(&library, video_path, media_info)?;

    let mut sequence = Sequence::new("Preview media perf");
    let mut settings = sequence.settings.clone();
    settings.frame_rate = probed_frame_rate.unwrap_or(Rational::FPS_30);
    sequence.apply_settings(settings)?;
    let tb = sequence.time_base();
    let duration = tt(frame_count as i64, tb);
    sequence.video_tracks[0]
        .add_clip(Clip::new(asset_id, tt(0, tb), duration).expect("valid clip"))?;
    sequence.playhead = tt(0, tb);
    sequence.mark_out(duration);
    let sequence_id = sequence.id;

    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));
    state.test_set_active_sequence(sequence_id);
    state.test_set_default_sequence(sequence_id);
    state.test_set_sequences(vec![sequence.clone()]);
    state.test_set_sequence(Some(sequence));
    Ok(state)
}

fn configure_preview_media_authored_output(
    sequence: &mut Sequence,
    mode: PreviewMediaAuthoredOutput,
    source_resolution: Resolution,
) -> mondrian_core::Result<()> {
    if mode == PreviewMediaAuthoredOutput::SequenceDefault {
        return Ok(());
    }
    let mut settings = sequence.settings.clone();
    settings.resolution = source_resolution;
    settings.preview.resolution_scale = 1.0;
    sequence.apply_settings(settings)
}

fn configure_preview_media_dual_video_layers(sequence: &mut Sequence) -> mondrian_core::Result<()> {
    let first_clip = sequence
        .video_tracks
        .first()
        .and_then(|track| track.clips.first())
        .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "configure_preview_media_dual_video_layers".to_owned(),
            reason: "primary video Track has no media Clip".to_owned(),
        })?;
    let asset_id = first_clip.media_asset_id().ok_or_else(|| {
        mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "configure_preview_media_dual_video_layers".to_owned(),
            reason: "primary video Clip is not file-backed media".to_owned(),
        }
    })?;
    let position = first_clip.position;
    let duration = first_clip.duration;
    let mut second_clip = Clip::new(asset_id, position, duration)?;
    second_clip.set_source_origin(tt(1, sequence.time_base()))?;
    let track_id = sequence.add_video_track();
    let track = sequence.video_track_mut(track_id).ok_or_else(|| {
        mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "configure_preview_media_dual_video_layers".to_owned(),
            reason: "new video Track was not retained by the Sequence".to_owned(),
        }
    })?;
    track.add_clip(second_clip)
}

fn canonical_preview_probe_sequence_frame_rate(frame_rate: Rational) -> anyhow::Result<Rational> {
    anyhow::ensure!(
        frame_rate.num > 0 && frame_rate.den > 0,
        "preview probe frame rate must be positive: {frame_rate}"
    );
    let observed = frame_rate.num as f64 / frame_rate.den as f64;
    let candidate = Rational::SEQUENCE_FRAME_RATES
        .iter()
        .copied()
        .min_by(|left, right| {
            let left_distance = (observed - left.num as f64 / left.den as f64).abs();
            let right_distance = (observed - right.num as f64 / right.den as f64).abs();
            left_distance.total_cmp(&right_distance)
        })
        .context("Sequence frame-rate catalog is empty")?;
    let difference = (i128::from(frame_rate.num) * i128::from(candidate.den)
        - i128::from(candidate.num) * i128::from(frame_rate.den))
    .unsigned_abs();
    let relative_denominator = (candidate.num as u128)
        .checked_mul(frame_rate.den as u128)
        .context("preview probe frame-rate tolerance denominator overflow")?;
    let scaled_difference = difference
        .checked_mul(1_000)
        .context("preview probe frame-rate tolerance numerator overflow")?;
    anyhow::ensure!(
        scaled_difference <= relative_denominator,
        "probed frame rate {frame_rate} is not within 0.1% of a supported Sequence rate (nearest {candidate})"
    );
    Ok(candidate)
}

fn probe_external_preview_media_info(video_path: &Path) -> anyhow::Result<MediaInfo> {
    probe_media_info(video_path)
        .with_context(|| format!("probe external preview media {}", video_path.display()))
}

fn summarize_active_sequence_media_color_issues(
    state: &AppState,
) -> anyhow::Result<VideoColorDiagnosticIssueAggregate> {
    let Some(sequence) = state.active_sequence() else {
        return Ok(VideoColorDiagnosticIssueAggregate::default());
    };
    let Some(library) = state.asset_library() else {
        return Ok(VideoColorDiagnosticIssueAggregate::default());
    };

    let mut asset_ids = std::collections::HashSet::new();
    for track in &sequence.video_tracks {
        for clip in &track.clips {
            if let Some(asset_id) = clip.media_asset_id() {
                asset_ids.insert(asset_id);
            }
        }
    }

    let mut aggregate = VideoColorDiagnosticIssueAggregate::default();
    for asset_id in asset_ids {
        let Some(asset) = library.get_asset(asset_id)? else {
            continue;
        };
        let Some(video) = asset.media_probe().and_then(|probe| probe.primary_video()) else {
            continue;
        };
        aggregate.observe(&VideoColorDiagnostic::from_stream(video));
    }

    Ok(aggregate)
}

#[test]
fn preview_color_report_summarizes_legacy_and_gpu_blockers() {
    let diagnostics = PreviewDiagnostics {
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
        ..PreviewDiagnostics::default()
    };

    let report =
        build_preview_color_health_report(diagnostics.color_health_summary(), "preview-test");

    let summary = report.summary.expect("preview color summary");
    assert_eq!(report.verdict, PreviewColorHealthVerdict::Fail);
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
    let diagnostics = PreviewDiagnostics {
        color_composite_plans: 2,
        color_composite_elements: 2,
        color_composite_float_linear: 2,
        ..PreviewDiagnostics::default()
    };

    let report =
        build_preview_color_health_report(diagnostics.color_health_summary(), "preview-clean");
    let summary = report.summary.expect("preview color summary");

    assert_eq!(report.verdict, PreviewColorHealthVerdict::Pass);
    assert_eq!(summary.legacy_reason_total, 0);
    assert!(summary.fully_float_linear);
    assert!(summary.gpu_path_ready);
}

fn test_complete_perf_decode_success_evidence(
    mut profile: PreviewDecodeAccessModeProfile,
) -> PreviewDecodeAccessModeProfile {
    assert!(profile.frames > 0);
    let work_frames = [
        profile.work_classes.cache_hit.frames,
        profile.work_classes.session_opened.frames,
        profile.work_classes.session_replaced.frames,
        profile.work_classes.forward_steady.frames,
        profile.work_classes.reused_seek.frames,
        profile.work_classes.reused_other.frames,
        profile.work_classes.unclassified.frames,
    ]
    .into_iter()
    .fold(0_u64, u64::saturating_add);
    assert_eq!(work_frames, profile.frames);
    let lifecycle_frames = profile
        .session_opened_frames
        .saturating_add(profile.session_replaced_frames)
        .saturating_add(profile.session_reused_frames)
        .saturating_add(profile.session_bypassed_cache_frames)
        .saturating_add(profile.session_unclassified_frames);
    assert_eq!(lifecycle_frames, profile.frames);

    let queue_histogram_samples = [
        profile.queue_wait_buckets.le_10ms,
        profile.queue_wait_buckets.le_16ms,
        profile.queue_wait_buckets.le_25ms,
        profile.queue_wait_buckets.le_40ms,
        profile.queue_wait_buckets.le_50ms,
        profile.queue_wait_buckets.le_80ms,
        profile.queue_wait_buckets.gt_80ms,
    ]
    .into_iter()
    .fold(0_u64, u64::saturating_add);
    match (profile.queue_wait_samples, queue_histogram_samples) {
        (0, 0) => {
            profile.queue_wait_samples = profile.frames;
            match profile.queue_wait_max_us {
                0..=10_000 => profile.queue_wait_buckets.le_10ms = profile.frames,
                10_001..=16_000 => profile.queue_wait_buckets.le_16ms = profile.frames,
                16_001..=25_000 => profile.queue_wait_buckets.le_25ms = profile.frames,
                25_001..=40_000 => profile.queue_wait_buckets.le_40ms = profile.frames,
                40_001..=50_000 => profile.queue_wait_buckets.le_50ms = profile.frames,
                50_001..=80_000 => profile.queue_wait_buckets.le_80ms = profile.frames,
                _ => profile.queue_wait_buckets.gt_80ms = profile.frames,
            }
        }
        (0, samples) => profile.queue_wait_samples = samples,
        (samples, histogram_samples) => assert_eq!(samples, histogram_samples),
    }
    assert!(profile.queue_wait_samples >= profile.frames);
    profile
}

#[test]
fn preview_decode_hard_failures_include_failed_report() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 80_000,
        decode_max_duration_us: 80_000,
        decode_last_duration_us: 80_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            random_access_still: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                total_duration_us: 80_000,
                max_duration_us: 80_000,
                last_duration_us: 80_000,
                session_reused_frames: 1,
                work_classes: PreviewDecodeWorkClassProfiles {
                    reused_other: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        total_duration_us: 80_000,
                        max_duration_us: 80_000,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_80ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                    },
                    ..PreviewDecodeWorkClassProfiles::default()
                },
                stage_durations: PreviewDecodeStageDurations {
                    packet_decode_us: 75_000,
                    ..PreviewDecodeStageDurations::default()
                },
                max_frame_stage_durations: PreviewDecodeStageDurations {
                    packet_decode_us: 75_000,
                    ..PreviewDecodeStageDurations::default()
                },
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-random-access-hard-failure-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    let failures = preview_decode_hard_failures(&report);
    assert!(failures.contains(&"preview_decode_report_failed"));
    assert!(failures
        .contains(&"preview_decode_random_access_still_reused_other_max_worker_execution_us"));
    assert!(failures.contains(&"preview_decode_work_class_over_budget"));
}

#[test]
fn preview_render_hard_failures_include_checks_and_root_causes() {
    let diagnostics = PreviewDiagnostics {
        render_timed_frames: 1,
        render_total_duration_us: 90_000,
        render_max_duration_us: 90_000,
        render_last_duration_us: 90_000,
        render_stage_durations: crate::app::preview_runtime::PreviewRenderStageDurations {
            cpu_output_boundary_us: 80_000,
            ..crate::app::preview_runtime::PreviewRenderStageDurations::default()
        },
        render_max_frame_stage_durations:
            crate::app::preview_runtime::PreviewRenderStageDurations {
                cpu_output_boundary_us: 80_000,
                ..crate::app::preview_runtime::PreviewRenderStageDurations::default()
            },
        ..PreviewDiagnostics::default()
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
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            random_access_still: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
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
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_frames: 2,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                bounded_any_seek_strategy_frames: 1,
                any_seek_window_ms_max: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            random_access_still: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
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
fn preview_media_decode_access_mode_coverage_accepts_mode_local_ring_samples() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_cache_hit_frames: 1,
        decode_playback_session_ring_hit_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                cache_hit_frames: 1,
                playback_session_ring_hit_frames: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-access-mode-local-ring-coverage-test",
        50_000,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    );

    assert!(preview_decode_required_access_mode_failures(&report).is_empty());
}

#[test]
fn preview_decode_access_mode_queue_wait_failures_are_scoped_by_mode() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_frames: 2,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                queue_wait_total_us: 70_000,
                queue_wait_max_us: 70_000,
                queue_wait_last_us: 70_000,
                bounded_any_seek_strategy_frames: 1,
                any_seek_window_ms_max: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            random_access_still: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                queue_wait_total_us: 10_000,
                queue_wait_max_us: 10_000,
                queue_wait_last_us: 10_000,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
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
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            random_access_still: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
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
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_current_queue_wait_max_us: 85_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                queue_wait_total_us: 85_000,
                queue_wait_max_us: 85_000,
                queue_wait_last_us: 85_000,
                session_reused_frames: 1,
                work_classes: PreviewDecodeWorkClassProfiles {
                    reused_other: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_10ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                        ..PreviewDecodeWorkLatencyProfile::default()
                    },
                    ..PreviewDecodeWorkClassProfiles::default()
                },
                ..PreviewDecodeAccessModeProfile::default()
            },
            scrub_cursor: PreviewDecodeAccessModeProfile {
                queue_wait_total_us: 90_000,
                queue_wait_max_us: 90_000,
                queue_wait_last_us: 90_000,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
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
fn preview_playback_decode_failures_ignore_prefetch_only_queue_residency() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_queue_wait_max_us: 850_000,
        decode_prefetch_queue_wait_max_us: 850_000,
        decode_current_queue_wait_max_us: 84,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                frames: 1,
                in_process_cpu_frames: 1,
                queue_wait_total_us: 850_000,
                queue_wait_max_us: 850_000,
                queue_wait_last_us: 850_000,
                session_reused_frames: 1,
                work_classes: PreviewDecodeWorkClassProfiles {
                    reused_other: PreviewDecodeWorkLatencyProfile {
                        frames: 1,
                        latency_buckets: PreviewDecodeWorkLatencyBuckets {
                            le_10ms: 1,
                            ..PreviewDecodeWorkLatencyBuckets::default()
                        },
                        ..PreviewDecodeWorkLatencyProfile::default()
                    },
                    ..PreviewDecodeWorkClassProfiles::default()
                },
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-prefetch-queue-test",
        50_000,
    );

    let failures = preview_playback_decode_failures(&report);

    assert!(!failures.contains(&"preview_decode_playback_cursor_queue_wait_over_budget"));
}

#[test]
fn preview_playback_decode_failures_include_sustained_pressure() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_perf_decode_success_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
                    session_reused_frames: 1,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        reused_other: PreviewDecodeWorkLatencyProfile {
                            frames: 1,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_10ms: 1,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                            ..PreviewDecodeWorkLatencyProfile::default()
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        playback_schedule: crate::app::preview_runtime::PreviewPlaybackScheduleDiagnostics {
            sustained_pressure_active: true,
            sustained_pressure_events: 1,
            current_late_streak: 2,
            ..crate::app::preview_runtime::PreviewPlaybackScheduleDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
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
    let diagnostics = PreviewDiagnostics {
        decode_successes: 2,
        decode_in_process_cpu_frames: 2,
        decode_total_duration_us: 80_000,
        decode_max_duration_us: 45_000,
        decode_last_duration_us: 35_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_perf_decode_success_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 2,
                    in_process_cpu_frames: 2,
                    total_duration_us: 80_000,
                    max_duration_us: 45_000,
                    last_duration_us: 35_000,
                    seeked_frames: 2,
                    session_opened_frames: 2,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        session_opened: PreviewDecodeWorkLatencyProfile {
                            frames: 2,
                            total_duration_us: 80_000,
                            max_duration_us: 45_000,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_40ms: 1,
                                le_50ms: 1,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
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
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
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
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
        decode_total_duration_us: 12_000,
        decode_max_duration_us: 12_000,
        decode_last_duration_us: 12_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            scrub_cursor: test_complete_perf_decode_success_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
                    total_duration_us: 12_000,
                    max_duration_us: 12_000,
                    last_duration_us: 12_000,
                    session_opened_frames: 1,
                    bounded_any_seek_strategy_frames: 1,
                    any_seek_window_ms_max: 1,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        session_opened: PreviewDecodeWorkLatencyProfile {
                            frames: 1,
                            total_duration_us: 12_000,
                            max_duration_us: 12_000,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_16ms: 1,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(50_000),
        "preview-non-playback-warn-test",
        50_000,
    );

    assert_eq!(report.verdict, PreviewDecodePerformanceVerdict::Warn);
    assert!(preview_playback_decode_failures(&report).is_empty());
}

#[test]
fn preview_playback_decode_failures_ignore_slow_random_still_startup() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 5,
        decode_in_process_cpu_frames: 5,
        decode_total_duration_us: 160_000,
        decode_max_duration_us: 120_000,
        decode_last_duration_us: 10_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_perf_decode_success_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 4,
                    in_process_cpu_frames: 4,
                    total_duration_us: 40_000,
                    max_duration_us: 10_000,
                    last_duration_us: 10_000,
                    session_reused_frames: 4,
                    forward_reused_frames: 4,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        forward_steady: PreviewDecodeWorkLatencyProfile {
                            frames: 4,
                            total_duration_us: 40_000,
                            max_duration_us: 10_000,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_10ms: 4,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            random_access_still: test_complete_perf_decode_success_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 1,
                    in_process_cpu_frames: 1,
                    total_duration_us: 120_000,
                    max_duration_us: 120_000,
                    last_duration_us: 120_000,
                    session_opened_frames: 1,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        session_opened: PreviewDecodeWorkLatencyProfile {
                            frames: 1,
                            total_duration_us: 120_000,
                            max_duration_us: 120_000,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_120ms: 1,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-scenario-scope-test",
        50_000,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    );

    assert_ne!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(preview_playback_decode_failures(&report).is_empty());
}

#[test]
fn preview_playback_decode_failures_ignore_one_session_open_tail_when_p95_is_healthy() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 101,
        decode_in_process_cpu_frames: 101,
        decode_total_duration_us: 1_120_000,
        decode_max_duration_us: 120_000,
        decode_last_duration_us: 10_000,
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: test_complete_perf_decode_success_evidence(
                PreviewDecodeAccessModeProfile {
                    frames: 101,
                    in_process_cpu_frames: 101,
                    total_duration_us: 1_120_000,
                    max_duration_us: 120_000,
                    last_duration_us: 10_000,
                    latency_buckets: PreviewDecodeLatencyBuckets {
                        le_10ms: 100,
                        gt_80ms: 1,
                        ..PreviewDecodeLatencyBuckets::default()
                    },
                    session_opened_frames: 1,
                    session_reused_frames: 100,
                    forward_reused_frames: 100,
                    work_classes: PreviewDecodeWorkClassProfiles {
                        session_opened: PreviewDecodeWorkLatencyProfile {
                            frames: 1,
                            total_duration_us: 120_000,
                            max_duration_us: 120_000,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_120ms: 1,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                        },
                        forward_steady: PreviewDecodeWorkLatencyProfile {
                            frames: 100,
                            total_duration_us: 1_000_000,
                            max_duration_us: 10_000,
                            latency_buckets: PreviewDecodeWorkLatencyBuckets {
                                le_10ms: 100,
                                ..PreviewDecodeWorkLatencyBuckets::default()
                            },
                        },
                        ..PreviewDecodeWorkClassProfiles::default()
                    },
                    ..PreviewDecodeAccessModeProfile::default()
                },
            ),
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };
    let report = build_preview_decode_performance_report_with_required_access_modes(
        diagnostics.decode_performance_summary(50_000),
        "preview-playback-session-open-tail-test",
        50_000,
        &[PreviewDecodeAccessMode::PlaybackCursor],
    );

    assert_ne!(report.verdict, PreviewDecodePerformanceVerdict::Fail);
    assert!(preview_playback_decode_failures(&report).is_empty());
}

#[test]
fn preview_perf_report_serializes_color_report() {
    let diagnostics = PreviewDiagnostics {
        decode_successes: 1,
        decode_in_process_cpu_frames: 1,
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
        render_stage_durations: crate::app::preview_runtime::PreviewRenderStageDurations {
            resolve_us: 2_000,
            final_cache_lookup_us: 100,
            working_prepare_us: 5_000,
            cpu_composite_us: 20_000,
            cpu_output_boundary_us: 60_000,
            frame_packaging_us: 2_900,
        },
        render_max_frame_stage_durations:
            crate::app::preview_runtime::PreviewRenderStageDurations {
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
        ..PreviewDiagnostics::default()
    };
    let preview_decode_report = build_preview_decode_performance_report(
        diagnostics.decode_performance_summary(PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US),
        "preview-color-health-test",
        PREVIEW_DECODE_DEFAULT_SLOW_FRAME_BUDGET_US,
    );
    let preview_render_report = build_preview_render_performance_report(
        diagnostics.render_performance_summary(PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US),
        "preview-color-health-test",
        PREVIEW_RENDER_DEFAULT_SLOW_FRAME_BUDGET_US,
    );
    let decode_failure_codes = preview_decode_hard_failures(&preview_decode_report);
    let render_failure_codes = preview_render_hard_failures(&preview_render_report);
    let report = PreviewMediaPerfReport {
        scenario: "preview-color-health-test",
        frames: 1,
        cache_iterations: 1,
        headless_gpu: HeadlessViewerGpuExecutionSummary::default(),
        media_color_issues: VideoColorDiagnosticIssueAggregate {
            diagnostics: 1,
            diagnostics_with_executable_color_space: 1,
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
        preview_render_report: Some(preview_render_report),
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
        Some(PreviewColorHealthSummary {
            policy_rejections: 1,
            gpu_blockers: 2,
            transfer_stages: 3,
            legacy_reason_total: 4,
            fully_float_linear: false,
            gpu_path_ready: false,
            ..PreviewColorHealthSummary::default()
        }),
        "preview-ci",
    );

    assert_eq!(report.verdict, PreviewColorHealthVerdict::Fail);
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
    assert_eq!(missing.verdict, PreviewColorHealthVerdict::Fail);
    assert!(missing
        .root_causes
        .iter()
        .any(|root| root.code == "missing_preview_color_evidence"));
}

#[test]
fn app_ui_scale_color_gate_rejects_each_failed_report() {
    let pass = build_preview_color_health_report(
        Some(PreviewColorHealthSummary {
            fully_float_linear: true,
            gpu_path_ready: true,
            ..PreviewColorHealthSummary::default()
        }),
        "app-ui-pass",
    );
    let missing = build_preview_color_health_report(None, "app-ui-missing");
    let invalid = build_preview_color_health_report(
        Some(PreviewColorHealthSummary {
            policy_rejections: 1,
            fully_float_linear: true,
            gpu_path_ready: true,
            ..PreviewColorHealthSummary::default()
        }),
        "app-ui-invalid",
    );

    assert!(app_ui_scale_color_gate_failures(&pass, &pass).is_empty());
    assert_eq!(
        app_ui_scale_color_gate_failures(&missing, &pass),
        ["preview"]
    );
    assert_eq!(
        app_ui_scale_color_gate_failures(&pass, &invalid),
        ["playback"]
    );
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
    let readiness = PreviewReadinessCounts {
        ready: 10,
        stale: 4,
        loading: 2,
        unavailable: 4,
        ..PreviewReadinessCounts::default()
    };
    let report = preview_decode_report_with_playback_p95(80_000, 12_000);

    let evidence = PlaybackEvidenceCollector::default().report();
    let diagnostics = PreviewDiagnostics {
        decode_current_queue_wait_max_us: 12_000,
        ..PreviewDiagnostics::default()
    };
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
            "playback_current_queue_wait",
            "visible_frame_ratio",
            "current_ready_ratio"
        ]
    );
}

#[test]
fn external_playback_gates_pass_when_real_media_thresholds_hold() {
    let readiness = PreviewReadinessCounts {
        ready: 18,
        stale: 1,
        loading: 1,
        ..PreviewReadinessCounts::default()
    };
    let report = preview_decode_report_with_playback_p95(25_000, 4_000);

    let evidence = PlaybackEvidenceCollector::default().report();
    let diagnostics = PreviewDiagnostics::default();
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
fn external_playback_gates_classify_prepared_successor_executions_separately_from_presentation() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = PreviewDiagnostics::default();
    let evidence = PlaybackEvidenceCollector::default().report();
    let mut gpu = passing_headless_gpu_summary(20);
    gpu.published_rendered_frames = 1;
    gpu.prepared_successor_frames = 19;

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

    assert!(gates.passed);
    assert!(gates.failures.is_empty());
    assert_eq!(gates.gpu_published_rendered_frames, 1);
    assert_eq!(gates.gpu_prepared_successor_frames, 19);
    assert_eq!(gates.gpu_presented_unique_frame_completions, 20);
}

#[test]
fn external_playback_gates_fail_closed_when_decode_or_queue_p95_evidence_is_missing() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let diagnostics = PreviewDiagnostics::default();
    let evidence = PlaybackEvidenceCollector::default().report();
    let gpu = passing_headless_gpu_summary(20);

    for (missing_code, expected_failure) in [
        (
            "preview_decode_playback_cursor_forward_steady_p95_worker_execution_us",
            "playback_decode_p95",
        ),
        (
            "preview_decode_playback_cursor_queue_wait_p95_us",
            "playback_current_queue_wait",
        ),
    ] {
        let mut report = preview_decode_report_with_playback_p95(25_000, 4_000);
        report.checks.retain(|check| check.code != missing_code);
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

        assert!(!gates.passed, "missing check {missing_code} passed");
        assert!(gates.failures.contains(&expected_failure));
    }
}

#[test]
fn external_playback_gates_require_observed_gpu_completion_for_every_render() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = PreviewDiagnostics::default();
    let evidence = PlaybackEvidenceCollector::default().report();
    let mut gpu = passing_headless_gpu_summary(20);
    gpu.gpu_completion_observed_frames = 19;

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

    assert_eq!(gates.failures, vec!["viewer_gpu_completion_coverage"]);
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_do_not_treat_repeated_stale_frames_as_current_ready() {
    let readiness = PreviewReadinessCounts {
        ready: 2,
        stale: 18,
        ..PreviewReadinessCounts::default()
    };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = PreviewDiagnostics::default();
    let evidence = PlaybackEvidenceCollector::default().report();
    let mut gpu = passing_headless_gpu_summary(20);
    gpu.presented_demand_completions = 2;
    gpu.presented_unique_frame_completions = 2;

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
    assert_eq!(
        gates.failures,
        vec!["current_ready_ratio", "viewer_gpu_publication_coverage"]
    );
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_use_exact_presentation_over_transient_polling_and_supersession() {
    let readiness = PreviewReadinessCounts {
        ready: 19,
        stale: 1,
        ..PreviewReadinessCounts::default()
    };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = PreviewDiagnostics::default();
    let mut evidence = PlaybackEvidenceCollector::default().report();
    evidence.demand_count = 20;
    evidence.deliveries.ready = 18;
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
        9_950,
    );

    assert!(gates.passed);
    assert_eq!(gates.ready_frames, 20);
    assert!(gates.failures.is_empty());
}

#[test]
fn external_playback_gates_do_not_treat_released_gpu_work_as_published() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = PreviewDiagnostics::default();
    let evidence = PlaybackEvidenceCollector::default().report();
    let mut gpu = passing_headless_gpu_summary(20);
    gpu.published_rendered_frames = 0;
    gpu.released_rendered_frames = 20;
    gpu.published_output_observations = 0;
    gpu.presented_demand_completions = 0;
    gpu.presented_unique_frame_completions = 0;

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

    assert_eq!(gates.failures, vec!["viewer_gpu_publication_coverage"]);
    assert_eq!(gates.gpu_rendered_frames, 20);
    assert_eq!(gates.gpu_released_rendered_frames, 20);
    assert_eq!(gates.gpu_presented_demand_completions, 0);
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_require_unique_frame_publication_coverage() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = PreviewDiagnostics::default();
    let evidence = PlaybackEvidenceCollector::default().report();
    let mut gpu = passing_headless_gpu_summary(20);
    gpu.presented_demand_completions = 40;
    gpu.presented_unique_frame_completions = 1;

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

    assert_eq!(gates.gpu_presented_demand_completions, 40);
    assert_eq!(gates.gpu_presented_unique_frame_completions, 1);
    assert_eq!(gates.failures, vec!["viewer_gpu_publication_coverage"]);
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_fail_closed_on_temporal_mismatch_or_clock_skip() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = PreviewDiagnostics {
        decode_access_mode_profiles: PreviewDecodeAccessModeProfiles {
            playback_cursor: PreviewDecodeAccessModeProfile {
                temporal_approximation_frames: 1,
                temporal_mismatch_failures: 1,
                ..PreviewDecodeAccessModeProfile::default()
            },
            ..PreviewDecodeAccessModeProfiles::default()
        },
        ..PreviewDiagnostics::default()
    };
    let mut evidence = PlaybackEvidenceCollector::default().report();
    evidence.clock_frame_advances.advanced_frames = 21;
    evidence.clock_frame_advances.skipped_intermediate_frames = 1;
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
            "playback_temporal_approximation",
            "playback_temporal_mismatch",
            "clock_skipped_intermediate_frames"
        ]
    );
    assert_eq!(gates.playback_temporal_approximation_frames, 1);
    assert_eq!(gates.playback_temporal_mismatch_failures, 1);
    assert_eq!(gates.clock_skipped_intermediate_frames, 1);
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_require_real_gpu_execution_without_readback_or_blockers() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let diagnostics = PreviewDiagnostics::default();
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
            "viewer_gpu_execution_missing",
            "viewer_gpu_publication_coverage",
            "viewer_gpu_readback",
            "viewer_gpu_blockers",
            "viewer_gpu_execution_p95"
        ]
    );
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_fail_on_sustained_phase_or_audio_but_allow_bounded_eviction() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let mut evidence = PlaybackEvidenceCollector::default().report();
    evidence.delivery_phase_error.audio_device.proven_error.p95_us = 25_000;
    evidence.audio_underrun_recoveries = 1;
    evidence.evicted_event_count = 1;

    let diagnostics = PreviewDiagnostics::default();
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
        vec!["delivery_phase_error", "audio_underrun_recovery"]
    );
    assert!(!gates.passed);
}

#[test]
fn external_playback_gates_fail_on_cpu_frame_store_budget_or_admission() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let evidence = PlaybackEvidenceCollector::default().report();
    let diagnostics = PreviewDiagnostics {
        frame_store: mondrian_playback::PreviewFrameStoreDiagnostics {
            media_reserved_bytes: 101,
            media_byte_budget: 100,
            media_aggregate_reserved_bytes: 120,
            media_aggregate_byte_high_water: 120,
            current_media_working_set_byte_limit: 100,
            media_aggregate_hard_byte_limit: 100,
            viewer_reserved_bytes: 80,
            viewer_byte_budget: 100,
            pinned_viewer_bytes: 120,
            media_oversize_rejections: 1,
            ..mondrian_playback::PreviewFrameStoreDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
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

#[test]
fn external_playback_gates_allow_bounded_active_prefetch_reservations() {
    let readiness = PreviewReadinessCounts { ready: 20, ..PreviewReadinessCounts::default() };
    let decode = preview_decode_report_with_playback_p95(25_000, 4_000);
    let evidence = PlaybackEvidenceCollector::default().report();
    let diagnostics = PreviewDiagnostics {
        frame_store: mondrian_playback::PreviewFrameStoreDiagnostics {
            media_work_reservations: 4,
            media_prefetch_work_reservations: 4,
            media_work_reserved_bytes: 40_000_000,
            media_aggregate_reserved_bytes: 390_000_000,
            media_aggregate_byte_high_water: 390_000_000,
            media_byte_budget: 402_653_184,
            media_aggregate_hard_byte_limit: 1_073_741_824,
            current_media_working_set_byte_limit: 1_073_741_824,
            viewer_byte_budget: 201_326_592,
            ..mondrian_playback::PreviewFrameStoreDiagnostics::default()
        },
        ..PreviewDiagnostics::default()
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

    assert!(gates.cpu_frame_store_within_budget);
    assert!(!gates.failures.contains(&"cpu_frame_store_budget"));
}

fn passing_headless_gpu_summary(frames: usize) -> HeadlessViewerGpuExecutionSummary {
    HeadlessViewerGpuExecutionSummary {
        rendered_frames: frames,
        gpu_completion_observed_frames: frames,
        published_rendered_frames: frames,
        published_output_observations: frames,
        presented_demand_completions: frames,
        presented_unique_frame_completions: frames,
        wall_duration_samples_us: vec![1_000; frames],
        record_submit_samples_us: vec![400; frames],
        completion_wait_samples_us: vec![600; frames],
        gpu_duration_samples_us: vec![500; frames],
        recorded_gpu_timestamp_tokens: (0..frames as u64).collect(),
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
) -> PreviewDecodePerformanceReport {
    PreviewDecodePerformanceReport {
        schema_version: PREVIEW_DECODE_PERFORMANCE_REPORT_SCHEMA_VERSION,
        profile: "test".to_owned(),
        verdict: PreviewDecodePerformanceVerdict::Pass,
        required_access_modes: vec![PreviewDecodeAccessMode::PlaybackCursor],
        policy: PreviewDecodePerformancePolicy {
            work_budgets: Vec::new(),
            queue_wait_budget_us: 10_000,
            session_churn_grace_frames: 2,
            max_session_churn_basis_points: 2_500,
        },
        summary: None,
        checks: vec![
            PreviewDecodePerformanceCheck {
                area: PreviewDecodePerformanceArea::AccessMode,
                code: "preview_decode_playback_cursor_forward_steady_p95_worker_execution_us",
                severity: PreviewDecodePerformanceSeverity::Pass,
                observed: playback_decode_p95_us,
                limit: Some(40_000),
            },
            PreviewDecodePerformanceCheck {
                area: PreviewDecodePerformanceArea::AccessMode,
                code: "preview_decode_playback_cursor_queue_wait_p95_us",
                severity: PreviewDecodePerformanceSeverity::Pass,
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
