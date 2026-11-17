//! Beam: a soft line of light across the frame, with a travelling highlight.
//!
//! The effect is a pure point operation. Every output pixel depends on its own
//! coordinates and the authored settings only, so there is no halo, no scratch
//! plane, and no ordering constraint beyond the point-op chain.
//!
//! Geometry. The frame is measured in pixels, with pixel centres at
//! `(x + 0.5, y + 0.5)`. The authored endpoints are normalized to the frame,
//!
//! ```text
//! A = start * (width, height)
//! B = end * (width, height)
//! B' = A + (B - A) * clamp(length, 0, 1)
//! e = B' - A
//! ```
//!
//! and for a pixel `p` the projected station along the beam is
//!
//! ```text
//! s = clamp((p - A) . e / max(e . e, 1e-6), 0, 1)
//! q = A + e * s
//! d = |p - q|
//! ```
//!
//! so `s` runs from the start (0) to the visible end (1) and `d` is the pixel
//! distance to the beam axis.
//!
//! Profile. The half thickness is `h = mix(thickness_start, thickness_end, s) /
//! 2`; `perspective` tapers it along the beam,
//!
//! ```text
//! half = max(h * (1 + perspective * (s - 0.5)), 0)
//! core = 1 - smoothstep(0, max(half, 0.5), d)
//! band = max(half * (1 + softness * 4), half + 0.5)
//! halo = 1 - smoothstep(max(half, 0.5), band, d)
//! ```
//!
//! so `softness = 0` still leaves a half-pixel antialiasing ramp and larger
//! values widen the falloff. The core is `inside_color`, the falloff mixes
//! toward `outside_color`.
//!
//! Light. A highlight travels along the beam with `time`,
//!
//! ```text
//! pulse = 1 - smoothstep(0.02, 0.25, |s - time|)
//! light = mix(outside_color, inside_color, core) * halo * (1 + 0.75 * pulse)
//! ```
//!
//! and `composite` chooses how the source receives it: at 0 the light is added
//! to the source radiance and the source alpha grows by the halo; at 1 the
//! source is covered by the light inside the halo, with the alpha replaced by
//! `max(source.a, halo)`. Values are not clamped, so the maths stays valid in
//! scene-linear HDR; the grading chain above owns the final display clamp.
//!
//! Coverage is never invented. Pixels without positive coverage are left bit
//! for bit, matching the renderer's global coverage guard, and pixels outside
//! the halo are returned bit for bit; a zero-alpha result is normalized to
//! zeroed RGBA.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Documented start point, normalized to the frame.
pub const BEAM_DEFAULT_START: [f32; 2] = [0.1, 0.5];
/// Documented end point, normalized to the frame.
pub const BEAM_DEFAULT_END: [f32; 2] = [0.9, 0.5];
/// Documented visible length, as a fraction of the authored span.
pub const BEAM_DEFAULT_LENGTH: f32 = 0.25;
/// Documented highlight station along the beam.
pub const BEAM_DEFAULT_TIME: f32 = 0.5;
/// Documented starting thickness in pixels.
pub const BEAM_DEFAULT_THICKNESS: f32 = 8.0;
/// Documented softness of the falloff.
pub const BEAM_DEFAULT_SOFTNESS: f32 = 0.5;
/// Documented core colour.
pub const BEAM_DEFAULT_INSIDE_COLOUR: [f32; 3] = [1.0, 0.75, 0.98];
/// Documented falloff colour.
pub const BEAM_DEFAULT_OUTSIDE_COLOUR: [f32; 3] = [1.0, 0.1, 0.1];
/// Documented perspective taper.
pub const BEAM_DEFAULT_PERSPECTIVE: f32 = 1.0;
/// Documented mix between adding and covering.
pub const BEAM_DEFAULT_COMPOSITE: f32 = 0.0;
/// Largest documented thickness in pixels.
pub const BEAM_MAX_THICKNESS: f32 = 4_000.0;

/// Rows between cooperative cancellation checks.
const CHECKPOINT_STRIDE: usize = 4_096;
/// Shortest beam axis squared, so a degenerate beam stays a point sprite.
const BEAM_MIN_LENGTH_SQUARED: f32 = 1.0e-6;

/// Renderer-facing controls: six four-component cells.
pub type BeamGpuControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// The eleven authored values of a beam.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamParameters {
    /// Start point, normalized to the frame.
    pub start: [f32; 2],
    /// End point, normalized to the frame.
    pub end: [f32; 2],
    /// Visible length, as a fraction of the authored span.
    pub length: f32,
    /// Highlight station along the visible beam.
    pub time: f32,
    /// Thickness at the start, in pixels.
    pub thickness_start: f32,
    /// Thickness at the end, in pixels.
    pub thickness_end: f32,
    /// Falloff softness.
    pub softness: f32,
    /// Core colour.
    pub inside_color: [f32; 3],
    /// Falloff colour.
    pub outside_color: [f32; 3],
    /// Perspective taper along the beam.
    pub perspective: f32,
    /// Mix between adding the light and covering the source.
    pub composite: f32,
}

impl Default for BeamParameters {
    fn default() -> Self {
        Self {
            start: BEAM_DEFAULT_START,
            end: BEAM_DEFAULT_END,
            length: BEAM_DEFAULT_LENGTH,
            time: BEAM_DEFAULT_TIME,
            thickness_start: BEAM_DEFAULT_THICKNESS,
            thickness_end: BEAM_DEFAULT_THICKNESS,
            softness: BEAM_DEFAULT_SOFTNESS,
            inside_color: BEAM_DEFAULT_INSIDE_COLOUR,
            outside_color: BEAM_DEFAULT_OUTSIDE_COLOUR,
            perspective: BEAM_DEFAULT_PERSPECTIVE,
            composite: BEAM_DEFAULT_COMPOSITE,
        }
    }
}

/// Why a beam parameter set was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BeamError {
    /// A parameter was infinite or NaN.
    #[error("beam.{0} must be finite")]
    NonFinite(&'static str),
    /// A parameter was outside its documented range.
    #[error("beam.{0} must be within its documented range")]
    OutOfRange(&'static str),
}

impl BeamError {
    /// The parameter that was rejected.
    pub const fn parameter(self) -> &'static str {
        match self {
            Self::NonFinite(parameter) | Self::OutOfRange(parameter) => parameter,
        }
    }
}

/// A validated beam configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamGrade {
    parameters: BeamParameters,
}

impl BeamGrade {
    /// Validates and wraps an authored parameter set.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError`] naming the first parameter that is not finite or
    /// that falls outside its documented range.
    pub fn new(parameters: BeamParameters) -> Result<Self, BeamError> {
        let numeric = [
            ("start", parameters.start[0], 0.0, 1.0),
            ("start", parameters.start[1], 0.0, 1.0),
            ("end", parameters.end[0], 0.0, 1.0),
            ("end", parameters.end[1], 0.0, 1.0),
            ("length", parameters.length, 0.0, 1.0),
            ("time", parameters.time, 0.0, 1.0),
            (
                "thickness_start",
                parameters.thickness_start,
                0.0,
                BEAM_MAX_THICKNESS,
            ),
            (
                "thickness_end",
                parameters.thickness_end,
                0.0,
                BEAM_MAX_THICKNESS,
            ),
            ("softness", parameters.softness, 0.0, 1.0),
            ("perspective", parameters.perspective, 0.0, 1.0),
            ("composite", parameters.composite, 0.0, 1.0),
        ];
        for (parameter, value, minimum, maximum) in numeric {
            if !value.is_finite() {
                return Err(BeamError::NonFinite(parameter));
            }
            if value < minimum || value > maximum {
                return Err(BeamError::OutOfRange(parameter));
            }
        }
        for (parameter, colour) in [
            ("inside_color", parameters.inside_color),
            ("outside_color", parameters.outside_color),
        ] {
            for component in colour {
                if !component.is_finite() {
                    return Err(BeamError::NonFinite(parameter));
                }
                if !(0.0..=1.0).contains(&component) {
                    return Err(BeamError::OutOfRange(parameter));
                }
            }
        }
        Ok(Self { parameters })
    }

    /// The documented default configuration.
    pub fn documented_defaults() -> Self {
        Self::new(BeamParameters::default()).expect("the documented beam defaults are in range")
    }

    /// The validated parameters.
    pub const fn parameters(&self) -> &BeamParameters {
        &self.parameters
    }

    /// Start point, normalized to the frame.
    pub const fn start(&self) -> [f32; 2] {
        self.parameters.start
    }

    /// End point, normalized to the frame.
    pub const fn end(&self) -> [f32; 2] {
        self.parameters.end
    }

    /// Visible length, as a fraction of the authored span.
    pub const fn length(&self) -> f32 {
        self.parameters.length
    }

    /// Highlight station along the visible beam.
    pub const fn time(&self) -> f32 {
        self.parameters.time
    }

    /// Thickness at the start, in pixels.
    pub const fn thickness_start(&self) -> f32 {
        self.parameters.thickness_start
    }

    /// Thickness at the end, in pixels.
    pub const fn thickness_end(&self) -> f32 {
        self.parameters.thickness_end
    }

    /// Falloff softness.
    pub const fn softness(&self) -> f32 {
        self.parameters.softness
    }

    /// Core colour.
    pub const fn inside_color(&self) -> [f32; 3] {
        self.parameters.inside_color
    }

    /// Falloff colour.
    pub const fn outside_color(&self) -> [f32; 3] {
        self.parameters.outside_color
    }

    /// Perspective taper along the beam.
    pub const fn perspective(&self) -> f32 {
        self.parameters.perspective
    }

    /// Mix between adding the light and covering the source.
    pub const fn composite(&self) -> f32 {
        self.parameters.composite
    }

    /// Whether the grade leaves every pixel untouched.
    ///
    /// A zero length hides the beam, and two zero thicknesses leave nothing to
    /// draw; the perspective taper alone cannot hide a visible beam.
    pub fn is_identity(&self) -> bool {
        self.parameters.length <= 0.0
            || self.parameters.thickness_start.max(self.parameters.thickness_end) <= 0.0
    }

    /// Relative render cost of the point operation.
    pub const fn cost(&self) -> u8 {
        2
    }

    /// The six four-component cells the GPU point pass reads.
    pub fn gpu_controls(&self) -> BeamGpuControls {
        let parameters = self.parameters;
        let params = [
            parameters.length,
            parameters.time,
            parameters.thickness_start,
            parameters.thickness_end,
        ];
        let color = [
            parameters.softness,
            parameters.perspective,
            parameters.composite,
            0.0,
        ];
        let extra0 = [
            parameters.start[0],
            parameters.start[1],
            parameters.end[0],
            parameters.end[1],
        ];
        let extra1 = [
            parameters.inside_color[0],
            parameters.inside_color[1],
            parameters.inside_color[2],
            1.0,
        ];
        let extra2 = [
            parameters.outside_color[0],
            parameters.outside_color[1],
            parameters.outside_color[2],
            0.0,
        ];
        let extra3 = [0.0; 4];
        (params, color, extra0, extra1, extra2, extra3)
    }

    /// Feeds every authored value into a stable hasher.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        let parameters = self.parameters;
        for component in parameters.start {
            component.to_bits().hash(state);
        }
        for component in parameters.end {
            component.to_bits().hash(state);
        }
        for value in [
            parameters.length,
            parameters.time,
            parameters.thickness_start,
            parameters.thickness_end,
            parameters.softness,
            parameters.perspective,
            parameters.composite,
        ] {
            value.to_bits().hash(state);
        }
        for component in parameters.inside_color {
            component.to_bits().hash(state);
        }
        for component in parameters.outside_color {
            component.to_bits().hash(state);
        }
    }
}

/// Applies the beam to a frame, ignoring cooperative cancellation.
///
/// Pixels whose alpha has no positive coverage are left untouched, so the frame
/// matches the renderer's coverage guard.
pub fn apply_beam_f32(pixels: &mut [[f32; 4]], width: u32, height: u32, grade: &BeamGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_beam_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the beam to a frame of `width * height` pixels.
///
/// Pixels whose alpha has no positive coverage are left untouched, matching the
/// renderer's coverage guard.
///
/// # Errors
///
/// Propagates the first error returned by `checkpoint`, leaving already written
/// rows in place.
pub fn apply_beam_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: u32,
    height: u32,
    grade: &BeamGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let width = width as usize;
    let height = height as usize;
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || pixels.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let parameters = grade.parameters;
    let frame_width = width as f32;
    let frame_height = height as f32;
    let start = [
        parameters.start[0] * frame_width,
        parameters.start[1] * frame_height,
    ];
    let end = [
        parameters.end[0] * frame_width,
        parameters.end[1] * frame_height,
    ];
    let length = parameters.length.clamp(0.0, 1.0);
    let axis = [(end[0] - start[0]) * length, (end[1] - start[1]) * length];
    let axis_length_squared = (axis[0] * axis[0] + axis[1] * axis[1]).max(BEAM_MIN_LENGTH_SQUARED);
    let core_edge_floor = 0.5_f32;
    let composite = parameters.composite.clamp(0.0, 1.0);

    let rows_per_chunk = (CHECKPOINT_STRIDE / width).max(1);
    let mut chunk_start = 0usize;
    while chunk_start < height {
        checkpoint()?;
        let rows = rows_per_chunk.min(height - chunk_start);
        let chunk_end = chunk_start + rows;
        crate::parallel::for_each_row_band(
            &mut pixels[chunk_start * width..chunk_end * width],
            width,
            0..rows,
            |first_row, band| {
                for (offset, row_pixels) in band.chunks_mut(width).enumerate() {
                    let row = chunk_start + first_row + offset;
                    let y = row as f32 + 0.5;
                    for (column, pixel) in row_pixels.iter_mut().enumerate() {
                        if !crate::coverage::has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
                            continue;
                        }
                        let x = column as f32 + 0.5;
                        let relative_x = x - start[0];
                        let relative_y = y - start[1];
                        let station = ((relative_x * axis[0] + relative_y * axis[1])
                            / axis_length_squared)
                            .clamp(0.0, 1.0);
                        let nearest_x = start[0] + axis[0] * station;
                        let nearest_y = start[1] + axis[1] * station;
                        let distance = ((x - nearest_x).powi(2) + (y - nearest_y).powi(2)).sqrt();
                        let base = (parameters.thickness_start
                            + (parameters.thickness_end - parameters.thickness_start) * station)
                            * 0.5;
                        let half =
                            (base * (1.0 + parameters.perspective * (station - 0.5))).max(0.0);
                        let core_edge = half.max(core_edge_floor);
                        let core = 1.0 - smoothstep(0.0, core_edge, distance);
                        let band =
                            (half * (1.0 + parameters.softness * 4.0)).max(half + core_edge_floor);
                        let halo = 1.0 - smoothstep(core_edge, band, distance);
                        if halo <= 0.0 {
                            continue;
                        }
                        let pulse = 1.0 - smoothstep(0.02, 0.25, (station - parameters.time).abs());
                        let gain = halo * (1.0 + 0.75 * pulse);
                        let mut light = [0.0_f32; 3];
                        for (channel, slot) in light.iter_mut().enumerate() {
                            let colour = mix(
                                parameters.outside_color[channel],
                                parameters.inside_color[channel],
                                core,
                            );
                            *slot = colour * gain;
                        }
                        let source_alpha = pixel[3];
                        let mut added = [0.0_f32; 4];
                        let mut covered = [0.0_f32; 4];
                        for channel in 0..3 {
                            added[channel] = pixel[channel] + light[channel];
                            covered[channel] = mix(pixel[channel], light[channel], halo);
                        }
                        added[3] = (source_alpha + halo).clamp(0.0, 1.0);
                        covered[3] = source_alpha.max(halo);
                        let mut result = [0.0_f32; 4];
                        for channel in 0..4 {
                            result[channel] = mix(added[channel], covered[channel], composite);
                        }
                        *pixel = if result[3] <= 0.0 { [0.0; 4] } else { result };
                    }
                }
            },
        );
        chunk_start = chunk_end;
    }
    checkpoint()?;
    Ok(true)
}

/// Linear interpolation between two scalars.
fn mix(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

/// Hermite step from 0 at `edge0` to 1 at `edge1`.
fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]

    use super::*;

    type BeamCase = (&'static str, fn(&mut BeamParameters));

    fn valid_grade(parameters: BeamParameters) -> BeamGrade {
        BeamGrade::new(parameters).expect("the authored beam parameters are valid")
    }

    fn filled(width: usize, height: usize, pixel: [f32; 4]) -> Vec<[f32; 4]> {
        vec![pixel; width * height]
    }

    fn pixel(frame: &[[f32; 4]], width: usize, column: usize, row: usize) -> [f32; 4] {
        frame[row * width + column]
    }

    fn luminance(pixel: [f32; 4]) -> f32 {
        pixel[0] + pixel[1] + pixel[2]
    }

    #[test]
    fn documented_defaults_are_in_range() {
        let grade = BeamGrade::documented_defaults();
        assert_eq!(grade.start(), BEAM_DEFAULT_START);
        assert_eq!(grade.end(), BEAM_DEFAULT_END);
        assert_eq!(grade.length(), 0.25);
        assert_eq!(grade.time(), 0.5);
        assert_eq!(grade.thickness_start(), 8.0);
        assert_eq!(grade.thickness_end(), 8.0);
        assert_eq!(grade.softness(), 0.5);
        assert_eq!(grade.inside_color(), [1.0, 0.75, 0.98]);
        assert_eq!(grade.outside_color(), [1.0, 0.1, 0.1]);
        assert_eq!(grade.perspective(), 1.0);
        assert_eq!(grade.composite(), 0.0);
        assert!(!grade.is_identity());
        assert_eq!(grade.cost(), 2);
    }

    #[test]
    fn out_of_range_parameters_are_rejected() {
        let cases: [BeamCase; 11] = [
            ("start", |parameters| parameters.start = [1.5, 0.5]),
            ("end", |parameters| parameters.end = [-0.5, 0.5]),
            ("length", |parameters| parameters.length = 1.5),
            ("time", |parameters| parameters.time = 2.0),
            ("thickness_start", |parameters| {
                parameters.thickness_start = 4_001.0
            }),
            ("thickness_end", |parameters| {
                parameters.thickness_end = -1.0
            }),
            ("softness", |parameters| parameters.softness = 1.5),
            ("inside_color", |parameters| {
                parameters.inside_color = [-0.1, 0.0, 0.0]
            }),
            ("outside_color", |parameters| {
                parameters.outside_color = [0.0, 0.0, 1.1]
            }),
            ("perspective", |parameters| parameters.perspective = 1.5),
            ("composite", |parameters| parameters.composite = -1.0),
        ];
        for (parameter, break_it) in cases {
            let mut parameters = BeamParameters::default();
            break_it(&mut parameters);
            let error = BeamGrade::new(parameters).expect_err(parameter);
            assert_eq!(error.parameter(), parameter);
        }
    }

    #[test]
    fn non_finite_parameters_are_rejected() {
        let mut parameters = BeamParameters::default();
        parameters.time = f32::NAN;
        assert_eq!(
            BeamGrade::new(parameters).expect_err("NaN is rejected"),
            BeamError::NonFinite("time")
        );
        let mut parameters = BeamParameters::default();
        parameters.inside_color = [0.0, f32::INFINITY, 0.0];
        assert_eq!(
            BeamGrade::new(parameters).expect_err("infinity is rejected"),
            BeamError::NonFinite("inside_color")
        );
    }

    #[test]
    fn identity_cases_are_the_documented_ones() {
        let mut hidden = BeamParameters::default();
        hidden.length = 0.0;
        assert!(valid_grade(hidden).is_identity());

        let mut flat = BeamParameters::default();
        flat.thickness_start = 0.0;
        flat.thickness_end = 0.0;
        assert!(valid_grade(flat).is_identity());

        let mut tapered = BeamParameters::default();
        tapered.perspective = 0.0;
        assert!(!valid_grade(tapered).is_identity());
    }

    #[test]
    fn a_point_beam_is_brightest_at_its_centre() {
        let mut parameters = BeamParameters::default();
        parameters.start = [0.5, 0.5];
        parameters.end = [0.5, 0.5];
        parameters.length = 0.1;
        parameters.time = 0.0;
        parameters.thickness_start = 12.0;
        parameters.thickness_end = 12.0;
        let grade = valid_grade(parameters);
        let mut frame = filled(9, 9, [0.0, 0.0, 0.0, 1.0]);
        assert!(apply_beam_f32(&mut frame, 9, 9, &grade));

        let centre = luminance(pixel(&frame, 9, 4, 4));
        assert!(centre > 1.0, "the centre is bright, got {centre}");
        for (column, row) in [(0, 0), (8, 8), (0, 4), (4, 0), (8, 4), (4, 8)] {
            let edge = luminance(pixel(&frame, 9, column, row));
            assert!(edge < centre, "({column}, {row}) is dimmer than the centre");
        }
    }

    #[test]
    fn perspective_controls_the_width_of_the_beam() {
        let mut parameters = BeamParameters::default();
        parameters.start = [0.0, 0.5];
        parameters.end = [1.0, 0.5];
        parameters.length = 1.0;
        parameters.time = 0.0;
        parameters.thickness_start = 20.0;
        parameters.thickness_end = 20.0;
        parameters.softness = 0.0;
        parameters.perspective = 0.0;
        parameters.composite = 0.0;
        let flat = valid_grade(parameters);

        let mut wide = parameters;
        wide.perspective = 1.0;
        let tapered = valid_grade(wide);

        let width = 64;
        let height = 33;
        // The beam runs along the middle row, so each row's station is 0.5 and
        // the taper is neutral there; sample the vertical extent at the ends by
        // comparing a column near the start against one near the end.
        let mut flat_frame = filled(width, height, [0.0, 0.0, 0.0, 1.0]);
        assert!(apply_beam_f32(
            &mut flat_frame,
            width as u32,
            height as u32,
            &flat
        ));
        let mut tapered_frame = flat_frame.clone();
        assert!(apply_beam_f32(
            &mut tapered_frame,
            width as u32,
            height as u32,
            &tapered
        ));

        let lit = |frame: &[[f32; 4]], column: usize| {
            (0..height)
                .filter(|row| luminance(pixel(frame, width, column, *row)) > 0.0)
                .count()
        };
        let flat_start = lit(&flat_frame, 2);
        let flat_end = lit(&flat_frame, width - 3);
        assert_eq!(
            flat_start, flat_end,
            "perspective 0 keeps the width constant along the beam"
        );
        let tapered_end = lit(&tapered_frame, width - 3);
        assert!(
            tapered_end > lit(&tapered_frame, 2),
            "perspective 1 widens the beam toward the end: {}, {}",
            lit(&tapered_frame, 2),
            tapered_end
        );
        assert!(tapered_end > flat_end);
    }

    #[test]
    fn length_truncation_keeps_the_highlight_near_the_visible_end() {
        let mut parameters = BeamParameters::default();
        parameters.start = [0.0, 0.5];
        parameters.end = [1.0, 0.5];
        parameters.length = 0.5;
        parameters.time = 1.0;
        parameters.thickness_start = 6.0;
        parameters.thickness_end = 6.0;
        parameters.perspective = 0.0;
        parameters.softness = 0.0;
        let grade = valid_grade(parameters);
        let width = 64;
        let height = 9;
        let mut frame = filled(width, height, [0.0, 0.0, 0.0, 1.0]);
        assert!(apply_beam_f32(
            &mut frame,
            width as u32,
            height as u32,
            &grade
        ));

        let row = 4;
        let mut brightest = 0;
        let mut brightest_value = f32::MIN;
        for column in 0..width {
            let value = luminance(pixel(&frame, width, column, row));
            if value > brightest_value {
                brightest_value = value;
                brightest = column;
            }
        }
        assert!(
            (29..=33).contains(&brightest),
            "the highlight sits at the truncated end, column {brightest}"
        );
    }

    #[test]
    fn softness_widens_the_falloff_from_half_a_pixel() {
        let width = 65;
        let height = 65;
        let base = |softness: f32| {
            let mut parameters = BeamParameters::default();
            parameters.start = [0.5, 0.5];
            parameters.end = [0.5, 0.5];
            parameters.length = 0.1;
            parameters.time = 0.0;
            parameters.thickness_start = 6.0;
            parameters.thickness_end = 6.0;
            parameters.perspective = 0.0;
            parameters.softness = softness;
            parameters.composite = 0.0;
            valid_grade(parameters)
        };

        // A single point beam at (32.5, 32.5); column 35 is exactly three
        // pixels away, which is the half thickness, and column 36 is four.
        let mut hard = filled(width, height, [0.0, 0.0, 0.0, 1.0]);
        assert!(apply_beam_f32(
            &mut hard,
            width as u32,
            height as u32,
            &base(0.0)
        ));
        let mut soft = filled(width, height, [0.0, 0.0, 0.0, 1.0]);
        assert!(apply_beam_f32(
            &mut soft,
            width as u32,
            height as u32,
            &base(0.5)
        ));

        let row = 32;
        let hard_edge = luminance(pixel(&hard, width, 35, row));
        let hard_outside = luminance(pixel(&hard, width, 36, row));
        assert!(hard_edge > 0.0, "the half-thickness pixel is lit");
        assert_eq!(hard_outside, 0.0, "softness 0 stops within a pixel");

        let soft_outside = luminance(pixel(&soft, width, 36, row));
        assert!(soft_outside > 0.0, "softness widens the falloff");
        assert!(
            soft_outside < luminance(pixel(&soft, width, 35, row)),
            "the falloff still dims outward"
        );
    }

    #[test]
    fn composite_zero_adds_light_to_black() {
        let mut parameters = BeamParameters::default();
        parameters.composite = 0.0;
        parameters.time = 0.0;
        let grade = valid_grade(parameters);
        let width = 32;
        let height = 8;
        let mut frame = filled(width, height, [0.0, 0.0, 0.0, 1.0]);
        assert!(apply_beam_f32(
            &mut frame,
            width as u32,
            height as u32,
            &grade
        ));
        let centre = pixel(&frame, width, width / 2, height / 2);
        assert!(luminance(centre) > 0.0, "the beam adds light on black");
        assert_eq!(centre[3], 1.0, "an opaque source stays opaque");
    }

    #[test]
    fn composite_one_covers_the_source_inside_the_halo() {
        let mut parameters = BeamParameters::default();
        parameters.composite = 1.0;
        parameters.time = 0.0;
        parameters.thickness_start = 40.0;
        parameters.thickness_end = 40.0;
        parameters.softness = 0.0;
        parameters.perspective = 0.0;
        let grade = valid_grade(parameters);
        let width = 32;
        let height = 8;
        let source = [0.25_f32, 0.25, 0.25, 0.5];
        let mut frame = filled(width, height, source);
        assert!(apply_beam_f32(
            &mut frame,
            width as u32,
            height as u32,
            &grade
        ));
        let covered = pixel(&frame, width, width / 2, height / 2);
        assert!(
            covered[0] > source[0],
            "the light covers the source instead of adding to it"
        );
        assert_eq!(covered[3], 1.0, "the halo replaces the alpha");
    }

    #[test]
    fn pixels_outside_the_halo_are_untouched() {
        let mut parameters = BeamParameters::default();
        parameters.thickness_start = 2.0;
        parameters.thickness_end = 2.0;
        parameters.softness = 0.0;
        parameters.perspective = 0.0;
        parameters.composite = 0.0;
        let grade = valid_grade(parameters);
        let width = 32;
        let height = 32;
        let source = [0.125_f32, 0.25, 0.5, 1.0];
        let mut frame = filled(width, height, source);
        assert!(apply_beam_f32(
            &mut frame,
            width as u32,
            height as u32,
            &grade
        ));
        let corner = pixel(&frame, width, 0, 0);
        assert_eq!(
            corner, source,
            "a transparent corner outside the halo stays bit-for-bit"
        );
    }

    #[test]
    fn transparent_pixels_are_never_painted() {
        let mut parameters = BeamParameters::default();
        parameters.start = [0.0, 0.5];
        parameters.end = [1.0, 0.5];
        parameters.length = 1.0;
        parameters.thickness_start = 12.0;
        parameters.thickness_end = 12.0;
        parameters.perspective = 0.0;
        let grade = valid_grade(parameters);
        let width = 32;
        let height = 8;
        let mut frame = filled(width, height, [0.0, 0.0, 0.0, 0.0]);
        // One covered row keeps the beam running; every other row is empty.
        for column in 0..width {
            frame[4 * width + column] = [0.2, 0.3, 0.4, 1.0];
        }
        let original = frame.clone();
        assert!(apply_beam_f32(
            &mut frame,
            width as u32,
            height as u32,
            &grade
        ));
        for row in 0..height {
            if row == 4 {
                continue;
            }
            for column in 0..width {
                assert_eq!(
                    pixel(&frame, width, column, row),
                    pixel(&original, width, column, row),
                    "the empty pixel at ({column}, {row}) stays bit-for-bit"
                );
            }
        }
        assert_ne!(
            pixel(&frame, width, width / 2, 4),
            pixel(&original, width, width / 2, 4),
            "the covered row still receives the beam"
        );
    }

    #[test]
    fn time_moves_the_highlight_along_the_beam() {
        let width = 64;
        let height = 5;
        let sample = |time: f32| {
            let mut parameters = BeamParameters::default();
            parameters.start = [0.0, 0.5];
            parameters.end = [1.0, 0.5];
            parameters.length = 1.0;
            parameters.time = time;
            parameters.thickness_start = 6.0;
            parameters.thickness_end = 6.0;
            parameters.perspective = 0.0;
            parameters.softness = 0.0;
            let grade = valid_grade(parameters);
            let mut frame = filled(width, height, [0.0, 0.0, 0.0, 1.0]);
            assert!(apply_beam_f32(
                &mut frame,
                width as u32,
                height as u32,
                &grade
            ));
            let row = 2;
            let mut brightest = 0;
            let mut brightest_value = f32::MIN;
            for column in 0..width {
                let value = luminance(pixel(&frame, width, column, row));
                if value > brightest_value {
                    brightest_value = value;
                    brightest = column;
                }
            }
            brightest
        };
        let early = sample(0.15);
        let late = sample(0.85);
        assert!(early < width / 2, "the early highlight is near the start");
        assert!(late > width / 2, "the late highlight is near the end");
        assert!(late > early);
    }

    #[test]
    fn the_checkpoint_runs_per_chunk() {
        let width = 1;
        let height = CHECKPOINT_STRIDE + 1;
        let grade = BeamGrade::documented_defaults();
        let mut frame = filled(width, height, [0.0, 0.0, 0.0, 1.0]);
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), ()>(())
        };
        let applied = apply_beam_f32_controlled(
            &mut frame,
            width as u32,
            height as u32,
            &grade,
            &mut checkpoint,
        )
        .expect("the frame is applied");
        assert!(applied);
        assert!(
            calls >= 2,
            "the kernel checks in while it works, got {calls}"
        );
    }

    #[test]
    fn degenerate_frames_do_not_panic() {
        let grade = BeamGrade::documented_defaults();
        let mut single = [[0.0_f32, 0.0, 0.0, 1.0]; 1];
        assert!(apply_beam_f32(&mut single, 1, 1, &grade));
        let mut empty: [[f32; 4]; 0] = [];
        assert!(!apply_beam_f32(&mut empty, 0, 0, &grade));
        let mut short = filled(2, 2, [0.0, 0.0, 0.0, 1.0]);
        assert!(!apply_beam_f32(&mut short, 3, 3, &grade));
    }

    #[test]
    fn identity_leaves_the_frame_untouched() {
        let mut parameters = BeamParameters::default();
        parameters.length = 0.0;
        let grade = valid_grade(parameters);
        let source = [0.5_f32, 0.25, 0.125, 0.75];
        let mut frame = filled(4, 4, source);
        assert!(apply_beam_f32(&mut frame, 4, 4, &grade));
        assert!(frame.iter().all(|pixel| *pixel == source));
    }

    #[test]
    fn hashing_covers_the_authored_values() {
        use std::collections::hash_map::DefaultHasher;

        let mut hasher = DefaultHasher::new();
        BeamGrade::documented_defaults().hash_state(&mut hasher);
        let default_hash = hasher.finish();

        let mut parameters = BeamParameters::default();
        parameters.time = 0.75;
        let mut hasher = DefaultHasher::new();
        valid_grade(parameters).hash_state(&mut hasher);
        assert_ne!(default_hash, hasher.finish());

        let mut hasher = DefaultHasher::new();
        BeamGrade::documented_defaults().hash_state(&mut hasher);
        assert_eq!(default_hash, hasher.finish());
    }
}
