//! Camera View (TRF-08), a perspective view from an orbit around the source plane.

use glam::{DMat3, Vec2, Vec3};

use crate::coverage::straight_rgba_from_premultiplied;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::premultiply_rgba_controlled;

/// Inclusive angle magnitude in degrees.
pub const CAMERA_VIEW_MAX_ANGLE: f32 = 180.0;
/// Inclusive focal length range in pixels.
pub const CAMERA_VIEW_FOCAL_RANGE: [f32; 2] = [1.0, 5000.0];
/// Inclusive distance range in units of the default orbit radius.
pub const CAMERA_VIEW_DISTANCE_RANGE: [f32; 2] = [0.1, 100.0];
/// Inclusive zoom range in percent.
pub const CAMERA_VIEW_ZOOM_RANGE: [f32; 2] = [1.0, 1000.0];
/// Focal length that preserves scale with the default distance and zoom.
pub const CAMERA_VIEW_DEFAULT_FOCAL: f32 = 500.0;
/// Default orbit radius.
pub const CAMERA_VIEW_DEFAULT_DISTANCE: f32 = 1.0;
/// Default zoom in percent.
pub const CAMERA_VIEW_DEFAULT_ZOOM: f32 = 100.0;
/// Default straight-alpha fill swatch.
pub const CAMERA_VIEW_DEFAULT_FILL: [f32; 4] = [1.0; 4];

const MIN_DEPTH: f32 = 1.0e-5;
const EDGE_EPSILON: f32 = 1.0e-6;

/// An invalid Camera View authoring value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CameraViewError {
    /// A parameter is NaN or infinite.
    #[error("Camera View parameter `{field}` must be finite")]
    NonFinite {
        /// Stable authoring parameter name.
        field: &'static str,
    },
    /// A parameter exceeds its supported range.
    #[error("Camera View parameter `{field}` is out of range")]
    OutOfRange {
        /// Stable authoring parameter name.
        field: &'static str,
    },
}

/// Camera orientation, lens, orbit radius, and straight-alpha fill controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraViewGrade {
    angles: [f32; 3],
    focal_length: f32,
    distance: f32,
    zoom: f32,
    fill_color: [f32; 4],
    fill_alpha: f32,
}

impl CameraViewGrade {
    /// Validates longitude, latitude, and roll in that order, the lens,
    /// orbit radius, and the fill controls.
    pub fn new(
        angles: [f32; 3],
        focal_length: f32,
        distance: f32,
        zoom: f32,
        fill_color: [f32; 4],
        fill_alpha: f32,
    ) -> Result<Self, CameraViewError> {
        let angle_range = [-CAMERA_VIEW_MAX_ANGLE, CAMERA_VIEW_MAX_ANGLE];
        for (field, value, [min, max]) in [
            ("longitude", angles[0], angle_range),
            ("latitude", angles[1], angle_range),
            ("roll", angles[2], angle_range),
            ("focal_length", focal_length, CAMERA_VIEW_FOCAL_RANGE),
            ("distance", distance, CAMERA_VIEW_DISTANCE_RANGE),
            ("zoom", zoom, CAMERA_VIEW_ZOOM_RANGE),
            ("fill_color", fill_color[0], [0.0, 1.0]),
            ("fill_color", fill_color[1], [0.0, 1.0]),
            ("fill_color", fill_color[2], [0.0, 1.0]),
            ("fill_color", fill_color[3], [0.0, 1.0]),
            ("fill_alpha", fill_alpha, [0.0, 1.0]),
        ] {
            if !value.is_finite() {
                return Err(CameraViewError::NonFinite { field });
            }
            if !(min..=max).contains(&value) {
                return Err(CameraViewError::OutOfRange { field });
            }
        }
        Ok(Self {
            angles,
            focal_length,
            distance,
            zoom,
            fill_color,
            fill_alpha,
        })
    }

    /// Raw angle, lens, distance, zoom, and RGBA/alpha values used by the graph fingerprint.
    pub const fn authored_values(self) -> [f32; 11] {
        let [longitude, latitude, roll] = self.angles;
        let [red, green, blue, alpha] = self.fill_color;
        [
            longitude,
            latitude,
            roll,
            self.focal_length,
            self.distance,
            self.zoom,
            red,
            green,
            blue,
            alpha,
            self.fill_alpha,
        ]
    }

    /// Whether the camera has default geometry and never samples the fill colour.
    pub fn is_identity(self) -> bool {
        self.angles == [0.0; 3]
            && self.focal_length == CAMERA_VIEW_DEFAULT_FOCAL
            && self.distance == CAMERA_VIEW_DEFAULT_DISTANCE
            && self.zoom == CAMERA_VIEW_DEFAULT_ZOOM
    }

    fn projection(self, focal_reference: f32) -> CameraProjection {
        let [longitude, latitude, roll] =
            self.angles.map(|degrees| f64::from(degrees).to_radians());
        let rotation = DMat3::from_rotation_y(longitude)
            * DMat3::from_rotation_x(latitude)
            * DMat3::from_rotation_z(roll);
        let lens = (f64::from(CAMERA_VIEW_DEFAULT_FOCAL) / f64::from(self.focal_length))
            * (f64::from(CAMERA_VIEW_DEFAULT_ZOOM) / f64::from(self.zoom));
        let scaled_distance = f64::from(self.distance) * lens;
        let n = rotation.z_axis;
        let horizontal = rotation.x_axis;
        let vertical = rotation.y_axis;
        // Cancel the constant term in the ray/plane intersection before converting to f32.
        let numerator_x = ((horizontal * n.z - n * horizontal.z) * scaled_distance).as_vec3();
        let numerator_y = ((vertical * n.z - n * vertical.z) * scaled_distance).as_vec3();
        let mut fill = self.fill_color;
        fill[3] *= self.fill_alpha;
        if fill[3] <= 0.0 {
            fill = [0.0; 4];
        }
        CameraProjection {
            numerator: [
                Vec2::new(numerator_x.x, numerator_y.x),
                Vec2::new(numerator_x.y, numerator_y.y),
            ],
            denominator: Vec3::new(
                (lens * horizontal.z) as f32,
                (lens * vertical.z) as f32,
                n.z as f32,
            ),
            inverse_focal_reference: focal_reference.recip(),
            edge_tolerance_pixels: EDGE_EPSILON * focal_reference,
            fill,
        }
    }

    /// Packs `params`, `extra`, `aces`, and `extra1` for the actual texture.
    /// Default geometry returns `None` so the source can pass through exactly.
    pub fn gpu_controls(self, width: u32, height: u32) -> Option<[[f32; 4]; 4]> {
        if width == 0 || height == 0 || self.is_identity() {
            return None;
        }
        let projection = self.projection(width.max(height) as f32);
        let [x, y] = projection.numerator;
        Some([
            [x.x, x.y, projection.inverse_focal_reference, MIN_DEPTH],
            [y.x, y.y, projection.edge_tolerance_pixels, 0.0],
            projection.denominator.extend(0.0).to_array(),
            projection.fill,
        ])
    }
}

struct CameraProjection {
    numerator: [Vec2; 2],
    denominator: Vec3,
    inverse_focal_reference: f32,
    edge_tolerance_pixels: f32,
    fill: [f32; 4],
}

impl CameraProjection {
    fn source_offset_at(&self, pixel_offset: Vec2) -> Option<Vec2> {
        let normalized = pixel_offset * self.inverse_focal_reference;
        let depth = self.denominator.dot(normalized.extend(1.0));
        if self.denominator.z <= MIN_DEPTH || depth <= MIN_DEPTH {
            return None;
        }
        let [x, y] = self.numerator;
        // The focal reference cancels in the numerator when the result is in pixels.
        // Keeping pixel offsets here preserves exact integer texel centres.
        Some(Vec2::new(x.dot(pixel_offset), y.dot(pixel_offset)) / depth)
    }
}

/// Reproject a straight-alpha Float32 frame in place. Returns `false` for an invalid grid.
pub fn camera_view_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: CameraViewGrade,
) -> bool {
    apply_camera_view_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

pub(crate) fn apply_camera_view_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: CameraViewGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(pixels.len()) {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }

    checkpoint()?;
    let focal_reference = width.max(height) as f32;
    let projection = grade.projection(focal_reference);
    let centre = Vec2::new(width as f32, height as f32) * 0.5;
    let bounds = centre + Vec2::splat(projection.edge_tolerance_pixels);
    let centre_texel = centre - 0.5;
    let mut source = pixels.to_vec();
    premultiply_rgba_controlled(&mut source, checkpoint)?;
    crate::parallel::for_each_row_band(pixels, width, 0..height, |start, band| {
        for (row, scanline) in band.chunks_mut(width).enumerate() {
            for (column, pixel) in scanline.iter_mut().enumerate() {
                let destination = Vec2::new(column as f32, (start + row) as f32) - centre_texel;
                let Some(source_offset) = projection.source_offset_at(destination) else {
                    *pixel = projection.fill;
                    continue;
                };
                if !source_offset.abs().cmple(bounds).all() {
                    *pixel = projection.fill;
                    continue;
                }
                let texel = centre_texel + source_offset;
                let associated = sample_bilinear_clamped(
                    &source,
                    width,
                    height,
                    f64::from(texel.x),
                    f64::from(texel.y),
                );
                *pixel = straight_rgba_from_premultiplied(associated);
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;

    fn grade(angles: [f32; 3], focal: f32, distance: f32, zoom: f32) -> CameraViewGrade {
        CameraViewGrade::new(angles, focal, distance, zoom, [0.25, 0.5, 0.75, 0.5], 0.5).unwrap()
    }

    fn grid(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| [(index % width) as f32, (index / width) as f32, -0.25, 1.0])
            .collect()
    }

    #[test]
    fn camera_view_validates_all_authoring_fields() {
        let values = [0.0, 0.0, 0.0, 500.0, 1.0, 100.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let from_values =
            |[longitude, latitude, roll, focal, distance, zoom, r, g, b, a, alpha]: [f32; 11]| {
                CameraViewGrade::new(
                    [longitude, latitude, roll],
                    focal,
                    distance,
                    zoom,
                    [r, g, b, a],
                    alpha,
                )
            };
        for (index, (field, invalid)) in [
            ("longitude", 180.1),
            ("latitude", -180.1),
            ("roll", 181.0),
            ("focal_length", 0.0),
            ("distance", 0.05),
            ("zoom", 1001.0),
            ("fill_color", 1.1),
            ("fill_color", -0.1),
            ("fill_color", 1.1),
            ("fill_color", -0.1),
            ("fill_alpha", 1.1),
        ]
        .into_iter()
        .enumerate()
        {
            let mut changed = values;
            changed[index] = invalid;
            assert_eq!(
                from_values(changed),
                Err(CameraViewError::OutOfRange { field })
            );
            changed[index] = f32::INFINITY;
            assert_eq!(
                from_values(changed),
                Err(CameraViewError::NonFinite { field })
            );
            changed[index] = f32::NAN;
            assert_eq!(
                from_values(changed),
                Err(CameraViewError::NonFinite { field })
            );
        }
        for endpoint in [
            [
                -180.0, -180.0, -180.0, 1.0, 0.1, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            ],
            [
                180.0, 180.0, 180.0, 5000.0, 100.0, 1000.0, 1.0, 1.0, 1.0, 1.0, 1.0,
            ],
        ] {
            assert_eq!(from_values(endpoint).unwrap().authored_values(), endpoint);
        }
    }

    #[test]
    fn camera_view_default_geometry_preserves_hidden_rgb_even_with_custom_fill() {
        let grade = grade([0.0; 3], 500.0, 1.0, 100.0);
        let source = [[-0.0, 1.25, -0.25, 0.0]; 9];
        let mut frame = source;
        assert!(grade.is_identity());
        assert!(grade.gpu_controls(3, 3).is_none());
        assert!(camera_view_frame_f32(&mut frame, 3, 3, grade));
        assert_eq!(
            frame.map(|pixel| pixel.map(f32::to_bits)),
            source.map(|pixel| pixel.map(f32::to_bits))
        );
    }

    #[test]
    fn camera_view_focal_distance_and_zoom_change_scale_about_the_centre() {
        let source = grid(9, 9);
        for (focal, distance, zoom) in
            [(250.0, 1.0, 100.0), (500.0, 2.0, 100.0), (500.0, 1.0, 50.0)]
        {
            let mut frame = source.clone();
            assert!(camera_view_frame_f32(
                &mut frame,
                9,
                9,
                grade([0.0; 3], focal, distance, zoom)
            ));
            assert_eq!(frame[40], source[40]);
            assert_eq!(frame[42], source[44]);
            assert_eq!(frame[43], [0.25, 0.5, 0.75, 0.25]);
        }
    }

    #[test]
    fn camera_view_roll_turns_the_image_about_the_optical_axis() {
        let source = grid(9, 9);
        let mut frame = source.clone();
        assert!(camera_view_frame_f32(
            &mut frame,
            9,
            9,
            grade([0.0, 0.0, 90.0], 500.0, 1.0, 100.0)
        ));
        assert_eq!(frame[41], source[49]);
        assert_eq!(frame[49], source[39]);
    }

    #[test]
    fn camera_view_projection_matches_an_independent_ray_plane_intersection() {
        let [longitude, latitude, roll] = [33.0_f64, -21.0, 15.0];
        let (sin_lon, cos_lon) = longitude.to_radians().sin_cos();
        let (sin_lat, cos_lat) = latitude.to_radians().sin_cos();
        let (sin_roll, cos_roll) = roll.to_radians().sin_cos();
        let rotate = |ray: DVec3| {
            let x = cos_roll * ray.x - sin_roll * ray.y;
            let y = sin_roll * ray.x + cos_roll * ray.y;
            let z = sin_lat * y + cos_lat * ray.z;
            DVec3::new(
                cos_lon * x + sin_lon * z,
                cos_lat * y - sin_lat * ray.z,
                -sin_lon * x + cos_lon * z,
            )
        };
        let grade = grade(
            [longitude as f32, latitude as f32, roll as f32],
            625.0,
            2.0,
            125.0,
        );
        let projection = grade.projection(53.0);
        let eye = -2.0 * rotate(DVec3::Z);
        for (column, row) in [(0, 0), (39, 17), (26, 15), (52, 30)] {
            let pixel_offset = Vec2::new(column as f32 - 26.0, row as f32 - 15.0);
            let ray = rotate(DVec3::new(
                f64::from(pixel_offset.x) * (0.64 / 53.0),
                f64::from(pixel_offset.y) * (0.64 / 53.0),
                1.0,
            ));
            let hit = eye - (eye.z / ray.z) * ray;
            let actual = projection.source_offset_at(pixel_offset).unwrap() / 53.0;
            assert!((actual - hit.as_vec3().truncate()).length() < 5.0e-7);
        }
    }

    #[test]
    fn camera_view_fills_the_horizon_and_back_face() {
        for angles in [[90.0, 0.0, 0.0], [0.0, -90.0, 0.0], [180.0, 0.0, 0.0]] {
            let grade = grade(angles, 500.0, 1.0, 100.0);
            let mut frame = grid(7, 5);
            assert!(camera_view_frame_f32(&mut frame, 7, 5, grade));
            assert!(frame.iter().all(|&pixel| pixel == [0.25, 0.5, 0.75, 0.25]));
        }
        let projection = grade([60.0, 0.0, 0.0], 250.0, 1.0, 100.0).projection(9.0);
        assert!(projection.source_offset_at(Vec2::new(0.25 * 9.0, 0.0)).is_some());
        assert!(projection.source_offset_at(Vec2::new(0.29 * 9.0, 0.0)).is_none());
        let horizon = -projection.denominator.z / projection.denominator.x;
        assert!(projection.source_offset_at(Vec2::new(horizon * 9.0, 0.0)).is_none());
    }

    #[test]
    fn camera_view_keeps_the_exact_texel_centre_on_an_unrotated_axis() {
        let grade = grade([60.0, 0.0, 0.0], 250.0, 1.0, 100.0);
        let projection = grade.projection(31.0);
        assert_eq!(
            projection.source_offset_at(Vec2::new(0.0, -4.0)),
            Some(Vec2::new(0.0, -8.0))
        );
        let mut frame = vec![[1.25, 0.75, -0.25, 1.0 / 65_535.0]; 31 * 17];
        frame[15] = [1.25, 0.75, -0.25, 0.0];
        assert!(camera_view_frame_f32(&mut frame, 31, 17, grade));
        assert_eq!(frame[4 * 31 + 15], [0.0; 4]);
    }

    #[test]
    fn camera_view_creates_coverage_only_outside_the_source_rectangle() {
        let mut frame = [[-0.25, 0.75, 1.25, 0.0]; 81];
        assert!(camera_view_frame_f32(
            &mut frame,
            9,
            9,
            grade([0.0; 3], 500.0, 2.0, 100.0)
        ));
        assert_eq!(frame[40], [0.0; 4]);
        assert_eq!(frame[0], [0.25, 0.5, 0.75, 0.25]);
        let transparent = CameraViewGrade::new([0.0; 3], 500.0, 2.0, 100.0, [1.0; 4], 0.0).unwrap();
        let mut frame = [[-0.25, 0.75, 1.25, 0.0]; 81];
        assert!(camera_view_frame_f32(&mut frame, 9, 9, transparent));
        assert_eq!(frame, [[0.0; 4]; 81]);
    }

    #[test]
    fn camera_view_clamps_the_rectangular_edge_to_the_last_texel() {
        let source = grid(9, 5);
        let mut frame = source.clone();
        assert!(camera_view_frame_f32(
            &mut frame,
            9,
            5,
            grade([0.0; 3], 500.0, 1.125, 100.0)
        ));
        assert_eq!(frame[26], source[26]);
        assert!(camera_view_frame_f32(
            &mut frame,
            9,
            5,
            grade([0.0; 3], 500.0, 1.126, 100.0)
        ));
        assert_eq!(frame[26], [0.25, 0.5, 0.75, 0.25]);
    }

    #[test]
    fn camera_view_resamples_premultiplied_hdr_on_a_single_row() {
        let mut frame = [[9.0, 9.0, 9.0, 0.0]; 5];
        frame[1] = [1.25, -0.5, 0.25, 0.25];
        let grade = grade([0.0; 3], 500.0, 0.5, 100.0);
        assert!(camera_view_frame_f32(&mut frame, 5, 1, grade));
        assert_eq!(frame[1], [1.25, -0.5, 0.25, 0.125]);
        assert_eq!(frame[2], [0.0; 4]);
        assert!(!camera_view_frame_f32(&mut frame, 1, 2, grade));
        assert!(!camera_view_frame_f32(&mut [], 0, 0, grade));
    }
}
