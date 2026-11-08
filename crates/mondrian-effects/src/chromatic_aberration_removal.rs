//! Radial chromatic aberration correction (RES-08).
//!
//! Red and blue sample on opposite sides of the green reference channel's
//! radius. Their signed displacement blends the two authored axis strengths
//! by the squared direction components. Alpha stays on the destination grid.

use crate::coverage::has_positive_coverage;

/// Largest magnitude of an authored lateral or longitudinal correction, in pixels.
pub const CHROMATIC_ABERRATION_REMOVAL_MAX_AMOUNT: f32 = 5.0;
/// Largest magnitude of the authored edge weighting.
pub const CHROMATIC_ABERRATION_REMOVAL_MAX_EDGE_SCALE: f32 = 10.0;

const IDENTITY_AMOUNT: f32 = 1.0e-6;
const MIN_RADIAL_SCALE: f64 = 0.25;

/// Invalid authoring input for the radial correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChromaticAberrationRemovalError {
    /// The parameter is NaN or infinite.
    #[error("chromatic aberration correction parameter `{field}` must be finite")]
    NonFinite {
        /// Authoring parameter identifier.
        field: &'static str,
    },
    /// The parameter is outside the supported authoring range.
    #[error("chromatic aberration correction parameter `{field}` is out of range")]
    OutOfRange {
        /// Authoring parameter identifier.
        field: &'static str,
    },
}

/// Authored radial correction. The centre is normalized to the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromaticAberrationRemovalGrade {
    lateral: f32,
    longitudinal: f32,
    edge_scale: f32,
    centre: [f32; 2],
}

impl ChromaticAberrationRemovalGrade {
    /// Validate the four authored parameters.
    pub fn new(
        lateral: f32,
        longitudinal: f32,
        edge_scale: f32,
        centre: [f32; 2],
    ) -> Result<Self, ChromaticAberrationRemovalError> {
        let amount = CHROMATIC_ABERRATION_REMOVAL_MAX_AMOUNT;
        let edge = CHROMATIC_ABERRATION_REMOVAL_MAX_EDGE_SCALE;
        for (field, value, min, max) in [
            ("lateral", lateral, -amount, amount),
            ("longitudinal", longitudinal, -amount, amount),
            ("edge_scale", edge_scale, -edge, edge),
            ("centre", centre[0], 0.0, 1.0),
            ("centre", centre[1], 0.0, 1.0),
        ] {
            if !value.is_finite() {
                return Err(ChromaticAberrationRemovalError::NonFinite { field });
            }
            if !(min..=max).contains(&value) {
                return Err(ChromaticAberrationRemovalError::OutOfRange { field });
            }
        }
        Ok(Self { lateral, longitudinal, edge_scale, centre })
    }

    /// Horizontal-axis correction in pixels.
    pub const fn lateral(self) -> f32 {
        self.lateral
    }

    /// Vertical-axis correction in pixels.
    pub const fn longitudinal(self) -> f32 {
        self.longitudinal
    }

    /// Raw edge weighting in the authored range −10 to 10.
    pub const fn edge_scale(self) -> f32 {
        self.edge_scale
    }

    /// Normalized picture centre.
    pub const fn centre(self) -> [f32; 2] {
        self.centre
    }

    /// Whether neither axis can introduce a visible shift.
    pub fn is_identity(self) -> bool {
        self.lateral.abs() <= IDENTITY_AMOUNT && self.longitudinal.abs() <= IDENTITY_AMOUNT
    }

    /// Spatial uniform cells `params` and `extra`.
    pub const fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.centre[0],
                self.centre[1],
                self.lateral,
                self.longitudinal,
            ],
            [self.edge_scale, 0.0, 0.0, 0.0],
        )
    }
}

#[derive(Clone, Copy)]
struct SamplingGeometry {
    centre: [f64; 2],
    radius: f64,
}

impl SamplingGeometry {
    fn new(width: usize, height: usize, centre: [f32; 2]) -> Self {
        let centre = [
            f64::from(centre[0]) * width as f64 - 0.5,
            f64::from(centre[1]) * height as f64 - 0.5,
        ];
        let radius_x = centre[0].abs().max(((width - 1) as f64 - centre[0]).abs());
        let radius_y = centre[1].abs().max(((height - 1) as f64 - centre[1]).abs());
        Self { centre, radius: radius_x.max(radius_y).max(1.0) }
    }

    fn sample_positions(
        self,
        [x, y]: [f64; 2],
        grade: ChromaticAberrationRemovalGrade,
    ) -> ([f64; 2], [f64; 2]) {
        let dx = x - self.centre[0];
        let dy = y - self.centre[1];
        let distance = dx.hypot(dy);
        if distance == 0.0 {
            return (self.centre, self.centre);
        }
        let cos = dx / distance;
        let sin = dy / distance;
        let direction =
            f64::from(grade.lateral) * cos * cos + f64::from(grade.longitudinal) * sin * sin;
        let edge =
            f64::from(grade.edge_scale) / f64::from(CHROMATIC_ABERRATION_REMOVAL_MAX_EDGE_SCALE);
        let weight = 1.0 + edge * (distance / self.radius).min(1.0);
        let shift = direction / self.radius * weight;
        let position = |scale: f64| {
            let scale = scale.max(MIN_RADIAL_SCALE);
            [self.centre[0] + dx * scale, self.centre[1] + dy * scale]
        };
        (position(1.0 + shift), position(1.0 - shift))
    }
}

fn sample_channel(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    [x, y]: [f64; 2],
    channel: usize,
    original: f32,
) -> f32 {
    let x = x.clamp(0.0, (width - 1) as f64);
    let y = y.clamp(0.0, (height - 1) as f64);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;

    let tap = |column: usize, row: usize| {
        let pixel = source[row * width + column];
        let alpha = pixel[3].clamp(0.0, 1.0);
        [pixel[channel] * alpha, alpha]
    };
    let lerp = |left: [f32; 2], right: [f32; 2], weight: f32| {
        [
            left[0] + (right[0] - left[0]) * weight,
            left[1] + (right[1] - left[1]) * weight,
        ]
    };
    let top = lerp(tap(x0, y0), tap(x1, y0), fx);
    let bottom = lerp(tap(x0, y1), tap(x1, y1), fx);
    let [associated, alpha] = lerp(top, bottom, fy);
    if has_positive_coverage(alpha) {
        associated / alpha
    } else {
        original
    }
}

/// Correct a full straight-alpha Float32 frame. Returns `false` for an invalid grid.
pub fn chromatic_aberration_removal_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ChromaticAberrationRemovalGrade,
) -> bool {
    apply_chromatic_aberration_removal_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

pub(crate) fn apply_chromatic_aberration_removal_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ChromaticAberrationRemovalGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(pixels.len()) {
        return Ok(false);
    }
    if width == 1 || height == 1 || grade.is_identity() {
        return Ok(true);
    }

    checkpoint()?;
    let source = pixels.to_vec();
    let geometry = SamplingGeometry::new(width, height, grade.centre);
    crate::parallel::for_each_row_band(pixels, width, 0..height, |first_row, band| {
        for (row_offset, row) in band.chunks_mut(width).enumerate() {
            let y = (first_row + row_offset) as f64;
            for (x, pixel) in row.iter_mut().enumerate() {
                if !has_positive_coverage(pixel[3]) {
                    continue;
                }
                let (red, blue) = geometry.sample_positions([x as f64, y], grade);
                if red == blue {
                    continue;
                }
                pixel[0] = sample_channel(&source, width, height, red, 0, pixel[0]);
                pixel[2] = sample_channel(&source, width, height, blue, 2, pixel[2]);
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(lateral: f32, longitudinal: f32, edge_scale: f32) -> ChromaticAberrationRemovalGrade {
        ChromaticAberrationRemovalGrade::new(lateral, longitudinal, edge_scale, [0.5, 0.5]).unwrap()
    }

    fn columns() -> Vec<[f32; 4]> {
        (0..81)
            .map(|index| {
                let x = (index % 9) as f32;
                let y = (index / 9) as f32;
                [x, y, x, 1.0]
            })
            .collect()
    }

    fn apply(frame: &mut [[f32; 4]], grade: ChromaticAberrationRemovalGrade) {
        assert!(chromatic_aberration_removal_frame_f32(frame, 9, 9, grade));
    }

    fn pixel(frame: &[[f32; 4]], x: usize, y: usize) -> [f32; 4] {
        frame[y * 9 + x]
    }

    #[test]
    fn a_zero_amount_is_an_identity() {
        let mut frame = columns();
        let original = frame.clone();
        let grade = grade(1.0e-6, -1.0e-6, 10.0);
        assert!(grade.is_identity());
        apply(&mut frame, grade);
        for (actual, expected) in frame.into_iter().zip(original) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
    }

    #[test]
    fn the_centre_is_a_fixed_point() {
        let mut frame = columns();
        apply(&mut frame, grade(1.0, -2.0, 4.0));
        assert_eq!(pixel(&frame, 4, 4), [4.0, 4.0, 4.0, 1.0]);
    }

    #[test]
    fn red_samples_outward_and_blue_inward_on_the_horizontal_meridian() {
        let mut frame = columns();
        apply(&mut frame, grade(1.0, 0.0, 0.0));
        assert_eq!(pixel(&frame, 6, 4), [6.5, 4.0, 5.5, 1.0]);
        assert_eq!(pixel(&frame, 7, 4), [7.75, 4.0, 6.25, 1.0]);
    }

    #[test]
    fn the_two_chroma_channels_stay_symmetric_about_green() {
        let mut frame = columns();
        apply(&mut frame, grade(1.0, 0.0, 0.0));
        for x in 5..=7 {
            let [red, _, blue, _] = pixel(&frame, x, 4);
            assert_eq!(red + blue, 2.0 * x as f32);
        }
    }

    #[test]
    fn samples_past_the_frame_edge_are_clamped() {
        let mut frame = columns();
        apply(&mut frame, grade(1.0, 0.0, 0.0));
        assert_eq!(pixel(&frame, 8, 4)[0], 8.0);
        assert_eq!(pixel(&frame, 0, 4)[0], 0.0);
    }

    #[test]
    fn longitudinal_shifts_rows_and_leaves_columns_alone() {
        let mut frame: Vec<_> =
            columns().into_iter().map(|[x, y, _, alpha]| [y, x, y, alpha]).collect();
        apply(&mut frame, grade(0.0, 1.0, 0.0));
        assert_eq!(pixel(&frame, 4, 6), [6.5, 4.0, 5.5, 1.0]);
        assert_eq!(pixel(&frame, 4, 8)[0], 8.0);
        assert_eq!(pixel(&frame, 8, 4), [4.0, 8.0, 4.0, 1.0]);
    }

    #[test]
    fn the_two_axes_compose_on_the_diagonal() {
        let geometry = SamplingGeometry::new(9, 9, [0.5, 0.5]);
        let (red, blue) = geometry.sample_positions([8.0, 8.0], grade(1.0, 1.0, 0.0));
        for (actual, expected) in red.into_iter().chain(blue).zip([9.0, 9.0, 7.0, 7.0]) {
            assert!((actual - expected).abs() < 1.0e-12);
        }
    }

    #[test]
    fn the_edge_scale_weights_the_shift_by_radius() {
        let mut frame = columns();
        apply(&mut frame, grade(1.0, 0.0, 10.0));
        assert_eq!(pixel(&frame, 5, 4)[0], 5.3125);
        assert_eq!(pixel(&frame, 6, 4)[0], 6.75);
        assert_eq!(pixel(&frame, 8, 4)[0], 8.0);
    }

    #[test]
    fn a_negative_edge_scale_unshifts_the_border() {
        let mut frame = columns();
        apply(&mut frame, grade(1.0, 0.0, -10.0));
        assert_eq!(pixel(&frame, 6, 4)[0], 6.25);
        assert_eq!(pixel(&frame, 8, 4), [8.0, 4.0, 8.0, 1.0]);
    }

    #[test]
    fn a_tiny_frame_clamps_the_scale_instead_of_mirroring() {
        let mut frame: Vec<_> = (0..9)
            .map(|index| {
                let x = (index % 3) as f32;
                [x, 0.0, x, 1.0]
            })
            .collect();
        assert!(chromatic_aberration_removal_frame_f32(
            &mut frame,
            3,
            3,
            grade(5.0, 0.0, 0.0)
        ));
        assert_eq!(frame[5][2], 1.25);
        assert!(frame.iter().flatten().all(|value| value.is_finite()));
    }

    #[test]
    fn a_fully_transparent_pixel_is_left_alone() {
        let mut frame = columns();
        frame[4 * 9 + 6][3] = 0.0;
        apply(&mut frame, grade(3.0, 0.0, 0.0));
        assert_eq!(
            pixel(&frame, 6, 4).map(f32::to_bits),
            [6.0_f32, 4.0, 6.0, 0.0].map(f32::to_bits)
        );
    }

    fn transparent_sixth_column() -> Vec<[f32; 4]> {
        let mut frame = columns();
        for row in frame.chunks_mut(9) {
            row[6][3] = 0.0;
        }
        frame
    }

    #[test]
    fn a_transparent_sample_keeps_the_authored_channel() {
        let mut frame = transparent_sixth_column();
        apply(&mut frame, grade(2.0, 0.0, 0.0));
        assert_eq!(pixel(&frame, 8, 4), [8.0, 4.0, 8.0, 1.0]);
    }

    #[test]
    fn opaque_samples_are_not_darkened_by_transparent_neighbours() {
        let mut frame = transparent_sixth_column();
        apply(&mut frame, grade(2.0, 0.0, 0.0));
        assert_eq!(pixel(&frame, 7, 4)[2], 5.0);
    }

    #[test]
    fn green_and_alpha_are_passed_through_bit_exactly() {
        let mut frame = columns();
        for (index, pixel) in frame.iter_mut().enumerate() {
            pixel[3] = (index % 5) as f32 * 0.2;
        }
        let original = frame.clone();
        apply(&mut frame, grade(3.0, -2.0, 7.0));
        for (actual, expected) in frame.into_iter().zip(original) {
            assert_eq!(actual[1].to_bits(), expected[1].to_bits());
            assert_eq!(actual[3].to_bits(), expected[3].to_bits());
        }
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let mut frame: Vec<_> = columns()
            .into_iter()
            .map(|[x, _, _, alpha]| {
                let value = 0.5 * x - 1.0;
                [value, 0.0, value, alpha]
            })
            .collect();
        apply(&mut frame, grade(1.0, 0.0, 0.0));
        assert_eq!(pixel(&frame, 6, 4)[0], 2.25);
        assert_eq!(pixel(&frame, 0, 4)[0], -1.0);
    }

    #[test]
    fn the_sample_position_matches_an_independent_polar_form() {
        let geometry = SamplingGeometry::new(9, 9, [0.5, 0.5]);
        let grade = grade(1.5, -0.75, 4.0);
        for y in 0..9 {
            for x in 0..9 {
                let dx = f64::from(x) - 4.0;
                let dy = f64::from(y) - 4.0;
                let theta = dy.atan2(dx);
                let radius = (dx * dx + dy * dy).sqrt();
                let k = (1.5 * theta.cos().powi(2) - 0.75 * theta.sin().powi(2)) / 4.0;
                let shift = k * (1.0 + 0.4 * (radius / 4.0).min(1.0));
                let expected = [1.0 + shift, 1.0 - shift].map(|scale| {
                    [
                        4.0 + radius * scale * theta.cos(),
                        4.0 + radius * scale * theta.sin(),
                    ]
                });
                let (red, blue) = geometry.sample_positions([f64::from(x), f64::from(y)], grade);
                for (actual, expected) in
                    [red, blue].into_iter().flatten().zip(expected.into_iter().flatten())
                {
                    assert!(
                        (actual - expected).abs() < 1.0e-9,
                        "{x}, {y}: {actual} != {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_inverse_mapping_round_trips_the_radius() {
        let geometry = SamplingGeometry::new(9, 9, [0.5, 0.5]);
        let grade = grade(1.5, -0.75, 4.0);
        // On the horizontal ray the normalized strength is 1.5 / 4.0.
        for (index, q) in [0.375_f64, -0.375].into_iter().enumerate() {
            let a = 1.0 + q;
            let b = q * 0.4;
            for source_radius in [0.1, 0.5, 0.9, 1.4, 1.9] {
                let destination_radius = if source_radius <= a + b {
                    2.0 * source_radius / (a + (a * a + 4.0 * b * source_radius).sqrt())
                } else {
                    source_radius / (a + b)
                };
                let (red, blue) =
                    geometry.sample_positions([4.0 + 4.0 * destination_radius, 4.0], grade);
                let measured = ([red, blue][index][0] - 4.0) / 4.0;
                assert!(
                    (measured - source_radius).abs() < 1.0e-9,
                    "{q}: {measured} != {source_radius}"
                );
            }
        }
    }

    #[test]
    fn a_geometry_mismatch_is_refused() {
        for (width, height, len) in [(0, 9, 0), (9, 9, 80), (9, 9, 82), (usize::MAX, 2, 81)] {
            let mut frame = vec![[1.0, 2.0, 3.0, 1.0]; len];
            let original = frame.clone();
            assert!(!chromatic_aberration_removal_frame_f32(
                &mut frame,
                width,
                height,
                grade(1.0, 0.0, 0.0)
            ));
            assert_eq!(frame, original);
        }
        for (width, height) in [(1, 9), (9, 1)] {
            let original = [[1.0, 2.0, 3.0, 0.3]; 9];
            let mut frame = original;
            assert!(chromatic_aberration_removal_frame_f32(
                &mut frame,
                width,
                height,
                grade(1.0, 0.0, 0.0),
            ));
            assert_eq!(frame, original);
        }
    }

    #[test]
    fn the_constructor_rejects_non_finite_parameters() {
        use ChromaticAberrationRemovalError::NonFinite;
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for (result, field) in [
                (
                    ChromaticAberrationRemovalGrade::new(value, 0.0, 0.0, [0.5, 0.5]),
                    "lateral",
                ),
                (
                    ChromaticAberrationRemovalGrade::new(0.0, value, 0.0, [0.5, 0.5]),
                    "longitudinal",
                ),
                (
                    ChromaticAberrationRemovalGrade::new(0.0, 0.0, value, [0.5, 0.5]),
                    "edge_scale",
                ),
                (
                    ChromaticAberrationRemovalGrade::new(0.0, 0.0, 0.0, [value, 0.5]),
                    "centre",
                ),
                (
                    ChromaticAberrationRemovalGrade::new(0.0, 0.0, 0.0, [0.5, value]),
                    "centre",
                ),
            ] {
                assert_eq!(result, Err(NonFinite { field }));
            }
        }
    }
}
