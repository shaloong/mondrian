//! Mosaic (STY-11): quantise the frame into blocks of a fixed pixel size.
//!
//! Each block becomes one colour: by default the coverage-weighted average of
//! its premultiplied samples, or — with `sharp_colors` — the colour of the
//! block's top-left sample, which keeps hard edges inside the block grid.
//! Blocks are clipped at the right and bottom frame edges, so partial blocks
//! average only over the samples that exist. Both sizes are authored in whole
//! pixels and measured on the frame, not on the view.

/// Smallest block edge in pixels.
pub const MOSAIC_MIN_BLOCK_PIXELS: u32 = 1;
/// Largest block edge in pixels.
pub const MOSAIC_MAX_BLOCK_PIXELS: u32 = 4_000;

/// Rejected [`MosaicGrade`] authoring values.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MosaicError {
    /// The horizontal block size is outside `1..=4000`.
    #[error("mosaic horizontal block must be within [1, 4000]")]
    Horizontal,
    /// The vertical block size is outside `1..=4000`.
    #[error("mosaic vertical block must be within [1, 4000]")]
    Vertical,
}

/// Validated mosaic parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MosaicGrade {
    horizontal: u32,
    vertical: u32,
    sharp_colors: bool,
}

impl MosaicGrade {
    /// Validates the block sizes.
    pub fn new(horizontal: u32, vertical: u32, sharp_colors: bool) -> Result<Self, MosaicError> {
        if !(MOSAIC_MIN_BLOCK_PIXELS..=MOSAIC_MAX_BLOCK_PIXELS).contains(&horizontal) {
            return Err(MosaicError::Horizontal);
        }
        if !(MOSAIC_MIN_BLOCK_PIXELS..=MOSAIC_MAX_BLOCK_PIXELS).contains(&vertical) {
            return Err(MosaicError::Vertical);
        }
        Ok(Self { horizontal, vertical, sharp_colors })
    }

    /// Horizontal block edge in pixels.
    pub const fn horizontal(self) -> u32 {
        self.horizontal
    }

    /// Vertical block edge in pixels.
    pub const fn vertical(self) -> u32 {
        self.vertical
    }

    /// Whether each block keeps its top-left colour instead of the average.
    pub const fn sharp_colors(self) -> bool {
        self.sharp_colors
    }

    /// A one-pixel grid reproduces the input exactly.
    pub const fn is_identity(self) -> bool {
        self.horizontal == 1 && self.vertical == 1
    }

    /// Packed GPU controls: `[horizontal, vertical, sharp, 0]`.
    pub fn gpu_controls(self) -> [f32; 4] {
        [
            self.horizontal as f32,
            self.vertical as f32,
            f32::from(u8::from(self.sharp_colors)),
            0.0,
        ]
    }

    /// The block colour at the top-left corner of one block.
    fn block_colour(
        &self,
        source: &[[f32; 4]],
        width: usize,
        height: usize,
        x0: usize,
        y0: usize,
    ) -> [f32; 4] {
        let x1 = (x0 + self.horizontal as usize).min(width);
        let y1 = (y0 + self.vertical as usize).min(height);
        if self.sharp_colors {
            return source[y0 * width + x0];
        }
        let mut sum = [0.0_f32; 4];
        for y in y0..y1 {
            for x in x0..x1 {
                let pixel = source[y * width + x];
                let alpha = pixel[3].clamp(0.0, 1.0);
                sum[0] += pixel[0] * alpha;
                sum[1] += pixel[1] * alpha;
                sum[2] += pixel[2] * alpha;
                sum[3] += alpha;
            }
        }
        let samples = ((x1 - x0) * (y1 - y0)) as f32;
        let coverage = sum[3] / samples;
        if sum[3] <= 0.0 {
            return [0.0; 4];
        }
        // sum[0..3] are premultiplied sums, so dividing by the summed coverage
        // yields the straight colour, while `coverage` stays the mean alpha.
        [sum[0] / sum[3], sum[1] / sum[3], sum[2] / sum[3], coverage]
    }
}

/// Applies the mosaic grid, reporting cooperatively through `checkpoint`.
pub fn apply_mosaic_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: MosaicGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || pixels.len() != width * height {
        return Ok(false);
    }
    let source = pixels.to_vec();
    let step_x = grade.horizontal as usize;
    let step_y = grade.vertical as usize;
    let mut written = 0_usize;
    for y0 in (0..height).step_by(step_y) {
        for x0 in (0..width).step_by(step_x) {
            let color = grade.block_colour(&source, width, height, x0, y0);
            let x1 = (x0 + step_x).min(width);
            let y1 = (y0 + step_y).min(height);
            for y in y0..y1 {
                for x in x0..x1 {
                    pixels[y * width + x] = color;
                    written += 1;
                }
            }
            if written % 65_536 < step_x * step_y {
                checkpoint()?;
            }
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Applies the mosaic grid to a whole frame.
pub fn mosaic_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: MosaicGrade,
) -> bool {
    apply_mosaic_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(horizontal: u32, vertical: u32) -> MosaicGrade {
        MosaicGrade::new(horizontal, vertical, false).expect("valid mosaic")
    }

    #[test]
    fn rejects_block_sizes_outside_the_listed_window() {
        assert_eq!(MosaicGrade::new(0, 10, false), Err(MosaicError::Horizontal));
        assert_eq!(
            MosaicGrade::new(4_001, 10, false),
            Err(MosaicError::Horizontal)
        );
        assert_eq!(MosaicGrade::new(10, 0, false), Err(MosaicError::Vertical));
        assert_eq!(
            MosaicGrade::new(10, 4_001, false),
            Err(MosaicError::Vertical)
        );
        assert!(MosaicGrade::new(1, 1, false).is_ok());
        assert!(MosaicGrade::new(4_000, 4_000, true).is_ok());
    }

    #[test]
    fn a_one_pixel_grid_is_identity() {
        assert!(grade(1, 1).is_identity());
        assert!(!grade(2, 1).is_identity());
        assert!(!grade(1, 2).is_identity());
        let mut pixels = vec![[0.2, 0.4, 0.6, 1.0], [0.8, 0.1, 0.3, 0.5]];
        let expected = pixels.clone();
        assert!(mosaic_frame_f32(&mut pixels, 2, 1, grade(1, 1)));
        assert_eq!(pixels, expected);
    }

    #[test]
    fn blocks_carry_the_coverage_weighted_average() {
        let mut pixels = vec![
            [0.2, 0.2, 0.2, 1.0],
            [0.6, 0.6, 0.6, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert!(mosaic_frame_f32(&mut pixels, 2, 2, grade(2, 2)));
        for pixel in &pixels {
            assert!((pixel[0] - 0.45).abs() < 1.0e-6, "{pixel:?}");
            assert!((pixel[3] - 1.0).abs() < 1.0e-6, "{pixel:?}");
        }
    }

    #[test]
    fn sharp_colors_repeat_the_top_left_sample() {
        let mut pixels = vec![
            [0.2, 0.2, 0.2, 1.0],
            [0.6, 0.6, 0.6, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let sharp = MosaicGrade::new(2, 2, true).expect("valid mosaic");
        assert!(mosaic_frame_f32(&mut pixels, 2, 2, sharp));
        for pixel in &pixels {
            assert_eq!(*pixel, [0.2, 0.2, 0.2, 1.0]);
        }
    }

    #[test]
    fn partial_blocks_average_only_over_existing_samples() {
        let mut pixels = vec![
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
        ];
        // 2x2 grid on a 3x1 frame: the first block is full, the second holds one sample.
        assert!(mosaic_frame_f32(&mut pixels, 3, 1, grade(2, 2)));
        assert!((pixels[0][0] - 0.5).abs() < 1.0e-6);
        assert!((pixels[1][0] - 0.5).abs() < 1.0e-6);
        assert!((pixels[2][0] - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn transparent_samples_do_not_darken_the_block() {
        let mut pixels = vec![
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 1.0, 0.0, 1.0],
        ];
        assert!(mosaic_frame_f32(&mut pixels, 2, 2, grade(2, 2)));
        let pixel = pixels[0];
        // Three visible samples contribute: green, blue and yellow (r + g).
        assert!((pixel[0] - 1.0 / 3.0).abs() < 1.0e-6, "{pixel:?}");
        assert!((pixel[1] - 2.0 / 3.0).abs() < 1.0e-6, "{pixel:?}");
        assert!((pixel[2] - 1.0 / 3.0).abs() < 1.0e-6, "{pixel:?}");
        assert!((pixel[3] - 0.75).abs() < 1.0e-6, "{pixel:?}");
    }

    #[test]
    fn a_fully_transparent_block_stays_empty() {
        let mut pixels = vec![[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]];
        assert!(mosaic_frame_f32(&mut pixels, 2, 1, grade(2, 1)));
        assert_eq!(pixels[0], [0.0; 4]);
        assert_eq!(pixels[1], [0.0; 4]);
    }

    #[test]
    fn extended_range_samples_are_averaged_without_clamping() {
        let mut pixels = vec![[2.0, 0.0, 0.0, 1.0], [-0.5, 1.0, 0.0, 1.0]];
        assert!(mosaic_frame_f32(&mut pixels, 2, 1, grade(2, 1)));
        assert!((pixels[0][0] - 0.75).abs() < 1.0e-6, "{:?}", pixels[0]);
    }

    #[test]
    fn rejects_frames_whose_extent_disagrees_with_the_buffer() {
        let mut pixels = vec![[0.0; 4]; 3];
        assert!(!mosaic_frame_f32(&mut pixels, 2, 2, grade(2, 2)));
    }

    #[test]
    fn gpu_controls_round_trip() {
        let grade = MosaicGrade::new(7, 3, true).expect("valid mosaic");
        let controls = grade.gpu_controls();
        assert_eq!(controls[0], 7.0);
        assert_eq!(controls[1], 3.0);
        assert_eq!(controls[2], 1.0);
        assert_eq!(controls[3], 0.0);
    }
}
