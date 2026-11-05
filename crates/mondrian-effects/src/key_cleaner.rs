//! Key cleaner: repairs the edge of a keyed matte on the alpha channel alone.
//!
//! The kernel is the whole effect; the render op only carries a validated
//! [`KeyCleanerGrade`]. The colour channels are never read and never written:
//! the output pixel is `[r, g, b, cleaned(alpha)]`, so a straight-alpha RGB
//! survives bit for bit and the later compositor premultiply still counts the
//! coverage exactly once.
//!
//! # The filter
//!
//! Four authored controls, evaluated in this exact order:
//!
//! ```text
//! R = round(其他边缘半径)             0..250 erosion radius, taps
//! F = round(减少震颤 × R)             0..R softening radius, taps
//! K = Alpha 对比度 / 100              0..1 contrast weight
//! S = 强度 / 100                      0..1 strength weight
//!
//! m = min over the (2R+1)² square of a0        squared-min erosion
//! b = mean over the (2F+1)² square of m        integer box average
//! c = mix(b, smoothstep(0.25, 0.75, b), K)     alpha-contrast S-curve
//! a' = mix(a0, c, S)                           strength blend
//! ```
//!
//! `mix(x, y, t)` is written `x * (1 - t) + y * t`, the same expression WGSL's
//! `mix` expands to, so the endpoints `t = 0` and `t = 1` are bit-exact.
//!
//! The structuring element is a **square**, not a circle: a radius `R` window
//! spans `2R + 1` pixels on both axes and therefore reaches the Chebyshev
//! distance, which is what the effect's copy ("the transparent area grows")
//! describes. The radius is the *erosion* radius: a larger value makes the
//! matte thinner, never thicker.
//!
//! Erosion runs **before** softening on purpose. Softening the raw alpha and
//! eroding afterwards would let the blur pull coverage back into the pixels the
//! erosion just removed, so the two orders are not interchangeable.
//!
//! # Edge handling
//!
//! Both axes clamp at the frame edge (`repeat_edge`): a window that overhangs
//! the frame repeats the border pixel instead of shrinking or reading zero. A
//! zero-padded window would erode the whole outer band to transparent, which is
//! a visible frame border, not an edge repair.
//!
//! # The identity and coverage invariants
//!
//! - `S == 0` leaves every byte of the frame untouched, because the strength
//!   blend is the last step.
//! - `R == 0`, `F == 0` and `K == 0` together leave every byte untouched, and
//!   the graph builder elides the op.
//! - `R == 0` implies `F == 0`, because `F = round(减少震颤 × R)`.
//! - `a0 == 0` implies `a' == 0` bit for bit. The erosion of a transparent
//!   pixel is zero, and because `F <= R` every pixel in the softening window is
//!   also zero, so the box average and the contrast curve stay zero. The effect
//!   can therefore never invent coverage out of nothing, which is why it does
//!   **not** register an authoring footprint (`effects_author_coverage`).
//!
//! # GPU packing
//!
//! [`KeyCleanerGrade::gpu_controls`] returns the stage-level pair the renderer
//! splices into the `SpatialPassUniform` cells of four records:
//!
//! ```text
//! A  fs_keylight_erode   source      -> scratch[0]   params [R, 0, 0, 1]
//! B  fs_keylight_erode   scratch[0]  -> scratch[1]   params [R, 1, 0, 0]
//! C  fs_box_blur         scratch[1]  -> scratch[2]   params [0, 0, F, 0]
//! D  fs_key_cleaner_composite  source + scratch[2 or 1] -> scratch[0]
//!                                                    params [F, K, S, 0]
//! ```
//!
//! Pass C is skipped when `F == 0`; `D` then reads the vertical erosion plane
//! directly. The stage-level payload is
//! `([radius_pixels, 0, 0, 0], [chatter_pixels, K, S, 0])`, and every integer is
//! rounded here, once, from the same authored value the CPU uses — the renderer
//! never re-derives a radius from a float.
//!
//! Both morphology passes run even when `R == 0`: they collapse to one tap, but
//! they keep the shader's `select(1.0, 0.0, dilate)` clamp, which the CPU
//! mirrors by starting its erosion at `1.0`. Skipping them on the CPU would let
//! an out-of-range authored alpha above `1.0` survive on one side only.
//!
//! # Tests
//!
//! The unit tests below cover the documented matrix: the defaults, the four
//! rejection paths and their messages, half-away-from-zero rounding, the
//! radius/chatter coupling, both identity paths, the contrast curve, the square
//! window, the chatter softening, the separable decomposition against a direct
//! two-dimensional reference, the strength blend, bit-identical RGB and
//! transparent pixels, the frame-edge clamp, the border replication, the
//! unaddressable-frame guard, and the checkpoint cadence.

/// Largest authored edge radius in pixels.
pub const KEY_CLEANER_AUTHOR_MAX_EDGE_RADIUS_PIXELS: f32 = 250.0;
/// Documented default edge radius: a visible but conservative cleanup.
pub const KEY_CLEANER_DEFAULT_EDGE_RADIUS_PIXELS: f32 = 10.0;
/// Documented default chatter fraction: no softening.
pub const KEY_CLEANER_DEFAULT_REDUCE_CHATTER: f32 = 0.0;
/// Documented default alpha contrast: no S-curve.
pub const KEY_CLEANER_DEFAULT_ALPHA_CONTRAST: f32 = 0.0;
/// Documented default strength: the cleanup is fully applied.
pub const KEY_CLEANER_DEFAULT_STRENGTH: f32 = 100.0;

/// The rejection reason of a [`KeyCleanerGrade`] field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCleanerError {
    /// The edge radius is outside `[0, 250]` or not finite.
    EdgeRadius,
    /// The chatter fraction is outside `[0, 1]` or not finite.
    ReduceChatter,
    /// The alpha contrast is outside `[0, 100]` or not finite.
    AlphaContrast,
    /// The strength is outside `[0, 100]` or not finite.
    Strength,
}

impl std::fmt::Display for KeyCleanerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::EdgeRadius => "key cleaner edge radius must be a finite value inside [0, 250]",
            Self::ReduceChatter => {
                "key cleaner reduce chatter must be a finite value inside [0, 1]"
            }
            Self::AlphaContrast => {
                "key cleaner alpha contrast must be a finite value inside [0, 100]"
            }
            Self::Strength => "key cleaner strength must be a finite value inside [0, 100]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for KeyCleanerError {}

/// The authored parameters of one key cleaner operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyCleanerGrade {
    edge_radius: f32,
    reduce_chatter: f32,
    alpha_contrast: f32,
    strength: f32,
}

impl KeyCleanerGrade {
    /// Builds a grade from the four authored controls.
    ///
    /// `edge_radius` is in pixels, `reduce_chatter` is the `0..1` fraction of
    /// the radius used as the softening radius, and the two remaining controls
    /// are percentages.
    ///
    /// # Errors
    ///
    /// Returns the first field that is not finite or outside its documented
    /// range, in the fixed order `edge_radius`, `reduce_chatter`,
    /// `alpha_contrast`, `strength`. The values are never clamped: silently
    /// rewriting an authored `300` to `250` would hide the authoring bug.
    pub fn new(
        edge_radius: f32,
        reduce_chatter: f32,
        alpha_contrast: f32,
        strength: f32,
    ) -> Result<Self, KeyCleanerError> {
        if !edge_radius.is_finite()
            || !(0.0..=KEY_CLEANER_AUTHOR_MAX_EDGE_RADIUS_PIXELS).contains(&edge_radius)
        {
            return Err(KeyCleanerError::EdgeRadius);
        }
        if !reduce_chatter.is_finite() || !(0.0..=1.0).contains(&reduce_chatter) {
            return Err(KeyCleanerError::ReduceChatter);
        }
        if !alpha_contrast.is_finite() || !(0.0..=100.0).contains(&alpha_contrast) {
            return Err(KeyCleanerError::AlphaContrast);
        }
        if !strength.is_finite() || !(0.0..=100.0).contains(&strength) {
            return Err(KeyCleanerError::Strength);
        }
        Ok(Self {
            edge_radius,
            reduce_chatter,
            alpha_contrast,
            strength,
        })
    }

    /// The documented default grade: `(10, 0, 0, 100)`.
    ///
    /// The default is **not** the identity — it erodes the matte by ten pixels
    /// — so the graph builder keeps the node instead of eliding it.
    #[must_use]
    pub const fn documented_defaults() -> Self {
        Self {
            edge_radius: KEY_CLEANER_DEFAULT_EDGE_RADIUS_PIXELS,
            reduce_chatter: KEY_CLEANER_DEFAULT_REDUCE_CHATTER,
            alpha_contrast: KEY_CLEANER_DEFAULT_ALPHA_CONTRAST,
            strength: KEY_CLEANER_DEFAULT_STRENGTH,
        }
    }

    /// The authored edge radius in pixels.
    #[must_use]
    pub const fn edge_radius(&self) -> f32 {
        self.edge_radius
    }

    /// The authored chatter fraction of the edge radius.
    #[must_use]
    pub const fn reduce_chatter(&self) -> f32 {
        self.reduce_chatter
    }

    /// The authored alpha contrast percentage.
    #[must_use]
    pub const fn alpha_contrast(&self) -> f32 {
        self.alpha_contrast
    }

    /// The authored strength percentage.
    #[must_use]
    pub const fn strength(&self) -> f32 {
        self.strength
    }

    /// The integer erosion radius per axis, rounded half away from zero.
    ///
    /// The filter cannot read a fractional tap, so the radius is rounded here
    /// and in the shader from the same packed value.
    #[must_use]
    pub fn radius_pixels(&self) -> usize {
        self.edge_radius.round() as usize
    }

    /// The integer softening radius per axis, `round(减少震颤 × R)`.
    ///
    /// Coupling the softening radius to the erosion radius is what makes
    /// `F <= R` hold, and `F <= R` is what makes a transparent pixel stay
    /// transparent through the box average.
    #[must_use]
    pub fn chatter_pixels(&self) -> usize {
        (self.reduce_chatter * self.radius_pixels() as f32).round() as usize
    }

    /// Whether the grade is a no-op and the graph builder may elide it.
    ///
    /// Two paths are the identity: zero strength, and zero radius with zero
    /// chatter and zero contrast. A zero radius with a non-zero contrast is
    /// **not** the identity: the contrast curve still re-steepens the existing
    /// edge, and the erosion passes collapse to a one-tap clamp.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.strength <= 0.0
            || (self.radius_pixels() == 0
                && self.chatter_pixels() == 0
                && self.alpha_contrast <= 0.0)
    }

    /// The stage-level uniform payload: `params` and `extra`.
    ///
    /// `params` is `[radius_pixels, 0, 0, 0]` and `extra` is
    /// `[chatter_pixels, K, S, 0]`. The renderer splices the erosion radius
    /// into the `[radius, axis, dilate, read_frame]` cell of the two morphology
    /// passes, the softening radius into `[0, 0, F, 0]`, and the remaining two
    /// weights into the composite's `[F, K, S, 0]` cell.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [self.radius_pixels() as f32, 0.0, 0.0, 0.0],
            [
                self.chatter_pixels() as f32,
                self.alpha_contrast / 100.0,
                self.strength / 100.0,
                0.0,
            ],
        )
    }

    /// The relative cost the estimator quotes: three passes without the
    /// optional softening, four with it.
    #[must_use]
    pub fn cost(&self) -> u32 {
        if self.chatter_pixels() > 0 {
            4
        } else {
            3
        }
    }

    /// Hashes every authored scalar so a cache key cannot alias two grades.
    ///
    /// The four raw bit patterns are hashed, not the packed uniform: the packed
    /// lanes would alias `(radius 10, contrast 0)` and `(radius 0, contrast
    /// 10/100)`, which are different frames.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.edge_radius.to_bits().hash(state);
        self.reduce_chatter.to_bits().hash(state);
        self.alpha_contrast.to_bits().hash(state);
        self.strength.to_bits().hash(state);
    }
}

const CHECKPOINT_STRIDE: usize = 4096;

/// Applies the key cleaner to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn key_cleaner_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &KeyCleanerGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_key_cleaner_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed kernel behind [`key_cleaner_frame_f32`].
pub(crate) fn apply_key_cleaner_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &KeyCleanerGrade,
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

    let radius = grade.radius_pixels();
    let blur = grade.chatter_pixels();

    // Stage 1: copy the frame alpha into a matte plane. The colour is never
    // read again, so a transparent pixel keeps its RGB even when the matte
    // around it changes.
    let mut alpha = Vec::with_capacity(pixel_count);
    for (index, pixel) in frame.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        alpha.push(pixel[3]);
    }
    checkpoint()?;

    // Stage 2: the separable squared-min erosion, horizontal into `plane_b`
    // then vertical back into `alpha`. Both passes run even at radius zero: the
    // one-tap window then only mirrors the shader's clamp to `<= 1`, which is
    // what keeps an out-of-range authored alpha identical on both paths.
    let mut plane_b = vec![0.0_f32; pixel_count];
    morph_horizontal(&alpha, &mut plane_b, width, height, radius);
    checkpoint()?;
    morph_vertical(&plane_b, &mut alpha, width, height, radius);
    checkpoint()?;

    // Stage 3: optional horizontal softening. The vertical half runs inside the
    // composite below, next to the source alpha it blends with.
    if blur > 0 {
        box_horizontal(&alpha, &mut plane_b, width, height, blur);
        checkpoint()?;
        std::mem::swap(&mut alpha, &mut plane_b);
    }

    // Stage 4: vertical softening, contrast curve, strength blend. Only the
    // alpha channel is written, so the RGB of every pixel stays bit-identical.
    let rows = pixel_count / width;
    let contrast = grade.alpha_contrast() / 100.0;
    let strength = grade.strength() / 100.0;
    let divisor = (2 * blur + 1) as f32;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let mut sum = 0.0_f32;
                for offset in 0..=(2 * blur) {
                    let sample = row as isize + offset as isize - blur as isize;
                    let clamped = sample.clamp(0, height as isize - 1) as usize;
                    sum += alpha[clamped * width + column];
                }
                let softened = sum / divisor;
                let t = ((softened - 0.25) / 0.5).clamp(0.0, 1.0);
                let sharpened = t * t * (3.0 - 2.0 * t);
                let contrasted = softened * (1.0 - contrast) + sharpened * contrast;
                pixel[3] = pixel[3] * (1.0 - strength) + contrasted * strength;
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

/// Runs one horizontal squared-min erosion pass over the alpha plane.
///
/// The window clamps at the frame edge, so a radius larger than the frame width
/// reads the border column repeatedly instead of running out of taps. The
/// accumulator starts at `1.0`, exactly like the shader's
/// `select(1.0, 0.0, dilate)` for the minimum direction.
fn morph_horizontal(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
) {
    for row in 0..height {
        for column in 0..width {
            let mut value = 1.0_f32;
            for offset in 0..=(2 * radius) {
                let sample = column as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, width as isize - 1) as usize;
                value = value.min(source[row * width + clamped]);
            }
            destination[row * width + column] = value;
        }
    }
}

/// Runs one vertical squared-min erosion pass over the intermediate plane.
fn morph_vertical(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
) {
    for row in 0..height {
        for column in 0..width {
            let mut value = 1.0_f32;
            for offset in 0..=(2 * radius) {
                let sample = row as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, height as isize - 1) as usize;
                value = value.min(source[clamped * width + column]);
            }
            destination[row * width + column] = value;
        }
    }
}

/// Runs one horizontal integer box average over the eroded plane.
///
/// The accumulator starts at `0.0` and the divisor is the tap count, matching
/// `fs_box_blur` with `extra.x = 2F + 1` and `extra.y = 1` (clamp at the edge).
fn box_horizontal(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
) {
    let divisor = (2 * radius + 1) as f32;
    for row in 0..height {
        for column in 0..width {
            let mut sum = 0.0_f32;
            for offset in 0..=(2 * radius) {
                let sample = column as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, width as isize - 1) as usize;
                sum += source[row * width + clamped];
            }
            destination[row * width + column] = sum / divisor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_key_cleaner_f32_controlled, box_horizontal, key_cleaner_frame_f32, KeyCleanerError,
        KeyCleanerGrade, KEY_CLEANER_AUTHOR_MAX_EDGE_RADIUS_PIXELS,
        KEY_CLEANER_DEFAULT_ALPHA_CONTRAST, KEY_CLEANER_DEFAULT_EDGE_RADIUS_PIXELS,
        KEY_CLEANER_DEFAULT_REDUCE_CHATTER, KEY_CLEANER_DEFAULT_STRENGTH,
    };

    fn grade(
        edge_radius: f32,
        reduce_chatter: f32,
        alpha_contrast: f32,
        strength: f32,
    ) -> KeyCleanerGrade {
        KeyCleanerGrade::new(edge_radius, reduce_chatter, alpha_contrast, strength)
            .expect("test grades stay inside the authored range")
    }

    /// A deterministic frame whose RGB and alpha both vary per pixel.
    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for index in 0..width * height {
            let value = index as f32;
            let x = (index % width) as f32 / width as f32;
            let y = (index / width) as f32 / height as f32;
            pixels.push([x, y, value * 0.001, (x * 0.7 + y * 0.3).clamp(0.0, 1.0)]);
        }
        pixels
    }

    fn alphas(pixels: &[[f32; 4]]) -> Vec<f32> {
        pixels.iter().map(|pixel| pixel[3]).collect()
    }

    #[test]
    fn defaults_are_the_documented_grade() {
        let defaults = KeyCleanerGrade::documented_defaults();
        let explicit = grade(10.0, 0.0, 0.0, 100.0);
        assert_eq!(defaults, explicit);
        assert_eq!(KEY_CLEANER_DEFAULT_EDGE_RADIUS_PIXELS, 10.0);
        assert_eq!(KEY_CLEANER_DEFAULT_REDUCE_CHATTER, 0.0);
        assert_eq!(KEY_CLEANER_DEFAULT_ALPHA_CONTRAST, 0.0);
        assert_eq!(KEY_CLEANER_DEFAULT_STRENGTH, 100.0);
        assert_eq!(defaults.radius_pixels(), 10);
        assert_eq!(defaults.chatter_pixels(), 0);
        assert!(!defaults.is_identity());
        // Three passes without the optional softening, four with it.
        assert_eq!(defaults.cost(), 3);
        assert_eq!(grade(10.0, 1.0, 0.0, 100.0).cost(), 4);
        // The stage payload rounds once, here, from the same authored value.
        let (params, extra) = grade(10.4, 0.5, 50.0, 25.0).gpu_controls();
        assert_eq!(params, [10.0, 0.0, 0.0, 0.0]);
        assert_eq!(extra, [5.0, 0.5, 0.25, 0.0]);
    }

    #[test]
    fn authored_values_outside_the_range_are_rejected() {
        let cases = [
            ((-1.0, 0.0, 0.0, 100.0), KeyCleanerError::EdgeRadius),
            ((251.0, 0.0, 0.0, 100.0), KeyCleanerError::EdgeRadius),
            ((10.0, -0.1, 0.0, 100.0), KeyCleanerError::ReduceChatter),
            ((10.0, 1.1, 0.0, 100.0), KeyCleanerError::ReduceChatter),
            ((10.0, 0.0, -1.0, 100.0), KeyCleanerError::AlphaContrast),
            ((10.0, 0.0, 101.0, 100.0), KeyCleanerError::AlphaContrast),
            ((10.0, 0.0, 0.0, -1.0), KeyCleanerError::Strength),
            ((10.0, 0.0, 0.0, 101.0), KeyCleanerError::Strength),
            ((f32::NAN, 0.0, 0.0, 100.0), KeyCleanerError::EdgeRadius),
            (
                (f32::INFINITY, 0.0, 0.0, 100.0),
                KeyCleanerError::EdgeRadius,
            ),
            ((10.0, f32::NAN, 0.0, 100.0), KeyCleanerError::ReduceChatter),
            (
                (10.0, f32::INFINITY, 0.0, 100.0),
                KeyCleanerError::ReduceChatter,
            ),
            ((10.0, 0.0, f32::NAN, 100.0), KeyCleanerError::AlphaContrast),
            (
                (10.0, 0.0, f32::INFINITY, 100.0),
                KeyCleanerError::AlphaContrast,
            ),
            ((10.0, 0.0, 0.0, f32::NAN), KeyCleanerError::Strength),
            ((10.0, 0.0, 0.0, f32::INFINITY), KeyCleanerError::Strength),
        ];
        for ((radius, chatter, contrast, strength), expected) in cases {
            let error = KeyCleanerGrade::new(radius, chatter, contrast, strength)
                .expect_err("out-of-range grade");
            assert_eq!(
                error, expected,
                "wrong rejection for {radius} {chatter} {contrast} {strength}"
            );
        }
        assert!(KeyCleanerGrade::new(250.0, 1.0, 100.0, 100.0).is_ok());
        assert!(KeyCleanerGrade::new(0.0, 0.0, 0.0, 0.0).is_ok());
    }

    #[test]
    fn error_messages_name_the_documented_ranges() {
        assert_eq!(
            KeyCleanerError::EdgeRadius.to_string(),
            "key cleaner edge radius must be a finite value inside [0, 250]"
        );
        assert_eq!(
            KeyCleanerError::ReduceChatter.to_string(),
            "key cleaner reduce chatter must be a finite value inside [0, 1]"
        );
        assert_eq!(
            KeyCleanerError::AlphaContrast.to_string(),
            "key cleaner alpha contrast must be a finite value inside [0, 100]"
        );
        assert_eq!(
            KeyCleanerError::Strength.to_string(),
            "key cleaner strength must be a finite value inside [0, 100]"
        );
    }

    #[test]
    fn radius_rounds_half_away_from_zero() {
        assert_eq!(grade(10.4, 0.0, 0.0, 100.0).radius_pixels(), 10);
        assert_eq!(grade(10.5, 0.0, 0.0, 100.0).radius_pixels(), 11);
        assert_eq!(grade(10.6, 0.0, 0.0, 100.0).radius_pixels(), 11);
        assert_eq!(grade(249.5, 0.0, 0.0, 100.0).radius_pixels(), 250);
        assert_eq!(grade(0.49, 0.0, 0.0, 100.0).radius_pixels(), 0);
        assert_eq!(
            grade(KEY_CLEANER_AUTHOR_MAX_EDGE_RADIUS_PIXELS, 0.0, 0.0, 100.0).radius_pixels(),
            250
        );
    }

    #[test]
    fn chatter_is_measured_in_radius_units() {
        assert_eq!(grade(20.0, 0.5, 0.0, 100.0).chatter_pixels(), 10);
        // 0.025 * 20 == 0.5 exactly, and f32::round is half away from zero.
        assert_eq!(grade(20.0, 0.025, 0.0, 100.0).chatter_pixels(), 1);
        assert_eq!(grade(20.0, 0.02, 0.0, 100.0).chatter_pixels(), 0);
        assert_eq!(grade(0.0, 1.0, 0.0, 100.0).chatter_pixels(), 0);
        // The coupling is the invariant that keeps a transparent pixel clear.
        for radius in [0.0_f32, 1.4, 7.0, 63.5, 250.0] {
            for chatter in [0.0_f32, 0.1, 0.33, 0.5, 0.99, 1.0] {
                let grade = grade(radius, chatter, 0.0, 100.0);
                assert!(grade.chatter_pixels() <= grade.radius_pixels());
            }
        }
    }

    #[test]
    fn zero_strength_is_the_identity() {
        let mut pixels = frame(16, 12);
        let original = pixels.clone();
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            16,
            12,
            &grade(10.0, 0.5, 80.0, 0.0)
        ));
        for (before, after) in original.iter().zip(pixels.iter()) {
            for channel in 0..4 {
                assert_eq!(
                    before[channel].to_bits(),
                    after[channel].to_bits(),
                    "channel {channel} changed under zero strength"
                );
            }
        }
    }

    #[test]
    fn zero_radius_with_zero_contrast_is_the_identity() {
        let mut pixels = frame(16, 12);
        let original = pixels.clone();
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            16,
            12,
            &grade(0.0, 1.0, 0.0, 100.0)
        ));
        for (before, after) in original.iter().zip(pixels.iter()) {
            for channel in 0..4 {
                assert_eq!(before[channel].to_bits(), after[channel].to_bits());
            }
        }
    }

    #[test]
    fn zero_radius_keeps_the_contrast_curve() {
        let mut pixels = vec![[0.1_f32, 0.2, 0.3, 0.4]];
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            1,
            1,
            &grade(0.0, 0.0, 50.0, 100.0)
        ));
        // t = (0.4 - 0.25) / 0.5 = 0.3, smoothstep = 0.216,
        // mix(0.4, 0.216, 0.5) = 0.308.
        assert!(
            (pixels[0][3] - 0.308).abs() <= 1.0e-6,
            "expected 0.308, got {}",
            pixels[0][3]
        );
        // The colour is still untouched.
        assert_eq!(pixels[0][0], 0.1);
        assert_eq!(pixels[0][1], 0.2);
        assert_eq!(pixels[0][2], 0.3);
    }

    #[test]
    fn erosion_reaches_exactly_the_radius() {
        let width = 24;
        let height = 8;
        let ramp = |x: usize| (x as f32 / 32.0).clamp(0.0, 1.0);
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; width * height];
        for y in 0..height {
            for x in 0..width {
                pixels[y * width + x] = [0.5, 0.5, 0.5, ramp(x)];
            }
        }
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(4.0, 0.0, 0.0, 100.0)
        ));
        for y in 0..height {
            for x in 0..width {
                let expected = if x >= 4 { ramp(x - 4) } else { 0.0 };
                let actual = pixels[y * width + x][3];
                assert!(
                    (actual - expected).abs() <= 1.0e-6,
                    "pixel ({x}, {y}): expected {expected}, got {actual}"
                );
            }
        }
    }

    #[test]
    fn the_window_is_a_square_not_a_cross() {
        let width = 5;
        let height = 5;
        let mut pixels = vec![[0.2_f32, 0.4, 0.6, 1.0]; width * height];
        pixels[2 * width + 2][3] = 0.0;
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(1.0, 0.0, 0.0, 100.0)
        ));
        let zeros = pixels.iter().filter(|pixel| pixel[3] == 0.0).count();
        assert_eq!(zeros, 9, "a square 3x3 window clears nine pixels");
        assert_eq!(pixels[width][3], 1.0, "the window does not reach (0, 1)");
        assert_eq!(
            pixels[width + 1][3],
            0.0,
            "the diagonal is inside the square"
        );
        assert_eq!(
            pixels[3 * width + 1][3],
            0.0,
            "the anti-diagonal is inside too"
        );
        // A cross-shaped element would have cleared five pixels and left (1, 1)
        // opaque, so this count pins the structuring element.
    }

    #[test]
    fn chatter_softens_the_eroded_step() {
        let width = 12;
        let height = 6;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; width * height];
        for y in 0..height {
            for x in 0..width {
                let alpha = if x < 4 { 0.0 } else { 1.0 };
                pixels[y * width + x] = [0.25, 0.5, 0.75, alpha];
            }
        }
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(2.0, 1.0, 0.0, 100.0)
        ));
        let row = 2;
        let at = |x: usize| pixels[row * width + x][3];
        // The erosion moved the step to x = 6, and the radius-2 box average
        // turns it into the ramp b(4) = 0.2, b(5) = 0.4, b(6) = 0.6,
        // b(7) = 0.8, b(8) = 1.0.
        assert!((at(3) - 0.0).abs() <= 1.0e-6, "b(3) = {}", at(3));
        assert!((at(4) - 0.2).abs() <= 1.0e-6, "b(4) = {}", at(4));
        assert!((at(5) - 0.4).abs() <= 1.0e-6, "b(5) = {}", at(5));
        assert!((at(6) - 0.6).abs() <= 1.0e-6, "b(6) = {}", at(6));
        assert!((at(7) - 0.8).abs() <= 1.0e-6, "b(7) = {}", at(7));
        assert!((at(8) - 1.0).abs() <= 1.0e-6, "b(8) = {}", at(8));
    }

    #[test]
    fn alpha_contrast_matches_the_smoothstep_curve() {
        // One pixel per case, so the erosion is a one-tap clamp and the box
        // average is the identity: the output is exactly the curve.
        let cases = [
            (0.25_f32, 100.0_f32, 0.0_f32),
            (0.4, 100.0, 0.216),
            (0.5, 100.0, 0.5),
            (0.6, 100.0, 0.784),
            (0.75, 100.0, 1.0),
            (0.4, 50.0, 0.308),
            (0.4, 0.0, 0.4),
        ];
        for (alpha, contrast, expected) in cases {
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, alpha]];
            assert!(key_cleaner_frame_f32(
                &mut pixels,
                1,
                1,
                &grade(0.0, 0.0, contrast, 100.0)
            ));
            assert!(
                (pixels[0][3] - expected).abs() <= 1.0e-6,
                "alpha {alpha} contrast {contrast}: expected {expected}, got {}",
                pixels[0][3]
            );
        }
    }

    #[test]
    fn separable_passes_match_a_direct_two_dimensional_reference() {
        let width = 9;
        let height = 7;
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let alpha = (((x * 7 + y * 5) % 11) as f32 / 10.0).clamp(0.0, 1.0);
                pixels.push([0.1 * x as f32, 0.2 * y as f32, 0.3, alpha]);
            }
        }
        let radius = 2_usize;
        let blur = 1_usize;
        let grade = grade(2.0, 0.5, 0.0, 100.0);
        assert_eq!(grade.radius_pixels(), radius);
        assert_eq!(grade.chatter_pixels(), blur);
        let mut actual = pixels.clone();
        assert!(key_cleaner_frame_f32(&mut actual, width, height, &grade));

        let clamped_index =
            |value: isize, extent: usize| value.clamp(0, extent as isize - 1) as usize;
        for y in 0..height {
            for x in 0..width {
                let mut minimum = 1.0_f32;
                for dy in 0..=(2 * radius) {
                    let sy = clamped_index(y as isize + dy as isize - radius as isize, height);
                    for dx in 0..=(2 * radius) {
                        let sx = clamped_index(x as isize + dx as isize - radius as isize, width);
                        minimum = minimum.min(pixels[sy * width + sx][3]);
                    }
                }
                let mut sum = 0.0_f32;
                for dy in 0..=(2 * blur) {
                    let sy = clamped_index(y as isize + dy as isize - blur as isize, height);
                    for dx in 0..=(2 * blur) {
                        let sx = clamped_index(x as isize + dx as isize - blur as isize, width);
                        let mut inner = 1.0_f32;
                        for my in 0..=(2 * radius) {
                            let iy =
                                clamped_index(sy as isize + my as isize - radius as isize, height);
                            for mx in 0..=(2 * radius) {
                                let ix = clamped_index(
                                    sx as isize + mx as isize - radius as isize,
                                    width,
                                );
                                inner = inner.min(pixels[iy * width + ix][3]);
                            }
                        }
                        sum += inner;
                    }
                }
                let expected = sum / (2 * blur + 1).pow(2) as f32;
                let actual_alpha = actual[y * width + x][3];
                assert!(
                    (actual_alpha - expected).abs() <= 1.0e-6,
                    "pixel ({x}, {y}): expected {expected}, got {actual_alpha} \
                     (separable minimum was {minimum})"
                );
            }
        }
    }

    #[test]
    fn strength_blends_between_the_source_and_the_processed_alpha() {
        let width = 11;
        let height = 9;
        let pixels = frame(width, height);
        let full = grade(5.0, 0.5, 60.0, 100.0);
        let half = grade(5.0, 0.5, 60.0, 50.0);
        let mut full_pixels = pixels.clone();
        let mut half_pixels = pixels.clone();
        assert!(key_cleaner_frame_f32(
            &mut full_pixels,
            width,
            height,
            &full
        ));
        assert!(key_cleaner_frame_f32(
            &mut half_pixels,
            width,
            height,
            &half
        ));
        for (index, ((before, full_pixel), half_pixel)) in
            pixels.iter().zip(full_pixels.iter()).zip(half_pixels.iter()).enumerate()
        {
            let expected = before[3] * 0.5 + full_pixel[3] * 0.5;
            assert!(
                (half_pixel[3] - expected).abs() <= 1.0e-6,
                "pixel {index}: expected {expected}, got {}",
                half_pixel[3]
            );
        }
    }

    #[test]
    fn rgb_is_bit_identical_after_the_alpha_change() {
        let width = 13;
        let height = 10;
        let mut pixels = Vec::with_capacity(width * height);
        for index in 0..width * height {
            let value = index as f32;
            let alpha = (index % 7) as f32 / 6.0;
            pixels.push([value * 0.01, 1.0 - value * 0.005, value * 0.002, alpha]);
        }
        let original = pixels.clone();
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(10.0, 1.0, 100.0, 100.0)
        ));
        let mut changed = 0;
        for (before, after) in original.iter().zip(pixels.iter()) {
            for channel in 0..3 {
                assert_eq!(
                    before[channel].to_bits(),
                    after[channel].to_bits(),
                    "channel {channel} changed"
                );
            }
            if before[3].to_bits() != after[3].to_bits() {
                changed += 1;
            }
        }
        assert!(changed > 0, "the fixture must exercise the alpha change");
    }

    #[test]
    fn transparent_pixels_stay_transparent() {
        let width = 12;
        let height = 10;
        let mut pixels = Vec::with_capacity(width * height);
        for index in 0..width * height {
            let transparent = index % 2 == 0;
            pixels.push([0.9, 0.8, 0.7, if transparent { 0.0 } else { 1.0 }]);
        }
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(5.0, 1.0, 100.0, 100.0)
        ));
        for (index, pixel) in pixels.iter().enumerate() {
            if index % 2 == 0 {
                assert_eq!(
                    pixel[3].to_bits(),
                    0.0_f32.to_bits(),
                    "pixel {index} invented coverage: {}",
                    pixel[3]
                );
            }
        }
    }

    #[test]
    fn frame_edges_clamp_instead_of_zero_padding() {
        let width = 5;
        let height = 3;
        let mut pixels = vec![[0.3_f32, 0.4, 0.5, 1.0]; width * height];
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(3.0, 0.0, 0.0, 100.0)
        ));
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[3], 1.0, "pixel {index} was eroded at the frame edge");
        }
    }

    #[test]
    fn box_horizontal_replicates_the_border_column() {
        let mut destination = [0.0_f32; 2];
        box_horizontal(&[1.0, 0.0], &mut destination, 2, 1, 1);
        // Column 0 reads (1, 1, 0) and column 1 reads (1, 0, 0); a shrinking
        // window would give (0.5, 0.5).
        assert_eq!(destination, [0.666_666_7, 0.333_333_34]);
    }

    #[test]
    fn unaddressable_frames_are_rejected() {
        let original = [[0.4_f32, 0.5, 0.6, 0.7]];
        let grade = grade(3.0, 0.0, 0.0, 100.0);

        let mut overflowing = original.to_vec();
        assert!(!key_cleaner_frame_f32(
            &mut overflowing,
            usize::MAX,
            2,
            &grade
        ));
        assert_eq!(overflowing, original);

        let mut short = original.to_vec();
        assert!(!key_cleaner_frame_f32(&mut short, 4, 4, &grade));
        assert_eq!(short, original);

        let mut zero_width = original.to_vec();
        assert!(!key_cleaner_frame_f32(&mut zero_width, 0, 4, &grade));
        assert_eq!(zero_width, original);

        let mut zero_height = original.to_vec();
        assert!(!key_cleaner_frame_f32(&mut zero_height, 4, 0, &grade));
        assert_eq!(zero_height, original);
    }

    #[test]
    fn checkpoint_cadence_is_bounded() {
        let width = 4096;
        let height = 2;
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 0.75]; width * height];
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        let applied = apply_key_cleaner_f32_controlled(
            &mut pixels,
            width,
            height,
            &grade(20.0, 1.0, 50.0, 100.0),
            &mut checkpoint,
        )
        .expect("infallible checkpoint");
        assert!(applied);
        assert!(
            calls <= 16,
            "checkpoint ran {calls} times on {width}x{height}, expected O(pixels / 4096)"
        );

        // An error is propagated and no alpha write happens before stage four.
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 0.75]; width * height];
        let original = pixels.clone();
        let mut calls = 0_usize;
        let mut failing = || {
            calls += 1;
            if calls > 2 {
                Err(TestCheckpointError)
            } else {
                Ok(())
            }
        };
        let result = apply_key_cleaner_f32_controlled(
            &mut pixels,
            width,
            height,
            &grade(20.0, 1.0, 50.0, 100.0),
            &mut failing,
        );
        assert!(result.is_err(), "the checkpoint error must propagate");
        assert_eq!(pixels, original, "the frame must stay untouched");
    }

    #[derive(Debug, PartialEq, Eq)]
    struct TestCheckpointError;

    #[test]
    fn alphas_are_only_ever_shrunk_by_the_erosion() {
        // The erosion is a minimum, so no pixel may gain coverage, and the
        // softening can only average values the erosion already produced.
        let width = 8;
        let height = 6;
        let mut pixels = frame(width, height);
        let original = alphas(&pixels);
        assert!(key_cleaner_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(2.0, 1.0, 100.0, 100.0)
        ));
        for (index, pixel) in pixels.iter().enumerate() {
            assert!(
                pixel[3] <= original[index] + 1.0e-6,
                "pixel {index} grew from {} to {}",
                original[index],
                pixel[3]
            );
        }
    }
}
