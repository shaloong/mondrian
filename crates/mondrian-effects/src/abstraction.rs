//! Abstraction: a quantised redraw of a pre-blurred frame with an optional
//! edge overlay.
//!
//! The host effect this mirrors (`ResolveFX Abstraction`) exposes eleven
//! parameters. The kernel here keeps the same names and defaults, and defines
//! the arithmetic so the CPU and the GPU can share one description:
//!
//! 1. The frame is pre-blurred with a separable tent kernel. The author value
//!    `blur` is normalised to `[0, 1]` and becomes a radius of
//!    `round(blur * 8)` pixels, so the largest window is 17 taps per axis.
//!    Both the horizontal and the vertical pass accumulate premultiplied
//!    samples and normalise by the weight sum, which keeps coverage correct.
//! 2. Each colour channel is quantised to `bins` levels and mixed back toward
//!    its unquantised value by `quantize_control`; that step is repeated
//!    `iterations` times. Repeating it is idempotent once a channel sits on the
//!    grid, but the host exposes the count so an author can stop early.
//! 3. `smoothness` mixes the quantised colour back toward the blurred frame,
//!    and `softness` mixes toward it a second time. The two values are kept
//!    separate because the host treats them as independent controls: the first
//!    limits how much abstraction is applied, the second softens the result.
//! 4. When `draw_edges` is on, the kernel measures a four-neighbour luminance
//!    gradient on the blurred frame, thresholds it with `edge_threshold`,
//!    saturates at `edge_strength`, and multiplies by `edge_control` and
//!    `draw_edges` before darkening the colour. Alpha is never changed.
//!
//! Everything above is a neighbourhood read of at most `radius + 1` pixels, so
//! the CPU kernel materialises two full-frame planes (one for each blur pass)
//! and the GPU plan uses three spatial passes. `blur = 0`, quantisation off and
//! edges off is an exact identity, and authoring then emits no node at all.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Largest documented pre-blur radius, in pixels.
pub const ABSTRACTION_MAX_RADIUS: u32 = 8;
/// Smallest documented author value for `blur`.
pub const ABSTRACTION_MIN_BLUR: f32 = 0.0;
/// Largest documented author value for `blur`.
pub const ABSTRACTION_MAX_BLUR: f32 = 1.0;
/// Default author value for `blur` (`ResolveFX Abstraction` uses 0.1).
pub const ABSTRACTION_DEFAULT_BLUR: f32 = 0.1;
/// Default author value for `smoothness` (`ResolveFX Abstraction` uses 0.15).
pub const ABSTRACTION_DEFAULT_SMOOTHNESS: f32 = 0.15;
/// Smallest documented iteration count.
pub const ABSTRACTION_MIN_ITERATIONS: u32 = 1;
/// Largest documented iteration count.
pub const ABSTRACTION_MAX_ITERATIONS: u32 = 8;
/// Default iteration count (`ResolveFX Abstraction` uses 3).
pub const ABSTRACTION_DEFAULT_ITERATIONS: u32 = 3;
/// Smallest documented quantisation step count.
pub const ABSTRACTION_MIN_BINS: u32 = 2;
/// Largest documented quantisation step count.
pub const ABSTRACTION_MAX_BINS: u32 = 32;
/// Default quantisation step count (`ResolveFX Abstraction` uses 8).
pub const ABSTRACTION_DEFAULT_BINS: u32 = 8;
/// Default author value for `quantize_control` (`ResolveFX` uses 1).
pub const ABSTRACTION_DEFAULT_QUANTIZE_CONTROL: f32 = 1.0;
/// Default author value for `softness` (`ResolveFX Abstraction` uses 0.15).
pub const ABSTRACTION_DEFAULT_SOFTNESS: f32 = 0.15;
/// Default author value for `edge_control` (`ResolveFX` uses 1).
pub const ABSTRACTION_DEFAULT_EDGE_CONTROL: f32 = 1.0;
/// Default author value for `edge_strength` (`ResolveFX` uses 3).
pub const ABSTRACTION_DEFAULT_EDGE_STRENGTH: f32 = 3.0;
/// Largest documented edge strength.
pub const ABSTRACTION_MAX_EDGE_STRENGTH: f32 = 10.0;
/// Default author value for `edge_threshold` (`ResolveFX` uses 0.2).
pub const ABSTRACTION_DEFAULT_EDGE_THRESHOLD: f32 = 0.2;

/// Fraction of a step that separates the edge threshold from full saturation.
const EDGE_SOFTNESS: f32 = 0.1;

/// The uniform slots the fused spatial pass reads. The first cell carries
/// `[radius, bins, iterations, quantize_control]`, the second
/// `[smoothness, softness, quantize, edge_control]`, and the third
/// `[draw_edges, edge_strength, edge_threshold, 0]`.
pub type AbstractionGpuControls = ([f32; 4], [f32; 4], [f32; 4]);

/// The eleven author-facing controls of the abstraction effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbstractionParameters {
    /// Pre-blur amount, normalised to `[0, 1]`.
    pub blur: f32,
    /// How much of the quantised colour replaces the blurred colour.
    pub smoothness: f32,
    /// How many quantisation passes run.
    pub iterations: f32,
    /// How much each quantisation pass moves a channel.
    pub quantize_control: f32,
    /// Whether quantisation runs at all (values at or above 0.5 turn it on).
    pub quantize: f32,
    /// Number of steps each colour channel is quantised to.
    pub bins: f32,
    /// How much the blurred colour is mixed back in after quantisation.
    pub softness: f32,
    /// Opacity of the drawn edges.
    pub edge_control: f32,
    /// Whether edges are drawn at all (values at or above 0.5 turn it on).
    pub draw_edges: f32,
    /// How strongly the edges darken the colour.
    pub edge_strength: f32,
    /// Luminance gradient below which no edge is drawn.
    pub edge_threshold: f32,
}

impl Default for AbstractionParameters {
    fn default() -> Self {
        Self {
            blur: ABSTRACTION_DEFAULT_BLUR,
            smoothness: ABSTRACTION_DEFAULT_SMOOTHNESS,
            iterations: ABSTRACTION_DEFAULT_ITERATIONS as f32,
            quantize_control: ABSTRACTION_DEFAULT_QUANTIZE_CONTROL,
            quantize: 0.0,
            bins: ABSTRACTION_DEFAULT_BINS as f32,
            softness: ABSTRACTION_DEFAULT_SOFTNESS,
            edge_control: ABSTRACTION_DEFAULT_EDGE_CONTROL,
            draw_edges: 1.0,
            edge_strength: ABSTRACTION_DEFAULT_EDGE_STRENGTH,
            edge_threshold: ABSTRACTION_DEFAULT_EDGE_THRESHOLD,
        }
    }
}

/// Why an abstraction grade could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("abstraction.{parameter} must be finite and within its documented range")]
pub struct AbstractionError {
    parameter: &'static str,
}

impl AbstractionError {
    /// The author parameter that was rejected.
    pub const fn parameter(&self) -> &'static str {
        self.parameter
    }
}

/// A validated abstraction grade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbstractionGrade {
    parameters: AbstractionParameters,
}

impl AbstractionGrade {
    /// Validates every author value and records it.
    pub fn new(parameters: AbstractionParameters) -> Result<Self, AbstractionError> {
        let range = |parameter: &'static str, value: f32, low: f32, high: f32| {
            if !value.is_finite() || value < low || value > high {
                Err(AbstractionError { parameter })
            } else {
                Ok(())
            }
        };
        range(
            "blur",
            parameters.blur,
            ABSTRACTION_MIN_BLUR,
            ABSTRACTION_MAX_BLUR,
        )?;
        range("smoothness", parameters.smoothness, 0.0, 1.0)?;
        range(
            "iterations",
            parameters.iterations,
            ABSTRACTION_MIN_ITERATIONS as f32,
            ABSTRACTION_MAX_ITERATIONS as f32,
        )?;
        range("quantize_control", parameters.quantize_control, 0.0, 1.0)?;
        range("quantize", parameters.quantize, 0.0, 1.0)?;
        range(
            "bins",
            parameters.bins,
            ABSTRACTION_MIN_BINS as f32,
            ABSTRACTION_MAX_BINS as f32,
        )?;
        range("softness", parameters.softness, 0.0, 1.0)?;
        range("edge_control", parameters.edge_control, 0.0, 1.0)?;
        range("draw_edges", parameters.draw_edges, 0.0, 1.0)?;
        range(
            "edge_strength",
            parameters.edge_strength,
            0.0,
            ABSTRACTION_MAX_EDGE_STRENGTH,
        )?;
        range("edge_threshold", parameters.edge_threshold, 0.0, 1.0)?;
        Ok(Self { parameters })
    }

    /// The documented defaults, which always validate.
    pub fn documented_defaults() -> Self {
        Self { parameters: AbstractionParameters::default() }
    }

    /// The recorded author values.
    pub const fn parameters(&self) -> &AbstractionParameters {
        &self.parameters
    }

    /// Pre-blur radius in pixels.
    pub fn radius(&self) -> u32 {
        ((self.parameters.blur * ABSTRACTION_MAX_RADIUS as f32).round() as i64)
            .clamp(0, ABSTRACTION_MAX_RADIUS as i64) as u32
    }

    /// Quantisation step count.
    pub fn bins(&self) -> u32 {
        (self.parameters.bins.round() as i64)
            .clamp(ABSTRACTION_MIN_BINS as i64, ABSTRACTION_MAX_BINS as i64) as u32
    }

    /// Number of quantisation passes.
    pub fn iterations(&self) -> u32 {
        (self.parameters.iterations.round() as i64).clamp(
            ABSTRACTION_MIN_ITERATIONS as i64,
            ABSTRACTION_MAX_ITERATIONS as i64,
        ) as u32
    }

    /// Whether quantisation runs.
    pub fn quantizes(&self) -> bool {
        self.parameters.quantize >= 0.5
    }

    /// Whether edges are drawn.
    pub fn draws_edges(&self) -> bool {
        self.parameters.draw_edges >= 0.5
    }

    /// Whether the grade leaves the frame untouched.
    pub fn is_identity(&self) -> bool {
        self.radius() == 0
            && (!self.quantizes() || self.parameters.smoothness <= 0.0)
            && !self.draws_edges()
    }

    /// The halo the spatial chain must reserve around the frame.
    pub fn input_halo(&self) -> (u32, u32) {
        let halo = self.radius() + 1;
        (halo, halo)
    }

    /// The fused uniform cells for the three spatial passes.
    pub fn gpu_controls(&self) -> AbstractionGpuControls {
        (
            [
                self.radius() as f32,
                self.bins() as f32,
                self.iterations() as f32,
                self.parameters.quantize_control,
            ],
            [
                self.parameters.smoothness,
                self.parameters.softness,
                self.parameters.quantize,
                self.parameters.edge_control,
            ],
            [
                self.parameters.draw_edges,
                self.parameters.edge_strength,
                self.parameters.edge_threshold,
                0.0,
            ],
        )
    }

    /// Relative cost of the three spatial passes.
    pub fn cost(&self) -> u8 {
        3
    }

    /// Mixes the grade into an operation hash.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        let parameters = &self.parameters;
        for value in [
            parameters.blur,
            parameters.smoothness,
            parameters.iterations,
            parameters.quantize_control,
            parameters.quantize,
            parameters.bins,
            parameters.softness,
            parameters.edge_control,
            parameters.draw_edges,
            parameters.edge_strength,
            parameters.edge_threshold,
        ] {
            value.to_bits().hash(state);
        }
    }
}

/// Applies the abstraction effect to a straight RGBA frame.
///
/// Returns `false` when the frame is empty or shorter than its declared size,
/// and `true` otherwise. A grade that is an exact identity still returns
/// `true`.
pub fn abstraction_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &AbstractionGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_abstraction_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Checkpointed kernel shared by the scalar executor and the frame harness.
pub(crate) fn apply_abstraction_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &AbstractionGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || width == 0 || height == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    checkpoint()?;

    let radius = grade.radius() as usize;
    let mut horizontal = vec![[0.0f32; 4]; pixel_count];
    let mut vertical = vec![[0.0f32; 4]; pixel_count];
    blur_axis(
        working,
        &mut horizontal,
        width,
        height,
        radius,
        BlurAxis::Horizontal,
    );
    blur_axis(
        &horizontal,
        &mut vertical,
        width,
        height,
        radius,
        BlurAxis::Vertical,
    );

    let bins = grade.bins() as f32;
    let iterations = grade.iterations();
    let quantize = grade.quantizes();
    let quantize_control = grade.parameters.quantize_control.clamp(0.0, 1.0);
    let smoothness = grade.parameters.smoothness.clamp(0.0, 1.0);
    let softness = grade.parameters.softness.clamp(0.0, 1.0);
    let draw_edges = grade.draws_edges();
    let edge_control = grade.parameters.edge_control.clamp(0.0, 1.0);
    let edge_strength = grade.parameters.edge_strength.clamp(0.0, ABSTRACTION_MAX_EDGE_STRENGTH);
    let threshold = grade.parameters.edge_threshold.clamp(0.0, 1.0);
    let last_column = width as i64 - 1;
    let last_row = height as i64 - 1;

    crate::parallel::for_each_row_band(working, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let centre = vertical[row * width + column];
            if !crate::coverage::has_positive_coverage(centre[3].clamp(0.0, 1.0)) {
                *pixel = [0.0; 4];
                continue;
            }
            let mut result = [centre[0], centre[1], centre[2]];
            if quantize {
                let mut quantised = [
                    centre[0].clamp(0.0, 1.0),
                    centre[1].clamp(0.0, 1.0),
                    centre[2].clamp(0.0, 1.0),
                ];
                for _ in 0..iterations {
                    for channel in quantised.iter_mut() {
                        let snapped = quantize_channel(*channel, bins);
                        *channel += (snapped - *channel) * quantize_control;
                    }
                }
                for channel in 0..3 {
                    result[channel] += (quantised[channel] - result[channel]) * smoothness;
                    result[channel] += (centre[channel] - result[channel]) * softness;
                }
            }
            if draw_edges {
                let left =
                    vertical[row * width + (column as i64 - 1).clamp(0, last_column) as usize];
                let right =
                    vertical[row * width + (column as i64 + 1).clamp(0, last_column) as usize];
                let above = vertical[(row as i64 - 1).clamp(0, last_row) as usize * width + column];
                let below = vertical[(row as i64 + 1).clamp(0, last_row) as usize * width + column];
                let gradient_x = luma(right) - luma(left);
                let gradient_y = luma(below) - luma(above);
                let magnitude = gradient_x.abs() + gradient_y.abs();
                let mask = smoothstep(threshold, threshold + EDGE_SOFTNESS, magnitude);
                let darken = (edge_strength * mask).clamp(0.0, 1.0) * edge_control;
                for channel in result.iter_mut() {
                    *channel *= 1.0 - darken;
                }
            }
            pixel[0] = result[0];
            pixel[1] = result[1];
            pixel[2] = result[2];
            pixel[3] = centre[3];
        }
    });

    checkpoint()?;
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlurAxis {
    Horizontal,
    Vertical,
}

/// One separable tent pass: `w(d) = 1 / (1 + d²)` over `-radius..=radius`.
fn blur_axis(
    source: &[[f32; 4]],
    destination: &mut [[f32; 4]],
    width: usize,
    height: usize,
    radius: usize,
    axis: BlurAxis,
) {
    if radius == 0 {
        destination[..width * height].copy_from_slice(&source[..width * height]);
        return;
    }
    let last_column = width as i64 - 1;
    let last_row = height as i64 - 1;
    let reach = radius as i64;
    crate::parallel::for_each_row_band(destination, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let mut sum = [0.0f32; 4];
            let mut weight_sum = 0.0f32;
            for offset in -reach..=reach {
                let weight = 1.0 / (1.0 + (offset * offset) as f32);
                let sample_row = row as i64;
                let sample_column = column as i64;
                let (sample_row, sample_column) = match axis {
                    BlurAxis::Horizontal => {
                        (sample_row, (sample_column + offset).clamp(0, last_column))
                    }
                    BlurAxis::Vertical => ((sample_row + offset).clamp(0, last_row), sample_column),
                };
                let sample =
                    premultiply(source[sample_row as usize * width + sample_column as usize]);
                for channel in 0..4 {
                    sum[channel] += sample[channel] * weight;
                }
                weight_sum += weight;
            }
            *pixel = if weight_sum > 0.0 {
                unpremultiply([
                    sum[0] / weight_sum,
                    sum[1] / weight_sum,
                    sum[2] / weight_sum,
                    sum[3] / weight_sum,
                ])
            } else {
                [0.0; 4]
            };
        }
    });
}

fn quantize_channel(value: f32, bins: f32) -> f32 {
    ((value.clamp(0.0, 1.0) * bins + 0.5).floor() / bins).clamp(0.0, 1.0)
}

fn premultiply(sample: [f32; 4]) -> [f32; 4] {
    let alpha = sample[3].clamp(0.0, 1.0);
    [
        sample[0] * alpha,
        sample[1] * alpha,
        sample[2] * alpha,
        alpha,
    ]
}

fn unpremultiply(sample: [f32; 4]) -> [f32; 4] {
    let alpha = sample[3].clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    [
        sample[0] / alpha,
        sample[1] / alpha,
        sample[2] / alpha,
        alpha,
    ]
}

fn luma(sample: [f32; 4]) -> f32 {
    if sample[3].clamp(0.0, 1.0) <= 0.0 {
        return 0.0;
    }
    0.2126 * sample[0] + 0.7152 * sample[1] + 0.0722 * sample[2]
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_grade(parameters: AbstractionParameters) -> AbstractionGrade {
        AbstractionGrade::new(parameters).expect("the fixture grade must validate")
    }

    fn flat_frame(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        let defaults = AbstractionParameters::default();
        for mutate in [
            |p: &mut AbstractionParameters| p.blur = -0.01,
            |p: &mut AbstractionParameters| p.blur = 1.01,
            |p: &mut AbstractionParameters| p.blur = f32::NAN,
            |p: &mut AbstractionParameters| p.smoothness = 1.1,
            |p: &mut AbstractionParameters| p.iterations = 0.4,
            |p: &mut AbstractionParameters| p.iterations = 8.6,
            |p: &mut AbstractionParameters| p.quantize_control = -0.1,
            |p: &mut AbstractionParameters| p.quantize = 1.5,
            |p: &mut AbstractionParameters| p.bins = 1.4,
            |p: &mut AbstractionParameters| p.bins = 32.6,
            |p: &mut AbstractionParameters| p.softness = f32::INFINITY,
            |p: &mut AbstractionParameters| p.edge_control = 2.0,
            |p: &mut AbstractionParameters| p.draw_edges = -0.1,
            |p: &mut AbstractionParameters| p.edge_strength = 10.1,
            |p: &mut AbstractionParameters| p.edge_threshold = -0.5,
        ] {
            let mut parameters = defaults;
            mutate(&mut parameters);
            assert!(
                AbstractionGrade::new(parameters).is_err(),
                "{parameters:?} must not validate"
            );
        }
        assert!(AbstractionGrade::new(defaults).is_ok());
    }

    #[test]
    fn documented_defaults_stay_stable() {
        let grade = AbstractionGrade::documented_defaults();
        let parameters = grade.parameters();
        assert_eq!(parameters.blur, ABSTRACTION_DEFAULT_BLUR);
        assert_eq!(parameters.smoothness, ABSTRACTION_DEFAULT_SMOOTHNESS);
        assert_eq!(grade.iterations(), ABSTRACTION_DEFAULT_ITERATIONS);
        assert_eq!(grade.bins(), ABSTRACTION_DEFAULT_BINS);
        assert_eq!(parameters.softness, ABSTRACTION_DEFAULT_SOFTNESS);
        assert_eq!(parameters.edge_strength, ABSTRACTION_DEFAULT_EDGE_STRENGTH);
        assert_eq!(
            parameters.edge_threshold,
            ABSTRACTION_DEFAULT_EDGE_THRESHOLD
        );
        assert_eq!(grade.radius(), 1);
        assert_eq!(grade.input_halo(), (2, 2));
        assert_eq!(grade.cost(), 3);
        assert!(
            !grade.quantizes(),
            "the host default leaves quantisation off"
        );
        assert!(grade.draws_edges());
        assert!(!grade.is_identity());
    }

    #[test]
    fn radius_follows_the_normalised_blur() {
        let mut parameters = AbstractionParameters::default();
        for (blur, radius) in [(0.0, 0), (0.1, 1), (0.3, 2), (0.5, 4), (0.75, 6), (1.0, 8)] {
            parameters.blur = blur;
            assert_eq!(valid_grade(parameters).radius(), radius);
        }
    }

    #[test]
    fn zero_blur_quantisation_off_and_edges_off_is_an_exact_identity() {
        let parameters = AbstractionParameters {
            blur: 0.0,
            quantize: 0.0,
            draw_edges: 0.0,
            ..AbstractionParameters::default()
        };
        let grade = valid_grade(parameters);
        assert!(grade.is_identity());
        let original = vec![
            [0.25, 0.5, 0.75, 1.0],
            [0.1, 0.2, 0.3, 0.5],
            [0.0, 0.0, 0.0, 0.0],
            [0.9, 0.8, 0.7, 1.0],
        ];
        let mut frame = original.clone();
        assert!(abstraction_frame_f32(&mut frame, 2, 2, &grade));
        assert_eq!(frame, original);
    }

    #[test]
    fn flat_frames_stay_flat() {
        let grade = valid_grade(AbstractionParameters {
            quantize: 1.0,
            draw_edges: 1.0,
            edge_threshold: 0.0,
            ..AbstractionParameters::default()
        });
        let mut frame = flat_frame(5, 4, [0.3, 0.3, 0.3, 1.0]);
        assert!(abstraction_frame_f32(&mut frame, 5, 4, &grade));
        // A uniform frame stays uniform; the exact value is the blur, then the
        // quantisation grid, then the two mixes back toward the blurred colour.
        let first = frame[0];
        for pixel in &frame {
            assert_eq!(*pixel, first, "a flat frame must stay flat");
        }
        let quantised = 0.25;
        let abstracted = 0.3 + (quantised - 0.3) * 0.15;
        let softened = abstracted + (0.3 - abstracted) * 0.15;
        assert!((first[0] - softened).abs() <= 1e-6);
    }

    #[test]
    fn quantisation_snaps_channels_to_the_step_grid() {
        let grade = valid_grade(AbstractionParameters {
            blur: 0.0,
            bins: 2.0,
            quantize: 1.0,
            quantize_control: 1.0,
            smoothness: 1.0,
            softness: 0.0,
            draw_edges: 0.0,
            ..AbstractionParameters::default()
        });
        let mut frame = vec![
            [0.3, 0.3, 0.3, 1.0],
            [0.7, 0.7, 0.7, 1.0],
            [0.9, 0.9, 0.9, 1.0],
            [0.1, 0.1, 0.1, 1.0],
        ];
        assert!(abstraction_frame_f32(&mut frame, 2, 2, &grade));
        for pixel in &frame {
            for value in &pixel[..3] {
                assert!(*value == 0.0 || *value == 0.5 || *value == 1.0);
            }
        }
    }

    #[test]
    fn repeated_quantisation_converges() {
        let mut once = AbstractionParameters {
            blur: 0.0,
            bins: 3.0,
            quantize: 1.0,
            smoothness: 1.0,
            softness: 0.0,
            draw_edges: 0.0,
            iterations: 1.0,
            ..AbstractionParameters::default()
        };
        let mut twice = once;
        twice.iterations = 8.0;
        let mut frame_once = vec![[0.41, 0.12, 0.87, 1.0]; 1];
        let mut frame_twice = frame_once.clone();
        assert!(abstraction_frame_f32(
            &mut frame_once,
            1,
            1,
            &valid_grade(once)
        ));
        assert!(abstraction_frame_f32(
            &mut frame_twice,
            1,
            1,
            &valid_grade(twice)
        ));
        assert_eq!(frame_once, frame_twice);
        once.smoothness = 0.0;
        assert_eq!(valid_grade(once).bins(), 3);
    }

    #[test]
    fn smoothness_zero_keeps_the_blurred_frame() {
        let grade = valid_grade(AbstractionParameters {
            quantize: 1.0,
            smoothness: 0.0,
            softness: 0.5,
            draw_edges: 0.0,
            ..AbstractionParameters::default()
        });
        let mut frame: Vec<[f32; 4]> =
            (0..8).map(|index| [index as f32 / 8.0, 0.4, 0.6, 1.0]).collect();
        let blurred = {
            let mut copy = frame.clone();
            let mut harness = || Ok::<(), std::convert::Infallible>(());
            let mut plain = AbstractionParameters {
                quantize: 0.0,
                smoothness: 0.0,
                softness: 0.0,
                draw_edges: 0.0,
                ..AbstractionParameters::default()
            };
            plain.bins = 8.0;
            apply_abstraction_f32_controlled(&mut copy, 8, 1, &valid_grade(plain), &mut harness)
                .unwrap();
            copy
        };
        assert!(abstraction_frame_f32(&mut frame, 8, 1, &grade));
        for (actual, expected) in frame.iter().zip(blurred.iter()) {
            for channel in 0..4 {
                assert!((actual[channel] - expected[channel]).abs() <= 1e-6);
            }
        }
    }

    #[test]
    fn softness_one_returns_to_the_blurred_frame() {
        let soft = valid_grade(AbstractionParameters {
            quantize: 1.0,
            smoothness: 1.0,
            softness: 1.0,
            draw_edges: 0.0,
            ..AbstractionParameters::default()
        });
        let blurred = valid_grade(AbstractionParameters {
            quantize: 0.0,
            draw_edges: 0.0,
            ..AbstractionParameters::default()
        });
        let original: Vec<[f32; 4]> =
            (0..8).map(|index| [index as f32 / 8.0, 0.4, 0.6, 1.0]).collect();
        let mut with_softness = original.clone();
        let mut plain = original.clone();
        assert!(abstraction_frame_f32(&mut with_softness, 8, 1, &soft));
        assert!(abstraction_frame_f32(&mut plain, 8, 1, &blurred));
        for (actual, expected) in with_softness.iter().zip(plain.iter()) {
            for channel in 0..3 {
                assert!((actual[channel] - expected[channel]).abs() <= 1e-6);
            }
        }
    }

    #[test]
    fn edges_darken_a_step_boundary() {
        let grade = valid_grade(AbstractionParameters {
            blur: 0.0,
            quantize: 0.0,
            draw_edges: 1.0,
            edge_control: 1.0,
            edge_strength: 3.0,
            edge_threshold: 0.2,
            ..AbstractionParameters::default()
        });
        let mut frame = vec![
            [0.8, 0.8, 0.8, 1.0],
            [0.8, 0.8, 0.8, 1.0],
            [0.2, 0.2, 0.2, 1.0],
            [0.2, 0.2, 0.2, 1.0],
        ];
        assert!(abstraction_frame_f32(&mut frame, 4, 1, &grade));
        assert_eq!(frame[0][0], 0.8, "the flat left edge must stay unchanged");
        assert_eq!(frame[1][0], 0.0, "a full strength edge must clip to black");
        assert_eq!(frame[2][0], 0.0, "a full strength edge must clip to black");
        assert_eq!(frame[3][0], 0.2, "the flat right edge must stay unchanged");
    }

    #[test]
    fn a_high_threshold_disables_the_edge_overlay() {
        let parameters = AbstractionParameters {
            blur: 0.0,
            quantize: 0.0,
            draw_edges: 1.0,
            edge_threshold: 1.0,
            ..AbstractionParameters::default()
        };
        let grade = valid_grade(parameters);
        let original = vec![
            [1.0, 1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut frame = original.clone();
        assert!(abstraction_frame_f32(&mut frame, 4, 1, &grade));
        assert_eq!(frame, original, "a threshold of one masks every gradient");
    }

    #[test]
    fn transparent_pixels_are_left_alone() {
        let grade = valid_grade(AbstractionParameters {
            blur: 0.0,
            quantize: 1.0,
            draw_edges: 1.0,
            edge_threshold: 0.0,
            ..AbstractionParameters::default()
        });
        let mut frame = vec![
            [0.5, 0.5, 0.5, 0.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.5, 0.5, 0.5, 0.4],
            [0.5, 0.5, 0.5, 1.0],
        ];
        assert!(abstraction_frame_f32(&mut frame, 2, 2, &grade));
        assert_eq!(frame[0], [0.0; 4]);
        assert_eq!(frame[2][3], 0.4);
        assert_eq!(frame[1][3], 1.0);
    }

    #[test]
    fn short_and_empty_frames_are_refused() {
        let grade = AbstractionGrade::documented_defaults();
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!abstraction_frame_f32(&mut empty, 0, 0, &grade));
        let mut short = vec![[0.5, 0.5, 0.5, 1.0]; 3];
        assert!(!abstraction_frame_f32(&mut short, 4, 1, &grade));
        let mut single = vec![[0.5, 0.5, 0.5, 1.0]; 1];
        assert!(abstraction_frame_f32(&mut single, 1, 1, &grade));
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let grade = AbstractionGrade::documented_defaults();
        let original: Vec<[f32; 4]> = (0..35)
            .map(|index| {
                [
                    index as f32 / 35.0,
                    (index % 7) as f32 / 7.0,
                    (index % 5) as f32 / 5.0,
                    1.0,
                ]
            })
            .collect();
        let mut first = original.clone();
        let mut second = original;
        assert!(abstraction_frame_f32(&mut first, 7, 5, &grade));
        assert!(abstraction_frame_f32(&mut second, 7, 5, &grade));
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_is_reported() {
        let grade = AbstractionGrade::documented_defaults();
        let mut frame = vec![[0.5, 0.5, 0.5, 1.0]; 16];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            if calls == 2 {
                Err("cancelled")
            } else {
                Ok(())
            }
        };
        let outcome = apply_abstraction_f32_controlled(&mut frame, 4, 4, &grade, &mut checkpoint);
        assert_eq!(outcome.unwrap_err(), "cancelled");
    }

    #[test]
    fn the_hash_separates_distinct_configurations() {
        use std::collections::hash_map::DefaultHasher;
        let mut hashes = Vec::new();
        for mutate in [
            |p: &mut AbstractionParameters| p.blur = 0.5,
            |p: &mut AbstractionParameters| p.smoothness = 0.5,
            |p: &mut AbstractionParameters| p.bins = 16.0,
            |p: &mut AbstractionParameters| p.edge_threshold = 0.5,
            |p: &mut AbstractionParameters| p.quantize = 1.0,
        ] {
            let mut parameters = AbstractionParameters::default();
            mutate(&mut parameters);
            let grade = valid_grade(parameters);
            let mut hasher = DefaultHasher::new();
            grade.hash_state(&mut hasher);
            hashes.push(hasher.finish());
        }
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), 5);
    }

    #[test]
    fn packs_the_uniform_cells_for_the_spatial_passes() {
        let grade = valid_grade(AbstractionParameters {
            blur: 0.5,
            bins: 16.0,
            iterations: 4.0,
            quantize_control: 0.75,
            smoothness: 0.5,
            softness: 0.25,
            quantize: 1.0,
            edge_control: 0.5,
            draw_edges: 1.0,
            edge_strength: 2.0,
            edge_threshold: 0.3,
        });
        let controls = grade.gpu_controls();
        assert_eq!(controls.0, [4.0, 16.0, 4.0, 0.75]);
        assert_eq!(controls.1, [0.5, 0.25, 1.0, 0.5]);
        assert_eq!(controls.2, [1.0, 2.0, 0.3, 0.0]);
        assert_eq!(grade.radius(), 4);
        assert_eq!(grade.input_halo(), (5, 5));
    }
}
