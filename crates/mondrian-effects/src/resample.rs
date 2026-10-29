//! Shared bilinear resampling helpers.
//!
//! Every geometric effect in this crate maps output pixels onto source
//! positions and then samples the source. They all share the arithmetic below
//! so the boundary convention (clamp or wrap), the handling of the fractional
//! weights and the channel interpolation order are defined exactly once. The
//! fused WGSL passes mirror this arithmetic step by step, which is what makes a
//! per-pixel parity test against the CPU kernels meaningful.

/// Bilinear sample whose integer coordinates clamp to the frame edges.
///
/// The fractional weights are taken from the clamped coordinate, so a position
/// far outside the frame reads the border texel instead of extrapolating.
pub(crate) fn sample_bilinear_clamped(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
) -> [f32; 4] {
    let limit_x = (width - 1) as f64;
    let limit_y = (height - 1) as f64;
    let clamped_x = x.clamp(0.0, limit_x);
    let clamped_y = y.clamp(0.0, limit_y);
    let base_x = clamped_x.floor();
    let base_y = clamped_y.floor();
    let frac_x = (clamped_x - base_x) as f32;
    let frac_y = (clamped_y - base_y) as f32;
    let x0 = base_x as usize;
    let y0 = base_y as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);

    let top_left = source[y0 * width + x0];
    let top_right = source[y0 * width + x1];
    let bottom_left = source[y1 * width + x0];
    let bottom_right = source[y1 * width + x1];

    let mut sample = [0.0_f32; 4];
    for (channel, value) in sample.iter_mut().enumerate() {
        let top = top_left[channel] + (top_right[channel] - top_left[channel]) * frac_x;
        let bottom = bottom_left[channel] + (bottom_right[channel] - bottom_left[channel]) * frac_x;
        *value = top + (bottom - top) * frac_y;
    }
    sample
}

/// Bilinear sample whose integer coordinates wrap around the frame edges.
///
/// The fractional weights are taken from the unclamped coordinate, so a shift
/// that moves the picture across a seam interpolates between the two opposite
/// borders instead of doubling a border texel.
pub(crate) fn sample_bilinear_wrapped(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
) -> [f32; 4] {
    let base_x = x.floor();
    let base_y = y.floor();
    let frac_x = (x - base_x) as f32;
    let frac_y = (y - base_y) as f32;
    let x0 = wrap_index(base_x as i64, width);
    let y0 = wrap_index(base_y as i64, height);
    let x1 = wrap_index(base_x as i64 + 1, width);
    let y1 = wrap_index(base_y as i64 + 1, height);

    let top_left = source[y0 * width + x0];
    let top_right = source[y0 * width + x1];
    let bottom_left = source[y1 * width + x0];
    let bottom_right = source[y1 * width + x1];

    let mut sample = [0.0_f32; 4];
    for (channel, value) in sample.iter_mut().enumerate() {
        let top = top_left[channel] + (top_right[channel] - top_left[channel]) * frac_x;
        let bottom = bottom_left[channel] + (bottom_right[channel] - bottom_left[channel]) * frac_x;
        *value = top + (bottom - top) * frac_y;
    }
    sample
}

/// Nearest sample whose integer coordinates clamp to the frame edges.
///
/// The rounding is `floor(value + 0.5)` rather than a language-provided round:
/// WGSL rounds half-way cases to even while Rust rounds them away from zero, so
/// spelling the direction out keeps the CPU kernel and the WGSL pass on the same
/// texel for every input.
pub(crate) fn sample_nearest_clamped(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
) -> [f32; 4] {
    let column = (x + 0.5).floor().clamp(0.0, (width - 1) as f64) as usize;
    let row = (y + 0.5).floor().clamp(0.0, (height - 1) as f64) as usize;
    source[row * width + column]
}

/// Wraps a signed index into `0..limit`.
pub(crate) fn wrap_index(index: i64, limit: usize) -> usize {
    let limit = limit as i64;
    index.rem_euclid(limit) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| {
                let value = index as f32;
                [value, -value * 0.5, value * 0.25, 1.0]
            })
            .collect()
    }

    #[test]
    fn clamped_sampling_reads_the_border_outside_the_frame() {
        let pixels = ramp(4, 3);
        assert_eq!(
            sample_bilinear_clamped(&pixels, 4, 3, -10.0, -10.0),
            pixels[0]
        );
        assert_eq!(
            sample_bilinear_clamped(&pixels, 4, 3, 99.0, 99.0),
            pixels[11]
        );
    }

    #[test]
    fn clamped_sampling_interpolates_four_neighbours() {
        let pixels = ramp(4, 3);
        let sample = sample_bilinear_clamped(&pixels, 4, 3, 1.5, 0.5);
        for channel in 0..4 {
            let expected =
                (pixels[1][channel] + pixels[2][channel] + pixels[5][channel] + pixels[6][channel])
                    / 4.0;
            assert!((sample[channel] - expected).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn wrapped_sampling_crosses_the_seam() {
        let pixels = ramp(4, 3);
        let sample = sample_bilinear_wrapped(&pixels, 4, 3, 3.5, 2.5);
        // (3.5, 2.5) sits between the last and the first texel of both axes.
        for channel in 0..4 {
            let expected = (pixels[2 * 4 + 3][channel]
                + pixels[2 * 4][channel]
                + pixels[3][channel]
                + pixels[0][channel])
                / 4.0;
            assert!((sample[channel] - expected).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn nearest_sampling_rounds_half_up_and_clamps() {
        let pixels = ramp(4, 3);
        assert_eq!(sample_nearest_clamped(&pixels, 4, 3, 1.5, 0.0), pixels[2]);
        assert_eq!(sample_nearest_clamped(&pixels, 4, 3, 1.4, 0.0), pixels[1]);
        assert_eq!(sample_nearest_clamped(&pixels, 4, 3, -2.0, -2.0), pixels[0]);
        assert_eq!(sample_nearest_clamped(&pixels, 4, 3, 9.0, 9.0), pixels[11]);
    }

    #[test]
    fn wrap_index_accepts_negative_and_oversized_indices() {
        assert_eq!(wrap_index(-1, 5), 4);
        assert_eq!(wrap_index(-6, 5), 4);
        assert_eq!(wrap_index(7, 5), 2);
        assert_eq!(wrap_index(0, 1), 0);
    }
}
