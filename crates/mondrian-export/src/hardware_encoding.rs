//! Device-qualified hardware video-encoder admission.
//!
//! FFmpeg's encoder registry only proves that an implementation was compiled
//! in. Mondrian admits a hardware backend only after it matches the active
//! renderer adapter vendor and completes a bounded real encode using the exact
//! codec profile and output pixel format. Export currently feeds a CPU rawvideo
//! pipe, so a selected hardware backend still performs one CPU-to-encoder
//! upload; this module intentionally makes no zero-copy claim.

use crate::preset::{H264Profile, HevcProfile, VideoCodecConfig, VideoRateControl};
use crate::video_encoding::ResolvedVideoCodingStructure;
use mondrian_core::ExecutionCancellationToken;
use mondrian_media::{run_supervised_command, SupervisedProcessPolicy, SupervisedStreamCapture};
use std::process::Command;
use std::time::{Duration, Instant};

const NVIDIA_VENDOR_ID: u32 = 0x10de;
const INTEL_VENDOR_ID: u32 = 0x8086;
const AMD_VENDOR_ID: u32 = 0x1002;
const ENCODER_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Concrete encoder selected for one immutable export attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedVideoEncoder {
    Libx264,
    Libx265,
    LibaomAv1,
    ProResKs,
    Gif,
    Professional(crate::mezzanine::MezzanineEncoderAdapter),
    NvidiaNvenc,
    IntelQsv,
    AmdAmf,
}

impl ResolvedVideoEncoder {
    /// Return whether encoded frames execute on a hardware video engine.
    pub(crate) const fn is_hardware(self) -> bool {
        matches!(self, Self::NvidiaNvenc | Self::IntelQsv | Self::AmdAmf)
    }

    pub(crate) const fn ffmpeg_name(self, codec: &VideoCodecConfig) -> &'static str {
        match (self, codec) {
            (Self::Libx264, VideoCodecConfig::H264 { .. }) => "libx264",
            (Self::Libx265, VideoCodecConfig::Hevc { .. }) => "libx265",
            (Self::LibaomAv1, VideoCodecConfig::Av1 { .. }) => "libaom-av1",
            (Self::ProResKs, VideoCodecConfig::ProRes { .. }) => "prores_ks",
            (Self::Gif, VideoCodecConfig::Gif { .. }) => "gif",
            (Self::Professional(adapter), _) => adapter.ffmpeg_name(),
            (Self::NvidiaNvenc, VideoCodecConfig::H264 { .. }) => "h264_nvenc",
            (Self::NvidiaNvenc, VideoCodecConfig::Hevc { .. }) => "hevc_nvenc",
            (Self::IntelQsv, VideoCodecConfig::H264 { .. }) => "h264_qsv",
            (Self::IntelQsv, VideoCodecConfig::Hevc { .. }) => "hevc_qsv",
            (Self::AmdAmf, VideoCodecConfig::H264 { .. }) => "h264_amf",
            (Self::AmdAmf, VideoCodecConfig::Hevc { .. }) => "hevc_amf",
            _ => "invalid-encoder-contract",
        }
    }
}

/// Runtime adapter evidence used to qualify hardware encoder candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveGraphicsAdapterIdentity {
    pub(crate) name: String,
    pub(crate) vendor: u32,
    pub(crate) device: u32,
    pub(crate) backend: String,
}

impl From<&wgpu::AdapterInfo> for ActiveGraphicsAdapterIdentity {
    fn from(info: &wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name.clone(),
            vendor: info.vendor,
            device: info.device,
            backend: format!("{:?}", info.backend),
        }
    }
}

/// Resolve the preferred encoder. Hardware is an opportunistic acceleration,
/// never an unproved alias for the software delivery contract.
pub(crate) fn resolve_video_encoder(
    codec: &VideoCodecConfig,
    output_pixel_format: &str,
    adapter: Option<&ActiveGraphicsAdapterIdentity>,
    static_hdr_metadata: bool,
    cancellation: &ExecutionCancellationToken,
) -> mondrian_core::Result<ResolvedVideoEncoder> {
    resolve_video_encoder_with_probe(
        codec,
        adapter,
        static_hdr_metadata,
        cancellation,
        mondrian_media::ffmpeg_command,
        |command, candidate| {
            probe_encoder(command, candidate, codec, output_pixel_format, cancellation)
        },
    )
}

fn resolve_video_encoder_with_probe(
    codec: &VideoCodecConfig,
    adapter: Option<&ActiveGraphicsAdapterIdentity>,
    static_hdr_metadata: bool,
    cancellation: &ExecutionCancellationToken,
    command: impl FnOnce() -> Result<Command, mondrian_media::FfmpegCommandError>,
    probe: impl FnOnce(&mut Command, ResolvedVideoEncoder) -> Result<(), String>,
) -> mondrian_core::Result<ResolvedVideoEncoder> {
    let software = software_encoder(codec);
    if static_hdr_metadata {
        tracing::info!(
            encoder = software.ffmpeg_name(codec),
            "hardware encoder bypassed because authored static HDR metadata has an exact libx265 lowering only"
        );
        return Ok(software);
    }
    let Some(adapter) = adapter else {
        tracing::info!(
            encoder = software.ffmpeg_name(codec),
            "hardware encoder unavailable because no active renderer adapter evidence was acquired"
        );
        return Ok(software);
    };
    let Some(candidate) = hardware_candidate(codec, adapter.vendor) else {
        return Ok(software);
    };
    if cancellation.is_canceled() {
        return Err(mondrian_core::MondrianError::Cancelled);
    }
    // Failed identity admission is fatal, not evidence of an unsupported GPU.
    let mut command = command()?;
    match probe(&mut command, candidate) {
        Ok(()) => {
            tracing::info!(
                encoder = candidate.ffmpeg_name(codec),
                adapter_name = adapter.name,
                adapter_vendor = format_args!("{:#06x}", adapter.vendor),
                adapter_device = format_args!("{:#06x}", adapter.device),
                adapter_backend = adapter.backend,
                hardware = candidate.is_hardware(),
                cpu_to_encoder_uploads_per_frame = 1,
                "admitted hardware export encoder after bounded real encode probe"
            );
            Ok(candidate)
        }
        Err(error) => {
            tracing::warn!(
                encoder = candidate.ffmpeg_name(codec),
                fallback_encoder = software.ffmpeg_name(codec),
                adapter_name = adapter.name,
                reason = error,
                "hardware encoder probe failed; using proven software fallback"
            );
            Ok(software)
        }
    }
}

pub(crate) const fn software_encoder(codec: &VideoCodecConfig) -> ResolvedVideoEncoder {
    match codec {
        VideoCodecConfig::H264 { .. } => ResolvedVideoEncoder::Libx264,
        VideoCodecConfig::Hevc { .. } => ResolvedVideoEncoder::Libx265,
        VideoCodecConfig::Av1 { .. } => ResolvedVideoEncoder::LibaomAv1,
        VideoCodecConfig::ProRes { .. } => ResolvedVideoEncoder::ProResKs,
        VideoCodecConfig::DnxHr { .. }
        | VideoCodecConfig::AvcIntra { .. }
        | VideoCodecConfig::Uncompressed { .. } => {
            let Some(contract) = crate::mezzanine::professional_mezzanine_contract(codec) else {
                panic!("professional codec must resolve an encoder Adapter");
            };
            ResolvedVideoEncoder::Professional(contract.adapter)
        }
        VideoCodecConfig::Gif { .. } => ResolvedVideoEncoder::Gif,
    }
}

const fn hardware_candidate(
    codec: &VideoCodecConfig,
    adapter_vendor: u32,
) -> Option<ResolvedVideoEncoder> {
    if !matches!(
        codec,
        VideoCodecConfig::H264 { .. } | VideoCodecConfig::Hevc { .. }
    ) {
        return None;
    }
    match adapter_vendor {
        NVIDIA_VENDOR_ID => Some(ResolvedVideoEncoder::NvidiaNvenc),
        INTEL_VENDOR_ID => Some(ResolvedVideoEncoder::IntelQsv),
        AMD_VENDOR_ID => Some(ResolvedVideoEncoder::AmdAmf),
        _ => None,
    }
}

fn probe_encoder(
    command: &mut Command,
    encoder: ResolvedVideoEncoder,
    codec: &VideoCodecConfig,
    output_pixel_format: &str,
    cancellation: &ExecutionCancellationToken,
) -> Result<(), String> {
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("color=c=black:s=256x256:r=30:d=0.1")
        .arg("-frames:v")
        .arg("1")
        .arg("-an")
        .arg("-pix_fmt")
        .arg(output_pixel_format);
    apply_video_encoder_args(
        command,
        codec,
        ResolvedVideoCodingStructure::H26xLongGop {
            keyframe_interval_frames: 60,
            max_b_frames: 3,
            closed_gop: true,
            scene_cut: crate::video_encoding::VideoSceneCutPolicy::Disabled,
        },
        encoder,
    );
    command.arg("-f").arg("null").arg("-");
    let output = run_supervised_command(
        command,
        None,
        SupervisedProcessPolicy {
            stdout: SupervisedStreamCapture::Drain,
            stderr: SupervisedStreamCapture::Tail { limit_bytes: 32 * 1024 },
            deadline: Instant::now().checked_add(ENCODER_PROBE_TIMEOUT),
            ..SupervisedProcessPolicy::default()
        },
        cancellation,
    )
    .map_err(|error| format!("probe process failed: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if detail.is_empty() {
        format!("probe exited with {}", output.status)
    } else {
        detail
    })
}

pub(crate) fn apply_video_encoder_args(
    command: &mut Command,
    codec: &VideoCodecConfig,
    coding: ResolvedVideoCodingStructure,
    encoder: ResolvedVideoEncoder,
) {
    if matches!(encoder, ResolvedVideoEncoder::Professional(_)) {
        let applied = crate::mezzanine::apply_professional_mezzanine_encoder_args(command, codec);
        debug_assert!(
            applied,
            "professional encoder selected for non-professional codec"
        );
        apply_coding_structure(command, coding);
        return;
    }
    command.arg("-c:v").arg(encoder.ffmpeg_name(codec));
    match encoder {
        ResolvedVideoEncoder::Libx264 | ResolvedVideoEncoder::Libx265 => {
            command.arg("-preset").arg("medium");
            apply_profile(command, codec);
            apply_software_quality(command, codec);
        }
        ResolvedVideoEncoder::NvidiaNvenc => {
            command.arg("-preset").arg("p4").arg("-tune").arg("hq");
            apply_profile(command, codec);
            apply_nvenc_quality(command, codec);
        }
        ResolvedVideoEncoder::IntelQsv => {
            command.arg("-preset").arg("medium");
            apply_profile(command, codec);
            apply_qsv_quality(command, codec);
        }
        ResolvedVideoEncoder::AmdAmf => {
            command.arg("-quality").arg("balanced");
            apply_profile(command, codec);
            apply_amf_quality(command, codec);
        }
        ResolvedVideoEncoder::LibaomAv1 => {
            command.arg("-profile:v").arg("0").arg("-b:v").arg("0");
            apply_software_quality(command, codec);
        }
        ResolvedVideoEncoder::ProResKs => {
            if let VideoCodecConfig::ProRes { profile } = codec {
                command.arg("-profile:v").arg(super::queue::prores_profile_variant(*profile));
            }
        }
        ResolvedVideoEncoder::Gif => {}
        ResolvedVideoEncoder::Professional(_) => unreachable!("handled above"),
    }
    apply_coding_structure(command, coding);
}

fn apply_profile(command: &mut Command, codec: &VideoCodecConfig) {
    let profile = match codec {
        VideoCodecConfig::H264 { profile: H264Profile::High, .. } => "high",
        VideoCodecConfig::Hevc { profile: HevcProfile::Main, .. } => "main",
        VideoCodecConfig::Hevc { profile: HevcProfile::Main10, .. } => "main10",
        VideoCodecConfig::Av1 { .. }
        | VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::DnxHr { .. }
        | VideoCodecConfig::AvcIntra { .. }
        | VideoCodecConfig::Uncompressed { .. }
        | VideoCodecConfig::Gif { .. } => return,
    };
    command.arg("-profile:v").arg(profile);
}

fn codec_rate_control(codec: &VideoCodecConfig) -> Option<VideoRateControl> {
    match codec {
        VideoCodecConfig::H264 { rate_control, .. }
        | VideoCodecConfig::Hevc { rate_control, .. }
        | VideoCodecConfig::Av1 { rate_control, .. } => Some(*rate_control),
        VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::DnxHr { .. }
        | VideoCodecConfig::AvcIntra { .. }
        | VideoCodecConfig::Uncompressed { .. }
        | VideoCodecConfig::Gif { .. } => None,
    }
}

fn apply_software_quality(command: &mut Command, codec: &VideoCodecConfig) {
    let Some(rate) = codec_rate_control(codec) else {
        return;
    };
    command.arg("-crf").arg(rate.crf.to_string());
    apply_vbv(command, rate);
}

fn apply_nvenc_quality(command: &mut Command, codec: &VideoCodecConfig) {
    let Some(rate) = codec_rate_control(codec) else {
        return;
    };
    command
        .arg("-rc")
        .arg("vbr")
        .arg("-cq")
        .arg(rate.crf.to_string())
        .arg("-b:v")
        .arg("0");
    apply_vbv(command, rate);
}

fn apply_qsv_quality(command: &mut Command, codec: &VideoCodecConfig) {
    let Some(rate) = codec_rate_control(codec) else {
        return;
    };
    command.arg("-global_quality").arg(rate.crf.to_string()).arg("-b:v").arg("0");
    apply_vbv(command, rate);
}

fn apply_amf_quality(command: &mut Command, codec: &VideoCodecConfig) {
    let Some(rate) = codec_rate_control(codec) else {
        return;
    };
    command
        .arg("-rc")
        .arg("qvbr")
        .arg("-qvbr_quality_level")
        .arg(rate.crf.to_string());
    apply_vbv(command, rate);
}

fn apply_vbv(command: &mut Command, rate: VideoRateControl) {
    if let (Some(max_bitrate), Some(buffer_size)) = (rate.max_bitrate_kbps, rate.buffer_size_kbits)
    {
        command
            .arg("-maxrate")
            .arg(format!("{max_bitrate}k"))
            .arg("-bufsize")
            .arg(format!("{buffer_size}k"));
    }
}

fn apply_coding_structure(command: &mut Command, coding: ResolvedVideoCodingStructure) {
    match coding {
        ResolvedVideoCodingStructure::H26xLongGop {
            keyframe_interval_frames,
            max_b_frames,
            closed_gop,
            scene_cut: _,
        } => {
            command
                .arg("-g")
                .arg(keyframe_interval_frames.to_string())
                .arg("-keyint_min")
                .arg(keyframe_interval_frames.to_string())
                .arg("-bf")
                .arg(max_b_frames.to_string())
                .arg("-flags")
                .arg(if closed_gop { "+cgop" } else { "-cgop" });
        }
        ResolvedVideoCodingStructure::Av1RandomAccess {
            keyframe_interval_frames,
            lookahead_frames,
        } => {
            command
                .arg("-g")
                .arg(keyframe_interval_frames.to_string())
                .arg("-lag-in-frames")
                .arg(lookahead_frames.to_string());
        }
        ResolvedVideoCodingStructure::IntraOnly => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::VideoRateControl;

    fn h264() -> VideoCodecConfig {
        VideoCodecConfig::H264 {
            profile: H264Profile::High,
            rate_control: VideoRateControl::constant_quality(18),
        }
    }

    fn test_adapter() -> ActiveGraphicsAdapterIdentity {
        ActiveGraphicsAdapterIdentity {
            name: "protocol-only adapter".to_owned(),
            vendor: NVIDIA_VENDOR_ID,
            device: 0,
            backend: "test".to_owned(),
        }
    }

    #[cfg(feature = "validation")]
    #[test]
    fn command_admission_failure_does_not_probe_or_select_software() {
        let result = resolve_video_encoder_with_probe(
            &h264(),
            Some(&test_adapter()),
            false,
            &ExecutionCancellationToken::new(),
            || Err(mondrian_media::QualifiedFfmpegToolchainError::CapsuleNamespaceChanged.into()),
            |_, _| panic!("an unadmitted command must not reach the encoder probe"),
        );
        let error = result.expect_err("must not return a software encoder");
        let mondrian_core::MondrianError::Other(source) = error else {
            panic!("typed admission cause");
        };
        assert!(source.is::<mondrian_media::FfmpegCommandError>());
    }

    #[test]
    fn admitted_but_unsupported_hardware_retains_software_fallback() {
        let selected = resolve_video_encoder_with_probe(
            &h264(),
            Some(&test_adapter()),
            false,
            &ExecutionCancellationToken::new(),
            || Ok(Command::new("protocol-only-not-spawned")),
            |_, candidate| {
                assert_eq!(candidate, ResolvedVideoEncoder::NvidiaNvenc);
                Err("unsupported driver".to_owned())
            },
        )
        .expect("ordinary codec fallback remains legal");
        assert_eq!(selected, ResolvedVideoEncoder::Libx264);
    }

    #[test]
    fn candidates_are_qualified_by_active_adapter_vendor() {
        assert_eq!(
            hardware_candidate(&h264(), NVIDIA_VENDOR_ID),
            Some(ResolvedVideoEncoder::NvidiaNvenc)
        );
        assert_eq!(
            hardware_candidate(&h264(), INTEL_VENDOR_ID),
            Some(ResolvedVideoEncoder::IntelQsv)
        );
        assert_eq!(
            hardware_candidate(&h264(), AMD_VENDOR_ID),
            Some(ResolvedVideoEncoder::AmdAmf)
        );
        assert_eq!(hardware_candidate(&h264(), 0xffff), None);
    }

    #[test]
    fn hardware_args_do_not_claim_zero_copy_and_lower_backend_quality() {
        let mut command = Command::new("ffmpeg");
        apply_video_encoder_args(
            &mut command,
            &h264(),
            ResolvedVideoCodingStructure::H26xLongGop {
                keyframe_interval_frames: 60,
                max_b_frames: 3,
                closed_gop: true,
                scene_cut: crate::video_encoding::VideoSceneCutPolicy::Disabled,
            },
            ResolvedVideoEncoder::NvidiaNvenc,
        );
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "h264_nvenc"]));
        assert!(args.windows(2).any(|pair| pair == ["-cq", "18"]));
        assert!(args.windows(2).any(|pair| pair == ["-g", "60"]));
    }

    #[test]
    #[ignore = "manual real adapter/driver/FFmpeg hardware encoder qualification"]
    fn active_renderer_adapter_completes_hardware_encoder_probe() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("GPU probe runtime");
        let context = runtime
            .block_on(mondrian_renderer::GpuContext::new())
            .expect("active renderer adapter");
        let adapter = ActiveGraphicsAdapterIdentity::from(&context.adapter.get_info());
        let expected = hardware_candidate(&h264(), adapter.vendor);
        let selected = resolve_video_encoder(
            &h264(),
            "yuv420p",
            Some(&adapter),
            false,
            &ExecutionCancellationToken::new(),
        )
        .expect("bounded encoder resolution");
        eprintln!("adapter={adapter:?} expected={expected:?} selected={selected:?}");
        if let Some(expected) = expected {
            assert_eq!(
                selected, expected,
                "known GPU vendor must pass its real encoder probe"
            );
        } else {
            assert_eq!(selected, ResolvedVideoEncoder::Libx264);
        }
    }
}
