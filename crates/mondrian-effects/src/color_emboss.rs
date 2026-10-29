//! Colour relief (colour emboss) of the working-space colour channels.
//!
//! Unlike the luminance emboss, the Sobel gradient is taken per channel and each
//! channel keeps its own relief value, so neutral surfaces roll off with chroma
//! instead of collapsing to grey.
//!
//! Contract notes:
//! - the gradient is taken on premultiplied values, so transparent neighbours do
//!   not pull the relief of visible pixels towards black;
//! - every channel is centred on 0.5, i.e. a flat frame embosses to mid grey;
//! - contrast is a gain around that centre and is never clamped, so HDR and
//!   negative reliefs stay representable;
//! - pixels with zero coverage stay fully transparent and are not written.

/// Halo needed to evaluate the 3x3 Sobel stencil.
pub const COLOR_EMBOSS_HALO_PIXELS: u32 = 1;
/// Largest authored relief height.
pub const COLOR_EMBOSS_MAX_RELIEF: f32 = 1_000.0;
/// Largest authored contrast percentage.
pub const COLOR_EMBOSS_MAX_CONTRAST_PERCENT: f32 = 32_767.0;
/// Smallest blend fraction that still counts as fully original.
const IDENTITY_BLEND: f32 = 1.0 - 1.0e-6;

/// Validation failure for [`ColorEmbossGrade`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ColorEmbossError {
    /// The direction is not a finite angle.
    #[error("colour emboss direction must be finite")]
    Direction,
    /// The relief height is outside the authored range.
    #[error("colour emboss relief must be within [0, 1000]")]
    Relief,
    /// The contrast percentage is outside the authored range.
    #[error("colour emboss contrast must be within [0, 32767] percent")]
    Contrast,
    /// The original-image blend is outside the unit interval.
    #[error("colour emboss blend must be within [0, 1]")]
    Blend,
}

/// Authorable parameters of the colour emboss effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorEmbossGrade {
    direction_degrees: f32,
    relief: f32,
    contrast_percent: f32,
    blend_with_original: f32,
}

impl ColorEmbossGrade {
    /// Builds a grade, rejecting values the authoring schema cannot express.
    pub fn new(
        direction_degrees: f32,
        relief: f32,
        contrast_percent: f32,
        blend_with_original: f32,
    ) -> Result<Self, ColorEmbossError> {
        if !direction_degrees.is_finite() {
            return Err(ColorEmbossError::Direction);
        }
        if !relief.is_finite() || !(0.0..=COLOR_EMBOSS_MAX_RELIEF).contains(&relief) {
            return Err(ColorEmbossError::Relief);
        }
        if !contrast_percent.is_finite()
            || !(0.0..=COLOR_EMBOSS_MAX_CONTRAST_PERCENT).contains(&contrast_percent)
        {
            return Err(ColorEmbossError::Contrast);
        }
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(ColorEmbossError::Blend);
        }
        Ok(Self {
            direction_degrees,
            relief,
            contrast_percent,
            blend_with_original,
        })
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

    /// Halo the executor must supply for this grade.
    pub const fn input_halo(self) -> (u32, u32) {
        (COLOR_EMBOSS_HALO_PIXELS, COLOR_EMBOSS_HALO_PIXELS)
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

    /// Applies the relief to one straight-alpha working-space colour.
    fn apply(self, original: [f32; 4], derivative: [f32; 3]) -> [f32; 4] {
        let gain = self.relief * 0.25 * self.contrast_percent / 100.0;
        let blend = self.blend_with_original.clamp(0.0, 1.0);
        let mut output = [0.0_f32; 4];
        for channel in 0..3 {
            let value = 0.5 + derivative[channel] * gain;
            output[channel] = value * (1.0 - blend) + original[channel] * blend;
        }
        output[3] = original[3];
        output
    }

    /// Packs the values the GPU pass needs: light vector, gain and blend.
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

fn premultiplied(pixel: &[f32; 4], channel: usize) -> f32 {
    let alpha = pixel[3].clamp(0.0, 1.0);
    pixel[channel] * alpha
}

fn sample(
    pixels: &[[f32; 4]],
    width: usize,
    height: usize,
    x: isize,
    y: isize,
    channel: usize,
) -> f32 {
    let clamped_x = x.clamp(0, width as isize - 1) as usize;
    let clamped_y = y.clamp(0, height as isize - 1) as usize;
    premultiplied(&pixels[clamped_y * width + clamped_x], channel)
}

/// Applies the colour emboss kernel to one frame region in place.
pub(crate) fn apply_color_emboss_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ColorEmbossGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || pixels.len() < width * height {
        return Ok(false);
    }
    let (cosine, sine) = grade.light();
    // The stencil reads eight neighbours, so it must not observe values this
    // pass already wrote: read from a snapshot and write into the frame.
    let source = pixels.to_vec();
    let mut band_start = 0;
    while band_start < height {
        checkpoint()?;
        let band_end = (band_start + 4_096 / width.max(1)).min(height);
        for y in band_start..band_end {
            for x in 0..width {
                let index = y * width + x;
                let original = pixels[index];
                if original[3].clamp(0.0, 1.0) <= 0.0 {
                    continue;
                }
                let column = x as isize;
                let row = y as isize;
                let mut derivative = [0.0_f32; 3];
                for (channel, value) in derivative.iter_mut().enumerate() {
                    let top_left = sample(&source, width, height, column - 1, row - 1, channel);
                    let top = sample(&source, width, height, column, row - 1, channel);
                    let top_right = sample(&source, width, height, column + 1, row - 1, channel);
                    let middle_left = sample(&source, width, height, column - 1, row, channel);
                    let middle_right = sample(&source, width, height, column + 1, row, channel);
                    let bottom_left = sample(&source, width, height, column - 1, row + 1, channel);
                    let bottom = sample(&source, width, height, column, row + 1, channel);
                    let bottom_right = sample(&source, width, height, column + 1, row + 1, channel);
                    let horizontal = (top_right + 2.0 * middle_right + bottom_right)
                        - (top_left + 2.0 * middle_left + bottom_left);
                    let vertical = (bottom_left + 2.0 * bottom + bottom_right)
                        - (top_left + 2.0 * top + top_right);
                    // Screen rows grow downwards while the authored angle grows
                    // counter-clockwise, hence the negated vertical term.
                    *value = horizontal * cosine - vertical * sine;
                }
                pixels[index] = grade.apply(original, derivative);
            }
        }
        band_start = band_end;
    }
    Ok(true)
}

/// Applies the colour emboss kernel to a whole frame.
pub fn color_emboss_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ColorEmbossGrade,
) -> bool {
    let mut checkpoint = || -> Result<(), std::convert::Infallible> { Ok(()) };
    apply_color_emboss_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: usize, height: usize, value: [f32; 4]) -> Vec<[f32; 4]> {
        vec![value; width * height]
    }

    #[test]
    fn new_rejects_values_outside_the_authored_ranges() {
        assert_eq!(
            ColorEmbossGrade::new(f32::NAN, 1.0, 100.0, 0.0),
            Err(ColorEmbossError::Direction)
        );
        assert_eq!(
            ColorEmbossGrade::new(45.0, -1.0, 100.0, 0.0),
            Err(ColorEmbossError::Relief)
        );
        assert_eq!(
            ColorEmbossGrade::new(45.0, 1.0, 32_768.0, 0.0),
            Err(ColorEmbossError::Contrast)
        );
        assert_eq!(
            ColorEmbossGrade::new(45.0, 1.0, 100.0, 1.5),
            Err(ColorEmbossError::Blend)
        );
        assert!(ColorEmbossGrade::new(45.0, 1.0, 100.0, 0.0).is_ok());
    }

    #[test]
    fn full_mix_is_identity() {
        let grade = ColorEmbossGrade::new(45.0, 1.0, 100.0, 1.0).expect("grade");
        assert!(grade.is_identity());
        let mut pixels = flat(3, 3, [0.2, 0.4, 0.6, 1.0]);
        pixels[4] = [0.9, 0.1, 0.3, 1.0];
        let before = pixels.clone();
        assert!(color_emboss_frame_f32(&mut pixels, 3, 3, grade));
        assert_eq!(pixels, before);
    }

    #[test]
    fn flat_frame_embosses_to_mid_grey() {
        let grade = ColorEmbossGrade::new(45.0, 4.0, 200.0, 0.0).expect("grade");
        let mut pixels = flat(3, 3, [0.2, 0.4, 0.6, 1.0]);
        assert!(color_emboss_frame_f32(&mut pixels, 3, 3, grade));
        assert_eq!(pixels[4], [0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn chroma_steps_relief_per_channel() {
        // A red step is visible on red, and stays mid grey on green and blue.
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        pixels[2] = [0.5, 0.0, 0.0, 1.0];
        let grade = ColorEmbossGrade::new(0.0, 4.0, 100.0, 0.0).expect("grade");
        assert!(color_emboss_frame_f32(&mut pixels, 3, 1, grade));
        assert!(pixels[1][0] > 0.5, "expected a lit red relief");
        assert_eq!(pixels[1][1], 0.5);
        assert_eq!(pixels[1][2], 0.5);
    }

    #[test]
    fn contrast_scales_the_relief() {
        let mut low = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        low[2] = [0.5, 0.0, 0.0, 1.0];
        let mut high = low.clone();
        let low_grade = ColorEmbossGrade::new(0.0, 4.0, 100.0, 0.0).expect("grade");
        let high_grade = ColorEmbossGrade::new(0.0, 4.0, 400.0, 0.0).expect("grade");
        assert!(color_emboss_frame_f32(&mut low, 3, 1, low_grade));
        assert!(color_emboss_frame_f32(&mut high, 3, 1, high_grade));
        let low_peak = (low[1][0] - 0.5).abs();
        let high_peak = (high[1][0] - 0.5).abs();
        assert!((high_peak - low_peak * 4.0).abs() < 1.0e-5);
    }

    #[test]
    fn opposite_directions_flip_the_sign() {
        let mut forward = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        forward[2] = [0.5, 0.0, 0.0, 1.0];
        let mut backward = forward.clone();
        let forward_grade = ColorEmbossGrade::new(0.0, 4.0, 100.0, 0.0).expect("grade");
        let backward_grade = ColorEmbossGrade::new(180.0, 4.0, 100.0, 0.0).expect("grade");
        assert!(color_emboss_frame_f32(&mut forward, 3, 1, forward_grade));
        assert!(color_emboss_frame_f32(&mut backward, 3, 1, backward_grade));
        let total = (forward[1][0] - 0.5) + (backward[1][0] - 0.5);
        assert!(total.abs() < 1.0e-6);
    }

    #[test]
    fn transparent_pixels_stay_empty_and_do_not_pull_neighbours() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 0.0]; 3];
        pixels[1] = [0.4, 0.4, 0.4, 1.0];
        pixels[2] = [0.4, 0.4, 0.4, 1.0];
        let grade = ColorEmbossGrade::new(0.0, 4.0, 100.0, 0.0).expect("grade");
        assert!(color_emboss_frame_f32(&mut pixels, 3, 1, grade));
        assert_eq!(pixels[0], [0.0, 0.0, 0.0, 0.0]);
        assert!(pixels[1].iter().all(|value| value.is_finite()));
        assert_eq!(pixels[1][3], 1.0);
    }

    #[test]
    fn extended_range_is_not_clamped() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        pixels[2] = [4.0, 0.0, 0.0, 1.0];
        let grade = ColorEmbossGrade::new(0.0, 4.0, 100.0, 0.0).expect("grade");
        assert!(color_emboss_frame_f32(&mut pixels, 3, 1, grade));
        assert!(pixels[1][0] > 1.0);
    }

    #[test]
    fn gpu_controls_round_trip() {
        let grade = ColorEmbossGrade::new(90.0, 2.0, 150.0, 0.25).expect("grade");
        let controls = grade.gpu_controls();
        assert!(controls[0].abs() < 1.0e-6);
        assert!((controls[1] - 1.0).abs() < 1.0e-6);
        assert!((controls[2] - 2.0 * 0.25 * 1.5).abs() < 1.0e-6);
        assert_eq!(controls[3], 0.25);
    }
}
