//! Deinterlace: rebuild the dropped field from the retained lines.
//!
//! The effect removes the comb and the streaks of interlaced material by
//! keeping one field and reconstructing the other. The render op only carries a
//! validated [`DeinterlaceGrade`]; the kernel below is the whole effect.
//!
//! # The filter
//!
//! ```text
//! keep  = rows with y % 2 == 0          the retained field, top line first
//! rebuild = rows with y % 2 == 1
//! S      = 1 + floor(softness / 250)    taps per side, 1..=4
//! d(k)   = 2k - 1                       distance to the k-th retained row
//! w(k)   = 1 / d(k)
//!
//! P(y)   = premultiplied sample
//! out(y) = sum_k w(k) [P(y - d(k)) + P(y + d(k))] / sum_k w(k) [both inside]
//! ```
//!
//! The retained rows are copied through unchanged, bit for bit. A rebuilt row
//! averages the nearest retained rows above and below it; a neighbour outside
//! the frame simply does not contribute, so the top and bottom rows use the one
//! side that exists instead of repeating a border or reading zero.
//!
//! `softness` is the single authored control and it only widens the kernel:
//! `1 + floor(softness / 250)` taps per side, so `0` averages the two nearest
//! retained rows and `1000` reaches seven rows out. The step size is a code
//! constant, not a parameter, because the effect list declares exactly one
//! control. There is no horizontal pass: the comb is a vertical structure, and a
//! horizontal blur would only cost detail.
//!
//! # Why the average is premultiplied
//!
//! The frame reaching this kernel is straight alpha, but the GPU pass reads the
//! compositor's premultiplied working texture, and both sides have to produce
//! the same samples. The kernel therefore premultiplies every tap, averages,
//! then unpremultiplies the result:
//!
//! ```text
//! alpha     = average of the clamped tap alphas
//! alpha <= 0 -> (0, 0, 0, 0)
//! rgb       = average of the premultiplied rgb / alpha
//! ```
//!
//! An uncovered pixel can therefore only ever be written as `(0, 0, 0, 0)`, so
//! this effect registers **no authoring footprint**: the criterion is "can an
//! uncovered pixel gain colour?", and a weighted mean of premultiplied samples
//! cannot.
//!
//! # Why there is no identity shortcut
//!
//! `softness = 0` still rebuilds every dropped row, so the effect changes any
//! frame taller than one pixel. Only a single-row frame (and an empty one) is
//! returned untouched, and both are guarded in the kernel rather than elided by
//! the graph builder.
//!
//! # GPU packing
//!
//! [`DeinterlaceGrade::gpu_controls`] returns the stage-level pair the renderer
//! splices into the shared `SpatialPassUniform`:
//!
//! ```text
//! params = [softness, taps, 0, 0]      extra = [0, 0, 0, 0]
//! ```
//!
//! One draw, one texture: the pass reads the source and writes the frame, and
//! the vertical reach is `2 * taps - 1` rows.
//!
//! # Tests
//!
//! The unit tests cover the rejection path, the tap table, the retained rows,
//! the rebuilt row, the widening kernel, the alpha contract, a transparent
//! neighbourhood, the single-row guard, determinism, the checkpoint, the halo
//! and the packed lanes.

/// The largest authored softness.
pub const DEINTERLACE_MAX_SOFTNESS: f32 = 1000.0;
/// The smallest authored softness.
pub const DEINTERLACE_MIN_SOFTNESS: f32 = 0.0;
/// The documented default softness: the two nearest retained rows only.
pub const DEINTERLACE_DEFAULT_SOFTNESS: f32 = 0.0;
/// The softness step that adds one tap per side.
pub const DEINTERLACE_SOFTNESS_PER_TAP: f32 = 250.0;
/// The widest kernel, in taps per side.
pub const DEINTERLACE_MAX_TAPS: u32 = 4;

const CHECKPOINT_STRIDE: usize = 4096;

/// The rejection reason of a [`DeinterlaceGrade`] field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeinterlaceError {
    /// The softness is outside `[0, 1000]` or not finite.
    Softness,
}

impl std::fmt::Display for DeinterlaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("deinterlace softness must be a finite value inside [0, 1000]")
    }
}

impl std::error::Error for DeinterlaceError {}

/// The authored parameters of one deinterlace operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeinterlaceGrade {
    softness: f32,
}

impl DeinterlaceGrade {
    /// Builds a grade from the authored softness.
    ///
    /// # Errors
    ///
    /// Returns [`DeinterlaceError::Softness`] when the value is not finite or
    /// outside the documented range. The value is never clamped: silently
    /// rewriting an authored `1200` to `1000` would hide the authoring bug.
    pub fn new(softness: f32) -> Result<Self, DeinterlaceError> {
        if !softness.is_finite()
            || !(DEINTERLACE_MIN_SOFTNESS..=DEINTERLACE_MAX_SOFTNESS).contains(&softness)
        {
            return Err(DeinterlaceError::Softness);
        }
        Ok(Self { softness })
    }

    /// The documented default grade: the two nearest retained rows only.
    #[must_use]
    pub const fn documented_defaults() -> Self {
        Self { softness: DEINTERLACE_DEFAULT_SOFTNESS }
    }

    /// The authored softness.
    #[must_use]
    pub const fn softness(&self) -> f32 {
        self.softness
    }

    /// How many retained rows each side of the rebuilt row contribute.
    ///
    /// The value is clamped exactly like the shader clamps its packed lane, so
    /// a directly constructed op can never run an unbounded loop.
    #[must_use]
    pub fn taps(&self) -> u32 {
        let steps = (self.softness / DEINTERLACE_SOFTNESS_PER_TAP).floor();
        let taps = 1.0 + steps;
        taps.clamp(1.0, DEINTERLACE_MAX_TAPS as f32) as u32
    }

    /// The vertical reach of the kernel, in rows.
    ///
    /// The furthest tap sits `2 * taps - 1` rows away.
    #[must_use]
    pub fn halo(&self) -> u32 {
        2 * self.taps() - 1
    }

    /// The stage-level lanes the renderer splices into `SpatialPassUniform`.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        ([self.softness, self.taps() as f32, 0.0, 0.0], [0.0; 4])
    }

    /// The relative dispatch cost, in whole passes.
    #[must_use]
    pub const fn cost(&self) -> u32 {
        1
    }

    /// Hashes every authored value that changes the kernel.
    ///
    /// Only the softness selects the tap table, and the tap count is derived
    /// from it, so one lane is enough.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.softness.to_bits().hash(state);
    }
}

/// Applies the deinterlace to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn deinterlace_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &DeinterlaceGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_deinterlace_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

fn premultiply(pixel: [f32; 4]) -> [f32; 4] {
    let alpha = pixel[3].clamp(0.0, 1.0);
    [pixel[0] * alpha, pixel[1] * alpha, pixel[2] * alpha, alpha]
}

fn unpremultiply(pixel: [f32; 4]) -> [f32; 4] {
    let alpha = pixel[3].clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    [pixel[0] / alpha, pixel[1] / alpha, pixel[2] / alpha, alpha]
}

/// The checkpointed kernel behind [`deinterlace_frame_f32`].
///
/// No full-frame plane is live: a rebuilt row is read from the retained rows
/// only, which this kernel never writes, so the frame can be updated in place.
/// That is why `render_op_f32_scratch_frames` reports zero frames.
pub(crate) fn apply_deinterlace_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &DeinterlaceGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    // A single-row frame has no dropped row, so there is nothing to rebuild.
    if height < 2 {
        return Ok(true);
    }
    let taps = grade.taps();
    let mut visited = 0_usize;
    let mut row_out = vec![[0.0_f32; 4]; width];
    for row in (1..height).step_by(2) {
        {
            let frame = &working[..pixel_count];
            for (column, destination) in row_out.iter_mut().enumerate() {
                if visited.is_multiple_of(CHECKPOINT_STRIDE) {
                    checkpoint()?;
                }
                visited += 1;
                let mut sum = [0.0_f32; 4];
                let mut weight_sum = 0.0_f32;
                for tap in 1..=taps {
                    let distance = 2 * tap - 1;
                    let weight = 1.0 / distance as f32;
                    let above = row as isize - distance as isize;
                    if above >= 0 {
                        let sample = frame[above as usize * width + column];
                        let premultiplied = premultiply(sample);
                        for channel in 0..4 {
                            sum[channel] += weight * premultiplied[channel];
                        }
                        weight_sum += weight;
                    }
                    let below = row + distance as usize;
                    if below < height {
                        let sample = frame[below * width + column];
                        let premultiplied = premultiply(sample);
                        for channel in 0..4 {
                            sum[channel] += weight * premultiplied[channel];
                        }
                        weight_sum += weight;
                    }
                }
                *destination = if weight_sum <= 0.0 {
                    [0.0; 4]
                } else {
                    unpremultiply([
                        sum[0] / weight_sum,
                        sum[1] / weight_sum,
                        sum[2] / weight_sum,
                        sum[3] / weight_sum,
                    ])
                };
            }
        }
        {
            let frame = &mut working[..pixel_count];
            frame[row * width..row * width + width].copy_from_slice(&row_out);
        }
        checkpoint()?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.0; 4]; width * height]
    }

    #[test]
    fn rejects_softness_outside_the_documented_range() {
        assert_eq!(
            DeinterlaceGrade::new(-1.0).err(),
            Some(DeinterlaceError::Softness)
        );
        assert_eq!(
            DeinterlaceGrade::new(1000.1).err(),
            Some(DeinterlaceError::Softness)
        );
        assert_eq!(
            DeinterlaceGrade::new(f32::NAN).err(),
            Some(DeinterlaceError::Softness)
        );
        assert!(DeinterlaceGrade::new(0.0).is_ok());
        assert!(DeinterlaceGrade::new(1000.0).is_ok());
    }

    #[test]
    fn the_documented_default_rebuilds_the_nearest_rows() {
        let grade = DeinterlaceGrade::documented_defaults();
        assert_eq!(grade.softness(), 0.0);
        assert_eq!(grade.taps(), 1);
        assert_eq!(grade.halo(), 1);
    }

    #[test]
    fn softness_widens_the_kernel_by_one_tap_per_step() {
        let taps =
            |softness: f32| DeinterlaceGrade::new(softness).expect("the value is in range").taps();
        assert_eq!(taps(0.0), 1);
        assert_eq!(taps(249.0), 1);
        assert_eq!(taps(250.0), 2);
        assert_eq!(taps(499.0), 2);
        assert_eq!(taps(500.0), 3);
        assert_eq!(taps(750.0), 4);
        assert_eq!(taps(1000.0), DEINTERLACE_MAX_TAPS);
    }

    #[test]
    fn retained_rows_are_copied_through_bit_for_bit() {
        let mut pixels = frame(3, 4);
        for row in 0..4 {
            for column in 0..3 {
                let value = row as f32 + column as f32 * 0.5;
                pixels[row * 3 + column] = [value, value * 2.0, value * 0.25, 1.0];
            }
        }
        let original = pixels.clone();
        let grade = DeinterlaceGrade::new(1000.0).expect("the value is in range");
        assert!(deinterlace_frame_f32(&mut pixels, 3, 4, &grade));
        for row in [0_usize, 2] {
            for column in 0..3 {
                assert_eq!(pixels[row * 3 + column], original[row * 3 + column]);
            }
        }
    }

    #[test]
    fn a_rebuilt_row_is_the_weighted_mean_of_the_retained_rows() {
        let mut pixels = frame(1, 3);
        pixels[0] = [1.0, 0.0, 0.0, 1.0];
        pixels[1] = [0.0, 0.0, 0.0, 1.0];
        pixels[2] = [3.0, 0.0, 0.0, 1.0];
        let grade = DeinterlaceGrade::documented_defaults();
        assert!(deinterlace_frame_f32(&mut pixels, 1, 3, &grade));
        assert_eq!(pixels[1], [2.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_wide_kernel_reaches_further_and_weighs_the_near_rows_more() {
        let mut pixels = frame(1, 5);
        pixels[0] = [1.0, 0.0, 0.0, 1.0];
        pixels[1] = [0.0, 0.0, 0.0, 1.0];
        pixels[2] = [3.0, 0.0, 0.0, 1.0];
        pixels[3] = [0.0, 0.0, 0.0, 1.0];
        pixels[4] = [5.0, 0.0, 0.0, 1.0];
        let grade = DeinterlaceGrade::new(250.0).expect("the value is in range");
        assert_eq!(grade.taps(), 2);
        assert!(deinterlace_frame_f32(&mut pixels, 1, 5, &grade));
        // Row 1 averages rows 0 and 2 at weight one, and row 4 at weight 1/3.
        let expected = (1.0 + 3.0 + 5.0 / 3.0) / (1.0 + 1.0 + 1.0 / 3.0);
        assert!(
            (pixels[1][0] - expected).abs() <= 1.0e-6,
            "{}",
            pixels[1][0]
        );
    }

    #[test]
    fn the_edge_rows_use_the_side_that_exists() {
        let mut pixels = frame(1, 4);
        pixels[0] = [2.0, 0.0, 0.0, 1.0];
        pixels[1] = [0.0; 4];
        pixels[2] = [4.0, 0.0, 0.0, 1.0];
        pixels[3] = [0.0; 4];
        let grade = DeinterlaceGrade::new(1000.0).expect("the value is in range");
        assert!(deinterlace_frame_f32(&mut pixels, 1, 4, &grade));
        // Row 1 sees row 0 above and row 2 below; row 3 sees rows 2 and 0 only,
        // because rows 4 and 6 are outside the frame.
        assert_eq!(pixels[1][3], 1.0);
        assert_eq!(pixels[3][3], 1.0);
    }

    #[test]
    fn the_alpha_of_a_rebuilt_row_follows_the_same_weights() {
        let mut pixels = frame(1, 3);
        pixels[0] = [1.0, 0.0, 0.0, 0.5];
        pixels[1] = [0.0; 4];
        pixels[2] = [1.0, 0.0, 0.0, 0.5];
        let grade = DeinterlaceGrade::documented_defaults();
        assert!(deinterlace_frame_f32(&mut pixels, 1, 3, &grade));
        assert_eq!(pixels[1], [1.0, 0.0, 0.0, 0.5]);
    }

    #[test]
    fn a_transparent_neighbourhood_stays_transparent() {
        let mut pixels = frame(2, 3);
        pixels[0] = [4.0, 2.0, 1.0, 0.0];
        pixels[2] = [4.0, 2.0, 1.0, 0.0];
        let grade = DeinterlaceGrade::new(1000.0).expect("the value is in range");
        assert!(deinterlace_frame_f32(&mut pixels, 2, 3, &grade));
        for column in 0..2 {
            assert_eq!(pixels[2 + column], [0.0; 4]);
        }
    }

    #[test]
    fn a_single_row_frame_is_untouched() {
        let mut pixels = vec![[0.25, 0.5, 0.75, 1.0]];
        let grade = DeinterlaceGrade::new(1000.0).expect("the value is in range");
        assert!(deinterlace_frame_f32(&mut pixels, 1, 1, &grade));
        assert_eq!(pixels, vec![[0.25, 0.5, 0.75, 1.0]]);
    }

    #[test]
    fn an_empty_frame_is_refused() {
        let mut pixels: Vec<[f32; 4]> = Vec::new();
        let grade = DeinterlaceGrade::documented_defaults();
        assert!(!deinterlace_frame_f32(&mut pixels, 0, 0, &grade));
        let mut too_short = vec![[0.0; 4]; 2];
        assert!(!deinterlace_frame_f32(&mut too_short, 3, 3, &grade));
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let mut first = frame(4, 6);
        for (index, pixel) in first.iter_mut().enumerate() {
            *pixel = [index as f32 * 0.125, index as f32 * 0.25, 0.5, 1.0];
        }
        let mut second = first.clone();
        let grade = DeinterlaceGrade::new(500.0).expect("the value is in range");
        assert!(deinterlace_frame_f32(&mut first, 4, 6, &grade));
        assert!(deinterlace_frame_f32(&mut second, 4, 6, &grade));
        assert_eq!(first, second);
    }

    #[test]
    fn the_checkpoint_is_honoured() {
        let mut pixels = frame(8, 8);
        let grade = DeinterlaceGrade::documented_defaults();
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            if calls > 1 {
                return Err("cancelled");
            }
            Ok(())
        };
        assert_eq!(
            apply_deinterlace_f32_controlled(&mut pixels, 8, 8, &grade, &mut checkpoint),
            Err("cancelled")
        );
    }

    #[test]
    fn the_packed_lanes_carry_the_softness_and_the_tap_count() {
        let grade = DeinterlaceGrade::new(600.0).expect("the value is in range");
        assert_eq!(grade.gpu_controls(), ([600.0, 3.0, 0.0, 0.0], [0.0; 4]));
        assert_eq!(grade.cost(), 1);
        assert_eq!(grade.halo(), 5);
    }
}
