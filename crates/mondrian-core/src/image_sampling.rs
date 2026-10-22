//! Image sampling density, independent of placement, crop and project geometry.

use crate::{Rational, Resolution, SampleAspectRatio};
use serde::{Deserialize, Serialize};

/// Original sample shape and current X/Y sampling rates of an image value.
///
/// Pixel coordinates map to canonical distances as `x * PAR / SX`, `y / SY`.
/// This value does not prove an image's origin, orientation or full-frame extent.
/// In particular, cropping does not change sampling density. Owners of windows
/// and placement transforms retain their existing coordinate responsibilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ImageSamplingWire")]
pub struct ImageSampling {
    sample_aspect_ratio: SampleAspectRatio,
    render_scale: [Rational; 2],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageSamplingWire {
    sample_aspect_ratio: SampleAspectRatio,
    render_scale: [Rational; 2],
}

impl TryFrom<ImageSamplingWire> for ImageSampling {
    type Error = ImageSamplingError;

    fn try_from(wire: ImageSamplingWire) -> Result<Self, Self::Error> {
        Self::new(wire.sample_aspect_ratio, wire.render_scale)
    }
}

impl ImageSampling {
    /// Explicit unit-rate square-pixel sampling, never an inferred fallback.
    pub const UNIT_SQUARE: Self = Self {
        sample_aspect_ratio: SampleAspectRatio::SQUARE,
        render_scale: [Rational::new(1, 1); 2],
    };

    /// Validate and canonicalize one complete sampling declaration.
    pub fn new(
        sample_aspect_ratio: SampleAspectRatio,
        render_scale: [Rational; 2],
    ) -> Result<Self, ImageSamplingError> {
        // SampleAspectRatio's external serde representation can contain zero
        // or unreduced components. Re-admit it at this checked value boundary.
        let sample_aspect_ratio = SampleAspectRatio::new(
            sample_aspect_ratio.numerator(),
            sample_aspect_ratio.denominator(),
        )
        .ok_or(ImageSamplingError::InvalidSampleAspectRatio)?;
        let mut canonical = render_scale;
        for (axis, scale) in canonical.iter_mut().enumerate() {
            if scale.num <= 0 || scale.den <= 0 {
                return Err(ImageSamplingError::InvalidRenderScale { axis });
            }
            *scale = scale.reduce();
        }
        Ok(Self { sample_aspect_ratio, render_scale: canonical })
    }

    /// Bind a proven full-raster resampling using its actual output dimensions.
    ///
    /// The caller proves that the two extents cover the same complete raster
    /// before orientation. A cropped window cannot use this constructor to
    /// recover its original sampling from its cropped dimensions.
    pub fn from_full_raster(
        sample_aspect_ratio: SampleAspectRatio,
        reference: Resolution,
        sampled: Resolution,
    ) -> Result<Self, ImageSamplingError> {
        if reference.width == 0 || reference.height == 0 {
            return Err(ImageSamplingError::EmptyReferenceRaster);
        }
        if sampled.width == 0 || sampled.height == 0 {
            return Err(ImageSamplingError::EmptySampledRaster);
        }
        Self::new(
            sample_aspect_ratio,
            [
                Rational::new(i64::from(sampled.width), i64::from(reference.width)),
                Rational::new(i64::from(sampled.height), i64::from(reference.height)),
            ],
        )
    }

    /// Preserve an already executed uniform f32 rasterization rate exactly.
    ///
    /// This admits the binary value used for rasterization rather than
    /// recomputing a higher-precision ideal ratio or quantizing it to a preset.
    /// Values outside positive i64 rational capacity refuse without rounding.
    pub fn from_uniform_f32(
        sample_aspect_ratio: SampleAspectRatio,
        scale: f32,
    ) -> Result<Self, ImageSamplingError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(ImageSamplingError::InvalidRenderScale { axis: 0 });
        }
        let bits = scale.to_bits();
        let exponent = ((bits >> 23) & 0xff) as i32;
        let mut mantissa = u128::from(bits & 0x7f_ffff);
        let mut power = if exponent == 0 {
            -149
        } else {
            mantissa |= 1 << 23;
            exponent - 150
        };
        let trailing = mantissa.trailing_zeros();
        mantissa >>= trailing;
        power += trailing as i32;
        let (numerator, denominator) = if power >= 0 {
            let factor = 1_u128
                .checked_shl(power as u32)
                .ok_or(ImageSamplingError::ScaleNotRepresentable)?;
            (
                mantissa.checked_mul(factor).ok_or(ImageSamplingError::ScaleNotRepresentable)?,
                1,
            )
        } else {
            (
                mantissa,
                1_u128
                    .checked_shl((-power) as u32)
                    .ok_or(ImageSamplingError::ScaleNotRepresentable)?,
            )
        };
        let scale = Rational::new(
            i64::try_from(numerator).map_err(|_| ImageSamplingError::ScaleNotRepresentable)?,
            i64::try_from(denominator).map_err(|_| ImageSamplingError::ScaleNotRepresentable)?,
        );
        Self::new(sample_aspect_ratio, [scale; 2])
    }

    /// Original interpreted pixel shape, independent of render scale.
    pub const fn sample_aspect_ratio(self) -> SampleAspectRatio {
        self.sample_aspect_ratio
    }

    /// Positive, reduced X/Y sampling rates.
    pub const fn render_scale(self) -> [Rational; 2] {
        self.render_scale
    }

    /// Whether both sampling rates are exactly one; not a full-frame proof.
    pub const fn is_unit_scale(self) -> bool {
        self.render_scale[0].num == 1
            && self.render_scale[0].den == 1
            && self.render_scale[1].num == 1
            && self.render_scale[1].den == 1
    }
}

/// A sampling declaration cannot be retained without changing its meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImageSamplingError {
    /// The source pixel shape contains a zero component.
    #[error("image sample aspect ratio must be positive")]
    InvalidSampleAspectRatio,
    /// An axis has a zero or negative scale numerator or denominator.
    #[error("image render scale axis {axis} must have positive components")]
    InvalidRenderScale {
        /// Zero-based X or Y axis.
        axis: usize,
    },
    /// A full-raster reference has no samples.
    #[error("image sampling reference raster must be nonempty")]
    EmptyReferenceRaster,
    /// A resampled raster has no samples.
    #[error("image sampling output raster must be nonempty")]
    EmptySampledRaster,
    /// Exact float conversion exceeds the positive rational carrier.
    #[error("image render scale is not exactly representable as an i64 rational")]
    ScaleNotRepresentable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PictureOrientation, PictureStreamMetadata, ResolvedPictureGeometry};

    #[test]
    fn full_raster_sampling_uses_actual_axes_and_preserves_source_interpretation() {
        let geometry = ResolvedPictureGeometry::resolve(
            Resolution { width: 1921, height: 1081 },
            PictureStreamMetadata {
                sample_aspect_ratio: SampleAspectRatio::new(4, 3),
                orientation: PictureOrientation::RotateClockwise90,
                ..PictureStreamMetadata::default()
            },
            None,
            None,
        )
        .unwrap();
        let sampling = geometry.image_sampling(Resolution { width: 960, height: 540 }).unwrap();
        assert_eq!(
            sampling.sample_aspect_ratio(),
            SampleAspectRatio::new(4, 3).unwrap()
        );
        assert_eq!(
            sampling.render_scale(),
            [Rational::new(960, 1921), Rational::new(540, 1081)]
        );
        assert!(!sampling.is_unit_scale());
        assert!(geometry.image_sampling(geometry.encoded_resolution()).unwrap().is_unit_scale());
    }

    #[test]
    fn rasterized_f32_rate_is_retained_without_idealizing_fractional_title_spans() {
        let shape = SampleAspectRatio::new(2, 1).unwrap();
        let sampling = ImageSampling::from_uniform_f32(shape, 0.75).unwrap();
        assert_eq!(sampling.render_scale(), [Rational::new(3, 4); 2]);
        // The crop is 25 samples wide, not an integer-sized reference raster.
        assert_eq!(25.0 / sampling.render_scale()[0].to_f64(), 100.0 / 3.0);
        let third = ImageSampling::from_uniform_f32(shape, 1.0_f32 / 3.0).unwrap();
        assert_eq!(
            third.render_scale(),
            [Rational::new(11_184_811, 33_554_432); 2]
        );
        let denominator_boundary = f32::from_bits((127 - 62) << 23);
        assert_eq!(
            ImageSampling::from_uniform_f32(shape, denominator_boundary)
                .unwrap()
                .render_scale(),
            [Rational::new(1, 1_i64 << 62); 2]
        );
        let numerator_limit = f32::from_bits((127 + 63) << 23);
        assert_eq!(
            ImageSampling::from_uniform_f32(shape, f32::from_bits(numerator_limit.to_bits() - 1))
                .unwrap()
                .render_scale(),
            [Rational::new(16_777_215_i64 << 39, 1); 2]
        );
        for scale in [f32::from_bits((127 - 63) << 23), numerator_limit] {
            assert_eq!(
                ImageSampling::from_uniform_f32(shape, scale),
                Err(ImageSamplingError::ScaleNotRepresentable)
            );
        }
        for scale in [f32::MIN_POSITIVE, f32::MAX, f32::from_bits(1)] {
            assert_eq!(
                ImageSampling::from_uniform_f32(shape, scale),
                Err(ImageSamplingError::ScaleNotRepresentable)
            );
        }
        for scale in [0.0, -0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(ImageSampling::from_uniform_f32(shape, scale).is_err());
        }
    }

    #[test]
    fn sampling_wire_revalidates_shape_and_scales_and_normalizes_identity() {
        let wire = r#"{"sample_aspect_ratio":{"numerator":12,"denominator":8},"render_scale":[{"num":2,"den":4},{"num":6,"den":8}]}"#;
        let sampling: ImageSampling = serde_json::from_str(wire).unwrap();
        let expected = ImageSampling::new(
            SampleAspectRatio::new(3, 2).unwrap(),
            [Rational::new(1, 2), Rational::new(3, 4)],
        )
        .unwrap();
        assert_eq!(sampling, expected);
        assert_eq!(
            serde_json::to_value(sampling).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        for invalid in [
            wire.replace("\"numerator\":12", "\"numerator\":0"),
            wire.replace("\"denominator\":8", "\"denominator\":0"),
            wire.replace("\"num\":2", "\"num\":0"),
            wire.replace("\"num\":6", "\"num\":-1"),
            wire.replace("\"den\":4", "\"den\":0"),
            wire.replace("\"den\":8", "\"den\":-1"),
            wire.replacen('{', "{\"offset\":1,", 1),
        ] {
            assert!(
                serde_json::from_str::<ImageSampling>(&invalid).is_err(),
                "{invalid}"
            );
        }
        let restored: ImageSampling =
            serde_json::from_str(&serde_json::to_string(&sampling).unwrap()).unwrap();
        assert_eq!(restored, sampling);
        assert_eq!(
            ImageSampling::from_full_raster(
                SampleAspectRatio::SQUARE,
                Resolution { width: 0, height: 5 },
                Resolution { width: 3, height: 5 },
            ),
            Err(ImageSamplingError::EmptyReferenceRaster)
        );
        assert_eq!(
            ImageSampling::from_full_raster(
                SampleAspectRatio::SQUARE,
                Resolution { width: 3, height: 5 },
                Resolution { width: 3, height: 0 },
            ),
            Err(ImageSamplingError::EmptySampledRaster)
        );
    }
}
