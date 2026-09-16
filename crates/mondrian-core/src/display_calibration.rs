//! Monitor ICC calibration LUT generation and CPU reference sampling.

use crate::color_models::IccRenderingIntent;
use crate::types::ColorSpace;
use moxcms::{
    CicpColorPrimaries, CicpProfile, ColorProfile, Layout, MatrixCoefficients, RenderingIntent,
    TransferCharacteristics, TransformOptions,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use thiserror::Error;

/// Default edge size for renderer monitor-calibration 3D LUTs.
pub const DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE: u16 = 33;
const MIN_LUT_EDGE: u16 = 17;
const MAX_LUT_EDGE: u16 = 65;

/// Content-derived identity for an ICC payload.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IccProfileFingerprint([u8; 32]);

impl std::fmt::Debug for IccProfileFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IccProfileFingerprint({:02x?})", &self.0[..8])
    }
}

impl IccProfileFingerprint {
    /// Compute a stable identity from the complete ICC payload.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(sha256_identity(
            b"mondrian.icc-profile-fingerprint.v1",
            bytes,
        ))
    }

    /// Borrow the complete content identity.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Full identity carried by renderer frame contracts.
    pub fn calibration_key(self) -> DisplayCalibrationKey {
        DisplayCalibrationKey(self.0)
    }

    /// Compact diagnostic projection. Never use this value for equality.
    pub fn diagnostic_key(self) -> u64 {
        diagnostic_key(self.0)
    }
}

/// Full ICC identity carried by device-color frame contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DisplayCalibrationKey([u8; 32]);

impl DisplayCalibrationKey {
    /// Borrow the complete calibration identity.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compact diagnostic projection. Never use this value for equality.
    pub fn diagnostic_key(self) -> u64 {
        diagnostic_key(self.0)
    }
}

/// Content identity of a complete sampled display-calibration LUT contract.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplayCalibrationLutIdentity([u8; 32]);

impl std::fmt::Debug for DisplayCalibrationLutIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DisplayCalibrationLutIdentity({:02x?})", &self.0[..8])
    }
}

impl DisplayCalibrationLutIdentity {
    /// Borrow the complete LUT identity.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compact diagnostic projection. Never use this value for equality.
    pub fn diagnostic_key(self) -> u64 {
        diagnostic_key(self.0)
    }
}

/// Renderer-ready monitor calibration sampled over normalized encoded RGB.
#[derive(Debug, Clone)]
pub struct DisplayCalibrationLut3d {
    /// Encoded standard/output identity consumed by the ICC transform.
    source_color_space: ColorSpace,
    /// Destination ICC payload identity.
    profile_fingerprint: IccProfileFingerprint,
    /// Number of samples along each RGB axis.
    edge_size: u16,
    /// RGBA32F texels in x/R-fastest order.
    samples: Arc<[f32]>,
    identity: DisplayCalibrationLutIdentity,
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
        let identity =
            calibration_lut_identity(source_color_space, profile_fingerprint, edge_size, &samples);
        Ok(Self {
            source_color_space,
            profile_fingerprint,
            edge_size,
            samples: samples.into(),
            identity,
        })
    }

    /// Build a default-size renderer calibration LUT from an ICC payload.
    pub fn from_icc_bytes(
        source_color_space: ColorSpace,
        icc_bytes: &[u8],
    ) -> Result<Self, DisplayCalibrationError> {
        Self::from_icc_bytes_with_intent(
            source_color_space,
            icc_bytes,
            IccRenderingIntent::Perceptual,
        )
    }

    /// Build a default-size calibration LUT with an explicit ICC rendering intent.
    pub fn from_icc_bytes_with_intent(
        source_color_space: ColorSpace,
        icc_bytes: &[u8],
        rendering_intent: IccRenderingIntent,
    ) -> Result<Self, DisplayCalibrationError> {
        Self::from_icc_bytes_with_edge_and_intent(
            source_color_space,
            icc_bytes,
            DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE,
            rendering_intent,
        )
    }

    /// Build a renderer calibration LUT with an explicit validated edge size.
    pub fn from_icc_bytes_with_edge(
        source_color_space: ColorSpace,
        icc_bytes: &[u8],
        edge_size: u16,
    ) -> Result<Self, DisplayCalibrationError> {
        Self::from_icc_bytes_with_edge_and_intent(
            source_color_space,
            icc_bytes,
            edge_size,
            IccRenderingIntent::Perceptual,
        )
    }

    /// Build a calibration LUT with explicit edge size and rendering intent.
    pub fn from_icc_bytes_with_edge_and_intent(
        source_color_space: ColorSpace,
        icc_bytes: &[u8],
        edge_size: u16,
        rendering_intent: IccRenderingIntent,
    ) -> Result<Self, DisplayCalibrationError> {
        let destination = ColorProfile::new_from_slice(icc_bytes)
            .map_err(|error| DisplayCalibrationError::InvalidIccProfile(error.to_string()))?;
        Self::from_profiles(
            source_color_space,
            IccProfileFingerprint::from_bytes(icc_bytes),
            edge_size,
            &destination,
            rendering_intent,
        )
    }

    fn from_profiles(
        source_color_space: ColorSpace,
        profile_fingerprint: IccProfileFingerprint,
        edge_size: u16,
        destination: &ColorProfile,
        rendering_intent: IccRenderingIntent,
    ) -> Result<Self, DisplayCalibrationError> {
        validate_edge_size(edge_size)?;
        let source = cms_profile_for_color_space(source_color_space).ok_or(
            DisplayCalibrationError::UnsupportedSourceColorSpace(source_color_space),
        )?;
        let transform_options = TransformOptions {
            rendering_intent: match rendering_intent {
                IccRenderingIntent::Perceptual => RenderingIntent::Perceptual,
                IccRenderingIntent::RelativeColorimetric => RenderingIntent::RelativeColorimetric,
                IccRenderingIntent::Saturation => RenderingIntent::Saturation,
                IccRenderingIntent::AbsoluteColorimetric => RenderingIntent::AbsoluteColorimetric,
            },
            ..TransformOptions::default()
        };
        let transform = source
            .create_transform_f32(Layout::Rgb, destination, Layout::Rgb, transform_options)
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

    /// Encoded standard/output identity consumed by the ICC transform.
    pub const fn source_color_space(&self) -> ColorSpace {
        self.source_color_space
    }

    /// Destination ICC payload identity.
    pub const fn profile_fingerprint(&self) -> IccProfileFingerprint {
        self.profile_fingerprint
    }

    /// Number of samples along each RGB axis.
    pub const fn edge_size(&self) -> u16 {
        self.edge_size
    }

    /// Full identity of metadata and every sampled texel.
    pub const fn identity(&self) -> DisplayCalibrationLutIdentity {
        self.identity
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

fn sha256_identity(domain: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update((domain.len() as u64).to_le_bytes());
    digest.update(domain);
    digest.update((payload.len() as u64).to_le_bytes());
    digest.update(payload);
    digest.finalize().into()
}

fn calibration_lut_identity(
    source_color_space: ColorSpace,
    profile_fingerprint: IccProfileFingerprint,
    edge_size: u16,
    samples: &[f32],
) -> DisplayCalibrationLutIdentity {
    let mut digest = Sha256::new();
    let domain = b"mondrian.display-calibration-lut.v1";
    digest.update((domain.len() as u64).to_le_bytes());
    digest.update(domain);
    let color_space = match serde_json::to_vec(&source_color_space) {
        Ok(color_space) => color_space,
        Err(error) => unreachable!("ColorSpace serialization is infallible: {error}"),
    };
    digest.update((color_space.len() as u64).to_le_bytes());
    digest.update(color_space);
    digest.update(profile_fingerprint.as_bytes());
    digest.update(edge_size.to_le_bytes());
    digest.update((samples.len() as u64).to_le_bytes());
    for sample in samples {
        digest.update(sample.to_bits().to_le_bytes());
    }
    DisplayCalibrationLutIdentity(digest.finalize().into())
}

fn diagnostic_key(identity: [u8; 32]) -> u64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&identity[..8]);
    u64::from_le_bytes(bytes)
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
        | ColorSpace::LinearRec709
        | ColorSpace::LinearRec2020
        | ColorSpace::LinearP3D65
        | ColorSpace::Aces2065_1
        | ColorSpace::AcesCg
        | ColorSpace::AcesCct
        | ColorSpace::AppleLogBt2020
        | ColorSpace::SonySLog2SGamut
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
        DisplayCalibrationLut3d::from_rgba32f_samples(
            ColorSpace::Srgb,
            IccProfileFingerprint::from_bytes(b"synthetic"),
            edge_size,
            samples,
        )
        .expect("synthetic calibration LUT")
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
    fn compact_diagnostic_collision_does_not_make_full_identities_equal() {
        let first = IccProfileFingerprint([0_u8; 32]);
        let mut second_bytes = [0_u8; 32];
        second_bytes[31] = 1;
        let second = IccProfileFingerprint(second_bytes);

        assert_eq!(first.diagnostic_key(), second.diagnostic_key());
        assert_ne!(first, second);
        assert_ne!(first.calibration_key(), second.calibration_key());
    }

    #[test]
    fn lut_identity_binds_sample_payload_even_with_same_profile_declaration() {
        let first = synthetic_lut(17);
        let mut samples = first.samples().to_vec();
        samples[0] = 0.25;
        let second = DisplayCalibrationLut3d::from_rgba32f_samples(
            first.source_color_space(),
            first.profile_fingerprint(),
            first.edge_size(),
            samples,
        )
        .expect("modified calibration LUT");

        assert_eq!(first.profile_fingerprint(), second.profile_fingerprint());
        assert_ne!(first.identity(), second.identity());
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
            IccRenderingIntent::Perceptual,
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
