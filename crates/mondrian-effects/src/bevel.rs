//! Bevel shading over the frame's coverage edge.
//!
//! Bevel reads the alpha channel, converts the local coverage gradient into a
//! surface normal, and adds a directional highlight or shadow next to the
//! edge. The shading term is an additive light contribution, so extended-range
//! colours keep their magnitude instead of being clamped into the display
//! range.

use thiserror::Error;

/// Pixels of source neighbourhood the kernel reads on every side.
pub const BEVEL_HALO_PIXELS: u32 = 1;
/// Largest bevel thickness the authoring surface accepts.
pub const BEVEL_MAX_THICKNESS: f32 = 200.0;

/// Divisor that turns a 3x3 `[1, 2, 1]` Sobel response on a hard edge into a
/// unit gradient, so the authored thickness reads as a slope rather than a
/// multiple of the kernel weights.
const GRADIENT_NORMALISATION: f32 = 4.0;
const IDENTITY_INTENSITY: f32 = 1.0e-6;
const IDENTITY_THICKNESS: f32 = 1.0e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BevelError {
    #[error("bevel thickness must be finite and within [0, 200] pixels")]
    Thickness,
    #[error("bevel light angle must be finite degrees")]
    Angle,
    #[error("bevel light colour components must be finite")]
    LightColor,
    #[error("bevel light intensity must be finite and within [0, 1]")]
    Intensity,
}

/// Authored bevel configuration.
///
/// `angle_degrees` is a compass bearing: zero points up, 90 points right and
/// 135 points down-right, matching the drop shadow convention.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BevelGrade {
    thickness: f32,
    angle_degrees: f32,
    light_color: [f32; 3],
    intensity: f32,
}

impl BevelGrade {
    pub fn new(
        thickness: f32,
        angle_degrees: f32,
        light_color: [f32; 3],
        intensity: f32,
    ) -> Result<Self, BevelError> {
        if !thickness.is_finite() || !(0.0..=BEVEL_MAX_THICKNESS).contains(&thickness) {
            return Err(BevelError::Thickness);
        }
        if !angle_degrees.is_finite() {
            return Err(BevelError::Angle);
        }
        if !light_color.iter().all(|value| value.is_finite()) {
            return Err(BevelError::LightColor);
        }
        if !intensity.is_finite() || !(0.0..=1.0).contains(&intensity) {
            return Err(BevelError::Intensity);
        }
        Ok(Self { thickness, angle_degrees, light_color, intensity })
    }

    pub fn thickness(&self) -> f32 {
        self.thickness
    }

    pub fn angle_degrees(&self) -> f32 {
        self.angle_degrees
    }

    pub fn light_color(&self) -> [f32; 3] {
        self.light_color
    }

    pub fn intensity(&self) -> f32 {
        self.intensity
    }

    /// No coverage edge and no light contribution leave the frame alone.
    pub fn is_identity(&self) -> bool {
        self.intensity <= IDENTITY_INTENSITY || self.thickness <= IDENTITY_THICKNESS
    }

    /// Source neighbourhood the kernel needs on every side.
    pub fn input_halo(&self) -> (u32, u32) {
        (BEVEL_HALO_PIXELS, BEVEL_HALO_PIXELS)
    }

    /// Unit light vector: compass bearing on the image plane plus a heading
    /// towards the viewer so flat areas still receive a stable fraction.
    pub fn light(&self) -> [f32; 3] {
        let radians = self.angle_degrees.to_radians();
        let x = radians.sin();
        let y = -radians.cos();
        let length = (x * x + y * y + 1.0).sqrt();
        [x / length, y / length, 1.0 / length]
    }

    /// Thickness folded into `[0, 1)` so a zero thickness is a perfect identity
    /// and large values approach a fully tilted edge.
    pub fn tilt(&self) -> f32 {
        self.thickness / (self.thickness + 1.0)
    }

    /// Signed lambert term for a unit coverage gradient.
    pub fn lambert(&self, gradient: [f32; 2]) -> f32 {
        let tilt = self.tilt();
        let nx = -gradient[0] * tilt;
        let ny = -gradient[1] * tilt;
        let inverse = 1.0 / (nx * nx + ny * ny + 1.0).sqrt();
        let light = self.light();
        ((nx * inverse) * light[0] + (ny * inverse) * light[1] + inverse * light[2])
            .clamp(-1.0, 1.0)
    }

    /// Applies the bevel to one straight-alpha pixel.
    ///
    /// `gradient` is the unit coverage gradient and `edge` is its magnitude in
    /// `[0, 1]`; the shading only acts where the coverage actually changes.
    pub fn apply(&self, pixel: [f32; 4], gradient: [f32; 2], edge: f32) -> [f32; 4] {
        if edge <= 0.0 || self.is_identity() {
            return pixel;
        }
        let shade = (self.lambert(gradient) - 0.5) * edge * self.intensity;
        [
            pixel[0] + self.light_color[0] * shade,
            pixel[1] + self.light_color[1] * shade,
            pixel[2] + self.light_color[2] * shade,
            pixel[3],
        ]
    }

    /// Packed values the GPU pass needs: tilt, light x, light y, intensity.
    pub fn gpu_controls(&self) -> [f32; 4] {
        let light = self.light();
        [self.tilt(), light[0], light[1], self.intensity]
    }
}

fn alpha_at(source: &[[f32; 4]], width: usize, height: usize, x: i32, y: i32) -> f32 {
    let x = x.clamp(0, width as i32 - 1) as usize;
    let y = y.clamp(0, height as i32 - 1) as usize;
    source[y * width + x][3].clamp(0.0, 1.0)
}

/// Applies the bevel in place from a snapshot of the source frame.
pub(crate) fn apply_bevel_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &BevelGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || pixels.len() != width.saturating_mul(height) {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let source = pixels.to_vec();
    checkpoint()?;
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let index = y as usize * width + x as usize;
            let centre = source[index][3].clamp(0.0, 1.0);
            if centre <= 0.0 {
                pixels[index] = [0.0; 4];
                continue;
            }
            let top_left = alpha_at(&source, width, height, x - 1, y - 1);
            let top = alpha_at(&source, width, height, x, y - 1);
            let top_right = alpha_at(&source, width, height, x + 1, y - 1);
            let left = alpha_at(&source, width, height, x - 1, y);
            let right = alpha_at(&source, width, height, x + 1, y);
            let bottom_left = alpha_at(&source, width, height, x - 1, y + 1);
            let bottom = alpha_at(&source, width, height, x, y + 1);
            let bottom_right = alpha_at(&source, width, height, x + 1, y + 1);
            let horizontal =
                (top_right + 2.0 * right + bottom_right) - (top_left + 2.0 * left + bottom_left);
            let vertical =
                (bottom_left + 2.0 * bottom + bottom_right) - (top_left + 2.0 * top + top_right);
            let gradient = [
                horizontal / GRADIENT_NORMALISATION,
                vertical / GRADIENT_NORMALISATION,
            ];
            let edge = horizontal
                .hypot(vertical)
                .mul_add(1.0 / GRADIENT_NORMALISATION, 0.0)
                .clamp(0.0, 1.0);
            pixels[index] = grade.apply(source[index], gradient, edge);
        }
        if y % 512 == 0 {
            checkpoint()?;
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Whole-frame helper shared by tests and the renderer parity check.
pub fn bevel_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &BevelGrade,
) -> bool {
    match apply_bevel_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    }) {
        Ok(applied) => applied,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(thickness: f32, angle: f32, intensity: f32) -> BevelGrade {
        BevelGrade::new(thickness, angle, [1.0, 1.0, 1.0], intensity).expect("valid bevel")
    }

    fn opaque_ramp(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| [0.5, 0.5, 0.5, if index % width == 0 { 0.0 } else { 1.0 }])
            .collect()
    }

    #[test]
    fn range_validation_rejects_impossible_parameters() {
        assert_eq!(
            BevelGrade::new(-1.0, 0.0, [1.0, 1.0, 1.0], 0.5).unwrap_err(),
            BevelError::Thickness
        );
        assert_eq!(
            BevelGrade::new(2.0, f32::NAN, [1.0, 1.0, 1.0], 0.5).unwrap_err(),
            BevelError::Angle
        );
        assert_eq!(
            BevelGrade::new(2.0, 0.0, [1.0, f32::INFINITY, 1.0], 0.5).unwrap_err(),
            BevelError::LightColor
        );
        assert_eq!(
            BevelGrade::new(2.0, 0.0, [1.0, 1.0, 1.0], 1.5).unwrap_err(),
            BevelError::Intensity
        );
    }

    #[test]
    fn zero_thickness_or_intensity_is_identity() {
        assert!(grade(0.0, 45.0, 0.5).is_identity());
        assert!(grade(2.0, 45.0, 0.0).is_identity());
        let mut frame = opaque_ramp(3, 1);
        let original = frame.clone();
        assert!(bevel_frame_f32(&mut frame, 3, 1, &grade(0.0, 45.0, 0.5)));
        assert_eq!(frame, original);
    }

    #[test]
    fn flat_coverage_is_left_untouched() {
        let mut frame = vec![[0.25, 0.5, 0.75, 1.0]; 9];
        let original = frame.clone();
        assert!(bevel_frame_f32(&mut frame, 3, 3, &grade(20.0, 0.0, 1.0)));
        assert_eq!(frame, original);
    }

    #[test]
    fn opposite_light_angles_flip_the_edge_shading() {
        let frame = opaque_ramp(3, 1);
        let mut lit = frame.clone();
        let mut unlit = frame.clone();
        // The ramp only has a horizontal coverage gradient, so the light has
        // to point along x for the two bearings to differ.
        assert!(bevel_frame_f32(&mut lit, 3, 1, &grade(2.0, 270.0, 1.0)));
        assert!(bevel_frame_f32(&mut unlit, 3, 1, &grade(2.0, 90.0, 1.0)));
        assert!(lit[1][0] > 0.5);
        assert!(unlit[1][0] < 0.5);
        assert!(lit[1][0] > unlit[1][0]);
    }

    #[test]
    fn intensity_scales_the_light_contribution() {
        let frame = opaque_ramp(3, 1);
        let mut soft = frame.clone();
        let mut hard = frame.clone();
        assert!(bevel_frame_f32(&mut soft, 3, 1, &grade(4.0, 0.0, 0.25)));
        assert!(bevel_frame_f32(&mut hard, 3, 1, &grade(4.0, 0.0, 1.0)));
        let soft_delta = soft[1][0] - 0.5;
        let hard_delta = hard[1][0] - 0.5;
        assert!(soft_delta > 0.0);
        assert!((hard_delta - soft_delta * 4.0).abs() < 1.0e-6);
    }

    #[test]
    fn light_colour_tints_the_highlight() {
        let frame = opaque_ramp(3, 1);
        let mut tinted = frame.clone();
        let grade = BevelGrade::new(4.0, 0.0, [1.0, 0.0, 0.0], 1.0).expect("valid bevel");
        assert!(bevel_frame_f32(&mut tinted, 3, 1, &grade));
        assert!(tinted[1][0] > 0.5);
        assert!((tinted[1][1] - 0.5).abs() < 1.0e-6);
        assert!((tinted[1][2] - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn extended_range_colours_are_not_clamped() {
        let mut frame = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        frame[0] = [0.0, 0.0, 0.0, 0.0];
        frame[1] = [2.0, 0.5, 0.0, 1.0];
        assert!(bevel_frame_f32(&mut frame, 3, 1, &grade(8.0, 0.0, 1.0)));
        assert!(frame[1][0] > 2.0);
    }

    #[test]
    fn transparent_pixels_stay_empty() {
        let mut frame = vec![[0.4, 0.4, 0.4, 0.0]; 4];
        assert!(bevel_frame_f32(&mut frame, 2, 2, &grade(8.0, 0.0, 1.0)));
        assert!(frame.iter().all(|pixel| *pixel == [0.0; 4]));
    }

    #[test]
    fn gpu_controls_round_trip() {
        let grade = BevelGrade::new(9.0, 30.0, [0.5, 0.25, 0.75], 0.6).expect("valid bevel");
        let controls = grade.gpu_controls();
        assert!((controls[0] - grade.tilt()).abs() < 1.0e-6);
        assert!((controls[3] - 0.6).abs() < 1.0e-6);
        let light = grade.light();
        assert!((controls[1] - light[0]).abs() < 1.0e-6);
        assert!((controls[2] - light[1]).abs() < 1.0e-6);
    }

    #[test]
    fn shading_reads_the_source_snapshot_only() {
        let grade = grade(3.0, 45.0, 0.8);
        let mut frame = vec![[0.25, 0.25, 0.25, 1.0]; 4];
        frame[0][3] = 0.0;
        let original = frame.clone();
        assert!(bevel_frame_f32(&mut frame, 2, 2, &grade));
        for index in 1..4 {
            let neighbours = [
                alpha_at(
                    &original,
                    2,
                    2,
                    (index % 2) as i32 - 1,
                    (index / 2) as i32 - 1,
                ),
                alpha_at(&original, 2, 2, (index % 2) as i32, (index / 2) as i32 - 1),
                alpha_at(
                    &original,
                    2,
                    2,
                    (index % 2) as i32 + 1,
                    (index / 2) as i32 - 1,
                ),
                alpha_at(&original, 2, 2, (index % 2) as i32 - 1, (index / 2) as i32),
                alpha_at(&original, 2, 2, (index % 2) as i32 + 1, (index / 2) as i32),
                alpha_at(
                    &original,
                    2,
                    2,
                    (index % 2) as i32 - 1,
                    (index / 2) as i32 + 1,
                ),
                alpha_at(&original, 2, 2, (index % 2) as i32, (index / 2) as i32 + 1),
                alpha_at(
                    &original,
                    2,
                    2,
                    (index % 2) as i32 + 1,
                    (index / 2) as i32 + 1,
                ),
            ];
            let horizontal = (neighbours[2] + 2.0 * neighbours[4] + neighbours[7])
                - (neighbours[0] + 2.0 * neighbours[3] + neighbours[5]);
            let vertical = (neighbours[5] + 2.0 * neighbours[6] + neighbours[7])
                - (neighbours[0] + 2.0 * neighbours[1] + neighbours[2]);
            let gradient = [horizontal / 4.0, vertical / 4.0];
            let edge = horizontal.hypot(vertical) / 4.0;
            let expected = grade.apply(original[index], gradient, edge.clamp(0.0, 1.0));
            assert_eq!(frame[index], expected);
        }
    }
}
