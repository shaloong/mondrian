//! Watercolor: edge-aware pigment diffusion with a selectable guide channel.
//!
//! The authored form carries two parameters: which channel guides the
//! diffusion, and how much of the diffused result replaces the source.  The
//! list's host effect (`Resolve FX Watercolor`) exposes the same pair: a
//! channel menu whose default is `Luminance Only`, and a smoothness of `0.01`.
//!
//! The kernel averages a square window around every pixel with two weights
//! multiplied together:
//!
//! * a spatial weight `1 / (1 + d²)` that fades the window out, and
//! * an edge weight `1 / (1 + (Δguide / sigma)²)` that keeps pigment from
//!   crossing strong edges, where `sigma = 0.04 + 0.36 * smoothness`.
//!
//! The window radius grows with smoothness (`1 + round(3 * smoothness)`, from
//! one to four pixels), and the diffused result is mixed over the source by the
//! authored smoothness.  A smoothness of zero is therefore the exact identity,
//! which is also why the default `0.01` reads as a barely visible pass.
//!
//! Averaging happens on premultiplied samples, exactly like the shader, and the
//! result returns to the public straight-alpha contract.  Nothing here reads
//! another frame or the clock, so the effect is a pure function of its grade.

use thiserror::Error;

/// Largest smoothness the grade accepts.
pub const WATERCOLOR_MAX_SMOOTHNESS: f32 = 1.0;
/// Smallest smoothness the grade accepts.
pub const WATERCOLOR_MIN_SMOOTHNESS: f32 = 0.0;
/// The documented default smoothness.
pub const WATERCOLOR_DEFAULT_SMOOTHNESS: f32 = 0.01;
/// Largest window radius the smoothest grade reaches, in pixels.
pub const WATERCOLOR_MAX_RADIUS: u32 = 4;

/// Guide tolerance at zero smoothness, in channel units.
const EDGE_FLOOR: f32 = 0.04;
/// Guide tolerance gained at full smoothness.
const EDGE_GAIN: f32 = 0.36;
/// Radius growth per unit of smoothness.
const RADIUS_GAIN: f32 = 3.0;

/// Channel that guides the diffusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatercolorChannel {
    /// Guide on Rec.709 luma.
    Luminance,
    /// Guide on the red channel.
    Red,
    /// Guide on the green channel.
    Green,
    /// Guide on the blue channel.
    Blue,
}

impl WatercolorChannel {
    /// Every guide channel, in menu order.
    pub const ALL: [Self; 4] = [Self::Luminance, Self::Red, Self::Green, Self::Blue];

    /// Maps an authored enumeration key onto a channel.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "luminance" => Some(Self::Luminance),
            "red" => Some(Self::Red),
            "green" => Some(Self::Green),
            "blue" => Some(Self::Blue),
            _ => None,
        }
    }

    /// Stable key used by the authoring schema and by cached identities.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Luminance => "luminance",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
        }
    }

    /// The localized caption shown in the menu; the catalog translates it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Luminance => "Luminance",
            Self::Red => "Red",
            Self::Green => "Green",
            Self::Blue => "Blue",
        }
    }

    /// Stable tag used by the op hash and by the GPU plan.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Luminance => 0,
            Self::Red => 1,
            Self::Green => 2,
            Self::Blue => 3,
        }
    }
}

/// Reasons a watercolor grade cannot be built from its authored values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum WatercolorError {
    #[error("watercolor smoothness must be finite and within [0, 1]")]
    Smoothness,
}

/// Validated watercolor parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WatercolorGrade {
    channel: WatercolorChannel,
    smoothness: f32,
}

impl WatercolorGrade {
    /// Validates one authored pair.
    pub fn new(channel: WatercolorChannel, smoothness: f32) -> Result<Self, WatercolorError> {
        if !smoothness.is_finite()
            || !(WATERCOLOR_MIN_SMOOTHNESS..=WATERCOLOR_MAX_SMOOTHNESS).contains(&smoothness)
        {
            return Err(WatercolorError::Smoothness);
        }
        Ok(Self { channel, smoothness })
    }

    /// The documented default grade: luma guide at a smoothness of `0.01`.
    pub const fn documented_defaults() -> Self {
        Self {
            channel: WatercolorChannel::Luminance,
            smoothness: WATERCOLOR_DEFAULT_SMOOTHNESS,
        }
    }

    /// The authored guide channel.
    pub const fn channel(&self) -> WatercolorChannel {
        self.channel
    }

    /// The authored smoothness.
    pub const fn smoothness(&self) -> f32 {
        self.smoothness
    }

    /// The window radius in pixels, from one to
    /// [`WATERCOLOR_MAX_RADIUS`].
    pub fn radius(&self) -> u32 {
        let grown = (RADIUS_GAIN * self.smoothness).round() as u32;
        (1 + grown).clamp(1, WATERCOLOR_MAX_RADIUS)
    }

    /// The guide tolerance used by the edge weight.
    pub fn edge_scale(&self) -> f32 {
        EDGE_FLOOR + EDGE_GAIN * self.smoothness
    }

    /// Source neighbourhood the kernel reads on every side.
    pub fn input_halo(&self) -> (u32, u32) {
        let radius = self.radius();
        (radius, radius)
    }

    /// A zero smoothness leaves the frame alone.
    pub fn is_identity(&self) -> bool {
        self.smoothness <= WATERCOLOR_MIN_SMOOTHNESS
    }

    /// The stage-level uniform payload: `params` and `extra`.
    ///
    /// `params` is `[guide channel tag, radius, edge tolerance, smoothness]`
    /// and `extra` is reserved.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                f32::from(self.channel.as_u8()),
                self.radius() as f32,
                self.edge_scale(),
                self.smoothness(),
            ],
            [0.0; 4],
        )
    }

    /// The relative cost the estimator quotes for one window pass.
    pub const fn cost(&self) -> u32 {
        2
    }

    /// Hashes every authored scalar so a cache key cannot alias two grades.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.channel.as_u8().hash(state);
        self.smoothness.to_bits().hash(state);
    }
}

/// Applies the watercolor diffusion to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn watercolor_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &WatercolorGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_watercolor_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed kernel behind [`watercolor_frame_f32`].
///
/// The kernel keeps one full-frame snapshot of the straight source because a
/// pixel reads rows other than its own; that snapshot is why
/// `render_op_f32_scratch_frames` reports one frame.  Every write lands in the
/// destination row's own slice and never feeds a later read.
pub(crate) fn apply_watercolor_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &WatercolorGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let frame = &mut working[..pixel_count];
    checkpoint()?;

    let source = frame.to_vec();
    let channel = grade.channel();
    let radius = grade.radius().clamp(1, WATERCOLOR_MAX_RADIUS) as isize;
    let edge_scale = grade.edge_scale();
    let mix = grade.smoothness();
    let rows = pixel_count / width;

    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, destination) in chunk.iter_mut().enumerate() {
                let index = row * width + column;
                let centre = source[index];
                let centre_guide = guide(centre, channel);
                let mut sum = [0.0_f32; 4];
                let mut weight_sum = 0.0_f32;
                for offset_y in -radius..=radius {
                    let sample_row =
                        (row as isize + offset_y).clamp(0, height as isize - 1) as usize;
                    for offset_x in -radius..=radius {
                        let sample_column =
                            (column as isize + offset_x).clamp(0, width as isize - 1) as usize;
                        let sample = source[sample_row * width + sample_column];
                        let distance = (offset_x * offset_x + offset_y * offset_y) as f32;
                        let spatial = 1.0 / (1.0 + distance);
                        // The shader folds the two weights the same way, so the
                        // products stay associatively identical across devices.
                        let scaled = (guide(sample, channel) - centre_guide) / edge_scale;
                        let edge = 1.0 / (1.0 + scaled * scaled);
                        let weight = spatial * edge;
                        let premultiplied = premultiply(sample);
                        for lane in 0..4 {
                            sum[lane] += premultiplied[lane] * weight;
                        }
                        weight_sum += weight;
                    }
                }
                if weight_sum <= 0.0 {
                    continue;
                }
                let mean = [
                    sum[0] / weight_sum,
                    sum[1] / weight_sum,
                    sum[2] / weight_sum,
                    sum[3] / weight_sum,
                ];
                let diffused = crate::coverage::straight_rgba_from_premultiplied(mean);
                *destination = crate::coverage::mix_straight_rgba(centre, diffused, mix);
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

/// The premultiplied sample, with the coverage clamped exactly like the shader.
fn premultiply(pixel: [f32; 4]) -> [f32; 4] {
    let alpha = pixel[3].clamp(0.0, 1.0);
    [pixel[0] * alpha, pixel[1] * alpha, pixel[2] * alpha, alpha]
}

/// The guide value of one straight sample.
fn guide(pixel: [f32; 4], channel: WatercolorChannel) -> f32 {
    if pixel[3].clamp(0.0, 1.0) <= 0.0 {
        return 0.0;
    }
    match channel {
        WatercolorChannel::Luminance => 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2],
        WatercolorChannel::Red => pixel[0],
        WatercolorChannel::Green => pixel[1],
        WatercolorChannel::Blue => pixel[2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(smoothness: f32) -> WatercolorGrade {
        WatercolorGrade::new(WatercolorChannel::Luminance, smoothness).expect("valid grade")
    }

    fn flat(width: usize, height: usize, pixel: [f32; 4]) -> Vec<[f32; 4]> {
        vec![pixel; width * height]
    }

    fn checkerboard(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| {
                let on = (index % width + index / width).is_multiple_of(2);
                if on {
                    [0.9, 0.2, 0.1, 1.0]
                } else {
                    [0.05, 0.1, 0.7, 1.0]
                }
            })
            .collect()
    }

    #[test]
    fn smoothness_range_is_enforced() {
        assert_eq!(
            WatercolorGrade::new(WatercolorChannel::Luminance, -0.001).unwrap_err(),
            WatercolorError::Smoothness
        );
        assert_eq!(
            WatercolorGrade::new(WatercolorChannel::Red, 1.001).unwrap_err(),
            WatercolorError::Smoothness
        );
        assert_eq!(
            WatercolorGrade::new(WatercolorChannel::Green, f32::NAN).unwrap_err(),
            WatercolorError::Smoothness
        );
        assert!(WatercolorGrade::new(WatercolorChannel::Blue, 0.0).is_ok());
        assert!(WatercolorGrade::new(WatercolorChannel::Blue, 1.0).is_ok());
    }

    #[test]
    fn documented_defaults_are_the_listed_pair() {
        let defaults = WatercolorGrade::documented_defaults();
        assert_eq!(defaults.channel(), WatercolorChannel::Luminance);
        assert_eq!(defaults.smoothness(), 0.01);
        assert_eq!(defaults.radius(), 1);
        assert_eq!(defaults.input_halo(), (1, 1));
        assert_eq!(defaults.cost(), 2);
        assert_eq!(
            defaults.gpu_controls(),
            ([0.0, 1.0, 0.0436, 0.01], [0.0; 4])
        );
        assert!(!defaults.is_identity());
    }

    #[test]
    fn radius_steps_with_smoothness() {
        assert_eq!(grade(0.0).radius(), 1);
        assert_eq!(grade(0.1).radius(), 1);
        assert_eq!(grade(0.3).radius(), 2);
        assert_eq!(grade(0.5).radius(), 3);
        assert_eq!(grade(0.75).radius(), 3);
        assert_eq!(grade(1.0).radius(), 4);
    }

    #[test]
    fn zero_smoothness_is_the_exact_identity() {
        let mut frame = checkerboard(7, 5);
        let original = frame.clone();
        assert!(watercolor_frame_f32(&mut frame, 7, 5, &grade(0.0)));
        assert_eq!(frame, original);
    }

    #[test]
    fn uniform_frames_survive_every_radius() {
        for smoothness in [0.25, 0.5, 1.0] {
            let mut frame = flat(6, 4, [0.25, 0.5, 0.75, 1.0]);
            assert!(watercolor_frame_f32(&mut frame, 6, 4, &grade(smoothness)));
            for pixel in frame {
                for lane in 0..4 {
                    assert!((pixel[lane] - [0.25, 0.5, 0.75, 1.0][lane]).abs() < 1.0e-5);
                }
            }
        }
    }

    #[test]
    fn an_impulse_spreads_and_dims() {
        let mut frame = vec![[0.0, 0.0, 0.0, 1.0]; 9 * 9];
        frame[4 * 9 + 4] = [1.0, 1.0, 1.0, 1.0];
        let original = frame.clone();
        assert!(watercolor_frame_f32(&mut frame, 9, 9, &grade(1.0)));
        // The centre keeps some of its energy and its neighbours gain some.
        assert!(frame[4 * 9 + 4][0] < original[4 * 9 + 4][0]);
        assert!(frame[4 * 9 + 4][0] > frame[4 * 9 + 5][0]);
        assert!(frame[4 * 9 + 5][0] > 0.0);
        assert!(frame[3 * 9 + 4][0] > 0.0);
        // The window fades with distance: the corner is inside the radius but
        // receives far less pigment than an adjacent neighbour.
        assert!(frame[0][0] > 0.0);
        assert!(frame[0][0] < frame[4 * 9 + 5][0]);
    }

    #[test]
    fn the_guide_channel_changes_the_diffusion() {
        // A red impulse against black: a red guide sees a full step, while the
        // luma guide only sees 0.2126 of one.
        let mut red_guided = vec![[0.0, 0.0, 0.0, 1.0]; 9 * 5];
        let mut luma_guided = red_guided.clone();
        red_guided[2 * 9 + 4] = [1.0, 0.0, 0.0, 1.0];
        luma_guided[2 * 9 + 4] = [1.0, 0.0, 0.0, 1.0];
        let red_grade = WatercolorGrade::new(WatercolorChannel::Red, 1.0).expect("valid");
        let luma_grade = WatercolorGrade::new(WatercolorChannel::Luminance, 1.0).expect("valid");
        assert!(watercolor_frame_f32(&mut red_guided, 9, 5, &red_grade));
        assert!(watercolor_frame_f32(&mut luma_guided, 9, 5, &luma_grade));
        assert_ne!(red_guided, luma_guided);
        // The red guide weights the step down harder, so less energy leaves the
        // centre.
        assert!(red_guided[2 * 9 + 4][0] > luma_guided[2 * 9 + 4][0]);
    }

    #[test]
    fn partial_smoothness_lands_between_the_source_and_the_full_diffusion() {
        let mut half = checkerboard(8, 6);
        let mut full = half.clone();
        let original = half.clone();
        assert!(watercolor_frame_f32(&mut half, 8, 6, &grade(0.5)));
        assert!(watercolor_frame_f32(&mut full, 8, 6, &grade(1.0)));
        let mut moved = 0;
        for index in 0..half.len() {
            let start = original[index][2];
            let end = full[index][2];
            let middle = half[index][2];
            if (end - start).abs() > 0.05 {
                moved += 1;
                assert!(
                    (middle - start) * (end - start) >= 0.0,
                    "half smoothness left the source direction at {index}"
                );
            }
        }
        assert!(moved > 0, "the fixture must actually diffuse");
    }

    #[test]
    fn coverage_is_diffused_alongside_colour() {
        let mut frame = flat(5, 1, [0.8, 0.4, 0.2, 0.0]);
        frame[2] = [0.8, 0.4, 0.2, 1.0];
        assert!(watercolor_frame_f32(&mut frame, 5, 1, &grade(1.0)));
        // Neighbours gain fractional coverage, and the hidden colour stays
        // canonical where the coverage collapses to zero.
        assert!(frame[1][3] > 0.0 && frame[1][3] < 1.0);
        assert_eq!(frame[1][3], frame[3][3]);
        assert!(frame[0][3] > 0.0, "the window must reach the frame edge");
    }

    #[test]
    fn a_fully_transparent_frame_stays_canonical() {
        let mut frame = flat(4, 4, [0.0; 4]);
        assert!(watercolor_frame_f32(&mut frame, 4, 4, &grade(1.0)));
        assert_eq!(frame, flat(4, 4, [0.0; 4]));
    }

    #[test]
    fn tiny_frames_are_clamped_not_panicked() {
        let mut frame = vec![[0.5, 0.5, 0.5, 1.0]];
        assert!(watercolor_frame_f32(&mut frame, 1, 1, &grade(1.0)));
        assert_eq!(frame, vec![[0.5, 0.5, 0.5, 1.0]]);
        let mut narrow = vec![[0.2, 0.4, 0.6, 1.0], [0.8, 0.6, 0.4, 1.0]];
        assert!(watercolor_frame_f32(&mut narrow, 1, 2, &grade(1.0)));
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let mut first = checkerboard(11, 7);
        let mut second = first.clone();
        assert!(watercolor_frame_f32(&mut first, 11, 7, &grade(0.7)));
        assert!(watercolor_frame_f32(&mut second, 11, 7, &grade(0.7)));
        assert_eq!(first, second);
    }

    #[test]
    fn short_or_empty_frames_are_refused() {
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; 3];
        assert!(!watercolor_frame_f32(&mut frame, 2, 2, &grade(1.0)));
        assert!(!watercolor_frame_f32(&mut frame, 0, 3, &grade(1.0)));
        assert!(!watercolor_frame_f32(&mut frame, 3, 0, &grade(1.0)));
    }

    #[test]
    fn cancellation_is_reported() {
        let mut frame = checkerboard(5, 5);
        let mut calls = 0;
        let result = apply_watercolor_f32_controlled(&mut frame, 5, 5, &grade(1.0), &mut || {
            calls += 1;
            if calls > 1 {
                Err("cancelled")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("cancelled"));
    }

    #[test]
    fn gpu_controls_are_the_documented_lanes() {
        let controls = WatercolorGrade::new(WatercolorChannel::Blue, 1.0)
            .expect("valid")
            .gpu_controls();
        assert_eq!(controls.0[0], 3.0);
        assert_eq!(controls.0[1], 4.0);
        assert!((controls.0[2] - 0.4).abs() < 1.0e-6);
        assert_eq!(controls.0[3], 1.0);
        assert_eq!(controls.1, [0.0; 4]);
    }

    #[test]
    fn hash_separates_grades_that_pack_the_same_numbers() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        let mut first = DefaultHasher::new();
        WatercolorGrade::new(WatercolorChannel::Red, 1.0)
            .expect("valid")
            .hash_state(&mut first);
        let mut second = DefaultHasher::new();
        WatercolorGrade::new(WatercolorChannel::Luminance, 1.0)
            .expect("valid")
            .hash_state(&mut second);
        assert_ne!(first.finish(), second.finish());
    }
}
