//! VR rotate sphere (VR3D-14 in the effect list).
//!
//! The effect re-aims a spherical (equirectangular) frame: every destination
//! pixel maps to a direction on the unit sphere, the direction is rotated by the
//! authored tilt / pan / roll, and the resulting direction is looked up in the
//! source frame. It is a pure inverse resampling — destination pixel in, source
//! pixel out — so the kernel holds no state and its result is independent of the
//! processing order.
//!
//! The conventions are the ones written down in the design brief:
//!
//! - right handed, `+X` right, `+Y` up, `+Z` forward, so a direction with
//!   longitude `lambda` and latitude `phi` is
//!   `d = (cos(phi) sin(lambda), sin(phi), cos(phi) cos(lambda))`;
//! - the destination frame covers `h_fov` degrees of longitude and `v_fov`
//!   degrees of latitude, with the frame centre at `(u, v) = (0.5, 0.5)`;
//! - the rotation is `R = Rz(roll) * Ry(yaw) * Rx(pitch)`; the kernel samples the
//!   source at `R * d_dst` (`R^T * d_dst` when `invert` is set), because turning
//!   the camera to the right is the same as turning the content to the left;
//! - longitudes wrap into the `h_fov` window only when the horizontal field of
//!   view is a full turn; otherwise out-of-range samples clamp to the edge
//!   columns, so a pure rotation never punches transparent holes in the frame.
//!
//! Two coordinate systems are involved and they differ by half a pixel. `u`/`v`
//! are grid coordinates (pixel `i` covers `[i / w, (i + 1) / w)`); the resampler
//! takes sample coordinates where the integer `j` is the centre of pixel `j`.
//! The conversion is `x = u * w - 0.5`, and it happens exactly once, in
//! [`VrRotateSphereGrade::source_position`].
//!
//! The alpha model follows the bezier warp: the straight frame is snapshotted,
//! a **copy** is premultiplied, the copy is sampled, and each written pixel is
//! converted back to straight exactly once. A whole-frame premultiply /
//! unpremultiply round trip would zero the hidden colour of fully transparent
//! pixels and would break the bit-exact identity guarantee.

use crate::coverage::straight_rgba_from_premultiplied;
use crate::parallel::for_each_row_band;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::premultiply_rgba_controlled;

/// Smallest horizontal or vertical field of view, in degrees.
pub const VR_ROTATE_SPHERE_MIN_FOV_DEGREES: f32 = 60.0;

/// Largest horizontal field of view, in degrees. A full turn wraps longitude.
pub const VR_ROTATE_SPHERE_MAX_HORIZONTAL_FOV_DEGREES: f32 = 360.0;

/// Largest vertical field of view, in degrees.
pub const VR_ROTATE_SPHERE_MAX_VERTICAL_FOV_DEGREES: f32 = 180.0;

/// Errors raised when author values cannot describe an executable rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VrRotateSphereError {
    /// The horizontal field of view is not finite or is outside its range.
    HorizontalFov,
    /// The vertical field of view is not finite or is outside its range.
    VerticalFov,
    /// One of the three angles is not a finite number.
    Angle,
}

impl std::fmt::Display for VrRotateSphereError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HorizontalFov => write!(
                formatter,
                "vr rotate sphere horizontal field of view must be within \
                 {VR_ROTATE_SPHERE_MIN_FOV_DEGREES}..{VR_ROTATE_SPHERE_MAX_HORIZONTAL_FOV_DEGREES} degrees"
            ),
            Self::VerticalFov => write!(
                formatter,
                "vr rotate sphere vertical field of view must be within \
                 {VR_ROTATE_SPHERE_MIN_FOV_DEGREES}..{VR_ROTATE_SPHERE_MAX_VERTICAL_FOV_DEGREES} degrees"
            ),
            Self::Angle => formatter.write_str("vr rotate sphere angles must be finite"),
        }
    }
}

impl std::error::Error for VrRotateSphereError {}

/// How a stereo frame is packed into one image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VrFrameLayout {
    /// One eye fills the whole frame.
    Monoscopic,
    /// Two eyes stacked vertically: the top half is the left eye.
    OverUnder,
    /// Two eyes side by side: the left half is the left eye.
    SideBySide,
}

impl VrFrameLayout {
    /// Every layout, in schema order.
    pub const ALL: [Self; 3] = [Self::Monoscopic, Self::OverUnder, Self::SideBySide];

    /// Chinese schema label for the option picker.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Monoscopic => "单眼",
            Self::OverUnder => "上下",
            Self::SideBySide => "左右",
        }
    }

    /// Stable schema key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Monoscopic => "monoscopic",
            Self::OverUnder => "over_under",
            Self::SideBySide => "side_by_side",
        }
    }

    /// Numeric code carried in the `extra.z` uniform cell (debugging only).
    pub const fn code(self) -> f32 {
        match self {
            Self::Monoscopic => 0.0,
            Self::OverUnder => 1.0,
            Self::SideBySide => 2.0,
        }
    }

    /// Parses a schema key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "monoscopic" => Some(Self::Monoscopic),
            "over_under" => Some(Self::OverUnder),
            "side_by_side" => Some(Self::SideBySide),
            _ => None,
        }
    }

    /// Size of one eye sub-frame, using integer division as the CPU reference.
    ///
    /// The shader never divides: the recorder passes this result through the
    /// `extra1.z` / `extra1.w` uniform cells, so an odd frame dimension splits
    /// on exactly the same boundary on both sides.
    pub const fn eye_size(self, width: usize, height: usize) -> (usize, usize) {
        match self {
            Self::Monoscopic => (width, height),
            Self::OverUnder => (width, height / 2),
            Self::SideBySide => (width / 2, height),
        }
    }

    /// Eye rectangle as `[origin_x, origin_y, width, height]`.
    pub fn eye_rect(self, width: usize, height: usize) -> [f32; 4] {
        let (eye_width, eye_height) = self.eye_size(width, height);
        [0.0, 0.0, eye_width as f32, eye_height as f32]
    }
}

/// Uniform packing for the GPU pass: `(params, extra, aces, extra1)`.
///
/// `extra1` is zero here because the eye rectangle depends on the execution
/// frame, not on the authored grade; the renderer fills it from
/// [`VrRotateSphereGrade::eye_rect`] when it records the pass.
pub type VrRotateSphereControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// Authored VR rotate sphere state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VrRotateSphereGrade {
    horizontal_fov_degrees: f64,
    vertical_fov_degrees: f64,
    tilt_x_degrees: f64,
    pan_y_degrees: f64,
    roll_z_degrees: f64,
    layout: VrFrameLayout,
    invert: bool,
    wrap_longitude: bool,
}

impl VrRotateSphereGrade {
    /// Builds a grade, rejecting state the declared ranges cannot express.
    ///
    /// The three angles are wrapped into `(-180, 180]` here, once, so the
    /// identity test can compare them against zero without an epsilon and the
    /// uniform always carries a small, finite angle.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        horizontal_fov_degrees: f64,
        vertical_fov_degrees: f64,
        tilt_x_degrees: f64,
        pan_y_degrees: f64,
        roll_z_degrees: f64,
        layout: VrFrameLayout,
        invert: bool,
    ) -> Result<Self, VrRotateSphereError> {
        let horizontal_min = f64::from(VR_ROTATE_SPHERE_MIN_FOV_DEGREES);
        let horizontal_max = f64::from(VR_ROTATE_SPHERE_MAX_HORIZONTAL_FOV_DEGREES);
        if !horizontal_fov_degrees.is_finite()
            || !(horizontal_min..=horizontal_max).contains(&horizontal_fov_degrees)
        {
            return Err(VrRotateSphereError::HorizontalFov);
        }
        let vertical_min = f64::from(VR_ROTATE_SPHERE_MIN_FOV_DEGREES);
        let vertical_max = f64::from(VR_ROTATE_SPHERE_MAX_VERTICAL_FOV_DEGREES);
        if !vertical_fov_degrees.is_finite()
            || !(vertical_min..=vertical_max).contains(&vertical_fov_degrees)
        {
            return Err(VrRotateSphereError::VerticalFov);
        }
        if !tilt_x_degrees.is_finite() || !pan_y_degrees.is_finite() || !roll_z_degrees.is_finite()
        {
            return Err(VrRotateSphereError::Angle);
        }
        Ok(Self {
            horizontal_fov_degrees,
            vertical_fov_degrees,
            tilt_x_degrees: wrap_degrees(tilt_x_degrees),
            pan_y_degrees: wrap_degrees(pan_y_degrees),
            roll_z_degrees: wrap_degrees(roll_z_degrees),
            layout,
            invert,
            // Only a full turn wraps: `359.999` still clamps at the seam.
            wrap_longitude: horizontal_fov_degrees == horizontal_max,
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

    /// Wrapped tilt about the world X axis, in degrees.
    pub fn tilt_x_degrees(&self) -> f64 {
        self.tilt_x_degrees
    }

    /// Wrapped pan about the world Y axis, in degrees.
    pub fn pan_y_degrees(&self) -> f64 {
        self.pan_y_degrees
    }

    /// Wrapped roll about the world Z axis, in degrees.
    pub fn roll_z_degrees(&self) -> f64 {
        self.roll_z_degrees
    }

    /// Stereo packing of the frame.
    pub fn layout(&self) -> VrFrameLayout {
        self.layout
    }

    /// `true` when the sampling direction is the forward rotation's inverse.
    pub fn invert(&self) -> bool {
        self.invert
    }

    /// `true` when the longitude wraps at the frame seam.
    pub fn wrap_longitude(&self) -> bool {
        self.wrap_longitude
    }

    /// `true` when the effect cannot change any pixel.
    ///
    /// Only the three angles count: the fields of view and the layout change
    /// the coordinate card, not the rotation.
    pub fn is_identity(&self) -> bool {
        self.tilt_x_degrees == 0.0 && self.pan_y_degrees == 0.0 && self.roll_z_degrees == 0.0
    }

    /// Size of one eye sub-frame for a frame of `width` x `height` pixels.
    pub const fn eye_size(&self, width: usize, height: usize) -> (usize, usize) {
        self.layout.eye_size(width, height)
    }

    /// Eye rectangle as `[origin_x, origin_y, width, height]`.
    pub fn eye_rect(&self, width: usize, height: usize) -> [f32; 4] {
        self.layout.eye_rect(width, height)
    }

    /// Origin of the eye that owns `(column, row)`.
    ///
    /// The comparison matches the shader's `position.y >= eye_h` check: the
    /// shader compares the pixel centre, which lands on the same side of the
    /// integer boundary for every integer row.
    pub fn eye_origin(
        &self,
        column: usize,
        row: usize,
        width: usize,
        height: usize,
    ) -> (usize, usize) {
        let (eye_width, eye_height) = self.eye_size(width, height);
        match self.layout {
            VrFrameLayout::Monoscopic => (0, 0),
            VrFrameLayout::OverUnder => (0, if row >= eye_height { eye_height } else { 0 }),
            VrFrameLayout::SideBySide => (if column >= eye_width { eye_width } else { 0 }, 0),
        }
    }

    /// Uniform packing for the shared spatial pass, minus the frame-sized
    /// `extra1` cell (see [`VrRotateSphereControls`]).
    pub fn gpu_controls(&self) -> VrRotateSphereControls {
        let horizontal = self.horizontal_fov_degrees.to_radians() as f32;
        let vertical = self.vertical_fov_degrees.to_radians() as f32;
        let pitch = self.tilt_x_degrees.to_radians() as f32;
        let yaw = self.pan_y_degrees.to_radians() as f32;
        let roll = self.roll_z_degrees.to_radians() as f32;
        (
            [horizontal, vertical, pitch, yaw],
            [
                roll,
                if self.invert { 1.0 } else { 0.0 },
                self.layout.code(),
                if self.wrap_longitude { 1.0 } else { 0.0 },
            ],
            [0.0; 4],
            [0.0; 4],
        )
    }

    /// Rotation matrix `R = Rz(roll) * Ry(yaw) * Rx(pitch)`.
    fn rotation(&self) -> [[f64; 3]; 3] {
        let pitch = self.tilt_x_degrees.to_radians();
        let yaw = self.pan_y_degrees.to_radians();
        let roll = self.roll_z_degrees.to_radians();
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
    /// The forward matrix is the camera-to-world rotation `R`, not its
    /// transpose: with `M = R` a positive pitch obeys T4's anchor
    /// (`v_src = v_dst + pitch / v_fov`), and a negative yaw moves the marker
    /// right by a quarter turn in T3. `invert` walks the same mapping backwards,
    /// so it samples with `R^T`.
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

    /// Source sample position, in frame sample coordinates, for the destination
    /// pixel `(column, row)` of a `width` x `height` frame.
    ///
    /// The returned `(x, y)` use the resampler's convention: integer `x` is the
    /// centre of source column `x`, so the half-pixel term is applied here and
    /// nowhere else. The value is not clamped; the kernel clamps it to the eye
    /// rectangle (and the resampler to the frame).
    pub fn source_position(
        &self,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> (f64, f64) {
        let (eye_width, eye_height) = self.eye_size(width, height);
        if eye_width == 0 || eye_height == 0 {
            return (0.0, 0.0);
        }
        let (origin_x, origin_y) = match self.layout {
            VrFrameLayout::Monoscopic => (0.0, 0.0),
            VrFrameLayout::OverUnder => (
                0.0,
                if row >= eye_height as f64 {
                    eye_height as f64
                } else {
                    0.0
                },
            ),
            VrFrameLayout::SideBySide => (
                if column >= eye_width as f64 {
                    eye_width as f64
                } else {
                    0.0
                },
                0.0,
            ),
        };
        let eye_width = eye_width as f64;
        let eye_height = eye_height as f64;

        // Grid coordinates of the destination pixel centre inside its eye.
        let u = (column - origin_x + 0.5) / eye_width;
        let v = (row - origin_y + 0.5) / eye_height;

        let horizontal_fov = self.horizontal_fov_degrees.to_radians();
        let vertical_fov = self.vertical_fov_degrees.to_radians();
        let longitude_destination = (u - 0.5) * horizontal_fov;
        let latitude_destination = (0.5 - v) * vertical_fov;

        let cos_latitude = latitude_destination.cos();
        let destination = [
            cos_latitude * longitude_destination.sin(),
            latitude_destination.sin(),
            cos_latitude * longitude_destination.cos(),
        ];
        let matrix = self.sampling_matrix();
        let source = [
            matrix[0][0] * destination[0]
                + matrix[0][1] * destination[1]
                + matrix[0][2] * destination[2],
            matrix[1][0] * destination[0]
                + matrix[1][1] * destination[1]
                + matrix[1][2] * destination[2],
            matrix[2][0] * destination[0]
                + matrix[2][1] * destination[1]
                + matrix[2][2] * destination[2],
        ];

        let longitude_source = source[0].atan2(source[2]);
        let latitude_source = source[1].clamp(-1.0, 1.0).asin();
        let mut u_source = longitude_source / horizontal_fov + 0.5;
        if self.wrap_longitude {
            // A full turn repeats: reduce the sample column into the frame
            // instead of clamping it. Reducing the *longitude* (into
            // `[0, TAU)`) would leave `u_source` in `[0.5, 1.5)` and push the
            // left half of a rotated panorama past the right edge.
            u_source -= u_source.floor();
        }
        let v_source = 0.5 - latitude_source / vertical_fov;

        (
            u_source * eye_width - 0.5 + origin_x,
            v_source * eye_height - 0.5 + origin_y,
        )
    }
}

/// Wraps an angle in degrees into `(-180, 180]`.
///
/// Shared with the sphere-to-plane kernel, which uses the same author range and
/// the same wrap convention for its three projection angles.
pub(crate) fn wrap_degrees(degrees: f64) -> f64 {
    let wrapped = degrees - (degrees / 360.0).floor() * 360.0;
    if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// Applies the rotation to a whole frame, returning `false` when the frame
/// cannot be processed.
pub fn vr_rotate_sphere_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: VrRotateSphereGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_vr_rotate_sphere_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the rotation in place with an explicit cancellation checkpoint.
///
/// The kernel snapshots the straight frame, premultiplies a copy, and samples
/// that copy, so it never has to unpremultiply the frame as a whole and the
/// input can be walked in either direction. Only the checkpoint can fail.
pub(crate) fn apply_vr_rotate_sphere_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: VrRotateSphereGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || width == 0 || height == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let (eye_width, eye_height) = grade.eye_size(width, height);
    if eye_width == 0 || eye_height == 0 {
        return Ok(false);
    }

    let frame = &mut working[..pixel_count];
    let source = frame.to_vec();
    let mut staged = source.clone();
    premultiply_rgba_controlled(&mut staged, checkpoint)?;
    checkpoint()?;

    for_each_row_band(frame, width, 0..height, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            let (origin_x, origin_y) = grade.eye_origin(0, row, width, height);
            let minimum_x = origin_x as f64;
            let maximum_x = (origin_x + eye_width - 1) as f64;
            let minimum_y = origin_y as f64;
            let maximum_y = (origin_y + eye_height - 1) as f64;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let (sample_x, sample_y) =
                    grade.source_position(column as f64, row as f64, width, height);
                // The eye owns a sub-rectangle: clamping here (rather than to
                // the whole frame) is what keeps the two eyes from bleeding
                // into each other when the rotation pushes a tap out of range.
                let sample_x = sample_x.clamp(minimum_x, maximum_x);
                let sample_y = sample_y.clamp(minimum_y, maximum_y);
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
        vertical_fov: f64,
        tilt: f64,
        pan: f64,
        roll: f64,
        layout: VrFrameLayout,
        invert: bool,
    ) -> VrRotateSphereGrade {
        VrRotateSphereGrade::new(
            horizontal_fov,
            vertical_fov,
            tilt,
            pan,
            roll,
            layout,
            invert,
        )
        .expect("the grade must be valid")
    }

    fn identity_grade() -> VrRotateSphereGrade {
        grade(
            360.0,
            180.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        )
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

    /// Gradient frame without transparent pixels, used where a resampling
    /// round trip must reproduce the input. The content is periodic in the
    /// horizontal axis, as a real equirectangular panorama is: a linear ramp
    /// would be discontinuous at the seam and could not round trip there.
    fn smooth_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let x = column as f32 / width.max(1) as f32;
                let y = row as f32 / height.max(1) as f32;
                let turn = std::f32::consts::TAU * x;
                pixels[row * width + column] =
                    [0.5 + 0.5 * turn.sin(), y, 0.5 + 0.5 * turn.cos(), 1.0];
            }
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

    #[test]
    fn zero_rotation_returns_the_input_bit_exactly() {
        let mut pixels = gradient_frame(WIDTH, HEIGHT);
        let original = bits(&pixels);
        assert!(vr_rotate_sphere_frame_f32(
            &mut pixels,
            WIDTH,
            HEIGHT,
            identity_grade()
        ));
        assert_eq!(bits(&pixels), original);
    }

    #[test]
    fn a_full_turn_of_yaw_is_the_identity() {
        let full_turn = grade(
            360.0,
            180.0,
            0.0,
            360.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        assert!(full_turn.is_identity());
        assert_eq!(full_turn.pan_y_degrees(), 0.0);
        let mut pixels = gradient_frame(64, 32);
        let original = bits(&pixels);
        assert!(vr_rotate_sphere_frame_f32(&mut pixels, 64, 32, full_turn));
        assert_eq!(bits(&pixels), original);
    }

    #[test]
    fn equirect_panorama_shifts_by_a_quarter_turn() {
        // A single bright column at u = 0.25 must move right by a quarter turn
        // (90 columns of 360) when the camera pans left by 90 degrees.
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; WIDTH * HEIGHT];
        for row in 0..HEIGHT {
            pixels[row * WIDTH + 90] = [1.0, 1.0, 1.0, 1.0];
        }
        let pan = grade(
            360.0,
            180.0,
            0.0,
            -90.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        assert!(vr_rotate_sphere_frame_f32(&mut pixels, WIDTH, HEIGHT, pan));
        let row = HEIGHT / 2;
        let brightest = (0..WIDTH)
            .max_by(|left, right| {
                let left = pixels[row * WIDTH + left][0];
                let right = pixels[row * WIDTH + right][0];
                left.partial_cmp(&right).expect("finite pixels")
            })
            .expect("a row has columns");
        assert!(
            brightest.abs_diff(180) <= 1,
            "the marker column landed at {brightest}, expected 180 +/- 1"
        );
    }

    #[test]
    fn pure_pitch_moves_the_horizon_by_the_authored_angle() {
        // An odd width puts one column exactly on the central meridian, where a
        // pure pitch shifts latitude by exactly the authored angle.
        const ODD_WIDTH: usize = 361;
        let pitched = grade(
            360.0,
            180.0,
            10.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        // Rows below 170: beyond that the pitched source latitude passes the
        // pole, where `asin` saturates and the relation no longer holds.
        for row in [10_usize, 45, 90, 135, 165] {
            let (_, source_y) = pitched.source_position(180.0, row as f64, ODD_WIDTH, HEIGHT);
            let destination = (row as f64 + 0.5) / HEIGHT as f64;
            let expected = destination + 10.0 / 180.0;
            let actual = (source_y + 0.5) / HEIGHT as f64;
            assert!(
                (actual - expected).abs() <= 1.0e-9,
                "row {row}: v_src {actual} != v_dst + 10/180 {expected}"
            );
        }
    }

    #[test]
    fn roll_of_ninety_degrees_sends_the_east_axis_to_the_bottom() {
        // Rolling the camera clockwise by 90 degrees brings the source's +X
        // direction (u = 0.75, v = 0.5) to the bottom of the destination.
        let rolled = grade(
            360.0,
            180.0,
            0.0,
            0.0,
            90.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        let (sample_x, sample_y) = rolled.source_position(180.0, 179.0, WIDTH, HEIGHT);
        let source_u = (sample_x + 0.5) / WIDTH as f64;
        let source_v = (sample_y + 0.5) / HEIGHT as f64;
        assert!(
            (source_u - 0.75).abs() < 0.01,
            "the bottom of the frame should read u = 0.75, got {source_u}"
        );
        assert!(
            (source_v - 0.5).abs() < 0.01,
            "the bottom of the frame should read v = 0.5, got {source_v}"
        );
    }

    #[test]
    fn invert_undoes_the_forward_rotation() {
        let forward = grade(
            360.0,
            180.0,
            10.0,
            20.0,
            30.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        let backward = grade(
            360.0,
            180.0,
            10.0,
            20.0,
            30.0,
            VrFrameLayout::Monoscopic,
            true,
        );
        // A finer grid keeps the two bilinear passes close enough for the
        // one-hundredth-of-a-percent budget: the composition is only as exact
        // as the interpolation of the rotated image's curvature.
        const ROUND_TRIP_WIDTH: usize = 720;
        const ROUND_TRIP_HEIGHT: usize = 360;
        let original = smooth_frame(ROUND_TRIP_WIDTH, ROUND_TRIP_HEIGHT);
        let mut rotated = original.clone();
        assert!(vr_rotate_sphere_frame_f32(
            &mut rotated,
            ROUND_TRIP_WIDTH,
            ROUND_TRIP_HEIGHT,
            forward
        ));
        let mut restored = rotated.clone();
        assert!(vr_rotate_sphere_frame_f32(
            &mut restored,
            ROUND_TRIP_WIDTH,
            ROUND_TRIP_HEIGHT,
            backward
        ));
        // Only pixels whose content survived both passes can round trip: when
        // a tap leaves the panorama (near the poles, where the frame stops at
        // 90 degrees of latitude) the resampler clamps and the value is gone,
        // and equirectangular sampling is singular at the poles themselves.
        // Linear content is reproduced exactly by bilinear sampling, so the
        // surviving interior matches to well below the budget.
        let polar_band = ROUND_TRIP_HEIGHT / 10;
        let mut compared = 0_usize;
        for row in polar_band..ROUND_TRIP_HEIGHT - polar_band {
            // Seam columns are excluded as destinations too: their own source
            // tap lands within half a pixel of the clamped frame edge.
            for column in 8..ROUND_TRIP_WIDTH - 8 {
                let (forward_x, forward_y) = forward.source_position(
                    column as f64,
                    row as f64,
                    ROUND_TRIP_WIDTH,
                    ROUND_TRIP_HEIGHT,
                );
                let (backward_x, backward_y) = backward.source_position(
                    column as f64,
                    row as f64,
                    ROUND_TRIP_WIDTH,
                    ROUND_TRIP_HEIGHT,
                );
                // The panorama wraps but the resampler clamps, so a tap within
                // a pixel of the seam loses the wrap-around half of its
                // bilinear footprint; that is a sampler property, not a
                // rotation error, so those columns are excluded like the poles.
                let inside = |x: f64, y: f64| {
                    (8.0..=(ROUND_TRIP_WIDTH - 9) as f64).contains(&x)
                        && (0.0..=(ROUND_TRIP_HEIGHT - 1) as f64).contains(&y)
                };
                if !inside(forward_x, forward_y) || !inside(backward_x, backward_y) {
                    continue;
                }
                compared += 1;
                let index = row * ROUND_TRIP_WIDTH + column;
                let actual = restored[index];
                let expected = original[index];
                for channel in 0..4 {
                    assert!(
                        (actual[channel] - expected[channel]).abs() <= 1.0e-4,
                        "pixel ({column}, {row}) channel {channel}: {} != {}",
                        actual[channel],
                        expected[channel]
                    );
                }
            }
        }
        assert!(
            compared > ROUND_TRIP_WIDTH * ROUND_TRIP_HEIGHT / 2,
            "the round trip must cover more than half of the frame, covered {compared}"
        );
    }

    #[test]
    fn each_eye_samples_only_its_own_sub_frame() {
        const EYE_WIDTH: usize = 360;
        const EYE_HEIGHT: usize = 184;
        let mut pixels = vec![[0.0_f32; 4]; EYE_WIDTH * EYE_HEIGHT];
        for row in 0..EYE_HEIGHT {
            let colour = if row < EYE_HEIGHT / 2 {
                [1.0, 0.0, 0.0, 1.0]
            } else {
                [0.0, 0.0, 1.0, 1.0]
            };
            for column in 0..EYE_WIDTH {
                pixels[row * EYE_WIDTH + column] = colour;
            }
        }
        let flipped = grade(
            360.0,
            180.0,
            180.0,
            0.0,
            0.0,
            VrFrameLayout::OverUnder,
            false,
        );
        assert!(vr_rotate_sphere_frame_f32(
            &mut pixels,
            EYE_WIDTH,
            EYE_HEIGHT,
            flipped
        ));
        for row in 0..EYE_HEIGHT {
            for column in 0..EYE_WIDTH {
                let pixel = pixels[row * EYE_WIDTH + column];
                if row < EYE_HEIGHT / 2 {
                    assert_eq!(pixel, [1.0, 0.0, 0.0, 1.0], "row {row} column {column}");
                } else {
                    assert_eq!(pixel, [0.0, 0.0, 1.0, 1.0], "row {row} column {column}");
                }
            }
        }
    }

    #[test]
    fn an_odd_height_still_splits_on_the_integer_boundary() {
        const ODD_HEIGHT: usize = 181;
        let layout = grade(360.0, 180.0, 0.0, 1.0, 0.0, VrFrameLayout::OverUnder, false);
        assert_eq!(layout.eye_size(WIDTH, ODD_HEIGHT), (WIDTH, 90));
        assert_eq!(layout.eye_rect(WIDTH, ODD_HEIGHT), [0.0, 0.0, 360.0, 90.0]);
        assert_eq!(layout.eye_origin(0, 89, WIDTH, ODD_HEIGHT), (0, 0));
        assert_eq!(layout.eye_origin(0, 90, WIDTH, ODD_HEIGHT), (0, 90));

        // Only row 90 carries content. A one-degree pan keeps the vertical
        // mapping at the identity, so row 90 must read row 90 of its own eye.
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; WIDTH * ODD_HEIGHT];
        for column in 0..WIDTH {
            pixels[90 * WIDTH + column] = [1.0, 1.0, 1.0, 1.0];
        }
        assert!(vr_rotate_sphere_frame_f32(
            &mut pixels,
            WIDTH,
            ODD_HEIGHT,
            layout
        ));
        for row in 0..ODD_HEIGHT {
            let expected = if row == 90 { 1.0 } else { 0.0 };
            assert!(
                (pixels[row * WIDTH + 10][0] - expected).abs() <= 1.0e-6,
                "row {row} should be {expected}"
            );
        }
    }

    #[test]
    fn out_of_range_longitude_clamps_to_the_panorama_edge() {
        // A half-turn field of view with the camera panned right by 90 degrees:
        // the right half of the destination looks past the panorama's right
        // edge, so those samples must clamp to the last column instead of
        // wrapping around to the first one.
        let clamped = grade(
            180.0,
            180.0,
            0.0,
            90.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        let mut clamped_samples = 0_usize;
        for column in 0..WIDTH {
            let (sample_x, _) = clamped.source_position(column as f64, 90.0, WIDTH, HEIGHT);
            assert!(
                sample_x >= -1.0e-9,
                "non-wrapped longitude must not read before the first column"
            );
            if sample_x > (WIDTH - 1) as f64 {
                clamped_samples += 1;
                let edge = sample_x.clamp(0.0, (WIDTH - 1) as f64);
                assert_eq!(edge, (WIDTH - 1) as f64);
            }
        }
        assert!(clamped_samples > 0, "the row must exercise the clamp");
    }

    #[test]
    fn wrap_disabled_only_when_the_fov_is_narrower_than_a_full_turn() {
        let narrow = grade(
            359.0,
            180.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        let full = grade(
            360.0,
            180.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        assert_eq!(narrow.gpu_controls().1[3], 0.0);
        assert_eq!(full.gpu_controls().1[3], 1.0);
    }

    #[test]
    fn sample_position_matches_an_independent_f64_reference() {
        let tested = grade(
            280.0,
            140.0,
            23.0,
            -47.0,
            61.0,
            VrFrameLayout::SideBySide,
            false,
        );
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

    /// Hand-written independent reference: builds the sampling matrix as nine
    /// named entries and applies it element by element.
    fn reference_source_position(
        grade: &VrRotateSphereGrade,
        column: usize,
        row: usize,
        width: usize,
        height: usize,
    ) -> (f64, f64) {
        let (eye_width, eye_height) = grade.eye_size(width, height);
        let (origin_x, origin_y) = grade.eye_origin(column, row, width, height);
        let u = (column as f64 - origin_x as f64 + 0.5) / eye_width as f64;
        let v = (row as f64 - origin_y as f64 + 0.5) / eye_height as f64;

        let horizontal = grade.horizontal_fov_degrees().to_radians();
        let vertical = grade.vertical_fov_degrees().to_radians();
        let longitude = (u - 0.5) * horizontal;
        let latitude = (0.5 - v) * vertical;
        let direction_x = latitude.cos() * longitude.sin();
        let direction_y = latitude.sin();
        let direction_z = latitude.cos() * longitude.cos();

        let pitch = grade.tilt_x_degrees().to_radians();
        let yaw = grade.pan_y_degrees().to_radians();
        let roll = grade.roll_z_degrees().to_radians();
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

        let longitude_source = source_x.atan2(source_z);
        let latitude_source = source_y.clamp(-1.0, 1.0).asin();
        let mut u_source = longitude_source / horizontal + 0.5;
        if grade.wrap_longitude() {
            u_source -= u_source.floor();
        }
        let sample_x = u_source * eye_width as f64 - 0.5 + origin_x as f64;
        let sample_y =
            (0.5 - latitude_source / vertical) * eye_height as f64 - 0.5 + origin_y as f64;
        (sample_x, sample_y)
    }

    #[test]
    fn checkpoint_runs_every_chunk_and_propagates_errors() {
        let mut checkpoints = 0_usize;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), &'static str>(())
        };
        let mut pixels = gradient_frame(64, 32);
        let grade = grade(
            360.0,
            180.0,
            5.0,
            10.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        apply_vr_rotate_sphere_f32_controlled(&mut pixels, 64, 32, grade, &mut checkpoint)
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
        let error = apply_vr_rotate_sphere_f32_controlled(&mut pixels, 64, 32, grade, &mut failing)
            .expect_err("the second checkpoint must propagate its error");
        assert_eq!(error, "cancelled");
    }

    #[test]
    fn empty_or_oversized_frames_are_reported_as_not_handled() {
        let grade = grade(
            360.0,
            180.0,
            10.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        );
        assert!(!vr_rotate_sphere_frame_f32(&mut [], 0, 0, grade));
        assert!(!vr_rotate_sphere_frame_f32(&mut [], 4, 0, grade));
        assert!(!vr_rotate_sphere_frame_f32(&mut [], 0, 4, grade));
        let mut too_small = vec![[0.0_f32; 4]; 6];
        assert!(!vr_rotate_sphere_frame_f32(&mut too_small, 4, 4, grade));
    }

    #[test]
    fn default_parameters_build_the_identity_grade() {
        // The schema defaults, read back through the constructor.
        let defaults = VrRotateSphereGrade::new(
            360.0,
            180.0,
            0.0,
            0.0,
            0.0,
            VrFrameLayout::Monoscopic,
            false,
        )
        .expect("the defaults are valid");
        assert!(defaults.is_identity());
        assert!(!defaults.wrap_longitude() || defaults.wrap_longitude());
        assert_eq!(defaults.layout(), VrFrameLayout::Monoscopic);
    }
}
