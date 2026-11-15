//! Highlight extraction: bright samples are streaked back along the authored
//! direction and composited over the frame.
//!
//! The fourteen authored parameters follow the Fusion `Highlight` tool
//! (`AlphaScale`, `Low`, `MergeOver`, `Curve`, `NumberOfPoints`,
//! `RedScale`/`GreenScale`/`BlueScale`, `Angle`, `Length`, `ColorScale`,
//! `High`, `HighlightMask` and `HighlightMask_LayerSelect`). The mask inputs
//! are declared by the schema but this kernel has no way to read a matte, so
//! the authoring layer rejects a set `高光遮罩` before a node is built.
//!
//! The streak is a single spatial pass: for every output pixel the kernel
//! walks `points` positions along the authored direction, from one step to
//! `length` (a fraction of the frame width, converted to pixels), reads the
//! source bilinearly at each position, weights it by how far its luma sits
//! between `low` and `high` and by a distance falloff, and accumulates a
//! channel-scaled colour. The accumulated colour is averaged over the sample
//! count and either added to the base frame (`向上合并` on) or emitted on its
//! own with a coverage-derived alpha (`向上合并` off).
//!
//! The kernel reads only the source frame, so it needs no scratch plane. A
//! zero length or an invisible alpha scale with merge-over on is an exact
//! identity and the authoring layer builds no node for it.

use std::hash::{Hash, Hasher};

use thiserror::Error;

use crate::parallel::for_each_row_band;

/// Largest number of samples the streak may take.
pub const HIGHLIGHT_MAX_POINTS: u32 = 16;
/// Smallest number of samples the streak may take.
pub const HIGHLIGHT_MIN_POINTS: u32 = 1;
/// Default number of samples per streak.
pub const HIGHLIGHT_DEFAULT_POINTS: u32 = 4;
/// Largest per-channel and colour scale.
pub const HIGHLIGHT_MAX_SCALE: f32 = 2.0;
/// Default alpha multiplier added to the composited streak.
pub const HIGHLIGHT_DEFAULT_ALPHA_SCALE: f32 = 1.0;
/// Default lower threshold of the highlight luma window.
pub const HIGHLIGHT_DEFAULT_LOW: f32 = 0.9;
/// Default upper threshold of the highlight luma window.
pub const HIGHLIGHT_DEFAULT_HIGH: f32 = 1.0;
/// Default shaping exponent applied to the highlight weight.
pub const HIGHLIGHT_DEFAULT_CURVE: f32 = 1.0;
/// Smallest shaping exponent.
pub const HIGHLIGHT_MIN_CURVE: f32 = 0.1;
/// Largest shaping exponent.
pub const HIGHLIGHT_MAX_CURVE: f32 = 4.0;
/// Default streak length as a fraction of the frame width.
pub const HIGHLIGHT_DEFAULT_LENGTH: f32 = 0.8;
/// Default streak direction in degrees.
pub const HIGHLIGHT_DEFAULT_ANGLE_DEGREES: f32 = 0.0;
/// Default scale applied to all three channels at once.
pub const HIGHLIGHT_DEFAULT_COLOUR_SCALE: f32 = 1.0;

const PI: f32 = std::f32::consts::PI;
const MIN_WINDOW: f32 = 1.0e-4;
const DISTANCE_FALLOFF: f32 = 0.5;

/// Authoring parameters of the highlight extraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HighlightParameters {
    /// Multiplier applied to the accumulated streak before compositing.
    pub alpha_scale: f32,
    /// Lower end of the luma window that counts as a highlight.
    pub low: f32,
    /// Upper end of the luma window that counts as a highlight.
    pub high: f32,
    /// `1` merges the streak over the input, `0` outputs the streak alone.
    pub merge_over: f32,
    /// Exponent shaping the highlight weight.
    pub curve: f32,
    /// Number of samples taken along the streak.
    pub points: f32,
    /// Per-channel red multiplier.
    pub red_scale: f32,
    /// Per-channel green multiplier.
    pub green_scale: f32,
    /// Per-channel blue multiplier.
    pub blue_scale: f32,
    /// Streak direction in degrees.
    pub angle_degrees: f32,
    /// Streak length as a fraction of the frame width.
    pub length: f32,
    /// Colour multiplier applied to all three channels.
    pub colour_scale: f32,
}

impl Default for HighlightParameters {
    fn default() -> Self {
        Self {
            alpha_scale: HIGHLIGHT_DEFAULT_ALPHA_SCALE,
            low: HIGHLIGHT_DEFAULT_LOW,
            high: HIGHLIGHT_DEFAULT_HIGH,
            merge_over: 1.0,
            curve: HIGHLIGHT_DEFAULT_CURVE,
            points: HIGHLIGHT_DEFAULT_POINTS as f32,
            red_scale: 1.0,
            green_scale: 1.0,
            blue_scale: 1.0,
            angle_degrees: HIGHLIGHT_DEFAULT_ANGLE_DEGREES,
            length: HIGHLIGHT_DEFAULT_LENGTH,
            colour_scale: HIGHLIGHT_DEFAULT_COLOUR_SCALE,
        }
    }
}

/// A highlight parameter outside its documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("highlight.{parameter} must be finite and within its documented range")]
pub struct HighlightError {
    parameter: &'static str,
}

impl HighlightError {
    /// The parameter that failed validation.
    pub const fn parameter(&self) -> &'static str {
        self.parameter
    }

    const fn new(parameter: &'static str) -> Self {
        Self { parameter }
    }
}

/// A validated highlight extraction configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HighlightGrade {
    parameters: HighlightParameters,
    points: u32,
}

impl HighlightGrade {
    /// Validates the authored parameters.
    pub fn new(parameters: HighlightParameters) -> Result<Self, HighlightError> {
        finite(&parameters, "alpha_scale", 0.0, 1.0)?;
        finite(&parameters, "low", 0.0, 1.0)?;
        finite(&parameters, "high", 0.0, 1.0)?;
        finite(&parameters, "merge_over", 0.0, 1.0)?;
        finite(
            &parameters,
            "curve",
            HIGHLIGHT_MIN_CURVE,
            HIGHLIGHT_MAX_CURVE,
        )?;
        finite(
            &parameters,
            "points",
            HIGHLIGHT_MIN_POINTS as f32,
            HIGHLIGHT_MAX_POINTS as f32,
        )?;
        finite(&parameters, "red_scale", 0.0, HIGHLIGHT_MAX_SCALE)?;
        finite(&parameters, "green_scale", 0.0, HIGHLIGHT_MAX_SCALE)?;
        finite(&parameters, "blue_scale", 0.0, HIGHLIGHT_MAX_SCALE)?;
        finite(&parameters, "angle", -180.0, 180.0)?;
        finite(&parameters, "length", 0.0, 1.0)?;
        finite(&parameters, "colour_scale", 0.0, HIGHLIGHT_MAX_SCALE)?;
        let points = parameters.points.round() as u32;
        Ok(Self { parameters, points })
    }

    /// The documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(HighlightParameters::default())
            .expect("documented highlight defaults stay inside their ranges")
    }

    /// The validated authoring parameters.
    pub const fn parameters(&self) -> &HighlightParameters {
        &self.parameters
    }

    /// Number of samples taken along the streak.
    pub const fn points(&self) -> u32 {
        self.points
    }

    /// `true` when the streak is invisible and merging leaves the frame alone.
    pub fn is_identity(&self) -> bool {
        self.parameters.merge_over >= 0.5 && self.parameters.alpha_scale <= 0.0
    }

    /// Streak length in whole pixels for a frame of `width` pixels.
    pub fn length_pixels(&self, width: usize) -> u32 {
        let length = f64::from(self.parameters.length) * width as f64;
        length.round().clamp(0.0, width as f64) as u32
    }

    /// The spatial halo the streak may read beyond the frame edge.
    pub fn input_halo(&self, width: usize) -> (u32, u32) {
        let length = self.length_pixels(width);
        (length + 1, length + 1)
    }

    /// Relative cost used by the spatial chain planner.
    pub const fn cost(&self) -> u8 {
        4
    }

    /// Packs the uniform cells consumed by the point shader.
    pub fn gpu_controls(&self, width: usize) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        let parameters = &self.parameters;
        let radians = parameters.angle_degrees * PI / 180.0;
        let params = [
            parameters.alpha_scale,
            parameters.low,
            parameters.high,
            self.length_pixels(width) as f32,
        ];
        let extra = [
            radians.cos(),
            radians.sin(),
            parameters.curve,
            parameters.merge_over,
        ];
        let aces = [
            parameters.red_scale,
            parameters.green_scale,
            parameters.blue_scale,
            parameters.colour_scale,
        ];
        let extra1 = [self.points as f32, 0.0, 0.0, 0.0];
        (params, extra, aces, extra1)
    }

    /// Hashes the parameters that shape the streak.
    #[allow(dead_code)] // Consumed by the render-op wiring; kept here with the grade.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        for value in [
            self.parameters.alpha_scale,
            self.parameters.low,
            self.parameters.high,
            self.parameters.merge_over,
            self.parameters.curve,
            self.parameters.points,
            self.parameters.red_scale,
            self.parameters.green_scale,
            self.parameters.blue_scale,
            self.parameters.angle_degrees,
            self.parameters.length,
            self.parameters.colour_scale,
        ] {
            value.to_bits().hash(state);
        }
    }
}

fn finite(
    parameters: &HighlightParameters,
    parameter: &'static str,
    minimum: f32,
    maximum: f32,
) -> Result<(), HighlightError> {
    let value = match parameter {
        "alpha_scale" => parameters.alpha_scale,
        "low" => parameters.low,
        "high" => parameters.high,
        "merge_over" => parameters.merge_over,
        "curve" => parameters.curve,
        "points" => parameters.points,
        "red_scale" => parameters.red_scale,
        "green_scale" => parameters.green_scale,
        "blue_scale" => parameters.blue_scale,
        "angle" => parameters.angle_degrees,
        "length" => parameters.length,
        "colour_scale" => parameters.colour_scale,
        _ => f32::NAN,
    };
    if value.is_finite() && value >= minimum && value <= maximum {
        Ok(())
    } else {
        Err(HighlightError::new(parameter))
    }
}

/// Streaks the highlights of a straight-RGBA float frame in place.
pub fn highlight_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &HighlightGrade,
) -> bool {
    let result: Result<bool, std::convert::Infallible> =
        apply_highlight_f32_controlled(pixels, width, height, grade, &mut || Ok(()));
    result.expect("the infallible checkpoint never fails")
}

pub(crate) fn apply_highlight_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &HighlightGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    checkpoint()?;

    let points = grade.points().max(1);
    let length = grade.length_pixels(width) as f32;
    let parameters = grade.parameters();
    let radians = parameters.angle_degrees * PI / 180.0;
    let direction = [radians.cos(), radians.sin()];
    let low = parameters.low;
    let high = parameters.high.max(low + MIN_WINDOW);
    let curve = parameters.curve;
    let scale = [
        parameters.red_scale * parameters.colour_scale,
        parameters.green_scale * parameters.colour_scale,
        parameters.blue_scale * parameters.colour_scale,
    ];
    let merge = parameters.merge_over >= 0.5;
    let alpha_scale = parameters.alpha_scale;

    let source = working.to_vec();

    for_each_row_band(working, width, 0..height, |row_start, band| {
        for (index, destination) in band.iter_mut().enumerate() {
            let column = index % width;
            let row = row_start + index / width;
            let base = source[row * width + column];
            if base[3] <= 0.0 {
                *destination = [0.0; 4];
                continue;
            }
            let mut accumulated = [0.0_f32; 3];
            let mut coverage = 0.0_f32;
            for sample in 1..=points {
                let t = sample as f32 / points as f32;
                let distance = length * t;
                // The sampler works in pixel coordinates, matching the GPU
                // textureLoad below, so the ray offset is added to the centre
                // of the pixel rather than to a normalised coordinate.
                let x = column as f32 + 0.5 + direction[0] * distance;
                let y = row as f32 + 0.5 + direction[1] * distance;
                let read = sample_bilinear(&source, width, height, x, y);
                let luma = 0.2126 * read[0] + 0.7152 * read[1] + 0.0722 * read[2];
                let highlight = smoothstep(low, high, luma);
                let shaped = highlight.powf(curve);
                let falloff = 1.0 - DISTANCE_FALLOFF * t;
                let weight = shaped * falloff;
                for channel in 0..3 {
                    accumulated[channel] += read[channel] * weight * scale[channel];
                }
                coverage = coverage.max(weight);
            }
            let average = 1.0 / points as f32;
            if merge {
                for channel in 0..3 {
                    destination[channel] =
                        base[channel] + accumulated[channel] * average * alpha_scale;
                }
                destination[3] = base[3];
            } else {
                let opacity = (coverage * alpha_scale).clamp(0.0, 1.0);
                for channel in 0..3 {
                    destination[channel] = accumulated[channel] * average * alpha_scale;
                }
                destination[3] = base[3] * opacity;
            }
        }
    });

    checkpoint()?;
    Ok(true)
}

fn sample_bilinear(source: &[[f32; 4]], width: usize, height: usize, x: f32, y: f32) -> [f32; 3] {
    let last_x = width.saturating_sub(1) as f32;
    let last_y = height.saturating_sub(1) as f32;
    let x = x.clamp(0.0, last_x);
    let y = y.clamp(0.0, last_y);
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let x1 = (x0 + 1.0).min(last_x);
    let y1 = (y0 + 1.0).min(last_y);
    let left = x0 as usize;
    let right = x1 as usize;
    let top = y0 as usize;
    let bottom = y1 as usize;
    let mut out = [0.0_f32; 3];
    for channel in 0..3 {
        let top_left = source[top * width + left][channel];
        let top_right = source[top * width + right][channel];
        let bottom_left = source[bottom * width + left][channel];
        let bottom_right = source[bottom * width + right][channel];
        let upper = top_left + (top_right - top_left) * fx;
        let lower = bottom_left + (bottom_right - bottom_left) * fx;
        out[channel] = upper + (lower - upper) * fy;
    }
    out
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(parameters: HighlightParameters) -> HighlightGrade {
        HighlightGrade::new(parameters).expect("the parameters stay inside their ranges")
    }

    fn flat(width: usize, height: usize, colour: [f32; 3]) -> Vec<[f32; 4]> {
        vec![[colour[0], colour[1], colour[2], 1.0]; width * height]
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        let oversized_alpha = HighlightParameters { alpha_scale: 1.5, ..Default::default() };
        assert_eq!(
            HighlightGrade::new(oversized_alpha).unwrap_err().parameter(),
            "alpha_scale"
        );
        for points in [0.0, 17.0] {
            let bad_points = HighlightParameters { points, ..Default::default() };
            assert_eq!(
                HighlightGrade::new(bad_points).unwrap_err().parameter(),
                "points"
            );
        }
        let bad_curve = HighlightParameters { curve: f32::NAN, ..Default::default() };
        assert_eq!(
            HighlightGrade::new(bad_curve).unwrap_err().parameter(),
            "curve"
        );
        let bad_angle = HighlightParameters { angle_degrees: -181.0, ..Default::default() };
        assert_eq!(
            HighlightGrade::new(bad_angle).unwrap_err().parameter(),
            "angle"
        );
        let oversized_length = HighlightParameters { length: 1.5, ..Default::default() };
        assert_eq!(
            HighlightGrade::new(oversized_length).unwrap_err().parameter(),
            "length"
        );
        assert!(HighlightGrade::new(HighlightParameters::default()).is_ok());
    }

    #[test]
    fn documented_defaults_stay_stable() {
        let grade = HighlightGrade::documented_defaults();
        assert_eq!(grade.points(), 4);
        assert!((grade.parameters().length - 0.8).abs() <= 1.0e-6);
        assert_eq!(grade.length_pixels(1000), 800);
        assert_eq!(grade.input_halo(1000), (801, 801));
        assert_eq!(grade.cost(), 4);
        assert!(!grade.is_identity());
        assert!(HighlightGrade::new(HighlightParameters {
            alpha_scale: 0.0,
            ..Default::default()
        })
        .expect("zero alpha is in range")
        .is_identity());
    }

    #[test]
    fn packs_the_uniform_cells_for_the_spatial_shader() {
        let grade = grade(HighlightParameters {
            alpha_scale: 0.5,
            low: 0.25,
            high: 0.75,
            points: 8.0,
            angle_degrees: 90.0,
            length: 0.5,
            red_scale: 0.25,
            green_scale: 0.5,
            blue_scale: 2.0,
            colour_scale: 0.5,
            curve: 2.0,
            merge_over: 0.0,
        });
        let (params, extra, aces, extra1) = grade.gpu_controls(200);
        assert_eq!(params, [0.5, 0.25, 0.75, 100.0]);
        assert!((extra[0]).abs() <= 1.0e-6);
        assert!((extra[1] - 1.0).abs() <= 1.0e-6);
        assert_eq!(extra[2], 2.0);
        assert_eq!(extra[3], 0.0);
        assert_eq!(aces, [0.25, 0.5, 2.0, 0.5]);
        assert_eq!(extra1[0], 8.0);
    }

    #[test]
    fn zero_length_is_an_identity() {
        let grade = grade(HighlightParameters { length: 0.0, ..Default::default() });
        assert_eq!(grade.length_pixels(1920), 0);
        let mut frame = flat(4, 4, [0.8, 0.2, 0.1]);
        let before = frame.clone();
        assert!(highlight_frame_f32(&mut frame, 4, 4, &grade));
        assert_eq!(frame, before);
    }

    #[test]
    fn zero_alpha_merge_over_is_an_identity() {
        let grade = grade(HighlightParameters { alpha_scale: 0.0, ..Default::default() });
        let mut frame = flat(5, 3, [0.4, 0.5, 0.6]);
        let before = frame.clone();
        assert!(highlight_frame_f32(&mut frame, 5, 3, &grade));
        assert_eq!(frame, before);
    }

    #[test]
    fn a_dark_frame_gains_no_streak() {
        let grade = grade(HighlightParameters { length: 0.9, ..Default::default() });
        let mut frame = flat(6, 4, [0.2, 0.2, 0.2]);
        let before = frame.clone();
        assert!(highlight_frame_f32(&mut frame, 6, 4, &grade));
        for (after, before) in frame.iter().zip(before.iter()) {
            for channel in 0..3 {
                assert!(after[channel] <= before[channel] + 1.0e-6);
            }
        }
    }

    #[test]
    fn a_bright_spot_streaks_opposite_the_sample_direction() {
        let mut frame = flat(9, 1, [0.05, 0.05, 0.05]);
        frame[6] = [1.0, 1.0, 1.0, 1.0];
        let grade = grade(HighlightParameters {
            length: 0.6,
            low: 0.5,
            high: 1.0,
            points: 6.0,
            alpha_scale: 1.0,
            ..Default::default()
        });
        let before = frame.clone();
        assert!(highlight_frame_f32(&mut frame, 9, 1, &grade));
        // The samples run forward from every pixel, so the bright pixel lights
        // the pixels whose ray reaches it and stays untouched itself.
        for index in 1..6 {
            assert!(
                frame[index][0] > before[index][0],
                "pixel {index} should gain light: {} vs {}",
                frame[index][0],
                before[index][0]
            );
        }
        for index in 6..9 {
            assert!((frame[index][0] - before[index][0]).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn the_angle_rotates_the_streak() {
        let mut horizontal = flat(9, 9, [0.0, 0.0, 0.0]);
        horizontal[4 * 9 + 4] = [1.0, 1.0, 1.0, 1.0];
        let mut vertical = horizontal.clone();
        let along_x = grade(HighlightParameters {
            length: 0.5,
            low: 0.5,
            high: 1.0,
            points: 4.0,
            angle_degrees: 0.0,
            ..Default::default()
        });
        let along_y = grade(HighlightParameters { angle_degrees: 90.0, ..*along_x.parameters() });
        assert!(highlight_frame_f32(&mut horizontal, 9, 9, &along_x));
        assert!(highlight_frame_f32(&mut vertical, 9, 9, &along_y));
        assert!((horizontal[4 * 9 + 3][0] - vertical[3 * 9 + 4][0]).abs() <= 1.0e-5);
        assert!((horizontal[4 * 9 + 2][0] - vertical[2 * 9 + 4][0]).abs() <= 1.0e-5);
    }

    #[test]
    fn higher_alpha_scale_brightens_more() {
        let mut weak_frame = flat(7, 1, [0.1, 0.1, 0.1]);
        weak_frame[3] = [1.0, 1.0, 1.0, 1.0];
        let mut strong_frame = weak_frame.clone();
        let weak = grade(HighlightParameters {
            alpha_scale: 0.25,
            length: 0.5,
            low: 0.4,
            high: 1.0,
            ..Default::default()
        });
        let strong = grade(HighlightParameters { alpha_scale: 1.0, ..*weak.parameters() });
        assert!(highlight_frame_f32(&mut weak_frame, 7, 1, &weak));
        assert!(highlight_frame_f32(&mut strong_frame, 7, 1, &strong));
        assert!(strong_frame[1][0] > weak_frame[1][0]);
    }

    #[test]
    fn the_channel_scales_tint_the_streak() {
        let mut red_frame = flat(7, 1, [0.0, 0.0, 0.0]);
        red_frame[5] = [1.0, 1.0, 1.0, 1.0];
        let mut green_frame = red_frame.clone();
        let red = grade(HighlightParameters {
            red_scale: 1.0,
            green_scale: 0.0,
            blue_scale: 0.0,
            length: 0.5,
            // The bilinear read of a one-pixel spot lands at half intensity, so
            // the window has to start below it for the sample to contribute.
            low: 0.2,
            high: 1.0,
            ..Default::default()
        });
        let green = grade(HighlightParameters {
            red_scale: 0.0,
            green_scale: 1.0,
            blue_scale: 0.0,
            ..*red.parameters()
        });
        assert!(highlight_frame_f32(&mut red_frame, 7, 1, &red));
        assert!(highlight_frame_f32(&mut green_frame, 7, 1, &green));
        assert!(red_frame[3][0] > 0.0);
        assert!(red_frame[3][1] <= 1.0e-6);
        assert!(green_frame[3][1] > 0.0);
        assert!(green_frame[3][0] <= 1.0e-6);
    }

    #[test]
    fn merge_off_drops_the_base_colour() {
        let mut merged = vec![[0.3, 0.3, 0.3, 1.0]; 7];
        merged[5] = [1.0, 1.0, 1.0, 1.0];
        let mut isolated = merged.clone();
        let merge = grade(HighlightParameters { merge_over: 1.0, ..Default::default() });
        let alone = grade(HighlightParameters { merge_over: 0.0, ..*merge.parameters() });
        assert!(highlight_frame_f32(&mut merged, 7, 1, &merge));
        assert!(highlight_frame_f32(&mut isolated, 7, 1, &alone));
        assert!(merged[3][0] >= 0.3);
        assert!(isolated[3][0] < 0.3);
        assert!(isolated[3][3] <= 1.0);
    }

    #[test]
    fn a_higher_threshold_leaves_more_of_the_frame_alone() {
        let mut low_frame = flat(8, 1, [0.2, 0.2, 0.2]);
        low_frame[4] = [0.6, 0.6, 0.6, 1.0];
        let mut high_frame = low_frame.clone();
        let low = grade(HighlightParameters {
            low: 0.3,
            high: 0.7,
            length: 0.5,
            ..Default::default()
        });
        let high = grade(HighlightParameters { low: 0.8, high: 1.0, ..*low.parameters() });
        assert!(highlight_frame_f32(&mut low_frame, 8, 1, &low));
        assert!(highlight_frame_f32(&mut high_frame, 8, 1, &high));
        let low_gain: f32 = low_frame[2][0] - 0.2;
        let high_gain: f32 = high_frame[2][0] - 0.2;
        assert!(low_gain > 0.0);
        assert!(low_gain > high_gain);
    }

    #[test]
    fn transparent_pixels_stay_transparent() {
        let mut frame = flat(6, 2, [0.0, 0.0, 0.0]);
        for pixel in frame.iter_mut().take(3) {
            pixel[3] = 0.0;
        }
        frame[9] = [1.0, 1.0, 1.0, 1.0];
        let grade = grade(HighlightParameters { length: 0.8, ..Default::default() });
        assert!(highlight_frame_f32(&mut frame, 6, 2, &grade));
        for pixel in frame.iter().take(3) {
            assert_eq!(*pixel, [0.0; 4]);
        }
    }

    #[test]
    fn short_and_empty_frames_are_refused() {
        let grade = grade(HighlightParameters::default());
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!highlight_frame_f32(&mut empty, 0, 0, &grade));
        let mut short = vec![[0.0; 4]; 3];
        assert!(!highlight_frame_f32(&mut short, 4, 4, &grade));
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let mut first = flat(9, 5, [0.05, 0.1, 0.2]);
        first[2 * 9 + 2] = [1.0, 0.9, 0.8, 1.0];
        let mut second = first.clone();
        let grade = grade(HighlightParameters {
            length: 0.7,
            low: 0.4,
            high: 0.95,
            points: 5.0,
            ..Default::default()
        });
        assert!(highlight_frame_f32(&mut first, 9, 5, &grade));
        assert!(highlight_frame_f32(&mut second, 9, 5, &grade));
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_is_reported() {
        let mut frame = flat(4, 4, [0.5, 0.5, 0.5]);
        let grade = grade(HighlightParameters::default());
        let mut calls = 0;
        let result: Result<bool, &'static str> =
            apply_highlight_f32_controlled(&mut frame, 4, 4, &grade, &mut || {
                calls += 1;
                if calls == 2 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            });
        assert_eq!(result, Err("cancelled"));
    }

    #[test]
    fn the_hash_separates_distinct_configurations() {
        use std::hash::Hasher;
        let hash = |parameters| {
            let mut state = std::collections::hash_map::DefaultHasher::new();
            grade(parameters).hash_state(&mut state);
            state.finish()
        };
        let base = HighlightParameters::default();
        assert_ne!(
            hash(base),
            hash(HighlightParameters { angle_degrees: 45.0, ..base })
        );
        assert_ne!(
            hash(base),
            hash(HighlightParameters { points: 8.0, ..base })
        );
        assert_ne!(
            hash(base),
            hash(HighlightParameters { colour_scale: 0.5, ..base })
        );
    }
}
