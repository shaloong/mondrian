//! Monitor ICC calibration LUT generation and CPU reference sampling.

use crate::types::ColorSpace;
use moxcms::{
    CicpColorPrimaries, CicpProfile, ColorProfile, Layout, MatrixCoefficients,
    TransferCharacteristics, TransformOptions,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

/// Default edge size for renderer monitor-calibration 3D LUTs.
pub const DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE: u16 = 33;
const MIN_LUT_EDGE: u16 = 17;
const MAX_LUT_EDGE: u16 = 65;

/// Content-derived identity for an ICC payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IccProfileFingerprint {
    /// ICC payload length.
    pub byte_len: u64,
    /// Two independent stable hashes used by calibration caches.
    pub digest: [u64; 2],
}

impl IccProfileFingerprint {
    /// Compute a stable identity from the complete ICC payload.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut fnv = 0xcbf2_9ce4_8422_2325_u64;
        let mut mixed = 0x9e37_79b9_7f4a_7c15_u64;
        for (index, byte) in bytes.iter().copied().enumerate() {
            fnv = (fnv ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            mixed ^= u64::from(byte).wrapping_add((index as u64).rotate_left(17));
            mixed = mixed.rotate_left(11).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        }
        Self { byte_len: bytes.len() as u64, digest: [fnv, mixed] }
    }

    /// Compact identity carried by per-frame renderer contracts.
    pub fn calibration_key(self) -> DisplayCalibrationKey {
        let mixed = self.digest[0]
            ^ self.digest[1].rotate_left(23)
            ^ self.byte_len.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        DisplayCalibrationKey((mixed ^ (mixed >> 32)) as u32)
    }
}

/// Non-authoritative compact identity for hot frame contracts.
///
/// LUT caches and pass admission must validate [`IccProfileFingerprint`]; this
/// key exists only to keep per-frame descriptors compact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayCalibrationKey(u32);

/// Renderer-ready monitor calibration sampled over normalized encoded RGB.
#[derive(Debug, Clone)]
pub struct DisplayCalibrationLut3d {
    /// Encoded standard/output identity consumed by the ICC transform.
    pub source_color_space: ColorSpace,
    /// Destination ICC payload identity.
    pub profile_fingerprint: IccProfileFingerprint,
    /// Number of samples along each RGB axis.
    pub edge_size: u16,
    /// RGBA32F texels in x/R-fastest order.
    samples: Arc<[f32]>,
}

impl DisplayCalibrationLut3d {
    /// Build a calibration LUT from precomputed renderer-order RGBA32F samples.
    pub fn from_rgba32f_samples(
        source_color_space: ColorSpace,
        profile_fingerprint: IccProfileFingerprint,
        edge_size: u16,
        samples: Vec<f32>,
    ) -> Result<Self, DisplayCalibrationError> {
        if cms_profile_for_color_space(source_color_space).is_none() {
            return Err(DisplayCalibrationError::UnsupportedSourceColorSpace(
                source_color_space,
            ));
        }
        validate_edge_size(edge_size)?;
        let edge = usize::from(edge_size);
        let expected = edge
            .checked_mul(edge)
            .and_then(|count| count.checked_mul(edge))
            .and_then(|count| count.checked_mul(4))
            .ok_or(DisplayCalibrationError::LutSizeOverflow { edge_size })?;
        if samples.len() != expected {
            return Err(DisplayCalibrationError::InvalidSampleCount {
                expected,
                actual: samples.len(),
            });
        }
        if let Some(component_index) = samples.iter().position(|sample| !sample.is_finite()) {
            return Err(DisplayCalibrationError::NonFiniteOutput {
                texel_index: component_index / 4,
            });
        }
        Ok(Self {
            source_color_space,
            profile_fingerprint,
            edge_size,
            samples: samples.into(),
        })
    }

    /// Build a default-size renderer calibration LUT from an ICC payload.
    pub fn from_icc_bytes(
        source_color_space: ColorSpace,
        icc_bytes: &[u8],
    ) -> Result<Self, DisplayCalibrationError> {
        Self::from_icc_bytes_with_edge(
            source_color_space,
            icc_bytes,
            DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE,
        )
    }

    /// Build a renderer calibration LUT with an explicit validated edge size.
    pub fn from_icc_bytes_with_edge(
        source_color_space: ColorSpace,
        icc_bytes: &[u8],
        edge_size: u16,
    ) -> Result<Self, DisplayCalibrationError> {
        let destination = ColorProfile::new_from_slice(icc_bytes)
            .map_err(|error| DisplayCalibrationError::InvalidIccProfile(error.to_string()))?;
        Self::from_profiles(
            source_color_space,
            IccProfileFingerprint::from_bytes(icc_bytes),
            edge_size,
            &destination,
        )
    }

    fn from_profiles(
        source_color_space: ColorSpace,
        profile_fingerprint: IccProfileFingerprint,
        edge_size: u16,
        destination: &ColorProfile,
    ) -> Result<Self, DisplayCalibrationError> {
        validate_edge_size(edge_size)?;
        let source = cms_profile_for_color_space(source_color_space).ok_or(
            DisplayCalibrationError::UnsupportedSourceColorSpace(source_color_space),
        )?;
        let transform = source
            .create_transform_f32(
                Layout::Rgb,
                destination,
                Layout::Rgb,
                TransformOptions::default(),
            )
            .map_err(|error| DisplayCalibrationError::CreateTransform(error.to_string()))?;

        let edge = usize::from(edge_size);
        let texels = edge
            .checked_mul(edge)
            .and_then(|count| count.checked_mul(edge))
            .ok_or(DisplayCalibrationError::LutSizeOverflow { edge_size })?;
        let mut source_samples = Vec::with_capacity(texels * 3);
        let denominator = f32::from(edge_size - 1);
        for blue in 0..edge_size {
            for green in 0..edge_size {
                for red in 0..edge_size {
                    source_samples.extend_from_slice(&[
                        f32::from(red) / denominator,
                        f32::from(green) / denominator,
                        f32::from(blue) / denominator,
                    ]);
                }
            }
        }
        let mut transformed = vec![0.0_f32; source_samples.len()];
        transform
            .transform(&source_samples, &mut transformed)
            .map_err(|error| DisplayCalibrationError::ExecuteTransform(error.to_string()))?;

        let mut samples = Vec::with_capacity(texels * 4);
        for (component_index, rgb) in transformed.chunks_exact(3).enumerate() {
            if rgb.iter().any(|component| !component.is_finite()) {
                return Err(DisplayCalibrationError::NonFiniteOutput {
                    texel_index: component_index,
                });
            }
            samples.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }

        Self::from_rgba32f_samples(source_color_space, profile_fingerprint, edge_size, samples)
    }

    /// RGBA32F texels in renderer upload order.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Sample the LUT with the same normalized trilinear rule required on GPU.
    pub fn sample_trilinear(&self, rgb: [f32; 3]) -> [f32; 3] {
        let edge = usize::from(self.edge_size);
        let scale = (edge - 1) as f32;
        let coordinate = rgb.map(|value| value.clamp(0.0, 1.0) * scale);
        let lower = coordinate.map(|value| value.floor() as usize);
        let upper = lower.map(|value| (value + 1).min(edge - 1));
        let fraction = [
            coordinate[0] - lower[0] as f32,
            coordinate[1] - lower[1] as f32,
            coordinate[2] - lower[2] as f32,
        ];

        let mut output = [0.0_f32; 3];
        for z in 0..=1 {
            for y in 0..=1 {
                for x in 0..=1 {
                    let indices = [
                        if x == 0 { lower[0] } else { upper[0] },
                        if y == 0 { lower[1] } else { upper[1] },
                        if z == 0 { lower[2] } else { upper[2] },
                    ];
                    let weight = axis_weight(fraction[0], x)
                        * axis_weight(fraction[1], y)
                        * axis_weight(fraction[2], z);
                    let texel = self.texel(indices[0], indices[1], indices[2]);
                    for channel in 0..3 {
                        output[channel] += texel[channel] * weight;
                    }
                }
            }
        }
        output
    }

    fn texel(&self, red: usize, green: usize, blue: usize) -> [f32; 3] {
        let edge = usize::from(self.edge_size);
        let offset = ((blue * edge + green) * edge + red) * 4;
        [
            self.samples[offset],
            self.samples[offset + 1],
            self.samples[offset + 2],
        ]
    }
}

/// Calibration LUT construction failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DisplayCalibrationError {
    /// LUT edge must be odd and within the supported quality envelope.
    #[error("invalid display calibration LUT edge {edge_size}; expected an odd value in {MIN_LUT_EDGE}..={MAX_LUT_EDGE}")]
    InvalidEdgeSize { edge_size: u16 },
    /// Encoded source cannot be represented by the ICC engine.
    #[error("unsupported ICC calibration source color space {0:?}")]
    UnsupportedSourceColorSpace(ColorSpace),
    /// Destination ICC payload did not parse.
    #[error("invalid destination ICC profile: {0}")]
    InvalidIccProfile(String),
    /// ICC executor creation failed.
    #[error("failed to create display calibration transform: {0}")]
    CreateTransform(String),
    /// ICC executor failed while sampling the LUT.
    #[error("failed to execute display calibration transform: {0}")]
    ExecuteTransform(String),
    /// Edge-size arithmetic overflowed.
    #[error("display calibration LUT size overflow for edge {edge_size}")]
    LutSizeOverflow { edge_size: u16 },
    /// Precomputed sample payload does not match the cube extent.
    #[error("invalid display calibration sample count: expected {expected}, got {actual}")]
    InvalidSampleCount { expected: usize, actual: usize },
    /// ICC produced a non-finite device value.
    #[error("display calibration produced non-finite output at texel {texel_index}")]
    NonFiniteOutput { texel_index: usize },
}

fn validate_edge_size(edge_size: u16) -> Result<(), DisplayCalibrationError> {
    if !(MIN_LUT_EDGE..=MAX_LUT_EDGE).contains(&edge_size) || edge_size.is_multiple_of(2) {
        return Err(DisplayCalibrationError::InvalidEdgeSize { edge_size });
    }
    Ok(())
}

fn axis_weight(fraction: f32, upper: usize) -> f32 {
    if upper == 0 {
        1.0 - fraction
    } else {
        fraction
    }
}

pub(crate) fn cms_profile_for_color_space(color_space: ColorSpace) -> Option<ColorProfile> {
    match color_space {
        ColorSpace::Srgb => Some(ColorProfile::new_srgb()),
        ColorSpace::Rec709 => Some(ColorProfile::new_from_cicp(CicpProfile {
            color_primaries: CicpColorPrimaries::Bt709,
            transfer_characteristics: TransferCharacteristics::Bt709,
            matrix_coefficients: MatrixCoefficients::Bt709,
            full_range: false,
        })),
        ColorSpace::Rec2020 => Some(ColorProfile::new_bt2020()),
        ColorSpace::Rec2100Pq => Some(ColorProfile::new_bt2020_pq()),
        ColorSpace::Rec2100Hlg => Some(ColorProfile::new_bt2020_hlg()),
        ColorSpace::DisplayP3 => Some(ColorProfile::new_display_p3()),
        ColorSpace::Rec601Pal
        | ColorSpace::Rec601Ntsc
        | ColorSpace::AppleLogBt2020
        | ColorSpace::SonySLog3SGamut3
        | ColorSpace::SonySLog3SGamut3Cine
        | ColorSpace::ArriLogC3WideGamut3
        | ColorSpace::ArriLogC4WideGamut4
        | ColorSpace::CanonLog2CinemaGamutD55
        | ColorSpace::CanonLog3CinemaGamutD55
        | ColorSpace::PanasonicVLogVGamut
        | ColorSpace::RedLog3G10WideGamutRgb
        | ColorSpace::BlackmagicFilmWideGamutGen5
        | ColorSpace::DjiDLogDGamut
        | ColorSpace::DavinciIntermediateWideGamut => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_lut(edge_size: u16) -> DisplayCalibrationLut3d {
        let mut samples = Vec::new();
        let denominator = f32::from(edge_size - 1);
        for blue in 0..edge_size {
            for green in 0..edge_size {
                for red in 0..edge_size {
                    samples.extend_from_slice(&[
                        f32::from(red) / denominator,
                        f32::from(green) / denominator,
                        f32::from(blue) / denominator,
                        1.0,
                    ]);
                }
            }
        }
        DisplayCalibrationLut3d {
            source_color_space: ColorSpace::Srgb,
            profile_fingerprint: IccProfileFingerprint::from_bytes(b"synthetic"),
            edge_size,
            samples: samples.into(),
        }
    }

    #[test]
    fn fingerprint_is_content_derived_and_length_sensitive() {
        assert_eq!(
            IccProfileFingerprint::from_bytes(b"profile"),
            IccProfileFingerprint::from_bytes(b"profile")
        );
        assert_ne!(
            IccProfileFingerprint::from_bytes(b"profile"),
            IccProfileFingerprint::from_bytes(b"profile2")
        );
    }

    #[test]
    fn calibration_edge_validation_enforces_quality_envelope() {
        for edge_size in [0, 16, 18, 66] {
            assert_eq!(
                validate_edge_size(edge_size),
                Err(DisplayCalibrationError::InvalidEdgeSize { edge_size })
            );
        }
        assert!(validate_edge_size(17).is_ok());
        assert!(validate_edge_size(33).is_ok());
        assert!(validate_edge_size(65).is_ok());
    }

    #[test]
    fn precomputed_lut_rejects_unsupported_encoded_source() {
        let edge_size = 17;
        let sample_count = usize::from(edge_size).pow(3) * 4;
        let error = DisplayCalibrationLut3d::from_rgba32f_samples(
            ColorSpace::AppleLogBt2020,
            IccProfileFingerprint::from_bytes(b"unsupported"),
            edge_size,
            vec![0.0; sample_count],
        )
        .expect_err("camera log is not a monitor calibration source");

        assert_eq!(
            error,
            DisplayCalibrationError::UnsupportedSourceColorSpace(ColorSpace::AppleLogBt2020)
        );
    }

    #[test]
    fn trilinear_reference_preserves_identity_and_clamps_domain() {
        let lut = synthetic_lut(17);
        for input in [[0.0, 0.0, 0.0], [0.1, 0.5, 0.9], [1.0, 1.0, 1.0]] {
            let output = lut.sample_trilinear(input);
            for channel in 0..3 {
                assert!((output[channel] - input[channel]).abs() < 1.0e-6);
            }
        }
        assert_eq!(lut.sample_trilinear([-1.0, 0.5, 2.0]), [0.0, 0.5, 1.0]);
    }

    #[test]
    fn generated_identity_profile_lut_matches_direct_transform() {
        let destination = ColorProfile::new_srgb();
        let lut = DisplayCalibrationLut3d::from_profiles(
            ColorSpace::Srgb,
            IccProfileFingerprint::from_bytes(b"synthetic-srgb"),
            33,
            &destination,
        )
        .expect("sRGB calibration LUT");
        let direct = cms_profile_for_color_space(ColorSpace::Srgb)
            .expect("sRGB profile")
            .create_transform_f32(
                Layout::Rgb,
                &destination,
                Layout::Rgb,
                TransformOptions::default(),
            )
            .expect("direct transform");

        for input in [[0.02, 0.18, 0.73], [0.25, 0.5, 0.75], [0.91, 0.4, 0.07]] {
            let mut expected = [0.0_f32; 3];
            direct.transform(&input, &mut expected).expect("direct sample");
            let actual = lut.sample_trilinear(input);
            for channel in 0..3 {
                assert!((actual[channel] - expected[channel]).abs() < 2.0e-4);
            }
        }
    }
}
