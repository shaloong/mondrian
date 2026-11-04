//! VR project 2D (VR3D-12 in the effect list).
//!
//! The effect projects a flat card into an equirectangular panorama: the
//! destination frame is the 360 x 180 sphere, every destination pixel maps to a
//! direction on that sphere, the direction is rotated into the patch frame by
//! the authored projection pose, the patch plane is intersected, and the
//! intersection point is looked up on a source card that itself carries the
//! authored source pose.
//!
//! The conventions match the rotate-sphere and sphere-to-plane kernels exactly:
//!
//! - right handed, `+X` right, `+Y` up, `+Z` forward, so a direction with
//!   longitude `lambda` and latitude `phi` is
//!   `d = (cos(phi) sin(lambda), sin(phi), cos(phi) cos(lambda))`;
//! - the rotations are `R = Rz(roll) * Ry(yaw) * Rx(pitch)`; the destination
//!   direction is rotated into the patch frame by `R_p^T` and the patch point is
//!   rotated into the card frame by `R_s^T`, both real transposes;
//! - a pixel is written only when the destination direction is in front of the
//!   patch (`q.z > 0`), inside the square patch (`|a| <= h`, `|b| <= h`), and in
//!   front of the source card (`s.z > 0`). Every skipped pixel is left
//!   bit-identical, which is the strongest invariant of the effect;
//! - `h = tan(scale / 2)`, clamped to at least `1e-6` with the same literal on
//!   the CPU and in the shader.
//!
//! The alpha model follows the bezier warp: the straight frame is snapshotted,
//! a **copy** is premultiplied, the copy is sampled, and each written pixel is
//! converted back to straight exactly once before being composited over the
//! straight source pixel. A whole-frame premultiply / unpremultiply round trip
//! would zero the hidden colour of fully transparent pixels.
//!
//! There is **no identity state**: a 60 degree card at the centre of the frame
//! is the normal result of the default parameters, so the graph builder always
//! appends the node. [`VrProject2DGrade::placement_is_identity`] only reports
//! whether the six authored angles are zero; it never suppresses the pass.

use crate::adjustment::blend_rgba_f32_pixel;
use crate::coverage::straight_rgba_from_premultiplied;
use crate::parallel::for_each_row_band;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::premultiply_rgba_controlled;
use crate::vr_rotate_sphere::{wrap_degrees, VrFrameLayout};
use mondrian_core::types::BlendMode;

/// Smallest horizontal field of view, in degrees.
pub const VR_PROJECT_2D_MIN_HORIZONTAL_FOV_DEGREES: f32 = 60.0;

/// Largest horizontal field of view, in degrees.
pub const VR_PROJECT_2D_MAX_HORIZONTAL_FOV_DEGREES: f32 = 360.0;

/// Smallest vertical field of view, in degrees.
pub const VR_PROJECT_2D_MIN_VERTICAL_FOV_DEGREES: f32 = 60.0;

/// Largest vertical field of view, in degrees.
pub const VR_PROJECT_2D_MAX_VERTICAL_FOV_DEGREES: f32 = 180.0;

/// Smallest patch scale, in degrees.
pub const VR_PROJECT_2D_MIN_SCALE_DEGREES: f32 = 0.0;

/// Largest patch scale, in degrees.
pub const VR_PROJECT_2D_MAX_SCALE_DEGREES: f32 = 180.0;

/// Smallest stereo disparity, in degrees.
pub const VR_PROJECT_2D_MIN_STEREO_DISPARITY_DEGREES: f32 = 0.0;

/// Largest stereo disparity, in degrees.
pub const VR_PROJECT_2D_MAX_STEREO_DISPARITY_DEGREES: f32 = 45.0;

/// Smallest feather, in percent.
pub const VR_PROJECT_2D_MIN_FEATHER_PERCENT: f32 = 0.0;

/// Largest feather, in percent.
pub const VR_PROJECT_2D_MAX_FEATHER_PERCENT: f32 = 100.0;

/// Default horizontal field of view, in degrees.
pub const VR_PROJECT_2D_DEFAULT_HORIZONTAL_FOV_DEGREES: f32 = 360.0;

/// Default vertical field of view, in degrees.
pub const VR_PROJECT_2D_DEFAULT_VERTICAL_FOV_DEGREES: f32 = 180.0;

/// Default patch scale, in degrees.
pub const VR_PROJECT_2D_DEFAULT_SCALE_DEGREES: f32 = 60.0;

/// Default stereo disparity, in degrees.
pub const VR_PROJECT_2D_DEFAULT_STEREO_DISPARITY_DEGREES: f32 = 0.0;

/// Default feather, in percent.
pub const VR_PROJECT_2D_DEFAULT_FEATHER_PERCENT: f32 = 10.0;

/// Floor of the patch half extent `h = tan(scale / 2)`.
///
/// The same literal is used by the shader so a zero scale degenerates to the
/// smallest patch instead of a division by zero.
pub const VR_PROJECT_2D_PATCH_HALF_EXTENT_FLOOR: f64 = 1.0e-6;

/// Why a [`VrProject2DGrade`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VrProject2DError {
    /// The horizontal field of view is outside its authored range.
    HorizontalFov,
    /// The vertical field of view is outside its authored range.
    VerticalFov,
    /// The patch scale is outside its authored range.
    Scale,
    /// The stereo disparity is outside its authored range.
    StereoDisparity,
    /// The feather is outside its authored range.
    Feather,
    /// One of the six pose angles is not finite.
    Angle,
}

impl std::fmt::Display for VrProject2DError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HorizontalFov => write!(
                formatter,
                "horizontal field of view must be between {} and {} degrees",
                VR_PROJECT_2D_MIN_HORIZONTAL_FOV_DEGREES, VR_PROJECT_2D_MAX_HORIZONTAL_FOV_DEGREES
            ),
            Self::VerticalFov => write!(
                formatter,
                "vertical field of view must be between {} and {} degrees",
                VR_PROJECT_2D_MIN_VERTICAL_FOV_DEGREES, VR_PROJECT_2D_MAX_VERTICAL_FOV_DEGREES
            ),
            Self::Scale => write!(
                formatter,
                "scale must be between {} and {} degrees",
                VR_PROJECT_2D_MIN_SCALE_DEGREES, VR_PROJECT_2D_MAX_SCALE_DEGREES
            ),
            Self::StereoDisparity => write!(
                formatter,
                "stereo disparity must be between {} and {} degrees",
                VR_PROJECT_2D_MIN_STEREO_DISPARITY_DEGREES,
                VR_PROJECT_2D_MAX_STEREO_DISPARITY_DEGREES
            ),
            Self::Feather => write!(
                formatter,
                "feather must be between {} and {} percent",
                VR_PROJECT_2D_MIN_FEATHER_PERCENT, VR_PROJECT_2D_MAX_FEATHER_PERCENT
            ),
            Self::Angle => write!(formatter, "pose angles must be finite"),
        }
    }
}

impl std::error::Error for VrProject2DError {}

/// Uniform packing for the GPU pass: `(params, extra, aces, extra1)`.
pub type VrProject2DControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// Authored parameters of the VR project 2D pass.
///
/// The values are exactly the authored degrees; the trigonometric projections
/// are derived on demand so the grade stays cheap to hash and compare.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VrProject2DGrade {
    horizontal_fov_degrees: f64,
    vertical_fov_degrees: f64,
    scale_degrees: f64,
    stereo_disparity_degrees: f64,
    feather_percent: f64,
    source_tilt_x_degrees: f64,
    source_pan_y_degrees: f64,
    source_roll_z_degrees: f64,
    projection_tilt_x_degrees: f64,
    projection_pan_y_degrees: f64,
    projection_roll_z_degrees: f64,
    layout: VrFrameLayout,
    patch_half_extent: f64,
    tan_stereo_disparity: f64,
}

impl VrProject2DGrade {
    /// Builds a grade, validating every authored range.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        horizontal_fov_degrees: f64,
        vertical_fov_degrees: f64,
        scale_degrees: f64,
        stereo_disparity_degrees: f64,
        feather_percent: f64,
        source_tilt_x_degrees: f64,
        source_pan_y_degrees: f64,
        source_roll_z_degrees: f64,
        projection_tilt_x_degrees: f64,
        projection_pan_y_degrees: f64,
        projection_roll_z_degrees: f64,
        layout: VrFrameLayout,
    ) -> Result<Self, VrProject2DError> {
        if !in_range(
            horizontal_fov_degrees,
            VR_PROJECT_2D_MIN_HORIZONTAL_FOV_DEGREES,
            VR_PROJECT_2D_MAX_HORIZONTAL_FOV_DEGREES,
        ) {
            return Err(VrProject2DError::HorizontalFov);
        }
        if !in_range(
            vertical_fov_degrees,
            VR_PROJECT_2D_MIN_VERTICAL_FOV_DEGREES,
            VR_PROJECT_2D_MAX_VERTICAL_FOV_DEGREES,
        ) {
            return Err(VrProject2DError::VerticalFov);
        }
        if !in_range(
            scale_degrees,
            VR_PROJECT_2D_MIN_SCALE_DEGREES,
            VR_PROJECT_2D_MAX_SCALE_DEGREES,
        ) {
            return Err(VrProject2DError::Scale);
        }
        if !in_range(
            stereo_disparity_degrees,
            VR_PROJECT_2D_MIN_STEREO_DISPARITY_DEGREES,
            VR_PROJECT_2D_MAX_STEREO_DISPARITY_DEGREES,
        ) {
            return Err(VrProject2DError::StereoDisparity);
        }
        if !in_range(
            feather_percent,
            VR_PROJECT_2D_MIN_FEATHER_PERCENT,
            VR_PROJECT_2D_MAX_FEATHER_PERCENT,
        ) {
            return Err(VrProject2DError::Feather);
        }
        let angles = [
            source_tilt_x_degrees,
            source_pan_y_degrees,
            source_roll_z_degrees,
            projection_tilt_x_degrees,
            projection_pan_y_degrees,
            projection_roll_z_degrees,
        ];
        if angles.iter().any(|angle| !angle.is_finite()) {
            return Err(VrProject2DError::Angle);
        }
        Ok(Self {
            horizontal_fov_degrees,
            vertical_fov_degrees,
            scale_degrees,
            stereo_disparity_degrees,
            feather_percent,
            source_tilt_x_degrees: wrap_degrees(source_tilt_x_degrees),
            source_pan_y_degrees: wrap_degrees(source_pan_y_degrees),
            source_roll_z_degrees: wrap_degrees(source_roll_z_degrees),
            projection_tilt_x_degrees: wrap_degrees(projection_tilt_x_degrees),
            projection_pan_y_degrees: wrap_degrees(projection_pan_y_degrees),
            projection_roll_z_degrees: wrap_degrees(projection_roll_z_degrees),
            layout,
            patch_half_extent: (scale_degrees * 0.5)
                .to_radians()
                .tan()
                .max(VR_PROJECT_2D_PATCH_HALF_EXTENT_FLOOR),
            tan_stereo_disparity: stereo_disparity_degrees.to_radians().tan(),
        })
    }

    /// Authored horizontal field of view, in degrees.
    pub fn horizontal_fov_degrees(&self) -> f64 {
        self.horizontal_fov_degrees
    }

    /// Authored vertical field of view, in degrees.
    pub fn vertical_fov_degrees(&self) -> f64 {
        self.vertical_fov_degrees
    }

    /// Authored patch scale, in degrees.
    pub fn scale_degrees(&self) -> f64 {
        self.scale_degrees
    }

    /// Authored stereo disparity, in degrees.
    pub fn stereo_disparity_degrees(&self) -> f64 {
        self.stereo_disparity_degrees
    }

    /// Authored feather, in percent.
    pub fn feather_percent(&self) -> f64 {
        self.feather_percent
    }

    /// Authored source tilt, in degrees.
    pub fn source_tilt_x_degrees(&self) -> f64 {
        self.source_tilt_x_degrees
    }

    /// Authored source pan, in degrees.
    pub fn source_pan_y_degrees(&self) -> f64 {
        self.source_pan_y_degrees
    }

    /// Authored source roll, in degrees.
    pub fn source_roll_z_degrees(&self) -> f64 {
        self.source_roll_z_degrees
    }

    /// Authored projection tilt, in degrees.
    pub fn projection_tilt_x_degrees(&self) -> f64 {
        self.projection_tilt_x_degrees
    }

    /// Authored projection pan, in degrees.
    pub fn projection_pan_y_degrees(&self) -> f64 {
        self.projection_pan_y_degrees
    }

    /// Authored projection roll, in degrees.
    pub fn projection_roll_z_degrees(&self) -> f64 {
        self.projection_roll_z_degrees
    }

    /// Authored stereo layout.
    pub fn layout(&self) -> VrFrameLayout {
        self.layout
    }

    /// Half extent of the square patch, `tan(scale / 2)` clamped to the floor.
    pub fn patch_half_extent(&self) -> f64 {
        self.patch_half_extent
    }

    /// Whether the six authored pose angles are all zero.
    ///
    /// This is **not** an identity test: a zero pose still projects the default
    /// 60 degree card, so the graph builder always appends the pass.
    pub fn placement_is_identity(&self) -> bool {
        self.source_tilt_x_degrees == 0.0
            && self.source_pan_y_degrees == 0.0
            && self.source_roll_z_degrees == 0.0
            && self.projection_tilt_x_degrees == 0.0
            && self.projection_pan_y_degrees == 0.0
            && self.projection_roll_z_degrees == 0.0
    }

    /// Packs the twelve uniform scalars used by the spatial pass.
    pub fn gpu_controls(&self) -> VrProject2DControls {
        (
            [
                self.horizontal_fov_degrees.to_radians() as f32,
                self.vertical_fov_degrees.to_radians() as f32,
                self.patch_half_extent as f32,
                self.tan_stereo_disparity as f32,
            ],
            [
                self.source_tilt_x_degrees.to_radians() as f32,
                self.source_pan_y_degrees.to_radians() as f32,
                self.source_roll_z_degrees.to_radians() as f32,
                (self.feather_percent / 100.0) as f32,
            ],
            [0.0; 4],
            [
                self.projection_tilt_x_degrees.to_radians() as f32,
                self.projection_pan_y_degrees.to_radians() as f32,
                self.projection_roll_z_degrees.to_radians() as f32,
                self.layout.code(),
            ],
        )
    }

    /// Direction of a destination pixel on the panorama sphere.
    pub fn target_direction(&self, column: f64, row: f64, width: usize, height: usize) -> [f64; 3] {
        if width == 0 || height == 0 {
            return [0.0, 0.0, 1.0];
        }
        let u = (column + 0.5) / width as f64;
        let v = (row + 0.5) / height as f64;
        let longitude = (u - 0.5) * self.horizontal_fov_degrees.to_radians();
        let latitude = (0.5 - v) * self.vertical_fov_degrees.to_radians();
        let (sin_longitude, cos_longitude) = longitude.sin_cos();
        let (sin_latitude, cos_latitude) = latitude.sin_cos();
        [
            cos_latitude * sin_longitude,
            sin_latitude,
            cos_latitude * cos_longitude,
        ]
    }

    /// Patch plane coordinates of a destination pixel, if it hits the patch.
    pub fn patch_coordinates(
        &self,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> Option<(f64, f64)> {
        let direction = self.target_direction(column, row, width, height);
        VrProject2DGeometry::new(self).patch_coordinates(direction)
    }

    /// Source pixel coordinates of a destination pixel, if it hits the patch.
    ///
    /// The coordinates are expressed in the same pixel space as `column` and
    /// `row` and are not clamped; the kernel clamps them to the source frame.
    pub fn source_position(
        &self,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> Option<(f64, f64)> {
        let direction = self.target_direction(column, row, width, height);
        let geometry = VrProject2DGeometry::new(self);
        let (patch_x, patch_y) = geometry.patch_coordinates(direction)?;
        let (u_source, v_source) = geometry.source_coordinates(patch_x, patch_y)?;
        Some((
            u_source * width as f64 - 0.5,
            v_source * height as f64 - 0.5,
        ))
    }

    /// Feather weight of a patch plane point.
    ///
    /// The band is measured on the **destination** patch coordinates: the patch
    /// is square, so the normalized radius is `max(|a|, |b|) / h`, not the
    /// Euclidean length.
    pub fn feather_weight(&self, patch_x: f64, patch_y: f64) -> f64 {
        let radius = patch_x.abs().max(patch_y.abs()) / self.patch_half_extent;
        let edge = 1.0 - radius;
        if edge <= 0.0 {
            return 0.0;
        }
        let fraction = self.feather_percent / 100.0;
        if fraction <= 0.0 {
            return 1.0;
        }
        let t = (edge / fraction).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

/// Precomputed rotation and trigonometry shared by every pixel of one pass.
#[derive(Debug, Clone, Copy)]
struct VrProject2DGeometry {
    projection_sampling: [[f64; 3]; 3],
    source_sampling: [[f64; 3]; 3],
    patch_half_extent: f64,
    tan_stereo_disparity: f64,
}

impl VrProject2DGeometry {
    fn new(grade: &VrProject2DGrade) -> Self {
        let projection = rotation_matrix(
            grade.projection_tilt_x_degrees.to_radians(),
            grade.projection_pan_y_degrees.to_radians(),
            grade.projection_roll_z_degrees.to_radians(),
        );
        let source = rotation_matrix(
            grade.source_tilt_x_degrees.to_radians(),
            grade.source_pan_y_degrees.to_radians(),
            grade.source_roll_z_degrees.to_radians(),
        );
        Self {
            projection_sampling: transpose(projection),
            source_sampling: transpose(source),
            patch_half_extent: grade.patch_half_extent,
            tan_stereo_disparity: grade.tan_stereo_disparity,
        }
    }

    fn patch_coordinates(&self, direction: [f64; 3]) -> Option<(f64, f64)> {
        let q = transform(self.projection_sampling, direction);
        if q[2] <= 0.0 {
            return None;
        }
        let patch_x = q[0] / q[2];
        let patch_y = q[1] / q[2];
        if patch_x.abs() > self.patch_half_extent || patch_y.abs() > self.patch_half_extent {
            return None;
        }
        Some((patch_x, patch_y))
    }

    fn source_coordinates(&self, patch_x: f64, patch_y: f64) -> Option<(f64, f64)> {
        let s = transform(self.source_sampling, [patch_x, patch_y, 1.0]);
        if s[2] <= 0.0 {
            return None;
        }
        let a_source = s[0] / s[2];
        let b_source = s[1] / s[2];
        let denominator = 2.0 * self.patch_half_extent;
        let u_source = 0.5 + (a_source - self.tan_stereo_disparity) / denominator;
        let v_source = 0.5 - b_source / denominator;
        Some((u_source, v_source))
    }
}

/// Row-major `R = Rz(roll) * Ry(yaw) * Rx(pitch)`.
fn rotation_matrix(pitch: f64, yaw: f64, roll: f64) -> [[f64; 3]; 3] {
    let (sin_pitch, cos_pitch) = pitch.sin_cos();
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    let (sin_roll, cos_roll) = roll.sin_cos();
    [
        [
            cos_roll * cos_yaw,
            cos_roll * sin_yaw * sin_pitch - sin_roll * cos_pitch,
            cos_roll * sin_yaw * cos_pitch + sin_roll * sin_pitch,
        ],
        [
            sin_roll * cos_yaw,
            sin_roll * sin_yaw * sin_pitch + cos_roll * cos_pitch,
            sin_roll * sin_yaw * cos_pitch - cos_roll * sin_pitch,
        ],
        [-sin_yaw, cos_yaw * sin_pitch, cos_yaw * cos_pitch],
    ]
}

fn transpose(matrix: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    [
        [matrix[0][0], matrix[1][0], matrix[2][0]],
        [matrix[0][1], matrix[1][1], matrix[2][1]],
        [matrix[0][2], matrix[1][2], matrix[2][2]],
    ]
}

fn transform(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

fn in_range(value: f64, minimum: f32, maximum: f32) -> bool {
    value.is_finite() && value >= f64::from(minimum) && value <= f64::from(maximum)
}

fn eye_origins(layout: VrFrameLayout, width: usize, height: usize) -> ([(usize, usize); 2], usize) {
    let (eye_width, eye_height) = layout.eye_size(width, height);
    match layout {
        VrFrameLayout::Monoscopic => ([(0, 0), (0, 0)], 1),
        VrFrameLayout::OverUnder => ([(0, 0), (0, eye_height)], 2),
        VrFrameLayout::SideBySide => ([(0, 0), (eye_width, 0)], 2),
    }
}

/// Applies the projection to a whole frame, returning `false` when the frame
/// cannot be processed.
pub fn vr_project_2d_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: VrProject2DGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_vr_project_2d_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the projection in place with an explicit cancellation checkpoint.
///
/// The kernel snapshots the straight frame, premultiplies a copy, and samples
/// that copy; every written pixel is converted back to straight exactly once and
/// composited over the straight snapshot. Each eye of a stereo layout is walked
/// independently, because the patch shape is identical but the background
/// differs.
pub(crate) fn apply_vr_project_2d_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: VrProject2DGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || width == 0 || height == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    let (eye_width, eye_height) = grade.layout.eye_size(width, height);
    if eye_width == 0 || eye_height == 0 {
        return Ok(false);
    }

    let frame = &mut working[..pixel_count];
    let source = frame.to_vec();
    let mut staged = source.clone();
    premultiply_rgba_controlled(&mut staged, checkpoint)?;
    checkpoint()?;

    let geometry = VrProject2DGeometry::new(&grade);
    let (origins, eye_count) = eye_origins(grade.layout, width, height);

    for &(origin_x, origin_y) in &origins[..eye_count] {
        let maximum_x = (origin_x + eye_width - 1) as f64;
        let maximum_y = (origin_y + eye_height - 1) as f64;
        let minimum_x = origin_x as f64;
        let minimum_y = origin_y as f64;
        for_each_row_band(
            frame,
            width,
            origin_y..origin_y + eye_height,
            |row_start, band| {
                for (offset, chunk) in band.chunks_mut(width).enumerate() {
                    let row = row_start + offset;
                    let local_row = row - origin_y;
                    for local_column in 0..eye_width {
                        let column = origin_x + local_column;
                        let direction = grade.target_direction(
                            local_column as f64,
                            local_row as f64,
                            eye_width,
                            eye_height,
                        );
                        let Some((patch_x, patch_y)) = geometry.patch_coordinates(direction) else {
                            continue;
                        };
                        let weight = grade.feather_weight(patch_x, patch_y);
                        if weight <= 0.0 {
                            continue;
                        }
                        let Some((u_source, v_source)) =
                            geometry.source_coordinates(patch_x, patch_y)
                        else {
                            continue;
                        };
                        let sample_x = (minimum_x + u_source * eye_width as f64 - 0.5)
                            .clamp(minimum_x, maximum_x);
                        let sample_y = (minimum_y + v_source * eye_height as f64 - 0.5)
                            .clamp(minimum_y, maximum_y);
                        let sampled =
                            sample_bilinear_clamped(&staged, width, height, sample_x, sample_y);
                        let patch = straight_rgba_from_premultiplied(sampled);
                        let base = source[row * width + column];
                        chunk[column] =
                            blend_rgba_f32_pixel(base, patch, weight as f32, BlendMode::Normal);
                    }
                }
            },
        );
        checkpoint()?;
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: usize = 360;
    const HEIGHT: usize = 180;

    fn grade(
        horizontal_fov: f64,
        vertical_fov: f64,
        scale: f64,
        stereo_disparity: f64,
        feather: f64,
    ) -> VrProject2DGrade {
        VrProject2DGrade::new(
            horizontal_fov,
            vertical_fov,
            scale,
            stereo_disparity,
            feather,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
        )
        .expect("the grade must be valid")
    }

    /// Gradient frame with a few deliberately hostile pixels: hidden colour
    /// under zero alpha, negative channels and alpha above one.
    fn projection_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let x = column as f32 / width.max(1) as f32;
                let y = row as f32 / height.max(1) as f32;
                pixels[row * width + column] = [x, y, 0.25 + 0.5 * x, 1.0];
            }
        }
        if pixels.len() > 3 {
            pixels[0] = [0.75, -0.25, 1.5, 0.0];
            pixels[1] = [0.0, 0.0, 0.0, 1.0];
            pixels[2] = [4.0, -3.0, 2.0, 0.5];
        }
        pixels
    }

    fn bits(pixels: &[[f32; 4]]) -> Vec<[u32; 4]> {
        pixels
            .iter()
            .map(|pixel| {
                [
                    pixel[0].to_bits(),
                    pixel[1].to_bits(),
                    pixel[2].to_bits(),
                    pixel[3].to_bits(),
                ]
            })
            .collect()
    }

    fn angle_between(left: [f64; 3], right: [f64; 3]) -> f64 {
        let dot = left[0] * right[0] + left[1] * right[1] + left[2] * right[2];
        dot.clamp(-1.0, 1.0).acos()
    }

    #[test]
    fn the_patch_centre_maps_to_the_source_centre() {
        let grade = grade(360.0, 180.0, 60.0, 0.0, 0.0);
        let column = WIDTH as f64 / 2.0 - 0.5;
        let row = HEIGHT as f64 / 2.0 - 0.5;
        let (source_x, source_y) = grade
            .source_position(column, row, WIDTH, HEIGHT)
            .expect("the frame centre is inside the default patch");
        assert!((source_x - column).abs() <= 0.5);
        assert!((source_y - row).abs() <= 0.5);

        // A tilted projection moves the patch, but the patch centre still maps
        // to the source centre: it is the direction `R_p * e_z`.
        let tilted = VrProject2DGrade::new(
            360.0,
            180.0,
            60.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            20.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
        )
        .expect("valid tilted grade");
        let row = (0.5 + 20.0 / 180.0) * HEIGHT as f64 - 0.5;
        let (source_x, source_y) = tilted
            .source_position(column, row, WIDTH, HEIGHT)
            .expect("the tilted patch centre is inside the patch");
        assert!((source_x - column).abs() < 1e-9);
        assert!((source_y - HEIGHT as f64 / 2.0 + 0.5).abs() < 1e-9);
    }

    #[test]
    fn everything_outside_the_patch_is_bit_exact() {
        let grade = grade(360.0, 180.0, 60.0, 0.0, 0.0);
        let source = projection_frame(WIDTH, HEIGHT);
        let mut output = source.clone();
        assert!(vr_project_2d_frame_f32(&mut output, WIDTH, HEIGHT, grade));
        for (column, row) in [(0, 0), (20, 90), (90, 30), (100, 100), (359, 179)] {
            assert!(
                grade.patch_coordinates(column as f64, row as f64, WIDTH, HEIGHT).is_none(),
                "pixel ({column}, {row}) must lie outside the patch"
            );
            assert_eq!(
                bits(&output)[row * WIDTH + column],
                bits(&source)[row * WIDTH + column],
                "pixel ({column}, {row}) must stay bit-identical"
            );
        }
        assert_ne!(
            bits(&output)[90 * WIDTH + 180],
            bits(&source)[90 * WIDTH + 180],
            "the patch centre must be rewritten"
        );
    }

    #[test]
    fn the_patch_covers_the_authored_angular_width() {
        let grade = grade(360.0, 180.0, 60.0, 0.0, 0.0);
        let half_extent = grade.patch_half_extent();
        // With a 360 degree horizontal field of view, longitude is linear in the
        // column, so the patch edges sit at a longitude of +/- scale / 2.
        let left = (0.5 - 30.0 / 360.0) * WIDTH as f64 - 0.5;
        let right = (0.5 + 30.0 / 360.0) * WIDTH as f64 - 0.5;
        // A hundredth of a pixel inside the geometric edge is unambiguous; the
        // exact edge compares two equivalent tangents and may round to either
        // side of the skip test.
        let (left_x, left_y) = grade
            .patch_coordinates(left + 0.01, 89.5, WIDTH, HEIGHT)
            .expect("just inside the left patch edge is included");
        let (right_x, right_y) = grade
            .patch_coordinates(right - 0.01, 89.5, WIDTH, HEIGHT)
            .expect("just inside the right patch edge is included");
        assert!((left_x + half_extent).abs() < 1e-3);
        assert!((right_x - half_extent).abs() < 1e-3);
        assert!(left_y.abs() < 1e-3);
        assert!(right_y.abs() < 1e-3);
        assert!(grade.patch_coordinates(left - 0.01, 89.5, WIDTH, HEIGHT).is_none());
        assert!(grade.patch_coordinates(right + 0.01, 89.5, WIDTH, HEIGHT).is_none());
        let left_direction = grade.target_direction(left, 89.5, WIDTH, HEIGHT);
        let right_direction = grade.target_direction(right, 89.5, WIDTH, HEIGHT);
        assert!((angle_between(left_direction, right_direction).to_degrees() - 60.0).abs() < 1e-9);

        // The patch is square: the top and bottom edges sit at the same
        // angular distance from the centre.
        let (_, top_y) = grade
            .patch_coordinates(179.5, 59.51, WIDTH, HEIGHT)
            .expect("just inside the top patch edge is included");
        let (_, bottom_y) = grade
            .patch_coordinates(179.5, 119.49, WIDTH, HEIGHT)
            .expect("just inside the bottom patch edge is included");
        assert!((top_y - half_extent).abs() < 1e-3);
        assert!((bottom_y + half_extent).abs() < 1e-3);
    }

    #[test]
    fn a_zero_scale_patch_is_invisible() {
        let grade = grade(360.0, 180.0, 0.0, 0.0, 0.0);
        let source = projection_frame(WIDTH, HEIGHT);
        let mut output = source.clone();
        assert!(vr_project_2d_frame_f32(&mut output, WIDTH, HEIGHT, grade));
        assert_eq!(bits(&output), bits(&source));
    }

    #[test]
    fn a_patch_behind_the_viewer_is_invisible() {
        // A narrow destination field of view only contains the front of the
        // sphere; a patch placed at pan 180 degrees faces the other way and has
        // no destination direction in front of it.
        let grade = VrProject2DGrade::new(
            60.0,
            60.0,
            60.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            180.0,
            0.0,
            VrFrameLayout::Monoscopic,
        )
        .expect("valid grade");
        let source = projection_frame(WIDTH, HEIGHT);
        let mut output = source.clone();
        assert!(vr_project_2d_frame_f32(&mut output, WIDTH, HEIGHT, grade));
        assert_eq!(bits(&output), bits(&source));
    }

    #[test]
    fn stereo_disparity_shifts_the_content_without_moving_the_patch() {
        let base = grade(360.0, 180.0, 60.0, 0.0, 0.0);
        let shifted = VrProject2DGrade::new(
            360.0,
            180.0,
            60.0,
            10.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
        )
        .expect("valid grade");
        let mut source = projection_frame(WIDTH, HEIGHT);
        for (row, line) in source.chunks_mut(WIDTH).enumerate() {
            for (column, pixel) in line.iter_mut().enumerate() {
                pixel[0] = column as f32 / (WIDTH - 1) as f32;
                pixel[1] = row as f32 / (HEIGHT - 1) as f32;
                pixel[2] = 0.5;
                pixel[3] = 1.0;
            }
        }
        let mut left = source.clone();
        let mut right = source.clone();
        assert!(vr_project_2d_frame_f32(&mut left, WIDTH, HEIGHT, base));
        assert!(vr_project_2d_frame_f32(&mut right, WIDTH, HEIGHT, shifted));

        // The patch position is unaffected by the disparity.
        for row in (0..HEIGHT).step_by(9) {
            for column in (0..WIDTH).step_by(9) {
                assert_eq!(
                    base.patch_coordinates(column as f64, row as f64, WIDTH, HEIGHT).is_some(),
                    shifted.patch_coordinates(column as f64, row as f64, WIDTH, HEIGHT).is_some(),
                    "the patch mask must not depend on the disparity"
                );
            }
        }

        // A linear source ramp turns the internal shift into a measurable pixel
        // offset: the sampled source coordinate moves by tan(disparity) / (2h)
        // normalized units, which is tan(disparity) * width / (2h) pixels.
        let centre = 90 * WIDTH + 180;
        let measured = f64::from((left[centre][0] - right[centre][0]) * (WIDTH - 1) as f32);
        let expected =
            (10.0_f64.to_radians()).tan() * WIDTH as f64 / (2.0 * base.patch_half_extent());
        assert!(
            (measured - expected).abs() <= 1.0,
            "measured {measured} px shift, expected {expected} px"
        );
        assert_ne!(bits(&left), bits(&right), "the content must move");
    }

    #[test]
    fn feather_only_affects_the_outer_band() {
        let sharp = grade(360.0, 180.0, 60.0, 0.0, 0.0);
        let soft = VrProject2DGrade::new(
            360.0,
            180.0,
            60.0,
            0.0,
            20.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
        )
        .expect("valid grade");
        let source = projection_frame(WIDTH, HEIGHT);
        let mut sharp_output = source.clone();
        let mut soft_output = source.clone();
        assert!(vr_project_2d_frame_f32(
            &mut sharp_output,
            WIDTH,
            HEIGHT,
            sharp
        ));
        assert!(vr_project_2d_frame_f32(
            &mut soft_output,
            WIDTH,
            HEIGHT,
            soft
        ));

        let sharp_bits = bits(&sharp_output);
        let soft_bits = bits(&soft_output);
        let half_extent = sharp.patch_half_extent();
        let mut softened = false;
        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                let Some((patch_x, patch_y)) =
                    sharp.patch_coordinates(column as f64, row as f64, WIDTH, HEIGHT)
                else {
                    assert_eq!(
                        sharp_bits[row * WIDTH + column],
                        soft_bits[row * WIDTH + column]
                    );
                    continue;
                };
                let radius = patch_x.abs().max(patch_y.abs()) / half_extent;
                if radius < 0.79 {
                    assert_eq!(
                        sharp_bits[row * WIDTH + column],
                        soft_bits[row * WIDTH + column],
                        "the core (radius {radius}) must not be feathered"
                    );
                }
                if (0.85..1.0).contains(&radius)
                    && sharp_bits[row * WIDTH + column] != soft_bits[row * WIDTH + column]
                {
                    softened = true;
                }
            }
        }
        assert!(softened, "the outer band must be visibly softened");
    }

    #[test]
    fn both_eyes_receive_the_patch() {
        let grade = VrProject2DGrade::new(
            360.0,
            180.0,
            60.0,
            0.0,
            10.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::OverUnder,
        )
        .expect("valid grade");
        let source = projection_frame(WIDTH, HEIGHT);
        let mut output = source.clone();
        assert!(vr_project_2d_frame_f32(&mut output, WIDTH, HEIGHT, grade));
        assert_ne!(output[45 * WIDTH + 180], source[45 * WIDTH + 180]);
        assert_ne!(output[135 * WIDTH + 180], source[135 * WIDTH + 180]);

        let changed = |row: usize| -> Vec<usize> {
            (0..WIDTH)
                .filter(|column| output[row * WIDTH + column] != source[row * WIDTH + column])
                .collect()
        };
        assert_eq!(
            changed(45),
            changed(135),
            "both eyes must have the same patch footprint"
        );
    }

    #[test]
    fn a_tilted_source_card_disappears_past_ninety_degrees() {
        // At a 100 degree source tilt the card's visible half plane has rotated
        // past the patch: every patch point now lies behind the card.
        let grade = VrProject2DGrade::new(
            360.0,
            180.0,
            20.0,
            0.0,
            0.0,
            100.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
        )
        .expect("valid grade");
        let source = projection_frame(WIDTH, HEIGHT);
        let mut output = source.clone();
        assert!(vr_project_2d_frame_f32(&mut output, WIDTH, HEIGHT, grade));
        assert_eq!(bits(&output), bits(&source));
        assert!(grade.source_position(179.5, 89.5, WIDTH, HEIGHT).is_none());
    }

    #[test]
    fn sample_position_matches_an_independent_f64_reference() {
        let grade = VrProject2DGrade::new(
            270.0,
            120.0,
            80.0,
            7.5,
            0.0,
            12.0,
            -30.0,
            40.0,
            -15.0,
            25.0,
            -35.0,
            VrFrameLayout::Monoscopic,
        )
        .expect("valid grade");
        let cases = [
            (0.0, 0.0),
            (37.0, 142.0),
            (179.5, 89.5),
            (240.0, 40.0),
            (359.0, 179.0),
        ];
        for (column, row) in cases {
            let actual = grade.source_position(column, row, WIDTH, HEIGHT);
            let reference = reference_source_position(&grade, column, row, WIDTH, HEIGHT);
            match (actual, reference) {
                (Some(actual), Some(reference)) => {
                    assert!(
                        (actual.0 - reference.0).abs() < 1e-9,
                        "column {column} row {row}: x {} vs {}",
                        actual.0,
                        reference.0
                    );
                    assert!(
                        (actual.1 - reference.1).abs() < 1e-9,
                        "column {column} row {row}: y {} vs {}",
                        actual.1,
                        reference.1
                    );
                }
                (None, None) => {}
                _ => panic!("column {column} row {row}: visibility disagrees with the reference"),
            }
        }
    }

    #[test]
    fn checkpoint_runs_every_chunk_and_propagates_errors() {
        let grade = grade(360.0, 180.0, 60.0, 0.0, 0.0);
        // Small enough that the premultiply runs in one chunk, so the
        // checkpoint count is deterministic.
        let source = projection_frame(64, 32);
        let mut output = source.clone();
        let mut calls = 0;
        let result: Result<bool, &'static str> =
            apply_vr_project_2d_f32_controlled(&mut output, 64, 32, grade, &mut || {
                calls += 1;
                if calls == 2 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            });
        assert_eq!(result, Err("cancelled"));
        assert_eq!(calls, 2);

        let mut output = source.clone();
        let mut calls = 0;
        let result = apply_vr_project_2d_f32_controlled(&mut output, 64, 32, grade, &mut || {
            calls += 1;
            Ok::<(), &'static str>(())
        });
        assert_eq!(result, Ok(true));
        // Two checkpoints inside the premultiply, one after staging, one after
        // the band walk.
        assert_eq!(calls, 4);
    }

    #[test]
    fn empty_frames_are_reported_as_not_handled() {
        let grade = grade(360.0, 180.0, 60.0, 0.0, 0.0);
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!vr_project_2d_frame_f32(&mut empty, 0, 0, grade));
        assert!(!vr_project_2d_frame_f32(&mut empty, 4, 4, grade));
        let mut too_small = vec![[0.0_f32; 4]; 6];
        assert!(!vr_project_2d_frame_f32(&mut too_small, 4, 4, grade));
    }

    /// Independent f64 implementation of the documented mapping.
    fn reference_source_position(
        grade: &VrProject2DGrade,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> Option<(f64, f64)> {
        fn rotation(pitch: f64, yaw: f64, roll: f64) -> [[f64; 3]; 3] {
            let (sin_pitch, cos_pitch) = pitch.to_radians().sin_cos();
            let (sin_yaw, cos_yaw) = yaw.to_radians().sin_cos();
            let (sin_roll, cos_roll) = roll.to_radians().sin_cos();
            [
                [
                    cos_roll * cos_yaw,
                    cos_roll * sin_yaw * sin_pitch - sin_roll * cos_pitch,
                    cos_roll * sin_yaw * cos_pitch + sin_roll * sin_pitch,
                ],
                [
                    sin_roll * cos_yaw,
                    sin_roll * sin_yaw * sin_pitch + cos_roll * cos_pitch,
                    sin_roll * sin_yaw * cos_pitch - cos_roll * sin_pitch,
                ],
                [-sin_yaw, cos_yaw * sin_pitch, cos_yaw * cos_pitch],
            ]
        }
        let u = (column + 0.5) / width as f64;
        let v = (row + 0.5) / height as f64;
        let longitude = (u - 0.5) * grade.horizontal_fov_degrees().to_radians();
        let latitude = (0.5 - v) * grade.vertical_fov_degrees().to_radians();
        let (sin_longitude, cos_longitude) = longitude.sin_cos();
        let (sin_latitude, cos_latitude) = latitude.sin_cos();
        let direction = [
            cos_latitude * sin_longitude,
            sin_latitude,
            cos_latitude * cos_longitude,
        ];
        let projection = rotation(
            grade.projection_tilt_x_degrees(),
            grade.projection_pan_y_degrees(),
            grade.projection_roll_z_degrees(),
        );
        let q = [
            projection[0][0] * direction[0]
                + projection[1][0] * direction[1]
                + projection[2][0] * direction[2],
            projection[0][1] * direction[0]
                + projection[1][1] * direction[1]
                + projection[2][1] * direction[2],
            projection[0][2] * direction[0]
                + projection[1][2] * direction[1]
                + projection[2][2] * direction[2],
        ];
        if q[2] <= 0.0 {
            return None;
        }
        let patch_x = q[0] / q[2];
        let patch_y = q[1] / q[2];
        let half_extent = (grade.scale_degrees() * 0.5).to_radians().tan().max(1e-6);
        if patch_x.abs() > half_extent || patch_y.abs() > half_extent {
            return None;
        }
        let source = rotation(
            grade.source_tilt_x_degrees(),
            grade.source_pan_y_degrees(),
            grade.source_roll_z_degrees(),
        );
        let s = [
            source[0][0] * patch_x + source[1][0] * patch_y + source[2][0],
            source[0][1] * patch_x + source[1][1] * patch_y + source[2][1],
            source[0][2] * patch_x + source[1][2] * patch_y + source[2][2],
        ];
        if s[2] <= 0.0 {
            return None;
        }
        let a_source = s[0] / s[2];
        let b_source = s[1] / s[2];
        let disparity = grade.stereo_disparity_degrees().to_radians().tan();
        let u_source = 0.5 + (a_source - disparity) / (2.0 * half_extent);
        let v_source = 0.5 - b_source / (2.0 * half_extent);
        Some((
            u_source * width as f64 - 0.5,
            v_source * height as f64 - 0.5,
        ))
    }
}
