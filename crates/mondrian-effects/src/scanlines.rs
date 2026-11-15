//! Scanlines and old-television styling.
//!
//! The effect draws a periodic band pattern over the frame and composites it
//! with the source. It is a pure point operation: every output pixel depends on
//! its own coordinates and the authored settings only, so there is no halo, no
//! scratch plane, and no ordering constraint beyond the point-op chain.
//!
//! Geometry. The band coordinate is
//!
//! ```text
//! u = (x + 0.5) / width          (normalized across the frame)
//! v = (y + 0.5) / height
//! s = v * cos(angle) + u * sin(angle)
//! phase = s * frequency + shift + channel_offset
//! centred = phase - floor(phase)
//! ```
//!
//! `frequency` counts bands per frame height, `angle` rotates them, and
//! `shift` slides the pattern. Each colour channel carries its own phase
//! offset, which is what produces the fringed, mis-converged CRT look.
//!
//! Band profile. `t = min(centred / width, 1)` is the normalized distance from
//! a band centre to its edge, so `width` is the fraction of each period the
//! band covers. The profile
//!
//! ```text
//! soften = 0.5 + (0.02 - 0.5) * sharpness
//! profile = 1 - smoothstep(1 - soften, 1, t)
//! ```
//!
//! is one at the band centre and fades to zero at the edge; `sharpness` moves
//! the transition from half the band to a two-percent edge.
//!
//! Colour. A band pixel takes `color1` at `profile = 0` and `color2` at
//! `profile = 1`, so the documented white/black pair paints dark bands over a
//! white field. `colour` mixes that pair toward its Rec.709 luminance, and
//! `appearance` scales the pattern toward unity (no pattern): the scan colour
//! reaching the composite is `1 + (scan - 1) * appearance`.
//!
//! Composite. `composite_type` combines the source with the scan colour, and
//! `composite` mixes that result back over the source. `lines_only` bypasses
//! the composite and shows the scan colour itself. Values are not clamped, so
//! the maths stays valid in scene-linear HDR; the grading chain above owns the
//! final display clamp.
//!
//! Coverage is never invented: transparent pixels are returned untouched, and
//! `lines_only` keeps the source alpha.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Documented overall strength.
pub const SCANLINES_DEFAULT_APPEARANCE: f32 = 1.0;
/// Documented band count per frame height.
pub const SCANLINES_DEFAULT_FREQUENCY: f32 = 5.0;
/// Documented band edge sharpness.
pub const SCANLINES_DEFAULT_SHARPNESS: f32 = 1.0;
/// Documented rotation of the band direction.
pub const SCANLINES_DEFAULT_ANGLE_DEGREES: f32 = 0.0;
/// Documented band width as a fraction of the period.
pub const SCANLINES_DEFAULT_LINE_WIDTH: f32 = 0.5;
/// Documented phase slide.
pub const SCANLINES_DEFAULT_SHIFT: f32 = 0.0;
/// Documented chroma retention of the band colours.
pub const SCANLINES_DEFAULT_COLOUR: f32 = 1.0;
/// Documented colour at `profile = 0`.
pub const SCANLINES_DEFAULT_COLOR1: [f32; 3] = [1.0, 1.0, 1.0];
/// Documented colour at `profile = 1`.
pub const SCANLINES_DEFAULT_COLOR2: [f32; 3] = [0.0, 0.0, 0.0];
/// Documented composite strength.
pub const SCANLINES_DEFAULT_COMPOSITE: f32 = 1.0;
/// Documented numeric form of the lines-only switch.
pub const SCANLINES_DEFAULT_LINES_ONLY: f32 = 0.0;

/// Smallest accepted band count per frame height.
pub const SCANLINES_MIN_FREQUENCY: f32 = 0.5;
/// Largest accepted band count per frame height.
pub const SCANLINES_MAX_FREQUENCY: f32 = 100.0;
/// Smallest accepted band width fraction.
pub const SCANLINES_MIN_LINE_WIDTH: f32 = 0.01;
/// Largest accepted band width fraction.
pub const SCANLINES_MAX_LINE_WIDTH: f32 = 1.0;

const MIN_SOFTEN: f32 = 0.02;
const MAX_SOFTEN: f32 = 0.5;
const REC709_RED: f32 = 0.2126;
const REC709_GREEN: f32 = 0.7152;
const REC709_BLUE: f32 = 0.0722;

/// How the band pattern is combined with the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanlinesComposite {
    /// Overlay: darkens darks and brightens brights.
    Overlay,
    /// Screen: brightens toward the pattern.
    Screen,
    /// Add: adds the pattern to the source.
    Add,
    /// Multiply: multiplies the source by the pattern.
    Multiply,
    /// Normal: replaces the source with the pattern.
    Normal,
}

impl ScanlinesComposite {
    /// Every composite in display order.
    pub const ALL: [Self; 5] = [
        Self::Overlay,
        Self::Screen,
        Self::Add,
        Self::Multiply,
        Self::Normal,
    ];

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Overlay => "overlay",
            Self::Screen => "screen",
            Self::Add => "add",
            Self::Multiply => "multiply",
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
            Self::Overlay => 0,
            Self::Screen => 1,
            Self::Add => 2,
            Self::Multiply => 3,
            Self::Normal => 4,
        }
    }

    /// Chinese menu caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overlay => "叠加",
            Self::Screen => "滤色",
            Self::Add => "相加",
            Self::Multiply => "相乘",
            Self::Normal => "正常",
        }
    }
}

/// One authored scanlines configuration.
///
/// The fields mirror the host parameter list one for one, including the
/// per-channel colour components, so authoring code never renames or regroups
/// values on the way in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanlinesParameters {
    /// Overall pattern strength.
    pub appearance: f32,
    /// Bands per frame height.
    pub frequency: f32,
    /// Band edge sharpness.
    pub sharpness: f32,
    /// Band direction rotation in degrees.
    pub angle_degrees: f32,
    /// Band width as a fraction of the period.
    pub line_width: f32,
    /// Phase slide in periods.
    pub shift: f32,
    /// Chroma retention of the band colours.
    pub colour: f32,
    /// Colour at `profile = 0`.
    pub color1: [f32; 3],
    /// Colour at `profile = 1`.
    pub color2: [f32; 3],
    /// Red channel phase offset.
    pub red_offset: f32,
    /// Green channel phase offset.
    pub green_offset: f32,
    /// Blue channel phase offset.
    pub blue_offset: f32,
    /// Composite strength.
    pub composite: f32,
    /// Numeric form of the lines-only switch; at or above one half is on.
    pub lines_only: f32,
    /// Composite operator.
    pub composite_type: ScanlinesComposite,
}

impl Default for ScanlinesParameters {
    fn default() -> Self {
        Self {
            appearance: SCANLINES_DEFAULT_APPEARANCE,
            frequency: SCANLINES_DEFAULT_FREQUENCY,
            sharpness: SCANLINES_DEFAULT_SHARPNESS,
            angle_degrees: SCANLINES_DEFAULT_ANGLE_DEGREES,
            line_width: SCANLINES_DEFAULT_LINE_WIDTH,
            shift: SCANLINES_DEFAULT_SHIFT,
            colour: SCANLINES_DEFAULT_COLOUR,
            color1: SCANLINES_DEFAULT_COLOR1,
            color2: SCANLINES_DEFAULT_COLOR2,
            red_offset: 0.0,
            green_offset: 0.0,
            blue_offset: 0.0,
            composite: SCANLINES_DEFAULT_COMPOSITE,
            lines_only: SCANLINES_DEFAULT_LINES_ONLY,
            composite_type: ScanlinesComposite::Overlay,
        }
    }
}

/// Reports the first authored value that falls outside its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("scanlines.{parameter} must be finite and within its documented range")]
pub struct ScanlinesError {
    parameter: &'static str,
}

impl ScanlinesError {
    /// Stable snake_case id of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// The six fused point-op uniform cells produced by
/// [`ScanlinesGrade::gpu_controls`]: the four equal `params` lanes, the
/// colour cell, and four extra cells.
pub type ScanlinesGpuControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated scanlines configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanlinesGrade {
    parameters: ScanlinesParameters,
    lines_only: bool,
}

impl ScanlinesGrade {
    /// Validates one authored configuration.
    pub fn new(parameters: ScanlinesParameters) -> Result<Self, ScanlinesError> {
        let numeric = [
            ("appearance", parameters.appearance, 0.0, 1.0),
            (
                "frequency",
                parameters.frequency,
                SCANLINES_MIN_FREQUENCY,
                SCANLINES_MAX_FREQUENCY,
            ),
            ("sharpness", parameters.sharpness, 0.0, 1.0),
            ("angle", parameters.angle_degrees, -180.0, 180.0),
            (
                "line_width",
                parameters.line_width,
                SCANLINES_MIN_LINE_WIDTH,
                SCANLINES_MAX_LINE_WIDTH,
            ),
            ("shift", parameters.shift, -1.0, 1.0),
            ("colour", parameters.colour, 0.0, 1.0),
            ("red_offset", parameters.red_offset, -1.0, 1.0),
            ("green_offset", parameters.green_offset, -1.0, 1.0),
            ("blue_offset", parameters.blue_offset, -1.0, 1.0),
            ("composite", parameters.composite, 0.0, 1.0),
            ("lines_only", parameters.lines_only, 0.0, 1.0),
        ];
        for (parameter, value, minimum, maximum) in numeric {
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(ScanlinesError { parameter });
            }
        }
        for (parameter, components) in
            [("color1", parameters.color1), ("color2", parameters.color2)]
        {
            for component in components {
                if !component.is_finite() || !(0.0..=1.0).contains(&component) {
                    return Err(ScanlinesError { parameter });
                }
            }
        }
        Ok(Self {
            parameters,
            lines_only: parameters.lines_only >= 0.5,
        })
    }

    /// Documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(ScanlinesParameters::default())
            .expect("the documented scanlines defaults are in range")
    }

    /// The authored values.
    pub const fn parameters(&self) -> &ScanlinesParameters {
        &self.parameters
    }

    /// Whether the lines-only switch is on.
    pub const fn lines_only(&self) -> bool {
        self.lines_only
    }

    /// Whether the configuration leaves every pixel untouched.
    pub fn is_identity(&self) -> bool {
        !self.lines_only && (self.parameters.appearance <= 0.0 || self.parameters.composite <= 0.0)
    }

    /// Relative cost of one point pass.
    pub const fn cost(&self) -> u8 {
        1
    }

    /// Packs the grade into the fused point-op uniform cells.
    ///
    /// The cells are `[appearance, frequency, sharpness, width]`,
    /// `[shift, cos(angle), sin(angle), colour]`, `[color1 rgb, composite]`,
    /// `[color2 rgb, lines_only]`,
    /// `[red offset, green offset, blue offset, composite type]`, and an
    /// unused cell. Rotation is resolved here so the point shader never
    /// evaluates a trigonometric function per pixel.
    pub fn gpu_controls(&self) -> ScanlinesGpuControls {
        let angle = self.parameters.angle_degrees.to_radians();
        let params = [
            self.parameters.appearance,
            self.parameters.frequency,
            self.parameters.sharpness,
            self.parameters.line_width,
        ];
        let color = [
            self.parameters.shift,
            angle.cos(),
            angle.sin(),
            self.parameters.colour,
        ];
        let extra0 = [
            self.parameters.color1[0],
            self.parameters.color1[1],
            self.parameters.color1[2],
            self.parameters.composite,
        ];
        let extra1 = [
            self.parameters.color2[0],
            self.parameters.color2[1],
            self.parameters.color2[2],
            f32::from(self.lines_only),
        ];
        let extra2 = [
            self.parameters.red_offset,
            self.parameters.green_offset,
            self.parameters.blue_offset,
            f32::from(self.parameters.composite_type.as_u8()),
        ];
        (params, color, extra0, extra1, extra2, [0.0; 4])
    }

    /// Feeds the whole configuration into a graph or plan fingerprint.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        for value in [
            self.parameters.appearance,
            self.parameters.frequency,
            self.parameters.sharpness,
            self.parameters.angle_degrees,
            self.parameters.line_width,
            self.parameters.shift,
            self.parameters.colour,
        ] {
            value.to_bits().hash(state);
        }
        for component in self.parameters.color1.into_iter().chain(self.parameters.color2) {
            component.to_bits().hash(state);
        }
        for value in [
            self.parameters.red_offset,
            self.parameters.green_offset,
            self.parameters.blue_offset,
            self.parameters.composite,
            self.parameters.lines_only,
        ] {
            value.to_bits().hash(state);
        }
        self.parameters.composite_type.as_u8().hash(state);
        self.lines_only.hash(state);
    }
}

/// Applies scanlines to one tightly packed frame.
pub fn scanlines_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ScanlinesGrade,
) -> bool {
    apply_scanlines_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Cancellation-aware scanlines kernel over one tightly packed frame.
pub(crate) fn apply_scanlines_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ScanlinesGrade,
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
    let angle = parameters.angle_degrees.to_radians();
    let cos_angle = angle.cos();
    let sin_angle = angle.sin();
    let frequency = parameters.frequency;
    let line_width = parameters.line_width;
    let shift = parameters.shift;
    let soften = MAX_SOFTEN + (MIN_SOFTEN - MAX_SOFTEN) * parameters.sharpness;
    let soften_start = 1.0 - soften;
    let appearance = parameters.appearance;
    let colour = parameters.colour;
    let composite_strength = parameters.composite;
    let color1 = parameters.color1;
    let color2 = parameters.color2;
    let offsets = [
        parameters.red_offset,
        parameters.green_offset,
        parameters.blue_offset,
    ];
    let composite_type = parameters.composite_type;
    let lines_only = grade.lines_only;
    let inverse_width = 1.0 / width as f32;
    let inverse_height = 1.0 / height as f32;

    crate::parallel::for_each_row_band(working, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let v = row as f32 + 0.5;
            let v = v * inverse_height;
            {
                if !crate::coverage::has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
                    continue;
                }
                let u = column as f32 + 0.5;
                let along = v * cos_angle + u * inverse_width * sin_angle;
                let phase_base = along * frequency + shift;
                let mut scan = [0.0_f32; 3];
                for channel in 0..3 {
                    let phase = phase_base + offsets[channel];
                    let centred = phase - phase.floor();
                    let t = (centred / line_width).min(1.0);
                    let profile = 1.0 - smoothstep(soften_start, 1.0, t);
                    scan[channel] = color1[channel] + (color2[channel] - color1[channel]) * profile;
                }
                let luma = REC709_RED * scan[0] + REC709_GREEN * scan[1] + REC709_BLUE * scan[2];
                for channel in scan.iter_mut() {
                    *channel = luma + (*channel - luma) * colour;
                }
                if lines_only {
                    pixel[0] = scan[0];
                    pixel[1] = scan[1];
                    pixel[2] = scan[2];
                    continue;
                }
                for channel in 0..3 {
                    let pattern = 1.0 + (scan[channel] - 1.0) * appearance;
                    let composited = composite_channel(pixel[channel], pattern, composite_type);
                    pixel[channel] =
                        pixel[channel] + (composited - pixel[channel]) * composite_strength;
                }
            }
        }
    });

    checkpoint()?;
    Ok(true)
}

fn composite_channel(base: f32, pattern: f32, composite: ScanlinesComposite) -> f32 {
    match composite {
        ScanlinesComposite::Overlay => {
            if base < 0.5 {
                2.0 * base * pattern
            } else {
                1.0 - 2.0 * (1.0 - base) * (1.0 - pattern)
            }
        }
        ScanlinesComposite::Screen => 1.0 - (1.0 - base) * (1.0 - pattern),
        ScanlinesComposite::Add => base + pattern,
        ScanlinesComposite::Multiply => base * pattern,
        ScanlinesComposite::Normal => pattern,
    }
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_grade(parameters: ScanlinesParameters) -> ScanlinesGrade {
        ScanlinesGrade::new(parameters).expect("valid scanlines grade")
    }

    fn flat_frame(width: usize, height: usize, value: [f32; 4]) -> Vec<[f32; 4]> {
        vec![value; width * height]
    }

    fn luma(pixel: [f32; 4]) -> f32 {
        REC709_RED * pixel[0] + REC709_GREEN * pixel[1] + REC709_BLUE * pixel[2]
    }

    fn spread(values: &[f32]) -> f32 {
        values.iter().cloned().fold(f32::MIN, f32::max)
            - values.iter().cloned().fold(f32::MAX, f32::min)
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        let mut parameters = ScanlinesParameters::default();
        parameters.appearance = 1.5;
        assert_eq!(
            ScanlinesGrade::new(parameters).unwrap_err().parameter(),
            "appearance"
        );

        let mut parameters = ScanlinesParameters::default();
        parameters.frequency = 0.25;
        assert_eq!(
            ScanlinesGrade::new(parameters).unwrap_err().parameter(),
            "frequency"
        );

        let mut parameters = ScanlinesParameters::default();
        parameters.line_width = 0.0;
        assert_eq!(
            ScanlinesGrade::new(parameters).unwrap_err().parameter(),
            "line_width"
        );

        let mut parameters = ScanlinesParameters::default();
        parameters.angle_degrees = f32::NAN;
        assert_eq!(
            ScanlinesGrade::new(parameters).unwrap_err().parameter(),
            "angle"
        );

        let mut parameters = ScanlinesParameters::default();
        parameters.red_offset = -2.0;
        assert_eq!(
            ScanlinesGrade::new(parameters).unwrap_err().parameter(),
            "red_offset"
        );

        let mut parameters = ScanlinesParameters::default();
        parameters.color2 = [0.0, 1.5, 0.0];
        assert_eq!(
            ScanlinesGrade::new(parameters).unwrap_err().parameter(),
            "color2"
        );

        let mut parameters = ScanlinesParameters::default();
        parameters.frequency = SCANLINES_MIN_FREQUENCY;
        parameters.line_width = SCANLINES_MAX_LINE_WIDTH;
        parameters.angle_degrees = -180.0;
        parameters.lines_only = 1.0;
        assert!(ScanlinesGrade::new(parameters).is_ok());
    }

    #[test]
    fn documented_defaults_stay_stable() {
        let defaults = ScanlinesGrade::documented_defaults();
        let parameters = defaults.parameters();
        assert_eq!(parameters.appearance, 1.0);
        assert_eq!(parameters.frequency, 5.0);
        assert_eq!(parameters.sharpness, 1.0);
        assert_eq!(parameters.angle_degrees, 0.0);
        assert_eq!(parameters.line_width, 0.5);
        assert_eq!(parameters.shift, 0.0);
        assert_eq!(parameters.colour, 1.0);
        assert_eq!(parameters.color1, [1.0, 1.0, 1.0]);
        assert_eq!(parameters.color2, [0.0, 0.0, 0.0]);
        assert_eq!(parameters.composite, 1.0);
        assert_eq!(parameters.lines_only, 0.0);
        assert_eq!(parameters.composite_type, ScanlinesComposite::Overlay);
        assert!(!defaults.lines_only());
        assert!(!defaults.is_identity());
        assert_eq!(defaults.cost(), 1);
    }

    #[test]
    fn packs_the_uniform_cells_for_the_point_shader() {
        let defaults = ScanlinesGrade::documented_defaults();
        let (params, color, extra0, extra1, extra2, extra3) = defaults.gpu_controls();
        assert_eq!(params, [1.0, 5.0, 1.0, 0.5]);
        assert_eq!(color[0], 0.0);
        assert!((color[1] - 1.0).abs() <= 1e-6);
        assert!(color[2].abs() <= 1e-6);
        assert_eq!(color[3], 1.0);
        assert_eq!(extra0, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(extra1, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(extra2, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(extra3, [0.0; 4]);

        let mut parameters = ScanlinesParameters::default();
        parameters.angle_degrees = 90.0;
        parameters.lines_only = 1.0;
        parameters.red_offset = 0.25;
        parameters.composite_type = ScanlinesComposite::Screen;
        let grade = valid_grade(parameters);
        let (_, color, _, extra1, extra2, _) = grade.gpu_controls();
        assert!(color[1].abs() <= 1e-6);
        assert!((color[2] - 1.0).abs() <= 1e-6);
        assert_eq!(extra1[3], 1.0);
        assert_eq!(extra2, [0.25, 0.0, 0.0, 1.0]);
        assert!(grade.lines_only());
    }

    #[test]
    fn horizontal_bands_vary_down_a_column_only() {
        let grade = valid_grade(ScanlinesParameters::default());
        let mut frame = flat_frame(6, 20, [0.5, 0.5, 0.5, 1.0]);
        assert!(scanlines_frame_f32(&mut frame, 6, 20, &grade));

        let column: Vec<f32> = (0..20).map(|row| luma(frame[row * 6 + 3])).collect();
        let row: Vec<f32> = (0..6).map(|column| luma(frame[7 * 6 + column])).collect();
        assert!(spread(&column) > 0.1, "bands must vary down a column");
        assert!(spread(&row) <= 1e-6, "bands must be constant across a row");
    }

    #[test]
    fn rotating_the_bands_swaps_the_varying_axis() {
        let mut parameters = ScanlinesParameters::default();
        parameters.angle_degrees = 90.0;
        let grade = valid_grade(parameters);
        let mut frame = flat_frame(20, 8, [0.5, 0.5, 0.5, 1.0]);
        assert!(scanlines_frame_f32(&mut frame, 20, 8, &grade));

        let row: Vec<f32> = (0..20).map(|column| luma(frame[3 * 20 + column])).collect();
        let column: Vec<f32> = (0..8).map(|row| luma(frame[row * 20 + 3])).collect();
        assert!(spread(&row) > 0.1);
        assert!(spread(&column) <= 1e-6);
    }

    #[test]
    fn wider_lines_darken_more_of_the_frame() {
        let dark_fraction = |line_width: f32| {
            let mut parameters = ScanlinesParameters::default();
            parameters.line_width = line_width;
            let grade = valid_grade(parameters);
            let mut frame = flat_frame(4, 32, [0.5, 0.5, 0.5, 1.0]);
            assert!(scanlines_frame_f32(&mut frame, 4, 32, &grade));
            frame.iter().filter(|pixel| luma(**pixel) < 0.25).count() as f32 / frame.len() as f32
        };
        assert!(dark_fraction(0.1) < dark_fraction(0.4));
        assert!(dark_fraction(0.4) < dark_fraction(0.9));
    }

    #[test]
    fn sharpness_softens_a_band_edge() {
        let edge_value = |sharpness: f32| {
            let mut parameters = ScanlinesParameters::default();
            parameters.sharpness = sharpness;
            let grade = valid_grade(parameters);
            let mut frame = flat_frame(1, 16, [0.5, 0.5, 0.5, 1.0]);
            assert!(scanlines_frame_f32(&mut frame, 1, 16, &grade));
            // Row four sits near the outer edge of the first band, where the
            // soft profile still lets some pattern through.
            luma(frame[4])
        };
        assert!(edge_value(1.0) < edge_value(0.0));
    }

    #[test]
    fn channel_offsets_fringe_the_colour_channels() {
        let mut parameters = ScanlinesParameters::default();
        parameters.red_offset = 0.4;
        let grade = valid_grade(parameters);
        let mut frame = flat_frame(1, 32, [0.5, 0.5, 0.5, 1.0]);
        assert!(scanlines_frame_f32(&mut frame, 1, 32, &grade));
        assert!(
            frame.iter().any(|pixel| (pixel[0] - pixel[1]).abs() > 0.05),
            "the red offset must separate red from green"
        );
    }

    #[test]
    fn colour_zero_flattens_the_pattern_to_luminance() {
        let mut parameters = ScanlinesParameters::default();
        parameters.colour = 0.0;
        let grade = valid_grade(parameters);
        let mut frame = flat_frame(2, 24, [0.6, 0.6, 0.6, 1.0]);
        assert!(scanlines_frame_f32(&mut frame, 2, 24, &grade));
        for pixel in &frame {
            assert!((pixel[0] - pixel[1]).abs() <= 1e-6);
            assert!((pixel[1] - pixel[2]).abs() <= 1e-6);
        }

        let mut parameters = ScanlinesParameters::default();
        parameters.color2 = [0.2, 0.0, 0.0];
        let grade = valid_grade(parameters);
        let mut frame = flat_frame(2, 24, [0.6, 0.6, 0.6, 1.0]);
        assert!(scanlines_frame_f32(&mut frame, 2, 24, &grade));
        assert!(frame.iter().any(|pixel| (pixel[0] - pixel[2]).abs() > 0.05));
    }

    #[test]
    fn lines_only_replaces_the_source_and_keeps_coverage() {
        let mut parameters = ScanlinesParameters::default();
        parameters.lines_only = 1.0;
        let grade = valid_grade(parameters);
        let mut frame = flat_frame(1, 32, [0.25, 0.25, 0.25, 0.75]);
        assert!(scanlines_frame_f32(&mut frame, 1, 32, &grade));
        for pixel in &frame {
            assert_eq!(pixel[3], 0.75);
            assert!(
                (pixel[0] - 1.0).abs() <= 1e-6 || pixel[0].abs() <= 1e-6,
                "lines only output must be one of the two band colours"
            );
        }
    }

    #[test]
    fn composite_types_combine_the_pattern_differently() {
        let with = |composite_type: ScanlinesComposite| {
            let mut parameters = ScanlinesParameters::default();
            parameters.composite_type = composite_type;
            let grade = valid_grade(parameters);
            let mut frame = flat_frame(1, 16, [0.5, 0.5, 0.5, 1.0]);
            assert!(scanlines_frame_f32(&mut frame, 1, 16, &grade));
            // Row five lands in the light half of the band cycle, where the
            // pattern is white, so the composites stay distinguishable.
            frame[5][0]
        };
        let multiply = with(ScanlinesComposite::Multiply);
        let screen = with(ScanlinesComposite::Screen);
        let add = with(ScanlinesComposite::Add);
        let normal = with(ScanlinesComposite::Normal);
        assert!(multiply < screen, "multiply must darken below screen");
        assert!(screen < add, "screen must stay below an additive sum");
        assert!(normal >= 0.0);
        assert!(with(ScanlinesComposite::Overlay) <= 1.0);
    }

    #[test]
    fn zero_strength_settings_are_exact_identities() {
        for parameters in [
            {
                let mut parameters = ScanlinesParameters::default();
                parameters.appearance = 0.0;
                parameters
            },
            {
                let mut parameters = ScanlinesParameters::default();
                parameters.composite = 0.0;
                parameters
            },
        ] {
            let grade = valid_grade(parameters);
            assert!(grade.is_identity());
            let mut frame = flat_frame(4, 4, [0.3, 0.4, 0.5, 0.6]);
            let before = frame.clone();
            assert!(scanlines_frame_f32(&mut frame, 4, 4, &grade));
            assert_eq!(frame, before);
        }

        let mut parameters = ScanlinesParameters::default();
        parameters.appearance = 0.0;
        parameters.lines_only = 1.0;
        assert!(!valid_grade(parameters).is_identity());
    }

    #[test]
    fn transparent_pixels_are_left_alone() {
        let grade = valid_grade(ScanlinesParameters::default());
        let mut frame = flat_frame(8, 8, [0.0; 4]);
        assert!(scanlines_frame_f32(&mut frame, 8, 8, &grade));
        assert!(frame.iter().all(|pixel| *pixel == [0.0; 4]));
    }

    #[test]
    fn short_and_empty_frames_are_refused() {
        let grade = valid_grade(ScanlinesParameters::default());
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!scanlines_frame_f32(&mut empty, 0, 0, &grade));
        let mut short = flat_frame(2, 2, [1.0; 4]);
        short.pop();
        assert!(!scanlines_frame_f32(&mut short, 2, 2, &grade));
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let grade = valid_grade(ScanlinesParameters::default());
        let first = {
            let mut frame = flat_frame(5, 9, [0.4, 0.5, 0.6, 1.0]);
            assert!(scanlines_frame_f32(&mut frame, 5, 9, &grade));
            frame
        };
        let second = {
            let mut frame = flat_frame(5, 9, [0.4, 0.5, 0.6, 1.0]);
            assert!(scanlines_frame_f32(&mut frame, 5, 9, &grade));
            frame
        };
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_is_reported() {
        let grade = valid_grade(ScanlinesParameters::default());
        let mut frame = flat_frame(4, 4, [0.5, 0.5, 0.5, 1.0]);
        let mut checks = 0;
        let result = apply_scanlines_f32_controlled(&mut frame, 4, 4, &grade, &mut || {
            checks += 1;
            if checks == 2 {
                Err("cancelled")
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err(), "cancelled");
    }

    #[test]
    fn the_hash_separates_distinct_configurations() {
        use std::collections::hash_map::DefaultHasher;

        let hash = |parameters: ScanlinesParameters| {
            let mut hasher = DefaultHasher::new();
            valid_grade(parameters).hash_state(&mut hasher);
            hasher.finish()
        };
        let mut rotated = ScanlinesParameters::default();
        rotated.angle_degrees = 45.0;
        let mut shifted = ScanlinesParameters::default();
        shifted.shift = 0.5;
        assert_ne!(hash(ScanlinesParameters::default()), hash(rotated));
        assert_ne!(hash(ScanlinesParameters::default()), hash(shifted));
        assert_eq!(
            hash(ScanlinesParameters::default()),
            hash(ScanlinesParameters::default())
        );
    }
}
