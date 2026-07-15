//! Strict contracts for importing independently generated color reference frames.
//!
//! File decoding stays behind [`ColorReferenceDecoder`] so validation tooling can
//! support PNG, OpenEXR, or application-specific exports without coupling those
//! codecs to the realtime renderer. This module owns provenance, integrity, and
//! decoded pixel-domain validation before accuracy metrics consume a frame.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const COLOR_REFERENCE_SCHEMA_VERSION: u32 = 1;

/// Provenance class for a color reference frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorReferenceOrigin {
    /// Numeric or raster reference derived from a cited public specification.
    PublicSpecification,
    /// Frame exported by an application independent of Mondrian.
    IndependentApplication,
    /// Mondrian-generated regression image; useful but not independent evidence.
    MondrianRegression,
}

/// Encoded container presented to a [`ColorReferenceDecoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorReferencePayloadFormat {
    /// Portable Network Graphics image.
    Png,
    /// OpenEXR image, normally used for float scene-linear or display signals.
    OpenExr,
    /// Strict JSON array of float RGBA or L*a*b*/alpha tuples.
    JsonFloat,
}

/// Exact pixel-domain interpretation of a decoded reference frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorReferenceEncoding {
    /// Encoded sRGB display values stored as RGBA8.
    SrgbDisplayRgba8,
    /// Encoded Display P3 values stored as RGBA8.
    DisplayP3Rgba8,
    /// Normalized BT.2100 PQ signal values stored as float RGBA.
    Bt2100PqRgbaF32,
    /// Normalized BT.2100 HLG signal values stored as float RGBA.
    Bt2100HlgRgbaF32,
    /// Unbounded scene-linear Rec.2020 values stored as float RGBA.
    SceneLinearRec2020RgbaF32,
    /// CIELAB D50 patch values stored as float L*, a*, b*, alpha tuples.
    CieLabD50F32,
}

impl ColorReferenceEncoding {
    fn is_normalized_float_display_signal(self) -> bool {
        matches!(self, Self::Bt2100PqRgbaF32 | Self::Bt2100HlgRgbaF32)
    }

    fn is_hdr(self) -> bool {
        matches!(self, Self::Bt2100PqRgbaF32 | Self::Bt2100HlgRgbaF32)
    }
}

/// Alpha interpretation attached to a reference frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorReferenceAlpha {
    /// Every pixel is required to carry fully opaque alpha.
    Opaque,
    /// RGB is straight and alpha represents coverage.
    StraightCoverage,
    /// RGB is premultiplied by coverage alpha.
    PremultipliedCoverage,
}

/// Strict metadata required before an external frame can serve as quality evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorReferenceDescriptor {
    /// Contract schema version. Version 1 is the only accepted value.
    pub schema_version: u32,
    /// Immutable reference identifier.
    pub reference_id: String,
    /// Whether this is public, independently generated, or Mondrian-only evidence.
    pub origin: ColorReferenceOrigin,
    /// Specification name or application that produced the reference.
    pub producer: String,
    /// Exact specification revision or producer version.
    pub producer_version: String,
    /// Optional stable source URL or internal evidence locator.
    pub source_uri: Option<String>,
    /// SHA-256 of the stimulus or project used to produce the reference.
    pub source_artifact_sha256: String,
    /// SHA-256 of the encoded reference-frame payload passed to the importer.
    pub content_sha256: String,
    /// File or numeric payload format consumed by the injected decoder.
    pub payload_format: ColorReferencePayloadFormat,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel encoding and color-domain interpretation.
    pub encoding: ColorReferenceEncoding,
    /// Alpha interpretation.
    pub alpha: ColorReferenceAlpha,
    /// Display reference white in cd/m² when the encoding has display luminance semantics.
    pub reference_white_nits: Option<f32>,
    /// Nominal display peak in cd/m² when the encoding has HDR luminance semantics.
    pub nominal_peak_nits: Option<f32>,
}

impl ColorReferenceDescriptor {
    /// Whether this descriptor is independent evidence rather than a self-generated golden.
    pub const fn is_independent_quality_reference(&self) -> bool {
        matches!(
            self.origin,
            ColorReferenceOrigin::PublicSpecification
                | ColorReferenceOrigin::IndependentApplication
        )
    }

    fn validate(&self) -> Result<usize, ColorReferenceValidationError> {
        if self.schema_version != COLOR_REFERENCE_SCHEMA_VERSION {
            return Err(ColorReferenceValidationError::UnsupportedSchemaVersion {
                actual: self.schema_version,
            });
        }
        validate_identity("reference_id", &self.reference_id)?;
        validate_identity("producer", &self.producer)?;
        validate_identity("producer_version", &self.producer_version)?;
        if let Some(source_uri) = &self.source_uri {
            validate_identity("source_uri", source_uri)?;
        }
        validate_sha256("source_artifact_sha256", &self.source_artifact_sha256)?;
        validate_sha256("content_sha256", &self.content_sha256)?;
        if self.width == 0 || self.height == 0 {
            return Err(ColorReferenceValidationError::ZeroExtent {
                width: self.width,
                height: self.height,
            });
        }
        let pixel_count =
            usize::try_from(u64::from(self.width) * u64::from(self.height)).map_err(|_| {
                ColorReferenceValidationError::PixelCountOverflow {
                    width: self.width,
                    height: self.height,
                }
            })?;
        validate_luminance_contract(self)?;
        Ok(pixel_count)
    }
}

/// Decoded pixels accepted by the external reference importer.
#[derive(Debug, Clone, PartialEq)]
pub enum ColorReferencePixels {
    /// Pixel-major encoded RGBA8 values.
    Rgba8(Vec<u8>),
    /// Pixel-major float RGBA or L*a*b*/alpha tuples.
    RgbaF32(Vec<[f32; 4]>),
}

impl ColorReferencePixels {
    fn pixel_count(&self) -> Option<usize> {
        match self {
            Self::Rgba8(bytes) => bytes.len().checked_div(4).filter(|_| bytes.len() % 4 == 0),
            Self::RgbaF32(pixels) => Some(pixels.len()),
        }
    }
}

/// Validated external reference frame ready for accuracy comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorReferenceFrame {
    /// Trusted descriptor whose integrity and pixel semantics were checked.
    pub descriptor: ColorReferenceDescriptor,
    /// Decoded pixels matching the descriptor.
    pub pixels: ColorReferencePixels,
}

impl ColorReferenceFrame {
    /// Number of pixels in the validated frame.
    pub fn pixel_count(&self) -> usize {
        match &self.pixels {
            ColorReferencePixels::Rgba8(bytes) => bytes.len() / 4,
            ColorReferencePixels::RgbaF32(pixels) => pixels.len(),
        }
    }
}

/// Decoder supplied by validation tooling for one encoded reference payload.
pub trait ColorReferenceDecoder {
    /// Decode `encoded` according to its declared payload format without changing color semantics.
    fn decode(
        &self,
        encoded: &[u8],
        descriptor: &ColorReferenceDescriptor,
    ) -> Result<ColorReferencePixels, String>;
}

/// Failure to validate or decode an external color reference frame.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ColorReferenceValidationError {
    /// Only the current strict schema is accepted.
    #[error("unsupported color reference schema version {actual}; expected 1")]
    UnsupportedSchemaVersion {
        /// Unsupported schema version.
        actual: u32,
    },
    /// An identity field was empty.
    #[error("color reference identity field '{field}' must not be empty")]
    EmptyIdentity {
        /// Invalid field name.
        field: &'static str,
    },
    /// A placeholder value cannot establish provenance.
    #[error("color reference identity field '{field}' contains a placeholder")]
    PlaceholderIdentity {
        /// Invalid field name.
        field: &'static str,
    },
    /// A digest was not lowercase hexadecimal SHA-256.
    #[error("color reference field '{field}' must be a lowercase SHA-256 digest")]
    InvalidSha256 {
        /// Invalid digest field.
        field: &'static str,
    },
    /// Frame dimensions must be non-zero.
    #[error("color reference frame extent must be non-zero, got {width}x{height}")]
    ZeroExtent {
        /// Invalid width.
        width: u32,
        /// Invalid height.
        height: u32,
    },
    /// Dimensions could not be represented as an addressable pixel count.
    #[error("color reference frame extent {width}x{height} overflows the platform pixel count")]
    PixelCountOverflow {
        /// Frame width.
        width: u32,
        /// Frame height.
        height: u32,
    },
    /// HDR luminance metadata was missing or contradictory.
    #[error(
        "invalid color reference luminance contract: reference_white_nits={reference_white_nits:?}, nominal_peak_nits={nominal_peak_nits:?}"
    )]
    InvalidLuminanceContract {
        /// Declared display reference white.
        reference_white_nits: Option<f32>,
        /// Declared nominal peak.
        nominal_peak_nits: Option<f32>,
    },
    /// Encoded payload did not match the pinned digest.
    #[error("color reference payload SHA-256 mismatch: expected {expected}, got {actual}")]
    ContentHashMismatch {
        /// Pinned digest.
        expected: String,
        /// Actual digest.
        actual: String,
    },
    /// The injected decoder failed.
    #[error("color reference decoder failed: {message}")]
    Decode {
        /// Decoder error message.
        message: String,
    },
    /// Decoded storage did not match the descriptor encoding.
    #[error("decoded color reference storage does not match {encoding:?}")]
    PixelEncodingMismatch {
        /// Descriptor encoding.
        encoding: ColorReferenceEncoding,
    },
    /// Decoded pixel count differed from width × height.
    #[error("decoded color reference pixel count mismatch: expected {expected}, got {actual:?}")]
    PixelCountMismatch {
        /// Expected pixel count.
        expected: usize,
        /// Actual count, or `None` for malformed RGBA8 storage.
        actual: Option<usize>,
    },
    /// A float reference contained NaN or infinity.
    #[error("non-finite color reference sample at pixel {pixel_index}, channel {channel}")]
    NonFiniteSample {
        /// Pixel containing the value.
        pixel_index: usize,
        /// Channel containing the value.
        channel: usize,
    },
    /// A normalized PQ or HLG reference left the encoded signal domain.
    #[error(
        "display signal sample {value} is outside [0, 1] at pixel {pixel_index}, channel {channel}"
    )]
    DisplaySignalOutOfRange {
        /// Pixel containing the value.
        pixel_index: usize,
        /// RGB channel containing the value.
        channel: usize,
        /// Invalid value.
        value: f32,
    },
    /// A coverage alpha sample left the normalized domain.
    #[error("alpha sample {value} is outside [0, 1] at pixel {pixel_index}")]
    AlphaOutOfRange {
        /// Pixel containing the value.
        pixel_index: usize,
        /// Invalid alpha value.
        value: f32,
    },
    /// An opaque frame contained non-opaque alpha.
    #[error("opaque color reference has alpha {value} at pixel {pixel_index}")]
    OpaqueAlphaMismatch {
        /// Pixel containing non-opaque alpha.
        pixel_index: usize,
        /// Observed alpha represented as normalized float.
        value: f32,
    },
}

/// Validate provenance and payload integrity, decode pixels, and enforce color semantics.
pub fn import_external_color_reference(
    descriptor: ColorReferenceDescriptor,
    encoded: &[u8],
    decoder: &dyn ColorReferenceDecoder,
) -> Result<ColorReferenceFrame, ColorReferenceValidationError> {
    let expected_pixel_count = descriptor.validate()?;
    let actual_sha256 = sha256_hex(encoded);
    if actual_sha256 != descriptor.content_sha256 {
        return Err(ColorReferenceValidationError::ContentHashMismatch {
            expected: descriptor.content_sha256.clone(),
            actual: actual_sha256,
        });
    }
    let pixels = decoder
        .decode(encoded, &descriptor)
        .map_err(|message| ColorReferenceValidationError::Decode { message })?;
    validate_pixel_storage(&descriptor, &pixels, expected_pixel_count)?;
    Ok(ColorReferenceFrame { descriptor, pixels })
}

fn validate_identity(
    field: &'static str,
    value: &str,
) -> Result<(), ColorReferenceValidationError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ColorReferenceValidationError::EmptyIdentity { field });
    }
    if ["unknown", "unset", "tbd", "placeholder", "n/a"]
        .iter()
        .any(|placeholder| value.eq_ignore_ascii_case(placeholder))
    {
        return Err(ColorReferenceValidationError::PlaceholderIdentity { field });
    }
    Ok(())
}

fn validate_sha256(field: &'static str, value: &str) -> Result<(), ColorReferenceValidationError> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ColorReferenceValidationError::InvalidSha256 { field });
    }
    Ok(())
}

fn validate_luminance_contract(
    descriptor: &ColorReferenceDescriptor,
) -> Result<(), ColorReferenceValidationError> {
    let valid_positive =
        |value: Option<f32>| value.is_none_or(|value| value.is_finite() && value > 0.0);
    let values_valid = valid_positive(descriptor.reference_white_nits)
        && valid_positive(descriptor.nominal_peak_nits);
    let pair_valid = match (
        descriptor.reference_white_nits,
        descriptor.nominal_peak_nits,
    ) {
        (Some(reference_white), Some(nominal_peak)) => nominal_peak >= reference_white,
        (None, None) => !descriptor.encoding.is_hdr(),
        _ => false,
    };
    if values_valid && pair_valid {
        Ok(())
    } else {
        Err(ColorReferenceValidationError::InvalidLuminanceContract {
            reference_white_nits: descriptor.reference_white_nits,
            nominal_peak_nits: descriptor.nominal_peak_nits,
        })
    }
}

fn validate_pixel_storage(
    descriptor: &ColorReferenceDescriptor,
    pixels: &ColorReferencePixels,
    expected_pixel_count: usize,
) -> Result<(), ColorReferenceValidationError> {
    let storage_matches = matches!(
        (descriptor.encoding, pixels),
        (
            ColorReferenceEncoding::SrgbDisplayRgba8 | ColorReferenceEncoding::DisplayP3Rgba8,
            ColorReferencePixels::Rgba8(_)
        ) | (
            ColorReferenceEncoding::Bt2100PqRgbaF32
                | ColorReferenceEncoding::Bt2100HlgRgbaF32
                | ColorReferenceEncoding::SceneLinearRec2020RgbaF32
                | ColorReferenceEncoding::CieLabD50F32,
            ColorReferencePixels::RgbaF32(_)
        )
    );
    if !storage_matches {
        return Err(ColorReferenceValidationError::PixelEncodingMismatch {
            encoding: descriptor.encoding,
        });
    }
    let actual_pixel_count = pixels.pixel_count();
    if actual_pixel_count != Some(expected_pixel_count) {
        return Err(ColorReferenceValidationError::PixelCountMismatch {
            expected: expected_pixel_count,
            actual: actual_pixel_count,
        });
    }

    match pixels {
        ColorReferencePixels::Rgba8(bytes) => validate_rgba8_alpha(descriptor.alpha, bytes),
        ColorReferencePixels::RgbaF32(pixels) => validate_rgba_f32(descriptor, pixels),
    }
}

fn validate_rgba8_alpha(
    alpha: ColorReferenceAlpha,
    pixels: &[u8],
) -> Result<(), ColorReferenceValidationError> {
    if alpha == ColorReferenceAlpha::Opaque {
        if let Some((pixel_index, pixel)) =
            pixels.chunks_exact(4).enumerate().find(|(_, pixel)| pixel[3] != u8::MAX)
        {
            return Err(ColorReferenceValidationError::OpaqueAlphaMismatch {
                pixel_index,
                value: f32::from(pixel[3]) / 255.0,
            });
        }
    }
    Ok(())
}

fn validate_rgba_f32(
    descriptor: &ColorReferenceDescriptor,
    pixels: &[[f32; 4]],
) -> Result<(), ColorReferenceValidationError> {
    for (pixel_index, pixel) in pixels.iter().enumerate() {
        for (channel, value) in pixel.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(ColorReferenceValidationError::NonFiniteSample {
                    pixel_index,
                    channel,
                });
            }
        }
        if descriptor.encoding.is_normalized_float_display_signal() {
            for (channel, value) in pixel[..3].iter().copied().enumerate() {
                if !(0.0..=1.0).contains(&value) {
                    return Err(ColorReferenceValidationError::DisplaySignalOutOfRange {
                        pixel_index,
                        channel,
                        value,
                    });
                }
            }
        }
        if !(0.0..=1.0).contains(&pixel[3]) {
            return Err(ColorReferenceValidationError::AlphaOutOfRange {
                pixel_index,
                value: pixel[3],
            });
        }
        if descriptor.alpha == ColorReferenceAlpha::Opaque && pixel[3] != 1.0 {
            return Err(ColorReferenceValidationError::OpaqueAlphaMismatch {
                pixel_index,
                value: pixel[3],
            });
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}
