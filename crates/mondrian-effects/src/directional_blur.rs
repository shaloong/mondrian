//! Directional blur (effect list BLR-03).
//!
//! Averages the frame along one direction, which smears moving subjects the way a camera pans
//! during a long exposure. Authoring units follow the effect list: the direction is an angle in
//! degrees and the blur length is a distance in pixels.
//!
//! Conventions:
//! - `0°` smears to the right (`+x`), `90°` smears downwards (`+y`), so positive angles rotate
//!   clockwise in screen coordinates.
//! - The blur samples `ceil(length)` positions evenly spread across `[-length / 2, +length / 2]`
//!   with equal weights. Positions are continuous, so sub-pixel lengths produce a genuine
//!   fractional integration instead of rounding to whole pixels.
//! - Long blurs are capped at [`MAX_DIRECTIONAL_BLUR_TAPS`] samples. The samples still cover the
//!   whole span, so a 1000 px blur costs at most 128 taps while yielding the same box average.
//! - Colour is filtered with its coverage (premultiplied intermediate), and edge samples repeat
//!   the border pixel instead of treating the outside as transparent.

/// Largest blur length the authoring schema accepts, in pixels.
pub const DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS: f32 = 1_000.0;

/// Upper bound on the number of taps used for one blurred pixel.
pub const MAX_DIRECTIONAL_BLUR_TAPS: u32 = 128;

/// Largest input halo the effect declares before falling back to a full-frame request.
const MAX_DIRECTIONAL_BLUR_INPUT_HALO_PIXELS: u32 = 4_096;

const LENGTH_EPSILON: f32 = 1.0e-4;

/// Returns the authored blur length when it is finite and inside the declared range.
pub(crate) fn valid_directional_blur_length(length: f32) -> Option<f32> {
    (length.is_finite() && (0.0..=DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS).contains(&length))
        .then_some(length)
}

/// Returns the unit vector the blur walks along for the authored direction.
pub fn directional_blur_step(direction_degrees: f32) -> [f32; 2] {
    let radians = direction_degrees.to_radians();
    [radians.cos(), radians.sin()]
}

/// Returns the number of taps used for one blurred pixel.
pub fn directional_blur_taps(length: f32) -> u32 {
    if !length.is_finite() || length <= LENGTH_EPSILON {
        return 1;
    }
    (length.ceil() as u32).clamp(1, MAX_DIRECTIONAL_BLUR_TAPS)
}

/// Returns the halo this blur reads on each axis, or `None` when it must request the full frame.
pub(crate) fn directional_blur_axis_halo(length: f32) -> Option<(u32, u32)> {
    let length = valid_directional_blur_length(length)?;
    if length <= LENGTH_EPSILON {
        return Some((0, 0));
    }
    let halo = (length * 0.5).ceil() as u32 + 2;
    (halo <= MAX_DIRECTIONAL_BLUR_INPUT_HALO_PIXELS).then_some((halo, halo))
}

/// Blurs one frame in place along the authored direction.
///
/// Returns `Ok(false)` when the parameters cannot be executed so the caller can fall back to an
/// explicit unsupported-op error.
pub(crate) fn apply_directional_blur_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    length: f32,
    direction_degrees: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(length) = valid_directional_blur_length(length) else {
        return Ok(false);
    };
    if !direction_degrees.is_finite() {
        return Ok(false);
    }
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    if length <= LENGTH_EPSILON {
        return Ok(true);
    }

    let frame = &mut working[..pixel_count];
    crate::straight_alpha::premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    let taps = directional_blur_taps(length);
    let step = length / taps as f32;
    let [step_x, step_y] = directional_blur_step(direction_degrees);
    let half = length * 0.5;
    let weight = 1.0 / taps as f32;

    // The tap grid is identical for every pixel, so hoist it out of the pixel
    // loop instead of recomputing the trig-derived offsets per sample.
    let deltas = (0..taps)
        .map(|tap| {
            let offset = -half + step * (tap as f32 + 0.5);
            [step_x * offset, step_y * offset]
        })
        .collect::<Vec<_>>();
    let (max_dx, max_dy) = deltas.iter().fold((0.0_f32, 0.0_f32), |acc, delta| {
        (acc.0.max(delta[0].abs()), acc.1.max(delta[1].abs()))
    });
    // Pixels at least this far from the border never need coordinate clamping;
    // they take the branch-free path below. Everything else keeps the clamped
    // bilinear path, so border handling is bit-identical to before.
    let margin_x = max_dx.ceil() as usize + 1;
    let margin_y = max_dy.ceil() as usize + 1;

    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            let base_y = row as f32;
            let interior_row = row >= margin_y && row + margin_y < height;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let base_x = column as f32;
                let mut sum = [0.0_f32; 4];
                if interior_row && column >= margin_x && column + margin_x < width {
                    for delta in &deltas {
                        let x = base_x + delta[0];
                        let y = base_y + delta[1];
                        let x0 = x as usize;
                        let y0 = y as usize;
                        let frac_x = x - x0 as f32;
                        let frac_y = y - y0 as f32;
                        let x1 = (x0 + 1).min(width - 1);
                        let y1 = (y0 + 1).min(height - 1);
                        let top_left = source[y0 * width + x0];
                        let top_right = source[y0 * width + x1];
                        let bottom_left = source[y1 * width + x0];
                        let bottom_right = source[y1 * width + x1];
                        let w00 = (1.0 - frac_x) * (1.0 - frac_y);
                        let w10 = frac_x * (1.0 - frac_y);
                        let w01 = (1.0 - frac_x) * frac_y;
                        let w11 = frac_x * frac_y;
                        for channel in 0..4 {
                            sum[channel] += top_left[channel] * w00
                                + top_right[channel] * w10
                                + bottom_left[channel] * w01
                                + bottom_right[channel] * w11;
                        }
                    }
                } else {
                    for delta in &deltas {
                        let sample = sample_bilinear(
                            &source,
                            width,
                            height,
                            base_x + delta[0],
                            base_y + delta[1],
                        );
                        sum[0] += sample[0];
                        sum[1] += sample[1];
                        sum[2] += sample[2];
                        sum[3] += sample[3];
                    }
                }
                *pixel = [
                    sum[0] * weight,
                    sum[1] * weight,
                    sum[2] * weight,
                    sum[3] * weight,
                ];
            }
        }
    });

    crate::straight_alpha::unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

/// Bilinear sample that clamps coordinates to the frame so borders repeat instead of fading.
fn sample_bilinear(source: &[[f32; 4]], width: usize, height: usize, x: f32, y: f32) -> [f32; 4] {
    let max_x = (width - 1) as f32;
    let max_y = (height - 1) as f32;
    let x = x.clamp(0.0, max_x);
    let y = y.clamp(0.0, max_y);
    let x0 = x.floor();
    let y0 = y.floor();
    let frac_x = x - x0;
    let frac_y = y - y0;
    let x0 = x0 as usize;
    let y0 = y0 as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let top_left = source[y0 * width + x0];
    let top_right = source[y0 * width + x1];
    let bottom_left = source[y1 * width + x0];
    let bottom_right = source[y1 * width + x1];
    let mut sample = [0.0_f32; 4];
    for channel in 0..4 {
        let top = top_left[channel] + (top_right[channel] - top_left[channel]) * frac_x;
        let bottom = bottom_left[channel] + (bottom_right[channel] - bottom_left[channel]) * frac_x;
        sample[channel] = top + (bottom - top) * frac_y;
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKPOINT_LIMIT: usize = 1_000_000;

    fn blur(
        pixels: &mut [[f32; 4]],
        width: usize,
        height: usize,
        length: f32,
        direction: f32,
    ) -> bool {
        let mut counter = 0;
        apply_directional_blur_f32_controlled(pixels, width, height, length, direction, &mut || {
            counter += 1;
            (counter <= CHECKPOINT_LIMIT).then_some(()).ok_or(())
        })
        .expect("checkpoint limit is generous")
    }

    #[test]
    fn direction_step_uses_screen_coordinates() {
        let right = directional_blur_step(0.0);
        assert!((right[0] - 1.0).abs() < 1.0e-6);
        assert!(right[1].abs() < 1.0e-6);
        let down = directional_blur_step(90.0);
        assert!(down[0].abs() < 1.0e-6);
        assert!((down[1] - 1.0).abs() < 1.0e-6);
        let up = directional_blur_step(270.0);
        assert!(up[0].abs() < 1.0e-6);
        assert!((up[1] + 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn halo_covers_half_the_blur_length() {
        assert_eq!(directional_blur_axis_halo(0.0), Some((0, 0)));
        assert_eq!(directional_blur_axis_halo(4.0), Some((4, 4)));
        assert_eq!(directional_blur_axis_halo(5.0), Some((5, 5)));
        assert_eq!(directional_blur_axis_halo(f32::NAN), None);
        assert_eq!(
            directional_blur_axis_halo(DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS + 1.0),
            None
        );
    }

    #[test]
    fn taps_grow_with_length_and_stop_at_the_cap() {
        assert_eq!(directional_blur_taps(0.0), 1);
        assert_eq!(directional_blur_taps(1.0), 1);
        assert_eq!(directional_blur_taps(2.5), 3);
        assert_eq!(directional_blur_taps(10_000.0), MAX_DIRECTIONAL_BLUR_TAPS);
    }

    #[test]
    fn zero_length_is_an_identity() {
        let original = vec![[0.25, 0.5, 0.75, 1.0], [0.0, 0.0, 0.0, 0.0]];
        let mut pixels = original.clone();
        assert!(blur(&mut pixels, 2, 1, 0.0, 33.0));
        assert_eq!(pixels, original);
    }

    #[test]
    fn invalid_parameters_are_reported_as_unsupported() {
        let mut pixels = vec![[0.5, 0.5, 0.5, 1.0]];
        assert!(!blur(&mut pixels, 1, 1, f32::NAN, 0.0));
        assert!(!blur(&mut pixels, 1, 1, -4.0, 0.0));
        assert!(!blur(
            &mut pixels,
            1,
            1,
            DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS + 1.0,
            0.0
        ));
        assert!(!blur(&mut pixels, 1, 1, 4.0, f32::INFINITY));
    }

    #[test]
    fn constant_frames_survive_any_direction() {
        let mut pixels = vec![[0.25, 0.5, 0.75, 1.0]; 7 * 5];
        assert!(blur(&mut pixels, 7, 5, 5.0, 30.0));
        for pixel in &pixels {
            assert!((pixel[0] - 0.25).abs() < 1.0e-6, "{pixel:?}");
            assert!((pixel[1] - 0.5).abs() < 1.0e-6, "{pixel:?}");
            assert!((pixel[2] - 0.75).abs() < 1.0e-6, "{pixel:?}");
            assert!((pixel[3] - 1.0).abs() < 1.0e-6, "{pixel:?}");
        }
    }

    #[test]
    fn horizontal_blur_spreads_the_bright_pixel_along_its_row() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 9];
        pixels[4] = [1.0, 1.0, 1.0, 1.0];
        assert!(blur(&mut pixels, 9, 1, 3.0, 0.0));
        assert!(pixels[4][0] > 0.0);
        assert!((pixels[3][0] - pixels[5][0]).abs() < 1.0e-6);
        assert!(pixels[3][0] > 0.0);
        assert_eq!(pixels[0][0], 0.0);
        assert_eq!(pixels[8][0], 0.0);
        let total: f32 = pixels.iter().map(|pixel| pixel[0]).sum();
        assert!((total - 1.0).abs() < 1.0e-4, "mass {total}");
    }

    #[test]
    fn vertical_direction_only_affects_its_column() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 9];
        pixels[4] = [1.0, 1.0, 1.0, 1.0];
        assert!(blur(&mut pixels, 1, 9, 3.0, 90.0));
        assert!(pixels[4][0] > 0.0);
        assert!((pixels[3][0] - pixels[5][0]).abs() < 1.0e-6);
        assert!(pixels[3][0] > 0.0);
        assert_eq!(pixels[0][0], 0.0);
        assert_eq!(pixels[8][0], 0.0);
    }

    #[test]
    fn diagonal_direction_reaches_diagonal_neighbours() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 49];
        pixels[3 * 7 + 3] = [1.0, 1.0, 1.0, 1.0];
        assert!(blur(&mut pixels, 7, 7, 3.0, 45.0));
        assert!(pixels[2 * 7 + 2][0] > 0.0, "up-left sample");
        assert!(pixels[4 * 7 + 4][0] > 0.0, "down-right sample");
        assert_eq!(pixels[2 * 7 + 4][0], 0.0, "up-right must stay untouched");
    }

    #[test]
    fn transparent_neighbours_do_not_tint_an_opaque_pixel() {
        let mut pixels = vec![
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        assert!(blur(&mut pixels, 3, 1, 3.0, 0.0));
        let centre = pixels[1];
        assert!(centre[0].abs() < 1.0e-6, "red leaked in: {centre:?}");
        assert!(centre[2].abs() < 1.0e-6, "blue leaked in: {centre:?}");
        assert!((centre[1] - 1.0).abs() < 1.0e-6, "green lost: {centre:?}");
        assert!(centre[3] < 1.0, "coverage should be diluted: {centre:?}");
    }

    #[test]
    fn hdr_values_are_not_clamped() {
        let mut pixels = vec![
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 8.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert!(blur(&mut pixels, 3, 1, 4.0, 0.0));
        assert!(pixels[1][1] > 1.0, "HDR value was clamped: {pixels:?}");
    }

    #[test]
    fn fractional_lengths_stay_symmetric_and_conservative() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 11];
        pixels[5] = [1.0, 1.0, 1.0, 1.0];
        assert!(blur(&mut pixels, 11, 1, 2.5, 0.0));
        assert!((pixels[4][0] - pixels[6][0]).abs() < 1.0e-6);
        let total: f32 = pixels.iter().map(|pixel| pixel[0]).sum();
        assert!((total - 1.0).abs() < 1.0e-4, "mass {total}");
    }
}
