//! Remove colour matting: undo the background a straight-alpha sample was
//! composited against.
//!
//! [`RemoveColorMattingGrade`] implements `builtin.remove_color_matting`, a
//! pixel-local point grade. For a straight-alpha sample `(C, a)` over a matte
//! background `B`, the recovered colour is
//!
//! ```text
//! C' = C - B * (1 - a) / a
//! ```
//!
//! The `(1 - a) / a` factor is computed first and multiplied by `B`, which is
//! the form the fused point shader writes as well. Dividing the whole
//! difference, `(C - B * (1 - a)) / a`, is the classic mistake of treating a
//! straight sample as premultiplied: at `a = 0.5` it subtracts the background
//! twice and turns the soft edge of a matte grey.
//!
//! # Deviations from a generic matte removal
//!
//! * A sample with `a <= 0` is returned unchanged rather than zeroed or filled
//!   with the background. A fully transparent sample carries no colour to
//!   recover, and returning it as-is keeps the stage free of coverage
//!   decisions; `a = NaN` takes the same branch.
//! * `clip` is a single clamp applied to the finished result, never to an
//!   intermediate value. Host packages differ in where they clamp; one
//!   testable rule is worth more here than a per-version reproduction.
//! * There is no colour-space handling: the same arithmetic runs on linear and
//!   on non-linear f32 frames, because the caller decides what the buffer
//!   holds. Un-premultiplying in the wrong space would be worse than not
//!   converting at all.
//!
//! The default configuration is an exact identity — a black background with
//! `clip` on maps every in-range sample to itself bit for bit — so the graph
//! builder skips the node entirely unless something was authored.

/// How many pixels sit between two cooperative checkpoints.
///
/// A matte removal is a handful of flops per pixel, so the stride is a
/// cancellation cadence rather than a throughput knob.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Why a [`RemoveColorMattingGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveColorMattingError {
    /// One of the background colour's components is not finite.
    ///
    /// A non-finite matte poisons every pixel it touches, and no authored value
    /// can justify it, so the grade refuses to exist instead of shipping NaNs
    /// into the frame.
    BackgroundColor,
    /// The clip switch is not a boolean.
    ///
    /// The kernel takes a `bool`, so this is the authoring-side counterpart of
    /// the same rejection: the graph builder turns a non-boolean `clip`
    /// parameter into this variant's message inside an `InvalidAuthorState`.
    Clip,
}

impl std::fmt::Display for RemoveColorMattingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BackgroundColor => write!(
                formatter,
                "remove color matting background color must be finite"
            ),
            Self::Clip => write!(
                formatter,
                "remove color matting clip switch must be a boolean"
            ),
        }
    }
}

impl std::error::Error for RemoveColorMattingError {}

/// The un-matte parameters of one `builtin.remove_color_matting` node.
///
/// The background colour's alpha is authored with the colour widget but takes
/// no part in the formula: a matte background is opaque, so only the RGB
/// channels define it. It is still validated and stored, so the grade
/// round-trips the authored value without a lossy conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RemoveColorMattingGrade {
    background_color: [f32; 4],
    clip: bool,
}

impl RemoveColorMattingGrade {
    /// Builds a grade from a background colour and the clip switch.
    ///
    /// # Errors
    ///
    /// Returns [`RemoveColorMattingError::BackgroundColor`] when any component
    /// of `background_color` — including the alpha the formula ignores — is
    /// not finite.
    pub fn new(background_color: [f32; 4], clip: bool) -> Result<Self, RemoveColorMattingError> {
        if background_color.iter().any(|value| !value.is_finite()) {
            return Err(RemoveColorMattingError::BackgroundColor);
        }
        Ok(Self { background_color, clip })
    }

    /// The authored matte background; only the RGB channels matter.
    #[must_use]
    pub const fn background_color(&self) -> [f32; 4] {
        self.background_color
    }

    /// Whether the result is clamped to `[0, 1]`.
    #[must_use]
    pub const fn clip(&self) -> bool {
        self.clip
    }

    /// Whether this grade leaves every frame untouched.
    ///
    /// A black background already zeroes the matte term, and `clip` is part of
    /// the identity because switching it off is a visible authoring choice —
    /// extended-range samples survive it — even where the black background
    /// makes the two settings numerically equal for in-range pixels.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.clip
            && self.background_color[0] == 0.0
            && self.background_color[1] == 0.0
            && self.background_color[2] == 0.0
    }

    /// Removes the matte from one straight-alpha sample.
    #[must_use]
    pub fn apply_pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        let alpha = pixel[3];
        // Spelled as an explicit NaN test plus `<= 0.0` rather than the negated
        // comparison `!(alpha > 0.0)`; both are the shader's `a > 0.0` guard, so
        // NaN takes the uncovered branch on either side.
        if alpha.is_nan() || alpha <= 0.0 {
            return pixel;
        }
        let matte = (1.0 - alpha) / alpha;
        let mapped = [
            pixel[0] - self.background_color[0] * matte,
            pixel[1] - self.background_color[1] * matte,
            pixel[2] - self.background_color[2] * matte,
            alpha,
        ];
        if self.clip {
            [
                mapped[0].clamp(0.0, 1.0),
                mapped[1].clamp(0.0, 1.0),
                mapped[2].clamp(0.0, 1.0),
                mapped[3],
            ]
        } else {
            mapped
        }
    }

    /// Applies the grade to every sample of a frame.
    pub fn apply_to_frame(&self, frame: &mut [[f32; 4]]) {
        let _ = remove_color_matting_frame_f32(frame, self);
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `header = [kind, clip, 0, 0]` — the branch id is part of the payload so
    /// the CPU and the shader cannot disagree about which kind they are — and
    /// `params = [background.r, background.g, background.b, 0]`. The colour
    /// and the four extra vectors of the shared point uniform stay unused by
    /// the branch, so the renderer writes them as zeros.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [43.0, if self.clip { 1.0 } else { 0.0 }, 0.0, 0.0],
            [
                self.background_color[0],
                self.background_color[1],
                self.background_color[2],
                0.0,
            ],
        )
    }
}

/// Applies a grade to a frame, yielding to `checkpoint` between chunks.
///
/// Returns the "supported" flag the point dispatchers expect; the error type is
/// the caller's cancellation error, so a stop unwinds with its own type.
pub(crate) fn apply_remove_color_matting_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &RemoveColorMattingGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            *pixel = grade.apply_pixel(*pixel);
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Applies a grade to a frame, with no cancellation point.
#[must_use]
pub fn remove_color_matting_frame_f32(
    pixels: &mut [[f32; 4]],
    grade: &RemoveColorMattingGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_remove_color_matting_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    fn close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "expected {expected}, got {actual}"
        );
    }

    fn close_pixel(actual: [f32; 4], expected: [f32; 4]) {
        for channel in 0..4 {
            close(actual[channel], expected[channel]);
        }
    }

    fn bits(pixel: [f32; 4]) -> [u32; 4] {
        pixel.map(f32::to_bits)
    }

    fn gain(background_color: [f32; 4], clip: bool) -> RemoveColorMattingGrade {
        RemoveColorMattingGrade::new(background_color, clip).expect("valid grade")
    }

    /// T1: the default configuration is an exact identity at every alpha.
    #[test]
    fn default_grade_is_a_bit_exact_identity() {
        let grade = gain(BLACK, true);
        assert!(grade.is_identity());
        let frame = [
            [0.0, 0.25, 0.5, 0.0],
            [1.0, 0.75, 0.125, 1.0e-9],
            [0.2, 0.4, 0.6, 0.25],
            [1.0, 1.0, 1.0, 0.5],
            [0.5, 0.5, 0.5, 1.0],
            [0.1, 0.2, 0.3, 2.0],
        ];
        let mut mapped = frame;
        assert!(remove_color_matting_frame_f32(&mut mapped, &grade));
        for (index, pixel) in mapped.iter().enumerate() {
            assert_eq!(bits(*pixel), bits(frame[index]), "pixel {index}");
        }
    }

    /// T2: at full coverage the matte term vanishes whatever the background is.
    #[test]
    fn full_coverage_ignores_the_background_color() {
        let source = [0.2, 0.6, 0.9, 1.0];
        for background in [BLACK, WHITE, [0.5, 0.25, 0.75, 1.0]] {
            for clip in [true, false] {
                let grade = gain(background, clip);
                assert_eq!(
                    bits(grade.apply_pixel(source)),
                    bits(source),
                    "background {background:?} clip {clip}"
                );
            }
        }
    }

    /// T3: half coverage over white subtracts exactly one, then clips to zero.
    #[test]
    fn half_coverage_over_white_clips_to_black() {
        let grade = gain(WHITE, true);
        let source = [0.2, 0.6, 0.9, 0.5];
        // 0.2 - 1 = -0.8, 0.6 - 1 = -0.4, 0.9 - 1 = -0.1, and alpha passes.
        close_pixel(grade.apply_pixel(source), [0.0, 0.0, 0.0, 0.5]);
    }

    /// T4: an uncovered sample is returned untouched instead of being filled.
    #[test]
    fn zero_coverage_returns_the_input_pixel() {
        let grade = gain(WHITE, true);
        for source in [
            [0.0, 0.0, 0.0, 0.0],
            [0.4, 0.5, 0.6, 0.0],
            [1.0, 0.0, 0.5, -1.0],
            [0.7, 0.8, 0.9, 0.0],
        ] {
            assert_eq!(bits(grade.apply_pixel(source)), bits(source));
        }
    }

    /// T5: without the clip the negative the formula defines is kept.
    #[test]
    fn unclipped_grade_keeps_negative_values() {
        let grade = gain(WHITE, false);
        // a = 0.25 -> (1 - 0.25) / 0.25 = 3, so 0.2 - 3 = -2.8.
        let mapped = grade.apply_pixel([0.2, 0.2, 0.2, 0.25]);
        for value in mapped.iter().take(3) {
            close(*value, -2.8);
        }
        close(mapped[3], 0.25);
        assert!(!grade.is_identity());
    }

    /// T6: `clip` is one clamp of the finished result, not a mid-way clamp.
    #[test]
    fn clip_is_a_single_final_clamp() {
        let source = [
            [-0.5, 1.5, 0.3, 0.5],
            [0.2, 0.1, 0.9, 0.25],
            [1.2, -0.2, 0.0, 0.75],
        ];
        let clipped = gain(WHITE, true);
        let unclipped = gain(WHITE, false);
        for pixel in source {
            let crisp = clipped.apply_pixel(pixel);
            let raw = unclipped.apply_pixel(pixel);
            for channel in 0..3 {
                close(crisp[channel], raw[channel].clamp(0.0, 1.0));
            }
            close(crisp[3], pixel[3]);
        }
    }

    /// T7: alpha is never touched, clipped or not.
    #[test]
    fn alpha_passes_through_unchanged() {
        for clip in [true, false] {
            let grade = gain(WHITE, clip);
            for alpha in [0.0, 1.0e-6, 0.25, 0.5, 1.0, 2.0] {
                let mapped = grade.apply_pixel([0.1, 0.2, 0.3, alpha]);
                assert_eq!(bits([mapped[3]; 4]), bits([alpha; 4]), "alpha {alpha}");
            }
        }
    }

    /// T8: identity is "black background and clip on", nothing else.
    #[test]
    fn identity_requires_a_black_background_and_clip() {
        assert!(gain(BLACK, true).is_identity());
        assert!(!gain(BLACK, false).is_identity());
        assert!(!gain(WHITE, true).is_identity());
        assert!(!gain(WHITE, false).is_identity());
        assert!(!gain([0.0, 0.0, 1.0e-7, 1.0], true).is_identity());
    }

    /// T9: non-finite backgrounds are rejected with their own variant.
    #[test]
    fn non_finite_backgrounds_are_rejected() {
        for background in [
            [f32::NAN, 0.0, 0.0, 1.0],
            [0.0, f32::INFINITY, 0.0, 1.0],
            [0.0, 0.0, f32::NEG_INFINITY, 1.0],
            [0.0, 0.0, 0.0, f32::NAN],
        ] {
            assert_eq!(
                RemoveColorMattingGrade::new(background, true),
                Err(RemoveColorMattingError::BackgroundColor)
            );
        }
        assert_eq!(
            RemoveColorMattingGrade::new([0.5, 0.5, 0.5, 1.0], false),
            Ok(gain([0.5, 0.5, 0.5, 1.0], false))
        );
        assert_eq!(
            RemoveColorMattingError::BackgroundColor.to_string(),
            "remove color matting background color must be finite"
        );
        assert_eq!(
            RemoveColorMattingError::Clip.to_string(),
            "remove color matting clip switch must be a boolean"
        );
        assert_ne!(
            RemoveColorMattingError::Clip,
            RemoveColorMattingError::BackgroundColor
        );
    }
}
