//! Basic 3D (TRF-07), a perspective projection of the layer as a flat card.

use glam::{DMat3, DVec3, Vec2, Vec3};

use crate::coverage::{has_positive_coverage, straight_rgba_from_premultiplied};
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::premultiply_rgba_controlled;

/// Largest magnitude of either authored rotation, in degrees.
pub const BASIC_3D_MAX_ANGLE: f32 = 180.0;
/// Largest magnitude of the distance adjustment, in pixels.
pub const BASIC_3D_MAX_DISTANCE: f32 = 30_000.0;

const MIN_DETERMINANT: f64 = 1.0e-12;
const MAX_RECIPROCAL_DEPTH: f32 = 1.0e5;
const EDGE_EPSILON: f32 = 1.0e-6;

/// An invalid Basic 3D authoring value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Basic3DError {
    /// A parameter is NaN or infinite.
    #[error("Basic 3D parameter `{field}` must be finite")]
    NonFinite {
        /// Stable authoring parameter name.
        field: &'static str,
    },
    /// A parameter exceeds its supported range.
    #[error("Basic 3D parameter `{field}` is out of range")]
    OutOfRange {
        /// Stable authoring parameter name.
        field: &'static str,
    },
}

/// Authored rotations, camera distance adjustment, and specular strength.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Basic3DGrade {
    swivel: f32,
    tilt: f32,
    distance: f32,
    specular: f32,
}

impl Basic3DGrade {
    /// Builds a grade from the four validated authoring values.
    pub fn new(swivel: f32, tilt: f32, distance: f32, specular: f32) -> Result<Self, Basic3DError> {
        for (field, value, min, max) in [
            ("swivel", swivel, -BASIC_3D_MAX_ANGLE, BASIC_3D_MAX_ANGLE),
            ("tilt", tilt, -BASIC_3D_MAX_ANGLE, BASIC_3D_MAX_ANGLE),
            (
                "distance",
                distance,
                -BASIC_3D_MAX_DISTANCE,
                BASIC_3D_MAX_DISTANCE,
            ),
            ("specular", specular, 0.0, 1.0),
        ] {
            if !value.is_finite() {
                return Err(Basic3DError::NonFinite { field });
            }
            if !(min..=max).contains(&value) {
                return Err(Basic3DError::OutOfRange { field });
            }
        }
        Ok(Self { swivel, tilt, distance, specular })
    }

    /// Raw authored values in swivel, tilt, distance, and specular order.
    pub const fn authored_values(self) -> [f32; 4] {
        [self.swivel, self.tilt, self.distance, self.specular]
    }

    /// Whether the default camera and lighting leave the frame untouched.
    pub fn is_identity(self) -> bool {
        self.authored_values() == [0.0; 4]
    }

    fn projection(self, width: u32, height: u32) -> Option<Basic3DProjection> {
        if width < 2 || height < 2 || self.is_identity() {
            return None;
        }
        let focal_length = width.max(height);
        let rotation = DMat3::from_rotation_x(f64::from(self.tilt).to_radians())
            * DMat3::from_rotation_y(f64::from(self.swivel).to_radians());
        let depth = 1.0 + f64::from(self.distance) / f64::from(focal_length);
        let homography = DMat3::from_cols(rotation.x_axis, rotation.y_axis, DVec3::Z * depth);
        let determinant = homography.determinant();
        if !determinant.is_finite() || determinant.abs() <= MIN_DETERMINANT {
            return None;
        }
        // Keep the unscaled inverse so the homogeneous denominator is 1 / camera depth.
        let rows = homography
            .inverse()
            .as_mat3()
            .transpose()
            .to_cols_array_2d()
            .map(Vec3::from_array);
        if !rows.iter().all(|row| row.is_finite()) {
            return None;
        }
        Some(Basic3DProjection {
            rows,
            inverse_focal_length: (focal_length as f32).recip(),
            specular: self.specular,
            normal: -rotation.z_axis.as_vec3(),
        })
    }

    /// Pack `params`, `extra`, `aces`, and `extra1` for the actual raster.
    /// Identity and degenerate projections pass the source through unchanged.
    pub fn gpu_controls(self, width: u32, height: u32) -> Option<[[f32; 4]; 4]> {
        let projection = self.projection(width, height)?;
        let [x, y, z] = projection.rows;
        Some([
            x.extend(projection.inverse_focal_length).to_array(),
            y.extend(projection.specular).to_array(),
            z.extend(MAX_RECIPROCAL_DEPTH).to_array(),
            projection.normal.extend(EDGE_EPSILON).to_array(),
        ])
    }
}

struct Basic3DProjection {
    rows: [Vec3; 3],
    inverse_focal_length: f32,
    specular: f32,
    normal: Vec3,
}

impl Basic3DProjection {
    fn source_at(&self, destination: Vec2) -> Option<Vec2> {
        let homogeneous = destination.extend(1.0);
        let depth = self.rows[2].dot(homogeneous);
        if !(depth > 0.0 && depth < MAX_RECIPROCAL_DEPTH) {
            return None;
        }
        Some(Vec2::new(self.rows[0].dot(homogeneous), self.rows[1].dot(homogeneous)) / depth)
    }

    fn specular_at(&self, destination: Vec2, light: Vec3) -> f32 {
        let view = (-destination).extend(-1.0).normalize();
        let normal = if self.normal.dot(view) < 0.0 {
            -self.normal
        } else {
            self.normal
        };
        let alignment = normal.dot((light + view).normalize()).max(0.0);
        let squared = alignment * alignment;
        let fourth = squared * squared;
        self.specular * (fourth * fourth)
    }
}

/// Project a straight-alpha Float32 frame in place. Returns `false` for an invalid grid.
pub fn basic_3d_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: Basic3DGrade,
) -> bool {
    apply_basic_3d_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

pub(crate) fn apply_basic_3d_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: Basic3DGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(pixels.len()) {
        return Ok(false);
    }
    let (Ok(width_u32), Ok(height_u32)) = (u32::try_from(width), u32::try_from(height)) else {
        return Ok(false);
    };
    let Some(projection) = grade.projection(width_u32, height_u32) else {
        return Ok(true);
    };

    checkpoint()?;
    let mut source = pixels.to_vec();
    premultiply_rgba_controlled(&mut source, checkpoint)?;
    let centre = Vec2::new(width as f32, height as f32) * 0.5;
    let focal_length = width.max(height) as f32;
    let bounds = centre * projection.inverse_focal_length + Vec2::splat(EDGE_EPSILON);
    let light = Vec3::new(0.35, -0.45, -1.0).normalize();
    crate::parallel::for_each_row_band(pixels, width, 0..height, |start, band| {
        for (row, scanline) in band.chunks_mut(width).enumerate() {
            for (column, pixel) in scanline.iter_mut().enumerate() {
                let destination = (Vec2::new(column as f32, (start + row) as f32) + 0.5 - centre)
                    * projection.inverse_focal_length;
                let Some(source_point) = projection.source_at(destination) else {
                    *pixel = [0.0; 4];
                    continue;
                };
                if !source_point.abs().cmple(bounds).all() {
                    *pixel = [0.0; 4];
                    continue;
                }
                let texel = centre + source_point * focal_length - 0.5;
                let associated = sample_bilinear_clamped(
                    &source,
                    width,
                    height,
                    f64::from(texel.x),
                    f64::from(texel.y),
                );
                *pixel = straight_rgba_from_premultiplied(associated);
                if projection.specular > 0.0 && has_positive_coverage(pixel[3]) {
                    let specular = projection.specular_at(destination, light);
                    for colour in &mut pixel[..3] {
                        *colour += specular;
                    }
                }
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient() -> Vec<[f32; 4]> {
        (0..81).map(|i| [(i % 9) as f32, (i / 9) as f32, 0.25, 1.0]).collect()
    }

    #[test]
    fn basic_3d_checks_author_bounds_and_non_finite_values() {
        assert!(Basic3DGrade::new(-180.0, 180.0, -30_000.0, 1.0).is_ok());
        assert_eq!(
            Basic3DGrade::new(180.1, 0.0, 0.0, 0.0),
            Err(Basic3DError::OutOfRange { field: "swivel" })
        );
        assert_eq!(
            Basic3DGrade::new(0.0, -180.1, 0.0, 0.0),
            Err(Basic3DError::OutOfRange { field: "tilt" })
        );
        assert_eq!(
            Basic3DGrade::new(0.0, 0.0, 30_001.0, 0.0),
            Err(Basic3DError::OutOfRange { field: "distance" })
        );
        assert_eq!(
            Basic3DGrade::new(0.0, 0.0, 0.0, -0.01),
            Err(Basic3DError::OutOfRange { field: "specular" })
        );
        assert_eq!(
            Basic3DGrade::new(f32::NAN, 0.0, 0.0, 0.0),
            Err(Basic3DError::NonFinite { field: "swivel" })
        );
        assert_eq!(
            Basic3DGrade::new(0.0, f32::NEG_INFINITY, 0.0, 0.0),
            Err(Basic3DError::NonFinite { field: "tilt" })
        );
        assert_eq!(
            Basic3DGrade::new(0.0, 0.0, f32::INFINITY, 0.0),
            Err(Basic3DError::NonFinite { field: "distance" })
        );
        assert_eq!(
            Basic3DGrade::new(0.0, 0.0, 0.0, f32::NAN),
            Err(Basic3DError::NonFinite { field: "specular" })
        );
    }

    #[test]
    fn basic_3d_default_is_bit_exact_identity() {
        let grade = Basic3DGrade::new(0.0, 0.0, 0.0, 0.0).unwrap();
        let mut pixels = gradient();
        pixels[0] = [-0.0, 0.7, -0.1, 0.0];
        let original = pixels.clone();
        assert!(grade.is_identity());
        assert!(grade.gpu_controls(9, 9).is_none());
        assert!(basic_3d_frame_f32(&mut pixels, 9, 9, grade));
        assert!(pixels
            .iter()
            .zip(original)
            .all(|(a, b)| a.map(f32::to_bits) == b.map(f32::to_bits)));
    }

    #[test]
    fn basic_3d_degenerate_card_passes_through() {
        let original = gradient();
        for (swivel, tilt, distance) in [(90.0, 0.0, 0.0), (0.0, -90.0, 0.0), (20.0, 0.0, -9.0)] {
            let grade = Basic3DGrade::new(swivel, tilt, distance, 0.5).unwrap();
            let mut pixels = original.clone();
            assert!(grade.gpu_controls(9, 9).is_none());
            assert!(basic_3d_frame_f32(&mut pixels, 9, 9, grade));
            assert_eq!(pixels, original);
        }
    }

    #[test]
    fn basic_3d_distinguishes_single_axis_grids_from_invalid_storage() {
        let grade = Basic3DGrade::new(20.0, -30.0, 5.0, 0.0).unwrap();
        let original = [[1.0, 2.0, 3.0, 0.5]; 9];
        for (width, height) in [(1, 9), (9, 1)] {
            let mut pixels = original;
            assert!(grade.gpu_controls(width as u32, height as u32).is_none());
            assert!(basic_3d_frame_f32(&mut pixels, width, height, grade));
            assert_eq!(pixels, original);
        }
        for (width, height) in [(0, 9), (4, 4), (usize::MAX, 2)] {
            let mut pixels = original;
            assert!(!basic_3d_frame_f32(&mut pixels, width, height, grade));
            assert_eq!(pixels, original);
        }
    }

    #[test]
    fn basic_3d_distance_shrinks_or_magnifies_about_the_centre() {
        let distant = Basic3DGrade::new(0.0, 0.0, 9.0, 0.0).unwrap();
        let near = Basic3DGrade::new(0.0, 0.0, -4.5, 0.0).unwrap();
        let mut pixels = gradient();
        assert!(basic_3d_frame_f32(&mut pixels, 9, 9, distant));
        assert_eq!(pixels[4 * 9 + 4], [4.0, 4.0, 0.25, 1.0]);
        assert!((pixels[4 * 9 + 6][0] - 8.0).abs() < 1.0e-6);
        assert_eq!(pixels[4 * 9 + 7], [0.0; 4]);
        let mut pixels = gradient();
        assert!(basic_3d_frame_f32(&mut pixels, 9, 9, near));
        assert!((pixels[4 * 9 + 6][0] - 5.0).abs() < 1.0e-6);
        assert_eq!(pixels[4 * 9 + 7][3], 1.0);
    }

    #[test]
    fn basic_3d_inverse_round_trips_the_independent_forward_projection() {
        let swivel = 34.0_f64;
        let tilt = -27.0_f64;
        let grade = Basic3DGrade::new(swivel as f32, tilt as f32, 17.0, 0.0).unwrap();
        let projector = grade.projection(101, 49).unwrap();
        let (sy, cy) = swivel.to_radians().sin_cos();
        let (sx, cx) = tilt.to_radians().sin_cos();
        for [x, y] in [[0.0, 0.0], [0.34, -0.2], [-0.35, 0.17], [0.03, 0.21]] {
            let rotated_x = cy * x;
            let rotated_y = cx * y + sx * sy * x;
            let depth = 1.0 + 17.0 / 101.0 - cx * sy * x + sx * y;
            let t = Vec2::new((rotated_x / depth) as f32, (rotated_y / depth) as f32);
            let restored = projector.source_at(t).unwrap();
            assert!((f64::from(restored.x) - x).abs() < 1.0e-6);
            assert!((f64::from(restored.y) - y).abs() < 1.0e-6);
        }
    }

    #[test]
    fn basic_3d_clears_points_behind_the_camera_and_inside_the_near_plane() {
        for distance in [-18.0, -8.999_95] {
            let grade = Basic3DGrade::new(0.0, 0.0, distance, 1.0).unwrap();
            assert!(grade.gpu_controls(9, 9).is_some());
            let mut pixels = gradient();
            assert!(basic_3d_frame_f32(&mut pixels, 9, 9, grade));
            assert!(pixels.iter().all(|pixel| *pixel == [0.0; 4]));
        }
    }

    #[test]
    fn basic_3d_specular_is_linear_and_does_not_add_coverage() {
        let grade = Basic3DGrade::new(0.0, 0.0, 0.0, 0.75).unwrap();
        let mut pixels = vec![[2.0, -0.5, 0.25, 0.5]; 81];
        pixels[0][3] = 0.0;
        assert!(basic_3d_frame_f32(&mut pixels, 9, 9, grade));
        let cosine = 1.0 / (1.0_f32 + 0.35 * 0.35 + 0.45 * 0.45).sqrt();
        let specular = 0.75 * ((1.0 + cosine) / 2.0).powi(4);
        assert!((pixels[40][0] - (2.0 + specular)).abs() < 1.0e-6);
        assert!((pixels[40][1] - (-0.5 + specular)).abs() < 1.0e-6);
        assert_eq!(pixels[40][3], 0.5);
        assert_eq!(pixels[0], [0.0; 4]);
    }

    #[test]
    fn basic_3d_resamples_coverage_and_colour_together() {
        let mut pixels: Vec<_> = (0..81)
            .map(|index| {
                if index % 9 <= 4 {
                    [1.0, 0.0, 0.0, 0.0]
                } else {
                    [0.0, 0.0, 1.0, 1.0]
                }
            })
            .collect();
        let grade = Basic3DGrade::new(0.0, 0.0, -4.5, 0.0).unwrap();
        assert!(basic_3d_frame_f32(&mut pixels, 9, 9, grade));
        assert_eq!(pixels[4 * 9 + 5], [0.0, 0.0, 1.0, 0.5]);
    }
}
