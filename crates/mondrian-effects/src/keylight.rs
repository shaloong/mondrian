//! Keylight: pulls a matte from a screen colour and despills the spill.
//!
//! The kernel is the whole effect; the render op only carries a validated
//! [`KeylightGrade`]. Every stage below is straight-alpha and f32, and the CPU
//! kernel and the GPU shaders evaluate the same expressions in the same order so
//! a parity test can compare them to `1e-4`.
//!
//! # Stages
//!
//! 1. `preblur > 0`: two box-blur passes (horizontal then vertical) over the
//!    source. The first pass premultiplies and the last unpremultiplies, so
//!    hidden RGB cannot bleed into the key.
//! 2. The key pass separates the source from the screen colour and writes the
//!    matte into every channel of the matte plane with `a = 1`. Copying the
//!    matte into every channel means the morphology passes and the composite can
//!    all read `.x` without an alpha convention.
//! 3. `edge_shrink > 0`: two separable min-filter passes (horizontal then
//!    vertical) over the matte, which erodes the foreground.
//! 4. `edge_feather > 0`: two box-blur passes over the matte.
//! 5. The composite reads the original source and the final matte and writes the
//!    selected [`KeylightDisplay`] mode.
//!
//! # The key core
//!
//! The key channel is the largest component of the screen colour; ties resolve
//! to the lower channel index, so the choice is a pure function of the authored
//! colour. With `key` the key channel and `other_a`/`other_b` the two remaining
//! channels in ascending index order:
//!
//! ```text
//! ref   = balance * other_a + (1 - balance) * other_b + (despill_bias[key] - 0.5)
//! diff  = colour[key] - ref
//! raw   = clamp(1 - screen_gain * diff / max(screen_color[key], 0.01), 0, 1)
//! matte = clamp((raw - inner) / max(outer - inner, 1e-4), 0, 1)
//! ```
//!
//! with `inner = inner_matte / 100` and `outer = outer_matte / 100`. The despill
//! replaces the key channel of the output colour with
//! `key - despill * max(0, key - (ref - (despill_bias[key] - 0.5)))`, clamped at
//! zero, so the bias moves the separation and the despill threshold together
//! while the despill amount stays a pure strength.
//!
//! `screen_gain = 0` disables the key: `raw` is `1` everywhere, so the matte is
//! `1` (once the outer matte is above the inner one) and the effect becomes the
//! optional despill plus the morphology of a full matte. That is the documented
//! identity, not an all-transparent frame.
//!
//! # Uniform packing
//!
//! Each pass receives one `SpatialPassUniform` with five `[f32; 4]` cells:
//!
//! | pass | `params` | `extra` | `aces` | `extra1` |
//! | --- | --- | --- | --- | --- |
//! | key | `[screen_gain, screen_balance, inner, outer]` | `[key_channel, despill, despill_bias[key], 0]` | `[screen_color.rgb, 0]` | `[0; 4]` |
//! | shrink | `[radius, axis, 0, 0]` | `[0; 4]` | `[0; 4]` | `[0; 4]` |
//! | composite | `[display_index, despill, key_channel, 0]` | `[despill_bias[key] - 0.5, 0, 0, 0]` | `[screen_color.rgb, 0]` | `[screen_balance, 0, 0, 0]` |
//!
//! The composite's `extra1.x` carries the screen balance, which the design's
//! packing table leaves zero. The despill reference is the same balance-weighted
//! pair as the key reference, so the shader cannot rebuild it from the cells the
//! table lists; the spare cell is the only place it fits without moving a
//! documented value.
//!
//! # Decisions left open by the design brief
//!
//! * A source pixel with `a <= 0` is a hole: the key pass writes a zero matte
//!   (so shrinking and feathering see the hole) and the composite returns the
//!   source pixel untouched for every display mode, which keeps transparent
//!   pixels bit-identical across the effect.
//! * `edge_shrink` is a min filter over `2 * round(edge_shrink) + 1` taps: a
//!   morphology cannot read a fractional tap, so the authored radius is rounded
//!   on both the CPU and the GPU. `preblur` and `edge_feather` stay fractional
//!   because the box kernel has a fractional tail.
//! * `inner_matte > outer_matte` is allowed. The denominator floors at `1e-4`,
//!   which turns the ramp into a near-binary matte instead of dividing by zero.

use crate::box_blur::{box_blur_frame_f32, BoxBlurDirection};

/// Largest authored screen gain.
pub const KEYLIGHT_AUTHOR_MAX_SCREEN_GAIN: f32 = 10.0;
/// Largest authored despill amount.
pub const KEYLIGHT_AUTHOR_MAX_DESPILL: f32 = 2.0;
/// Largest authored matte percentage.
pub const KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT: f32 = 100.0;
/// Largest authored preblur radius in pixels.
pub const KEYLIGHT_AUTHOR_MAX_PREBLUR_PIXELS: f32 = 20.0;
/// Largest authored edge shrink radius in pixels.
pub const KEYLIGHT_AUTHOR_MAX_EDGE_SHRINK_PIXELS: f32 = 20.0;
/// Largest authored edge feather radius in pixels.
pub const KEYLIGHT_AUTHOR_MAX_EDGE_FEATHER_PIXELS: f32 = 50.0;

/// Documented default screen colour: pure green.
pub const KEYLIGHT_DEFAULT_SCREEN_COLOR: [f32; 3] = [0.0, 1.0, 0.0];
/// Documented default screen gain.
pub const KEYLIGHT_DEFAULT_SCREEN_GAIN: f32 = 1.0;
/// Documented default screen balance.
pub const KEYLIGHT_DEFAULT_SCREEN_BALANCE: f32 = 0.5;
/// Documented default despill amount.
pub const KEYLIGHT_DEFAULT_DESPILL: f32 = 1.0;
/// Documented default despill bias: neutral.
pub const KEYLIGHT_DEFAULT_DESPILL_BIAS: [f32; 3] = [0.5, 0.5, 0.5];
/// Documented default inner matte percentage.
pub const KEYLIGHT_DEFAULT_INNER_MATTE: f32 = 0.0;
/// Documented default outer matte percentage.
pub const KEYLIGHT_DEFAULT_OUTER_MATTE: f32 = 100.0;
/// Documented default preblur radius in pixels.
pub const KEYLIGHT_DEFAULT_PREBLUR_PIXELS: f32 = 0.0;
/// Documented default edge shrink radius in pixels.
pub const KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS: f32 = 0.0;
/// Documented default edge feather radius in pixels.
pub const KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS: f32 = 0.0;

/// The output a keylight composite writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeylightDisplay {
    /// The despilled source over the matte.
    Final,
    /// The matte itself, opaque.
    Matte,
    /// The despilled source, opaque.
    Foreground,
    /// The source colour with the inverse matte as alpha.
    Background,
}

impl KeylightDisplay {
    /// Every display mode in authoring menu order.
    pub const ALL: [Self; 4] = [Self::Final, Self::Matte, Self::Foreground, Self::Background];

    /// Stable persistence key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Final => "final",
            Self::Matte => "matte",
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }

    /// Parses a persistence key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "final" => Some(Self::Final),
            "matte" => Some(Self::Matte),
            "foreground" => Some(Self::Foreground),
            "background" => Some(Self::Background),
            _ => None,
        }
    }

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Final => "最终",
            Self::Matte => "遮罩",
            Self::Foreground => "前景",
            Self::Background => "背景",
        }
    }

    /// One-based authoring menu position.
    pub const fn as_menu(self) -> u32 {
        self.as_index() + 1
    }

    /// Parses a one-based authoring menu position.
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Final),
            2 => Some(Self::Matte),
            3 => Some(Self::Foreground),
            4 => Some(Self::Background),
            _ => None,
        }
    }

    /// Zero-based identifier packed into the GPU uniforms.
    pub const fn as_index(self) -> u32 {
        match self {
            Self::Final => 0,
            Self::Matte => 1,
            Self::Foreground => 2,
            Self::Background => 3,
        }
    }
}

/// The rejection reason of a [`KeylightGrade`] field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeylightError {
    /// The screen colour is not a finite RGB triple inside `[0, 1]`.
    ScreenColor,
    /// The screen gain is outside `[0, 10]` or not finite.
    ScreenGain,
    /// The screen balance is outside `[0, 1]` or not finite.
    ScreenBalance,
    /// The despill amount is outside `[0, 2]` or not finite.
    Despill,
    /// The despill bias is not a finite RGB triple inside `[0, 1]`.
    DespillBias,
    /// The inner matte is outside `[0, 100]` or not finite.
    InnerMatte,
    /// The outer matte is outside `[0, 100]` or not finite.
    OuterMatte,
    /// The preblur radius is outside `[0, 20]` or not finite.
    Preblur,
    /// The edge shrink radius is outside `[0, 20]` or not finite.
    EdgeShrink,
    /// The edge feather radius is outside `[0, 50]` or not finite.
    EdgeFeather,
}

impl std::fmt::Display for KeylightError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::ScreenColor => "keylight screen colour must be a finite rgb triple inside [0, 1]",
            Self::ScreenGain => "keylight screen gain must be a finite value inside [0, 10]",
            Self::ScreenBalance => "keylight screen balance must be a finite value inside [0, 1]",
            Self::Despill => "keylight despill must be a finite value inside [0, 2]",
            Self::DespillBias => "keylight despill bias must be a finite rgb triple inside [0, 1]",
            Self::InnerMatte => "keylight inner matte must be a finite value inside [0, 100]",
            Self::OuterMatte => "keylight outer matte must be a finite value inside [0, 100]",
            Self::Preblur => "keylight preblur must be a finite value inside [0, 20]",
            Self::EdgeShrink => "keylight edge shrink must be a finite value inside [0, 20]",
            Self::EdgeFeather => "keylight edge feather must be a finite value inside [0, 50]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for KeylightError {}

/// The two non-key channels of a key index, in ascending order.
#[must_use]
pub const fn keylight_other_channels(key_channel: usize) -> (usize, usize) {
    match key_channel {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

/// The index of the largest component of a screen colour.
///
/// Ties resolve to the lower channel index, so `[0.5, 0.5, 0.1]` keys on red and
/// `[0.1, 0.5, 0.5]` keys on green. The comparison is strict so a later channel
/// only wins when it is strictly larger.
#[must_use]
pub fn keylight_key_channel(screen_color: [f32; 3]) -> usize {
    let mut channel = 0;
    if screen_color[1] > screen_color[channel] {
        channel = 1;
    }
    if screen_color[2] > screen_color[channel] {
        channel = 2;
    }
    channel
}

/// The authored parameters of one keylight operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeylightGrade {
    screen_color: [f32; 3],
    screen_gain: f32,
    screen_balance: f32,
    despill: f32,
    despill_bias: [f32; 3],
    inner_matte: f32,
    outer_matte: f32,
    preblur: f32,
    edge_shrink: f32,
    edge_feather: f32,
    display: KeylightDisplay,
    key_channel: usize,
}

impl KeylightGrade {
    /// Builds a grade from authored parameters.
    ///
    /// # Errors
    ///
    /// Returns the [`KeylightError`] of the first field outside its documented
    /// range.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        screen_color: [f32; 3],
        screen_gain: f32,
        screen_balance: f32,
        despill: f32,
        despill_bias: [f32; 3],
        inner_matte: f32,
        outer_matte: f32,
        preblur: f32,
        edge_shrink: f32,
        edge_feather: f32,
        display: KeylightDisplay,
    ) -> Result<Self, KeylightError> {
        if screen_color
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(KeylightError::ScreenColor);
        }
        if !screen_gain.is_finite()
            || !(0.0..=KEYLIGHT_AUTHOR_MAX_SCREEN_GAIN).contains(&screen_gain)
        {
            return Err(KeylightError::ScreenGain);
        }
        if !screen_balance.is_finite() || !(0.0..=1.0).contains(&screen_balance) {
            return Err(KeylightError::ScreenBalance);
        }
        if !despill.is_finite() || !(0.0..=KEYLIGHT_AUTHOR_MAX_DESPILL).contains(&despill) {
            return Err(KeylightError::Despill);
        }
        if despill_bias
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(KeylightError::DespillBias);
        }
        if !inner_matte.is_finite()
            || !(0.0..=KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT).contains(&inner_matte)
        {
            return Err(KeylightError::InnerMatte);
        }
        if !outer_matte.is_finite()
            || !(0.0..=KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT).contains(&outer_matte)
        {
            return Err(KeylightError::OuterMatte);
        }
        if !preblur.is_finite() || !(0.0..=KEYLIGHT_AUTHOR_MAX_PREBLUR_PIXELS).contains(&preblur) {
            return Err(KeylightError::Preblur);
        }
        if !edge_shrink.is_finite()
            || !(0.0..=KEYLIGHT_AUTHOR_MAX_EDGE_SHRINK_PIXELS).contains(&edge_shrink)
        {
            return Err(KeylightError::EdgeShrink);
        }
        if !edge_feather.is_finite()
            || !(0.0..=KEYLIGHT_AUTHOR_MAX_EDGE_FEATHER_PIXELS).contains(&edge_feather)
        {
            return Err(KeylightError::EdgeFeather);
        }
        Ok(Self {
            screen_color,
            screen_gain,
            screen_balance,
            despill,
            despill_bias,
            inner_matte,
            outer_matte,
            preblur,
            edge_shrink,
            edge_feather,
            display,
            key_channel: keylight_key_channel(screen_color),
        })
    }

    /// The documented default grade.
    ///
    /// # Errors
    ///
    /// Never fails: every documented default is inside its range. The result is
    /// propagated as a `Result` so callers can share one parsing path.
    pub fn documented_defaults() -> Result<Self, KeylightError> {
        Self::new(
            KEYLIGHT_DEFAULT_SCREEN_COLOR,
            KEYLIGHT_DEFAULT_SCREEN_GAIN,
            KEYLIGHT_DEFAULT_SCREEN_BALANCE,
            KEYLIGHT_DEFAULT_DESPILL,
            KEYLIGHT_DEFAULT_DESPILL_BIAS,
            KEYLIGHT_DEFAULT_INNER_MATTE,
            KEYLIGHT_DEFAULT_OUTER_MATTE,
            KEYLIGHT_DEFAULT_PREBLUR_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS,
            KeylightDisplay::Final,
        )
    }

    /// The authored screen colour.
    #[must_use]
    pub const fn screen_color(&self) -> [f32; 3] {
        self.screen_color
    }

    /// The authored screen gain.
    #[must_use]
    pub const fn screen_gain(&self) -> f32 {
        self.screen_gain
    }

    /// The authored screen balance.
    #[must_use]
    pub const fn screen_balance(&self) -> f32 {
        self.screen_balance
    }

    /// The authored despill amount.
    #[must_use]
    pub const fn despill(&self) -> f32 {
        self.despill
    }

    /// The authored despill bias.
    #[must_use]
    pub const fn despill_bias(&self) -> [f32; 3] {
        self.despill_bias
    }

    /// The authored inner matte percentage.
    #[must_use]
    pub const fn inner_matte(&self) -> f32 {
        self.inner_matte
    }

    /// The authored outer matte percentage.
    #[must_use]
    pub const fn outer_matte(&self) -> f32 {
        self.outer_matte
    }

    /// The authored preblur radius in pixels.
    #[must_use]
    pub const fn preblur(&self) -> f32 {
        self.preblur
    }

    /// The authored edge shrink radius in pixels.
    #[must_use]
    pub const fn edge_shrink(&self) -> f32 {
        self.edge_shrink
    }

    /// The authored edge feather radius in pixels.
    #[must_use]
    pub const fn edge_feather(&self) -> f32 {
        self.edge_feather
    }

    /// The authored display mode.
    #[must_use]
    pub const fn display(&self) -> KeylightDisplay {
        self.display
    }

    /// The key channel derived from the screen colour.
    #[must_use]
    pub const fn key_channel(&self) -> usize {
        self.key_channel
    }

    /// The integer min-filter radius the edge shrink stage uses.
    ///
    /// The min filter cannot read a fractional tap, so the authored radius is
    /// rounded here and in the shader from the same packed value.
    #[must_use]
    pub fn shrink_radius(&self) -> usize {
        self.edge_shrink.round() as usize
    }

    /// Whether the grade is a no-op and the graph builder may elide it.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        matches!(self.display, KeylightDisplay::Final)
            && self.preblur == 0.0
            && self.edge_shrink == 0.0
            && self.edge_feather == 0.0
            && self.despill == 0.0
            && self.inner_matte == 0.0
            && self.outer_matte == KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT
            && self.screen_gain == 0.0
    }

    /// The biased separation reference of one straight colour.
    ///
    /// This is the `ref` of the key core: the balance-weighted pair of non-key
    /// channels plus the despill bias offset.
    #[must_use]
    pub fn reference(&self, colour: [f32; 3]) -> f32 {
        let (other_a, other_b) = keylight_other_channels(self.key_channel);
        let base =
            self.screen_balance * colour[other_a] + (1.0 - self.screen_balance) * colour[other_b];
        base + (self.despill_bias[self.key_channel] - 0.5)
    }

    /// The matte of one straight colour.
    #[must_use]
    pub fn matte_of_rgb(&self, colour: [f32; 3]) -> f32 {
        let reference = self.reference(colour);
        let separation = self.screen_color[self.key_channel].max(0.01);
        let raw = (1.0 - self.screen_gain * (colour[self.key_channel] - reference) / separation)
            .clamp(0.0, 1.0);
        let inner = self.inner_matte / KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT;
        let outer = self.outer_matte / KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT;
        ((raw - inner) / (outer - inner).max(0.0001)).clamp(0.0, 1.0)
    }

    /// The matte of one straight-alpha pixel.
    ///
    /// A pixel without coverage is a hole and contributes a zero matte.
    #[must_use]
    pub fn matte_for(&self, pixel: [f32; 4]) -> f32 {
        if pixel[3] <= 0.0 {
            return 0.0;
        }
        self.matte_of_rgb([pixel[0], pixel[1], pixel[2]])
    }

    /// Replaces the key channel of one straight colour with its despilled value.
    #[must_use]
    pub fn despilled(&self, colour: [f32; 3]) -> [f32; 3] {
        let key = self.key_channel;
        let bias = self.despill_bias[key] - 0.5;
        let reference = self.reference(colour) - bias;
        let value = colour[key];
        let stripped = (value - self.despill * (value - reference).max(0.0)).max(0.0);
        let mut despilled = colour;
        despilled[key] = stripped;
        despilled
    }

    /// Composites one original source pixel with its matte.
    ///
    /// A pixel without coverage is returned untouched for every display mode.
    #[must_use]
    pub fn composite(&self, source: [f32; 4], matte: f32) -> [f32; 4] {
        if source[3] <= 0.0 {
            return source;
        }
        match self.display {
            KeylightDisplay::Final => {
                let colour = self.despilled([source[0], source[1], source[2]]);
                [colour[0], colour[1], colour[2], source[3] * matte]
            }
            KeylightDisplay::Matte => [matte, matte, matte, 1.0],
            KeylightDisplay::Foreground => {
                let colour = self.despilled([source[0], source[1], source[2]]);
                [colour[0], colour[1], colour[2], 1.0]
            }
            KeylightDisplay::Background => [source[0], source[1], source[2], 1.0 - matte],
        }
    }

    /// The uniform payload of the key pass.
    ///
    /// See the module documentation for the exact cell layout.
    #[must_use]
    pub fn key_gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.screen_gain,
                self.screen_balance,
                self.inner_matte / KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT,
                self.outer_matte / KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT,
            ],
            [
                self.key_channel as f32,
                self.despill,
                self.despill_bias[self.key_channel],
                0.0,
            ],
            [
                self.screen_color[0],
                self.screen_color[1],
                self.screen_color[2],
                0.0,
            ],
            [0.0; 4],
        )
    }

    /// The uniform payload of the composite pass.
    ///
    /// See the module documentation for the exact cell layout, including the
    /// balance that `extra1.x` carries.
    #[must_use]
    pub fn composite_gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.display.as_index() as f32,
                self.despill,
                self.key_channel as f32,
                0.0,
            ],
            [self.despill_bias[self.key_channel] - 0.5, 0.0, 0.0, 0.0],
            [
                self.screen_color[0],
                self.screen_color[1],
                self.screen_color[2],
                0.0,
            ],
            [self.screen_balance, 0.0, 0.0, 0.0],
        )
    }

    /// Hashes every authored scalar so a cache key cannot alias two grades.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.screen_color.map(f32::to_bits).hash(state);
        self.screen_gain.to_bits().hash(state);
        self.screen_balance.to_bits().hash(state);
        self.despill.to_bits().hash(state);
        self.despill_bias.map(f32::to_bits).hash(state);
        self.inner_matte.to_bits().hash(state);
        self.outer_matte.to_bits().hash(state);
        self.preblur.to_bits().hash(state);
        self.edge_shrink.to_bits().hash(state);
        self.edge_feather.to_bits().hash(state);
        self.display.as_index().hash(state);
        self.key_channel.hash(state);
    }
}

const CHECKPOINT_STRIDE: usize = 4096;

/// Apply keylight to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn keylight_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &KeylightGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_keylight_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed kernel behind [`keylight_frame_f32`].
pub(crate) fn apply_keylight_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &KeylightGrade,
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
    let source: Vec<[f32; 4]> = frame.to_vec();
    checkpoint()?;

    // Stage 1: an optional preblur over a copy of the source. The box kernel
    // premultiplies once and unpremultiplies once, which is exactly the pair of
    // GPU passes the renderer records.
    let mut key_source = source.clone();
    if grade.preblur() > 0.0
        && !box_blur_frame_f32(
            &mut key_source,
            width,
            height,
            grade.preblur(),
            1,
            BoxBlurDirection::HorizontalAndVertical,
            true,
        )
    {
        return Ok(false);
    }
    checkpoint()?;

    // Stage 2: the key pass. The matte is copied into every channel of the matte
    // plane with a = 1, so the morphology and the composite all read `.x`.
    let mut matte = Vec::with_capacity(pixel_count);
    for (index, pixel) in key_source.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        let value = grade.matte_for(*pixel);
        matte.push([value, value, value, 1.0]);
    }
    checkpoint()?;

    // Stage 3: an optional separable min filter, which erodes the matte.
    let shrink_radius = grade.shrink_radius();
    if grade.edge_shrink() > 0.0 && shrink_radius > 0 {
        erode_matte(&mut matte, width, height, shrink_radius);
        checkpoint()?;
    }

    // Stage 4: an optional separable box blur over the matte.
    if grade.edge_feather() > 0.0
        && !box_blur_frame_f32(
            &mut matte,
            width,
            height,
            grade.edge_feather(),
            1,
            BoxBlurDirection::HorizontalAndVertical,
            true,
        )
    {
        return Ok(false);
    }
    checkpoint()?;

    // Stage 5: the composite, over row bands like every other spatial kernel.
    let rows = pixel_count / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let index = row * width + column;
                *pixel = grade.composite(source[index], matte[index][0]);
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

/// Erodes one matte plane with two separable min-filter passes.
///
/// Both axes clamp at the frame edge, and the matte lives in every channel, so
/// only `.x` is read and written.
fn erode_matte(matte: &mut [[f32; 4]], width: usize, height: usize, radius: usize) {
    let mut horizontal = vec![[0.0_f32; 4]; width * height];
    for row in 0..height {
        for column in 0..width {
            let mut minimum = 1.0_f32;
            for offset in 0..=(2 * radius) {
                let sample = column as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, width as isize - 1) as usize;
                minimum = minimum.min(matte[row * width + clamped][0]);
            }
            horizontal[row * width + column] = [minimum, minimum, minimum, 1.0];
        }
    }
    for column in 0..width {
        for row in 0..height {
            let mut minimum = 1.0_f32;
            for offset in 0..=(2 * radius) {
                let sample = row as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, height as isize - 1) as usize;
                minimum = minimum.min(horizontal[clamped * width + column][0]);
            }
            matte[row * width + column] = [minimum, minimum, minimum, 1.0];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_keylight_f32_controlled, keylight_frame_f32, keylight_key_channel,
        keylight_other_channels, KeylightDisplay, KeylightError, KeylightGrade,
        KEYLIGHT_AUTHOR_MAX_DESPILL, KEYLIGHT_AUTHOR_MAX_EDGE_FEATHER_PIXELS,
        KEYLIGHT_AUTHOR_MAX_EDGE_SHRINK_PIXELS, KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT,
        KEYLIGHT_AUTHOR_MAX_PREBLUR_PIXELS, KEYLIGHT_AUTHOR_MAX_SCREEN_GAIN,
        KEYLIGHT_DEFAULT_DESPILL, KEYLIGHT_DEFAULT_DESPILL_BIAS,
        KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS, KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
        KEYLIGHT_DEFAULT_INNER_MATTE, KEYLIGHT_DEFAULT_OUTER_MATTE,
        KEYLIGHT_DEFAULT_PREBLUR_PIXELS, KEYLIGHT_DEFAULT_SCREEN_BALANCE,
        KEYLIGHT_DEFAULT_SCREEN_COLOR, KEYLIGHT_DEFAULT_SCREEN_GAIN,
    };

    fn defaults() -> KeylightGrade {
        KeylightGrade::documented_defaults().expect("documented defaults are valid")
    }

    #[allow(clippy::too_many_arguments)]
    fn custom(
        screen_gain: f32,
        screen_balance: f32,
        despill: f32,
        despill_bias: [f32; 3],
        inner_matte: f32,
        outer_matte: f32,
        preblur: f32,
        edge_shrink: f32,
        edge_feather: f32,
        display: KeylightDisplay,
    ) -> KeylightGrade {
        KeylightGrade::new(
            KEYLIGHT_DEFAULT_SCREEN_COLOR,
            screen_gain,
            screen_balance,
            despill,
            despill_bias,
            inner_matte,
            outer_matte,
            preblur,
            edge_shrink,
            edge_feather,
            display,
        )
        .expect("the test grade is inside every range")
    }

    fn gain(screen_gain: f32) -> KeylightGrade {
        custom(
            screen_gain,
            KEYLIGHT_DEFAULT_SCREEN_BALANCE,
            KEYLIGHT_DEFAULT_DESPILL,
            KEYLIGHT_DEFAULT_DESPILL_BIAS,
            KEYLIGHT_DEFAULT_INNER_MATTE,
            KEYLIGHT_DEFAULT_OUTER_MATTE,
            KEYLIGHT_DEFAULT_PREBLUR_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS,
            KeylightDisplay::Final,
        )
    }

    fn balance(screen_balance: f32) -> KeylightGrade {
        custom(
            KEYLIGHT_DEFAULT_SCREEN_GAIN,
            screen_balance,
            KEYLIGHT_DEFAULT_DESPILL,
            KEYLIGHT_DEFAULT_DESPILL_BIAS,
            KEYLIGHT_DEFAULT_INNER_MATTE,
            KEYLIGHT_DEFAULT_OUTER_MATTE,
            KEYLIGHT_DEFAULT_PREBLUR_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS,
            KeylightDisplay::Final,
        )
    }

    fn matte_range(inner_matte: f32, outer_matte: f32) -> KeylightGrade {
        custom(
            KEYLIGHT_DEFAULT_SCREEN_GAIN,
            KEYLIGHT_DEFAULT_SCREEN_BALANCE,
            KEYLIGHT_DEFAULT_DESPILL,
            KEYLIGHT_DEFAULT_DESPILL_BIAS,
            inner_matte,
            outer_matte,
            KEYLIGHT_DEFAULT_PREBLUR_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS,
            KeylightDisplay::Final,
        )
    }

    fn despill_of(despill: f32, despill_bias: [f32; 3]) -> KeylightGrade {
        custom(
            KEYLIGHT_DEFAULT_SCREEN_GAIN,
            KEYLIGHT_DEFAULT_SCREEN_BALANCE,
            despill,
            despill_bias,
            KEYLIGHT_DEFAULT_INNER_MATTE,
            KEYLIGHT_DEFAULT_OUTER_MATTE,
            KEYLIGHT_DEFAULT_PREBLUR_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS,
            KeylightDisplay::Final,
        )
    }

    fn morphology(preblur: f32, edge_shrink: f32, edge_feather: f32) -> KeylightGrade {
        custom(
            KEYLIGHT_DEFAULT_SCREEN_GAIN,
            KEYLIGHT_DEFAULT_SCREEN_BALANCE,
            KEYLIGHT_DEFAULT_DESPILL,
            KEYLIGHT_DEFAULT_DESPILL_BIAS,
            KEYLIGHT_DEFAULT_INNER_MATTE,
            KEYLIGHT_DEFAULT_OUTER_MATTE,
            preblur,
            edge_shrink,
            edge_feather,
            KeylightDisplay::Final,
        )
    }

    fn showing(display: KeylightDisplay) -> KeylightGrade {
        custom(
            KEYLIGHT_DEFAULT_SCREEN_GAIN,
            KEYLIGHT_DEFAULT_SCREEN_BALANCE,
            KEYLIGHT_DEFAULT_DESPILL,
            KEYLIGHT_DEFAULT_DESPILL_BIAS,
            KEYLIGHT_DEFAULT_INNER_MATTE,
            KEYLIGHT_DEFAULT_OUTER_MATTE,
            KEYLIGHT_DEFAULT_PREBLUR_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
            KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS,
            display,
        )
    }

    const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
    const TINTED: [f32; 4] = [0.2, 0.9, 0.3, 1.0];
    const GREY: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

    fn frame<F: Fn(usize) -> [f32; 4]>(width: usize, height: usize, fill: F) -> Vec<[f32; 4]> {
        (0..width * height).map(fill).collect()
    }

    fn bits(pixels: &[[f32; 4]]) -> Vec<[u32; 4]> {
        pixels.iter().map(|pixel| pixel.map(f32::to_bits)).collect()
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() <= 1.0e-6
    }

    #[test]
    fn the_documented_defaults_are_stored_verbatim() {
        let grade = defaults();

        assert_eq!(grade.screen_color(), [0.0, 1.0, 0.0]);
        assert_eq!(grade.screen_gain(), 1.0);
        assert_eq!(grade.screen_balance(), 0.5);
        assert_eq!(grade.despill(), 1.0);
        assert_eq!(grade.despill_bias(), [0.5, 0.5, 0.5]);
        assert_eq!(grade.inner_matte(), 0.0);
        assert_eq!(grade.outer_matte(), 100.0);
        assert_eq!(grade.preblur(), 0.0);
        assert_eq!(grade.edge_shrink(), 0.0);
        assert_eq!(grade.edge_feather(), 0.0);
        assert_eq!(grade.display(), KeylightDisplay::Final);
        assert_eq!(grade.key_channel(), 1);
        assert!(
            !grade.is_identity(),
            "the default grade keys every green pixel"
        );
    }

    #[test]
    fn the_key_channel_is_the_largest_screen_component() {
        assert_eq!(keylight_key_channel([0.0, 1.0, 0.0]), 1);
        assert_eq!(keylight_key_channel([0.9, 0.1, 0.2]), 0);
        assert_eq!(keylight_key_channel([0.1, 0.2, 0.9]), 2);
        // Ties resolve to the lower channel index.
        assert_eq!(keylight_key_channel([0.5, 0.5, 0.1]), 0);
        assert_eq!(keylight_key_channel([0.1, 0.5, 0.5]), 1);
        assert_eq!(keylight_key_channel([0.5, 0.1, 0.5]), 0);
        assert_eq!(keylight_key_channel([0.2, 0.2, 0.2]), 0);

        assert_eq!(keylight_other_channels(0), (1, 2));
        assert_eq!(keylight_other_channels(1), (0, 2));
        assert_eq!(keylight_other_channels(2), (0, 1));
    }

    #[test]
    fn a_green_screen_keys_out_and_leaves_the_foreground() {
        let grade = defaults();
        let mut pixels = vec![GREEN, GREY, TINTED];

        assert!(keylight_frame_f32(&mut pixels, 3, 1, &grade));

        // The pure screen colour keys to a transparent black pixel.
        assert!(close(pixels[0][0], 0.0));
        assert!(close(pixels[0][1], 0.0));
        assert!(close(pixels[0][2], 0.0));
        assert!(close(pixels[0][3], 0.0));
        // A neutral pixel is fully foreground and despills to itself.
        assert!(close(pixels[1][0], 0.5));
        assert!(close(pixels[1][1], 0.5));
        assert!(close(pixels[1][2], 0.5));
        assert!(close(pixels[1][3], 1.0));
        // A green-tinted pixel keeps a partial matte and loses the spill.
        assert!(close(pixels[2][0], 0.2));
        assert!(close(pixels[2][1], 0.25));
        assert!(close(pixels[2][2], 0.3));
        assert!(close(pixels[2][3], 0.35));
    }

    #[test]
    fn the_screen_gain_scales_the_separation() {
        let colour = [0.2, 0.9, 0.3];

        assert!(close(gain(0.5).matte_of_rgb(colour), 0.675));
        assert!(close(gain(1.0).matte_of_rgb(colour), 0.35));
        assert!(close(gain(2.0).matte_of_rgb(colour), 0.0));
        // A zero gain disables the key: every pixel is fully foreground.
        assert!(close(gain(0.0).matte_of_rgb(colour), 1.0));
        assert!(close(gain(0.0).matte_of_rgb([0.0, 1.0, 0.0]), 1.0));
    }

    #[test]
    fn the_screen_balance_weights_the_non_key_pair() {
        let colour = [0.2, 0.9, 0.3];

        assert!(close(balance(0.0).matte_of_rgb(colour), 0.4));
        assert!(close(balance(0.5).matte_of_rgb(colour), 0.35));
        assert!(close(balance(1.0).matte_of_rgb(colour), 0.3));
    }

    #[test]
    fn the_inner_and_outer_matte_remap_the_separation() {
        let grade = matte_range(20.0, 80.0);

        assert!(close(grade.matte_of_rgb([0.5, 0.5, 0.5]), 1.0));
        assert!(close(grade.matte_of_rgb([0.2, 0.9, 0.3]), 0.25));
        assert!(close(grade.matte_of_rgb([0.0, 1.0, 0.0]), 0.0));
    }

    #[test]
    fn an_inverted_matte_range_collapses_to_a_soft_binary() {
        let grade = matte_range(80.0, 20.0);

        assert!(close(grade.matte_of_rgb([0.5, 0.5, 0.5]), 1.0));
        assert!(close(grade.matte_of_rgb([0.0, 1.0, 0.0]), 0.0));
        assert!(close(grade.matte_of_rgb([0.2, 0.9, 0.3]), 0.0));
    }

    #[test]
    fn the_despill_strips_the_key_channel_of_the_output() {
        let colour = [0.2, 0.9, 0.3];

        assert_eq!(
            despill_of(1.0, [0.5, 0.5, 0.5]).despilled(colour),
            [0.2, 0.25, 0.3]
        );
        assert_eq!(despill_of(0.0, [0.5, 0.5, 0.5]).despilled(colour), colour);
        assert_eq!(
            despill_of(2.0, [0.5, 0.5, 0.5]).despilled(colour),
            [0.2, 0.0, 0.3]
        );
    }

    #[test]
    fn the_despill_bias_moves_the_separation_but_not_the_despill() {
        let colour = [0.2, 0.6, 0.2];
        let neutral = despill_of(1.0, [0.5, 0.5, 0.5]);
        let biased = despill_of(1.0, [0.5, 0.75, 0.5]);

        assert!(close(neutral.matte_of_rgb(colour), 0.6));
        assert!(close(biased.matte_of_rgb(colour), 0.85));
        // The bias cancels out of the despill threshold, so only the matte moves.
        let neutral_despilled = neutral.despilled(colour);
        let biased_despilled = biased.despilled(colour);
        for channel in 0..3 {
            assert!(
                close(neutral_despilled[channel], biased_despilled[channel]),
                "channel {channel} moved with the bias"
            );
            assert!(close(biased_despilled[channel], 0.2));
        }
    }

    #[test]
    fn a_preblur_softens_the_key_before_the_matte() {
        let grade = morphology(1.0, 0.0, 0.0);
        let mut pixels = vec![GREEN, GREEN, GREY, GREY, GREY];

        assert!(keylight_frame_f32(&mut pixels, 5, 1, &grade));

        // The blurred source spans the screen and the foreground, so the matte
        // ramps across the seam instead of snapping at it.
        let expected = [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0, 1.0];
        for (index, value) in expected.into_iter().enumerate() {
            assert!(
                close(pixels[index][3], value),
                "pixel {index} alpha {} is not {value}",
                pixels[index][3]
            );
        }
    }

    #[test]
    fn the_edge_shrink_erodes_the_foreground() {
        let mut pixels = vec![GREEN, GREY, GREY, GREY, GREY];

        assert!(keylight_frame_f32(
            &mut pixels,
            5,
            1,
            &morphology(0.0, 1.0, 0.0)
        ));

        // The matte is alpha in the final display, and the shrink pulls the
        // foreground one pixel in from the left edge.
        for (index, expected) in [(0, 0.0), (1, 0.0), (2, 1.0), (3, 1.0), (4, 1.0)] {
            assert!(
                close(pixels[index][3], expected),
                "pixel {index} alpha {} is not {expected}",
                pixels[index][3]
            );
        }
    }

    #[test]
    fn the_edge_feather_softens_the_matte() {
        let grade = custom(
            KEYLIGHT_DEFAULT_SCREEN_GAIN,
            KEYLIGHT_DEFAULT_SCREEN_BALANCE,
            KEYLIGHT_DEFAULT_DESPILL,
            KEYLIGHT_DEFAULT_DESPILL_BIAS,
            KEYLIGHT_DEFAULT_INNER_MATTE,
            KEYLIGHT_DEFAULT_OUTER_MATTE,
            0.0,
            0.0,
            1.0,
            KeylightDisplay::Matte,
        );
        let mut pixels = vec![GREEN, GREEN, GREY, GREY, GREY];

        assert!(keylight_frame_f32(&mut pixels, 5, 1, &grade));

        let expected = [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0, 1.0];
        for (index, value) in expected.into_iter().enumerate() {
            assert!(
                close(pixels[index][0], value),
                "pixel {index} matte {} is not {value}",
                pixels[index][0]
            );
            assert!(
                close(pixels[index][1], value),
                "the matte fills every channel"
            );
            assert!(
                close(pixels[index][2], value),
                "the matte fills every channel"
            );
            assert!(close(pixels[index][3], 1.0), "the matte view is opaque");
        }
    }

    #[test]
    fn the_four_display_modes_write_the_documented_channels() {
        let source = TINTED;
        let matte = 0.35;

        assert_eq!(
            showing(KeylightDisplay::Final).composite(source, matte),
            [0.2, 0.25, 0.3, 0.35]
        );
        assert_eq!(
            showing(KeylightDisplay::Matte).composite(source, matte),
            [0.35, 0.35, 0.35, 1.0]
        );
        assert_eq!(
            showing(KeylightDisplay::Foreground).composite(source, matte),
            [0.2, 0.25, 0.3, 1.0]
        );
        assert_eq!(
            showing(KeylightDisplay::Background).composite(source, matte),
            [0.2, 0.9, 0.3, 0.65]
        );
    }

    #[test]
    fn transparent_source_pixels_stay_bit_identical_in_every_display_mode() {
        let hole = [0.4, 0.8, 0.2, 0.0];
        let mut expected = vec![hole];

        for display in KeylightDisplay::ALL {
            let mut pixels = vec![hole];
            assert!(keylight_frame_f32(&mut pixels, 1, 1, &showing(display)));
            assert_eq!(
                bits(&pixels),
                bits(&expected),
                "the {} display must not touch a hole",
                display.key()
            );
            expected[0] = hole;
        }
        // A hole contributes no matte even though its colour would key partially.
        assert_eq!(defaults().matte_for(hole), 0.0);
        assert!(defaults().matte_of_rgb([0.4, 0.8, 0.2]) > 0.0);
    }

    #[test]
    fn identity_is_a_disabled_key() {
        let grade = custom(
            0.0,
            KEYLIGHT_DEFAULT_SCREEN_BALANCE,
            0.0,
            KEYLIGHT_DEFAULT_DESPILL_BIAS,
            KEYLIGHT_DEFAULT_INNER_MATTE,
            KEYLIGHT_DEFAULT_OUTER_MATTE,
            0.0,
            0.0,
            0.0,
            KeylightDisplay::Final,
        );
        assert!(grade.is_identity());

        let source = vec![GREEN, GREY, [0.0, 0.0, 0.0, 0.0]];
        let mut pixels = source.clone();
        assert!(keylight_frame_f32(&mut pixels, 3, 1, &grade));
        assert_eq!(bits(&pixels), bits(&source));
    }

    #[derive(Clone, Copy)]
    struct Args {
        screen_color: [f32; 3],
        screen_gain: f32,
        screen_balance: f32,
        despill: f32,
        despill_bias: [f32; 3],
        inner_matte: f32,
        outer_matte: f32,
        preblur: f32,
        edge_shrink: f32,
        edge_feather: f32,
        display: KeylightDisplay,
    }

    impl Args {
        const fn valid() -> Self {
            Self {
                screen_color: KEYLIGHT_DEFAULT_SCREEN_COLOR,
                screen_gain: KEYLIGHT_DEFAULT_SCREEN_GAIN,
                screen_balance: KEYLIGHT_DEFAULT_SCREEN_BALANCE,
                despill: KEYLIGHT_DEFAULT_DESPILL,
                despill_bias: KEYLIGHT_DEFAULT_DESPILL_BIAS,
                inner_matte: KEYLIGHT_DEFAULT_INNER_MATTE,
                outer_matte: KEYLIGHT_DEFAULT_OUTER_MATTE,
                preblur: KEYLIGHT_DEFAULT_PREBLUR_PIXELS,
                edge_shrink: KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
                edge_feather: KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS,
                display: KeylightDisplay::Final,
            }
        }

        fn grade(self) -> Result<KeylightGrade, KeylightError> {
            KeylightGrade::new(
                self.screen_color,
                self.screen_gain,
                self.screen_balance,
                self.despill,
                self.despill_bias,
                self.inner_matte,
                self.outer_matte,
                self.preblur,
                self.edge_shrink,
                self.edge_feather,
                self.display,
            )
        }
    }

    #[test]
    fn out_of_range_parameters_are_rejected() {
        assert_eq!(Args::valid().grade(), Ok(defaults()));

        let mut args = Args::valid();
        args.screen_color = [0.5, 1.5, 0.5];
        assert_eq!(args.grade(), Err(KeylightError::ScreenColor));

        let mut args = Args::valid();
        args.screen_gain = KEYLIGHT_AUTHOR_MAX_SCREEN_GAIN + 0.5;
        assert_eq!(args.grade(), Err(KeylightError::ScreenGain));

        let mut args = Args::valid();
        args.screen_balance = -0.5;
        assert_eq!(args.grade(), Err(KeylightError::ScreenBalance));

        let mut args = Args::valid();
        args.despill = KEYLIGHT_AUTHOR_MAX_DESPILL + 0.5;
        assert_eq!(args.grade(), Err(KeylightError::Despill));

        let mut args = Args::valid();
        args.despill_bias = [0.5, 1.5, 0.5];
        assert_eq!(args.grade(), Err(KeylightError::DespillBias));

        let mut args = Args::valid();
        args.inner_matte = KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT + 1.0;
        assert_eq!(args.grade(), Err(KeylightError::InnerMatte));

        let mut args = Args::valid();
        args.outer_matte = KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT + 1.0;
        assert_eq!(args.grade(), Err(KeylightError::OuterMatte));

        let mut args = Args::valid();
        args.preblur = KEYLIGHT_AUTHOR_MAX_PREBLUR_PIXELS + 1.0;
        assert_eq!(args.grade(), Err(KeylightError::Preblur));

        let mut args = Args::valid();
        args.edge_shrink = KEYLIGHT_AUTHOR_MAX_EDGE_SHRINK_PIXELS + 1.0;
        assert_eq!(args.grade(), Err(KeylightError::EdgeShrink));

        let mut args = Args::valid();
        args.edge_feather = KEYLIGHT_AUTHOR_MAX_EDGE_FEATHER_PIXELS + 1.0;
        assert_eq!(args.grade(), Err(KeylightError::EdgeFeather));

        let mut args = Args::valid();
        args.screen_color = [f32::NAN, 0.0, 0.0];
        assert_eq!(args.grade(), Err(KeylightError::ScreenColor));

        let mut args = Args::valid();
        args.screen_gain = f32::INFINITY;
        assert_eq!(args.grade(), Err(KeylightError::ScreenGain));

        let mut args = Args::valid();
        args.screen_gain = KEYLIGHT_AUTHOR_MAX_SCREEN_GAIN;
        assert!(args.grade().is_ok(), "the documented maximum is inclusive");

        let mut args = Args::valid();
        args.despill = KEYLIGHT_AUTHOR_MAX_DESPILL;
        assert!(args.grade().is_ok(), "the documented maximum is inclusive");
    }

    #[test]
    fn the_frame_kernel_rejects_an_unaddressable_geometry() {
        let grade = defaults();

        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!keylight_frame_f32(&mut empty, 0, 4, &grade));
        assert!(!keylight_frame_f32(&mut empty, 4, 0, &grade));

        let mut short = vec![GREY; 3];
        assert!(!keylight_frame_f32(&mut short, 2, 2, &grade));

        let mut overflow = vec![GREY; 4];
        assert!(!keylight_frame_f32(&mut overflow, usize::MAX, 2, &grade));
    }

    #[test]
    fn the_gpu_controls_pack_the_documented_cells() {
        let grade = custom(
            2.0,
            0.25,
            1.5,
            [0.1, 0.75, 0.3],
            20.0,
            80.0,
            3.0,
            2.0,
            4.0,
            KeylightDisplay::Background,
        );

        let (params, extra, aces, extra1) = grade.key_gpu_controls();
        assert_eq!(params, [2.0, 0.25, 0.2, 0.8]);
        assert_eq!(extra, [1.0, 1.5, 0.75, 0.0]);
        assert_eq!(aces, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(extra1, [0.0; 4]);

        let (params, extra, aces, extra1) = grade.composite_gpu_controls();
        assert_eq!(params, [3.0, 1.5, 1.0, 0.0]);
        assert_eq!(extra, [0.25, 0.0, 0.0, 0.0]);
        assert_eq!(aces, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(extra1, [0.25, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_hand_computed_two_pixel_frame_matches_the_kernel() {
        let mut pixels = vec![GREEN, TINTED];

        assert!(keylight_frame_f32(&mut pixels, 2, 1, &defaults()));

        assert_eq!(pixels[0], [0.0, 0.0, 0.0, 0.0]);
        assert!(close(pixels[1][0], 0.2));
        assert!(close(pixels[1][1], 0.25));
        assert!(close(pixels[1][2], 0.3));
        assert!(close(pixels[1][3], 0.35));
    }

    #[test]
    fn the_shrink_radius_rounds_the_authored_radius() {
        assert_eq!(morphology(0.0, 2.4, 0.0).shrink_radius(), 2);
        assert_eq!(morphology(0.0, 2.6, 0.0).shrink_radius(), 3);
        assert_eq!(morphology(0.0, 0.0, 0.0).shrink_radius(), 0);
        // The preblur and the feather stay fractional; only the min filter rounds.
        assert_eq!(morphology(3.5, 0.0, 4.5).preblur(), 3.5);
        assert_eq!(morphology(3.5, 0.0, 4.5).edge_feather(), 4.5);
    }

    #[test]
    fn the_checkpointed_kernel_propagates_a_cancellation() {
        let mut pixels = frame(8, 8, |_| TINTED);
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Err::<(), &'static str>("cancelled")
        };

        assert_eq!(
            apply_keylight_f32_controlled(&mut pixels, 8, 8, &defaults(), &mut checkpoint),
            Err("cancelled")
        );
        assert!(calls > 0);
    }

    #[test]
    fn the_kernel_accepts_a_frame_wider_than_it_is_tall() {
        let mut pixels = frame(7, 3, |_| GREY);

        assert!(keylight_frame_f32(
            &mut pixels,
            7,
            3,
            &morphology(1.0, 1.0, 1.0)
        ));
        assert_eq!(pixels.len(), 21);
        for pixel in &pixels {
            assert!(
                close(pixel[3], 1.0),
                "a grey foreground keeps full coverage"
            );
            assert!(
                close(pixel[1], 0.5),
                "the despill leaves a neutral pixel alone"
            );
        }
    }
}
