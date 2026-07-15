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

use crate::app_ui::preview::{
    AppUiPreviewDecodeAccessModeProfile, AppUiPreviewDecodeExecutionSummary,
    AppUiPreviewDiagnostics,
};

pub(crate) const PROFESSIONAL_MIN_OBSERVED_DURATION_US: u64 = 30 * 60 * 1_000_000;
pub(crate) const PROFESSIONAL_MIN_WARM_SEEKS: u64 = 50;
pub(crate) const PROFESSIONAL_MIN_ACCURATE_SEEKS: u64 = 50;
pub(crate) const PROFESSIONAL_MIN_SUPERSEDED_SEEKS: u64 = 99;
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
        anyhow::ensure!(
            self.duration_us >= required_duration_us,
            "professional playback observation requires at least {required_duration_us} us of source media, but the probe provides {} us",
            self.duration_us
        );
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
    dropped_evidence_events: u64,
    dropped_evidence_samples: u64,
    broker_pending_requests: usize,
    broker_queued_jobs: usize,
    broker_in_flight_jobs: usize,
    cpu_frame_store_within_budget: bool,
    cpu_frame_store_oversize_rejections: u64,
    pub(crate) passed: bool,
    pub(crate) failures: Vec<PreviewAcceptanceFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PreviewAcceptanceFailure {
    code: &'static str,
    expected: String,
    observed: String,
    evidence: String,
}

pub(crate) struct ProfessionalPlaybackObservation<'a> {
    pub media: &'a PreviewPlaybackMediaProbeReport,
    pub rendered_decode_execution: AppUiPreviewDecodeExecutionSummary,
    pub viewer_fallback_count: usize,
    pub viewer_fallback_reasons: &'a [String],
    pub playback_decode: AppUiPreviewDecodeAccessModeProfile,
    pub playback_evidence: &'a mondrian_playback::PlaybackEvidenceReport,
    pub preview_diagnostics: &'a AppUiPreviewDiagnostics,
    pub frames: usize,
    pub frame_interval_ns: u64,
}

pub(crate) fn evaluate_professional_playback(
    observation: ProfessionalPlaybackObservation<'_>,
    required_hardware_execution_percent: usize,
) -> PreviewProfessionalPlaybackGateReport {
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
    if evidence.dropped_event_count > 0 || evidence.dropped_sample_count > 0 {
        push_failure(
            &mut failures,
            "playback_evidence_overflow",
            "0 dropped evidence events and samples",
            format!(
                "events={}, samples={}",
                evidence.dropped_event_count, evidence.dropped_sample_count
            ),
            "Playback Evidence retention diagnostics",
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
        profile: "uhd_hevc_main10_hardware_1x_v1",
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
        dropped_evidence_events: evidence.dropped_event_count,
        dropped_evidence_samples: evidence.dropped_sample_count,
        broker_pending_requests: diagnostics.scheduler.pending_requests,
        broker_queued_jobs: diagnostics.worker_queue.queued_jobs,
        broker_in_flight_jobs: diagnostics.worker_queue.in_flight_jobs,
        cpu_frame_store_within_budget,
        cpu_frame_store_oversize_rejections,
        passed: failures.is_empty(),
        failures,
    }
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

    #[test]
    fn accepts_presented_main10_hardware_execution() {
        let media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = AppUiPreviewDiagnostics::default();
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: AppUiPreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_cpu_transfer_layers: 60,
                hardware_native_layers: 40,
                p010_10_bit_hardware_layers: 100,
                ..AppUiPreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: AppUiPreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            preview_diagnostics: &diagnostics,
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation, 90);

        assert!(report.passed, "{:?}", report.failures);
        assert_eq!(report.presented_hardware_layers, 100);
        assert_eq!(report.hardware_execution_percent, 100);
    }

    #[test]
    fn rejects_unproven_identity_and_software_presentation() {
        let mut media = main10_media();
        let evidence = passing_playback_evidence();
        let diagnostics = AppUiPreviewDiagnostics::default();
        media.codec_profile = VideoCodecProfile::Unknown;
        media.frame_rate_proven = false;
        media.pixel_format_proven = false;
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: AppUiPreviewDecodeExecutionSummary {
                media_layers: 10,
                software_cpu_layers: 10,
                ..AppUiPreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: AppUiPreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            preview_diagnostics: &diagnostics,
            frames: 10,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation, 90);
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
    fn preflight_rejects_media_shorter_than_observation_window() {
        let mut media = main10_media();
        media.duration_us = 2_880_000;

        let error = media
            .ensure_observation_coverage(45_000, 40_000_000)
            .expect_err("short source must not start a thirty-minute gate");

        assert!(error.to_string().contains("requires at least 1800000000 us"));
        assert!(error.to_string().contains("provides 2880000 us"));
    }

    #[test]
    fn professional_gate_accepts_cinema_broadcast_and_high_frame_rates() {
        for frame_rate in PROFESSIONAL_FRAME_RATES {
            let mut media = main10_media();
            media.frame_rate = frame_rate;
            let evidence = passing_playback_evidence();
            let diagnostics = AppUiPreviewDiagnostics::default();
            let report = evaluate_professional_playback(
                ProfessionalPlaybackObservation {
                    media: &media,
                    rendered_decode_execution: AppUiPreviewDecodeExecutionSummary {
                        media_layers: 100,
                        hardware_native_layers: 100,
                        p010_10_bit_hardware_layers: 100,
                        ..AppUiPreviewDecodeExecutionSummary::default()
                    },
                    viewer_fallback_count: 0,
                    viewer_fallback_reasons: &[],
                    playback_decode: AppUiPreviewDecodeAccessModeProfile::default(),
                    playback_evidence: &evidence,
                    preview_diagnostics: &diagnostics,
                    frames: 45_000,
                    frame_interval_ns: 40_000_000,
                },
                90,
            );

            assert!(report.passed, "{frame_rate}: {:?}", report.failures);
        }
    }

    #[test]
    fn rejects_short_run_missing_seek_coverage_and_rejected_old_delivery() {
        let media = main10_media();
        let mut evidence = mondrian_playback::PlaybackEvidenceCollector::default().report();
        let mut diagnostics = AppUiPreviewDiagnostics::default();
        diagnostics.scheduler.pending_requests = 1;
        diagnostics.worker_queue.queued_jobs = 1;
        diagnostics.worker_queue.in_flight_jobs = 1;
        evidence.observed_duration_us = 10_000_000;
        evidence.deliveries.rejected = 1;
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: AppUiPreviewDecodeExecutionSummary {
                media_layers: 10,
                hardware_native_layers: 10,
                p010_10_bit_hardware_layers: 10,
                ..AppUiPreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: AppUiPreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            preview_diagnostics: &diagnostics,
            frames: 10,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation, 90);
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"playback_duration_below_minimum"));
        assert!(codes.contains(&"warm_seek_coverage_below_minimum"));
        assert!(codes.contains(&"accurate_seek_coverage_below_minimum"));
        assert!(codes.contains(&"latest_wins_seek_coverage_below_minimum"));
        assert!(codes.contains(&"rejected_terminal_delivery_observed"));
        assert!(codes.contains(&"frame_work_not_quiescent"));
    }

    #[test]
    fn rejects_seek_latency_above_professional_p95_limits() {
        let media = main10_media();
        let mut evidence = passing_playback_evidence();
        let diagnostics = AppUiPreviewDiagnostics::default();
        evidence.warm_seek_latency.p95_us = PROFESSIONAL_WARM_SEEK_P95_LIMIT_US + 1;
        evidence.accurate_seek_latency.p95_us = PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US + 1;
        let observation = ProfessionalPlaybackObservation {
            media: &media,
            rendered_decode_execution: AppUiPreviewDecodeExecutionSummary {
                media_layers: 100,
                hardware_native_layers: 100,
                p010_10_bit_hardware_layers: 100,
                ..AppUiPreviewDecodeExecutionSummary::default()
            },
            viewer_fallback_count: 0,
            viewer_fallback_reasons: &[],
            playback_decode: AppUiPreviewDecodeAccessModeProfile::default(),
            playback_evidence: &evidence,
            preview_diagnostics: &diagnostics,
            frames: 45_000,
            frame_interval_ns: 40_000_000,
        };

        let report = evaluate_professional_playback(observation, 90);
        let codes: Vec<_> = report.failures.iter().map(|failure| failure.code).collect();
        assert!(codes.contains(&"warm_seek_p95_above_limit"));
        assert!(codes.contains(&"accurate_seek_p95_above_limit"));
    }

    fn passing_playback_evidence() -> mondrian_playback::PlaybackEvidenceReport {
        let mut evidence = mondrian_playback::PlaybackEvidenceCollector::default().report();
        evidence.observed_duration_us = PROFESSIONAL_MIN_OBSERVED_DURATION_US;
        evidence.warm_seek_latency = mondrian_playback::PlaybackLatencySummary {
            count: PROFESSIONAL_MIN_WARM_SEEKS,
            p50_us: 100_000,
            p95_us: PROFESSIONAL_WARM_SEEK_P95_LIMIT_US,
            p99_us: PROFESSIONAL_WARM_SEEK_P95_LIMIT_US,
            max_us: PROFESSIONAL_WARM_SEEK_P95_LIMIT_US,
        };
        evidence.accurate_seek_latency = mondrian_playback::PlaybackLatencySummary {
            count: PROFESSIONAL_MIN_ACCURATE_SEEKS,
            p50_us: 250_000,
            p95_us: PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US,
            p99_us: PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US,
            max_us: PROFESSIONAL_ACCURATE_SEEK_P95_LIMIT_US,
        };
        evidence.seek_superseded_count = PROFESSIONAL_MIN_SUPERSEDED_SEEKS;
        evidence
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
            duration_us: PROFESSIONAL_MIN_OBSERVED_DURATION_US,
            total_frames: Some(45_000),
        }
    }
}
