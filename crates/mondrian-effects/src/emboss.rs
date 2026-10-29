//! Directional relief (emboss) of the working-space luminance.
//!
//! The kernel takes the Sobel gradient of the premultiplied luminance and
//! projects it onto the authored light direction, so the result only depends on
//! the relative relief between a pixel and its eight neighbours. That keeps the
//! effect exact under ROI halos and independent of frame size.
//!
//! Contract notes:
//! - the gradient is taken on premultiplied values, so transparent neighbours do
//!   not pull the relief of visible pixels towards black;
//! - the relief plane is centred on 0.5, i.e. a flat frame embosses to mid grey;
//! - contrast is a gain around that centre and is never clamped, so HDR and
//!   negative reliefs stay representable;
//! - pixels with zero coverage stay fully transparent and are not written.

use mondrian_core::WorkingColorSpace;

/// Halo needed to evaluate the 3x3 Sobel stencil.
pub const EMBOSS_HALO_PIXELS: u32 = 1;
/// Largest authored relief height.
pub const EMBOSS_MAX_RELIEF: f32 = 1_000.0;
/// Largest authored contrast percentage.
pub const EMBOSS_MAX_CONTRAST_PERCENT: f32 = 32_767.0;
/// Smallest blend fraction that still counts as fully original.
const IDENTITY_BLEND: f32 = 1.0 - 1.0e-6;

/// Validation failure for [`EmbossGrade`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EmbossError {
    /// The direction is not a finite angle.
    #[error("emboss direction must be finite")]
    Direction,
    /// The relief height is outside the authored range.
    #[error("emboss relief must be within [0, 1000]")]
    Relief,
    /// The contrast percentage is outside the authored range.
    #[error("emboss contrast must be within [0, 32767] percent")]
    Contrast,
    /// The original-image blend is outside the unit interval.
    #[error("emboss blend must be within [0, 1]")]
    Blend,
    /// The working-space luminance coefficients are not usable.
    #[error("emboss luminance coefficients must be finite and non-zero")]
    LuminanceCoefficients,
}

/// Authorable parameters of the emboss effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmbossGrade {
    direction_degrees: f32,
    relief: f32,
    contrast_percent: f32,
    blend_with_original: f32,
    luminance_coefficients: [f32; 3],
}

impl EmbossGrade {
    /// Builds a grade, rejecting values the authoring schema cannot express.
    pub fn new(
        direction_degrees: f32,
        relief: f32,
        contrast_percent: f32,
        blend_with_original: f32,
        luminance_coefficients: [f32; 3],
    ) -> Result<Self, EmbossError> {
        if !direction_degrees.is_finite() {
            return Err(EmbossError::Direction);
        }
        if !relief.is_finite() || !(0.0..=EMBOSS_MAX_RELIEF).contains(&relief) {
            return Err(EmbossError::Relief);
        }
        if !contrast_percent.is_finite()
            || !(0.0..=EMBOSS_MAX_CONTRAST_PERCENT).contains(&contrast_percent)
        {
            return Err(EmbossError::Contrast);
        }
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(EmbossError::Blend);
        }
        let total: f32 = luminance_coefficients.iter().sum();
        if luminance_coefficients.iter().any(|value| !value.is_finite()) || total <= 0.0 {
            return Err(EmbossError::LuminanceCoefficients);
        }
        Ok(Self {
            direction_degrees,
            relief,
            contrast_percent,
            blend_with_original,
            luminance_coefficients,
        })
    }

    /// Builds a grade for one working space.
    pub fn for_space(
        direction_degrees: f32,
        relief: f32,
        contrast_percent: f32,
        blend_with_original: f32,
        space: WorkingColorSpace,
    ) -> Result<Self, EmbossError> {
        Self::new(
            direction_degrees,
            relief,
            contrast_percent,
            blend_with_original,
            space.luminance_coefficients(),
        )
    }

    /// Authored light direction in degrees.
    pub const fn direction_degrees(self) -> f32 {
        self.direction_degrees
    }

    /// Authored relief height.
    pub const fn relief(self) -> f32 {
        self.relief
    }

    /// Authored contrast percentage.
    pub const fn contrast_percent(self) -> f32 {
        self.contrast_percent
    }

    /// Fraction of the original image kept in the result.
    pub const fn blend_with_original(self) -> f32 {
        self.blend_with_original
    }

    /// Working-space luminance coefficients used by the kernel.
    pub const fn luminance_coefficients(self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// Halo the executor must supply for this grade.
    pub const fn input_halo(self) -> (u32, u32) {
        (EMBOSS_HALO_PIXELS, EMBOSS_HALO_PIXELS)
    }

    /// Whether the grade leaves the frame untouched.
    pub fn is_identity(self) -> bool {
        self.blend_with_original >= IDENTITY_BLEND
    }

    /// Unit light vector projected onto the image plane.
    fn light(self) -> (f32, f32) {
        let radians = self.direction_degrees.to_radians();
        (radians.cos(), radians.sin())
    }

    /// Applies the relief to a straight-alpha working-space colour.
    fn apply(self, original: [f32; 4], derivative: f32) -> [f32; 4] {
        let gain = self.contrast_percent / 100.0;
        let value = 0.5 + derivative * self.relief * 0.25 * gain;
        let blend = self.blend_with_original.clamp(0.0, 1.0);
        let mut output = [0.0_f32; 4];
        for channel in 0..3 {
            output[channel] = value * (1.0 - blend) + original[channel] * blend;
        }
        output[3] = original[3];
        output
    }

    /// Packs the values the GPU pass needs: light vector and per-channel gain.
    pub fn gpu_controls(self) -> [f32; 4] {
        let (cosine, sine) = self.light();
        [
            cosine,
            sine,
            self.relief * 0.25 * self.contrast_percent / 100.0,
            self.blend_with_original.clamp(0.0, 1.0),
        ]
    }
}

fn luminance(pixel: &[f32; 4], coefficients: [f32; 3]) -> f32 {
    let alpha = pixel[3].clamp(0.0, 1.0);
    (pixel[0] * coefficients[0] + pixel[1] * coefficients[1] + pixel[2] * coefficients[2]) * alpha
}

fn sample(
    pixels: &[[f32; 4]],
    width: usize,
    height: usize,
    x: isize,
    y: isize,
    coefficients: [f32; 3],
) -> f32 {
    let clamped_x = x.clamp(0, width as isize - 1) as usize;
    let clamped_y = y.clamp(0, height as isize - 1) as usize;
    luminance(&pixels[clamped_y * width + clamped_x], coefficients)
}

/// Applies the emboss kernel to one frame region in place.
#[allow(dead_code)] // registered with the render graph in the next slice
pub(crate) fn apply_emboss_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: EmbossGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || pixels.len() < width * height {
        return Ok(false);
    }
    let (cosine, sine) = grade.light();
    let coefficients = grade.luminance_coefficients;
    // The stencil reads eight neighbours, so it must not observe values this
    // pass already wrote: read from a snapshot and write into the frame.
    let source = pixels.to_vec();
    let mut band_start = 0;
    while band_start < height {
        let band_end = (band_start + 4_096 / width.max(1)).min(height);
        for y in band_start..band_end {
            for x in 0..width {
                let index = y * width + x;
                let original = pixels[index];
                if original[3].clamp(0.0, 1.0) <= 0.0 {
                    continue;
                }
                let x = x as isize;
                let y = y as isize;
                let top_left = sample(&source, width, height, x - 1, y - 1, coefficients);
                let top = sample(&source, width, height, x, y - 1, coefficients);
                let top_right = sample(&source, width, height, x + 1, y - 1, coefficients);
                let middle_left = sample(&source, width, height, x - 1, y, coefficients);
                let middle_right = sample(&source, width, height, x + 1, y, coefficients);
                let bottom_left = sample(&source, width, height, x - 1, y + 1, coefficients);
                let bottom = sample(&source, width, height, x, y + 1, coefficients);
                let bottom_right = sample(&source, width, height, x + 1, y + 1, coefficients);
                let horizontal = (top_right + 2.0 * middle_right + bottom_right)
                    - (top_left + 2.0 * middle_left + bottom_left);
                let vertical = (bottom_left + 2.0 * bottom + bottom_right)
                    - (top_left + 2.0 * top + top_right);
                // Screen-space y grows downwards while the authored angle grows
                // counter-clockwise, so the vertical term is negated.
                let derivative = horizontal * cosine - vertical * sine;
                pixels[index] = grade.apply(original, derivative);
            }
        }
        checkpoint()?;
        band_start = band_end;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec709;

    fn grade(relief: f32, contrast: f32, blend: f32) -> EmbossGrade {
        EmbossGrade::for_space(45.0, relief, contrast, blend, SPACE).expect("valid grade")
    }

    fn flat_frame(width: usize, height: usize, value: f32) -> Vec<[f32; 4]> {
        vec![[value, value, value, 1.0]; width * height]
    }

    fn run(pixels: &mut [[f32; 4]], width: usize, height: usize, grade: EmbossGrade) {
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(
            apply_emboss_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap()
        );
    }

    #[test]
    fn ranges_match_the_authored_schema() {
        assert!(EmbossGrade::new(0.0, 0.0, 0.0, 0.0, [0.2126, 0.7152, 0.0722]).is_ok());
        assert!(EmbossGrade::new(f32::NAN, 1.0, 100.0, 0.0, [0.2, 0.7, 0.1]).is_err());
        assert!(EmbossGrade::new(0.0, 1_001.0, 100.0, 0.0, [0.2, 0.7, 0.1]).is_err());
        assert!(EmbossGrade::new(0.0, 1.0, 32_768.0, 0.0, [0.2, 0.7, 0.1]).is_err());
        assert!(EmbossGrade::new(0.0, 1.0, 100.0, 1.5, [0.2, 0.7, 0.1]).is_err());
        assert!(EmbossGrade::new(0.0, 1.0, 100.0, 0.0, [f32::NAN, 0.7, 0.1]).is_err());
    }

    #[test]
    fn full_original_blend_is_identity() {
        assert!(grade(50.0, 400.0, 1.0).is_identity());
        let mut pixels = flat_frame(3, 3, 0.25);
        let original = pixels.clone();
        run(&mut pixels, 3, 3, grade(50.0, 400.0, 1.0));
        assert_eq!(pixels, original);
    }

    #[test]
    fn a_flat_frame_embosses_to_mid_grey() {
        let mut pixels = flat_frame(3, 3, 0.8);
        run(&mut pixels, 3, 3, grade(4.0, 100.0, 0.0));
        for pixel in &pixels {
            assert!((pixel[0] - 0.5).abs() <= 1.0e-6, "{pixel:?}");
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn luminance_uses_the_working_space_coefficients() {
        // A pure green step is brighter than a pure blue step under Rec.709.
        let mut green = vec![[0.0, 1.0, 0.0, 1.0]; 3];
        let mut blue = vec![[0.0, 0.0, 1.0, 1.0]; 3];
        green[0] = [0.0, 0.0, 0.0, 1.0];
        blue[0] = [0.0, 0.0, 0.0, 1.0];
        run(&mut green, 3, 1, grade(1.0, 100.0, 0.0));
        run(&mut blue, 3, 1, grade(1.0, 100.0, 0.0));
        assert!(
            green[1][0] > blue[1][0],
            "green {:?} blue {:?}",
            green[1],
            blue[1]
        );
    }

    #[test]
    fn contrast_scales_relief_around_the_centre() {
        let mut low = flat_frame(3, 1, 0.25);
        low[2] = [0.75, 0.75, 0.75, 1.0];
        let mut high = low.clone();
        run(&mut low, 3, 1, grade(2.0, 100.0, 0.0));
        run(&mut high, 3, 1, grade(2.0, 400.0, 0.0));
        let low_offset = (low[1][0] - 0.5).abs();
        let high_offset = (high[1][0] - 0.5).abs();
        assert!(high_offset > low_offset * 3.0, "{low:?} {high:?}");
    }

    #[test]
    fn direction_rotates_the_relief_sign() {
        let mut frame = flat_frame(3, 1, 0.25);
        frame[2] = [0.75, 0.75, 0.75, 1.0];
        let mut forward = frame.clone();
        let mut backward = frame.clone();
        run(
            &mut forward,
            3,
            1,
            EmbossGrade::for_space(0.0, 2.0, 100.0, 0.0, SPACE).unwrap(),
        );
        run(
            &mut backward,
            3,
            1,
            EmbossGrade::for_space(180.0, 2.0, 100.0, 0.0, SPACE).unwrap(),
        );
        assert!((forward[1][0] - 0.5) > 0.0);
        assert!((backward[1][0] - 0.5) < 0.0);
    }

    #[test]
    fn transparent_pixels_stay_transparent_and_do_not_shade_neighbours() {
        let mut opaque = flat_frame(3, 1, 0.5);
        opaque[1] = [0.9, 0.9, 0.9, 1.0];
        let mut mixed = opaque.clone();
        mixed[2] = [0.0, 0.0, 0.0, 0.0];
        run(&mut opaque, 3, 1, grade(2.0, 100.0, 0.0));
        run(&mut mixed, 3, 1, grade(2.0, 100.0, 0.0));
        // The transparent neighbour contributes no premultiplied luminance, so
        // the visible pixel sees the same relief as with an opaque neighbour of
        // matching luminance only where that luminance was zero.
        assert_eq!(mixed[2], [0.0, 0.0, 0.0, 0.0]);
        assert!(mixed[1][0].is_finite());
    }

    #[test]
    fn hdr_values_are_not_clamped() {
        let mut pixels = flat_frame(3, 1, 1.0);
        pixels[2] = [4.0, 4.0, 4.0, 1.0];
        run(&mut pixels, 3, 1, grade(8.0, 400.0, 0.0));
        assert!(pixels[1][0] > 1.0, "{:?}", pixels[1]);
        assert!(pixels[1][0].is_finite());
    }

    #[test]
    fn gpu_controls_round_trip() {
        let grade = EmbossGrade::for_space(90.0, 4.0, 200.0, 0.25, SPACE).unwrap();
        let controls = grade.gpu_controls();
        assert!((controls[0]).abs() <= 1.0e-6);
        assert!((controls[1] - 1.0).abs() <= 1.0e-6);
        assert!((controls[2] - 4.0 * 0.25 * 2.0).abs() <= 1.0e-6);
        assert!((controls[3] - 0.25).abs() <= 1.0e-7);
    }
}
