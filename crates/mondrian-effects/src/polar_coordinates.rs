//! Polar coordinates (rectangular <-> polar) effect kernel.
//!
//! The effect reinterprets the frame in the other coordinate pair. In the
//! rectangular to polar direction the column of a pixel becomes the angle around
//! the frame centre and its row becomes the radius, so the top row collapses onto
//! the centre pixel and the bottom row wraps into a full ring. The polar to
//! rectangular direction is the inverse: the angle across the frame is read from
//! the pixel's direction and the row is read as the radius.
//!
//! Both directions are inverse mappings: the kernel computes where each output
//! pixel comes from and samples the snapshot of the source frame, so the result
//! never depends on the processing order. Colour is sampled on premultiplied
//! values and converted back to the straight-alpha contract, and extended-range
//! values pass through without clamping.

use crate::resample::{sample_bilinear_clamped, sample_nearest_clamped};
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

const TAU: f64 = std::f64::consts::TAU;

/// Which coordinate pair the frame is converted into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolarConversion {
    /// Read the column as an angle and the row as a radius.
    RectToPolar,
    /// Read the direction as an angle and the distance as a radius.
    PolarToRect,
}

impl PolarConversion {
    /// Maps the authored menu value (1 = rectangular to polar, 2 = the inverse).
    pub fn from_menu(value: i64) -> Option<Self> {
        match value {
            1 => Some(Self::RectToPolar),
            2 => Some(Self::PolarToRect),
            _ => None,
        }
    }

    /// The option key authored by the parameter schema.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::RectToPolar => "rect_to_polar",
            Self::PolarToRect => "polar_to_rect",
        }
    }

    /// Resolves the option key authored by the parameter schema.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "rect_to_polar" => Some(Self::RectToPolar),
            "polar_to_rect" => Some(Self::PolarToRect),
            _ => None,
        }
    }

    /// Packing used by the fused GPU pass.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::RectToPolar => 1,
            Self::PolarToRect => 2,
        }
    }
}

/// How the source is sampled at the computed position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolarInterpolation {
    /// Copy the nearest texel.
    Nearest,
    /// Interpolate the four surrounding texels.
    Bilinear,
}

impl PolarInterpolation {
    /// Maps the authored menu value (0 = nearest, 1 = bilinear).
    pub fn from_menu(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::Nearest),
            1 => Some(Self::Bilinear),
            _ => None,
        }
    }

    /// The option key authored by the parameter schema.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::Bilinear => "bilinear",
        }
    }

    /// Resolves the option key authored by the parameter schema.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "nearest" => Some(Self::Nearest),
            "bilinear" => Some(Self::Bilinear),
            _ => None,
        }
    }

    /// Packing used by the fused GPU pass.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Nearest => 0,
            Self::Bilinear => 1,
        }
    }
}

/// Validated author state of the polar coordinate effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolarCoordinatesGrade {
    conversion: PolarConversion,
    interpolation: PolarInterpolation,
}

impl PolarCoordinatesGrade {
    /// Builds a grade from the two authored choices.
    pub fn new(conversion: PolarConversion, interpolation: PolarInterpolation) -> Self {
        Self { conversion, interpolation }
    }

    /// The authored conversion direction.
    pub fn conversion(self) -> PolarConversion {
        self.conversion
    }

    /// The authored sampling interpolation.
    pub fn interpolation(self) -> PolarInterpolation {
        self.interpolation
    }

    /// Uniform packing for the GPU pass: `[conversion, interpolation, 0, 0]`.
    pub fn gpu_controls(self) -> [f32; 4] {
        [
            f32::from(self.conversion.as_u8()),
            f32::from(self.interpolation.as_u8()),
            0.0,
            0.0,
        ]
    }

    /// Computes the source position that an output pixel reads from.
    ///
    /// The arithmetic is written in `f64` so the WGSL pass can reproduce it with
    /// `f32` trig calls and still agree to the parity tolerance.
    fn source_position(self, width: usize, height: usize, column: f64, row: f64) -> (f64, f64) {
        let centre_x = width as f64 * 0.5;
        let centre_y = height as f64 * 0.5;
        match self.conversion {
            PolarConversion::RectToPolar => {
                let angle = column / width as f64 * TAU;
                (centre_x + row * angle.cos(), centre_y + row * angle.sin())
            }
            PolarConversion::PolarToRect => {
                let delta_x = column - centre_x;
                let delta_y = row - centre_y;
                let radius = delta_x.hypot(delta_y);
                let mut fraction = delta_y.atan2(delta_x) / TAU;
                if fraction < 0.0 {
                    fraction += 1.0;
                }
                (fraction * width as f64, radius)
            }
        }
    }
}

/// Applies the conversion to a whole frame, returning `false` when the frame
/// cannot be processed.
pub fn polar_coordinates_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: PolarCoordinatesGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_polar_coordinates_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the conversion in place with an explicit cancellation checkpoint.
pub(crate) fn apply_polar_coordinates_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: PolarCoordinatesGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }

    let frame = &mut working[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let (sample_x, sample_y) =
                    grade.source_position(width, height, column as f64, row as f64);
                *pixel = match grade.interpolation {
                    PolarInterpolation::Bilinear => sample_bilinear_clamped(
                        source.as_slice(),
                        width,
                        height,
                        sample_x,
                        sample_y,
                    ),
                    PolarInterpolation::Nearest => {
                        sample_nearest_clamped(source.as_slice(), width, height, sample_x, sample_y)
                    }
                };
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for (index, pixel) in pixels.iter_mut().enumerate() {
            let column = (index % width) as f32;
            let row = (index / width) as f32;
            *pixel = [
                column * 0.11 + row * 0.03,
                0.35 - column * 0.02,
                0.2 + row * 0.07,
                1.0,
            ];
        }
        pixels
    }

    fn grade(
        conversion: PolarConversion,
        interpolation: PolarInterpolation,
    ) -> PolarCoordinatesGrade {
        PolarCoordinatesGrade::new(conversion, interpolation)
    }

    #[test]
    fn menu_and_key_mappings_round_trip() {
        for conversion in [PolarConversion::RectToPolar, PolarConversion::PolarToRect] {
            assert_eq!(
                PolarConversion::from_key(conversion.as_key()),
                Some(conversion)
            );
            assert_eq!(
                PolarConversion::from_menu(i64::from(conversion.as_u8())),
                Some(conversion)
            );
        }
        for interpolation in [PolarInterpolation::Nearest, PolarInterpolation::Bilinear] {
            assert_eq!(
                PolarInterpolation::from_key(interpolation.as_key()),
                Some(interpolation)
            );
            assert_eq!(
                PolarInterpolation::from_menu(i64::from(interpolation.as_u8())),
                Some(interpolation)
            );
        }
        assert_eq!(PolarConversion::from_menu(0), None);
        assert_eq!(PolarConversion::from_menu(3), None);
        assert_eq!(PolarInterpolation::from_menu(2), None);
        assert_eq!(PolarConversion::from_key("spin"), None);
    }

    #[test]
    fn rectangular_to_polar_collapses_the_top_row_onto_the_centre() {
        let width = 8;
        let height = 6;
        let pixels = frame(width, height);
        let centre = pixels[(height / 2) * width + width / 2];
        let mut converted = pixels.clone();
        assert!(polar_coordinates_frame_f32(
            &mut converted,
            width,
            height,
            grade(PolarConversion::RectToPolar, PolarInterpolation::Bilinear),
        ));
        for pixel in converted.iter().take(width) {
            for (channel, value) in pixel.iter().enumerate() {
                assert!((value - centre[channel]).abs() <= 1.0e-6);
            }
        }
    }

    #[test]
    fn rectangular_to_polar_reads_the_angle_along_the_row() {
        let width = 8;
        let height = 6;
        let pixels = frame(width, height);
        let mut converted = pixels.clone();
        assert!(polar_coordinates_frame_f32(
            &mut converted,
            width,
            height,
            grade(PolarConversion::RectToPolar, PolarInterpolation::Bilinear),
        ));
        // Column `width / 4` is a quarter turn, so radius `r` reads straight
        // below the centre at `(centre_x, centre_y + r)`.
        let centre_x = width / 2;
        let centre_y = height / 2;
        for radius in 0..3_usize {
            let expected = pixels[(centre_y + radius) * width + centre_x];
            let actual = converted[radius * width + width / 4];
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 1.0e-6,
                    "radius {radius} channel {channel}: {actual:?} vs {expected:?}"
                );
            }
        }
    }

    #[test]
    fn polar_to_rectangular_puts_the_centre_at_the_origin() {
        let width = 8;
        let height = 6;
        let pixels = frame(width, height);
        let mut converted = pixels.clone();
        assert!(polar_coordinates_frame_f32(
            &mut converted,
            width,
            height,
            grade(PolarConversion::PolarToRect, PolarInterpolation::Bilinear),
        ));
        let centre_index = (height / 2) * width + width / 2;
        for channel in 0..4 {
            assert!((converted[centre_index][channel] - pixels[0][channel]).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn polar_to_rectangular_reads_the_radius_from_the_rows() {
        let width = 8;
        let height = 6;
        let pixels = frame(width, height);
        let mut converted = pixels.clone();
        assert!(polar_coordinates_frame_f32(
            &mut converted,
            width,
            height,
            grade(PolarConversion::PolarToRect, PolarInterpolation::Bilinear),
        ));
        let centre_y = height / 2;
        for radius in 0..3_usize {
            let expected = pixels[radius * width];
            let actual = converted[centre_y * width + width / 2 + radius];
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 1.0e-6,
                    "radius {radius} channel {channel}: {actual:?} vs {expected:?}"
                );
            }
        }
    }

    #[test]
    fn a_constant_frame_stays_constant_in_both_directions() {
        let width = 7;
        let height = 5;
        let constant = [0.25_f32, 0.5, 0.125, 1.0];
        for conversion in [PolarConversion::RectToPolar, PolarConversion::PolarToRect] {
            for interpolation in [PolarInterpolation::Nearest, PolarInterpolation::Bilinear] {
                let mut pixels = vec![constant; width * height];
                assert!(polar_coordinates_frame_f32(
                    &mut pixels,
                    width,
                    height,
                    grade(conversion, interpolation),
                ));
                for pixel in &pixels {
                    for channel in 0..4 {
                        assert!((pixel[channel] - constant[channel]).abs() <= 1.0e-6);
                    }
                }
            }
        }
    }

    #[test]
    fn nearest_and_bilinear_samples_disagree_on_a_gradient() {
        let width = 9;
        let height = 7;
        let pixels = frame(width, height);
        let mut nearest = pixels.clone();
        let mut bilinear = pixels.clone();
        assert!(polar_coordinates_frame_f32(
            &mut nearest,
            width,
            height,
            grade(PolarConversion::RectToPolar, PolarInterpolation::Nearest),
        ));
        assert!(polar_coordinates_frame_f32(
            &mut bilinear,
            width,
            height,
            grade(PolarConversion::RectToPolar, PolarInterpolation::Bilinear),
        ));
        let mut disagreements = 0;
        for (nearest_pixel, bilinear_pixel) in nearest.iter().zip(bilinear.iter()) {
            if (nearest_pixel[0] - bilinear_pixel[0]).abs() > 1.0e-4 {
                disagreements += 1;
            }
        }
        assert!(
            disagreements > 0,
            "the two samplers produced the same frame"
        );
    }

    #[test]
    fn the_mapping_matches_an_independent_evaluation() {
        let width = 6;
        let height = 4;
        let pixels = frame(width, height);
        let mut converted = pixels.clone();
        let authored = grade(PolarConversion::RectToPolar, PolarInterpolation::Bilinear);
        assert!(polar_coordinates_frame_f32(
            &mut converted,
            width,
            height,
            authored,
        ));
        let column = 3_usize;
        let row = 1_usize;
        let angle = column as f64 / width as f64 * std::f64::consts::TAU;
        let sample_x = width as f64 * 0.5 + row as f64 * angle.cos();
        let sample_y = height as f64 * 0.5 + row as f64 * angle.sin();
        let expected = sample_bilinear_clamped(&pixels, width, height, sample_x, sample_y);
        for channel in 0..4 {
            assert!(
                (converted[row * width + column][channel] - expected[channel]).abs() <= 1.0e-6,
                "channel {channel}: {:?} vs {expected:?}",
                converted[row * width + column]
            );
        }
    }

    #[test]
    fn transparent_neighbours_do_not_tint_visible_pixels() {
        let width = 4;
        let height = 4;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; width * height];
        for (index, pixel) in pixels.iter_mut().enumerate() {
            *pixel = [0.4, 0.2, 0.6, 0.5];
            // A fully transparent bright neighbour would pull the colour of an
            // unpremultiplied average towards white.
            if index == 5 {
                *pixel = [9.0, 9.0, 9.0, 0.0];
            }
        }
        assert!(polar_coordinates_frame_f32(
            &mut pixels,
            width,
            height,
            grade(PolarConversion::RectToPolar, PolarInterpolation::Bilinear),
        ));
        for pixel in &pixels {
            if pixel[3] <= 0.0 {
                continue;
            }
            assert!(
                (pixel[0] - 0.4).abs() <= 1.0e-6
                    && (pixel[1] - 0.2).abs() <= 1.0e-6
                    && (pixel[2] - 0.6).abs() <= 1.0e-6,
                "visible pixel took colour from a transparent neighbour: {pixel:?}"
            );
        }
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let width = 6;
        let height = 4;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; width * height];
        pixels[width] = [2.5, -0.75, 3.5, 1.0];
        assert!(polar_coordinates_frame_f32(
            &mut pixels,
            width,
            height,
            grade(PolarConversion::PolarToRect, PolarInterpolation::Nearest),
        ));
        // The pixel one step right of the centre reads the origin texel of row
        // one, which carries the extended values.
        let centre = pixels[(height / 2) * width + width / 2 + 1];
        assert!((centre[0] - 2.5).abs() <= 1.0e-6);
        assert!((centre[1] + 0.75).abs() <= 1.0e-6);
        assert!((centre[2] - 3.5).abs() <= 1.0e-6);
    }

    #[test]
    fn frames_that_cannot_be_processed_are_rejected() {
        let mut pixels = vec![[0.0_f32; 4]; 4];
        assert!(!polar_coordinates_frame_f32(
            &mut pixels,
            4,
            3,
            grade(PolarConversion::RectToPolar, PolarInterpolation::Bilinear),
        ));
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!polar_coordinates_frame_f32(
            &mut empty,
            0,
            0,
            grade(PolarConversion::RectToPolar, PolarInterpolation::Bilinear),
        ));
    }

    #[test]
    fn the_result_is_deterministic() {
        let width = 7;
        let height = 5;
        let pixels = frame(width, height);
        let mut first = pixels.clone();
        let mut second = pixels.clone();
        let authored = grade(PolarConversion::PolarToRect, PolarInterpolation::Bilinear);
        assert!(polar_coordinates_frame_f32(
            &mut first, width, height, authored
        ));
        assert!(polar_coordinates_frame_f32(
            &mut second,
            width,
            height,
            authored
        ));
        assert_eq!(first, second);
    }

    #[test]
    fn gpu_controls_carry_both_choices() {
        let authored = grade(PolarConversion::PolarToRect, PolarInterpolation::Nearest);
        assert_eq!(authored.gpu_controls(), [2.0, 0.0, 0.0, 0.0]);
        let authored = grade(PolarConversion::RectToPolar, PolarInterpolation::Bilinear);
        assert_eq!(authored.gpu_controls(), [1.0, 1.0, 0.0, 0.0]);
    }
}
