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

const PROCESS_MEMORY_WARMUP_END_US: u64 = 5 * 60 * 1_000_000;
const PROCESS_MEMORY_BASELINE_END_US: u64 = 10 * 60 * 1_000_000;
const PROCESS_MEMORY_FINAL_WINDOW_START_US: u64 = 25 * 60 * 1_000_000;
const PROCESS_MEMORY_MIN_WINDOW_SAMPLES: u64 = 240;
const PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES: u64 = 256 * 1024 * 1024;

use super::preview_access_mode::{
    MediaPreviewJobQueueDiagnostics, MediaPreviewSchedulerDiagnostics,
};

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
    min_warm_seeks: u64,
    warm_seek_count: u64,
    warm_seek_p95_limit_us: u64,
    warm_seek_p95_observed_us: u64,
    min_accurate_seeks: u64,
    accurate_seek_count: u64,
    accurate_seek_p95_limit_us: u64,
    accurate_seek_p95_observed_us: u64,
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
    pub(crate) passed: bool,
    pub(crate) failures: Vec<PreviewAcceptanceFailure>,
}

/// Bounded, process-wide memory evidence collected by a native platform Adapter.
///
/// The collector keeps only scalar aggregates. It deliberately uses private
/// committed memory for acceptance and reports the reclaimable resident set as
/// diagnostics rather than treating it as application ownership.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct PreviewProcessMemoryEvidenceReport {
    backend: Option<String>,
    discovery_available: bool,
    attempted_samples: u64,
    observed_samples: u64,
    probe_errors: u64,
    last_probe_error: Option<String>,
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
    max_private_committed_bytes: u64,
    max_settled_growth_bytes: u64,
    baseline_sample_count: u64,
    final_sample_count: u64,
    baseline_average_private_committed_bytes: u64,
    final_average_private_committed_bytes: u64,
    settled_growth_bytes: u64,
    post_stress_private_committed_bytes: Option<u64>,
    post_stress_growth_bytes: Option<u64>,
    passed: bool,
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

/// UI-independent point-in-time execution facts required by professional acceptance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewRuntimeAcceptanceEvidence {
    pub(crate) scheduler: MediaPreviewSchedulerDiagnostics,
    pub(crate) worker_queue: MediaPreviewJobQueueDiagnostics,
    pub(crate) media_cache_reserved_bytes: usize,
    pub(crate) media_cache_byte_budget: usize,
    pub(crate) media_cache_oversize_rejections: u64,
    pub(crate) viewer_frame_cache_reserved_bytes: usize,
    pub(crate) viewer_frame_cache_byte_budget: usize,
    pub(crate) viewer_frame_cache_oversize_rejections: u64,
    pub(crate) pinned_viewer_frame_bytes: usize,
    pub(crate) pinned_media_frame_bytes: usize,
    pub(crate) decode_cancellation: mondrian_playback::FrameCancellationEvidenceReport,
}

pub(crate) struct ProfessionalPlaybackObservation<'a> {
    pub media: &'a PreviewPlaybackMediaProbeReport,
    pub rendered_decode_execution: PresentedDecodeExecutionEvidence,
    pub viewer_fallback_count: usize,
    pub viewer_fallback_reasons: &'a [String],
    pub playback_decode: PlaybackDecodeExecutionEvidence,
    pub playback_evidence: &'a mondrian_playback::PlaybackEvidenceReport,
    pub preview_diagnostics: &'a PreviewRuntimeAcceptanceEvidence,
    pub process_memory: &'a PreviewProcessMemoryEvidenceReport,
    pub frames: usize,
    pub frame_interval_ns: u64,
}

pub(crate) fn evaluate_professional_playback(
    observation: ProfessionalPlaybackObservation<'_>,
) -> PreviewProfessionalPlaybackGateReport {
    let required_hardware_execution_percent = PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT;
    let media = observation.media;
    let mut failures = Vec::new();
    if media.codec != VideoCodec::H265 {
        push_failure(
            &mut failures,
            "media_codec_mismatch",
            "HEVC/H.265",
            format!("{:?}", media.codec),
            media.source,
        );
    }
    if media.codec_profile != VideoCodecProfile::HevcMain10 {
        push_failure(
            &mut failures,
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
            &mut failures,
            "media_resolution_mismatch",
            "at least 3840x2160",
            format!("{}x{}", media.width, media.height),
            media.source,
        );
    }
    if !media.frame_rate_proven {
        push_failure(
            &mut failures,
            "media_frame_rate_unknown",
            "positive FFmpeg average frame-rate evidence",
            media.frame_rate.to_string(),
            media.source,
        );
    } else if !PROFESSIONAL_FRAME_RATES.contains(&media.frame_rate) {
        push_failure(
            &mut failures,
            "media_frame_rate_mismatch",
            "24000/1001, 24, 25, 30000/1001, 30, 50, 60000/1001, or 60 fps",
            media.frame_rate.to_string(),
            media.source,
        );
    }
    if !media.pixel_format_proven {
        push_failure(
            &mut failures,
            "media_pixel_format_unknown",
            "decoder-proven 10-bit pixel format",
            format!("fallback={:?}", media.pixel_format),
            media.source,
        );
    }
    if media.bit_depth < 10 {
        push_failure(
            &mut failures,
            "media_bit_depth_mismatch",
            "at least 10 bit",
            media.bit_depth.to_string(),
            media.source,
        );
    }
    let required_duration_us =
        required_media_duration_us(observation.frames, observation.frame_interval_ns);
    if media.duration_us < required_duration_us {
        push_failure(
            &mut failures,
            "media_duration_insufficient",
            format!("at least {required_duration_us} us"),
            format!("{} us", media.duration_us),
            media.source,
        );
    }
    match media.video_stream_duration_us {
        None => push_failure(
            &mut failures,
            "media_video_stream_duration_unproven",
            format!("at least {required_duration_us} us of primary-video stream duration"),
            "unknown",
            media.source,
        ),
        Some(duration_us) if duration_us < required_duration_us => push_failure(
            &mut failures,
            "media_video_stream_duration_insufficient",
            format!("at least {required_duration_us} us"),
            format!("{duration_us} us"),
            media.source,
        ),
        Some(_) => {}
    }
    if let Some(total_frames) = media.total_frames {
        if total_frames < observation.frames as u64 {
            push_failure(
                &mut failures,
                "media_video_frame_count_insufficient",
                format!("at least {} frames", observation.frames),
                format!("{total_frames} frames"),
                media.source,
            );
        }
    }

    let evidence = observation.playback_evidence;
    if evidence.observed_duration_us < PROFESSIONAL_MIN_OBSERVED_DURATION_US {
        push_failure(
            &mut failures,
            "playback_duration_below_minimum",
            format!("at least {PROFESSIONAL_MIN_OBSERVED_DURATION_US} us"),
            format!("{} us", evidence.observed_duration_us),
            "Playback Evidence monotonic observation span",
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
    let diagnostics = observation.preview_diagnostics;
    if diagnostics.scheduler.pending_requests > 0
        || diagnostics.worker_queue.queued_jobs > 0
        || diagnostics.worker_queue.in_flight_jobs > 0
    {
        push_failure(
            &mut failures,
            "frame_work_not_quiescent",
            "0 pending bindings, queued jobs, and execution leases",
            format!(
                "pending={}, queued={}, in_flight={}",
                diagnostics.scheduler.pending_requests,
                diagnostics.worker_queue.queued_jobs,
                diagnostics.worker_queue.in_flight_jobs
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
    let cpu_frame_store_within_budget = diagnostics.media_cache_reserved_bytes
        <= diagnostics.media_cache_byte_budget
        && diagnostics.pinned_media_frame_bytes <= diagnostics.media_cache_byte_budget
        && diagnostics.viewer_frame_cache_reserved_bytes
            <= diagnostics.viewer_frame_cache_byte_budget
        && diagnostics.pinned_viewer_frame_bytes <= diagnostics.viewer_frame_cache_byte_budget;
    if !cpu_frame_store_within_budget {
        push_failure(
            &mut failures,
            "cpu_frame_store_budget_exceeded",
            "all evictable and pinned CPU residency within declared byte budgets",
            format!(
                "media={}/{}, pinned_media={}, viewer={}/{}, pinned_viewer={}",
                diagnostics.media_cache_reserved_bytes,
                diagnostics.media_cache_byte_budget,
                diagnostics.pinned_media_frame_bytes,
                diagnostics.viewer_frame_cache_reserved_bytes,
                diagnostics.viewer_frame_cache_byte_budget,
                diagnostics.pinned_viewer_frame_bytes
            ),
            "Preview Frame Store structured diagnostics",
        );
    }
    let cpu_frame_store_oversize_rejections = diagnostics
        .media_cache_oversize_rejections
        .saturating_add(diagnostics.viewer_frame_cache_oversize_rejections);
    if cpu_frame_store_oversize_rejections > 0 {
        push_failure(
            &mut failures,
            "cpu_frame_store_oversize_rejection",
            "0 oversize CPU payload rejections",
            cpu_frame_store_oversize_rejections.to_string(),
            "Preview Frame Store admission diagnostics",
        );
    }
    let process_memory = evaluate_process_memory(observation.process_memory, &mut failures);
    let cancellation_gate = mondrian_playback::evaluate_frame_cancellation(
        diagnostics.decode_cancellation,
        mondrian_playback::FrameCancellationPolicy::default(),
    );
    for failure in &cancellation_gate.failures {
        let code = match failure.kind {
            mondrian_playback::FrameCancellationGateFailureKind::UnknownCause => {
                "frame_cancellation_unknown_cause"
            }
            mondrian_playback::FrameCancellationGateFailureKind::MissingRequestToCheckpoint => {
                "frame_cancellation_request_evidence_missing"
            }
            mondrian_playback::FrameCancellationGateFailureKind::MissingExecutionToCheckpoint => {
                "frame_cancellation_checkpoint_evidence_missing"
            }
            mondrian_playback::FrameCancellationGateFailureKind::InvalidTimingOrder => {
                "frame_cancellation_timing_invalid"
            }
            mondrian_playback::FrameCancellationGateFailureKind::RequestToCheckpointExceeded => {
                "frame_cancellation_checkpoint_late"
            }
            mondrian_playback::FrameCancellationGateFailureKind::CheckpointToReturnExceeded => {
                "frame_cancellation_return_late"
            }
        };
        push_failure(
            &mut failures,
            code,
            format!("at most {} for {:?}", failure.limit, failure.work_class),
            failure.observed.to_string(),
            "Frame Cancellation Evidence evaluated by the playback-owned policy",
        );
    }

    let execution = observation.rendered_decode_execution;
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
            &mut failures,
            "hardware_decode_not_observed",
            "presented media layers with frame-local decode provenance",
            "0 layers",
            "headless Viewer GPU completion records",
        );
    } else if hardware_execution_percent < required_hardware_execution_percent as u64 {
        push_failure(
            &mut failures,
            "hardware_decode_coverage_below_minimum",
            format!("at least {required_hardware_execution_percent}%"),
            format!("{hardware_execution_percent}%"),
            "presented Frame Demand candidates only; prefetch aggregates excluded",
        );
    }
    if presented_media_layers > 0
        && p010_execution_percent < required_hardware_execution_percent as u64
    {
        push_failure(
            &mut failures,
            "hardware_main10_surface_coverage_below_minimum",
            format!("at least {required_hardware_execution_percent}% P010/10-bit hardware"),
            format!("{p010_execution_percent}%"),
            "frame-local decoded surface and sampling evidence",
        );
    }
    if observation.viewer_fallback_count > 0 {
        push_failure(
            &mut failures,
            "viewer_gpu_fallback_observed",
            "0 Viewer GPU input fallbacks",
            observation.viewer_fallback_count.to_string(),
            observation.viewer_fallback_reasons.join(" | "),
        );
    }

    let playback = observation.playback_decode;
    let hardware_requested_frames = playback
        .hardware_decode_prefer_hardware_requested_frames
        .saturating_add(playback.hardware_decode_prefer_gpu_requested_frames)
        .saturating_add(playback.hardware_decode_require_gpu_requested_frames);
    PreviewProfessionalPlaybackGateReport {
        profile: "uhd_hevc_main10_hardware_1x_v4",
        required_hardware_execution_percent,
        presented_media_layers,
        presented_hardware_layers,
        presented_hardware_cpu_transfer_layers,
        presented_hardware_native_layers,
        presented_p010_10_bit_hardware_layers,
        hardware_execution_percent,
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
        observed_duration_us: evidence.observed_duration_us,
        min_warm_seeks: PROFESSIONAL_MIN_WARM_SEEKS,
        warm_seek_count: evidence.warm_seek_latency.count,
        warm_seek_p95_limit_us: PROFESSIONAL_WARM_SEEK_P95_LIMIT_US,
        warm_seek_p95_observed_us: evidence.warm_seek_latency.p95_us,
        min_accurate_seeks: PROFESSIONAL_MIN_ACCURATE_SEEKS,
        accurate_seek_count: evidence.accurate_seek_latency.count,
        accurate_seek_p95_limit_us: PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US,
        accurate_seek_p95_observed_us: evidence.accurate_seek_latency.p95_us,
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
        passed: failures.is_empty(),
        failures,
    }
}

fn evaluate_process_memory(
    evidence: &PreviewProcessMemoryEvidenceReport,
    failures: &mut Vec<PreviewAcceptanceFailure>,
) -> PreviewProcessMemoryGateReport {
    if !evidence.discovery_available || evidence.backend.is_none() {
        push_failure(
            failures,
            "process_memory_probe_unavailable",
            "native private-commit process-memory evidence",
            evidence.backend.as_deref().unwrap_or("unavailable"),
            evidence.last_probe_error.as_deref().unwrap_or("no native backend"),
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
            "process-memory evidence observation span",
        );
    }
    if evidence.baseline_sample_count < PROCESS_MEMORY_MIN_WINDOW_SAMPLES {
        push_failure(
            failures,
            "process_memory_baseline_coverage_below_minimum",
            format!("at least {PROCESS_MEMORY_MIN_WINDOW_SAMPLES} samples from minutes 5-10"),
            evidence.baseline_sample_count.to_string(),
            "fixed-cadence private-commit samples",
        );
    }
    if evidence.final_sample_count < PROCESS_MEMORY_MIN_WINDOW_SAMPLES {
        push_failure(
            failures,
            "process_memory_final_coverage_below_minimum",
            format!("at least {PROCESS_MEMORY_MIN_WINDOW_SAMPLES} samples from minutes 25-30"),
            evidence.final_sample_count.to_string(),
            "fixed-cadence private-commit samples",
        );
    }
    if evidence.peak_private_committed_bytes > PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES {
        push_failure(
            failures,
            "process_memory_private_commit_above_limit",
            format!("at most {PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES} bytes"),
            format!("{} bytes", evidence.peak_private_committed_bytes),
            "native process private-commit high-water mark",
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
                    "post-stress native process private commit",
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

    let passed = !failures.iter().any(|failure| failure.code.starts_with("process_memory_"));
    PreviewProcessMemoryGateReport {
        profile: "whole_process_private_commit_v1",
        max_private_committed_bytes: PROCESS_MEMORY_MAX_PRIVATE_COMMITTED_BYTES,
        max_settled_growth_bytes: PROCESS_MEMORY_MAX_SETTLED_GROWTH_BYTES,
        baseline_sample_count: evidence.baseline_sample_count,
        final_sample_count: evidence.final_sample_count,
        baseline_average_private_committed_bytes: evidence.baseline_average_private_committed_bytes,
        final_average_private_committed_bytes: evidence.final_average_private_committed_bytes,
        settled_growth_bytes,
        post_stress_private_committed_bytes: evidence.post_stress_private_committed_bytes,
        post_stress_growth_bytes,
        passed,
    }
}

/// Evaluate the shared whole-process memory contract without coupling another
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
    fn accepts_presented_main10_hardware_execution() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = PreviewDiagnostics::default();
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
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);

        assert!(report.passed, "{:?}", report.failures);
        assert_eq!(report.profile, "uhd_hevc_main10_hardware_1x_v4");
        assert_eq!(
            report.required_hardware_execution_percent,
            PROFESSIONAL_REQUIRED_HARDWARE_EXECUTION_PERCENT
        );
        assert_eq!(report.presented_hardware_layers, 100);
        assert_eq!(report.hardware_execution_percent, 100);
    }

    #[test]
    fn rejects_playback_when_shared_cancellation_policy_fails() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let mut cancellation = mondrian_playback::FrameCancellationEvidenceCollector::default();
        cancellation.observe(mondrian_playback::FrameCancellationObservation {
            work_class: mondrian_playback::FrameWorkClass::Interactive,
            cause: mondrian_playback::FrameCancellationCause::Superseded,
            execution_duration: std::time::Duration::from_millis(90),
            execution_to_checkpoint: Some(std::time::Duration::from_millis(20)),
            request_to_checkpoint: Some(std::time::Duration::from_millis(1)),
        });
        let diagnostics = PreviewDiagnostics {
            decode_cancellation: cancellation.report(),
            ..PreviewDiagnostics::default()
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
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);

        assert!(!report.passed);
        assert!(!report.cancellation_gate.passed);
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.code == "frame_cancellation_return_late"));
    }

    #[test]
    fn rejects_unproven_identity_and_software_presentation() {
        let mut media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = PreviewDiagnostics::default();
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
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
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
        let diagnostics = PreviewDiagnostics::default();
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
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
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
            let diagnostics = PreviewDiagnostics::default();
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
                preview_diagnostics: &diagnostics,
                process_memory: &passing_process_memory_evidence(),
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
        let mut diagnostics = PreviewDiagnostics::default();
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
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
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
        let diagnostics = PreviewDiagnostics::default();
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
            preview_diagnostics: &diagnostics,
            process_memory: &passing_process_memory_evidence(),
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation);
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"warm_seek_p95_above_limit"));
        assert!(codes.contains(&"accurate_seek_p95_above_limit"));
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
    }

    #[test]
    fn rejects_process_memory_growth_that_does_not_plateau() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = PreviewDiagnostics::default();
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
            preview_diagnostics: &diagnostics,
            process_memory: &process_memory,
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        });

        assert!(!report.process_memory.passed);
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.code == "process_memory_did_not_plateau"));
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
            backend: Some("test-private-commit".to_owned()),
            discovery_available: true,
            attempted_samples: 601,
            observed_samples: 601,
            probe_errors: 0,
            last_probe_error: None,
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

    fn process_memory_sample(
        private_committed_bytes: u64,
    ) -> mondrian_platform::ProcessMemoryProbeResult {
        mondrian_platform::ProcessMemoryProbeResult::observed(
            mondrian_platform::ProcessMemoryProbeBackend::WindowsProcessStatus,
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
