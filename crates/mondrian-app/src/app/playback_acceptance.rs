//! Fail-closed professional playback acceptance contracts.
//!
//! The real perf/headless Adapter supplies probed media facts plus presentation-
//! bound decode provenance. This Module owns the stable acceptance rules and
//! structured failures; capability probes and speculative prefetch aggregates
//! are diagnostic context only.

use anyhow::Context;
use mondrian_core::types::Rational;
use mondrian_media::info::{PixelFormat, VideoCodec};
use mondrian_media::{MediaInfo, VideoCodecProfile};
use serde::Serialize;

mod isolated_demux;
use isolated_demux::{evaluate_isolated_demux, PreviewIsolatedDemuxGateEvidence};

const PROCESS_MEMORY_WARMUP_END_US: u64 = 5 * 60 * 1_000_000;
const PROCESS_MEMORY_BASELINE_END_US: u64 = 10 * 60 * 1_000_000;
const PROCESS_MEMORY_FINAL_WINDOW_START_US: u64 = 25 * 60 * 1_000_000;
const PROCESS_MEMORY_MIN_WINDOW_SAMPLES: u64 = 240;
const PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES: u64 = 256 * 1024 * 1024;

use super::preview_access_mode::{
    MediaPreviewJobQueueDiagnostics, MediaPreviewSchedulerDiagnostics,
};
use super::preview_runtime::PreviewDecodeWorkerExecutionDiagnostics;

pub(crate) const PROFESSIONAL_MIN_OBSERVED_DURATION_US: u64 = 30 * 60 * 1_000_000;
pub(crate) const PROFESSIONAL_MIN_WARM_SEEKS: u64 = 50;
pub(crate) const PROFESSIONAL_MIN_ACCURATE_SEEKS: u64 = 50;
pub(crate) const PROFESSIONAL_MIN_SUPERSEDED_SEEKS: u64 = 99;
pub(crate) const PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT: usize = 90;
pub(crate) const PROFESSIONAL_PLAYBACK_DECODE_P95_LIMIT_US: u64 = 40_000;
pub(crate) const PROFESSIONAL_PLAYBACK_QUEUE_WAIT_P95_LIMIT_US: u64 = 10_000;
pub(crate) const PROFESSIONAL_MIN_VISIBLE_PERCENT: usize = 95;
pub(crate) const PROFESSIONAL_MIN_READY_BASIS_POINTS: usize = 9_950;
pub(crate) const PROFESSIONAL_GPU_CANDIDATE_LIMIT_MS: u128 = 2_000;
pub(crate) const PROFESSIONAL_READY_TIMEOUT_MS: u64 = 30_000;
pub(crate) const PROFESSIONAL_TOTAL_TIMEOUT_MS: u64 = 40 * 60 * 1_000;
const PROFESSIONAL_WARM_SEEK_P95_LIMIT_US: u64 = 200_000;
const PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US: u64 = 500_000;
const PROFESSIONAL_FRAME_RATES: [Rational; 8] = [
    Rational::FPS_23976,
    Rational::FPS_24,
    Rational::FPS_25,
    Rational::FPS_2997,
    Rational::FPS_30,
    Rational::FPS_50,
    Rational::FPS_5994,
    Rational::FPS_60,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PreviewPlaybackMediaProbeReport {
    source: &'static str,
    codec: VideoCodec,
    codec_profile: VideoCodecProfile,
    width: u32,
    height: u32,
    frame_rate: Rational,
    frame_rate_proven: bool,
    pixel_format: PixelFormat,
    pixel_format_proven: bool,
    bit_depth: u8,
    video_stream_duration_us: Option<u64>,
    duration_us: u64,
    total_frames: Option<u64>,
}

impl PreviewPlaybackMediaProbeReport {
    pub(crate) fn from_media_info(media_info: &MediaInfo) -> anyhow::Result<Self> {
        let video =
            media_info.primary_video().context("FFmpeg media probe found no video stream")?;
        anyhow::ensure!(
            video.width > 0 && video.height > 0,
            "FFmpeg media probe did not resolve a positive video extent"
        );
        anyhow::ensure!(
            video.frame_rate.num > 0 && video.frame_rate.den > 0,
            "FFmpeg media probe did not resolve a positive average frame rate"
        );
        Ok(Self {
            source: "ffmpeg_avformat_decoder_probe",
            codec: video.codec.clone(),
            codec_profile: video.codec_profile,
            width: video.width,
            height: video.height,
            frame_rate: video.frame_rate.reduce(),
            frame_rate_proven: video.frame_rate_proven,
            pixel_format: video.pixel_format,
            pixel_format_proven: video.pixel_format_proven,
            bit_depth: video.bit_depth,
            video_stream_duration_us: video
                .duration
                .map(|duration| duration.as_micros().min(u64::MAX as u128) as u64),
            duration_us: media_info.duration.as_micros().min(u64::MAX as u128) as u64,
            total_frames: video.total_frames,
        })
    }

    pub(crate) fn frame_interval_ns(&self) -> anyhow::Result<u64> {
        anyhow::ensure!(
            self.frame_rate.num > 0 && self.frame_rate.den > 0,
            "probed video frame rate must be positive"
        );
        let numerator = 1_000_000_000u128.saturating_mul(self.frame_rate.den as u128);
        let denominator = self.frame_rate.num as u128;
        Ok(numerator
            .saturating_add(denominator / 2)
            .checked_div(denominator)
            .unwrap_or(u128::MAX)
            .min(u64::MAX as u128) as u64)
    }

    /// Reject a fixture that cannot cover the requested observation window.
    pub(crate) fn ensure_observation_coverage(
        &self,
        frames: usize,
        frame_interval_ns: u64,
    ) -> anyhow::Result<()> {
        let required_duration_us = required_media_duration_us(frames, frame_interval_ns);
        let video_stream_duration_us = self
            .video_stream_duration_us
            .context("professional playback requires a proven primary-video stream duration")?;
        anyhow::ensure!(
            video_stream_duration_us >= required_duration_us,
            "professional playback requires at least {required_duration_us} us of primary-video stream duration, but the probe provides {video_stream_duration_us} us"
        );
        anyhow::ensure!(
            self.duration_us >= required_duration_us,
            "professional playback observation requires at least {required_duration_us} us of source media, but the probe provides {} us",
            self.duration_us
        );
        if let Some(total_frames) = self.total_frames {
            anyhow::ensure!(
                total_frames >= frames as u64,
                "professional playback requires at least {frames} declared primary-video frames, but the probe provides {total_frames}"
            );
        }
        Ok(())
    }
}

fn required_media_duration_us(frames: usize, frame_interval_ns: u64) -> u64 {
    (frames as u128)
        .saturating_mul(frame_interval_ns as u128)
        .saturating_add(999)
        .checked_div(1_000)
        .unwrap_or(u128::MAX)
        .min(u64::MAX as u128) as u64
}

fn evaluate_main10_media_contract(
    media: &PreviewPlaybackMediaProbeReport,
    required_frames: usize,
    frame_interval_ns: u64,
    failures: &mut Vec<PreviewAcceptanceFailure>,
) {
    if media.codec != VideoCodec::H265 {
        push_failure(
            failures,
            "media_codec_mismatch",
            "HEVC/H.265",
            format!("{:?}", media.codec),
            media.source,
        );
    }
    if media.codec_profile != VideoCodecProfile::HevcMain10 {
        push_failure(
            failures,
            if media.codec_profile == VideoCodecProfile::Unknown {
                "media_codec_profile_unproven"
            } else {
                "media_codec_profile_mismatch"
            },
            "HEVC Main 10",
            format!("{:?}", media.codec_profile),
            media.source,
        );
    }
    if media.width < 3_840 || media.height < 2_160 {
        push_failure(
            failures,
            "media_resolution_mismatch",
            "at least 3840x2160",
            format!("{}x{}", media.width, media.height),
            media.source,
        );
    }
    if !media.frame_rate_proven {
        push_failure(
            failures,
            "media_frame_rate_unknown",
            "positive FFmpeg average frame-rate evidence",
            media.frame_rate.to_string(),
            media.source,
        );
    } else if !PROFESSIONAL_FRAME_RATES.contains(&media.frame_rate) {
        push_failure(
            failures,
            "media_frame_rate_mismatch",
            "24000/1001, 24, 25, 30000/1001, 30, 50, 60000/1001, or 60 fps",
            media.frame_rate.to_string(),
            media.source,
        );
    }
    if !media.pixel_format_proven {
        push_failure(
            failures,
            "media_pixel_format_unknown",
            "decoder-proven 10-bit pixel format",
            format!("fallback={:?}", media.pixel_format),
            media.source,
        );
    }
    if media.bit_depth < 10 {
        push_failure(
            failures,
            "media_bit_depth_mismatch",
            "at least 10 bit",
            media.bit_depth.to_string(),
            media.source,
        );
    }
    let required_duration_us = required_media_duration_us(required_frames, frame_interval_ns);
    if media.duration_us < required_duration_us {
        push_failure(
            failures,
            "media_duration_insufficient",
            format!("at least {required_duration_us} us"),
            format!("{} us", media.duration_us),
            media.source,
        );
    }
    match media.video_stream_duration_us {
        None => push_failure(
            failures,
            "media_video_stream_duration_unproven",
            format!("at least {required_duration_us} us of primary-video stream duration"),
            "unknown",
            media.source,
        ),
        Some(duration_us) if duration_us < required_duration_us => push_failure(
            failures,
            "media_video_stream_duration_insufficient",
            format!("at least {required_duration_us} us"),
            format!("{duration_us} us"),
            media.source,
        ),
        Some(_) => {}
    }
    if let Some(total_frames) = media.total_frames {
        if total_frames < required_frames as u64 {
            push_failure(
                failures,
                "media_video_frame_count_insufficient",
                format!("at least {required_frames} frames"),
                format!("{total_frames} frames"),
                media.source,
            );
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PreviewProfessionalPlaybackGateReport {
    profile: &'static str,
    required_hardware_execution_percent: usize,
    presented_media_layers: u64,
    presented_hardware_layers: u64,
    presented_hardware_cpu_transfer_layers: u64,
    presented_hardware_native_layers: u64,
    presented_p010_10_bit_hardware_layers: u64,
    hardware_execution_percent: u64,
    hardware_requested_frames: u64,
    fallback_cpu_not_requested_frames: u64,
    fallback_cpu_unavailable_frames: u64,
    fallback_backend_unavailable_frames: u64,
    fallback_codec_unsupported_frames: u64,
    fallback_device_context_unavailable_frames: u64,
    fallback_setup_failed_frames: u64,
    fallback_decoder_open_failed_frames: u64,
    fallback_awaiting_hardware_frame_frames: u64,
    fallback_backend_boundary_frames: u64,
    fallback_adapter_unavailable_frames: u64,
    observed_duration_limit_us: u64,
    observed_duration_us: u64,
    wall_duration_us: u64,
    min_warm_seeks: u64,
    warm_seek_count: u64,
    warm_seek_p95_limit_us: u64,
    warm_seek_p95_observed_us: u64,
    min_accurate_seeks: u64,
    accurate_seek_count: u64,
    accurate_seek_p95_limit_us: u64,
    accurate_seek_p95_observed_us: u64,
    accurate_seek_temporal_approximation_frames: u64,
    min_superseded_seeks: u64,
    superseded_seek_count: u64,
    rejected_terminal_deliveries: u64,
    evicted_detailed_events: u64,
    broker_pending_requests: usize,
    broker_queued_jobs: usize,
    broker_in_flight_jobs: usize,
    broker_clock_regressions: u64,
    cpu_frame_store_within_budget: bool,
    cpu_frame_store_oversize_rejections: u64,
    process_memory: PreviewProcessMemoryGateReport,
    cancellation_gate: mondrian_playback::FrameCancellationGateReport,
    decode_cancellation_checkpoints: mondrian_media::PreviewDecodeCancellationEvidence,
    isolated_demux: PreviewIsolatedDemuxGateEvidence,
    decode_worker_execution: PreviewDecodeWorkerExecutionDiagnostics,
    native_video_gpu_timing: ProfessionalNativeVideoGpuTimingEvidence,
    pub(crate) passed: bool,
    pub(crate) failures: Vec<PreviewAcceptanceFailure>,
}

/// Real-media proof that superseded Preview work reached an executing demux
/// call, was canceled through the production lifecycle, and recovered to the
/// latest requested frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct PreviewCancellationRecoveryEvidence {
    pub(crate) stage_before_supersession: mondrian_media::PreviewDecodeExecutionStage,
    pub(crate) superseded_target_frame: usize,
    pub(crate) recovery_target_frame: usize,
    pub(crate) broker_cancellation_delta: u64,
    pub(crate) media_cancellation_checkpoint_delta: u64,
    pub(crate) isolated_termination_delta: u64,
    pub(crate) isolated_checkpoint_delta: u64,
    pub(crate) recovery_presented: bool,
}

/// Short real-media qualification report for the production demux/cancel/
/// recovery seam. It deliberately does not claim long-run cadence or memory
/// stability; those remain obligations of the professional playback gate.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PreviewPlaybackQualificationGateReport {
    profile: &'static str,
    required_source_frames: usize,
    presented: PresentedHardwareGateEvidence,
    cancellation_recovery: PreviewCancellationRecoveryEvidence,
    cancellation_gate: mondrian_playback::FrameCancellationGateReport,
    decode_cancellation_checkpoints: mondrian_media::PreviewDecodeCancellationEvidence,
    isolated_demux: PreviewIsolatedDemuxGateEvidence,
    broker_pending_requests: usize,
    broker_queued_jobs: usize,
    broker_in_flight_jobs: usize,
    broker_clock_regressions: u64,
    rejected_terminal_deliveries: u64,
    accurate_seek_temporal_approximation_frames: u64,
    cpu_frame_store_within_budget: bool,
    cpu_frame_store_oversize_rejections: u64,
    pub(crate) passed: bool,
    pub(crate) failures: Vec<PreviewAcceptanceFailure>,
}

/// Bounded, whole-product memory evidence collected by a native platform Adapter.
///
/// The collector keeps only scalar aggregates. It deliberately uses private
/// commit summed across the verified Mondrian process tree for acceptance and
/// reports reclaimable working sets as diagnostics. A complete current-process
/// sample is rejected because it cannot account for demux or FFmpeg children.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct PreviewProcessMemoryEvidenceReport {
    scope: Option<String>,
    backend: Option<String>,
    discovery_available: bool,
    inventory_complete: bool,
    attempted_samples: u64,
    observed_samples: u64,
    probe_errors: u64,
    last_probe_error: Option<String>,
    minimum_observed_process_count: Option<u32>,
    maximum_observed_process_count: u32,
    maximum_inventory_attempts: u32,
    observed_duration_us: u64,
    peak_private_committed_bytes: u64,
    peak_resident_bytes: u64,
    os_peak_resident_bytes: u64,
    baseline_sample_count: u64,
    baseline_average_private_committed_bytes: u64,
    final_sample_count: u64,
    final_average_private_committed_bytes: u64,
    post_stress_private_committed_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewProcessMemoryEvidenceCollector {
    report: PreviewProcessMemoryEvidenceReport,
    baseline_private_sum: u128,
    final_private_sum: u128,
}

impl PreviewProcessMemoryEvidenceCollector {
    pub(crate) fn observe_playback_duration(&mut self, observed_duration_us: u64) {
        self.report.observed_duration_us =
            self.report.observed_duration_us.max(observed_duration_us);
    }

    pub(crate) fn observe_playback(
        &mut self,
        observed_at_us: u64,
        sample: mondrian_platform::ProcessMemoryProbeResult,
    ) {
        self.report.observed_duration_us = self.report.observed_duration_us.max(observed_at_us);
        let Some(private_bytes) = self.observe_sample(sample) else {
            return;
        };
        if (PROCESS_MEMORY_WARMUP_END_US..PROCESS_MEMORY_BASELINE_END_US).contains(&observed_at_us)
        {
            self.report.baseline_sample_count = self.report.baseline_sample_count.saturating_add(1);
            self.baseline_private_sum =
                self.baseline_private_sum.saturating_add(u128::from(private_bytes));
        }
        if (PROCESS_MEMORY_FINAL_WINDOW_START_US..=PROFESSIONAL_MIN_OBSERVED_DURATION_US)
            .contains(&observed_at_us)
        {
            self.report.final_sample_count = self.report.final_sample_count.saturating_add(1);
            self.final_private_sum =
                self.final_private_sum.saturating_add(u128::from(private_bytes));
        }
    }

    pub(crate) fn observe_post_stress(
        &mut self,
        sample: mondrian_platform::ProcessMemoryProbeResult,
    ) {
        self.report.post_stress_private_committed_bytes = self.observe_sample(sample);
    }

    pub(crate) fn report(mut self) -> PreviewProcessMemoryEvidenceReport {
        self.report.inventory_complete = self.report.attempted_samples > 0
            && self.report.observed_samples == self.report.attempted_samples
            && self.report.probe_errors == 0;
        self.report.baseline_average_private_committed_bytes =
            average_bytes(self.baseline_private_sum, self.report.baseline_sample_count);
        self.report.final_average_private_committed_bytes =
            average_bytes(self.final_private_sum, self.report.final_sample_count);
        self.report
    }

    fn observe_sample(
        &mut self,
        sample: mondrian_platform::ProcessMemoryProbeResult,
    ) -> Option<u64> {
        self.report.attempted_samples = self.report.attempted_samples.saturating_add(1);
        self.report.discovery_available |= sample.discovery_available;
        let scope = sample.scope.as_str().to_owned();
        if self.report.scope.as_ref().is_some_and(|known| known != &scope) {
            self.report.probe_errors = self.report.probe_errors.saturating_add(1);
            self.report.last_probe_error =
                Some("process-memory scope changed during one acceptance run".to_owned());
            return None;
        }
        self.report.scope = Some(scope);
        self.report.minimum_observed_process_count = Some(
            self.report
                .minimum_observed_process_count
                .map_or(sample.observed_process_count, |known| {
                    known.min(sample.observed_process_count)
                }),
        );
        self.report.maximum_observed_process_count =
            self.report.maximum_observed_process_count.max(sample.observed_process_count);
        self.report.maximum_inventory_attempts =
            self.report.maximum_inventory_attempts.max(sample.inventory_attempts);
        if let Some(backend) = sample.backend {
            let backend = backend.as_str().to_owned();
            if self.report.backend.as_ref().is_some_and(|known| known != &backend) {
                self.report.probe_errors = self.report.probe_errors.saturating_add(1);
                self.report.last_probe_error =
                    Some("process-memory backend changed during one acceptance run".to_owned());
                return None;
            }
            self.report.backend = Some(backend);
        }
        if !sample.is_complete_for(mondrian_platform::ProcessMemoryScope::ProductProcessTree) {
            self.report.probe_errors = self.report.probe_errors.saturating_add(1);
            self.report.last_probe_error = sample.error.or_else(|| {
                Some(format!(
                    "expected complete product-process-tree sample, observed scope={}, complete={}, processes={}, attempts={}",
                    sample.scope.as_str(),
                    sample.inventory_complete,
                    sample.observed_process_count,
                    sample.inventory_attempts,
                ))
            });
            return None;
        }
        let Some(private_bytes) = sample.private_committed_bytes else {
            self.report.probe_errors = self.report.probe_errors.saturating_add(1);
            self.report.last_probe_error = sample.error.or_else(|| {
                Some("process-memory sample omitted private committed bytes".to_owned())
            });
            return None;
        };
        self.report.observed_samples = self.report.observed_samples.saturating_add(1);
        self.report.peak_private_committed_bytes =
            self.report.peak_private_committed_bytes.max(private_bytes);
        if let Some(resident_bytes) = sample.resident_bytes {
            self.report.peak_resident_bytes = self.report.peak_resident_bytes.max(resident_bytes);
        }
        if let Some(peak_resident_bytes) = sample.peak_resident_bytes {
            self.report.os_peak_resident_bytes =
                self.report.os_peak_resident_bytes.max(peak_resident_bytes);
        }
        Some(private_bytes)
    }
}

fn average_bytes(sum: u128, count: u64) -> u64 {
    if count == 0 {
        return 0;
    }
    sum.checked_div(u128::from(count))
        .unwrap_or(u128::MAX)
        .min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PreviewProcessMemoryGateReport {
    profile: &'static str,
    scope: Option<String>,
    backend: Option<String>,
    inventory_complete: bool,
    minimum_observed_process_count: Option<u32>,
    maximum_observed_process_count: u32,
    maximum_inventory_attempts: u32,
    attempted_samples: u64,
    observed_samples: u64,
    probe_errors: u64,
    observed_duration_us: u64,
    max_private_committed_bytes: u64,
    peak_private_committed_bytes: u64,
    peak_resident_bytes: u64,
    os_peak_resident_bytes: u64,
    max_settled_growth_bytes: u64,
    baseline_sample_count: u64,
    final_sample_count: u64,
    baseline_average_private_committed_bytes: u64,
    final_average_private_committed_bytes: u64,
    settled_growth_bytes: u64,
    post_stress_private_committed_bytes: Option<u64>,
    post_stress_growth_bytes: Option<u64>,
    passed: bool,
    failures: Vec<PreviewAcceptanceFailure>,
}

impl PreviewProcessMemoryGateReport {
    pub(crate) fn passed(&self) -> bool {
        self.passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PreviewAcceptanceFailure {
    code: &'static str,
    expected: String,
    observed: String,
    evidence: String,
}

/// Presentation-bound decode provenance supplied by a concrete Viewer Adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PresentedDecodeExecutionEvidence {
    pub(crate) media_layers: u32,
    pub(crate) software_cpu_layers: u32,
    pub(crate) hardware_cpu_transfer_layers: u32,
    pub(crate) hardware_native_layers: u32,
    pub(crate) p010_10_bit_hardware_layers: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct PresentedHardwareGateEvidence {
    presented_media_layers: u64,
    presented_hardware_layers: u64,
    presented_hardware_cpu_transfer_layers: u64,
    presented_hardware_native_layers: u64,
    presented_p010_10_bit_hardware_layers: u64,
    hardware_execution_percent: u64,
}

fn evaluate_presented_main10_hardware(
    execution: PresentedDecodeExecutionEvidence,
    viewer_fallback_count: usize,
    viewer_fallback_reasons: &[String],
    failures: &mut Vec<PreviewAcceptanceFailure>,
) -> PresentedHardwareGateEvidence {
    let presented_media_layers = u64::from(execution.media_layers);
    let presented_hardware_cpu_transfer_layers = u64::from(execution.hardware_cpu_transfer_layers);
    let presented_hardware_native_layers = u64::from(execution.hardware_native_layers);
    let presented_hardware_layers =
        presented_hardware_cpu_transfer_layers.saturating_add(presented_hardware_native_layers);
    let presented_p010_10_bit_hardware_layers = u64::from(execution.p010_10_bit_hardware_layers);
    let hardware_execution_percent = percent(presented_hardware_layers, presented_media_layers);
    let p010_execution_percent = percent(
        presented_p010_10_bit_hardware_layers,
        presented_media_layers,
    );
    if presented_media_layers == 0 {
        push_failure(
            failures,
            "hardware_decode_not_observed",
            "presented media layers with frame-local decode provenance",
            "0 layers",
            "headless Viewer GPU completion records",
        );
    } else if hardware_execution_percent < PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT as u64 {
        push_failure(
            failures,
            "hardware_decode_coverage_below_minimum",
            format!("at least {PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT}%"),
            format!("{hardware_execution_percent}%"),
            "presented Frame Demand candidates only; prefetch aggregates excluded",
        );
    }
    if presented_media_layers > 0
        && p010_execution_percent < PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT as u64
    {
        push_failure(
            failures,
            "hardware_main10_surface_coverage_below_minimum",
            format!(
                "at least {PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT}% P010/10-bit hardware"
            ),
            format!("{p010_execution_percent}%"),
            "frame-local decoded surface and sampling evidence",
        );
    }
    if viewer_fallback_count > 0 {
        push_failure(
            failures,
            "viewer_gpu_fallback_observed",
            "0 Viewer GPU input fallbacks",
            viewer_fallback_count.to_string(),
            viewer_fallback_reasons.join(" | "),
        );
    }

    PresentedHardwareGateEvidence {
        presented_media_layers,
        presented_hardware_layers,
        presented_hardware_cpu_transfer_layers,
        presented_hardware_native_layers,
        presented_p010_10_bit_hardware_layers,
        hardware_execution_percent,
    }
}

fn evaluate_cancellation_contract(
    evidence: mondrian_playback::FrameCancellationEvidenceReport,
    failures: &mut Vec<PreviewAcceptanceFailure>,
) -> mondrian_playback::FrameCancellationGateReport {
    let gate = mondrian_playback::evaluate_frame_cancellation(
        evidence,
        mondrian_playback::FrameCancellationPolicy::default(),
    );
    for failure in &gate.failures {
        let code = match failure.kind {
            mondrian_playback::FrameCancellationGateFailureKind::UnknownCause => {
                "frame_cancellation_unknown_cause"
            }
            mondrian_playback::FrameCancellationGateFailureKind::MissingRequestToLogicalCancellation => {
                "frame_cancellation_request_to_logical_evidence_missing"
            }
            mondrian_playback::FrameCancellationGateFailureKind::MissingExecutionToLogicalCancellation => {
                "frame_cancellation_execution_to_logical_evidence_missing"
            }
            mondrian_playback::FrameCancellationGateFailureKind::InvalidTimingOrder => {
                "frame_cancellation_timing_invalid"
            }
            mondrian_playback::FrameCancellationGateFailureKind::RequestToLogicalCancellationExceeded => {
                "frame_cancellation_logical_observation_late"
            }
            mondrian_playback::FrameCancellationGateFailureKind::LogicalCancellationToReturnExceeded => {
                "frame_cancellation_physical_return_late"
            }
        };
        push_failure(
            failures,
            code,
            format!("at most {} for {:?}", failure.limit, failure.work_class),
            failure.observed.to_string(),
            "Frame Cancellation Evidence evaluated by the playback-owned policy",
        );
    }
    gate
}

fn evaluate_cancellation_recovery(
    evidence: PreviewCancellationRecoveryEvidence,
    failures: &mut Vec<PreviewAcceptanceFailure>,
) {
    if !matches!(
        evidence.stage_before_supersession,
        mondrian_media::PreviewDecodeExecutionStage::Seek
            | mondrian_media::PreviewDecodeExecutionStage::PacketRead
    ) {
        push_failure(
            failures,
            "isolated_demux_active_call_unproven",
            "a real isolated-demux Seek or PacketRead observed before supersession",
            format!("{:?}", evidence.stage_before_supersession),
            "Preview Decode Execution Progress sampled before generation rotation",
        );
    }
    if evidence.broker_cancellation_delta == 0 {
        push_failure(
            failures,
            "cancellation_recovery_broker_evidence_missing",
            "at least one Broker-owned cancellation",
            "0",
            "Frame Cancellation Evidence delta across the real-media supersession",
        );
    }
    if evidence.media_cancellation_checkpoint_delta == 0 {
        push_failure(
            failures,
            "cancellation_recovery_media_checkpoint_missing",
            "at least one concrete media cancellation checkpoint",
            "0",
            "Preview Decode Cancellation Evidence delta across the real-media supersession",
        );
    }
    if (evidence.isolated_termination_delta == 0) != (evidence.isolated_checkpoint_delta == 0) {
        push_failure(
            failures,
            "isolated_demux_termination_checkpoint_mismatch",
            "termination and checkpoint evidence are both absent or both present",
            format!(
                "terminations={}, checkpoints={}",
                evidence.isolated_termination_delta, evidence.isolated_checkpoint_delta
            ),
            "real-media cancellation-recovery observation delta",
        );
    }
    if !evidence.recovery_presented {
        push_failure(
            failures,
            "post_cancellation_presentation_missing",
            "the latest requested Timeline frame completed Viewer GPU presentation",
            "false",
            "headless Viewer GPU completion bound to the recovery Frame Demand",
        );
    }
}

/// Playback-cursor decode request and fallback facts used by acceptance reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlaybackDecodeExecutionEvidence {
    pub(crate) hardware_decode_prefer_hardware_requested_frames: u64,
    pub(crate) hardware_decode_prefer_gpu_requested_frames: u64,
    pub(crate) hardware_decode_require_gpu_requested_frames: u64,
    pub(crate) hardware_decode_cpu_not_requested_frames: u64,
    pub(crate) hardware_decode_cpu_unavailable_frames: u64,
    pub(crate) hardware_decode_backend_unavailable_frames: u64,
    pub(crate) hardware_decode_codec_unsupported_frames: u64,
    pub(crate) hardware_decode_device_context_unavailable_frames: u64,
    pub(crate) hardware_decode_cpu_transfer_setup_failed_frames: u64,
    pub(crate) hardware_decode_cpu_transfer_decoder_open_failed_frames: u64,
    pub(crate) hardware_decode_cpu_transfer_awaiting_frame_frames: u64,
    pub(crate) hardware_decode_backend_boundary_frames: u64,
    pub(crate) hardware_decode_adapter_unavailable_frames: u64,
}

/// Renderer-owned native-video GPU timing facts reconciled to successful,
/// presentation-bound Viewer candidates for one professional playback run.
///
/// Renderer counters, candidate receipts, asynchronous samples, and
/// publication ownership remain separate evidence families. The professional
/// gate accepts them only when both accounting identities and the ownership
/// reconciliation are exact; an Adapter may not infer expected samples from
/// layer counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ProfessionalNativeVideoGpuTimingEvidence {
    /// Whether the renderer device exposed the complete timestamp capability.
    pub(crate) capability_supported: bool,
    /// Whether explicit product policy activated a healthy timing runtime.
    pub(crate) activated: bool,
    /// Stable policy, capability, or runtime-failure reason when inactive.
    pub(crate) inactive_reason: Option<String>,
    /// Native imports that returned a valid renderer working-frame output.
    pub(crate) renderer_submitted_imports: u64,
    /// Asynchronously completed renderer timing samples.
    pub(crate) renderer_samples: u64,
    /// Renderer samples still awaiting asynchronous readback.
    pub(crate) renderer_pending_samples: u64,
    /// Valid imports for which the renderer could not schedule timing.
    pub(crate) renderer_missing_samples: u64,
    /// Valid imports deliberately unsampled because the bounded ring was full.
    pub(crate) renderer_dropped_samples: u64,
    /// Unique successful candidate receipts accepted by reconciliation.
    pub(crate) receipt_candidates: u64,
    /// Valid imports closed by those exact candidate receipts.
    pub(crate) receipt_submitted_imports: u64,
    /// Receipt imports admitted to asynchronous readback.
    pub(crate) receipt_scheduled_samples: u64,
    /// Receipt imports that could not schedule a timing sample.
    pub(crate) receipt_missing_samples: u64,
    /// Receipt imports deliberately unsampled because the ring was full.
    pub(crate) receipt_dropped_samples: u64,
    /// Renderer samples drained and observed exactly once by the gate Adapter.
    pub(crate) observed_samples: u64,
    /// Completed renderer samples discarded by the Adapter's explicitly
    /// bounded observation buffer before ownership reconciliation.
    pub(crate) adapter_dropped_samples: u64,
    /// Receipt-owning native Viewer candidates that completed publication.
    pub(crate) published_native_candidates: u64,
    /// Observed samples matched to native candidates that completed publication.
    ///
    /// This may be smaller than `observed_samples`: a uniquely owned successful
    /// record may be released, superseded, or retire late without publication.
    pub(crate) published_native_samples: u64,
    /// Repeated receipt ownership attempts rejected during reconciliation.
    pub(crate) duplicate_candidate_receipts: u64,
    /// Samples repeated with the same execution-session/import identity.
    pub(crate) duplicate_samples: u64,
    /// Attempts to assign one exact sample to multiple candidate/phase owners.
    pub(crate) duplicate_sample_ownership: u64,
    /// Unique successful candidates whose observed sample count differs from
    /// that exact receipt's scheduled sample count.
    pub(crate) candidate_sample_count_mismatches: u64,
    /// Observed samples without one exact successful candidate receipt owner.
    pub(crate) unmatched_samples: u64,
    /// Receipts that could not be assigned to one unique successful
    /// candidate/phase.
    ///
    /// A uniquely owned successful record that is later released, superseded,
    /// or retired late is not orphaned merely because it was never published.
    pub(crate) orphan_candidate_receipts: u64,
}

/// UI-independent point-in-time execution facts required by professional acceptance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewRuntimeAcceptanceEvidence {
    pub(crate) scheduler: MediaPreviewSchedulerDiagnostics,
    pub(crate) worker_queue: MediaPreviewJobQueueDiagnostics,
    pub(crate) frame_store: mondrian_playback::PreviewFrameStoreDiagnostics,
    pub(crate) accurate_seek_temporal_approximation_frames: u64,
    pub(crate) decode_cancellation: mondrian_playback::FrameCancellationEvidenceReport,
    pub(crate) decode_cancellation_checkpoints: mondrian_media::PreviewDecodeCancellationEvidence,
    pub(crate) decode_worker_execution: PreviewDecodeWorkerExecutionDiagnostics,
}

pub(crate) struct ProfessionalPlaybackObservation<'a> {
    pub media: &'a PreviewPlaybackMediaProbeReport,
    pub rendered_decode_execution: PresentedDecodeExecutionEvidence,
    pub viewer_fallback_count: usize,
    pub viewer_fallback_reasons: &'a [String],
    pub playback_decode: PlaybackDecodeExecutionEvidence,
    pub playback_evidence: &'a mondrian_playback::PlaybackEvidenceReport,
    pub continuous_playback_evidence: &'a mondrian_playback::PlaybackEvidenceReport,
    pub continuous_playback_wall_duration_us: u64,
    pub preview_diagnostics: &'a PreviewRuntimeAcceptanceEvidence,
    pub process_memory: &'a PreviewProcessMemoryEvidenceReport,
    pub native_video_gpu_timing: &'a ProfessionalNativeVideoGpuTimingEvidence,
    pub frames: usize,
    pub frame_interval_ns: u64,
}

/// UI-independent observation consumed by the short production qualification.
pub(crate) struct PreviewPlaybackQualificationObservation<'a> {
    pub media: &'a PreviewPlaybackMediaProbeReport,
    pub required_source_frames: usize,
    pub frame_interval_ns: u64,
    pub rendered_decode_execution: PresentedDecodeExecutionEvidence,
    pub viewer_fallback_count: usize,
    pub viewer_fallback_reasons: &'a [String],
    pub playback_evidence: &'a mondrian_playback::PlaybackEvidenceReport,
    pub preview_diagnostics: &'a PreviewRuntimeAcceptanceEvidence,
    pub cancellation_recovery: PreviewCancellationRecoveryEvidence,
}

fn evaluate_preview_frame_store_contract(
    diagnostics: mondrian_playback::PreviewFrameStoreDiagnostics,
    failures: &mut Vec<PreviewAcceptanceFailure>,
) -> (bool, u64) {
    let within_budget = diagnostics.production_residency_contract_holds();
    if !within_budget {
        push_failure(
            failures,
            "preview_frame_store_physical_budget_exceeded",
            "optional cache within trim policy, physical aggregate within its global hard grant, each exact current demand within its per-demand grant, zero overcommit events, and zero live work leases",
            format!(
                "cache=({}/{}, {}/{}, {}/{}), overflow=({}, {}, {}), aggregate=({}/{}, {}/{}, {}/{}), aggregate_high_water=({}, {}, {}), current_demand_high_water=({}/{}, {}/{}, {}/{}), work={}, aggregate_overcommitted={}, aggregate_overcommit_events={}, current_demand_overcommitted={}, current_demand_overcommit_events={}, current_demand_rejections={}, viewer={}/{}, pinned_viewer={}",
                diagnostics.media_entries,
                diagnostics.media_entry_capacity,
                diagnostics.media_reserved_bytes,
                diagnostics.media_byte_budget,
                diagnostics.media_resource_units,
                diagnostics.media_resource_unit_budget,
                diagnostics.current_media_overflow_entries,
                diagnostics.current_media_overflow_bytes,
                diagnostics.current_media_overflow_resource_units,
                diagnostics.media_aggregate_entries,
                diagnostics.media_aggregate_hard_entry_limit,
                diagnostics.media_aggregate_reserved_bytes,
                diagnostics.media_aggregate_hard_byte_limit,
                diagnostics.media_aggregate_resource_units,
                diagnostics.media_aggregate_hard_resource_unit_limit,
                diagnostics.media_aggregate_entry_high_water,
                diagnostics.media_aggregate_byte_high_water,
                diagnostics.media_aggregate_resource_unit_high_water,
                diagnostics.media_current_working_set_entry_high_water,
                diagnostics.current_media_working_set_entry_limit,
                diagnostics.media_current_working_set_byte_high_water,
                diagnostics.current_media_working_set_byte_limit,
                diagnostics.media_current_working_set_resource_unit_high_water,
                diagnostics.current_media_working_set_resource_unit_limit,
                diagnostics.media_work_reservations,
                diagnostics.media_capacity_overcommitted,
                diagnostics.media_capacity_overcommit_events,
                diagnostics.media_current_working_set_overcommitted,
                diagnostics.media_current_working_set_overcommit_events,
                diagnostics.media_current_working_set_rejections,
                diagnostics.viewer_reserved_bytes,
                diagnostics.viewer_byte_budget,
                diagnostics.pinned_viewer_bytes,
            ),
            "Preview Frame Store physical allocation ledger",
        );
    }

    let oversize_rejections = diagnostics.oversize_rejections();
    if oversize_rejections > 0 {
        push_failure(
            failures,
            "preview_frame_store_oversize_rejection",
            "0 payloads rejected by their applicable physical admission grant",
            oversize_rejections.to_string(),
            "Preview Frame Store admission diagnostics",
        );
    }
    (within_budget, oversize_rejections)
}

fn checked_evidence_total(values: &[u64]) -> Option<u64> {
    values.iter().try_fold(0_u64, |total, value| total.checked_add(*value))
}

fn evaluate_professional_native_video_gpu_timing(
    evidence: &ProfessionalNativeVideoGpuTimingEvidence,
    failures: &mut Vec<PreviewAcceptanceFailure>,
) {
    let renderer_accounted = checked_evidence_total(&[
        evidence.renderer_samples,
        evidence.renderer_pending_samples,
        evidence.renderer_missing_samples,
        evidence.renderer_dropped_samples,
    ]);
    if renderer_accounted != Some(evidence.renderer_submitted_imports) {
        push_failure(
            failures,
            "native_video_gpu_timing_renderer_accounting_mismatch",
            "renderer submitted = samples + pending + missing + dropped without overflow",
            format!(
                "submitted={}, samples={}, pending={}, missing={}, dropped={}, classified={renderer_accounted:?}",
                evidence.renderer_submitted_imports,
                evidence.renderer_samples,
                evidence.renderer_pending_samples,
                evidence.renderer_missing_samples,
                evidence.renderer_dropped_samples,
            ),
            "renderer NativeVideoImportGpuTimingDiagnostics",
        );
    }

    let receipt_accounted = checked_evidence_total(&[
        evidence.receipt_scheduled_samples,
        evidence.receipt_missing_samples,
        evidence.receipt_dropped_samples,
    ]);
    if receipt_accounted != Some(evidence.receipt_submitted_imports) {
        push_failure(
            failures,
            "native_video_gpu_timing_receipt_accounting_mismatch",
            "receipt submitted = scheduled + missing + dropped without overflow",
            format!(
                "submitted={}, scheduled={}, missing={}, dropped={}, classified={receipt_accounted:?}",
                evidence.receipt_submitted_imports,
                evidence.receipt_scheduled_samples,
                evidence.receipt_missing_samples,
                evidence.receipt_dropped_samples,
            ),
            "move-only native-video candidate timing receipts",
        );
    }

    if !evidence.capability_supported {
        push_failure(
            failures,
            "native_video_gpu_timing_capability_unavailable",
            "renderer hardware timestamp capability",
            "false",
            evidence
                .inactive_reason
                .as_deref()
                .unwrap_or("renderer reported no inactive reason"),
        );
    }
    if !evidence.activated {
        push_failure(
            failures,
            "native_video_gpu_timing_not_activated",
            "explicitly activated healthy native-import GPU timing",
            "false",
            evidence
                .inactive_reason
                .as_deref()
                .unwrap_or("renderer reported no inactive reason"),
        );
    } else if let Some(reason) = evidence.inactive_reason.as_deref() {
        push_failure(
            failures,
            "native_video_gpu_timing_active_with_inactive_reason",
            "activated timing with no inactive reason",
            reason,
            "renderer timing activation diagnostics",
        );
    }

    if evidence.renderer_submitted_imports == 0 {
        push_failure(
            failures,
            "native_video_gpu_timing_no_submitted_imports",
            "at least one valid native import",
            "0",
            "renderer native-import timing coverage",
        );
    }
    for (code, label, observed) in [
        (
            "native_video_gpu_timing_renderer_pending_samples",
            "pending renderer samples",
            evidence.renderer_pending_samples,
        ),
        (
            "native_video_gpu_timing_renderer_missing_samples",
            "missing renderer samples",
            evidence.renderer_missing_samples,
        ),
        (
            "native_video_gpu_timing_renderer_dropped_samples",
            "dropped renderer samples",
            evidence.renderer_dropped_samples,
        ),
        (
            "native_video_gpu_timing_receipt_missing_samples",
            "missing receipt samples",
            evidence.receipt_missing_samples,
        ),
        (
            "native_video_gpu_timing_receipt_dropped_samples",
            "dropped receipt samples",
            evidence.receipt_dropped_samples,
        ),
        (
            "native_video_gpu_timing_adapter_dropped_samples",
            "Adapter-dropped observation samples",
            evidence.adapter_dropped_samples,
        ),
    ] {
        if observed > 0 {
            push_failure(
                failures,
                code,
                format!("0 {label}"),
                observed.to_string(),
                "native-video GPU timing coverage",
            );
        }
    }

    if evidence.receipt_candidates == 0 {
        push_failure(
            failures,
            "native_video_gpu_timing_receipt_candidate_missing",
            "at least one unique successful candidate receipt",
            "0",
            "native-video candidate receipt reconciliation",
        );
    }

    if !(evidence.renderer_samples == evidence.observed_samples
        && evidence.observed_samples == evidence.receipt_scheduled_samples
        && evidence.receipt_scheduled_samples == evidence.receipt_submitted_imports
        && evidence.receipt_submitted_imports == evidence.renderer_submitted_imports)
    {
        push_failure(
            failures,
            "native_video_gpu_timing_sample_reconciliation_mismatch",
            "renderer samples = observed samples = receipt scheduled = receipt submitted = renderer submitted",
            format!(
                "renderer_samples={}, observed={}, receipt_scheduled={}, receipt_submitted={}, renderer_submitted={}",
                evidence.renderer_samples,
                evidence.observed_samples,
                evidence.receipt_scheduled_samples,
                evidence.receipt_submitted_imports,
                evidence.renderer_submitted_imports,
            ),
            "renderer diagnostics, move-only receipts, and drained timing samples",
        );
    }

    if evidence.candidate_sample_count_mismatches > 0 {
        push_failure(
            failures,
            "native_video_gpu_timing_candidate_sample_count_mismatch",
            "every successful candidate's observed samples = its receipt scheduled samples",
            evidence.candidate_sample_count_mismatches.to_string(),
            "session-qualified candidate receipt/sample reconciliation",
        );
    }

    for (code, label, observed) in [
        (
            "native_video_gpu_timing_duplicate_candidate_receipt",
            "duplicate candidate receipts",
            evidence.duplicate_candidate_receipts,
        ),
        (
            "native_video_gpu_timing_duplicate_sample",
            "duplicate timing samples",
            evidence.duplicate_samples,
        ),
        (
            "native_video_gpu_timing_duplicate_sample_ownership",
            "multiply owned timing samples",
            evidence.duplicate_sample_ownership,
        ),
        (
            "native_video_gpu_timing_unmatched_sample",
            "unmatched timing samples",
            evidence.unmatched_samples,
        ),
        (
            "native_video_gpu_timing_orphan_candidate_receipt",
            "orphan candidate receipts",
            evidence.orphan_candidate_receipts,
        ),
    ] {
        if observed > 0 {
            push_failure(
                failures,
                code,
                format!("0 {label}"),
                observed.to_string(),
                "native-video receipt/sample ownership reconciliation",
            );
        }
    }
    if evidence.published_native_candidates > evidence.receipt_candidates {
        push_failure(
            failures,
            "native_video_gpu_timing_published_candidates_exceed_receipts",
            "published native candidates <= unique successful candidate receipts",
            format!(
                "published={}, receipts={}",
                evidence.published_native_candidates, evidence.receipt_candidates
            ),
            "presentation-bound native candidate coverage",
        );
    }
    if evidence.published_native_samples > evidence.observed_samples {
        push_failure(
            failures,
            "native_video_gpu_timing_published_samples_exceed_observed",
            "published native samples <= uniquely observed timing samples",
            format!(
                "published={}, observed={}",
                evidence.published_native_samples, evidence.observed_samples
            ),
            "presentation-bound native timing sample coverage",
        );
    }
    if evidence.published_native_candidates == 0 {
        push_failure(
            failures,
            "native_video_gpu_timing_published_candidate_missing",
            "at least one native candidate completed Viewer publication",
            "0",
            "presentation-bound native candidate ownership",
        );
    }
    if evidence.published_native_samples == 0 {
        push_failure(
            failures,
            "native_video_gpu_timing_published_sample_missing",
            "at least one timing sample owned by a published native candidate",
            "0",
            "presentation-bound native timing sample ownership",
        );
    }
}

pub(crate) fn evaluate_playback_qualification(
    observation: PreviewPlaybackQualificationObservation<'_>,
) -> PreviewPlaybackQualificationGateReport {
    let diagnostics = observation.preview_diagnostics;
    let mut failures = Vec::new();
    evaluate_main10_media_contract(
        observation.media,
        observation.required_source_frames,
        observation.frame_interval_ns,
        &mut failures,
    );
    let presented = evaluate_presented_main10_hardware(
        observation.rendered_decode_execution,
        observation.viewer_fallback_count,
        observation.viewer_fallback_reasons,
        &mut failures,
    );
    evaluate_cancellation_recovery(observation.cancellation_recovery, &mut failures);

    if observation.playback_evidence.deliveries.rejected > 0 {
        push_failure(
            &mut failures,
            "rejected_terminal_delivery_observed",
            "0 stale, duplicate, or superseded terminal deliveries",
            observation.playback_evidence.deliveries.rejected.to_string(),
            "Playback Evidence terminal delivery acceptance",
        );
    }
    if diagnostics.accurate_seek_temporal_approximation_frames > 0 {
        push_failure(
            &mut failures,
            "accurate_seek_temporal_approximation_observed",
            "0 approximate frames for deterministic accurate seeks",
            diagnostics.accurate_seek_temporal_approximation_frames.to_string(),
            "RandomAccessStillFrame decode diagnostics",
        );
    }
    if diagnostics.scheduler.pending_requests > 0
        || diagnostics.worker_queue.queued_jobs > 0
        || diagnostics.worker_queue.in_flight_jobs > 0
    {
        push_failure(
            &mut failures,
            "frame_work_not_quiescent",
            "0 pending bindings, queued jobs, and execution leases",
            format!(
                "pending={}, queued={}, in_flight={}, worker_execution={:?}",
                diagnostics.scheduler.pending_requests,
                diagnostics.worker_queue.queued_jobs,
                diagnostics.worker_queue.in_flight_jobs,
                diagnostics.decode_worker_execution,
            ),
            "Frame Work Broker structured diagnostics after recovery and residency release",
        );
    }
    if diagnostics.scheduler.clock_regressions > 0 {
        push_failure(
            &mut failures,
            "frame_work_clock_regression",
            "0 monotonic runtime-clock regressions",
            diagnostics.scheduler.clock_regressions.to_string(),
            "Frame Work Broker runtime-clock evidence",
        );
    }
    let isolated_demux =
        evaluate_isolated_demux(diagnostics.decode_worker_execution, &mut failures);
    let (cpu_frame_store_within_budget, cpu_frame_store_oversize_rejections) =
        evaluate_preview_frame_store_contract(diagnostics.frame_store, &mut failures);
    let cancellation_gate =
        evaluate_cancellation_contract(diagnostics.decode_cancellation, &mut failures);

    PreviewPlaybackQualificationGateReport {
        profile: "uhd_hevc_main10_isolated_demux_qualification_v1",
        required_source_frames: observation.required_source_frames,
        presented,
        cancellation_recovery: observation.cancellation_recovery,
        cancellation_gate,
        decode_cancellation_checkpoints: diagnostics.decode_cancellation_checkpoints,
        isolated_demux,
        broker_pending_requests: diagnostics.scheduler.pending_requests,
        broker_queued_jobs: diagnostics.worker_queue.queued_jobs,
        broker_in_flight_jobs: diagnostics.worker_queue.in_flight_jobs,
        broker_clock_regressions: diagnostics.scheduler.clock_regressions,
        rejected_terminal_deliveries: observation.playback_evidence.deliveries.rejected,
        accurate_seek_temporal_approximation_frames: diagnostics
            .accurate_seek_temporal_approximation_frames,
        cpu_frame_store_within_budget,
        cpu_frame_store_oversize_rejections,
        passed: failures.is_empty(),
        failures,
    }
}

pub(crate) fn evaluate_professional_playback(
    observation: ProfessionalPlaybackObservation<'_>,
) -> PreviewProfessionalPlaybackGateReport {
    let required_hardware_execution_percent = PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT;
    let media = observation.media;
    let mut failures = Vec::new();
    evaluate_main10_media_contract(
        media,
        observation.frames,
        observation.frame_interval_ns,
        &mut failures,
    );

    let evidence = observation.playback_evidence;
    let continuous_evidence = observation.continuous_playback_evidence;
    let diagnostics = observation.preview_diagnostics;
    if continuous_evidence.observed_duration_us < PROFESSIONAL_MIN_OBSERVED_DURATION_US {
        push_failure(
            &mut failures,
            "playback_duration_below_minimum",
            format!("at least {PROFESSIONAL_MIN_OBSERVED_DURATION_US} us"),
            format!("{} us", continuous_evidence.observed_duration_us),
            "continuous-window Playback Evidence monotonic observation span",
        );
    }
    if observation.continuous_playback_wall_duration_us < PROFESSIONAL_MIN_OBSERVED_DURATION_US {
        push_failure(
            &mut failures,
            "playback_wall_duration_below_minimum",
            format!("at least {PROFESSIONAL_MIN_OBSERVED_DURATION_US} us"),
            format!("{} us", observation.continuous_playback_wall_duration_us),
            "continuous-window process monotonic elapsed time",
        );
    }
    if evidence.warm_seek_latency.count < PROFESSIONAL_MIN_WARM_SEEKS {
        push_failure(
            &mut failures,
            "warm_seek_coverage_below_minimum",
            format!("at least {PROFESSIONAL_MIN_WARM_SEEKS} completed warm seeks"),
            evidence.warm_seek_latency.count.to_string(),
            "Playback Evidence accepted seek-to-presentation samples",
        );
    } else if evidence.warm_seek_latency.p95_us > PROFESSIONAL_WARM_SEEK_P95_LIMIT_US {
        push_failure(
            &mut failures,
            "warm_seek_p95_above_limit",
            format!("at most {PROFESSIONAL_WARM_SEEK_P95_LIMIT_US} us"),
            format!("{} us", evidence.warm_seek_latency.p95_us),
            "Playback Evidence warm seek latency",
        );
    }
    if evidence.accurate_seek_latency.count < PROFESSIONAL_MIN_ACCURATE_SEEKS {
        push_failure(
            &mut failures,
            "accurate_seek_coverage_below_minimum",
            format!("at least {PROFESSIONAL_MIN_ACCURATE_SEEKS} completed accurate seeks"),
            evidence.accurate_seek_latency.count.to_string(),
            "Playback Evidence accepted seek-to-presentation samples",
        );
    } else if evidence.accurate_seek_latency.p95_us > PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US {
        push_failure(
            &mut failures,
            "accurate_seek_p95_above_limit",
            format!("at most {PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US} us"),
            format!("{} us", evidence.accurate_seek_latency.p95_us),
            "Playback Evidence accurate seek latency",
        );
    }
    if diagnostics.accurate_seek_temporal_approximation_frames > 0 {
        push_failure(
            &mut failures,
            "accurate_seek_temporal_approximation_observed",
            "0 approximate frames for deterministic accurate seeks",
            diagnostics.accurate_seek_temporal_approximation_frames.to_string(),
            "RandomAccessStillFrame decode diagnostics",
        );
    }
    if evidence.seek_superseded_count < PROFESSIONAL_MIN_SUPERSEDED_SEEKS {
        push_failure(
            &mut failures,
            "latest_wins_seek_coverage_below_minimum",
            format!("at least {PROFESSIONAL_MIN_SUPERSEDED_SEEKS} superseded seeks"),
            evidence.seek_superseded_count.to_string(),
            "Playback Evidence rapid cross-region seek burst",
        );
    }
    if evidence.deliveries.rejected > 0 {
        push_failure(
            &mut failures,
            "rejected_terminal_delivery_observed",
            "0 stale, duplicate, or superseded terminal deliveries",
            evidence.deliveries.rejected.to_string(),
            "Playback Evidence terminal delivery acceptance",
        );
    }
    if diagnostics.scheduler.pending_requests > 0
        || diagnostics.worker_queue.queued_jobs > 0
        || diagnostics.worker_queue.in_flight_jobs > 0
    {
        push_failure(
            &mut failures,
            "frame_work_not_quiescent",
            "0 pending bindings, queued jobs, and execution leases",
            format!(
                "pending={}, queued={}, in_flight={}, worker_execution={:?}",
                diagnostics.scheduler.pending_requests,
                diagnostics.worker_queue.queued_jobs,
                diagnostics.worker_queue.in_flight_jobs,
                diagnostics.decode_worker_execution,
            ),
            "Frame Work Broker structured diagnostics after latest-wins seek burst",
        );
    }
    if diagnostics.scheduler.clock_regressions > 0 {
        push_failure(
            &mut failures,
            "frame_work_clock_regression",
            "0 monotonic runtime-clock regressions",
            diagnostics.scheduler.clock_regressions.to_string(),
            "Frame Work Broker runtime-clock evidence",
        );
    }
    let isolated_demux =
        evaluate_isolated_demux(diagnostics.decode_worker_execution, &mut failures);
    let (cpu_frame_store_within_budget, cpu_frame_store_oversize_rejections) =
        evaluate_preview_frame_store_contract(diagnostics.frame_store, &mut failures);
    let process_memory = evaluate_process_memory(observation.process_memory, &mut failures);
    let cancellation_gate =
        evaluate_cancellation_contract(diagnostics.decode_cancellation, &mut failures);
    evaluate_professional_native_video_gpu_timing(
        observation.native_video_gpu_timing,
        &mut failures,
    );
    let presented = evaluate_presented_main10_hardware(
        observation.rendered_decode_execution,
        observation.viewer_fallback_count,
        observation.viewer_fallback_reasons,
        &mut failures,
    );

    let playback = observation.playback_decode;
    let hardware_requested_frames = playback
        .hardware_decode_prefer_hardware_requested_frames
        .saturating_add(playback.hardware_decode_prefer_gpu_requested_frames)
        .saturating_add(playback.hardware_decode_require_gpu_requested_frames);
    PreviewProfessionalPlaybackGateReport {
        profile: "uhd_hevc_main10_hardware_1x_v6",
        required_hardware_execution_percent,
        presented_media_layers: presented.presented_media_layers,
        presented_hardware_layers: presented.presented_hardware_layers,
        presented_hardware_cpu_transfer_layers: presented.presented_hardware_cpu_transfer_layers,
        presented_hardware_native_layers: presented.presented_hardware_native_layers,
        presented_p010_10_bit_hardware_layers: presented.presented_p010_10_bit_hardware_layers,
        hardware_execution_percent: presented.hardware_execution_percent,
        hardware_requested_frames,
        fallback_cpu_not_requested_frames: playback.hardware_decode_cpu_not_requested_frames,
        fallback_cpu_unavailable_frames: playback.hardware_decode_cpu_unavailable_frames,
        fallback_backend_unavailable_frames: playback.hardware_decode_backend_unavailable_frames,
        fallback_codec_unsupported_frames: playback.hardware_decode_codec_unsupported_frames,
        fallback_device_context_unavailable_frames: playback
            .hardware_decode_device_context_unavailable_frames,
        fallback_setup_failed_frames: playback.hardware_decode_cpu_transfer_setup_failed_frames,
        fallback_decoder_open_failed_frames: playback
            .hardware_decode_cpu_transfer_decoder_open_failed_frames,
        fallback_awaiting_hardware_frame_frames: playback
            .hardware_decode_cpu_transfer_awaiting_frame_frames,
        fallback_backend_boundary_frames: playback.hardware_decode_backend_boundary_frames,
        fallback_adapter_unavailable_frames: playback.hardware_decode_adapter_unavailable_frames,
        observed_duration_limit_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
        observed_duration_us: continuous_evidence.observed_duration_us,
        wall_duration_us: observation.continuous_playback_wall_duration_us,
        min_warm_seeks: PROFESSIONAL_MIN_WARM_SEEKS,
        warm_seek_count: evidence.warm_seek_latency.count,
        warm_seek_p95_limit_us: PROFESSIONAL_WARM_SEEK_P95_LIMIT_US,
        warm_seek_p95_observed_us: evidence.warm_seek_latency.p95_us,
        min_accurate_seeks: PROFESSIONAL_MIN_ACCURATE_SEEKS,
        accurate_seek_count: evidence.accurate_seek_latency.count,
        accurate_seek_p95_limit_us: PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US,
        accurate_seek_p95_observed_us: evidence.accurate_seek_latency.p95_us,
        accurate_seek_temporal_approximation_frames: diagnostics
            .accurate_seek_temporal_approximation_frames,
        min_superseded_seeks: PROFESSIONAL_MIN_SUPERSEDED_SEEKS,
        superseded_seek_count: evidence.seek_superseded_count,
        rejected_terminal_deliveries: evidence.deliveries.rejected,
        evicted_detailed_events: evidence.evicted_event_count,
        broker_pending_requests: diagnostics.scheduler.pending_requests,
        broker_queued_jobs: diagnostics.worker_queue.queued_jobs,
        broker_in_flight_jobs: diagnostics.worker_queue.in_flight_jobs,
        broker_clock_regressions: diagnostics.scheduler.clock_regressions,
        cpu_frame_store_within_budget,
        cpu_frame_store_oversize_rejections,
        process_memory,
        cancellation_gate,
        decode_cancellation_checkpoints: diagnostics.decode_cancellation_checkpoints,
        isolated_demux,
        decode_worker_execution: diagnostics.decode_worker_execution,
        native_video_gpu_timing: (*observation.native_video_gpu_timing).clone(),
        passed: failures.is_empty(),
        failures,
    }
}

fn evaluate_process_memory(
    evidence: &PreviewProcessMemoryEvidenceReport,
    failures: &mut Vec<PreviewAcceptanceFailure>,
) -> PreviewProcessMemoryGateReport {
    let failure_start = failures.len();
    if evidence.scope.as_deref()
        != Some(mondrian_platform::ProcessMemoryScope::ProductProcessTree.as_str())
    {
        push_failure(
            failures,
            "process_memory_scope_not_product_tree",
            mondrian_platform::ProcessMemoryScope::ProductProcessTree.as_str(),
            evidence.scope.as_deref().unwrap_or("unavailable"),
            "professional memory acceptance requires Mondrian plus every descendant process",
        );
    }
    if !evidence.discovery_available || evidence.backend.is_none() {
        push_failure(
            failures,
            "process_memory_probe_unavailable",
            "native product-process-tree private-commit evidence",
            evidence.backend.as_deref().unwrap_or("unavailable"),
            evidence.last_probe_error.as_deref().unwrap_or("no native backend"),
        );
    }
    if !evidence.inventory_complete
        || evidence.minimum_observed_process_count.is_none_or(|count| count == 0)
    {
        push_failure(
            failures,
            "process_memory_inventory_incomplete",
            "every attempted sample has one complete non-empty product process-tree inventory",
            format!(
                "complete={}, observed={}/{}, process_count={:?}..{}",
                evidence.inventory_complete,
                evidence.observed_samples,
                evidence.attempted_samples,
                evidence.minimum_observed_process_count,
                evidence.maximum_observed_process_count,
            ),
            evidence
                .last_probe_error
                .as_deref()
                .unwrap_or("no complete process-tree evidence"),
        );
    }
    if evidence.probe_errors > 0 {
        push_failure(
            failures,
            "process_memory_probe_error",
            "0 failed or incomplete samples",
            evidence.probe_errors.to_string(),
            evidence.last_probe_error.as_deref().unwrap_or("unknown probe error"),
        );
    }
    if evidence.observed_duration_us < PROFESSIONAL_MIN_OBSERVED_DURATION_US {
        push_failure(
            failures,
            "process_memory_observation_too_short",
            format!("at least {PROFESSIONAL_MIN_OBSERVED_DURATION_US} us"),
            format!("{} us", evidence.observed_duration_us),
            "product-process-tree memory evidence observation span",
        );
    }
    if evidence.baseline_sample_count < PROCESS_MEMORY_MIN_WINDOW_SAMPLES {
        push_failure(
            failures,
            "process_memory_baseline_coverage_below_minimum",
            format!("at least {PROCESS_MEMORY_MIN_WINDOW_SAMPLES} samples from minutes 5-10"),
            evidence.baseline_sample_count.to_string(),
            "fixed-cadence product-process-tree private-commit samples",
        );
    }
    if evidence.final_sample_count < PROCESS_MEMORY_MIN_WINDOW_SAMPLES {
        push_failure(
            failures,
            "process_memory_final_coverage_below_minimum",
            format!("at least {PROCESS_MEMORY_MIN_WINDOW_SAMPLES} samples from minutes 25-30"),
            evidence.final_sample_count.to_string(),
            "fixed-cadence product-process-tree private-commit samples",
        );
    }
    if evidence.peak_private_committed_bytes > PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES {
        push_failure(
            failures,
            "process_memory_private_commit_above_limit",
            format!("at most {PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES} bytes"),
            format!("{} bytes", evidence.peak_private_committed_bytes),
            "native product-process-tree private-commit high-water mark",
        );
    }
    let settled_growth_bytes = evidence
        .final_average_private_committed_bytes
        .saturating_sub(evidence.baseline_average_private_committed_bytes);
    if settled_growth_bytes > PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES {
        push_failure(
            failures,
            "process_memory_did_not_plateau",
            format!("at most {PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES} bytes average growth"),
            format!("{settled_growth_bytes} bytes"),
            "minutes 25-30 average private commit minus minutes 5-10 average",
        );
    }
    let post_stress_growth_bytes = evidence
        .post_stress_private_committed_bytes
        .map(|bytes| bytes.saturating_sub(evidence.final_average_private_committed_bytes));
    match (
        evidence.post_stress_private_committed_bytes,
        post_stress_growth_bytes,
    ) {
        (None, _) => push_failure(
            failures,
            "process_memory_post_stress_sample_missing",
            "one private-commit sample after the gate's terminal stress and quiescence",
            "missing",
            "post-stress process-memory evidence",
        ),
        (Some(bytes), Some(growth)) => {
            if bytes > PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES {
                push_failure(
                    failures,
                    "process_memory_post_stress_private_commit_above_limit",
                    format!("at most {PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES} bytes"),
                    format!("{bytes} bytes"),
                    "post-stress native product-process-tree private commit",
                );
            }
            if growth > PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES {
                push_failure(
                    failures,
                    "process_memory_stress_did_not_settle",
                    format!("at most {PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES} bytes above final playback average"),
                    format!("{growth} bytes"),
                    "private commit after Broker/worker quiescence",
                );
            }
        }
        _ => {}
    }

    let memory_failures = failures[failure_start..].to_vec();
    let passed = memory_failures.is_empty();
    PreviewProcessMemoryGateReport {
        profile: "product_process_tree_private_commit_v2",
        scope: evidence.scope.clone(),
        backend: evidence.backend.clone(),
        inventory_complete: evidence.inventory_complete,
        minimum_observed_process_count: evidence.minimum_observed_process_count,
        maximum_observed_process_count: evidence.maximum_observed_process_count,
        maximum_inventory_attempts: evidence.maximum_inventory_attempts,
        attempted_samples: evidence.attempted_samples,
        observed_samples: evidence.observed_samples,
        probe_errors: evidence.probe_errors,
        observed_duration_us: evidence.observed_duration_us,
        max_private_committed_bytes: PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES,
        peak_private_committed_bytes: evidence.peak_private_committed_bytes,
        peak_resident_bytes: evidence.peak_resident_bytes,
        os_peak_resident_bytes: evidence.os_peak_resident_bytes,
        max_settled_growth_bytes: PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES,
        baseline_sample_count: evidence.baseline_sample_count,
        final_sample_count: evidence.final_sample_count,
        baseline_average_private_committed_bytes: evidence.baseline_average_private_committed_bytes,
        final_average_private_committed_bytes: evidence.final_average_private_committed_bytes,
        settled_growth_bytes,
        post_stress_private_committed_bytes: evidence.post_stress_private_committed_bytes,
        post_stress_growth_bytes,
        passed,
        failures: memory_failures,
    }
}

/// Evaluate the shared whole-product process-tree memory contract without coupling another
/// professional gate to the video-specific failure representation.
pub(crate) fn evaluate_process_memory_gate(
    evidence: &PreviewProcessMemoryEvidenceReport,
) -> PreviewProcessMemoryGateReport {
    let mut failures = Vec::new();
    evaluate_process_memory(evidence, &mut failures)
}

fn percent(numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        0
    } else {
        numerator.saturating_mul(100) / denominator
    }
}

fn push_failure(
    failures: &mut Vec<PreviewAcceptanceFailure>,
    code: &'static str,
    expected: impl Into<String>,
    observed: impl Into<String>,
    evidence: impl Into<String>,
) {
    failures.push(PreviewAcceptanceFailure {
        code,
        expected: expected.into(),
        observed: observed.into(),
        evidence: evidence.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    type PreviewDiagnostics = PreviewRuntimeAcceptanceEvidence;
    type PreviewDecodeExecutionSummary = PresentedDecodeExecutionEvidence;
    type PreviewDecodeAccessModeProfile = PlaybackDecodeExecutionEvidence;

    #[test]
    fn native_gpu_timing_accepts_owned_successful_candidates_without_requiring_publication() {
        let evidence = passing_native_video_gpu_timing_evidence();

        assert!(
            native_video_gpu_timing_failure_codes(&evidence).is_empty(),
            "uniquely owned Release/superseded/late candidates remain valid"
        );
        assert!(evidence.receipt_candidates > evidence.published_native_candidates);
        assert!(evidence.observed_samples > evidence.published_native_samples);
    }

    #[test]
    fn native_gpu_timing_rejects_missing_capability_activation_and_contradictory_reason() {
        let mut inactive = passing_native_video_gpu_timing_evidence();
        inactive.capability_supported = false;
        inactive.activated = false;
        inactive.inactive_reason = Some("timestamp queries unavailable".to_owned());
        assert_eq!(
            native_video_gpu_timing_failure_codes(&inactive),
            vec![
                "native_video_gpu_timing_capability_unavailable",
                "native_video_gpu_timing_not_activated",
            ]
        );

        let mut contradictory = passing_native_video_gpu_timing_evidence();
        contradictory.inactive_reason = Some("readback failed".to_owned());
        assert_eq!(
            native_video_gpu_timing_failure_codes(&contradictory),
            vec!["native_video_gpu_timing_active_with_inactive_reason"]
        );
    }

    #[test]
    fn native_gpu_timing_rejects_each_renderer_and_receipt_coverage_hole() {
        let mut evidence = passing_native_video_gpu_timing_evidence();
        evidence.renderer_pending_samples = 1;
        evidence.renderer_missing_samples = 1;
        evidence.renderer_dropped_samples = 1;
        evidence.receipt_missing_samples = 1;
        evidence.receipt_dropped_samples = 1;
        evidence.adapter_dropped_samples = 1;

        assert_eq!(
            native_video_gpu_timing_failure_codes(&evidence),
            vec![
                "native_video_gpu_timing_renderer_accounting_mismatch",
                "native_video_gpu_timing_receipt_accounting_mismatch",
                "native_video_gpu_timing_renderer_pending_samples",
                "native_video_gpu_timing_renderer_missing_samples",
                "native_video_gpu_timing_renderer_dropped_samples",
                "native_video_gpu_timing_receipt_missing_samples",
                "native_video_gpu_timing_receipt_dropped_samples",
                "native_video_gpu_timing_adapter_dropped_samples",
            ]
        );
    }

    #[test]
    fn native_gpu_timing_rejects_zero_submission_receipt_and_publication_coverage() {
        let evidence = ProfessionalNativeVideoGpuTimingEvidence {
            capability_supported: true,
            activated: true,
            ..ProfessionalNativeVideoGpuTimingEvidence::default()
        };

        assert_eq!(
            native_video_gpu_timing_failure_codes(&evidence),
            vec![
                "native_video_gpu_timing_no_submitted_imports",
                "native_video_gpu_timing_receipt_candidate_missing",
                "native_video_gpu_timing_published_candidate_missing",
                "native_video_gpu_timing_published_sample_missing",
            ]
        );
    }

    #[test]
    fn native_gpu_timing_rejects_renderer_receipt_and_observation_mismatch() {
        let mut evidence = passing_native_video_gpu_timing_evidence();
        evidence.observed_samples = evidence.observed_samples.saturating_sub(1);

        assert_eq!(
            native_video_gpu_timing_failure_codes(&evidence),
            vec!["native_video_gpu_timing_sample_reconciliation_mismatch"]
        );
    }

    #[test]
    fn native_gpu_timing_rejects_candidate_local_sample_count_mismatch() {
        let mut evidence = passing_native_video_gpu_timing_evidence();
        evidence.candidate_sample_count_mismatches = 2;

        assert_eq!(
            native_video_gpu_timing_failure_codes(&evidence),
            vec!["native_video_gpu_timing_candidate_sample_count_mismatch"]
        );
    }

    #[test]
    fn native_gpu_timing_rejects_adapter_observation_overflow() {
        let mut evidence = passing_native_video_gpu_timing_evidence();
        evidence.adapter_dropped_samples = 1;

        assert_eq!(
            native_video_gpu_timing_failure_codes(&evidence),
            vec!["native_video_gpu_timing_adapter_dropped_samples"]
        );
    }

    #[test]
    fn native_gpu_timing_rejects_duplicate_unmatched_and_orphan_ownership() {
        let mut evidence = passing_native_video_gpu_timing_evidence();
        evidence.duplicate_candidate_receipts = 1;
        evidence.duplicate_samples = 1;
        evidence.duplicate_sample_ownership = 1;
        evidence.unmatched_samples = 1;
        evidence.orphan_candidate_receipts = 1;

        assert_eq!(
            native_video_gpu_timing_failure_codes(&evidence),
            vec![
                "native_video_gpu_timing_duplicate_candidate_receipt",
                "native_video_gpu_timing_duplicate_sample",
                "native_video_gpu_timing_duplicate_sample_ownership",
                "native_video_gpu_timing_unmatched_sample",
                "native_video_gpu_timing_orphan_candidate_receipt",
            ]
        );
    }

    #[test]
    fn native_gpu_timing_rejects_fabricated_publication_coverage() {
        let mut evidence = passing_native_video_gpu_timing_evidence();
        evidence.published_native_candidates = evidence.receipt_candidates.saturating_add(1);
        evidence.published_native_samples = evidence.observed_samples.saturating_add(1);

        assert_eq!(
            native_video_gpu_timing_failure_codes(&evidence),
            vec![
                "native_video_gpu_timing_published_candidates_exceed_receipts",
                "native_video_gpu_timing_published_samples_exceed_observed",
            ]
        );
    }

    #[test]
    fn accepts_short_production_demux_cancellation_qualification() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = passing_preview_diagnostics();

        let report = evaluate_playback_qualification(PreviewPlaybackQualificationObservation {
            media: &media,
            required_source_frames: 45_000,
            frame_interval_ns: 40_000_000,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_native_layers: 100,
                p010_10_bit_hardware_layers: 100,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_evidence: &evidence,
            preview_diagnostics: &diagnostics,
            cancellation_recovery: passing_cancellation_recovery(),
        });

        assert!(report.passed, "{:?}", report.failures);
        assert_eq!(
            report.profile,
            "uhd_hevc_main10_isolated_demux_qualification_v1"
        );
        assert_eq!(report.required_source_frames, 45_000);
    }

    #[test]
    fn short_qualification_fails_without_active_call_and_recovery_evidence() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = passing_preview_diagnostics();
        let mut cancellation_recovery = passing_cancellation_recovery();
        cancellation_recovery.stage_before_supersession =
            mondrian_media::PreviewDecodeExecutionStage::Idle;
        cancellation_recovery.broker_cancellation_delta = 0;
        cancellation_recovery.media_cancellation_checkpoint_delta = 0;
        cancellation_recovery.isolated_termination_delta = 1;
        cancellation_recovery.isolated_checkpoint_delta = 0;
        cancellation_recovery.recovery_presented = false;

        let report = evaluate_playback_qualification(PreviewPlaybackQualificationObservation {
            media: &media,
            required_source_frames: 45_000,
            frame_interval_ns: 40_000_000,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_native_layers: 100,
                p010_10_bit_hardware_layers: 100,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_evidence: &evidence,
            preview_diagnostics: &diagnostics,
            cancellation_recovery,
        });
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();

        assert!(!report.passed);
        assert!(codes.contains(&"isolated_demux_active_call_unproven"));
        assert!(codes.contains(&"cancellation_recovery_broker_evidence_missing"));
        assert!(codes.contains(&"cancellation_recovery_media_checkpoint_missing"));
        assert!(codes.contains(&"isolated_demux_termination_checkpoint_mismatch"));
        assert!(codes.contains(&"post_cancellation_presentation_missing"));
    }

    #[test]
    fn accepts_presented_main10_hardware_execution() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = passing_preview_diagnostics();
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_cpu_transfer_layers: 60,
                hardware_native_layers: 40,
                p010_10_bit_hardware_layers: 100,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: PreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            continuous_playback_evidence: &evidence,
            continuous_playback_wall_duration_us: evidence.observed_duration_us,
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);

        assert!(report.passed, "{:?}", report.failures);
        assert_eq!(report.profile, "uhd_hevc_main10_hardware_1x_v6");
        assert_eq!(
            report.required_hardware_execution_percent,
            PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT
        );
        assert_eq!(report.presented_hardware_layers, 100);
        assert_eq!(report.hardware_execution_percent, 100);
        assert_eq!(
            report.native_video_gpu_timing,
            passing_native_video_gpu_timing_evidence()
        );
    }

    #[test]
    fn rejects_playback_when_shared_cancellation_policy_fails() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let mut cancellation = mondrian_playback::FrameCancellationEvidenceCollector::default();
        cancellation.observe(mondrian_playback::FrameCancellationObservation {
            work_class: mondrian_playback::FrameWorkClass::Interactive,
            cause: mondrian_playback::FrameCancellationCause::Superseded,
            execution_duration: std::time::Duration::from_micros(70_001),
            execution_to_logical_cancellation: Some(std::time::Duration::from_millis(20)),
            request_to_logical_cancellation: Some(std::time::Duration::from_millis(1)),
        });
        let diagnostics = PreviewDiagnostics {
            decode_cancellation: cancellation.report(),
            ..passing_preview_diagnostics()
        };
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_native_layers: 100,
                p010_10_bit_hardware_layers: 100,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: PreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            continuous_playback_evidence: &evidence,
            continuous_playback_wall_duration_us: evidence.observed_duration_us,
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);

        assert!(!report.passed);
        assert!(!report.cancellation_gate.passed);
        let failure = report
            .cancellation_gate
            .failures
            .iter()
            .find(|failure| {
                failure.work_class == mondrian_playback::FrameWorkClass::Interactive
                    && failure.kind
                        == mondrian_playback::FrameCancellationGateFailureKind::LogicalCancellationToReturnExceeded
            })
            .expect("Interactive cancellation return must exceed the product gate");
        assert_eq!(failure.observed, 50_001);
        assert_eq!(failure.limit, 50_000);
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.code == "frame_cancellation_physical_return_late"));
    }

    #[test]
    fn rejects_unproven_identity_and_software_presentation() {
        let mut media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = passing_preview_diagnostics();
        media.codec_profile = VideoCodecProfile::Unknown;
        media.frame_rate_proven = false;
        media.pixel_format_proven = false;
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 10,
                software_cpu_layers: 10,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: PreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            continuous_playback_evidence: &evidence,
            continuous_playback_wall_duration_us: evidence.observed_duration_us,
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
            frames: 10,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();

        assert_eq!(
            codes,
            vec![
                "media_codec_profile_unproven",
                "media_frame_rate_unknown",
                "media_pixel_format_unknown",
                "hardware_decode_coverage_below_minimum",
                "hardware_main10_surface_coverage_below_minimum",
            ]
        );
        assert!(!report.passed);
    }

    #[test]
    fn preflight_rejects_primary_video_shorter_than_observation_window() {
        let mut media = main10_media();
        media.video_stream_duration_us = Some(2_880_000);

        let error = media
            .ensure_observation_coverage(45_000, 40_000_000)
            .expect_err("short source must not start a thirty-minute gate");

        assert!(error.to_string().contains("primary-video stream duration"));
        assert!(error.to_string().contains("provides 2880000 us"));
    }

    #[test]
    fn preflight_rejects_unproven_primary_video_duration() {
        let mut media = main10_media();
        media.video_stream_duration_us = None;

        let error = media
            .ensure_observation_coverage(45_000, 40_000_000)
            .expect_err("unknown video duration must fail closed");

        assert!(error.to_string().contains("proven primary-video stream duration"));
    }

    #[test]
    fn preflight_rejects_insufficient_declared_video_frame_count() {
        let mut media = main10_media();
        media.total_frames = Some(44_999);

        let error = media
            .ensure_observation_coverage(45_000, 40_000_000)
            .expect_err("short declared frame count must fail before a long run");

        assert!(error.to_string().contains("at least 45000 declared primary-video frames"));
        assert!(error.to_string().contains("provides 44999"));
    }

    #[test]
    fn rejects_declared_video_frame_count_below_observation() {
        let mut media = main10_media();
        media.total_frames = Some(44_999);
        let evidence = passing_playback_evidence();
        let diagnostics = passing_preview_diagnostics();
        let report = evaluate_professional_playback(ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_native_layers: 100,
                p010_10_bit_hardware_layers: 100,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: PreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            continuous_playback_evidence: &evidence,
            continuous_playback_wall_duration_us: evidence.observed_duration_us,
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        });

        assert!(report
            .failures
            .iter()
            .any(|failure| failure.code == "media_video_frame_count_insufficient"));
    }

    #[test]
    fn professional_gate_accepts_cinema_broadcast_and_high_frame_rates() {
        for frame_rate in PROFESSIONAL_FRAME_RATES {
            let mut media = main10_media();
            media.frame_rate = frame_rate;
            let evidence = passing_playback_evidence();
            let diagnostics = passing_preview_diagnostics();
            let report = evaluate_professional_playback(ProfessionalPlaybackObservation {
                media: &media,
                rendered_decode_execution: PreviewDecodeExecutionSummary {
                    media_layers: 100,
                    hardware_native_layers: 100,
                    p010_10_bit_hardware_layers: 100,
                    ..PreviewDecodeExecutionSummary::default()
                },
                viewer_fallback_count: 0,
                viewer_fallback_reasons: &[],
                playback_decode: PreviewDecodeAccessModeProfile::default(),
                playback_evidence: &evidence,
                continuous_playback_evidence: &evidence,
                continuous_playback_wall_duration_us: evidence.observed_duration_us,
                preview_diagnostics: &diagnostics,
                process_memory: &passing_process_memory_evidence(),
                native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
                frames: 45_000,
                frame_interval_ns: 40_000_000,
            });

            assert!(report.passed, "{frame_rate}: {:?}", report.failures);
        }
    }

    #[test]
    fn rejects_short_run_missing_seek_coverage_and_rejected_old_delivery() {
        let media = main10_media();
        let mut evidence = mondrian_playback::PlaybackEvidenceCollector::default().report();
        let mut diagnostics = passing_preview_diagnostics();
        diagnostics.scheduler.pending_requests = 1;
        diagnostics.scheduler.clock_regressions = 1;
        diagnostics.worker_queue.queued_jobs = 1;
        diagnostics.worker_queue.in_flight_jobs = 1;
        evidence.observed_duration_us = 10_000_000;
        evidence.deliveries.rejected = 1;
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 10,
                hardware_native_layers: 10,
                p010_10_bit_hardware_layers: 10,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: PreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            continuous_playback_evidence: &evidence,
            continuous_playback_wall_duration_us: evidence.observed_duration_us,
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
            frames: 10,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"playback_duration_below_minimum"));
        assert!(codes.contains(&"warm_seek_coverage_below_minimum"));
        assert!(codes.contains(&"accurate_seek_coverage_below_minimum"));
        assert!(codes.contains(&"latest_wins_seek_coverage_below_minimum"));
        assert!(codes.contains(&"rejected_terminal_delivery_observed"));
        assert!(codes.contains(&"frame_work_not_quiescent"));
        assert!(codes.contains(&"frame_work_clock_regression"));
    }

    #[test]
    fn rejects_seek_latency_above_professional_p95_limits() {
        let media = main10_media();
        let mut evidence = passing_playback_evidence();
        let diagnostics = passing_preview_diagnostics();
        evidence.warm_seek_latency.p95_us = PROFESSIONAL_WARM_SEEK_P95_LIMIT_US + 1;
        evidence.accurate_seek_latency.p95_us = PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US + 1;
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_native_layers: 100,
                p010_10_bit_hardware_layers: 100,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: PreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            continuous_playback_evidence: &evidence,
            continuous_playback_wall_duration_us: evidence.observed_duration_us,
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"warm_seek_p95_above_limit"));
        assert!(codes.contains(&"accurate_seek_p95_above_limit"));
    }

    #[test]
    fn rejects_temporal_approximation_in_deterministic_accurate_seek() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = PreviewDiagnostics {
            accurate_seek_temporal_approximation_frames: 1,
            ..passing_preview_diagnostics()
        };
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_native_layers: 100,
                p010_10_bit_hardware_layers: 100,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: PreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            continuous_playback_evidence: &evidence,
            continuous_playback_wall_duration_us: evidence.observed_duration_us,
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);

        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"accurate_seek_temporal_approximation_observed"));
    }

    #[test]
    fn rejects_missing_or_unreaped_isolated_demux_execution() {
        let missing = PreviewDecodeWorkerExecutionDiagnostics::default();
        let mut failures = Vec::new();

        let missing_evidence = evaluate_isolated_demux(missing, &mut failures);

        assert_eq!(missing_evidence.session_launches, 0);
        assert!(failures.iter().any(|failure| failure.code == "isolated_demux_not_executed"));

        let mut active = passing_preview_diagnostics().decode_worker_execution;
        let progress = active.playback.as_mut().expect("passing Playback worker evidence");
        progress.isolated_demux.clean_closes = 0;
        progress.isolated_demux.active_sessions = 1;
        let mut active_failures = Vec::new();

        let active_evidence = evaluate_isolated_demux(active, &mut active_failures);

        assert_eq!(active_evidence.active_sessions, 1);
        assert!(active_failures
            .iter()
            .any(|failure| failure.code == "isolated_demux_not_fully_reaped"));
        assert!(active_failures
            .iter()
            .any(|failure| failure.code == "isolated_demux_clean_close_unproven"));
    }

    #[test]
    fn process_memory_collector_keeps_fixed_window_aggregates() {
        let mib = 1024 * 1024;
        let mut collector = PreviewProcessMemoryEvidenceCollector::default();
        for index in 0..PROCESS_MEMORY_MIN_WINDOW_SAMPLES {
            collector.observe_playback(
                PROCESS_MEMORY_WARMUP_END_US + index * 1_000_000,
                process_memory_sample(500 * mib),
            );
            collector.observe_playback(
                PROCESS_MEMORY_FINAL_WINDOW_START_US + index * 1_000_000,
                process_memory_sample(540 * mib),
            );
        }
        collector.observe_playback(
            PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            process_memory_sample(540 * mib),
        );
        collector.observe_post_stress(process_memory_sample(560 * mib));

        let report = collector.report();

        assert_eq!(
            report.baseline_sample_count,
            PROCESS_MEMORY_MIN_WINDOW_SAMPLES
        );
        assert_eq!(report.baseline_average_private_committed_bytes, 500 * mib);
        assert_eq!(
            report.final_sample_count,
            PROCESS_MEMORY_MIN_WINDOW_SAMPLES + 1
        );
        assert_eq!(report.final_average_private_committed_bytes, 540 * mib);
        assert_eq!(report.post_stress_private_committed_bytes, Some(560 * mib));
        assert_eq!(
            report.attempted_samples,
            PROCESS_MEMORY_MIN_WINDOW_SAMPLES * 2 + 2
        );
        assert_eq!(
            report.scope.as_deref(),
            Some(mondrian_platform::ProcessMemoryScope::ProductProcessTree.as_str())
        );
        assert!(report.inventory_complete);
        assert_eq!(report.minimum_observed_process_count, Some(3));
        assert_eq!(report.maximum_observed_process_count, 3);
    }

    #[test]
    fn process_memory_gate_rejects_complete_current_process_scope() {
        let mut collector = PreviewProcessMemoryEvidenceCollector::default();
        collector.observe_playback(
            PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            mondrian_platform::ProcessMemoryProbeResult::observed(
                mondrian_platform::ProcessMemoryScope::CurrentProcess,
                mondrian_platform::ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
                1,
                1,
                512 * 1024 * 1024,
                384 * 1024 * 1024,
                512 * 1024 * 1024,
            ),
        );
        let evidence = collector.report();
        let mut failures = Vec::new();

        let gate = evaluate_process_memory(&evidence, &mut failures);

        assert!(!gate.passed());
        assert!(failures
            .iter()
            .any(|failure| failure.code == "process_memory_scope_not_product_tree"));
        assert!(failures
            .iter()
            .any(|failure| failure.code == "process_memory_inventory_incomplete"));
    }

    #[test]
    fn process_memory_gate_rejects_incomplete_product_tree_inventory() {
        let mut collector = PreviewProcessMemoryEvidenceCollector::default();
        collector.observe_playback(
            PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            mondrian_platform::ProcessMemoryProbeResult::failed(
                mondrian_platform::ProcessMemoryScope::ProductProcessTree,
                mondrian_platform::ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                2,
                4,
                "child exited during inventory validation",
            ),
        );
        let evidence = collector.report();
        let mut failures = Vec::new();

        let gate = evaluate_process_memory(&evidence, &mut failures);

        assert!(!gate.passed());
        assert!(failures
            .iter()
            .any(|failure| failure.code == "process_memory_inventory_incomplete"));
        assert!(failures.iter().any(|failure| failure.code == "process_memory_probe_error"));
    }

    #[test]
    fn rejects_process_memory_growth_that_does_not_plateau() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = passing_preview_diagnostics();
        let mut process_memory = passing_process_memory_evidence();
        process_memory.final_average_private_committed_bytes = process_memory
            .baseline_average_private_committed_bytes
            .saturating_add(PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES)
            .saturating_add(1);
        process_memory.post_stress_private_committed_bytes =
            Some(process_memory.final_average_private_committed_bytes);

        let report = evaluate_professional_playback(ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: PreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_native_layers: 100,
                p010_10_bit_hardware_layers: 100,
                ..PreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: PreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            continuous_playback_evidence: &evidence,
            continuous_playback_wall_duration_us: evidence.observed_duration_us,
            preview_diagnostics: &diagnostics,
            process_memory: &process_memory,
            native_video_gpu_timing: &passing_native_video_gpu_timing_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        });

        assert!(!report.process_memory.passed);
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.code == "process_memory_did_not_plateau"));
    }

    fn passing_preview_diagnostics() -> PreviewDiagnostics {
        PreviewDiagnostics {
            decode_worker_execution: PreviewDecodeWorkerExecutionDiagnostics {
                playback: Some(mondrian_media::PreviewDecodeExecutionProgress {
                    isolated_demux: mondrian_media::PreviewIsolatedDemuxExecutionEvidence {
                        session_launches: 1,
                        ready_sessions: 1,
                        cross_request_reused_sessions: 1,
                        completed_seeks: 2,
                        completed_reads: 4,
                        packet_responses: 4,
                        clean_closes: 1,
                        peak_active_sessions: 1,
                        ..mondrian_media::PreviewIsolatedDemuxExecutionEvidence::default()
                    },
                    ..mondrian_media::PreviewDecodeExecutionProgress::default()
                }),
                ..PreviewDecodeWorkerExecutionDiagnostics::default()
            },
            ..PreviewDiagnostics::default()
        }
    }

    fn passing_playback_evidence() -> mondrian_playback::PlaybackEvidenceReport {
        let mut evidence = mondrian_playback::PlaybackEvidenceCollector::default().report();
        evidence.observed_duration_us = PROFESSIONAL_MIN_OBSERVED_DURATION_US;
        evidence.warm_seek_latency = mondrian_playback::PlaybackLatencySummary {
            count: PROFESSIONAL_MIN_WARM_SEEKS,
            sampled_count: PROFESSIONAL_MIN_WARM_SEEKS,
            p50_us: 100_000,
            p95_us: PROFESSIONAL_WARM_SEEK_P95_LIMIT_US,
            p99_us: PROFESSIONAL_WARM_SEEK_P95_LIMIT_US,
            max_us: PROFESSIONAL_WARM_SEEK_P95_LIMIT_US,
        };
        evidence.accurate_seek_latency = mondrian_playback::PlaybackLatencySummary {
            count: PROFESSIONAL_MIN_ACCURATE_SEEKS,
            sampled_count: PROFESSIONAL_MIN_ACCURATE_SEEKS,
            p50_us: 250_000,
            p95_us: PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US,
            p99_us: PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US,
            max_us: PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US,
        };
        evidence.seek_superseded_count = PROFESSIONAL_MIN_SUPERSEDED_SEEKS;
        evidence
    }

    fn passing_process_memory_evidence() -> PreviewProcessMemoryEvidenceReport {
        PreviewProcessMemoryEvidenceReport {
            scope: Some(
                mondrian_platform::ProcessMemoryScope::ProductProcessTree.as_str().to_owned(),
            ),
            backend: Some("test-product-process-tree-private-commit".to_owned()),
            discovery_available: true,
            inventory_complete: true,
            attempted_samples: 601,
            observed_samples: 601,
            probe_errors: 0,
            last_probe_error: None,
            minimum_observed_process_count: Some(3),
            maximum_observed_process_count: 4,
            maximum_inventory_attempts: 2,
            observed_duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            peak_private_committed_bytes: 768 * 1024 * 1024,
            peak_resident_bytes: 512 * 1024 * 1024,
            os_peak_resident_bytes: 512 * 1024 * 1024,
            baseline_sample_count: PROCESS_MEMORY_MIN_WINDOW_SAMPLES,
            baseline_average_private_committed_bytes: 512 * 1024 * 1024,
            final_sample_count: PROCESS_MEMORY_MIN_WINDOW_SAMPLES,
            final_average_private_committed_bytes: 544 * 1024 * 1024,
            post_stress_private_committed_bytes: Some(560 * 1024 * 1024),
        }
    }

    fn passing_native_video_gpu_timing_evidence() -> ProfessionalNativeVideoGpuTimingEvidence {
        ProfessionalNativeVideoGpuTimingEvidence {
            capability_supported: true,
            activated: true,
            inactive_reason: None,
            renderer_submitted_imports: 4,
            renderer_samples: 4,
            renderer_pending_samples: 0,
            renderer_missing_samples: 0,
            renderer_dropped_samples: 0,
            receipt_candidates: 2,
            receipt_submitted_imports: 4,
            receipt_scheduled_samples: 4,
            receipt_missing_samples: 0,
            receipt_dropped_samples: 0,
            observed_samples: 4,
            adapter_dropped_samples: 0,
            published_native_candidates: 1,
            published_native_samples: 2,
            duplicate_candidate_receipts: 0,
            duplicate_samples: 0,
            duplicate_sample_ownership: 0,
            candidate_sample_count_mismatches: 0,
            unmatched_samples: 0,
            orphan_candidate_receipts: 0,
        }
    }

    fn native_video_gpu_timing_failure_codes(
        evidence: &ProfessionalNativeVideoGpuTimingEvidence,
    ) -> Vec<&'static str> {
        let mut failures = Vec::new();
        evaluate_professional_native_video_gpu_timing(evidence, &mut failures);
        failures.into_iter().map(|failure| failure.code).collect()
    }

    fn passing_cancellation_recovery() -> PreviewCancellationRecoveryEvidence {
        PreviewCancellationRecoveryEvidence {
            stage_before_supersession: mondrian_media::PreviewDecodeExecutionStage::PacketRead,
            superseded_target_frame: 33_750,
            recovery_target_frame: 11_250,
            broker_cancellation_delta: 1,
            media_cancellation_checkpoint_delta: 1,
            isolated_termination_delta: 0,
            isolated_checkpoint_delta: 0,
            recovery_presented: true,
        }
    }

    fn process_memory_sample(
        private_committed_bytes: u64,
    ) -> mondrian_platform::ProcessMemoryProbeResult {
        mondrian_platform::ProcessMemoryProbeResult::observed(
            mondrian_platform::ProcessMemoryScope::ProductProcessTree,
            mondrian_platform::ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
            3,
            1,
            private_committed_bytes,
            private_committed_bytes.saturating_sub(64 * 1024 * 1024),
            private_committed_bytes,
        )
    }

    fn main10_media() -> PreviewPlaybackMediaProbeReport {
        PreviewPlaybackMediaProbeReport {
            source: "ffmpeg_avformat_decoder_probe",
            codec: VideoCodec::H265,
            codec_profile: VideoCodecProfile::HevcMain10,
            width: 3_840,
            height: 2_160,
            frame_rate: Rational::FPS_25,
            frame_rate_proven: true,
            pixel_format: PixelFormat::Yuv420p10le,
            pixel_format_proven: true,
            bit_depth: 10,
            video_stream_duration_us: Some(PROFESSIONAL_MIN_OBSERVED_DURATION_US),
            duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            total_frames: Some(45_000),
        }
    }
}
