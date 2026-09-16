//! Backend-neutral HSL/3D color qualification and matte refinement.
//!
//! A qualifier consumes scene-linear working RGB and produces an AlphaMask
//! value. Selection and refinement live together because both CPU and GPU
//! backends must agree on the exact matte before any Mask or preview node uses
//! it; compositing remains a separate graph operation.

use mondrian_core::{
    automation::{QualifierSampleOperation, QualifierSampleSet},
    WorkingColorSpace,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Maximum spatial radius admitted by the product qualifier contract.
pub const MAX_QUALIFIER_DENOISE_RADIUS: u32 = 4;
/// Maximum Gaussian matte-feather sigma admitted by the product contract.
pub const MAX_QUALIFIER_BLUR_RADIUS: f32 = 12.0;

/// Authored qualifier selection model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualifierMode {
    /// Independent circular hue, saturation, and luminance ranges.
    Hsl,
    /// Union/subtraction of sampled volumes in normalized RGB cube space.
    ThreeDimensional,
}

/// Borrowed authoring values prepared into immutable execution state.
#[derive(Debug, Clone, Copy)]
pub struct QualifierAuthoring<'a> {
    pub mode: QualifierMode,
    pub hue_center_degrees: f32,
    pub hue_width_degrees: f32,
    pub hue_softness_degrees: f32,
    pub saturation_low: f32,
    pub saturation_high: f32,
    pub saturation_softness: f32,
    pub luminance_low: f32,
    pub luminance_high: f32,
    pub luminance_softness: f32,
    pub samples: &'a QualifierSampleSet,
    pub three_d_tolerance: f32,
    pub three_d_softness: f32,
    pub denoise_radius: u32,
    pub blur_radius: f32,
    pub clean_black: f32,
    pub clean_white: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PreparedQualifierSample {
    coordinate: [f32; 3],
    operation: QualifierSampleOperation,
}

/// Immutable qualifier parameters shared by CPU and GPU execution.
#[derive(Debug, Clone)]
pub struct PreparedQualifier {
    mode: QualifierMode,
    hue_center: f32,
    hue_half_width: f32,
    hue_softness: f32,
    saturation_low: f32,
    saturation_high: f32,
    saturation_softness: f32,
    luminance_low: f32,
    luminance_high: f32,
    luminance_softness: f32,
    luminance_coefficients: [f32; 3],
    samples: Arc<[PreparedQualifierSample]>,
    three_d_tolerance: f32,
    three_d_softness: f32,
    denoise_radius: u32,
    blur_radius: f32,
    clean_black: f32,
    clean_white: f32,
    semantic_fingerprint: [u8; 32],
}

impl PartialEq for PreparedQualifier {
    fn eq(&self, other: &Self) -> bool {
        self.semantic_fingerprint == other.semantic_fingerprint
            && self.mode == other.mode
            && self.hue_center.to_bits() == other.hue_center.to_bits()
            && self.hue_half_width.to_bits() == other.hue_half_width.to_bits()
            && self.hue_softness.to_bits() == other.hue_softness.to_bits()
            && self.saturation_low.to_bits() == other.saturation_low.to_bits()
            && self.saturation_high.to_bits() == other.saturation_high.to_bits()
            && self.saturation_softness.to_bits() == other.saturation_softness.to_bits()
            && self.luminance_low.to_bits() == other.luminance_low.to_bits()
            && self.luminance_high.to_bits() == other.luminance_high.to_bits()
            && self.luminance_softness.to_bits() == other.luminance_softness.to_bits()
            && self.luminance_coefficients == other.luminance_coefficients
            && self.samples == other.samples
            && self.three_d_tolerance.to_bits() == other.three_d_tolerance.to_bits()
            && self.three_d_softness.to_bits() == other.three_d_softness.to_bits()
            && self.denoise_radius == other.denoise_radius
            && self.blur_radius.to_bits() == other.blur_radius.to_bits()
            && self.clean_black.to_bits() == other.clean_black.to_bits()
            && self.clean_white.to_bits() == other.clean_white.to_bits()
    }
}

impl PreparedQualifier {
    /// Validate and prepare one authored qualifier.
    pub fn new(
        authored: QualifierAuthoring<'_>,
        working_color_space: WorkingColorSpace,
    ) -> Result<Self, QualifierError> {
        authored.samples.validate().map_err(|_| QualifierError::InvalidSamples)?;
        let finite = [
            authored.hue_center_degrees,
            authored.hue_width_degrees,
            authored.hue_softness_degrees,
            authored.saturation_low,
            authored.saturation_high,
            authored.saturation_softness,
            authored.luminance_low,
            authored.luminance_high,
            authored.luminance_softness,
            authored.three_d_tolerance,
            authored.three_d_softness,
            authored.blur_radius,
            authored.clean_black,
            authored.clean_white,
        ];
        if finite.into_iter().any(|value| !value.is_finite()) {
            return Err(QualifierError::NonFiniteParameter);
        }
        if authored.denoise_radius > MAX_QUALIFIER_DENOISE_RADIUS
            || !(0.0..=MAX_QUALIFIER_BLUR_RADIUS).contains(&authored.blur_radius)
        {
            return Err(QualifierError::RefinementRadiusOutOfRange);
        }

        let saturation_low = authored.saturation_low.min(authored.saturation_high).clamp(0.0, 1.0);
        let saturation_high = authored.saturation_low.max(authored.saturation_high).clamp(0.0, 1.0);
        let luminance_low = authored.luminance_low.min(authored.luminance_high).clamp(0.0, 1.0);
        let luminance_high = authored.luminance_low.max(authored.luminance_high).clamp(0.0, 1.0);
        let samples = authored
            .samples
            .samples()
            .iter()
            .map(|sample| PreparedQualifierSample {
                coordinate: normalized_rgb_coordinate(sample.rgb),
                operation: sample.operation,
            })
            .collect::<Vec<_>>();
        let mut prepared = Self {
            mode: authored.mode,
            hue_center: positive_mod(authored.hue_center_degrees / 360.0, 1.0),
            hue_half_width: (authored.hue_width_degrees / 720.0).clamp(0.0, 0.5),
            hue_softness: (authored.hue_softness_degrees / 360.0).clamp(0.0, 0.5),
            saturation_low,
            saturation_high,
            saturation_softness: authored.saturation_softness.clamp(0.0, 1.0),
            luminance_low,
            luminance_high,
            luminance_softness: authored.luminance_softness.clamp(0.0, 1.0),
            luminance_coefficients: working_color_space.luminance_coefficients(),
            samples: samples.into(),
            three_d_tolerance: authored.three_d_tolerance.clamp(0.0, 2.0),
            three_d_softness: authored.three_d_softness.clamp(0.0, 2.0),
            denoise_radius: authored.denoise_radius,
            blur_radius: authored.blur_radius,
            clean_black: authored.clean_black.clamp(0.0, 0.49),
            clean_white: authored.clean_white.clamp(0.0, 0.49),
            semantic_fingerprint: [0; 32],
        };
        prepared.semantic_fingerprint = prepared.compute_semantic_fingerprint();
        Ok(prepared)
    }

    /// Complete semantic fingerprint used by graph and GPU cache identities.
    pub const fn semantic_fingerprint(&self) -> &[u8; 32] {
        &self.semantic_fingerprint
    }

    /// Conservative logical bytes retained by this prepared payload.
    pub fn retained_bytes_estimate(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(
            self.samples
                .len()
                .saturating_mul(std::mem::size_of::<PreparedQualifierSample>()),
        )
    }

    pub const fn mode(&self) -> QualifierMode {
        self.mode
    }

    pub const fn hue_controls(&self) -> [f32; 3] {
        [self.hue_center, self.hue_half_width, self.hue_softness]
    }

    pub const fn saturation_controls(&self) -> [f32; 3] {
        [
            self.saturation_low,
            self.saturation_high,
            self.saturation_softness,
        ]
    }

    pub const fn luminance_controls(&self) -> [f32; 3] {
        [
            self.luminance_low,
            self.luminance_high,
            self.luminance_softness,
        ]
    }

    pub const fn luminance_coefficients(&self) -> [f32; 3] {
        self.luminance_coefficients
    }

    pub fn samples(
        &self,
    ) -> impl ExactSizeIterator<Item = ([f32; 3], QualifierSampleOperation)> + '_ {
        self.samples.iter().map(|sample| (sample.coordinate, sample.operation))
    }

    pub const fn three_d_controls(&self) -> [f32; 2] {
        [self.three_d_tolerance, self.three_d_softness]
    }

    pub const fn denoise_radius(&self) -> u32 {
        self.denoise_radius
    }

    pub const fn blur_radius(&self) -> f32 {
        self.blur_radius
    }

    pub const fn clean_controls(&self) -> [f32; 2] {
        [self.clean_black, self.clean_white]
    }

    /// Conservative spatial halo needed by the exact separable refinement.
    pub fn input_halo(&self) -> u32 {
        self.denoise_radius.saturating_add(gaussian_kernel_radius(self.blur_radius))
    }

    /// Exact full-frame GPU passes used by the separable refinement kernel.
    pub fn gpu_pass_count(&self) -> u32 {
        match (self.denoise_radius > 0, self.blur_radius > f32::EPSILON) {
            (false, false) => 1,
            (true, true) => 4,
            _ => 2,
        }
    }

    /// Evaluate the unrefined selection for one scene-linear RGB sample.
    pub fn raw_matte(&self, rgb: [f32; 3]) -> f32 {
        match self.mode {
            QualifierMode::Hsl => self.raw_hsl_matte(rgb),
            QualifierMode::ThreeDimensional => self.raw_three_dimensional_matte(rgb),
        }
    }

    fn raw_hsl_matte(&self, rgb: [f32; 3]) -> f32 {
        let positive = rgb.map(|channel| channel.max(0.0));
        let (hue, saturation) = rgb_hue_saturation(positive);
        let luminance = tone_map_positive(dot(positive, self.luminance_coefficients));
        let hue_distance = circular_distance(hue, self.hue_center);
        let hue_matte = if self.hue_half_width >= 0.5 {
            1.0
        } else {
            1.0 - smoothstep(
                self.hue_half_width,
                (self.hue_half_width + self.hue_softness).min(0.5),
                hue_distance,
            )
        };
        hue_matte
            * range_matte(
                saturation,
                self.saturation_low,
                self.saturation_high,
                self.saturation_softness,
            )
            * range_matte(
                luminance,
                self.luminance_low,
                self.luminance_high,
                self.luminance_softness,
            )
    }

    fn raw_three_dimensional_matte(&self, rgb: [f32; 3]) -> f32 {
        let coordinate = normalized_rgb_coordinate(rgb);
        let mut included: f32 = 0.0;
        let mut excluded: f32 = 0.0;
        for sample in self.samples.iter() {
            let delta = [
                coordinate[0] - sample.coordinate[0],
                coordinate[1] - sample.coordinate[1],
                coordinate[2] - sample.coordinate[2],
            ];
            let distance = dot(delta, delta).sqrt();
            let contribution = 1.0
                - smoothstep(
                    self.three_d_tolerance,
                    self.three_d_tolerance + self.three_d_softness,
                    distance,
                );
            match sample.operation {
                QualifierSampleOperation::Include => included = included.max(contribution),
                QualifierSampleOperation::Exclude => excluded = excluded.max(contribution),
            }
        }
        included * (1.0 - excluded)
    }

    fn compute_semantic_fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"mondrian.prepared-qualifier.v1");
        hasher.update([match self.mode {
            QualifierMode::Hsl => 0,
            QualifierMode::ThreeDimensional => 1,
        }]);
        for value in [
            self.hue_center,
            self.hue_half_width,
            self.hue_softness,
            self.saturation_low,
            self.saturation_high,
            self.saturation_softness,
            self.luminance_low,
            self.luminance_high,
            self.luminance_softness,
            self.three_d_tolerance,
            self.three_d_softness,
            self.blur_radius,
            self.clean_black,
            self.clean_white,
        ]
        .into_iter()
        .chain(self.luminance_coefficients)
        {
            hasher.update(value.to_bits().to_le_bytes());
        }
        hasher.update(self.denoise_radius.to_le_bytes());
        hasher.update((self.samples.len() as u64).to_le_bytes());
        for sample in self.samples.iter() {
            hasher.update([match sample.operation {
                QualifierSampleOperation::Include => 0,
                QualifierSampleOperation::Exclude => 1,
            }]);
            for channel in sample.coordinate {
                hasher.update(channel.to_bits().to_le_bytes());
            }
        }
        hasher.finalize().into()
    }
}

/// Invalid qualifier preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QualifierError {
    #[error("qualifier contains a non-finite parameter")]
    NonFiniteParameter,
    #[error("qualifier sample set is invalid")]
    InvalidSamples,
    #[error("qualifier matte-refinement radius is outside the admitted range")]
    RefinementRadiusOutOfRange,
}

/// Execute one qualifier into an AlphaMask-domain RGBA32F value.
///
/// RGB channels are canonical zero and alpha carries the matte. The function
/// uses two bounded scalar scratch planes irrespective of radius; callers may
/// conservatively admit one RGBA32F scratch frame.
pub(crate) fn apply_qualifier_rgba_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    qualifier: &PreparedQualifier,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let count = width.saturating_mul(height);
    if count == 0 || pixels.len() != count {
        return Ok(());
    }
    let mut matte = vec![0.0_f32; count];
    for (row, source) in pixels.chunks_exact(width).enumerate() {
        checkpoint()?;
        for (column, pixel) in source.iter().enumerate() {
            matte[row * width + column] = qualifier.raw_matte([pixel[0], pixel[1], pixel[2]]);
        }
    }
    let mut scratch = vec![0.0_f32; count];
    if qualifier.denoise_radius > 0 {
        separable_box_blur(
            &mut matte,
            &mut scratch,
            width,
            height,
            qualifier.denoise_radius as usize,
            checkpoint,
        )?;
    }
    if qualifier.blur_radius > f32::EPSILON {
        separable_gaussian_blur(
            &mut matte,
            &mut scratch,
            width,
            height,
            qualifier.blur_radius,
            checkpoint,
        )?;
    }
    let lower = qualifier.clean_black;
    let upper = 1.0 - qualifier.clean_white;
    for (row, output) in pixels.chunks_exact_mut(width).enumerate() {
        checkpoint()?;
        for (column, pixel) in output.iter_mut().enumerate() {
            let value = smoothstep(lower, upper, matte[row * width + column]);
            *pixel = [0.0, 0.0, 0.0, value];
        }
    }
    Ok(())
}

/// Convert an AlphaMask-domain value to an opaque black/white working preview.
pub(crate) fn apply_matte_preview_rgba_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    invert: bool,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for chunk in pixels.chunks_mut(4096) {
        checkpoint()?;
        for pixel in chunk {
            let matte = pixel[3].clamp(0.0, 1.0);
            let value = if invert { 1.0 - matte } else { matte };
            *pixel = [value, value, value, 1.0];
        }
    }
    Ok(())
}

fn separable_box_blur<E>(
    values: &mut [f32],
    scratch: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let diameter = radius.saturating_mul(2).saturating_add(1) as f32;
    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let mut sum = 0.0;
            for offset in 0..=radius.saturating_mul(2) {
                let source_x = x.saturating_add(offset).saturating_sub(radius).min(width - 1);
                sum += values[y * width + source_x];
            }
            scratch[y * width + x] = sum / diameter;
        }
    }
    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let mut sum = 0.0;
            for offset in 0..=radius.saturating_mul(2) {
                let source_y = y.saturating_add(offset).saturating_sub(radius).min(height - 1);
                sum += scratch[source_y * width + x];
            }
            values[y * width + x] = sum / diameter;
        }
    }
    Ok(())
}

fn separable_gaussian_blur<E>(
    values: &mut [f32],
    scratch: &mut [f32],
    width: usize,
    height: usize,
    sigma: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let radius = gaussian_kernel_radius(sigma) as usize;
    let weights = gaussian_weights(sigma, radius);
    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let mut sum = 0.0;
            for offset in -(radius as isize)..=radius as isize {
                let source_x = x.saturating_add_signed(offset).min(width - 1);
                sum += values[y * width + source_x] * weights[offset.unsigned_abs()];
            }
            scratch[y * width + x] = sum;
        }
    }
    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let mut sum = 0.0;
            for offset in -(radius as isize)..=radius as isize {
                let source_y = y.saturating_add_signed(offset).min(height - 1);
                sum += scratch[source_y * width + x] * weights[offset.unsigned_abs()];
            }
            values[y * width + x] = sum;
        }
    }
    Ok(())
}

fn gaussian_weights(sigma: f32, radius: usize) -> Vec<f32> {
    let sigma = sigma.max(1.0e-3);
    let mut weights = (0..=radius)
        .map(|offset| (-0.5 * (offset as f32 / sigma).powi(2)).exp())
        .collect::<Vec<_>>();
    let normalization = weights[0] + 2.0 * weights.iter().skip(1).sum::<f32>();
    for weight in &mut weights {
        *weight /= normalization;
    }
    weights
}

fn gaussian_kernel_radius(sigma: f32) -> u32 {
    (sigma.max(0.0) * 3.0).ceil() as u32
}

fn normalized_rgb_coordinate(rgb: [f32; 3]) -> [f32; 3] {
    let positive = rgb.map(|channel| channel.max(0.0));
    let peak = positive.into_iter().fold(0.0_f32, f32::max);
    let scale = 1.0 + peak;
    positive.map(|channel| channel / scale)
}

fn rgb_hue_saturation(rgb: [f32; 3]) -> (f32, f32) {
    let maximum = rgb.into_iter().fold(f32::NEG_INFINITY, f32::max);
    let minimum = rgb.into_iter().fold(f32::INFINITY, f32::min);
    let chroma = maximum - minimum;
    if chroma <= f32::EPSILON || maximum <= f32::EPSILON {
        return (0.0, 0.0);
    }
    let sector = if maximum == rgb[0] {
        positive_mod((rgb[1] - rgb[2]) / chroma, 6.0)
    } else if maximum == rgb[1] {
        (rgb[2] - rgb[0]) / chroma + 2.0
    } else {
        (rgb[0] - rgb[1]) / chroma + 4.0
    };
    (sector / 6.0, (chroma / maximum).clamp(0.0, 1.0))
}

fn range_matte(value: f32, low: f32, high: f32, softness: f32) -> f32 {
    smoothstep(low - softness, low, value) * (1.0 - smoothstep(high, high + softness, value))
}

fn circular_distance(left: f32, right: f32) -> f32 {
    let distance = (left - right).abs();
    distance.min(1.0 - distance)
}

fn tone_map_positive(value: f32) -> f32 {
    let value = value.max(0.0);
    value / (1.0 + value)
}

fn positive_mod(value: f32, modulus: f32) -> f32 {
    value - (value / modulus).floor() * modulus
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 {
        return f32::from(value >= edge1);
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{QualifierSample, QualifierSampleOperation};

    fn qualifier(mode: QualifierMode) -> PreparedQualifier {
        let samples = QualifierSampleSet::new(vec![
            QualifierSample::new([0.0, 1.0, 0.0], QualifierSampleOperation::Include),
            QualifierSample::new([1.0, 0.0, 0.0], QualifierSampleOperation::Exclude),
        ])
        .expect("samples");
        PreparedQualifier::new(
            QualifierAuthoring {
                mode,
                hue_center_degrees: 120.0,
                hue_width_degrees: 80.0,
                hue_softness_degrees: 20.0,
                saturation_low: 0.25,
                saturation_high: 1.0,
                saturation_softness: 0.1,
                luminance_low: 0.0,
                luminance_high: 1.0,
                luminance_softness: 0.0,
                samples: &samples,
                three_d_tolerance: 0.12,
                three_d_softness: 0.08,
                denoise_radius: 0,
                blur_radius: 0.0,
                clean_black: 0.0,
                clean_white: 0.0,
            },
            WorkingColorSpace::LinearRec709,
        )
        .expect("qualifier")
    }

    #[test]
    fn hsl_and_three_dimensional_selection_are_bounded_and_select_green() {
        for mode in [QualifierMode::Hsl, QualifierMode::ThreeDimensional] {
            let qualifier = qualifier(mode);
            assert!(qualifier.raw_matte([0.0, 1.0, 0.0]) > 0.95);
            assert!(qualifier.raw_matte([1.0, 0.0, 0.0]) < 0.05);
            assert!((0.0..=1.0).contains(&qualifier.raw_matte([-0.5, 4.0, 0.2])));
        }
    }

    #[test]
    fn refinement_outputs_canonical_alpha_mask_and_preview() {
        let qualifier = qualifier(QualifierMode::ThreeDimensional);
        let mut pixels = vec![[0.0, 1.0, 0.0, 0.25], [1.0, 0.0, 0.0, 0.75]];
        apply_qualifier_rgba_f32_controlled(
            &mut pixels,
            2,
            1,
            &qualifier,
            &mut || Ok::<(), ()>(()),
        )
        .expect("qualifier");
        assert_eq!(pixels[0][..3], [0.0; 3]);
        assert!(pixels[0][3] > 0.95);
        assert!(pixels[1][3] < 0.05);
        apply_matte_preview_rgba_f32_controlled(&mut pixels, false, &mut || Ok::<(), ()>(()))
            .expect("preview");
        assert_eq!(pixels[0][0], pixels[0][3]);
        assert_eq!(pixels[0][3], 1.0);
    }
}
