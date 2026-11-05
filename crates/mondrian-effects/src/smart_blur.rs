//! Smart blur: a gated separable box blur with three edge modes.
//!
//! The kernel is the whole effect; the render op only carries a validated
//! [`SmartBlurGrade`]. The blur is *selective*: a tap only contributes when its
//! Rec.709 luma is within the authored threshold of the centre pixel's luma, so
//! a flat region is averaged while a strong edge is left alone. That gate is
//! what separates this effect from [`crate::box_blur`]; the weight table, the
//! edge clamp and the fractional radius convention are shared with it.
//!
//! # The filter
//!
//! Four authored controls, evaluated in this exact order:
//!
//! ```text
//! R  = clamp(radius, 0.1, 100)         read radius, in pixels
//! tau = threshold / 100                0.001..1.0 luma difference
//! m  = mode                            1 normal, 2 edge only, 3 overlay edge
//!
//! whole    = floor(R)                  integer taps either side
//! fraction = R - whole                 weight of the two outer taps
//!
//! g(p, q) = 1 if |Y(P(q)) - Y(P(p))| <= tau else 0
//! h(p)    = sum_dx w(dx) g(p, q) P(q) / sum_dx w(dx) g(p, q)      horizontal
//! v(p)    = sum_dy w(dy) g(p, q) h(q) / sum_dy w(dy) g(p, q)      vertical
//! ```
//!
//! `P` is the premultiplied sample `(r a, g a, b a, a)` with `a = clamp(a, 0,
//! 1)` and `Y(P) = 0.2126 P.r + 0.7152 P.g + 0.0722 P.b`. The weight table is
//! `w(d) = 1` for `|d| <= whole`, `w(d) = fraction` for `|d| = whole + 1`, and
//! zero elsewhere, so `sum w = 2R + 1` and the divisor can never reach zero
//! (the centre tap always passes its own gate).
//!
//! The boundary is `<=`, not `<`: a tap whose luma difference is exactly the
//! threshold contributes. `threshold = 100` therefore opens the gate on every
//! frame whose premultiplied luma span is at most one, which is the
//! "ungated box blur" limit the tests pin.
//!
//! # Why the gate reads the source
//!
//! Both passes compare against the *source* luma, never against the horizontal
//! pass output. Gating the vertical pass on `h` would make the reach depend on
//! the previous pass and would no longer be symmetric in the two axes; reading
//! the source keeps the two passes structurally identical, keeps the CPU
//! reference a straight loop, and is what makes the gate a pure function of the
//! authored parameters and the frame.
//!
//! The consequence is that this is a *separable approximation* of a two
//! dimensional selective mean, not the exact two dimensional kernel: flat
//! regions agree, and the neighbourhood of a thin isolated line can differ.
//! That is a deliberate design decision, not a defect.
//!
//! # The three modes
//!
//! - `normal` returns `unpremultiply(v)`: the gated mean, unpremultiplied.
//! - `edge_only` returns `(e, e, e, src.a)` with
//!   `e = clamp(SMART_BLUR_EDGE_GAIN * |Y(P_src) - Y(v)|, 0, 1)`. A flat frame
//!   is therefore black, not unchanged.
//! - `overlay_edge` starts from `unpremultiply(v)` and mixes its rgb toward
//!   white by `e`: `rgb = base.rgb + e (1 - base.rgb)`.
//!
//! `SMART_BLUR_EDGE_GAIN` is a code constant, not a parameter: the effect list
//! declares exactly three parameters, and a fourth would be an invented
//! control. The constant is pinned by a unit test.
//!
//! # Edge handling and the guards
//!
//! Every tap clamps at the frame edge (`repeat_edge`); a window that overhangs
//! repeats the border pixel instead of shrinking or reading zero. A zero-padded
//! window would darken the outer band, which is a frame border, not a blur.
//!
//! Two guards keep the effect from ever writing colour into an uncovered pixel:
//!
//! ```text
//! unpremultiply alpha <= 0            -> (0, 0, 0, 0)
//! mode == edge_only and src.a <= 0    -> (0, 0, 0, 0)
//! ```
//!
//! With those two, an `a == 0` pixel can only ever be written as `(0, 0, 0, 0)`
//! — never as a hidden colour — which is the reason this effect registers **no
//! authoring footprint**.
//!
//! # Why there is no authoring footprint
//!
//! `effects_author_coverage()` is a WGSL helper *inside the point shader*
//! (`crates/mondrian-renderer/src/gpu_compositor.rs`): it lists the point
//! effect kinds for which an `a == 0` input may be returned early, and it is
//! consulted only by that shader's per-pixel early-out. Smart blur cannot
//! appear there, and does not need to:
//!
//! 1. the spatial pass is its own draw with no early-out to rescue — every
//!    destination pixel is written unconditionally;
//! 2. the kernel averages in the premultiplied domain, so a window of fully
//!    transparent samples averages to exactly `(0, 0, 0, 0)`;
//! 3. `edge_only` forces `(0, 0, 0, 0)` whenever `src.a <= 0`, and the other
//!    two modes force it whenever the blurred alpha is non-positive.
//!
//! The criterion is "can an uncovered pixel gain colour?", not "does the effect
//! touch alpha?" — a generator or a `change_empty_pixels` style effect must
//! register, a gated mean of premultiplied samples must not.
//!
//! # GPU packing
//!
//! [`SmartBlurGrade::gpu_controls`] returns the stage-level pair the renderer
//! splices into the shared `SpatialPassUniform`:
//!
//! ```text
//! params = [R, tau, mode.code(), 0]     extra = [0, 0, 0, 0]
//! ```
//!
//! The renderer overwrites `params.w` per draw: `0` selects the horizontal
//! pass, `1` the vertical pass. Both draws share one pipeline and one bind
//! group layout (`original_texture`, `auxiliary_texture`, `uniforms`):
//!
//! ```text
//! A  fs_smart_blur   source      + source      -> scratch[0]   params [R, tau, m, 0]
//! B  fs_smart_blur   source      + scratch[0]  -> scratch[1]   params [R, tau, m, 1]
//! ```
//!
//! Draw A never reads `auxiliary_texture`, but the bind group is fixed at three
//! entries, so the source is bound twice. Draw B both finishes the vertical
//! mean and writes the mode composite, which is why the pass count is two and
//! not three.
//!
//! # Tests
//!
//! The unit tests below cover the documented matrix: the two rejection paths,
//! the flat frame (bit for bit), both sides of the gate boundary, the
//! fractional radius table, the minimum radius, all three modes, a fully
//! transparent neighbourhood, the blurred alpha, the source-gated reference,
//! the edge clamp, the unaddressable-frame guard, determinism, the packed
//! lanes, and the checkpoint cadence of both passes.

/// The largest authored radius in pixels.
pub const SMART_BLUR_AUTHOR_MAX_RADIUS_PIXELS: f32 = 100.0;
/// The smallest authored radius in pixels.
pub const SMART_BLUR_AUTHOR_MIN_RADIUS_PIXELS: f32 = 0.1;
/// The smallest authored threshold, in levels.
pub const SMART_BLUR_MIN_THRESHOLD: f32 = 0.1;
/// The largest authored threshold, in levels.
pub const SMART_BLUR_MAX_THRESHOLD: f32 = 100.0;
/// The documented default radius: a visible but conservative blur.
pub const SMART_BLUR_DEFAULT_RADIUS_PIXELS: f32 = 3.0;
/// The documented default threshold: mid-range selectivity.
pub const SMART_BLUR_DEFAULT_THRESHOLD: f32 = 25.0;
/// How steeply a luma difference turns into an edge value.
///
/// A difference of `1 / SMART_BLUR_EDGE_GAIN` saturates the edge output. The
/// value is a code constant rather than a parameter because the effect list
/// declares exactly three controls.
pub const SMART_BLUR_EDGE_GAIN: f32 = 4.0;

/// The rejection reason of a [`SmartBlurGrade`] field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SmartBlurError {
    /// The radius is outside `[0.1, 100]` or not finite.
    Radius,
    /// The threshold is outside `[0.1, 100]` or not finite.
    Threshold,
}

impl std::fmt::Display for SmartBlurError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Radius => "smart blur radius must be a finite value inside [0.1, 100]",
            Self::Threshold => "smart blur threshold must be a finite value inside [0.1, 100]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SmartBlurError {}

/// What the edge output replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SmartBlurMode {
    /// Return the gated mean with its blurred alpha.
    Normal,
    /// Return the edge magnitude on a black field, keeping the source alpha.
    EdgeOnly,
    /// Return the gated mean with its rgb mixed toward white by the edge.
    OverlayEdge,
}

impl SmartBlurMode {
    /// Every mode, in menu order.
    pub const ALL: [Self; 3] = [Self::Normal, Self::EdgeOnly, Self::OverlayEdge];

    /// The value packed into `params.z`.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Normal => 1,
            Self::EdgeOnly => 2,
            Self::OverlayEdge => 3,
        }
    }

    /// The authoring key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::EdgeOnly => "edge_only",
            Self::OverlayEdge => "overlay_edge",
        }
    }

    /// The short display label, matching the zh-CN catalogue.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "正常",
            Self::EdgeOnly => "仅边缘",
            Self::OverlayEdge => "叠加边缘",
        }
    }

    /// Parses an authoring key; unknown keys are not coerced.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "normal" => Some(Self::Normal),
            "edge_only" => Some(Self::EdgeOnly),
            "overlay_edge" => Some(Self::OverlayEdge),
            _ => None,
        }
    }
}

/// The authored parameters of one smart blur operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmartBlurGrade {
    radius: f32,
    threshold: f32,
    mode: SmartBlurMode,
}

impl SmartBlurGrade {
    /// Builds a grade from the three authored controls.
    ///
    /// `radius` is in pixels in `[0.1, 100]` and `threshold` is in levels in
    /// `[0.1, 100]`.
    ///
    /// # Errors
    ///
    /// Returns the first field that is not finite or outside its documented
    /// range, in the fixed order `radius`, `threshold`. The values are never
    /// clamped: silently rewriting an authored `300` to `100` would hide the
    /// authoring bug.
    pub fn new(radius: f32, threshold: f32, mode: SmartBlurMode) -> Result<Self, SmartBlurError> {
        if !radius.is_finite()
            || !(SMART_BLUR_AUTHOR_MIN_RADIUS_PIXELS..=SMART_BLUR_AUTHOR_MAX_RADIUS_PIXELS)
                .contains(&radius)
        {
            return Err(SmartBlurError::Radius);
        }
        if !threshold.is_finite()
            || !(SMART_BLUR_MIN_THRESHOLD..=SMART_BLUR_MAX_THRESHOLD).contains(&threshold)
        {
            return Err(SmartBlurError::Threshold);
        }
        Ok(Self { radius, threshold, mode })
    }

    /// The documented default grade: a three pixel blur at a threshold of 25.
    #[must_use]
    pub const fn documented_defaults() -> Self {
        Self {
            radius: SMART_BLUR_DEFAULT_RADIUS_PIXELS,
            threshold: SMART_BLUR_DEFAULT_THRESHOLD,
            mode: SmartBlurMode::Normal,
        }
    }

    /// The authored radius in pixels.
    #[must_use]
    pub const fn radius(&self) -> f32 {
        self.radius
    }

    /// The authored threshold in levels.
    #[must_use]
    pub const fn threshold(&self) -> f32 {
        self.threshold
    }

    /// The authored mode.
    #[must_use]
    pub const fn mode(&self) -> SmartBlurMode {
        self.mode
    }

    /// The gate tolerance, `threshold / 100`.
    ///
    /// The authored range `[0.1, 100]` therefore maps to `[0.001, 1.0]`, which
    /// is the normalised premultiplied luma difference the gate compares.
    #[must_use]
    pub fn threshold_fraction(&self) -> f32 {
        self.threshold / 100.0
    }

    /// The stage-level uniform payload: `params` and `extra`.
    ///
    /// `params` is `[radius, threshold / 100, mode.code(), 0]`; the renderer
    /// overwrites the last lane with the axis selector of each draw. `extra` is
    /// reserved and always zero.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.radius(),
                self.threshold_fraction(),
                self.mode.code() as f32,
                0.0,
            ],
            [0.0; 4],
        )
    }

    /// The relative cost the estimator quotes: two passes and one composite
    /// folded into the second.
    #[must_use]
    pub const fn cost(&self) -> u32 {
        3
    }

    /// Hashes every authored scalar so a cache key cannot alias two grades.
    ///
    /// The three raw bit patterns are hashed, not the packed uniform: the
    /// packed lanes would alias `(radius 3, threshold 25)` and
    /// `(radius 3, threshold 25)` in every other arrangement of the same four
    /// floats, which are different frames once the mode changes.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.radius.to_bits().hash(state);
        self.threshold.to_bits().hash(state);
        self.mode.code().hash(state);
    }
}

const CHECKPOINT_STRIDE: usize = 4096;

/// Applies the smart blur to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn smart_blur_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &SmartBlurGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_smart_blur_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed kernel behind [`smart_blur_frame_f32`].
///
/// Two full-frame planes are live at once: the premultiplied source snapshot
/// (the gate reference and the coverage both non-blurred modes report) and the
/// horizontal pass output the vertical pass averages. That is why
/// `render_op_f32_scratch_frames` reports two frames, not one.
pub(crate) fn apply_smart_blur_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &SmartBlurGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    let frame = &mut working[..pixel_count];
    checkpoint()?;

    // The authored range is already enforced by `SmartBlurGrade::new`; clamping
    // here mirrors the shader, which clamps the packed lane instead of trusting
    // it, and keeps a directly constructed op from running an unbounded loop.
    let radius = grade.radius().clamp(
        SMART_BLUR_AUTHOR_MIN_RADIUS_PIXELS,
        SMART_BLUR_AUTHOR_MAX_RADIUS_PIXELS,
    );
    let whole = radius.floor() as usize;
    let fraction = radius - radius.floor();
    let threshold = grade.threshold_fraction();
    let mode = grade.mode();

    // Stage 1: the premultiplied source snapshot. Both passes gate against it,
    // and the mode composite reads its coverage, so it has to survive until the
    // very last write.
    let mut source = Vec::with_capacity(pixel_count);
    for (index, pixel) in frame.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        source.push(premultiply(*pixel));
    }
    checkpoint()?;

    // Stage 2: the horizontal gated mean. Both outer taps are always visited,
    // even when `fraction` is zero: the shader has no branch there either, and
    // a CPU that skipped the zero-weight taps would add a different chain.
    let mut horizontal = vec![[0.0_f32; 4]; pixel_count];
    let rows = pixel_count / width;
    crate::parallel::for_each_row_band(&mut horizontal, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, destination) in chunk.iter_mut().enumerate() {
                let centre = source[row * width + column];
                let centre_luma = luma(centre);
                let mut sum = [0.0_f32; 4];
                let mut weight_sum = 0.0_f32;
                for offset in 0..=(2 * whole + 2) {
                    let delta = offset as isize - (whole as isize + 1);
                    let sample_column =
                        (column as isize + delta).clamp(0, width as isize - 1) as usize;
                    let weight = axis_weight(delta.unsigned_abs(), whole, fraction);
                    let sample = source[row * width + sample_column];
                    let combined = weight * gate(luma(sample), centre_luma, threshold);
                    sum[0] += combined * sample[0];
                    sum[1] += combined * sample[1];
                    sum[2] += combined * sample[2];
                    sum[3] += combined * sample[3];
                    weight_sum += combined;
                }
                *destination = [
                    sum[0] / weight_sum,
                    sum[1] / weight_sum,
                    sum[2] / weight_sum,
                    sum[3] / weight_sum,
                ];
            }
        }
    });
    checkpoint()?;

    // Stage 3: the vertical gated mean and the mode composite. The gate still
    // reads the source, the average reads the horizontal plane, and the
    // destination is the frame itself — safe in place because a pixel only ever
    // reads its own alpha here.
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let index = row * width + column;
                let centre_luma = luma(source[index]);
                let source_alpha = pixel[3];
                let mut sum = [0.0_f32; 4];
                let mut weight_sum = 0.0_f32;
                for offset in 0..=(2 * whole + 2) {
                    let delta = offset as isize - (whole as isize + 1);
                    let sample_row = (row as isize + delta).clamp(0, height as isize - 1) as usize;
                    let weight = axis_weight(delta.unsigned_abs(), whole, fraction);
                    let sample = source[sample_row * width + column];
                    let combined = weight * gate(luma(sample), centre_luma, threshold);
                    let averaged = horizontal[sample_row * width + column];
                    sum[0] += combined * averaged[0];
                    sum[1] += combined * averaged[1];
                    sum[2] += combined * averaged[2];
                    sum[3] += combined * averaged[3];
                    weight_sum += combined;
                }
                let blurred = [
                    sum[0] / weight_sum,
                    sum[1] / weight_sum,
                    sum[2] / weight_sum,
                    sum[3] / weight_sum,
                ];
                *pixel = compose(mode, source_alpha, centre_luma, blurred);
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

/// Rec.709 luma of a premultiplied sample.
fn luma(pixel: [f32; 4]) -> f32 {
    0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2]
}

/// The fractional box weight of one tap, shared by both axes.
fn axis_weight(distance: usize, whole: usize, fraction: f32) -> f32 {
    if distance <= whole {
        1.0
    } else if distance == whole + 1 {
        fraction
    } else {
        0.0
    }
}

/// The gate: `1` when the luma difference is at or below the threshold.
fn gate(sample_luma: f32, centre_luma: f32, threshold: f32) -> f32 {
    if (sample_luma - centre_luma).abs() <= threshold {
        1.0
    } else {
        0.0
    }
}

/// The edge magnitude of one pixel, saturated by [`SMART_BLUR_EDGE_GAIN`].
fn edge_strength(source_luma: f32, blurred: [f32; 4]) -> f32 {
    (SMART_BLUR_EDGE_GAIN * (source_luma - luma(blurred)).abs()).clamp(0.0, 1.0)
}

/// The mode composite of one pixel.
fn compose(
    mode: SmartBlurMode,
    source_alpha: f32,
    source_luma: f32,
    blurred: [f32; 4],
) -> [f32; 4] {
    if mode == SmartBlurMode::EdgeOnly {
        if source_alpha <= 0.0 {
            return [0.0; 4];
        }
        let edge = edge_strength(source_luma, blurred);
        return [edge, edge, edge, source_alpha];
    }
    let alpha = blurred[3].clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let red = blurred[0] / alpha;
    let green = blurred[1] / alpha;
    let blue = blurred[2] / alpha;
    if mode == SmartBlurMode::OverlayEdge {
        let edge = edge_strength(source_luma, blurred);
        return [
            red + edge * (1.0 - red),
            green + edge * (1.0 - green),
            blue + edge * (1.0 - blue),
            alpha,
        ];
    }
    [red, green, blue, alpha]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(radius: f32, threshold: f32, mode: SmartBlurMode) -> SmartBlurGrade {
        SmartBlurGrade::new(radius, threshold, mode)
            .expect("the authored grade is inside the documented range")
    }

    fn gray(value: f32) -> [f32; 4] {
        [value, value, value, 1.0]
    }

    fn run(pixels: &mut [[f32; 4]], width: usize, height: usize, grade: &SmartBlurGrade) {
        assert!(
            smart_blur_frame_f32(pixels, width, height, grade),
            "the frame must be addressable"
        );
    }

    fn assert_close(actual: f32, expected: f32, label: &str) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "{label}: expected {expected}, got {actual}"
        );
    }

    fn assert_pixel_close(actual: [f32; 4], expected: [f32; 4], label: &str) {
        for (channel, (actual, expected)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(
                (actual - expected).abs() <= 1.0e-6,
                "{label} channel {channel}: expected {expected}, got {actual}"
            );
        }
    }

    /// K1: the authored range is enforced on the radius.
    #[test]
    fn rejects_radius_outside_the_authored_range() {
        assert_eq!(
            SmartBlurGrade::new(0.05, 25.0, SmartBlurMode::Normal),
            Err(SmartBlurError::Radius)
        );
        assert_eq!(
            SmartBlurGrade::new(100.1, 25.0, SmartBlurMode::Normal),
            Err(SmartBlurError::Radius)
        );
        assert!(SmartBlurGrade::new(0.1, 0.1, SmartBlurMode::Normal).is_ok());
        assert!(SmartBlurGrade::new(100.0, 100.0, SmartBlurMode::Normal).is_ok());
    }

    /// K2: non-finite controls are rejected rather than clamped.
    #[test]
    fn rejects_non_finite_parameters() {
        assert_eq!(
            SmartBlurGrade::new(f32::NAN, 25.0, SmartBlurMode::Normal),
            Err(SmartBlurError::Radius)
        );
        assert_eq!(
            SmartBlurGrade::new(3.0, f32::INFINITY, SmartBlurMode::Normal),
            Err(SmartBlurError::Threshold)
        );
        assert_eq!(
            SmartBlurGrade::new(3.0, f32::NEG_INFINITY, SmartBlurMode::Normal),
            Err(SmartBlurError::Threshold)
        );
    }

    /// K3: a constant frame survives to within one ulp of the weighted mean.
    ///
    /// The gated mean divides the weighted sum by the sum of the same weights,
    /// so a flat frame is mathematically the input again, but a fractional
    /// radius rounds each tap and the division does not cancel that exactly.
    #[test]
    fn constant_frame_is_unchanged() {
        let mut pixels = vec![[0.4_f32, 0.2, 0.7, 1.0]; 20];
        let expected = pixels.clone();
        run(&mut pixels, 5, 4, &grade(2.5, 25.0, SmartBlurMode::Normal));
        for (actual, wanted) in pixels.iter().zip(expected.iter()) {
            assert_pixel_close(*actual, *wanted, "flat frame");
        }
    }

    /// K4: a tap beyond the threshold contributes nothing.
    #[test]
    fn gate_excludes_a_far_sample() {
        let mut pixels = vec![gray(0.0), gray(1.0), gray(0.0)];
        run(&mut pixels, 3, 1, &grade(1.0, 50.0, SmartBlurMode::Normal));
        assert_pixel_close(pixels[1], [1.0, 1.0, 1.0, 1.0], "centre");
    }

    /// K5: the gate boundary is inclusive, so a difference of exactly the
    /// threshold passes.
    ///
    /// All three taps pass at `threshold = 100`, so the centre becomes the mean
    /// of one bright and two black premultiplied samples: a third of the colour
    /// over an unchanged alpha.
    #[test]
    fn gate_is_inclusive_at_the_threshold() {
        let mut pixels = vec![gray(0.0), gray(1.0), gray(0.0)];
        run(&mut pixels, 3, 1, &grade(1.0, 100.0, SmartBlurMode::Normal));
        assert_pixel_close(pixels[1], [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 1.0], "centre");
    }

    /// K6: `R = 1.5` weights the two outer taps by `0.5`, divisor four.
    #[test]
    fn fractional_radius_weights_the_outer_taps() {
        let mut pixels = vec![gray(0.0), gray(1.0), gray(0.0)];
        run(&mut pixels, 3, 1, &grade(1.5, 100.0, SmartBlurMode::Normal));
        assert_close(pixels[1][0], 0.25, "centre");
        assert_close(pixels[1][3], 1.0, "centre alpha");
    }

    /// K7: the smallest authored radius is not the identity.
    #[test]
    fn minimum_radius_is_not_identity() {
        let mut pixels = vec![gray(0.0), gray(1.0), gray(0.0)];
        run(&mut pixels, 3, 1, &grade(0.1, 100.0, SmartBlurMode::Normal));
        assert_close(pixels[1][0], 1.0 / 1.2, "centre");
    }

    /// K8: `edge_only` writes the saturated absolute difference.
    #[test]
    fn edge_only_writes_the_absolute_difference() {
        let mut pixels = vec![gray(0.3), gray(0.5), gray(0.3)];
        run(
            &mut pixels,
            3,
            1,
            &grade(1.0, 100.0, SmartBlurMode::EdgeOnly),
        );
        assert_pixel_close(
            pixels[1],
            [0.533_333_3, 0.533_333_3, 0.533_333_3, 1.0],
            "centre",
        );
    }

    /// K9: `overlay_edge` mixes the blurred rgb toward white by the edge.
    #[test]
    fn overlay_edge_mixes_toward_white() {
        let mut pixels = vec![gray(0.3), gray(0.5), gray(0.3)];
        run(
            &mut pixels,
            3,
            1,
            &grade(1.0, 100.0, SmartBlurMode::OverlayEdge),
        );
        assert_close(pixels[1][0], 0.704_444_4, "centre");
        assert_close(pixels[1][3], 1.0, "centre alpha");
    }

    /// K10: a fully transparent neighbourhood is cleared in all three modes.
    #[test]
    fn transparent_neighbourhood_is_cleared() {
        for mode in SmartBlurMode::ALL {
            let mut pixels = vec![[1.0_f32, 0.0, 0.0, 0.0]; 9];
            run(&mut pixels, 3, 3, &grade(1.0, 25.0, mode));
            for (index, pixel) in pixels.iter().enumerate() {
                assert_eq!(
                    *pixel, [0.0_f32; 4],
                    "{mode:?} pixel {index} must be cleared"
                );
            }
        }
    }

    /// K11: `normal` blurs the coverage too, and unpremultiplies the result.
    #[test]
    fn normal_mode_blurs_alpha() {
        let mut pixels = vec![
            [0.0_f32, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 0.0],
        ];
        run(&mut pixels, 3, 1, &grade(1.0, 100.0, SmartBlurMode::Normal));
        assert_pixel_close(pixels[1], [1.0, 1.0, 1.0, 1.0 / 3.0], "centre");
    }

    /// K12: `edge_only` reports the source coverage, not the blurred one.
    #[test]
    fn edge_only_keeps_the_source_alpha() {
        let mut pixels = vec![
            [0.3_f32, 0.3, 0.3, 0.5],
            [0.5, 0.5, 0.5, 0.5],
            [0.3, 0.3, 0.3, 0.5],
        ];
        run(
            &mut pixels,
            3,
            1,
            &grade(1.0, 100.0, SmartBlurMode::EdgeOnly),
        );
        assert_close(pixels[1][3], 0.5, "centre alpha");
    }

    /// K13: the vertical gate compares against the source, never against the
    /// horizontal pass output.
    #[test]
    fn gate_reference_is_the_source_not_the_first_pass() {
        let mut pixels = vec![
            gray(0.9),
            gray(0.0),
            gray(0.9),
            gray(0.5),
            gray(0.5),
            gray(0.5),
            gray(0.5),
            gray(0.5),
            gray(0.5),
        ];
        run(&mut pixels, 3, 3, &grade(1.0, 20.0, SmartBlurMode::Normal));
        // The one source difference of 0.5 is above the 0.2 threshold, so the
        // top row is excluded; gating on the first pass instead would have let
        // it through on the strength of its own averaged value.
        assert_close(pixels[4][0], 0.5, "centre");
    }

    /// K14: an overhanging window repeats the border pixel instead of reading
    /// zero.
    #[test]
    fn clamps_instead_of_zero_padding() {
        let mut pixels = vec![gray(0.0), gray(1.0), gray(0.0)];
        run(&mut pixels, 3, 1, &grade(2.0, 100.0, SmartBlurMode::Normal));
        assert_close(pixels[1][0], 0.2, "centre");
    }

    /// K15: unaddressable frames are rejected, not panicked on.
    #[test]
    fn rejects_dimension_mismatch() {
        let mut short = vec![gray(1.0); 3];
        assert!(!smart_blur_frame_f32(
            &mut short,
            3,
            2,
            &grade(1.0, 25.0, SmartBlurMode::Normal)
        ));
        let mut pixels = vec![gray(1.0); 3];
        assert!(!smart_blur_frame_f32(
            &mut pixels,
            0,
            1,
            &grade(1.0, 25.0, SmartBlurMode::Normal)
        ));
        assert!(!smart_blur_frame_f32(
            &mut pixels,
            1,
            0,
            &grade(1.0, 25.0, SmartBlurMode::Normal)
        ));
    }

    /// K16: the same frame and grade always produce the same bits.
    #[test]
    fn is_deterministic() {
        let fixture = vec![
            [0.1_f32, 0.2, 0.3, 1.0],
            [0.9, 0.8, 0.7, 0.5],
            [0.4, 0.4, 0.4, 0.25],
            [0.0, 1.0, 0.0, 1.0],
        ];
        let mut first = fixture.clone();
        let mut second = fixture;
        let grade = grade(1.5, 25.0, SmartBlurMode::OverlayEdge);
        run(&mut first, 2, 2, &grade);
        run(&mut second, 2, 2, &grade);
        assert_eq!(first, second, "two runs must agree bit for bit");
    }

    /// K17: the packed lanes are exactly the documented ones.
    #[test]
    fn gpu_controls_are_the_documented_lanes() {
        let controls = grade(3.0, 25.0, SmartBlurMode::OverlayEdge).gpu_controls();
        assert_eq!(controls, ([3.0, 0.25, 3.0, 0.0], [0.0; 4]));
    }

    /// K18: both passes offer a checkpoint.
    #[test]
    fn checkpoint_runs_in_both_passes() {
        #[derive(Debug)]
        struct TestCheckpointError;

        let mut pixels = vec![gray(0.5); 64 * 64];
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), TestCheckpointError>(())
        };
        let applied = apply_smart_blur_f32_controlled(
            &mut pixels,
            64,
            64,
            &grade(2.0, 25.0, SmartBlurMode::Normal),
            &mut checkpoint,
        )
        .expect("the checkpoint never fails");
        assert!(applied);
        assert!(calls >= 2, "both passes must checkpoint, got {calls}");
    }
}
