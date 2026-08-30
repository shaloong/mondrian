use crate::preset::{
    ExportChromaSampling, ProfessionalDeliveryOutput, ProfessionalDeliveryProfile, Resolution,
};
use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};
use mondrian_timeline::sequence::{DeliveryBitDepth, VideoRange};

/// Physical namespace shape produced by a professional delivery profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverableLayout {
    /// One immutable create-new package directory.
    ImmutableDirectory,
    /// One constrained MXF file.
    SingleMxfFile,
}

/// Exact picture essence required by one profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfessionalEssenceKind {
    /// SMPTE RDD 36 ProRes 422 HQ wrapped per RDD 45.
    ProRes422Hq,
    /// AVC High 4:2:2, 10-bit, progressive and intra-only.
    AvcHigh422Intra,
    /// ST 428-1 X'Y'Z' 12-bit JPEG 2000 Cinema 2K.
    DcdmXyzJpeg2000,
}

/// Fully resolved standards-facing contract consumed by execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfessionalDeliveryContract {
    /// Exact product profile.
    pub profile: ProfessionalDeliveryProfile,
    /// Final namespace shape.
    pub layout: DeliverableLayout,
    /// Exact picture essence.
    pub picture_essence: ProfessionalEssenceKind,
    /// Exact raster.
    pub resolution: Resolution,
    /// Exact edit rate.
    pub edit_rate: Rational,
    /// Exact sample depth.
    pub bit_depth: DeliveryBitDepth,
    /// Exact encoded range.
    pub range: VideoRange,
    /// Exact chroma/component representation.
    pub chroma_sampling: ExportChromaSampling,
    /// Renderer output consumed by the format Adapter.
    pub renderer_color_space: ColorSpace,
    /// Required PCM sample rate.
    pub audio_sample_rate: u32,
    /// Required primary Program Output layout.
    pub audio_layout: AudioChannelLayout,
}

/// Stable admission failure for a professional delivery request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfessionalDeliveryAdmissionError {
    /// Metadata is empty, too long, or not representable by the profile.
    #[error("invalid professional delivery metadata: {0}")]
    InvalidMetadata(String),
    /// Raster does not match the exact profile row.
    #[error("professional delivery raster must be {expected_width}x{expected_height}, got {actual_width}x{actual_height}")]
    Resolution {
        /// Required width.
        expected_width: u32,
        /// Required height.
        expected_height: u32,
        /// Supplied width.
        actual_width: u32,
        /// Supplied height.
        actual_height: u32,
    },
    /// Edit rate does not match the exact profile row.
    #[error("professional delivery edit rate must be {expected}, got {actual}")]
    FrameRate {
        /// Required exact rate.
        expected: Rational,
        /// Supplied exact rate.
        actual: Rational,
    },
    /// Signal depth/range/chroma disagrees with the profile.
    #[error("professional delivery signal must be {expected}, got {actual}")]
    Signal {
        /// Human-readable required signal.
        expected: &'static str,
        /// Human-readable supplied signal.
        actual: String,
    },
    /// Renderer color target is not the profile-owned transform input.
    #[error("professional delivery renderer color target must be {expected:?}, got {actual:?}")]
    ColorTarget {
        /// Required renderer color target.
        expected: ColorSpace,
        /// Supplied renderer color target.
        actual: ColorSpace,
    },
    /// Primary Program Output cannot be mapped to the qualified layout.
    #[error("professional delivery currently requires a Stereo primary Program Output, got {0:?}")]
    AudioLayout(AudioChannelLayout),
}

/// Resolve one exact professional delivery product row.
#[allow(clippy::too_many_arguments)]
pub fn resolve_professional_delivery(
    output: &ProfessionalDeliveryOutput,
    resolution: Resolution,
    edit_rate: Rational,
    bit_depth: DeliveryBitDepth,
    range: VideoRange,
    chroma_sampling: ExportChromaSampling,
    renderer_color_space: ColorSpace,
    audio_layout: AudioChannelLayout,
) -> Result<ResolvedProfessionalDeliveryContract, ProfessionalDeliveryAdmissionError> {
    validate_metadata(output)?;
    if audio_layout != AudioChannelLayout::Stereo {
        return Err(ProfessionalDeliveryAdmissionError::AudioLayout(
            audio_layout,
        ));
    }
    let contract = match output.profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
            ResolvedProfessionalDeliveryContract {
                profile: output.profile,
                layout: DeliverableLayout::ImmutableDirectory,
                picture_essence: ProfessionalEssenceKind::ProRes422Hq,
                resolution: Resolution { width: 1920, height: 1080 },
                edit_rate: Rational::FPS_25,
                bit_depth: DeliveryBitDepth::Ten,
                range: VideoRange::Legal,
                chroma_sampling: ExportChromaSampling::Yuv422,
                renderer_color_space: ColorSpace::Rec709,
                audio_sample_rate: 48_000,
                audio_layout: AudioChannelLayout::Stereo,
            }
        }
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => ResolvedProfessionalDeliveryContract {
            profile: output.profile,
            layout: DeliverableLayout::SingleMxfFile,
            picture_essence: ProfessionalEssenceKind::AvcHigh422Intra,
            resolution: Resolution { width: 1280, height: 720 },
            edit_rate: Rational::FPS_5994,
            bit_depth: DeliveryBitDepth::Ten,
            range: VideoRange::Legal,
            chroma_sampling: ExportChromaSampling::Yuv422,
            renderer_color_space: ColorSpace::Rec709,
            audio_sample_rate: 48_000,
            audio_layout: AudioChannelLayout::Stereo,
        },
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => ResolvedProfessionalDeliveryContract {
            profile: output.profile,
            layout: DeliverableLayout::ImmutableDirectory,
            picture_essence: ProfessionalEssenceKind::DcdmXyzJpeg2000,
            resolution: Resolution { width: 1998, height: 1080 },
            edit_rate: Rational::FPS_24,
            bit_depth: DeliveryBitDepth::Twelve,
            range: VideoRange::Full,
            chroma_sampling: ExportChromaSampling::Rgb,
            renderer_color_space: ColorSpace::LinearRec709,
            audio_sample_rate: 48_000,
            audio_layout: AudioChannelLayout::Stereo,
        },
    };
    validate_exact_contract(
        &contract,
        resolution,
        edit_rate,
        bit_depth,
        range,
        chroma_sampling,
        renderer_color_space,
    )?;
    Ok(contract)
}

fn validate_metadata(
    output: &ProfessionalDeliveryOutput,
) -> Result<(), ProfessionalDeliveryAdmissionError> {
    for (name, value, max_bytes) in [
        ("title", output.metadata.title.as_str(), 256usize),
        ("issuer", output.metadata.issuer.as_str(), 128usize),
        ("creator", output.metadata.creator.as_str(), 128usize),
        ("language", output.metadata.language.as_str(), 35usize),
    ] {
        if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control)
        {
            return Err(ProfessionalDeliveryAdmissionError::InvalidMetadata(
                format!("{name} must contain 1..={max_bytes} non-control UTF-8 bytes"),
            ));
        }
    }
    if !is_rfc5646_subset(&output.metadata.language) {
        return Err(ProfessionalDeliveryAdmissionError::InvalidMetadata(
            "language must use an ASCII RFC 5646 tag".to_owned(),
        ));
    }
    Ok(())
}

fn is_rfc5646_subset(value: &str) -> bool {
    !value.starts_with('-')
        && !value.ends_with('-')
        && value.split('-').all(|part| {
            !part.is_empty()
                && part.len() <= 8
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

#[allow(clippy::too_many_arguments)]
fn validate_exact_contract(
    expected: &ResolvedProfessionalDeliveryContract,
    resolution: Resolution,
    edit_rate: Rational,
    bit_depth: DeliveryBitDepth,
    range: VideoRange,
    chroma_sampling: ExportChromaSampling,
    renderer_color_space: ColorSpace,
) -> Result<(), ProfessionalDeliveryAdmissionError> {
    if resolution != expected.resolution {
        return Err(ProfessionalDeliveryAdmissionError::Resolution {
            expected_width: expected.resolution.width,
            expected_height: expected.resolution.height,
            actual_width: resolution.width,
            actual_height: resolution.height,
        });
    }
    if edit_rate != expected.edit_rate {
        return Err(ProfessionalDeliveryAdmissionError::FrameRate {
            expected: expected.edit_rate,
            actual: edit_rate,
        });
    }
    if (bit_depth, range, chroma_sampling)
        != (expected.bit_depth, expected.range, expected.chroma_sampling)
    {
        return Err(ProfessionalDeliveryAdmissionError::Signal {
            expected: match expected.profile {
                ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25
                | ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
                    "10-bit legal-range YUV 4:2:2"
                }
                ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => "12-bit full-range RGB/XYZ",
            },
            actual: format!("{bit_depth:?} {range:?} {chroma_sampling:?}"),
        });
    }
    if renderer_color_space != expected.renderer_color_space {
        return Err(ProfessionalDeliveryAdmissionError::ColorTarget {
            expected: expected.renderer_color_space,
            actual: renderer_color_space,
        });
    }
    Ok(())
}
