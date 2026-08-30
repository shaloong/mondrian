//! Professional mezzanine codec ownership.
//!
//! This Module is the single policy and lowering authority for DNxHR,
//! AVC-Intra, and uncompressed RGB/YUV export. Delivery admission, the FFmpeg
//! encoder Adapter, product UI, and finished-output validation consume this
//! contract instead of rebuilding profile/container/pixel-format facts.
//!
//! XAVC is deliberately not represented. The qualified FFmpeg MXF Adapter can
//! carry generic H.264 essence but exposes no XAVC-specific muxing contract;
//! labeling that output as XAVC would be an unverifiable product claim.

use crate::preset::{
    AvcIntraClass, Container, DnxHrProfile, ExportChromaSampling, Resolution,
    UncompressedVideoFormat, VideoCodecConfig,
};
use mondrian_core::{Rational, SampleAspectRatio};
use mondrian_timeline::sequence::{DeliveryBitDepth, VideoRange};
use std::process::Command;

/// Product-authoring defaults for one professional codec choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfessionalMezzanineAuthoringDefaults {
    /// Preferred container with qualified execution and validation.
    pub container: Container,
    /// Exact profile-owned bit depth.
    pub bit_depth: DeliveryBitDepth,
    /// Exact profile-owned range.
    pub video_range: VideoRange,
    /// Exact profile-owned RGB/YUV representation.
    pub chroma_sampling: ExportChromaSampling,
    /// Fixed raster when the codec class requires one.
    pub resolution: Option<Resolution>,
    /// Fixed cadence when the product preset selects one interoperable mode.
    pub frame_rate: Option<Rational>,
    /// Whether the qualified container contract is video-only.
    pub video_only: bool,
}

/// Human-readable product identity for a professional codec choice.
pub const fn professional_mezzanine_label(codec: &VideoCodecConfig) -> Option<&'static str> {
    match codec {
        VideoCodecConfig::DnxHr { profile } => Some(match profile {
            DnxHrProfile::Lb => "DNxHR LB",
            DnxHrProfile::Sq => "DNxHR SQ",
            DnxHrProfile::Hq => "DNxHR HQ",
            DnxHrProfile::Hqx => "DNxHR HQX",
            DnxHrProfile::FourFourFour => "DNxHR 444 RGB",
        }),
        VideoCodecConfig::AvcIntra { class } => Some(match class {
            AvcIntraClass::Class100 => "AVC-Intra Class 100",
            AvcIntraClass::Class200 => "AVC-Intra Class 200",
        }),
        VideoCodecConfig::Uncompressed { format } => Some(match format {
            UncompressedVideoFormat::Yuv422Eight => "Uncompressed YUV 4:2:2 8-bit (2vuy)",
            UncompressedVideoFormat::Yuv422Ten => "Uncompressed YUV 4:2:2 10-bit (v210)",
            UncompressedVideoFormat::RgbEight => "Uncompressed RGB 8-bit",
            UncompressedVideoFormat::RgbTen => "Uncompressed RGB 10-bit (r210)",
        }),
        VideoCodecConfig::H264 { .. }
        | VideoCodecConfig::Hevc { .. }
        | VideoCodecConfig::Av1 { .. }
        | VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::Gif { .. } => None,
    }
}

/// Return the compatible authoring defaults used when a frontend selects one
/// professional codec. Delivery admission remains the final authority.
pub fn professional_mezzanine_authoring_defaults(
    codec: &VideoCodecConfig,
) -> Option<ProfessionalMezzanineAuthoringDefaults> {
    let contract = professional_mezzanine_contract(codec)?;
    let (container, video_range, resolution, frame_rate) = match codec {
        VideoCodecConfig::DnxHr { .. } => (
            Container::Mov,
            if contract.chroma_sampling == ExportChromaSampling::Rgb {
                VideoRange::Full
            } else {
                VideoRange::Legal
            },
            None,
            None,
        ),
        VideoCodecConfig::AvcIntra { .. } => (
            Container::Mxf,
            VideoRange::Legal,
            Some(Resolution { width: 1920, height: 1080 }),
            Some(Rational::FPS_25),
        ),
        VideoCodecConfig::Uncompressed { .. } => (
            Container::Mov,
            if contract.chroma_sampling == ExportChromaSampling::Rgb {
                VideoRange::Full
            } else {
                VideoRange::Legal
            },
            None,
            None,
        ),
        VideoCodecConfig::H264 { .. }
        | VideoCodecConfig::Hevc { .. }
        | VideoCodecConfig::Av1 { .. }
        | VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::Gif { .. } => return None,
    };
    Some(ProfessionalMezzanineAuthoringDefaults {
        container,
        bit_depth: contract.bit_depth,
        video_range,
        chroma_sampling: contract.chroma_sampling,
        resolution,
        frame_rate,
        video_only: container == Container::Mxf,
    })
}

/// Concrete FFmpeg encoder Adapter owned by one professional representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MezzanineEncoderAdapter {
    DnxHd,
    Libx264AvcIntra,
    RawVideo,
    V210,
    R210,
}

impl MezzanineEncoderAdapter {
    pub(crate) const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::DnxHd => "dnxhd",
            Self::Libx264AvcIntra => "libx264",
            Self::RawVideo => "rawvideo",
            Self::V210 => "v210",
            Self::R210 => "r210",
        }
    }
}

/// Exact execution and validation contract for one professional essence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProfessionalMezzanineContract {
    pub(crate) adapter: MezzanineEncoderAdapter,
    pub(crate) output_pixel_format: &'static str,
    pub(crate) bit_depth: DeliveryBitDepth,
    pub(crate) chroma_sampling: ExportChromaSampling,
    pub(crate) codec_name: &'static str,
    pub(crate) accepted_profiles: &'static [&'static str],
    pub(crate) expected_level: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfessionalMezzanineDeliveryIssue {
    Signal,
    Resolution,
    FrameRate,
    SampleAspectRatio,
    Range,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfessionalMezzanineDeliveryError {
    pub(crate) issue: ProfessionalMezzanineDeliveryIssue,
    pub(crate) detail: String,
}

fn delivery_error(
    issue: ProfessionalMezzanineDeliveryIssue,
    detail: impl Into<String>,
) -> ProfessionalMezzanineDeliveryError {
    ProfessionalMezzanineDeliveryError { issue, detail: detail.into() }
}

impl ProfessionalMezzanineContract {
    pub(crate) const fn supports_container(self, container: Container) -> bool {
        match self.adapter {
            MezzanineEncoderAdapter::DnxHd | MezzanineEncoderAdapter::Libx264AvcIntra => {
                matches!(container, Container::Mov | Container::Mxf)
            }
            MezzanineEncoderAdapter::RawVideo
            | MezzanineEncoderAdapter::V210
            | MezzanineEncoderAdapter::R210 => matches!(container, Container::Mov),
        }
    }
}

/// Resolve professional-codec facts without applying delivery-specific state.
pub(crate) const fn professional_mezzanine_contract(
    codec: &VideoCodecConfig,
) -> Option<ProfessionalMezzanineContract> {
    let contract = match codec {
        VideoCodecConfig::DnxHr { profile } => match profile {
            DnxHrProfile::Lb => dnxhr_contract(&["DNXHR LB"], "yuv422p", DeliveryBitDepth::Eight),
            DnxHrProfile::Sq => dnxhr_contract(&["DNXHR SQ"], "yuv422p", DeliveryBitDepth::Eight),
            DnxHrProfile::Hq => dnxhr_contract(&["DNXHR HQ"], "yuv422p", DeliveryBitDepth::Eight),
            DnxHrProfile::Hqx => {
                dnxhr_contract(&["DNXHR HQX"], "yuv422p10le", DeliveryBitDepth::Ten)
            }
            DnxHrProfile::FourFourFour => ProfessionalMezzanineContract {
                adapter: MezzanineEncoderAdapter::DnxHd,
                output_pixel_format: "gbrp10le",
                bit_depth: DeliveryBitDepth::Ten,
                chroma_sampling: ExportChromaSampling::Rgb,
                codec_name: "dnxhd",
                accepted_profiles: &["DNXHR 444"],
                expected_level: None,
            },
        },
        VideoCodecConfig::AvcIntra { class } => ProfessionalMezzanineContract {
            adapter: MezzanineEncoderAdapter::Libx264AvcIntra,
            output_pixel_format: "yuv422p10le",
            bit_depth: DeliveryBitDepth::Ten,
            chroma_sampling: ExportChromaSampling::Yuv422,
            codec_name: "h264",
            accepted_profiles: &["High 4:2:2 Intra"],
            expected_level: Some(match class {
                AvcIntraClass::Class100 => 41,
                AvcIntraClass::Class200 => 50,
            }),
        },
        VideoCodecConfig::Uncompressed { format } => match format {
            UncompressedVideoFormat::Yuv422Eight => ProfessionalMezzanineContract {
                adapter: MezzanineEncoderAdapter::RawVideo,
                output_pixel_format: "uyvy422",
                bit_depth: DeliveryBitDepth::Eight,
                chroma_sampling: ExportChromaSampling::Yuv422,
                codec_name: "rawvideo",
                accepted_profiles: &[],
                expected_level: None,
            },
            UncompressedVideoFormat::Yuv422Ten => ProfessionalMezzanineContract {
                adapter: MezzanineEncoderAdapter::V210,
                output_pixel_format: "yuv422p10le",
                bit_depth: DeliveryBitDepth::Ten,
                chroma_sampling: ExportChromaSampling::Yuv422,
                codec_name: "v210",
                accepted_profiles: &[],
                expected_level: None,
            },
            UncompressedVideoFormat::RgbEight => ProfessionalMezzanineContract {
                adapter: MezzanineEncoderAdapter::RawVideo,
                output_pixel_format: "rgb24",
                bit_depth: DeliveryBitDepth::Eight,
                chroma_sampling: ExportChromaSampling::Rgb,
                codec_name: "rawvideo",
                accepted_profiles: &[],
                expected_level: None,
            },
            UncompressedVideoFormat::RgbTen => ProfessionalMezzanineContract {
                adapter: MezzanineEncoderAdapter::R210,
                output_pixel_format: "gbrp10le",
                bit_depth: DeliveryBitDepth::Ten,
                chroma_sampling: ExportChromaSampling::Rgb,
                codec_name: "r210",
                accepted_profiles: &[],
                expected_level: None,
            },
        },
        VideoCodecConfig::H264 { .. }
        | VideoCodecConfig::Hevc { .. }
        | VideoCodecConfig::Av1 { .. }
        | VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::Gif { .. } => return None,
    };
    Some(contract)
}

const fn dnxhr_contract(
    accepted_profiles: &'static [&'static str],
    output_pixel_format: &'static str,
    bit_depth: DeliveryBitDepth,
) -> ProfessionalMezzanineContract {
    ProfessionalMezzanineContract {
        adapter: MezzanineEncoderAdapter::DnxHd,
        output_pixel_format,
        bit_depth,
        chroma_sampling: ExportChromaSampling::Yuv422,
        codec_name: "dnxhd",
        accepted_profiles,
        expected_level: None,
    }
}

/// Validate all delivery-owned state required by a professional essence.
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_professional_mezzanine_delivery(
    codec: &VideoCodecConfig,
    container: Container,
    resolution: Resolution,
    frame_rate: Rational,
    sample_aspect_ratio: SampleAspectRatio,
    bit_depth: DeliveryBitDepth,
    video_range: VideoRange,
    chroma_sampling: ExportChromaSampling,
) -> Result<(), ProfessionalMezzanineDeliveryError> {
    let Some(contract) = professional_mezzanine_contract(codec) else {
        return Ok(());
    };
    if !contract.supports_container(container) {
        return Err(delivery_error(
            ProfessionalMezzanineDeliveryIssue::Signal,
            "professional mezzanine essence is not qualified for the selected container",
        ));
    }
    if bit_depth != contract.bit_depth || chroma_sampling != contract.chroma_sampling {
        return Err(delivery_error(
            ProfessionalMezzanineDeliveryIssue::Signal,
            format!(
                "professional mezzanine representation requires {:?} / {:?}",
                contract.bit_depth, contract.chroma_sampling
            ),
        ));
    }
    if contract.chroma_sampling == ExportChromaSampling::Rgb && video_range != VideoRange::Full {
        return Err(delivery_error(
            ProfessionalMezzanineDeliveryIssue::Range,
            "professional RGB export requires Full range",
        ));
    }
    if matches!(codec, VideoCodecConfig::AvcIntra { .. }) {
        if resolution != (Resolution { width: 1920, height: 1080 }) {
            return Err(delivery_error(
                ProfessionalMezzanineDeliveryIssue::Resolution,
                "AVC-Intra Class 100/200 is qualified only for 1920x1080 progressive HD",
            ));
        }
        if sample_aspect_ratio != SampleAspectRatio::SQUARE {
            return Err(delivery_error(
                ProfessionalMezzanineDeliveryIssue::SampleAspectRatio,
                "AVC-Intra Class 100/200 requires square pixels",
            ));
        }
        if !matches!(
            frame_rate,
            Rational::FPS_23976
                | Rational::FPS_24
                | Rational::FPS_25
                | Rational::FPS_2997
                | Rational::FPS_30
        ) {
            return Err(delivery_error(
                ProfessionalMezzanineDeliveryIssue::FrameRate,
                "AVC-Intra Class 100/200 supports only 23.976/24/25/29.97/30 progressive fps",
            ));
        }
    }
    Ok(())
}

/// Expected MOV sample-entry tag for representations where it is stable and
/// independently useful. MXF uses essence ULs rather than a QuickTime tag.
pub(crate) const fn expected_mov_codec_tag(codec: &VideoCodecConfig) -> Option<&'static str> {
    match codec {
        VideoCodecConfig::DnxHr { .. } => Some("AVdh"),
        VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::Yuv422Eight } => {
            Some("2vuy")
        }
        VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::Yuv422Ten } => {
            Some("v210")
        }
        VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::RgbEight } => {
            Some("raw ")
        }
        VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::RgbTen } => Some("r210"),
        VideoCodecConfig::AvcIntra { .. }
        | VideoCodecConfig::H264 { .. }
        | VideoCodecConfig::Hevc { .. }
        | VideoCodecConfig::Av1 { .. }
        | VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::Gif { .. } => None,
    }
}

/// Whether FFprobe must expose an explicit stream-level range tag. MOV raw
/// sample entries do not carry one; their code-value semantics are instead
/// fixed by the representation and the explicit renderer-to-encoder scale.
pub(crate) const fn requires_stream_range_tag(codec: &VideoCodecConfig) -> bool {
    !matches!(codec, VideoCodecConfig::Uncompressed { .. })
}

/// Apply exact professional encoder arguments. Returns `true` when this Module
/// owns the supplied codec.
pub(crate) fn apply_professional_mezzanine_encoder_args(
    command: &mut Command,
    codec: &VideoCodecConfig,
) -> bool {
    let Some(contract) = professional_mezzanine_contract(codec) else {
        return false;
    };
    command.arg("-c:v").arg(contract.adapter.ffmpeg_name());
    match codec {
        VideoCodecConfig::DnxHr { profile } => {
            let profile = match profile {
                DnxHrProfile::Lb => "dnxhr_lb",
                DnxHrProfile::Sq => "dnxhr_sq",
                DnxHrProfile::Hq => "dnxhr_hq",
                DnxHrProfile::Hqx => "dnxhr_hqx",
                DnxHrProfile::FourFourFour => "dnxhr_444",
            };
            command.arg("-profile:v").arg(profile);
        }
        VideoCodecConfig::AvcIntra { class } => {
            let class = match class {
                AvcIntraClass::Class100 => "100",
                AvcIntraClass::Class200 => "200",
            };
            command.arg("-avcintra-class").arg(class);
        }
        VideoCodecConfig::Uncompressed { .. } => {}
        VideoCodecConfig::H264 { .. }
        | VideoCodecConfig::Hevc { .. }
        | VideoCodecConfig::Av1 { .. }
        | VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::Gif { .. } => unreachable!("contract resolved only mezzanine codecs"),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validator::{
        delivery_bit_depth_value, expected_video_encoding, validate_export_output, ExpectedStream,
        ExpectedVideoConstraints, ExportValidationExpectations,
    };
    use crate::video_encoding::ResolvedVideoCodingStructure;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn exact_profile_matrix_is_closed_and_typed() {
        let cases = [
            (
                VideoCodecConfig::DnxHr { profile: DnxHrProfile::Lb },
                "dnxhd",
                "yuv422p",
                DeliveryBitDepth::Eight,
            ),
            (
                VideoCodecConfig::DnxHr { profile: DnxHrProfile::Hqx },
                "dnxhd",
                "yuv422p10le",
                DeliveryBitDepth::Ten,
            ),
            (
                VideoCodecConfig::AvcIntra { class: AvcIntraClass::Class100 },
                "h264",
                "yuv422p10le",
                DeliveryBitDepth::Ten,
            ),
            (
                VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::RgbTen },
                "r210",
                "gbrp10le",
                DeliveryBitDepth::Ten,
            ),
        ];
        for (codec, expected_codec, expected_pixel_format, expected_depth) in cases {
            let contract = professional_mezzanine_contract(&codec).expect("professional codec");
            assert_eq!(contract.codec_name, expected_codec);
            assert_eq!(contract.output_pixel_format, expected_pixel_format);
            assert_eq!(contract.bit_depth, expected_depth);
        }
    }

    #[test]
    fn avc_intra_rejects_unqualified_raster_and_cadence() {
        let codec = VideoCodecConfig::AvcIntra { class: AvcIntraClass::Class100 };
        let error = validate_professional_mezzanine_delivery(
            &codec,
            Container::Mxf,
            Resolution { width: 1280, height: 720 },
            Rational::FPS_25,
            SampleAspectRatio::SQUARE,
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv422,
        )
        .expect_err("raster must be exact");
        assert_eq!(error.issue, ProfessionalMezzanineDeliveryIssue::Resolution);
        assert!(error.detail.contains("1920x1080"));
    }

    #[test]
    fn avc_intra_admits_only_the_qualified_progressive_hd_cadences() {
        let codec = VideoCodecConfig::AvcIntra { class: AvcIntraClass::Class200 };
        for frame_rate in [
            Rational::FPS_23976,
            Rational::FPS_24,
            Rational::FPS_25,
            Rational::FPS_2997,
            Rational::FPS_30,
        ] {
            validate_professional_mezzanine_delivery(
                &codec,
                Container::Mxf,
                Resolution { width: 1920, height: 1080 },
                frame_rate,
                SampleAspectRatio::SQUARE,
                DeliveryBitDepth::Ten,
                VideoRange::Legal,
                ExportChromaSampling::Yuv422,
            )
            .unwrap_or_else(|error| panic!("qualified cadence rejected: {}", error.detail));
        }
        let error = validate_professional_mezzanine_delivery(
            &codec,
            Container::Mxf,
            Resolution { width: 1920, height: 1080 },
            Rational::FPS_50,
            SampleAspectRatio::SQUARE,
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv422,
        )
        .expect_err("50p is outside the qualified class contract");
        assert_eq!(error.issue, ProfessionalMezzanineDeliveryIssue::FrameRate);
    }

    #[test]
    fn authoring_defaults_follow_the_exact_professional_representation() {
        let avc = professional_mezzanine_authoring_defaults(&VideoCodecConfig::AvcIntra {
            class: AvcIntraClass::Class100,
        })
        .expect("AVC-Intra defaults");
        assert_eq!(avc.container, Container::Mxf);
        assert_eq!(
            avc.resolution,
            Some(Resolution { width: 1920, height: 1080 })
        );
        assert_eq!(avc.frame_rate, Some(Rational::FPS_25));
        assert!(avc.video_only);

        let dnxhr_rgb = professional_mezzanine_authoring_defaults(&VideoCodecConfig::DnxHr {
            profile: DnxHrProfile::FourFourFour,
        })
        .expect("DNxHR RGB defaults");
        assert_eq!(dnxhr_rgb.container, Container::Mov);
        assert_eq!(dnxhr_rgb.bit_depth, DeliveryBitDepth::Ten);
        assert_eq!(dnxhr_rgb.video_range, VideoRange::Full);
        assert_eq!(dnxhr_rgb.chroma_sampling, ExportChromaSampling::Rgb);
        assert!(!dnxhr_rgb.video_only);
    }

    #[test]
    fn professional_mezzanine_matrix_completes_real_encode_and_reprobe() {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);
        let temp = std::env::temp_dir().join(format!(
            "mondrian-col037-{}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&temp).expect("create codec-matrix temp directory");

        let mut cases = Vec::new();
        for profile in [
            DnxHrProfile::Lb,
            DnxHrProfile::Sq,
            DnxHrProfile::Hq,
            DnxHrProfile::Hqx,
            DnxHrProfile::FourFourFour,
        ] {
            for container in [Container::Mov, Container::Mxf] {
                cases.push((VideoCodecConfig::DnxHr { profile }, container, 256, 128));
            }
        }
        for class in [AvcIntraClass::Class100, AvcIntraClass::Class200] {
            for container in [Container::Mov, Container::Mxf] {
                cases.push((VideoCodecConfig::AvcIntra { class }, container, 1920, 1080));
            }
        }
        for format in [
            UncompressedVideoFormat::Yuv422Eight,
            UncompressedVideoFormat::Yuv422Ten,
            UncompressedVideoFormat::RgbEight,
            UncompressedVideoFormat::RgbTen,
        ] {
            cases.push((
                VideoCodecConfig::Uncompressed { format },
                Container::Mov,
                256,
                128,
            ));
        }

        for (index, (codec, container, width, height)) in cases.into_iter().enumerate() {
            let contract = professional_mezzanine_contract(&codec).expect("matrix codec");
            let extension = match container {
                Container::Mov => "mov",
                Container::Mxf => "mxf",
                _ => unreachable!("professional test matrix is MOV/MXF only"),
            };
            let output_path = temp.join(format!("case-{index}.{extension}"));
            let mut command = mondrian_media::ffmpeg_command();
            command
                .arg("-y")
                .arg("-hide_banner")
                .arg("-loglevel")
                .arg("error")
                .arg("-f")
                .arg("lavfi")
                .arg("-i")
                .arg(format!("testsrc2=size={width}x{height}:rate=25"))
                .arg("-frames:v")
                .arg("1")
                .arg("-an");
            assert!(
                apply_professional_mezzanine_encoder_args(&mut command, &codec),
                "matrix codec must be owned"
            );
            command
                .arg("-pix_fmt")
                .arg(contract.output_pixel_format)
                .arg("-f")
                .arg(match container {
                    Container::Mov => "mov",
                    Container::Mxf => "mxf",
                    _ => unreachable!("professional test matrix is MOV/MXF only"),
                })
                .arg(&output_path);
            let output = command.output().expect("start FFmpeg matrix encode");
            assert!(
                output.status.success(),
                "FFmpeg failed for {codec:?}/{container:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            validate_export_output(
                &output_path,
                &ExportValidationExpectations {
                    container,
                    video: ExpectedStream::Required(ExpectedVideoConstraints {
                        encoding: Some(expected_video_encoding(&codec)),
                        codec_tag: (container == Container::Mov)
                            .then(|| expected_mov_codec_tag(&codec))
                            .flatten()
                            .map(str::to_owned),
                        bit_depth: Some(delivery_bit_depth_value(contract.bit_depth)),
                        width: Some(width),
                        height: Some(height),
                        fps_num: Some(25),
                        fps_den: Some(1),
                        signal: None,
                        coding: Some(ResolvedVideoCodingStructure::IntraOnly),
                        require_progressive_frame: true,
                        require_interlaced_top_field_first: None,
                    }),
                    audio: ExpectedStream::Forbidden,
                    expected_duration_secs: None,
                },
            )
            .unwrap_or_else(|error| {
                panic!("finished output failed for {codec:?}/{container:?}: {error}")
            });
            let media = mondrian_media::probe_media_info(&output_path)
                .unwrap_or_else(|error| panic!("re-import probe failed for {codec:?}: {error}"));
            let imported_codec = &media.primary_video().expect("re-imported video stream").codec;
            match codec {
                VideoCodecConfig::DnxHr { .. } => {
                    assert_eq!(imported_codec, &mondrian_core::VideoCodec::DnxHr)
                }
                VideoCodecConfig::AvcIntra { .. } => {
                    assert_eq!(imported_codec, &mondrian_core::VideoCodec::H264)
                }
                VideoCodecConfig::Uncompressed { .. } => {
                    assert_eq!(imported_codec, &mondrian_core::VideoCodec::Raw)
                }
                _ => unreachable!("professional test matrix only"),
            }
        }

        std::fs::remove_dir_all(&temp).expect("remove codec-matrix temp directory");
    }
}
