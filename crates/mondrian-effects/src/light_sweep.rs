//! Light sweep: a soft band of coloured light dragged across the frame.
//!
//! The effect is a pure point operation. Every output pixel depends on its own
//! coordinates and the authored settings only, so there is no halo, no scratch
//! plane, and no ordering constraint beyond the point-op chain.
//!
//! Geometry. The frame is measured in pixels, with pixel centres at
//! `(x + 0.5, y + 0.5)`. The band centre is `c = (centre.x * width,
//! centre.y * height)` and the band normal is `n = (sin θ, -cos θ)`, so the
//! signed distance from the band centre line is
//!
//! ```text
//! d = (p - c) . n
//! ```
//!
//! and `|d|` is the distance in pixels. At `direction = 0` the band runs
//! horizontally; at `direction = 90` it runs vertically. Positive directions
//! rotate clockwise in image coordinates, where `y` grows downward.
//!
//! Profile. `h = width / 2` is the half width and `e = edge_thickness` is the
//! softness of the band edge. The three documented shapes share the same
//! boundary but approach it differently:
//!
//! ```text
//! hard:   |d| <= h ? 1 : 1 - smoothstep(h, h + e, |d|)
//! linear: clamp((h + e - |d|) / max(2e, 1e-6), 0, 1)
//! soft:   1 - smoothstep(max(h - e, 0), h + e, |d|)
//! ```
//!
//! With `edge_thickness = 0` all three shapes collapse to a hard step at `h`.
//! The edge highlight is a Gaussian centred exactly on the band boundary,
//! `rim = exp(-((|d| - h) / sigma)^2)` with `sigma = max(e / 2, 0.5)`.
//!
//! Light. The band radiance is
//!
//! ```text
//! intensity = sweep_intensity / 100 * profile + edge_intensity / 100 * rim
//! light     = light_color * intensity
//! ```
//!
//! and `reception` chooses how the source receives it: `screen` brightens the
//! source toward the light, `add` adds the light, and `normal` covers the
//! source with `light_color` at the clamped band opacity. Values are not
//! clamped, so the maths stays valid in scene-linear HDR; the grading chain
//! above owns the final display clamp.
//!
//! Coverage is never invented: transparent pixels are returned untouched, and
//! the source alpha is preserved.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Documented band centre, normalized to the frame.
pub const LIGHT_SWEEP_DEFAULT_CENTRE: [f32; 2] = [0.5, 0.5];
/// Documented band direction in degrees.
pub const LIGHT_SWEEP_DEFAULT_DIRECTION: f32 = -30.0;
/// Documented band width in pixels.
pub const LIGHT_SWEEP_DEFAULT_WIDTH: f32 = 50.0;
/// Documented band intensity, in percent.
pub const LIGHT_SWEEP_DEFAULT_SWEEP_INTENSITY: f32 = 25.0;
/// Documented edge highlight intensity, in percent.
pub const LIGHT_SWEEP_DEFAULT_EDGE_INTENSITY: f32 = 50.0;
/// Documented edge thickness in pixels.
pub const LIGHT_SWEEP_DEFAULT_EDGE_THICKNESS: f32 = 4.0;
/// Documented light colour.
pub const LIGHT_SWEEP_DEFAULT_LIGHT_COLOR: [f32; 3] = [1.0, 0.98, 0.94];

/// Smallest accepted band width in pixels.
pub const LIGHT_SWEEP_MIN_WIDTH: f32 = 0.0;
/// Largest accepted band width in pixels.
pub const LIGHT_SWEEP_MAX_WIDTH: f32 = 4000.0;
/// Smallest accepted intensity, in percent.
pub const LIGHT_SWEEP_MIN_INTENSITY: f32 = 0.0;
/// Largest accepted intensity, in percent.
pub const LIGHT_SWEEP_MAX_INTENSITY: f32 = 500.0;
/// Largest accepted edge thickness in pixels.
pub const LIGHT_SWEEP_MAX_EDGE_THICKNESS: f32 = 20.0;

/// Smallest Gaussian sigma for the edge highlight, in pixels.
const MIN_RIM_SIGMA: f32 = 0.5;
/// Guard against a zero-width linear ramp.
const LINEAR_RAMP_EPSILON: f32 = 1e-6;

/// How the band edge falls off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightSweepShape {
    /// Flat core with a short falloff outside the edge.
    Hard,
    /// Linear ramp centred on the edge.
    Linear,
    /// Smooth ramp centred on the edge.
    Soft,
}

impl LightSweepShape {
    /// Every shape in display order.
    pub const ALL: [Self; 3] = [Self::Hard, Self::Linear, Self::Soft];

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Hard => "hard",
            Self::Linear => "linear",
            Self::Soft => "soft",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_key() == key)
    }

    /// Packed tag used by the point-op uniform.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Hard => 0,
            Self::Linear => 1,
            Self::Soft => 2,
        }
    }

    /// Chinese menu caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hard => "硬边",
            Self::Linear => "线性",
            Self::Soft => "柔和",
        }
    }
}

/// How the source receives the band light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightSweepReception {
    /// Screen: brightens the source toward the light.
    Screen,
    /// Add: adds the light to the source.
    Add,
    /// Normal: covers the source with the light colour.
    Normal,
}

impl LightSweepReception {
    /// Every reception in display order.
    pub const ALL: [Self; 3] = [Self::Screen, Self::Add, Self::Normal];

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Add => "add",
            Self::Normal => "normal",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_key() == key)
    }

    /// Packed tag used by the point-op uniform.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Screen => 0,
            Self::Add => 1,
            Self::Normal => 2,
        }
    }

    /// Chinese menu caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Screen => "滤色",
            Self::Add => "相加",
            Self::Normal => "覆盖",
        }
    }
}

/// One authored light sweep configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightSweepParameters {
    /// Band centre, normalized to the frame.
    pub centre: [f32; 2],
    /// Band direction in degrees.
    pub direction: f32,
    /// Band edge profile.
    pub shape: LightSweepShape,
    /// Band width in pixels.
    pub width: f32,
    /// Band intensity, in percent.
    pub sweep_intensity: f32,
    /// Edge highlight intensity, in percent.
    pub edge_intensity: f32,
    /// Edge thickness in pixels.
    pub edge_thickness: f32,
    /// Colour of the band light.
    pub light_color: [f32; 3],
    /// How the source receives the light.
    pub reception: LightSweepReception,
}

impl Default for LightSweepParameters {
    fn default() -> Self {
        Self {
            centre: LIGHT_SWEEP_DEFAULT_CENTRE,
            direction: LIGHT_SWEEP_DEFAULT_DIRECTION,
            shape: LightSweepShape::Soft,
            width: LIGHT_SWEEP_DEFAULT_WIDTH,
            sweep_intensity: LIGHT_SWEEP_DEFAULT_SWEEP_INTENSITY,
            edge_intensity: LIGHT_SWEEP_DEFAULT_EDGE_INTENSITY,
            edge_thickness: LIGHT_SWEEP_DEFAULT_EDGE_THICKNESS,
            light_color: LIGHT_SWEEP_DEFAULT_LIGHT_COLOR,
            reception: LightSweepReception::Screen,
        }
    }
}

/// Reports the first authored value that falls outside its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("light_sweep.{parameter} must be finite and within its documented range")]
pub struct LightSweepError {
    parameter: &'static str,
}

impl LightSweepError {
    /// Stable snake_case id of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// The six fused point-op uniform cells produced by
/// [`LightSweepGrade::gpu_controls`].
pub type LightSweepGpuControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated light sweep configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightSweepGrade {
    parameters: LightSweepParameters,
}

impl LightSweepGrade {
    /// Validates one authored configuration.
    pub fn new(parameters: LightSweepParameters) -> Result<Self, LightSweepError> {
        let numeric = [
            ("centre_x", parameters.centre[0], 0.0, 1.0),
            ("centre_y", parameters.centre[1], 0.0, 1.0),
            ("direction", parameters.direction, -180.0, 180.0),
            (
                "width",
                parameters.width,
                LIGHT_SWEEP_MIN_WIDTH,
                LIGHT_SWEEP_MAX_WIDTH,
            ),
            (
                "sweep_intensity",
                parameters.sweep_intensity,
                LIGHT_SWEEP_MIN_INTENSITY,
                LIGHT_SWEEP_MAX_INTENSITY,
            ),
            (
                "edge_intensity",
                parameters.edge_intensity,
                LIGHT_SWEEP_MIN_INTENSITY,
                LIGHT_SWEEP_MAX_INTENSITY,
            ),
            (
                "edge_thickness",
                parameters.edge_thickness,
                0.0,
                LIGHT_SWEEP_MAX_EDGE_THICKNESS,
            ),
        ];
        for (parameter, value, minimum, maximum) in numeric {
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(LightSweepError { parameter });
            }
        }
        for component in parameters.light_color {
            if !component.is_finite() || !(0.0..=1.0).contains(&component) {
                return Err(LightSweepError { parameter: "light_color" });
            }
        }
        Ok(Self { parameters })
    }

    /// Documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(LightSweepParameters::default())
            .expect("the documented light sweep defaults are in range")
    }

    /// The authored values.
    pub const fn parameters(&self) -> &LightSweepParameters {
        &self.parameters
    }

    /// Whether the configuration leaves every pixel untouched.
    pub const fn is_identity(&self) -> bool {
        self.parameters.sweep_intensity <= 0.0 && self.parameters.edge_intensity <= 0.0
    }

    /// Relative cost of one point pass.
    pub const fn cost(&self) -> u8 {
        1
    }

    /// Packs the grade into the fused point-op uniform cells.
    ///
    /// The cells are `[cos θ, sin θ, width, edge thickness]`,
    /// `[centre x, centre y, sweep gain, edge gain]`,
    /// `[light rgb, shape tag]`, `[reception tag, 0, 0, 0]`, and two unused
    /// cells. Rotation is resolved here so the point shader never evaluates a
    /// trigonometric function per pixel.
    pub fn gpu_controls(&self) -> LightSweepGpuControls {
        let angle = self.parameters.direction.to_radians();
        let params = [
            angle.cos(),
            angle.sin(),
            self.parameters.width,
            self.parameters.edge_thickness,
        ];
        let colour = [
            self.parameters.centre[0],
            self.parameters.centre[1],
            self.parameters.sweep_intensity / 100.0,
            self.parameters.edge_intensity / 100.0,
        ];
        let light = [
            self.parameters.light_color[0],
            self.parameters.light_color[1],
            self.parameters.light_color[2],
            f32::from(self.parameters.shape.as_u8()),
        ];
        let extra0 = [f32::from(self.parameters.reception.as_u8()), 0.0, 0.0, 0.0];
        (params, colour, light, extra0, [0.0; 4], [0.0; 4])
    }

    /// Feeds the whole configuration into a graph or plan fingerprint.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        for component in self.parameters.centre {
            component.to_bits().hash(state);
        }
        for value in [
            self.parameters.direction,
            self.parameters.width,
            self.parameters.sweep_intensity,
            self.parameters.edge_intensity,
            self.parameters.edge_thickness,
        ] {
            value.to_bits().hash(state);
        }
        for component in self.parameters.light_color {
            component.to_bits().hash(state);
        }
        self.parameters.shape.as_u8().hash(state);
        self.parameters.reception.as_u8().hash(state);
    }
}

/// Applies the light sweep to one tightly packed frame.
pub fn light_sweep_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &LightSweepGrade,
) -> bool {
    apply_light_sweep_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Cancellation-aware light sweep kernel over one tightly packed frame.
pub(crate) fn apply_light_sweep_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &LightSweepGrade,
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

    let parameters = &grade.parameters;
    let angle = parameters.direction.to_radians();
    let normal = [angle.sin(), -angle.cos()];
    let centre = [
        parameters.centre[0] * width as f32,
        parameters.centre[1] * height as f32,
    ];
    let half_width = parameters.width * 0.5;
    let edge = parameters.edge_thickness;
    let shape = parameters.shape;
    let sweep_gain = parameters.sweep_intensity / 100.0;
    let edge_gain = parameters.edge_intensity / 100.0;
    let light_color = parameters.light_color;
    let reception = parameters.reception;
    let rim_sigma = (edge * 0.5).max(MIN_RIM_SIGMA);
    let linear_scale = (edge + edge).max(LINEAR_RAMP_EPSILON);
    let smooth_inner = (half_width - edge).max(0.0);
    let smooth_outer = half_width + edge;
    let hard_inner = half_width;
    let hard_outer = half_width + edge;
    let has_edge_light = edge_gain > 0.0;

    crate::parallel::for_each_row_band(working, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            if !crate::coverage::has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
                continue;
            }
            let row = row_start + index / width;
            let column = index % width;
            let x = column as f32 + 0.5;
            let y = row as f32 + 0.5;
            let distance = ((x - centre[0]) * normal[0] + (y - centre[1]) * normal[1]).abs();

            let profile = match shape {
                LightSweepShape::Hard => {
                    if distance <= hard_inner {
                        1.0
                    } else if edge > 0.0 {
                        1.0 - smoothstep(hard_inner, hard_outer, distance)
                    } else {
                        0.0
                    }
                }
                LightSweepShape::Linear => {
                    ((half_width + edge - distance) / linear_scale).clamp(0.0, 1.0)
                }
                LightSweepShape::Soft => {
                    if edge > 0.0 {
                        1.0 - smoothstep(smooth_inner, smooth_outer, distance)
                    } else if distance <= half_width {
                        1.0
                    } else {
                        0.0
                    }
                }
            };

            let mut intensity = sweep_gain * profile;
            if has_edge_light {
                let offset = (distance - half_width) / rim_sigma;
                intensity += edge_gain * (-(offset * offset)).exp();
            }
            if intensity <= 0.0 {
                continue;
            }

            match reception {
                LightSweepReception::Screen => {
                    for channel in 0..3 {
                        let light = light_color[channel] * intensity;
                        pixel[channel] = 1.0 - (1.0 - pixel[channel]) * (1.0 - light);
                    }
                }
                LightSweepReception::Add => {
                    for channel in 0..3 {
                        pixel[channel] += light_color[channel] * intensity;
                    }
                }
                LightSweepReception::Normal => {
                    let coverage = intensity.clamp(0.0, 1.0);
                    for channel in 0..3 {
                        pixel[channel] =
                            pixel[channel] * (1.0 - coverage) + light_color[channel] * coverage;
                    }
                }
            }
        }
    });

    checkpoint()?;
    Ok(true)
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    // The fixtures start from a valid grade and then mutate the one parameter
    // under test, which reads clearer than a full literal.
    #![allow(clippy::field_reassign_with_default)]

    use super::*;

    fn valid_grade(parameters: LightSweepParameters) -> LightSweepGrade {
        LightSweepGrade::new(parameters).expect("valid light sweep grade")
    }

    fn filled(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.2, 0.4, 0.6, 1.0]; width * height]
    }

    fn pixel(frame: &[[f32; 4]], width: usize, column: usize, row: usize) -> &[f32; 4] {
        &frame[row * width + column]
    }

    #[test]
    fn documented_defaults_are_in_range() {
        let grade = LightSweepGrade::documented_defaults();
        assert_eq!(grade.parameters().width, LIGHT_SWEEP_DEFAULT_WIDTH);
        assert_eq!(grade.parameters().shape, LightSweepShape::Soft);
        assert_eq!(grade.parameters().reception, LightSweepReception::Screen);
        assert!(!grade.is_identity());
        assert_eq!(grade.cost(), 1);
    }

    #[test]
    fn direction_out_of_range_is_rejected() {
        let mut parameters = LightSweepParameters::default();
        parameters.direction = 181.0;
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "direction"
        );
        parameters.direction = f32::NAN;
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "direction"
        );
    }

    #[test]
    fn width_out_of_range_is_rejected() {
        let mut parameters = LightSweepParameters::default();
        parameters.width = LIGHT_SWEEP_MAX_WIDTH + 0.5;
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "width"
        );
        parameters.width = -1.0;
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "width"
        );
    }

    #[test]
    fn intensity_out_of_range_is_rejected() {
        let mut parameters = LightSweepParameters::default();
        parameters.sweep_intensity = 501.0;
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "sweep_intensity"
        );
        parameters.sweep_intensity = 25.0;
        parameters.edge_intensity = -0.5;
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "edge_intensity"
        );
    }

    #[test]
    fn edge_thickness_out_of_range_is_rejected() {
        let mut parameters = LightSweepParameters::default();
        parameters.edge_thickness = 20.5;
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "edge_thickness"
        );
    }

    #[test]
    fn light_color_out_of_range_is_rejected() {
        let mut parameters = LightSweepParameters::default();
        parameters.light_color = [1.0, 1.2, 0.5];
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "light_color"
        );
    }

    #[test]
    fn centre_out_of_range_is_rejected() {
        let mut parameters = LightSweepParameters::default();
        parameters.centre = [1.5, 0.5];
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "centre_x"
        );
        parameters.centre = [0.5, -0.1];
        assert_eq!(
            LightSweepGrade::new(parameters).unwrap_err().parameter(),
            "centre_y"
        );
    }

    #[test]
    fn zero_intensities_are_identity() {
        let mut parameters = LightSweepParameters::default();
        parameters.sweep_intensity = 0.0;
        parameters.edge_intensity = 0.0;
        let grade = valid_grade(parameters);
        assert!(grade.is_identity());
        let mut frame = filled(32, 24);
        let original = frame.clone();
        assert!(light_sweep_frame_f32(&mut frame, 32, 24, &grade));
        assert_eq!(frame, original);
    }

    #[test]
    fn hard_shape_has_a_flat_core_and_a_soft_outside_edge() {
        let mut parameters = LightSweepParameters::default();
        parameters.centre = [0.5, 0.5];
        parameters.direction = 0.0;
        parameters.shape = LightSweepShape::Hard;
        parameters.width = 20.0;
        parameters.edge_thickness = 0.0;
        parameters.sweep_intensity = 100.0;
        parameters.edge_intensity = 0.0;
        parameters.light_color = [1.0, 1.0, 1.0];
        parameters.reception = LightSweepReception::Add;
        let grade = valid_grade(parameters);
        let mut frame = filled(64, 64);
        assert!(light_sweep_frame_f32(&mut frame, 64, 64, &grade));
        // Core: rows 27..=37 are within ten pixels of the centre line.
        assert!((pixel(&frame, 64, 32, 27)[0] - 1.2).abs() < 1e-6);
        assert!((pixel(&frame, 64, 32, 37)[0] - 1.2).abs() < 1e-6);
        // Outside the half width nothing is added.
        assert!((pixel(&frame, 64, 32, 43)[0] - 0.2).abs() < 1e-6);

        parameters.edge_thickness = 4.0;
        let grade = valid_grade(parameters);
        let mut frame = filled(64, 64);
        assert!(light_sweep_frame_f32(&mut frame, 64, 64, &grade));
        let inside = pixel(&frame, 64, 32, 37)[0];
        let off_edge = pixel(&frame, 64, 32, 43)[0];
        let outside = pixel(&frame, 64, 32, 46)[0];
        assert!((inside - 1.2).abs() < 1e-6);
        assert!(off_edge < inside && off_edge > outside);
    }

    #[test]
    fn linear_shape_ramps_across_two_edge_thicknesses() {
        let mut parameters = LightSweepParameters::default();
        parameters.direction = 0.0;
        parameters.shape = LightSweepShape::Linear;
        parameters.width = 20.0;
        parameters.edge_thickness = 4.0;
        parameters.sweep_intensity = 100.0;
        parameters.edge_intensity = 0.0;
        parameters.light_color = [1.0, 1.0, 1.0];
        parameters.reception = LightSweepReception::Add;
        let grade = valid_grade(parameters);
        let mut frame = filled(64, 64);
        assert!(light_sweep_frame_f32(&mut frame, 64, 64, &grade));
        // Distance 7.5 px: profile (14 - 7.5) / 8 = 0.8125.
        let inside = pixel(&frame, 64, 32, 39)[0];
        // Distance 9.5 px: profile (14 - 9.5) / 8 = 0.5625.
        let edge = pixel(&frame, 64, 32, 41)[0];
        // Distance 13.5 px: profile (14 - 13.5) / 8 = 0.0625.
        let outside = pixel(&frame, 64, 32, 45)[0];
        assert!((inside - 1.0125).abs() < 1e-5);
        assert!((edge - 0.7625).abs() < 1e-5);
        assert!((outside - 0.2625).abs() < 1e-5);
        assert!(inside > edge && edge > outside);
    }

    #[test]
    fn soft_shape_is_one_at_the_inner_edge_and_zero_at_the_outer_edge() {
        let mut parameters = LightSweepParameters::default();
        parameters.direction = 0.0;
        parameters.shape = LightSweepShape::Soft;
        parameters.width = 20.0;
        parameters.edge_thickness = 4.0;
        parameters.sweep_intensity = 100.0;
        parameters.edge_intensity = 0.0;
        parameters.light_color = [1.0, 1.0, 1.0];
        parameters.reception = LightSweepReception::Add;
        let grade = valid_grade(parameters);
        let mut frame = filled(64, 64);
        assert!(light_sweep_frame_f32(&mut frame, 64, 64, &grade));
        // Row 37 centre is 5.5 px from the band centre: inside h - e = 6, so
        // the smooth profile is still saturated.
        let inner = pixel(&frame, 64, 32, 37)[0];
        // Row 39 centre is 7.5 px: halfway through the smoothstep.
        let middle = pixel(&frame, 64, 32, 39)[0];
        // Row 43 centre is 11.5 px: past h + e = 14 is still inside, row 46 is out.
        let outer = pixel(&frame, 64, 32, 46)[0];
        assert!((inner - 1.2).abs() < 1e-5);
        assert!(middle < inner && middle > 0.2);
        assert!((outer - 0.2).abs() < 1e-5);
    }

    #[test]
    fn edge_rim_peaks_on_the_band_boundary() {
        let mut parameters = LightSweepParameters::default();
        parameters.direction = 0.0;
        parameters.width = 20.0;
        parameters.edge_thickness = 4.0;
        parameters.sweep_intensity = 0.0;
        parameters.edge_intensity = 100.0;
        parameters.light_color = [1.0, 1.0, 1.0];
        parameters.reception = LightSweepReception::Add;
        let grade = valid_grade(parameters);
        let mut frame = filled(64, 64);
        assert!(light_sweep_frame_f32(&mut frame, 64, 64, &grade));
        // Row 41 centre is 9.5 px away: the rim peaks at the boundary (10 px),
        // so the Gaussian offset is -0.25 sigma.
        let boundary = pixel(&frame, 64, 32, 41)[0];
        let farther = pixel(&frame, 64, 32, 46)[0];
        assert!(boundary > farther);
        assert!((boundary - (0.2 + (-0.0625f32).exp())).abs() < 1e-5);
    }

    #[test]
    fn centre_moves_the_band() {
        let mut parameters = LightSweepParameters::default();
        parameters.centre = [0.25, 0.5];
        parameters.direction = 90.0;
        parameters.width = 10.0;
        parameters.edge_thickness = 0.0;
        parameters.sweep_intensity = 100.0;
        parameters.edge_intensity = 0.0;
        parameters.light_color = [1.0, 1.0, 1.0];
        parameters.reception = LightSweepReception::Add;
        let grade = valid_grade(parameters);
        let mut frame = filled(64, 64);
        assert!(light_sweep_frame_f32(&mut frame, 64, 64, &grade));
        let centre_column = pixel(&frame, 64, 16, 32)[0];
        let far_column = pixel(&frame, 64, 40, 32)[0];
        assert!((centre_column - 1.2).abs() < 1e-6);
        assert!((far_column - 0.2).abs() < 1e-6);
    }

    #[test]
    fn direction_rotates_the_band() {
        let mut parameters = LightSweepParameters::default();
        parameters.direction = 90.0;
        parameters.width = 10.0;
        parameters.edge_thickness = 0.0;
        parameters.sweep_intensity = 100.0;
        parameters.edge_intensity = 0.0;
        parameters.light_color = [1.0, 1.0, 1.0];
        parameters.reception = LightSweepReception::Add;
        let grade = valid_grade(parameters);
        let mut frame = filled(64, 64);
        assert!(light_sweep_frame_f32(&mut frame, 64, 64, &grade));
        // The band is now vertical: brightness is constant down a column and
        // varies across columns.
        assert!((pixel(&frame, 64, 32, 0)[0] - pixel(&frame, 64, 32, 63)[0]).abs() < 1e-6);
        assert!(pixel(&frame, 64, 32, 0)[0] > pixel(&frame, 64, 0, 0)[0]);
    }

    #[test]
    fn width_is_measured_in_pixels() {
        let mut parameters = LightSweepParameters::default();
        parameters.direction = 0.0;
        parameters.width = 10.0;
        parameters.edge_thickness = 0.0;
        parameters.sweep_intensity = 100.0;
        parameters.edge_intensity = 0.0;
        parameters.light_color = [1.0, 1.0, 1.0];
        parameters.reception = LightSweepReception::Add;
        let grade = valid_grade(parameters);

        let mut small = filled(100, 100);
        assert!(light_sweep_frame_f32(&mut small, 100, 100, &grade));
        let small_pixel = pixel(&small, 100, 50, 50)[0];

        parameters.width = 20.0;
        let doubled = valid_grade(parameters);
        let mut large = filled(200, 200);
        assert!(light_sweep_frame_f32(&mut large, 200, 200, &doubled));
        let large_pixel = pixel(&large, 200, 100, 100)[0];

        assert!((small_pixel - large_pixel).abs() < 1e-6);
        // A pixel one and a half band widths off centre stays dark in both.
        assert!((pixel(&small, 100, 50, 65)[0] - 0.2).abs() < 1e-6);
        assert!((pixel(&large, 200, 100, 130)[0] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn screen_add_and_normal_differ() {
        let mut parameters = LightSweepParameters::default();
        parameters.direction = 0.0;
        parameters.width = 64.0;
        parameters.edge_thickness = 0.0;
        parameters.sweep_intensity = 50.0;
        parameters.edge_intensity = 0.0;
        parameters.light_color = [0.6, 0.6, 0.6];
        let mut results = Vec::new();
        for reception in LightSweepReception::ALL {
            parameters.reception = reception;
            let grade = valid_grade(parameters);
            let mut frame = filled(64, 64);
            assert!(light_sweep_frame_f32(&mut frame, 64, 64, &grade));
            results.push(pixel(&frame, 64, 32, 32)[0]);
        }
        let [screen, add, normal] = [results[0], results[1], results[2]];
        assert!((screen - (1.0 - 0.8 * 0.7)).abs() < 1e-6);
        assert!((add - 0.5).abs() < 1e-6);
        assert!((normal - 0.4).abs() < 1e-6);
        assert!(screen != add && add != normal && screen != normal);
    }

    #[test]
    fn alpha_is_preserved() {
        let mut parameters = LightSweepParameters::default();
        parameters.width = 64.0;
        parameters.sweep_intensity = 100.0;
        let grade = valid_grade(parameters);
        let mut frame: Vec<[f32; 4]> = vec![[0.2, 0.4, 0.6, 0.5]; 16 * 16];
        assert!(light_sweep_frame_f32(&mut frame, 16, 16, &grade));
        for pixel in &frame {
            assert!((pixel[3] - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn transparent_pixels_are_untouched() {
        let grade = LightSweepGrade::documented_defaults();
        let mut frame: Vec<[f32; 4]> = vec![[0.1, 0.2, 0.3, 0.0]; 32 * 32];
        let original = frame.clone();
        assert!(light_sweep_frame_f32(&mut frame, 32, 32, &grade));
        assert_eq!(frame, original);
    }

    #[test]
    fn hash_state_is_stable_and_shape_sensitive() {
        let hash = |grade: &LightSweepGrade| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            grade.hash_state(&mut hasher);
            hasher.finish()
        };
        let base = LightSweepGrade::documented_defaults();
        assert_eq!(hash(&base), hash(&LightSweepGrade::documented_defaults()));

        let mut parameters = LightSweepParameters::default();
        parameters.shape = LightSweepShape::Hard;
        let other = valid_grade(parameters);
        assert_ne!(hash(&base), hash(&other));

        parameters.shape = LightSweepShape::Soft;
        parameters.reception = LightSweepReception::Normal;
        let third = valid_grade(parameters);
        assert_ne!(hash(&base), hash(&third));
    }

    #[test]
    fn gpu_controls_carry_the_axis_gains_and_tags() {
        let mut parameters = LightSweepParameters::default();
        parameters.centre = [0.25, 0.75];
        parameters.direction = 90.0;
        parameters.width = 120.0;
        parameters.edge_thickness = 6.0;
        parameters.sweep_intensity = 100.0;
        parameters.edge_intensity = 200.0;
        parameters.shape = LightSweepShape::Hard;
        parameters.reception = LightSweepReception::Add;
        let grade = valid_grade(parameters);
        let (params, colour, light, extra0, extra1, extra2) = grade.gpu_controls();
        assert!(params[0].abs() < 1e-6);
        assert!((params[1] - 1.0).abs() < 1e-6);
        assert!((params[2] - 120.0).abs() < 1e-6);
        assert!((params[3] - 6.0).abs() < 1e-6);
        assert!((colour[0] - 0.25).abs() < 1e-6);
        assert!((colour[1] - 0.75).abs() < 1e-6);
        assert!((colour[2] - 1.0).abs() < 1e-6);
        assert!((colour[3] - 2.0).abs() < 1e-6);
        assert!((light[3] - f32::from(LightSweepShape::Hard.as_u8())).abs() < 1e-6);
        assert!((extra0[0] - f32::from(LightSweepReception::Add.as_u8())).abs() < 1e-6);
        assert_eq!(extra1, [0.0; 4]);
        assert_eq!(extra2, [0.0; 4]);
    }

    #[test]
    fn non_zero_grade_reports_true_from_the_frame_entry() {
        let grade = LightSweepGrade::documented_defaults();
        let mut frame = filled(8, 8);
        assert!(light_sweep_frame_f32(&mut frame, 8, 8, &grade));
    }

    #[test]
    fn wrong_frame_length_is_rejected() {
        let grade = LightSweepGrade::documented_defaults();
        let mut frame = filled(4, 4);
        assert!(!light_sweep_frame_f32(&mut frame, 8, 8, &grade));
        assert!(!light_sweep_frame_f32(&mut frame, 0, 4, &grade));
    }

    #[test]
    fn checkpoint_is_honoured() {
        let grade = LightSweepGrade::documented_defaults();
        let mut frame = filled(16, 16);
        let mut calls = 0;
        let result = apply_light_sweep_f32_controlled(&mut frame, 16, 16, &grade, &mut || {
            calls += 1;
            if calls > 1 {
                Err("cancelled")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("cancelled"));
        assert_eq!(calls, 2);
    }

    #[test]
    fn menu_keys_round_trip() {
        for shape in LightSweepShape::ALL {
            assert_eq!(LightSweepShape::from_key(shape.as_key()), Some(shape));
        }
        for reception in LightSweepReception::ALL {
            assert_eq!(
                LightSweepReception::from_key(reception.as_key()),
                Some(reception)
            );
        }
        assert_eq!(LightSweepShape::from_key("unknown"), None);
        assert_eq!(LightSweepReception::from_key("unknown"), None);
    }
}
