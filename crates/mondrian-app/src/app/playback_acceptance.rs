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
};

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
    fallback_access_mode_unsupported_frames: u64,
    fallback_backend_unavailable_frames: u64,
    fallback_codec_unsupported_frames: u64,
    fallback_device_context_unavailable_frames: u64,
    fallback_setup_failed_frames: u64,
    fallback_decoder_open_failed_frames: u64,
    fallback_awaiting_hardware_frame_frames: u64,
    fallback_backend_boundary_frames: u64,
    fallback_adapter_unavailable_frames: u64,
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
    pub frames: usize,
    pub frame_interval_ns: u64,
}

pub(crate) fn evaluate_professional_playback(
    observation: ProfessionalPlaybackObservation<'_>,
    required_hardware_execution_percent: usize,
) -> PreviewProfessionalPlaybackGateReport {
    let media = observation.media;
    let mut failures = Vec::new();
    let expected_frame_rates = [Rational::FPS_25, Rational::FPS_2997, Rational::FPS_30];
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
    } else if !expected_frame_rates.contains(&media.frame_rate) {
        push_failure(
            &mut failures,
            "media_frame_rate_mismatch",
            "25, 30000/1001, or 30 fps",
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
    let required_duration_us = (observation.frames as u128)
        .saturating_mul(observation.frame_interval_ns as u128)
        .saturating_add(999)
        .checked_div(1_000)
        .unwrap_or(u128::MAX)
        .min(u64::MAX as u128) as u64;
    if media.duration_us < required_duration_us {
        push_failure(
            &mut failures,
            "media_duration_insufficient",
            format!("at least {required_duration_us} us"),
            format!("{} us", media.duration_us),
            media.source,
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
        fallback_access_mode_unsupported_frames: playback
            .hardware_decode_access_mode_unsupported_frames,
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
            frames: 100,
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
            duration_us: 10_000_000,
            total_frames: Some(250),
        }
    }
}
