//! Convolution kernel: a general 3×3 linear filter.
//!
//! The catalogue exposes the nine cells as independent numbers, so this is a
//! plain correlation (not a rotation-flipped convolution): the output at a pixel
//! is `sum over the 3×3 neighbourhood of kernel_cell * sample`. Every
//! interesting 3×3 effect in the list is a special case of that matrix —
//! identity, box blur, sharpen, emboss, edge detect — so one well-tested kernel
//! covers them.
//!
//! Contract:
//!
//! - The filter runs on **premultiplied** colour, which is the only form where a
//!   linear combination of samples is meaningful across coverage changes: a
//!   fully transparent neighbour contributes nothing instead of dragging a dark
//!   colour into a visible edge. Alpha is filtered by the same matrix.
//! - Values are **never clamped**. A kernel with negative lobes can produce
//!   values outside `0..=1`, and an HDR source keeps its headroom; `Absolute
//!   Values` takes magnitudes in the authored space, again without clamping.
//! - `Blend w. Original` mixes the filtered result with the untouched source in
//!   premultiplied space, so a partial blend of a transparent pixel still
//!   resolves to the source.
//! - The neighbourhood reaches exactly one pixel, so the ROI halo is 1.

/// Lowest kernel cell the authoring schema can produce.
pub const CONVOLUTION_MIN_CELL: f32 = -100.0;
/// Highest kernel cell the authoring schema can produce.
pub const CONVOLUTION_MAX_CELL: f32 = 100.0;
/// Halo the filter needs from the effect's input.
pub const CONVOLUTION_HALO_PIXELS: u32 = 1;

/// Reasons a convolution cannot be built from its authored values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvolutionError {
    /// One of the nine cells was not finite or fell outside the declared range.
    Cell,
    /// The blend with the original was not finite or fell outside `0..=1`.
    Blend,
}

impl std::fmt::Display for ConvolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cell => formatter.write_str("convolution cells must be finite and in -100..=100"),
            Self::Blend => formatter.write_str("convolution blend must be finite and in 0..=1"),
        }
    }
}

impl std::error::Error for ConvolutionError {}

/// Validated convolution parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConvolutionGrade {
    kernel: [f32; 9],
    absolute_values: bool,
    blend_with_original: f32,
}

impl ConvolutionGrade {
    /// Builds a grade, rejecting values the authoring schema cannot produce.
    pub fn new(
        kernel: [f32; 9],
        absolute_values: bool,
        blend_with_original: f32,
    ) -> Result<Self, ConvolutionError> {
        if kernel.iter().any(|cell| {
            !cell.is_finite() || !(CONVOLUTION_MIN_CELL..=CONVOLUTION_MAX_CELL).contains(cell)
        }) {
            return Err(ConvolutionError::Cell);
        }
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(ConvolutionError::Blend);
        }
        Ok(Self { kernel, absolute_values, blend_with_original })
    }

    /// The nine cells in row-major order, cell 4 being the centre.
    pub fn kernel(self) -> [f32; 9] {
        self.kernel
    }

    /// Whether magnitudes are taken after filtering.
    pub fn absolute_values(self) -> bool {
        self.absolute_values
    }

    /// Blend with the untouched source, `0` being fully filtered.
    pub fn blend_with_original(self) -> f32 {
        self.blend_with_original
    }

    /// The declared neutral kernel: `1` in the centre and `0` everywhere else.
    pub fn is_identity_kernel(self) -> bool {
        const IDENTITY: [f32; 9] = [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        self.kernel == IDENTITY
    }

    /// A grade that leaves the frame untouched.
    ///
    /// A full blend with the original is the identity whatever the matrix says,
    /// because the output is the untouched source.
    pub fn is_identity(self) -> bool {
        self.blend_with_original >= 1.0 || (self.is_identity_kernel() && !self.absolute_values)
    }

    /// Filters one premultiplied pixel from its 3×3 neighbourhood.
    ///
    /// `samples` is row-major, matching [`Self::kernel`]. The result is a linear
    /// combination, so it is defined for extended-range values and for alpha.
    pub fn apply_premultiplied(self, samples: &[[f32; 4]; 9]) -> [f32; 4] {
        let mut filtered = [0.0_f32; 4];
        for (cell, sample) in self.kernel.iter().zip(samples.iter()) {
            for (accumulated, channel) in filtered.iter_mut().zip(sample.iter()) {
                *accumulated += cell * channel;
            }
        }
        if self.absolute_values {
            for value in filtered.iter_mut() {
                *value = value.abs();
            }
        }
        filtered
    }

    /// GPU controls: `params` and `extra` carry the first eight cells, `aces`
    /// carries the ninth cell, the absolute flag and the blend.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.kernel[0],
                self.kernel[1],
                self.kernel[2],
                self.kernel[3],
            ],
            [
                self.kernel[4],
                self.kernel[5],
                self.kernel[6],
                self.kernel[7],
            ],
            [
                self.kernel[8],
                f32::from(u8::from(self.absolute_values)),
                self.blend_with_original,
                0.0,
            ],
        )
    }
}

/// Filters one frame in place, returning `false` when the frame is unusable.
pub fn convolution_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ConvolutionGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_convolution_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Filters one frame in place with an explicit cancellation checkpoint.
pub(crate) fn apply_convolution_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ConvolutionGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }

    let frame = &mut working[..pixel_count];
    crate::straight_alpha::premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    let rows = frame.len() / width;
    checkpoint()?;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let mut samples = [[0.0_f32; 4]; 9];
                for (index, sample) in samples.iter_mut().enumerate() {
                    let dx = index % 3;
                    let dy = index / 3;
                    let x = clamp_axis(column as isize + dx as isize - 1, width);
                    let y = clamp_axis(row as isize + dy as isize - 1, height);
                    *sample = source[y * width + x];
                }
                let filtered = grade.apply_premultiplied(&samples);
                let original = source[row * width + column];
                let blend = grade.blend_with_original();
                *pixel = if blend > 0.0 {
                    let mut mixed = [0.0_f32; 4];
                    for channel in 0..4 {
                        mixed[channel] =
                            filtered[channel] * (1.0 - blend) + original[channel] * blend;
                    }
                    mixed
                } else {
                    filtered
                };
            }
        }
    });

    // The compositor seam is straight alpha; coverage was filtered by the same
    // matrix, so unpremultiplying restores `colour * coverage` semantics.
    crate::straight_alpha::unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

/// Clamps a neighbour coordinate to the frame, repeating edge pixels.
fn clamp_axis(value: isize, extent: usize) -> usize {
    let last = extent.saturating_sub(1) as isize;
    value.clamp(0, last) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, value: [f32; 4]) -> Vec<[f32; 4]> {
        vec![value; width * height]
    }

    fn grade(kernel: [f32; 9]) -> ConvolutionGrade {
        ConvolutionGrade::new(kernel, false, 0.0).expect("valid convolution")
    }

    #[test]
    fn new_rejects_values_the_schema_cannot_author() {
        let mut kernel = [0.0; 9];
        kernel[4] = 1.0;
        assert_eq!(
            ConvolutionGrade::new([100.5; 9], false, 0.0).unwrap_err(),
            ConvolutionError::Cell
        );
        assert_eq!(
            ConvolutionGrade::new([-100.5; 9], false, 0.0).unwrap_err(),
            ConvolutionError::Cell
        );
        assert_eq!(
            ConvolutionGrade::new([f32::NAN; 9], false, 0.0).unwrap_err(),
            ConvolutionError::Cell
        );
        assert_eq!(
            ConvolutionGrade::new(kernel, false, 1.5).unwrap_err(),
            ConvolutionError::Blend
        );
        assert_eq!(
            ConvolutionGrade::new(kernel, false, -0.5).unwrap_err(),
            ConvolutionError::Blend
        );
    }

    #[test]
    fn the_declared_neutral_kernel_is_the_identity() {
        let mut kernel = [0.0; 9];
        kernel[4] = 1.0;
        let grade = grade(kernel);
        assert!(grade.is_identity());
        let pixels = vec![
            [0.25_f32, 0.5, 0.75, 1.0],
            [0.0, 1.0, 0.0, 0.5],
            [2.0, -0.5, 1.5, 1.0],
        ];
        let mut working = pixels.clone();
        assert!(convolution_frame_f32(&mut working, 3, 1, grade));
        assert_eq!(working, pixels);
    }

    #[test]
    fn a_box_kernel_averages_the_neighbourhood() {
        let kernel = [1.0 / 9.0; 9];
        let mut working = frame(3, 3, [0.0, 0.0, 0.0, 1.0]);
        working[4] = [0.9, 0.9, 0.9, 1.0];
        assert!(convolution_frame_f32(&mut working, 3, 3, grade(kernel)));
        // The centre averages the single lit pixel across nine cells.
        assert!((working[4][0] - 0.1).abs() <= 1.0e-6, "{:?}", working[4]);
        // Edge pixels repeat their edge, so a corner still sees the centre once.
        assert!((working[0][0] - 0.1).abs() <= 1.0e-6, "{:?}", working[0]);
    }

    #[test]
    fn a_sharpen_kernel_amplifies_local_contrast() {
        let kernel = [0.0, -1.0, 0.0, -1.0, 5.0, -1.0, 0.0, -1.0, 0.0];
        let mut working = frame(3, 1, [0.2, 0.2, 0.2, 1.0]);
        working[1] = [0.6, 0.6, 0.6, 1.0];
        assert!(convolution_frame_f32(&mut working, 3, 1, grade(kernel)));
        // 5 * 0.6 - 0.2 (left) - 0.2 (right) - 0.6 (up, clamped) - 0.6 (down).
        assert!((working[1][0] - 1.4).abs() <= 1.0e-5, "{:?}", working[1]);
    }

    #[test]
    fn absolute_values_takes_magnitudes_after_filtering() {
        let kernel = [0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0];
        let mut working = frame(1, 1, [0.25, -0.5, 1.5, 1.0]);
        let grade = ConvolutionGrade::new(kernel, true, 0.0).expect("valid");
        assert!(convolution_frame_f32(&mut working, 1, 1, grade));
        assert!((working[0][0] - 0.25).abs() <= 1.0e-6);
        assert!((working[0][1] - 0.5).abs() <= 1.0e-6);
        assert!((working[0][2] - 1.5).abs() <= 1.0e-6);
    }

    #[test]
    fn the_blend_with_the_original_keeps_the_source_share() {
        // The kernel takes the left neighbour, so the middle pixel filters to the
        // leading black sample; a quarter blend keeps three quarters of the source.
        let kernel = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut working = vec![
            [0.0_f32, 0.0, 0.0, 1.0],
            [0.4, 0.6, 0.8, 1.0],
            [1.0, 1.0, 1.0, 1.0],
        ];
        let grade = ConvolutionGrade::new(kernel, false, 0.75).expect("valid");
        assert!(convolution_frame_f32(&mut working, 3, 1, grade));
        assert!((working[1][0] - 0.3).abs() <= 1.0e-6, "{:?}", working[1]);
        assert!((working[1][1] - 0.45).abs() <= 1.0e-6);
        assert!((working[1][2] - 0.6).abs() <= 1.0e-6);
        assert!((working[1][3] - 1.0).abs() <= 1.0e-6);
    }

    #[test]
    fn transparent_neighbours_do_not_pull_the_visible_colour_down() {
        // Dark fully transparent neighbours flank a bright opaque pixel. A linear
        // filter on premultiplied colour counts them as zero coverage, so the
        // bright pixel keeps its own colour after unpremultiplying.
        let kernel = [0.0, 0.0, 0.0, 0.25, 0.5, 0.25, 0.0, 0.0, 0.0];
        let mut working = vec![
            [0.0_f32, 0.0, 0.0, 0.0],
            [0.8, 0.8, 0.8, 1.0],
            [0.0, 0.0, 0.0, 0.0],
        ];
        assert!(convolution_frame_f32(&mut working, 3, 1, grade(kernel)));
        // Premultiplied 0.4 over coverage 0.5.
        assert!((working[1][0] - 0.8).abs() <= 1.0e-6, "{:?}", working[1]);
        assert!((working[1][3] - 0.5).abs() <= 1.0e-6, "{:?}", working[1]);
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let kernel = [0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0];
        let mut working = vec![[4.0_f32, -0.5, 1.5, 1.0]];
        assert!(convolution_frame_f32(&mut working, 1, 1, grade(kernel)));
        // Extended range colour doubles with no clamp. Coverage itself saturates
        // at one, which is the frame contract, so the doubled colour is kept as
        // the straight value rather than being divided by an out-of-range alpha.
        assert!((working[0][0] - 8.0).abs() <= 1.0e-5, "{:?}", working[0]);
        assert!((working[0][1] + 1.0).abs() <= 1.0e-6);
        assert!((working[0][2] - 3.0).abs() <= 1.0e-6);
        assert!((working[0][3] - 1.0).abs() <= 1.0e-6);
    }

    #[test]
    fn mismatched_dimensions_are_rejected() {
        let mut working = vec![[0.0_f32; 4]; 2];
        assert!(!convolution_frame_f32(&mut working, 3, 1, grade([1.0; 9])));
        assert!(!convolution_frame_f32(&mut working, 0, 0, grade([1.0; 9])));
    }

    #[test]
    fn a_full_blend_with_the_original_is_the_identity() {
        let grade = ConvolutionGrade::new([3.0; 9], true, 1.0).expect("valid");
        assert!(grade.is_identity());
        let mut working = vec![
            [0.25_f32, 0.5, -0.75, 0.5],
            [2.0, -1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let pixels = working.clone();
        assert!(convolution_frame_f32(&mut working, 3, 1, grade));
        assert_eq!(working, pixels);
    }

    #[test]
    fn gpu_controls_carry_the_matrix_the_flag_and_the_blend() {
        let kernel = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
        let grade = ConvolutionGrade::new(kernel, true, 0.75).expect("valid");
        let (params, extra, aces) = grade.gpu_controls();
        assert_eq!(params, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(extra, [5.0, 6.0, 7.0, 8.0]);
        assert_eq!(aces, [9.0, 1.0, 0.75, 0.0]);
    }
}
