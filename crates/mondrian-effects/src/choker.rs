//! Choker: erodes or dilates the frame alpha with a separable min/max filter.
//!
//! The kernel is the whole effect; the render op only carries a validated
//! [`ChokerGrade`]. The morphology reads and writes the **alpha channel only**:
//! the straight-alpha RGB of every pixel is preserved verbatim, because the
//! rendered result is premultiplied later by the compositor. Scaling the RGB by
//! the new coverage here would count the coverage twice, so the output pixel is
//! `[r, g, b, morph(alpha)]`.
//!
//! # The filter
//!
//! One authored amount in pixels:
//!
//! ```text
//! amount  > 0  erode   a' = min over the square window of radius |amount|
//! amount  < 0  dilate  a' = max over the square window of radius |amount|
//! amount == 0  identity, and the graph builder elides the op
//! ```
//!
//! A morphology cannot read a fractional tap, so `|amount|` is rounded to the
//! nearest integer tap count on both the CPU and the GPU (`i32(round(x))` in
//! `fs_keylight_erode`). `|amount| < 0.5` therefore renders as a one-tap window
//! and leaves the frame untouched. That is a rendering detail, not an identity:
//! [`ChokerGrade::is_identity`] is true only for `amount == 0`.
//!
//! The square window is separable: a horizontal min/max followed by a vertical
//! one reads exactly `2 * radius + 1` taps per axis. Both axes clamp at the frame
//! edge (`repeat_edge_pixels`), so a window that overhangs the frame repeats the
//! border pixel instead of shrinking.
//!
//! # GPU packing
//!
//! The renderer reuses `fs_keylight_erode`, whose `params` cell is
//! `[radius, axis, dilate, read_frame]`. [`ChokerGrade::gpu_controls`] returns the
//! stage-level pair `[radius, mode, 0, 0]` — `mode` is `0` for the min filter and
//! `1` for the max filter — and the renderer splices those two cells into the
//! per-pass layout: the first pass reads the frame alpha (`read_frame = 1`), the
//! second reads the matte plane (`read_frame = 0`), and the matte plane copies
//! the value into every channel with `a = 1` so every later stage can read `.x`.
//!
//! # Tests
//!
//! The unit tests below cover the documented matrix: positive erode, negative
//! dilate, zero identity, the square window, the frame-edge clamp, the alpha
//! endpoints, bit-identical RGB, the checkpoint cadence, and the largest
//! authored radius.

/// Largest authored choke magnitude in pixels, on both sides of zero.
pub const CHOKER_AUTHOR_MAX_CHOKE_PIXELS: f32 = 100.0;
/// Documented default choke amount: the identity.
pub const CHOKER_DEFAULT_CHOKE_PIXELS: f32 = 0.0;

/// The rejection reason of a [`ChokerGrade`] field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChokerError {
    /// The choke amount is outside `[-100, 100]` or not finite.
    Choke,
}

impl std::fmt::Display for ChokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Choke => "choker choke must be a finite value inside [-100, 100]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ChokerError {}

/// The authored parameters of one choker operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChokerGrade {
    choke: f32,
}

impl ChokerGrade {
    /// Builds a grade from the authored choke amount in pixels.
    ///
    /// # Errors
    ///
    /// Returns [`ChokerError::Choke`] when the amount is not finite or outside
    /// `[-100, 100]`.
    pub fn new(choke: f32) -> Result<Self, ChokerError> {
        if !choke.is_finite() || choke.abs() > CHOKER_AUTHOR_MAX_CHOKE_PIXELS {
            return Err(ChokerError::Choke);
        }
        Ok(Self { choke })
    }

    /// The documented default grade.
    ///
    /// # Errors
    ///
    /// Never fails: the documented default is inside its range. The result is
    /// propagated as a `Result` so callers can share one parsing path.
    pub fn documented_defaults() -> Result<Self, ChokerError> {
        Self::new(CHOKER_DEFAULT_CHOKE_PIXELS)
    }

    /// The authored choke amount in pixels.
    #[must_use]
    pub const fn choke(&self) -> f32 {
        self.choke
    }

    /// The integer morphology radius per axis.
    ///
    /// The filter cannot read a fractional tap, so the authored magnitude is
    /// rounded here and in the shader from the same packed value.
    #[must_use]
    pub fn radius(&self) -> usize {
        self.choke.abs().round() as usize
    }

    /// Whether the filter dilates coverage instead of eroding it.
    #[must_use]
    pub const fn dilates(&self) -> bool {
        self.choke < 0.0
    }

    /// Whether the grade is a no-op and the graph builder may elide it.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.choke == 0.0
    }

    /// The stage-level uniform payload: `params` and `extra`.
    ///
    /// `params` is `[radius, mode, 0, 0]` with `mode = 0` for the min filter and
    /// `1` for the max filter; `extra` is unused. The renderer splices `radius`
    /// and `mode` into the `[radius, axis, dilate, read_frame]` cell that
    /// `fs_keylight_erode` reads.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.radius() as f32,
                f32::from(u8::from(self.dilates())),
                0.0,
                0.0,
            ],
            [0.0; 4],
        )
    }

    /// Hashes every authored scalar so a cache key cannot alias two grades.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.choke.to_bits().hash(state);
    }
}

const CHECKPOINT_STRIDE: usize = 4096;

/// Apply choker to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn choker_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ChokerGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_choker_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed kernel behind [`choker_frame_f32`].
pub(crate) fn apply_choker_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ChokerGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    let radius = grade.radius();
    if grade.is_identity() || radius == 0 {
        return Ok(true);
    }
    let frame = &mut working[..pixel_count];
    checkpoint()?;

    // Stage 1: copy the frame alpha into a single-channel matte plane. The
    // colour is never read again, so a transparent pixel keeps its RGB even
    // when the dilation brings coverage back.
    let mut alpha = Vec::with_capacity(pixel_count);
    for (index, pixel) in frame.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        alpha.push(pixel[3]);
    }
    checkpoint()?;

    // Stage 2: the separable morphology, horizontal then vertical. The square
    // window is the composition of the two one-dimensional windows.
    let dilates = grade.dilates();
    let mut horizontal = vec![0.0_f32; pixel_count];
    morph_horizontal(&alpha, &mut horizontal, width, height, radius, dilates);
    checkpoint()?;
    let mut matte = vec![0.0_f32; pixel_count];
    morph_vertical(&horizontal, &mut matte, width, height, radius, dilates);
    checkpoint()?;

    // Stage 3: write the morphology alpha back over row bands like every other
    // spatial kernel. Only the alpha channel is touched, so the RGB of every
    // pixel stays bit-identical.
    let rows = pixel_count / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                pixel[3] = matte[row * width + column];
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

/// Runs one horizontal min or max filter over the alpha plane.
///
/// The window clamps at the frame edge, so a radius larger than the frame width
/// reads the border column repeatedly instead of running out of taps.
fn morph_horizontal(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
    dilates: bool,
) {
    for row in 0..height {
        for column in 0..width {
            let mut value = if dilates { 0.0_f32 } else { 1.0_f32 };
            for offset in 0..=(2 * radius) {
                let sample = column as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, width as isize - 1) as usize;
                let candidate = source[row * width + clamped];
                value = if dilates {
                    value.max(candidate)
                } else {
                    value.min(candidate)
                };
            }
            destination[row * width + column] = value;
        }
    }
}

/// Runs one vertical min or max filter over the intermediate alpha plane.
fn morph_vertical(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
    dilates: bool,
) {
    for row in 0..height {
        for column in 0..width {
            let mut value = if dilates { 0.0_f32 } else { 1.0_f32 };
            for offset in 0..=(2 * radius) {
                let sample = row as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, height as isize - 1) as usize;
                let candidate = source[clamped * width + column];
                value = if dilates {
                    value.max(candidate)
                } else {
                    value.min(candidate)
                };
            }
            destination[row * width + column] = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_choker_f32_controlled, choker_frame_f32, ChokerError, ChokerGrade,
        CHOKER_AUTHOR_MAX_CHOKE_PIXELS, CHOKER_DEFAULT_CHOKE_PIXELS,
    };

    fn grade(choke: f32) -> ChokerGrade {
        ChokerGrade::new(choke).expect("test grades stay inside the authored range")
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for index in 0..width * height {
            let value = index as f32;
            pixels.push([value + 0.25, value + 0.5, value + 0.75, 1.0]);
        }
        pixels
    }

    fn alphas(pixels: &[[f32; 4]]) -> Vec<f32> {
        pixels.iter().map(|pixel| pixel[3]).collect()
    }

    #[test]
    fn defaults_are_the_documented_identity() {
        let defaults = ChokerGrade::documented_defaults().expect("documented defaults are valid");
        assert_eq!(defaults.choke(), CHOKER_DEFAULT_CHOKE_PIXELS);
        assert!(defaults.is_identity());
        assert_eq!(defaults.radius(), 0);
        assert!(!defaults.dilates());
        assert_eq!(defaults.gpu_controls(), ([0.0, 0.0, 0.0, 0.0], [0.0; 4]));
    }

    #[test]
    fn authored_amount_outside_the_range_is_rejected() {
        for choke in [
            CHOKER_AUTHOR_MAX_CHOKE_PIXELS + 0.1,
            -CHOKER_AUTHOR_MAX_CHOKE_PIXELS - 0.1,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            assert_eq!(ChokerGrade::new(choke), Err(ChokerError::Choke), "{choke}");
        }
        assert!(ChokerGrade::new(CHOKER_AUTHOR_MAX_CHOKE_PIXELS).is_ok());
        assert!(ChokerGrade::new(-CHOKER_AUTHOR_MAX_CHOKE_PIXELS).is_ok());
        assert!(matches!(ChokerGrade::new(-101.0), Err(ChokerError::Choke)));
    }

    #[test]
    fn error_message_names_the_documented_range() {
        assert_eq!(
            ChokerError::Choke.to_string(),
            "choker choke must be a finite value inside [-100, 100]"
        );
    }

    #[test]
    fn radius_rounds_like_the_shader_and_mode_selects_the_filter() {
        let erode = grade(3.0);
        assert_eq!(erode.radius(), 3);
        assert!(!erode.dilates());
        assert_eq!(erode.gpu_controls().0, [3.0, 0.0, 0.0, 0.0]);

        let dilate = grade(-4.0);
        assert_eq!(dilate.radius(), 4);
        assert!(dilate.dilates());
        assert_eq!(dilate.gpu_controls().0, [4.0, 1.0, 0.0, 0.0]);

        // The shader packs the same radius through `i32(round(x))`, so `0.4`
        // renders as a one-tap window and `0.5` as the smallest real window.
        assert_eq!(grade(0.4).radius(), 0);
        assert_eq!(grade(0.5).radius(), 1);
        assert_eq!(grade(-2.5).radius(), 3);
        assert!(!grade(0.4).is_identity());
        assert!(!grade(-0.4).is_identity());
    }

    #[test]
    fn zero_amount_is_the_identity() {
        let mut pixels = frame(4, 3);
        let source = pixels.clone();
        assert!(choker_frame_f32(&mut pixels, 4, 3, &grade(0.0)));
        assert_eq!(pixels, source);
    }

    #[test]
    fn sub_pixel_amount_renders_as_a_one_tap_window() {
        let mut pixels = frame(4, 3);
        let source = pixels.clone();
        assert!(choker_frame_f32(&mut pixels, 4, 3, &grade(-0.4)));
        assert_eq!(pixels, source);
    }

    #[test]
    fn positive_amount_erodes_a_semi_transparent_edge_to_transparent() {
        let mut pixels = vec![
            [0.1, 0.2, 0.3, 1.0],
            [0.1, 0.2, 0.3, 0.5],
            [0.1, 0.2, 0.3, 0.0],
        ];
        assert!(choker_frame_f32(&mut pixels, 3, 1, &grade(1.0)));
        assert_eq!(alphas(&pixels), vec![0.5, 0.0, 0.0]);
    }

    #[test]
    fn negative_amount_dilates_a_transparent_hole() {
        let mut pixels = vec![
            [0.1, 0.2, 0.3, 0.0],
            [0.1, 0.2, 0.3, 0.0],
            [0.1, 0.2, 0.3, 0.0],
            [0.1, 0.2, 0.3, 1.0],
            [0.1, 0.2, 0.3, 0.0],
        ];
        assert!(choker_frame_f32(&mut pixels, 5, 1, &grade(-1.0)));
        assert_eq!(alphas(&pixels), vec![0.0, 0.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn the_window_is_a_square_not_a_cross() {
        // A 3x3 frame whose only opaque pixel is the centre. A square window of
        // radius 1 must dilate every neighbour, including the diagonals.
        let mut pixels = vec![[0.25, 0.5, 0.75, 0.0]; 9];
        pixels[4][3] = 1.0;
        assert!(choker_frame_f32(&mut pixels, 3, 3, &grade(-1.0)));
        assert_eq!(
            alphas(&pixels),
            vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]
        );

        // Eroding must reach the diagonals as well: every orthogonal neighbour
        // of the centre is opaque, so only the transparent corner can clear it.
        let mut pixels = vec![[0.25, 0.5, 0.75, 1.0]; 9];
        pixels[0][3] = 0.0;
        let mut eroded = pixels.clone();
        assert!(choker_frame_f32(&mut eroded, 3, 3, &grade(1.0)));
        assert_eq!(
            alphas(&eroded),
            vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn frame_edges_clamp_instead_of_shrinking_the_window() {
        let mut pixels = vec![
            [0.1, 0.2, 0.3, 0.2],
            [0.1, 0.2, 0.3, 0.6],
            [0.1, 0.2, 0.3, 0.4],
        ];
        assert!(choker_frame_f32(&mut pixels, 1, 3, &grade(1.0)));
        assert_eq!(alphas(&pixels), vec![0.2, 0.2, 0.4]);
    }

    #[test]
    fn the_alpha_endpoints_are_preserved() {
        let opaque = vec![[0.4, 0.6, 0.8, 1.0]; 12];
        let mut eroded = opaque.clone();
        assert!(choker_frame_f32(&mut eroded, 4, 3, &grade(2.0)));
        assert_eq!(eroded, opaque);

        let transparent = vec![[0.4, 0.6, 0.8, 0.0]; 12];
        let mut dilated = transparent.clone();
        assert!(choker_frame_f32(&mut dilated, 4, 3, &grade(-2.0)));
        assert_eq!(dilated, transparent);
    }

    #[test]
    fn rgb_is_bit_identical_after_the_alpha_change() {
        let mut pixels = frame(5, 4);
        for (index, pixel) in pixels.iter_mut().enumerate() {
            pixel[3] = (index % 3) as f32 * 0.5;
        }
        let source = pixels.clone();
        assert!(choker_frame_f32(&mut pixels, 5, 4, &grade(-2.0)));
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[0].to_bits(), source[index][0].to_bits());
            assert_eq!(pixel[1].to_bits(), source[index][1].to_bits());
            assert_eq!(pixel[2].to_bits(), source[index][2].to_bits());
        }
        assert_ne!(pixels, source);
    }

    #[test]
    fn checkpoint_cadence_is_bounded() {
        let mut pixels = frame(4, 2);
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert_eq!(
            apply_choker_f32_controlled(&mut pixels, 4, 2, &grade(1.0), &mut checkpoint),
            Ok(true)
        );
        // One before the matte, one at the first matte pixel, one after the
        // matte, one after each morphology axis, and one after the composite.
        assert_eq!(calls, 6);
    }

    #[test]
    fn the_checkpoint_error_cancels_the_frame() {
        let mut pixels = frame(4, 2);
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            if calls == 3 {
                return Err("cancelled");
            }
            Ok(())
        };
        assert_eq!(
            apply_choker_f32_controlled(&mut pixels, 4, 2, &grade(1.0), &mut checkpoint),
            Err("cancelled")
        );
    }

    #[test]
    fn the_largest_radius_saturates_the_frame() {
        let mut pixels = frame(8, 8);
        pixels[0][3] = 0.0;
        assert!(choker_frame_f32(
            &mut pixels,
            8,
            8,
            &grade(CHOKER_AUTHOR_MAX_CHOKE_PIXELS)
        ));
        assert_eq!(alphas(&pixels), vec![0.0; 64]);

        let mut pixels = frame(8, 8);
        for pixel in &mut pixels {
            pixel[3] = 0.0;
        }
        pixels[27][3] = 1.0;
        assert!(choker_frame_f32(
            &mut pixels,
            8,
            8,
            &grade(-CHOKER_AUTHOR_MAX_CHOKE_PIXELS)
        ));
        assert_eq!(alphas(&pixels), vec![1.0; 64]);
    }

    #[test]
    fn unaddressable_frames_are_rejected() {
        let mut pixels = frame(2, 2);
        assert!(!choker_frame_f32(&mut pixels, 0, 2, &grade(1.0)));
        assert!(!choker_frame_f32(&mut pixels, 2, 0, &grade(1.0)));
        assert!(!choker_frame_f32(&mut pixels, 3, 2, &grade(1.0)));
        assert!(!choker_frame_f32(&mut pixels, usize::MAX, 2, &grade(1.0)));
    }

    #[test]
    fn hashes_differ_per_authored_amount() {
        use std::hash::{Hash as _, Hasher as _};
        let hash = |grade: &ChokerGrade| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            grade.hash_state(&mut hasher);
            "choker".hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(&grade(2.0)), hash(&grade(2.0)));
        assert_ne!(hash(&grade(2.0)), hash(&grade(-2.0)));
        assert_ne!(hash(&grade(0.0)), hash(&grade(0.5)));
    }
}
