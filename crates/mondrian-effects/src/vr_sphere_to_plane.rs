//! VR sphere to plane (VR3D-13 in the effect list).
//!
//! The effect is the inverse of [`crate::vr_rotate_sphere`]: the destination
//! frame is a perspective view, every destination pixel maps to a direction in
//! the view camera, that direction is rotated by the authored projection tilt,
//! and the resulting direction is looked up in a full equirectangular panorama.
//! It is a pure inverse resampling — destination pixel in, source pixel out — so
//! the kernel holds no state and its result is independent of the processing
//! order.
//!
//! The conventions match the rotate-sphere kernel exactly:
//!
//! - right handed, `+X` right, `+Y` up, `+Z` forward, so a direction with
//!   longitude `lambda` and latitude `phi` is
//!   `d = (cos(phi) sin(lambda), sin(phi), cos(phi) cos(lambda))`;
//! - the destination is a perspective camera with a horizontal field of view
//!   `h_fov` (full angle); the vertical field of view follows from the frame
//!   aspect ratio, `tan(v/2) = tan(h/2) * h_eye / w_eye`;
//! - the rotation is `R = Rz(roll) * Ry(yaw) * Rx(pitch)`; the kernel samples
//!   the source at `R * d` (`R^T * d` when `invert` is set), because turning
//!   the camera to the right is the same as turning the content to the left;
//! - the source is the whole 360 x 180 equirectangular frame, so there is no
//!   source field of view and no longitude wrap branch: `lon` naturally lands
//!   in `(-pi, pi]`. The seam at `lon = pi` samples `w - 0.5`, which the
//!   resampler clamps to the last column — the known one-column approximation
//!   documented as risk R5 in the design brief.
//!
//! Unlike the rotate-sphere effect, this one has **no identity state**: with a
//! perspective camera there is always a lens, so the default parameters still
//! project the centre `h_fov` degrees of the panorama onto the whole frame. The
//! graph builder therefore always appends the node; [`VrSphereToPlaneGrade::rotation_is_identity`]
//! only lets the kernel skip the nine multiply-adds of the rotation matrix.
//!
//! The alpha model follows the bezier warp: the straight frame is snapshotted,
//! a **copy** is premultiplied, the copy is sampled, and each written pixel is
//! converted back to straight exactly once. A whole-frame premultiply /
//! unpremultiply round trip would zero the hidden colour of fully transparent
//! pixels.

use crate::coverage::straight_rgba_from_premultiplied;
use crate::parallel::for_each_row_band;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::premultiply_rgba_controlled;

/// Smallest horizontal field of view, in degrees.
pub const VR_SPHERE_TO_PLANE_MIN_FOV_DEGREES: f32 = 60.0;

/// Largest horizontal field of view, in degrees.
///
/// `180` degrees is the limit of a perspective projection: `tan(90)` diverges,
/// so the implementation keeps a finite tangent at the endpoint.
pub const VR_SPHERE_TO_PLANE_MAX_FOV_DEGREES: f32 = 180.0;

/// Default horizontal field of view, in degrees.
pub const VR_SPHERE_TO_PLANE_DEFAULT_FOV_DEGREES: f32 = 90.0;

/// Errors raised when author values cannot describe an executable view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VrSphereToPlaneError {
    /// The horizontal field of view is not finite or is outside its range.
    HorizontalFov,
    /// One of the three projection angles is not a finite number.
    Angle,
}

impl std::fmt::Display for VrSphereToPlaneError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HorizontalFov => write!(
                formatter,
                "sphere to plane horizontal field of view must be within \
                 {VR_SPHERE_TO_PLANE_MIN_FOV_DEGREES}..{VR_SPHERE_TO_PLANE_MAX_FOV_DEGREES} degrees"
            ),
            Self::Angle => formatter.write_str("sphere to plane angles must be finite"),
        }
    }
}

impl std::error::Error for VrSphereToPlaneError {}

/// Uniform packing for the GPU pass: `(params, extra, aces, extra1)`.
///
/// `extra.y` (the frame aspect) is zero here because it depends on the
/// execution frame, not on the authored grade; the renderer fills it with
/// [`VrSphereToPlaneGrade::gpu_controls_for_frame`].
pub type VrSphereToPlaneControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// Authored sphere-to-plane state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VrSphereToPlaneGrade {
    horizontal_fov_degrees: f64,
    projection_tilt_x_degrees: f64,
    projection_tilt_y_degrees: f64,
    projection_tilt_z_degrees: f64,
    invert: bool,
}

impl VrSphereToPlaneGrade {
    /// Builds a grade, rejecting state the declared ranges cannot express.
    ///
    /// The three angles are wrapped into `(-180, 180]` here, once, so the
    /// rotation-is-identity test can compare them against zero without an
    /// epsilon and the uniform always carries a small, finite angle.
    pub fn new(
        horizontal_fov_degrees: f64,
        projection_tilt_x_degrees: f64,
        projection_tilt_y_degrees: f64,
        projection_tilt_z_degrees: f64,
        invert: bool,
    ) -> Result<Self, VrSphereToPlaneError> {
        let minimum = f64::from(VR_SPHERE_TO_PLANE_MIN_FOV_DEGREES);
        let maximum = f64::from(VR_SPHERE_TO_PLANE_MAX_FOV_DEGREES);
        if !horizontal_fov_degrees.is_finite()
            || !(minimum..=maximum).contains(&horizontal_fov_degrees)
        {
            return Err(VrSphereToPlaneError::HorizontalFov);
        }
        if !projection_tilt_x_degrees.is_finite()
            || !projection_tilt_y_degrees.is_finite()
            || !projection_tilt_z_degrees.is_finite()
        {
            return Err(VrSphereToPlaneError::Angle);
        }
        Ok(Self {
            horizontal_fov_degrees,
            projection_tilt_x_degrees: crate::vr_rotate_sphere::wrap_degrees(
                projection_tilt_x_degrees,
            ),
            projection_tilt_y_degrees: crate::vr_rotate_sphere::wrap_degrees(
                projection_tilt_y_degrees,
            ),
            projection_tilt_z_degrees: crate::vr_rotate_sphere::wrap_degrees(
                projection_tilt_z_degrees,
            ),
            invert,
        })
    }

    /// Authored horizontal field of view, in degrees.
    pub fn horizontal_fov_degrees(&self) -> f64 {
        self.horizontal_fov_degrees
    }

    /// Wrapped projection pitch about the world X axis, in degrees.
    pub fn projection_tilt_x_degrees(&self) -> f64 {
        self.projection_tilt_x_degrees
    }

    /// Wrapped projection yaw about the world Y axis, in degrees.
    pub fn projection_tilt_y_degrees(&self) -> f64 {
        self.projection_tilt_y_degrees
    }

    /// Wrapped projection roll about the world Z axis, in degrees.
    pub fn projection_tilt_z_degrees(&self) -> f64 {
        self.projection_tilt_z_degrees
    }

    /// `true` when the sampling direction is the forward rotation's inverse.
    pub fn invert(&self) -> bool {
        self.invert
    }

    /// `true` when the view camera is not rotated.
    ///
    /// This is **not** an identity test: even with all three angles at zero the
    /// effect still projects the panorama onto the plane. It only lets the
    /// kernel skip the rotation matrix multiplication, which is bit-identical
    /// when `R = I` because multiplying by one and adding zero is exact.
    pub fn rotation_is_identity(&self) -> bool {
        self.projection_tilt_x_degrees == 0.0
            && self.projection_tilt_y_degrees == 0.0
            && self.projection_tilt_z_degrees == 0.0
    }

    /// `tan(h_fov / 2)`, computed once on the CPU side.
    fn tan_half_horizontal_fov(&self) -> f64 {
        (self.horizontal_fov_degrees.to_radians() / 2.0).tan()
    }

    /// Uniform packing for the shared spatial pass, minus the frame aspect that
    /// depends on the execution frame (see [`VrSphereToPlaneControls`]).
    pub fn gpu_controls(&self) -> VrSphereToPlaneControls {
        let horizontal = self.horizontal_fov_degrees.to_radians() as f32;
        let tangent = self.tan_half_horizontal_fov() as f32;
        let pitch = self.projection_tilt_x_degrees.to_radians() as f32;
        let yaw = self.projection_tilt_y_degrees.to_radians() as f32;
        let roll = self.projection_tilt_z_degrees.to_radians() as f32;
        (
            [horizontal, tangent, pitch, yaw],
            [roll, 0.0, if self.invert { 1.0 } else { 0.0 }, 0.0],
            [0.0; 4],
            [0.0; 4],
        )
    }

    /// Uniform packing with `extra.y` set to the frame aspect `h_eye / w_eye`.
    ///
    /// The division happens here, once, so the shader only multiplies and the
    /// CPU and GPU aspect cannot drift apart.
    pub fn gpu_controls_for_frame(&self, width: usize, height: usize) -> VrSphereToPlaneControls {
        let mut controls = self.gpu_controls();
        controls.1[1] = if width == 0 {
            0.0
        } else {
            height as f32 / width as f32
        };
        controls
    }

    /// Rotation matrix `R = Rz(roll) * Ry(yaw) * Rx(pitch)`.
    fn rotation(&self) -> [[f64; 3]; 3] {
        let pitch = self.projection_tilt_x_degrees.to_radians();
        let yaw = self.projection_tilt_y_degrees.to_radians();
        let roll = self.projection_tilt_z_degrees.to_radians();
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let (sr, cr) = roll.sin_cos();
        [
            [cr * cy, cr * sy * sp - sr * cp, cr * sy * cp + sr * sp],
            [sr * cy, sr * sy * sp + cr * cp, sr * sy * cp - cr * sp],
            [-sy, cy * sp, cy * cp],
        ]
    }

    /// Sampling matrix: `R` forward, `R^T` when inverted.
    ///
    /// `invert` walks the same mapping backwards, so it samples with the
    /// transpose. Negating all three angles while keeping the multiplication
    /// order would produce `Rz(-r) * Ry(-y) * Rx(-p)`, which is **not** `R^T`
    /// (`R^T = Rx(-p) * Ry(-y) * Rz(-r)`).
    fn sampling_matrix(&self) -> [[f64; 3]; 3] {
        let rotation = self.rotation();
        if self.invert {
            [
                [rotation[0][0], rotation[1][0], rotation[2][0]],
                [rotation[0][1], rotation[1][1], rotation[2][1]],
                [rotation[0][2], rotation[1][2], rotation[2][2]],
            ]
        } else {
            rotation
        }
    }

    /// Direction of the destination pixel centre, in camera space.
    ///
    /// `(column, row)` use the grid convention: the centre of pixel `i` is at
    /// `i + 0.5`, so passing `-0.5` addresses the left frame edge and
    /// `width - 0.5` the right one. The returned vector is normalized; its `z`
    /// component is positive (the camera looks down `+Z`).
    pub fn target_direction(&self, column: f64, row: f64, width: usize, height: usize) -> [f64; 3] {
        if width == 0 || height == 0 {
            return [0.0, 0.0, 1.0];
        }
        let u = (column + 0.5) / width as f64;
        let v = (row + 0.5) / height as f64;
        let tangent = self.tan_half_horizontal_fov();
        let aspect = height as f64 / width as f64;
        let ax = (2.0 * u - 1.0) * tangent;
        let ay = (1.0 - 2.0 * v) * tangent * aspect;
        let length = (ax * ax + ay * ay + 1.0).sqrt();
        [ax / length, ay / length, 1.0 / length]
    }

    /// Source sample position, in frame sample coordinates, for the destination
    /// pixel `(column, row)` of a `width` x `height` frame.
    ///
    /// The returned `(x, y)` use the resampler's convention: integer `x` is the
    /// centre of source column `x`, so the half-pixel term is applied here and
    /// nowhere else. The value is not clamped; the kernel clamps it to the
    /// frame.
    pub fn source_position(
        &self,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> (f64, f64) {
        if width == 0 || height == 0 {
            return (0.0, 0.0);
        }
        let destination = self.target_direction(column, row, width, height);
        let source = if self.rotation_is_identity() {
            destination
        } else {
            let matrix = self.sampling_matrix();
            [
                matrix[0][0] * destination[0]
                    + matrix[0][1] * destination[1]
                    + matrix[0][2] * destination[2],
                matrix[1][0] * destination[0]
                    + matrix[1][1] * destination[1]
                    + matrix[1][2] * destination[2],
                matrix[2][0] * destination[0]
                    + matrix[2][1] * destination[1]
                    + matrix[2][2] * destination[2],
            ]
        };

        let longitude = source[0].atan2(source[2]);
        let latitude = source[1].clamp(-1.0, 1.0).asin();
        let u_source = longitude / std::f64::consts::TAU + 0.5;
        let v_source = 0.5 - latitude / std::f64::consts::PI;
        (
            u_source * width as f64 - 0.5,
            v_source * height as f64 - 0.5,
        )
    }
}

/// Applies the projection to a whole frame, returning `false` when the frame
/// cannot be processed.
pub fn vr_sphere_to_plane_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: VrSphereToPlaneGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_vr_sphere_to_plane_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the projection in place with an explicit cancellation checkpoint.
///
/// The kernel snapshots the straight frame, premultiplies a copy, and samples
/// that copy, so it never has to unpremultiply the frame as a whole and the
/// input can be walked in either direction. Only the checkpoint can fail.
pub(crate) fn apply_vr_sphere_to_plane_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: VrSphereToPlaneGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || width == 0 || height == 0 || working.len() < pixel_count {
        return Ok(false);
    }

    let frame = &mut working[..pixel_count];
    let source = frame.to_vec();
    let mut staged = source.clone();
    premultiply_rgba_controlled(&mut staged, checkpoint)?;
    checkpoint()?;

    let maximum_x = (width - 1) as f64;
    let maximum_y = (height - 1) as f64;
    for_each_row_band(frame, width, 0..height, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let (sample_x, sample_y) =
                    grade.source_position(column as f64, row as f64, width, height);
                // The whole equirectangular frame is the source, so clamping to
                // the frame is exactly the seam / pole policy: the last column
                // and the first one are the same meridian, and the poles are
                // single rows.
                let sample_x = sample_x.clamp(0.0, maximum_x);
                let sample_y = sample_y.clamp(0.0, maximum_y);
                let sampled = sample_bilinear_clamped(&staged, width, height, sample_x, sample_y);
                *pixel = straight_rgba_from_premultiplied(sampled);
            }
        }
    });

    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: usize = 360;
    const HEIGHT: usize = 180;

    fn grade(
        horizontal_fov: f64,
        tilt_x: f64,
        tilt_y: f64,
        tilt_z: f64,
        invert: bool,
    ) -> VrSphereToPlaneGrade {
        VrSphereToPlaneGrade::new(horizontal_fov, tilt_x, tilt_y, tilt_z, invert)
            .expect("the grade must be valid")
    }

    /// Gradient frame with a few deliberately hostile pixels: hidden colour
    /// under zero alpha, negative channels and alpha above one.
    fn gradient_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let x = column as f32 / width.max(1) as f32;
                let y = row as f32 / height.max(1) as f32;
                pixels[row * width + column] = [x, y, 0.25 + 0.5 * x, 0.5 + 0.5 * y];
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
    fn zero_angles_produce_a_finite_centred_view() {
        let defaults = grade(90.0, 0.0, 0.0, 0.0, false);
        let mut pixels = gradient_frame(WIDTH, HEIGHT);
        assert!(vr_sphere_to_plane_frame_f32(
            &mut pixels,
            WIDTH,
            HEIGHT,
            defaults
        ));
        assert!(
            pixels.iter().flat_map(|pixel| pixel.iter()).all(|value| value.is_finite()),
            "the default view must not produce NaN or infinity"
        );
        // A 90 degree view takes the centre quarter of the 360 degree source,
        // so the panorama centre lands in the middle of the destination.
        let (centre_x, centre_y) = defaults.source_position(179.5, 89.5, WIDTH, HEIGHT);
        assert!((centre_x - 179.5).abs() <= 1.0e-9);
        assert!((centre_y - 89.5).abs() <= 1.0e-9);
    }

    #[test]
    fn the_centre_pixel_reads_the_centre_of_the_sphere() {
        // An odd frame puts one pixel centre exactly on the optical axis.
        let view = grade(90.0, 0.0, 0.0, 0.0, false);
        let (sample_x, sample_y) = view.source_position(180.0, 90.0, 361, 181);
        assert!(
            (sample_x - 180.0).abs() <= 0.5,
            "centre ray must read source column 180, got {sample_x}"
        );
        assert!(
            (sample_y - 90.0).abs() <= 0.5,
            "centre ray must read source row 90, got {sample_y}"
        );
        assert!((sample_x - 180.0).abs() <= 1.0e-9);
        assert!((sample_y - 90.0).abs() <= 1.0e-9);
    }

    #[test]
    fn a_ninety_degree_view_covers_exactly_ninety_degrees() {
        // Pixel centres sit half a pixel inside the frame, so the field of view
        // is measured between the frame edges, addressed as -0.5 and w - 0.5.
        let view = grade(90.0, 0.0, 0.0, 0.0, false);
        let left = view.target_direction(-0.5, 90.0, 181, 181);
        let right = view.target_direction(180.5, 90.0, 181, 181);
        let span = angle_between(left, right);
        assert!(
            (span - std::f64::consts::FRAC_PI_2).abs() <= 1.0e-6,
            "the view spans {span} radians, expected pi/2"
        );
    }

    #[test]
    fn doubling_the_field_of_view_doubles_the_angular_span() {
        let narrow = grade(60.0, 0.0, 0.0, 0.0, false);
        let wide = grade(120.0, 0.0, 0.0, 0.0, false);
        let span = |view: &VrSphereToPlaneGrade| {
            angle_between(
                view.target_direction(-0.5, 90.0, 181, 181),
                view.target_direction(180.5, 90.0, 181, 181),
            )
        };
        let narrow_span = span(&narrow);
        let wide_span = span(&wide);
        assert!((narrow_span - 60.0_f64.to_radians()).abs() <= 1.0e-9);
        assert!((wide_span - 120.0_f64.to_radians()).abs() <= 1.0e-9);
        assert!((wide_span / narrow_span - 2.0).abs() <= 1.0e-9);
    }

    #[test]
    fn a_hundred_and_eighty_degree_view_still_produces_finite_pixels() {
        let view = grade(180.0, 0.0, 0.0, 0.0, false);
        let mut pixels = gradient_frame(181, 181);
        assert!(vr_sphere_to_plane_frame_f32(&mut pixels, 181, 181, view));
        assert!(
            pixels.iter().flat_map(|pixel| pixel.iter()).all(|value| value.is_finite()),
            "the 180 degree endpoint must stay finite"
        );
        let edge = view.target_direction(-0.5, 90.0, 181, 181);
        let span = angle_between(edge, [0.0, 0.0, 1.0]);
        assert!(
            span < std::f64::consts::FRAC_PI_2 + 1.0e-9 && span > 89.0_f64.to_radians(),
            "the edge ray must approach but not exceed 90 degrees, got {span}"
        );
    }

    #[test]
    fn projection_tilt_moves_the_view_like_the_rotate_sphere_kernel() {
        // Both kernels apply the same `R` to the central `+Z` ray, so their
        // centre samples must agree for the same authored angles.
        let pitch = 17.0;
        let yaw = -31.0;
        let roll = 43.0;
        let plane = grade(90.0, pitch, yaw, roll, false);
        let sphere = crate::vr_rotate_sphere::VrRotateSphereGrade::new(
            360.0,
            180.0,
            pitch,
            yaw,
            roll,
            crate::vr_rotate_sphere::VrFrameLayout::Monoscopic,
            false,
        )
        .expect("the rotate-sphere grade must be valid");
        let (plane_x, plane_y) = plane.source_position(180.0, 90.0, 361, 181);
        let (sphere_x, sphere_y) = sphere.source_position(180.0, 90.0, 361, 181);
        assert!(
            (plane_x - sphere_x).abs() <= 1.0e-9,
            "centre x {plane_x} != rotate-sphere {sphere_x}"
        );
        assert!(
            (plane_y - sphere_y).abs() <= 1.0e-9,
            "centre y {plane_y} != rotate-sphere {sphere_y}"
        );
    }

    #[test]
    fn invert_undoes_the_forward_rotation() {
        let forward = grade(90.0, 12.0, -25.0, 40.0, false);
        let backward = grade(90.0, 12.0, -25.0, 40.0, true);
        let forward_matrix = forward.sampling_matrix();
        let backward_matrix = backward.sampling_matrix();
        for row in 0..3 {
            for column in 0..3 {
                assert!(
                    (backward_matrix[row][column] - forward_matrix[column][row]).abs() <= 1.0e-12,
                    "invert must be the transpose, not the negated angles"
                );
            }
        }
        // Composing the two sampling matrices must return the central ray.
        let mut direction = [0.0_f64, 0.0, 1.0];
        for matrix in [forward_matrix, backward_matrix] {
            direction = [
                matrix[0][0] * direction[0]
                    + matrix[0][1] * direction[1]
                    + matrix[0][2] * direction[2],
                matrix[1][0] * direction[0]
                    + matrix[1][1] * direction[1]
                    + matrix[1][2] * direction[2],
                matrix[2][0] * direction[0]
                    + matrix[2][1] * direction[1]
                    + matrix[2][2] * direction[2],
            ];
        }
        for (axis, value) in direction.iter().enumerate() {
            let expected = if axis == 2 { 1.0 } else { 0.0 };
            assert!(
                (value - expected).abs() <= 1.0e-4,
                "axis {axis}: {value} != {expected}"
            );
        }
    }

    #[test]
    fn the_view_is_square_pixel_correct_on_a_sixteen_by_nine_frame() {
        // 160 x 90 is 16:9; with a 90 degree horizontal field of view the
        // vertical half angle is `atan(tan(45) * 9 / 16)`.
        let view = grade(90.0, 0.0, 0.0, 0.0, false);
        let top = view.target_direction(79.5, -0.5, 160, 90);
        let bottom = view.target_direction(79.5, 89.5, 160, 90);
        let vertical_span = angle_between(top, bottom);
        let expected = 2.0 * (9.0_f64 / 16.0).atan();
        assert!(
            (vertical_span - expected).abs() <= 1.0e-6,
            "vertical span {vertical_span} != {expected}"
        );
        let left = view.target_direction(-0.5, 44.5, 160, 90);
        let right = view.target_direction(159.5, 44.5, 160, 90);
        assert!((angle_between(left, right) - std::f64::consts::FRAC_PI_2).abs() <= 1.0e-6);
    }

    #[test]
    fn sample_position_matches_an_independent_f64_reference() {
        for invert in [false, true] {
            let tested = grade(123.0, 23.0, -47.0, 61.0, invert);
            for column in [0_usize, 17, 90, 179, 180, 233, 359] {
                for row in [0_usize, 3, 44, 90, 137, 179] {
                    let (sample_x, sample_y) =
                        tested.source_position(column as f64, row as f64, WIDTH, HEIGHT);
                    let (expected_x, expected_y) =
                        reference_source_position(&tested, column, row, WIDTH, HEIGHT);
                    assert!(
                        (sample_x - expected_x).abs() <= 1.0e-9,
                        "column {column} row {row}: x {sample_x} != {expected_x}"
                    );
                    assert!(
                        (sample_y - expected_y).abs() <= 1.0e-9,
                        "column {column} row {row}: y {sample_y} != {expected_y}"
                    );
                }
            }
        }
    }

    /// Hand-written independent reference: computes the target ray inline, then
    /// builds the sampling matrix as nine named entries and applies it element
    /// by element.
    fn reference_source_position(
        grade: &VrSphereToPlaneGrade,
        column: usize,
        row: usize,
        width: usize,
        height: usize,
    ) -> (f64, f64) {
        let u = (column as f64 + 0.5) / width as f64;
        let v = (row as f64 + 0.5) / height as f64;
        let tangent = (grade.horizontal_fov_degrees().to_radians() / 2.0).tan();
        let aspect = height as f64 / width as f64;
        let ax = (2.0 * u - 1.0) * tangent;
        let ay = (1.0 - 2.0 * v) * tangent * aspect;
        let length = (ax * ax + ay * ay + 1.0).sqrt();
        let (direction_x, direction_y, direction_z) = (ax / length, ay / length, 1.0 / length);

        let pitch = grade.projection_tilt_x_degrees().to_radians();
        let yaw = grade.projection_tilt_y_degrees().to_radians();
        let roll = grade.projection_tilt_z_degrees().to_radians();
        let (sp, cp) = (pitch.sin(), pitch.cos());
        let (sy, cy) = (yaw.sin(), yaw.cos());
        let (sr, cr) = (roll.sin(), roll.cos());
        // Row-major entries of R = Rz * Ry * Rx.
        let r00 = cr * cy;
        let r01 = cr * sy * sp - sr * cp;
        let r02 = cr * sy * cp + sr * sp;
        let r10 = sr * cy;
        let r11 = sr * sy * sp + cr * cp;
        let r12 = sr * sy * cp - cr * sp;
        let r20 = -sy;
        let r21 = cy * sp;
        let r22 = cy * cp;
        let (m00, m01, m02, m10, m11, m12, m20, m21, m22) = if grade.invert() {
            (r00, r10, r20, r01, r11, r21, r02, r12, r22)
        } else {
            (r00, r01, r02, r10, r11, r12, r20, r21, r22)
        };
        let source_x = m00 * direction_x + m01 * direction_y + m02 * direction_z;
        let source_y = m10 * direction_x + m11 * direction_y + m12 * direction_z;
        let source_z = m20 * direction_x + m21 * direction_y + m22 * direction_z;

        let longitude = source_x.atan2(source_z);
        let latitude = source_y.clamp(-1.0, 1.0).asin();
        (
            (longitude / std::f64::consts::TAU + 0.5) * width as f64 - 0.5,
            (0.5 - latitude / std::f64::consts::PI) * height as f64 - 0.5,
        )
    }

    #[test]
    fn checkpoint_runs_every_chunk_and_propagates_errors() {
        let mut checkpoints = 0_usize;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), &'static str>(())
        };
        let mut pixels = gradient_frame(64, 32);
        let view = grade(90.0, 5.0, 10.0, 0.0, false);
        apply_vr_sphere_to_plane_f32_controlled(&mut pixels, 64, 32, view, &mut checkpoint)
            .expect("the checkpoint never fails");
        // Two inside the premultiply plus one after the staging and one after
        // the row bands.
        assert_eq!(checkpoints, 4);

        let mut calls = 0_usize;
        let mut failing = || {
            calls += 1;
            if calls == 2 {
                Err("cancelled")
            } else {
                Ok(())
            }
        };
        let error =
            apply_vr_sphere_to_plane_f32_controlled(&mut pixels, 64, 32, view, &mut failing)
                .expect_err("the second checkpoint must propagate its error");
        assert_eq!(error, "cancelled");
    }

    #[test]
    fn empty_frames_are_reported_as_not_handled() {
        let view = grade(90.0, 10.0, 0.0, 0.0, false);
        assert!(!vr_sphere_to_plane_frame_f32(&mut [], 0, 0, view));
        assert!(!vr_sphere_to_plane_frame_f32(&mut [], 4, 0, view));
        assert!(!vr_sphere_to_plane_frame_f32(&mut [], 0, 4, view));
        let mut too_small = vec![[0.0_f32; 4]; 6];
        assert!(!vr_sphere_to_plane_frame_f32(&mut too_small, 4, 4, view));
    }

    #[test]
    fn the_seam_column_is_clamped_and_produces_no_wrap_artefact() {
        // Columns 0 and w - 1 are the same meridian; paint the whole seam band
        // red and the rest blue. A 180 degree pitch turns the view around so
        // the centre pixel taps `lon = pi`, i.e. `x_src = w - 0.5`, which the
        // resampler clamps to the last column.
        let mut pixels = vec![[0.0_f32, 0.0, 1.0, 1.0]; WIDTH * HEIGHT];
        for row in 0..HEIGHT {
            for column in [0, 1, WIDTH - 2, WIDTH - 1] {
                pixels[row * WIDTH + column] = [1.0, 0.0, 0.0, 1.0];
            }
        }
        let turned = grade(90.0, 180.0, 0.0, 0.0, false);
        assert!(vr_sphere_to_plane_frame_f32(
            &mut pixels,
            WIDTH,
            HEIGHT,
            turned
        ));
        assert!(pixels.iter().flat_map(|pixel| pixel.iter()).all(|value| value.is_finite()));
        let centre = pixels[(HEIGHT / 2) * WIDTH + WIDTH / 2];
        assert!(
            (centre[0] - 1.0).abs() <= 1.0e-6 && centre[1] <= 1.0e-6 && centre[2] <= 1.0e-6,
            "the seam must read the red band, got {centre:?}"
        );
    }

    #[test]
    fn default_parameters_still_project_a_view() {
        // There is no identity state: the default 90 degree view must change
        // the frame, otherwise the effect would silently do nothing.
        let defaults = grade(90.0, 0.0, 0.0, 0.0, false);
        let mut pixels = gradient_frame(64, 32);
        let original = bits(&pixels);
        assert!(vr_sphere_to_plane_frame_f32(&mut pixels, 64, 32, defaults));
        assert_ne!(bits(&pixels), original);
    }

    #[test]
    fn gpu_controls_pack_the_documented_fields() {
        let view = grade(90.0, 10.0, 20.0, 30.0, true);
        let (params, extra, aces, extra1) = view.gpu_controls_for_frame(160, 90);
        assert_eq!(params[0], std::f32::consts::FRAC_PI_2);
        assert!((params[1] - 1.0).abs() <= 1.0e-6);
        assert_eq!(params[2], 10.0_f64.to_radians() as f32);
        assert_eq!(params[3], 20.0_f64.to_radians() as f32);
        assert_eq!(extra[0], 30.0_f64.to_radians() as f32);
        assert!((extra[1] - 0.5625).abs() <= 1.0e-6);
        assert_eq!(extra[2], 1.0);
        assert_eq!(extra[3], 0.0);
        assert_eq!(aces, [0.0; 4]);
        assert_eq!(extra1, [0.0; 4]);
    }

    #[test]
    fn author_ranges_are_enforced() {
        assert_eq!(
            VrSphereToPlaneGrade::new(59.0, 0.0, 0.0, 0.0, false),
            Err(VrSphereToPlaneError::HorizontalFov)
        );
        assert_eq!(
            VrSphereToPlaneGrade::new(181.0, 0.0, 0.0, 0.0, false),
            Err(VrSphereToPlaneError::HorizontalFov)
        );
        assert_eq!(
            VrSphereToPlaneGrade::new(f64::NAN, 0.0, 0.0, 0.0, false),
            Err(VrSphereToPlaneError::HorizontalFov)
        );
        assert_eq!(
            VrSphereToPlaneGrade::new(90.0, f64::INFINITY, 0.0, 0.0, false),
            Err(VrSphereToPlaneError::Angle)
        );
        // Angles wrap into (-180, 180].
        let wrapped = VrSphereToPlaneGrade::new(90.0, 540.0, -190.0, 180.0, false)
            .expect("wrapped angles are valid");
        assert_eq!(wrapped.projection_tilt_x_degrees(), 180.0);
        assert_eq!(wrapped.projection_tilt_y_degrees(), 170.0);
        assert_eq!(wrapped.projection_tilt_z_degrees(), 180.0);
        assert!(!wrapped.rotation_is_identity());
    }
}
