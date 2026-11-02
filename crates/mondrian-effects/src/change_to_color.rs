//! The change-to-colour effect from the built-in effect list.
//!
//! The list authors the colour to replace, the colour to move towards, a change
//! menu, a change-by menu, one amount per changed component, an edge softness and
//! a correction matte view. The effect is a point operation, so it runs on the
//! fused GPU point path as well as the CPU kernel.
//!
//! # Documented reading
//!
//! The list names the effect but does not define the maths, so this module fixes
//! one reading and documents it here. With `c` the current straight-alpha colour
//! sample, `k` the authored source colour and `softness` the authored softness in
//! `0..=1`, the retained weight is
//!
//! - `m(x) = max(max(xr, xg), max(xb, 1e-4))`, the largest channel with an
//!   explicit floor so the division below is never by zero,
//! - `ĉ = c / m(c)` and `k̂ = k / m(k)`, the chroma normalised samples,
//! - `d = ||ĉ - k̂|| / sqrt(3)`, the Euclidean distance in the normalised cube
//!   with the opposite corner at one,
//! - `w = 1 - smoothstep(0, softness + 1e-4, d)`.
//!
//! Normalising by the largest channel is what makes the match a hue family
//! match: a dark red `(0.2, 0, 0)` and an over-range red `(2.5, 0, 0)` both
//! normalise to `(1, 0, 0)` and both reach `w = 1`. The explicit `1e-4` on the
//! edge keeps the ramp strictly wider than a zero softness, so the documented
//! default is a near-hard edge rather than a degenerate one, and the two sides
//! never evaluate `0 / 0`. `smoothstep(edge0, edge1, x)` is `t * t * (3 - 2t)`
//! with `t` the clamped `(x - edge0) / (edge1 - edge0)`.
//!
//! # Component decomposition
//!
//! The changed components are decomposed in HSV, not HSL. With
//! `maximum = max(rgb)`, `minimum = min(rgb)` and `delta = maximum - minimum`:
//!
//! - `hue` is the usual maximum/minimum difference wrapped into `0..1`: a grey
//!   sample (`delta == 0`) has hue zero, a red maximal sample has
//!   `((g - b) / delta) / 6` wrapped into `0..1`, a green maximal sample has
//!   `((b - r) / delta + 2) / 6` and a blue maximal sample has
//!   `((r - g) / delta + 4) / 6`. The CPU kernel and the fused WGSL branch use
//!   the same expression so the two sides agree at the boundary.
//! - `saturation = delta / max(maximum, 1e-4)`.
//! - `lightness = maximum`.
//!
//! HSV is the reading that makes the listed effect do what its name says on a
//! video frame: changing "lightness" in HSL moves a saturated red towards white
//! long before it moves towards black, so a hue change in HSL picks up an
//! unsaturated component that the author did not ask for. HSV also decomposes
//! from a single channel maximum, so it stays defined for the extended range the
//! rest of the engine carries: a negative or over-range sample still maps to a
//! finite hue, saturation and lightness, and the hue reconstruction returns the
//! same extended range instead of folding it into the unit cube.
//!
//! # Change and change-by
//!
//! The change menu selects which components move. It is stored as the bitfield
//! `bit0 = hue`, `bit1 = lightness`, `bit2 = saturation`; the amount triple
//! `(hue, lightness, saturation)` is multiplied by the same bits. The change-by
//! menu selects the reference the target is compared with:
//!
//! - `relative` shifts by the authored offset, `dh = wrap_delta(h(to) - h(k))`,
//!   `ds = s(to) - s(k)` and `dv = v(to) - v(k)`, so the shift is the same for
//!   every matched sample.
//! - `absolute` replaces the sample, `dh = wrap_delta(h(to) - h(c))`,
//!   `ds = s(to) - s(c)` and `dv = v(to) - v(c)`, so every matched sample lands
//!   on the target components.
//!
//! `wrap_delta(x) = x - floor(x + 0.5)` keeps a hue shift on the shortest arc, so
//! a source at `0.958` and a target at `0.017` shift by `+0.058` rather than
//! `-0.941`. The shifted components are
//! `h1 = fract(h + w * amount_hue * dh)`, `s1 = s + w * amount_sat * ds` and
//! `v1 = v + w * amount_light * dv`; nothing is clamped between the three, so a
//! saturation above one keeps its full amount until the reconstruction.
//!
//! # Output
//!
//! `out = mix(hsv_to_rgb(h1, s1, v1), vec3(w), view)` with `view` the authored
//! correction matte view, and the result is clamped into `0..=1` exactly once,
//! at the end. Equality is on straight alpha: a sample with `alpha <= 0` is
//! returned bit-identically, and the source alpha is always preserved. The
//! effect never authors coverage.
//!
//! A weight of zero with no matte view is the documented no-op and returns the
//! sample bit-identically. A zero weight with a matte view is a real operation:
//! it paints the uncovered part of the matte black, so the matte view is never
//! short circuited away.
//!
//! The authored state is the identity when the matte view is off and either every
//! selected amount is zero or the two authored colours are equal in relative
//! mode, so [`ChangeToColorGrade::is_identity`] is true and the graph builder
//! elides the operation.
//!
//! Point kernels stay single threaded: the pass is bandwidth bound, and a row
//! band split measured slower than the linear walk it replaced.

/// Number of change menu options the list declares.
pub const CHANGE_TO_COLOR_CHANGE_COUNT: usize = 4;

/// Number of change-by menu options the list declares.
pub const CHANGE_TO_COLOR_BY_COUNT: usize = 2;

/// Lowest authored change amount.
pub const CHANGE_TO_COLOR_MIN_AMOUNT: f32 = 0.0;

/// Highest authored change amount (`0` to `2`, so an authored `2` doubles the
/// shift).
pub const CHANGE_TO_COLOR_MAX_AMOUNT: f32 = 2.0;

/// Lowest authored softness and correction matte view.
pub const CHANGE_TO_COLOR_MIN_UNIT: f32 = 0.0;

/// Highest authored softness and correction matte view.
pub const CHANGE_TO_COLOR_MAX_UNIT: f32 = 1.0;

/// Smallest divisor the chroma normalisation and the saturation use.
pub const CHANGE_TO_COLOR_CHROMA_FLOOR: f32 = 1.0e-4;

/// Added to the authored softness so the ramp is never degenerate.
pub const CHANGE_TO_COLOR_SOFTNESS_FLOOR: f32 = 1.0e-4;

/// `sqrt(3)`: the largest distance between two normalised chroma samples.
pub const CHANGE_TO_COLOR_SQRT_3: f32 = 1.732_050_8;

/// Which colour components the change menu moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeToColorChange {
    /// Move the hue only.
    Hue,
    /// Move the hue and the lightness.
    HueLightness,
    /// Move the hue and the saturation.
    HueSaturation,
    /// Move the hue, the lightness and the saturation.
    HueLightnessSaturation,
}

impl ChangeToColorChange {
    /// Every declared option, in list order.
    pub const ALL: [Self; CHANGE_TO_COLOR_CHANGE_COUNT] = [
        Self::Hue,
        Self::HueLightness,
        Self::HueSaturation,
        Self::HueLightnessSaturation,
    ];

    /// The stable authoring key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Hue => "hue",
            Self::HueLightness => "hue_lightness",
            Self::HueSaturation => "hue_saturation",
            Self::HueLightnessSaturation => "hue_lightness_saturation",
        }
    }

    /// The option for a stable authoring key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.as_key() == key)
    }

    /// The one-based menu position shown to the author.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// The option for a one-based menu position.
    #[must_use]
    pub fn from_menu(menu: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.as_menu() == menu)
    }

    /// The zero-based index the fused shader reads from `header.z`. Unused by the
    /// change menu, which packs into the `header.y` bitfield; the change-by menu
    /// uses the corresponding [`ChangeToColorBy::as_u32`] slot.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Hue => 0,
            Self::HueLightness => 1,
            Self::HueSaturation => 2,
            Self::HueLightnessSaturation => 3,
        }
    }

    /// The authored label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hue => "色相",
            Self::HueLightness => "色相与亮度",
            Self::HueSaturation => "色相与饱和度",
            Self::HueLightnessSaturation => "色相、亮度与饱和度",
        }
    }

    /// The `header.y` change bitfield: bit0 hue, bit1 lightness, bit2 saturation.
    #[must_use]
    pub const fn flags(self) -> u32 {
        match self {
            Self::Hue => 0b001,
            Self::HueLightness => 0b011,
            Self::HueSaturation => 0b101,
            Self::HueLightnessSaturation => 0b111,
        }
    }
}

/// Which reference the target colour is compared with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeToColorBy {
    /// Shift by the authored offset between the two colours.
    Relative,
    /// Replace the sample components with the target colour components.
    Absolute,
}

impl ChangeToColorBy {
    /// Every declared option, in list order.
    pub const ALL: [Self; CHANGE_TO_COLOR_BY_COUNT] = [Self::Relative, Self::Absolute];

    /// The stable authoring key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Relative => "relative",
            Self::Absolute => "absolute",
        }
    }

    /// The option for a stable authoring key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.as_key() == key)
    }

    /// The one-based menu position shown to the author.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// The option for a one-based menu position.
    #[must_use]
    pub fn from_menu(menu: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.as_menu() == menu)
    }

    /// The zero-based index the fused shader reads from `header.z`.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Relative => 0,
            Self::Absolute => 1,
        }
    }

    /// The authored label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Relative => "相对偏移",
            Self::Absolute => "替换为至色",
        }
    }
}

/// What can go wrong while building a [`ChangeToColorGrade`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeToColorError {
    /// The hue amount is not a finite number in `0..=2`.
    Hue,
    /// The lightness amount is not a finite number in `0..=2`.
    Lightness,
    /// The saturation amount is not a finite number in `0..=2`.
    Saturation,
    /// The softness is not a finite number in `0..=1`.
    Softness,
    /// The correction matte view is not a finite number in `0..=1`.
    View,
    /// The source colour is not a finite colour.
    From,
    /// The target colour is not a finite colour.
    To,
}

impl std::fmt::Display for ChangeToColorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hue => {
                formatter.write_str("change to color hue amount must be finite and within 0..=2")
            }
            Self::Lightness => formatter
                .write_str("change to color lightness amount must be finite and within 0..=2"),
            Self::Saturation => formatter
                .write_str("change to color saturation amount must be finite and within 0..=2"),
            Self::Softness => {
                formatter.write_str("change to color softness must be finite and within 0..=1")
            }
            Self::View => {
                formatter.write_str("change to color view must be finite and within 0..=1")
            }
            Self::From => {
                formatter.write_str("change to color source colour must be a finite colour")
            }
            Self::To => {
                formatter.write_str("change to color target colour must be a finite colour")
            }
        }
    }
}

impl std::error::Error for ChangeToColorError {}

/// One authored change-to-colour state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChangeToColorGrade {
    from: [f32; 3],
    to: [f32; 3],
    change: ChangeToColorChange,
    by: ChangeToColorBy,
    hue: f32,
    lightness: f32,
    saturation: f32,
    softness: f32,
    view: f32,
}

impl ChangeToColorGrade {
    /// Validates one authored state.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        from: [f32; 3],
        to: [f32; 3],
        change: ChangeToColorChange,
        by: ChangeToColorBy,
        hue: f32,
        lightness: f32,
        saturation: f32,
        softness: f32,
        view: f32,
    ) -> Result<Self, ChangeToColorError> {
        validate_amount(hue, ChangeToColorError::Hue)?;
        validate_amount(lightness, ChangeToColorError::Lightness)?;
        validate_amount(saturation, ChangeToColorError::Saturation)?;
        validate_unit(softness, ChangeToColorError::Softness)?;
        validate_unit(view, ChangeToColorError::View)?;
        if from.iter().any(|channel| !channel.is_finite()) {
            return Err(ChangeToColorError::From);
        }
        if to.iter().any(|channel| !channel.is_finite()) {
            return Err(ChangeToColorError::To);
        }
        Ok(Self {
            from,
            to,
            change,
            by,
            hue,
            lightness,
            saturation,
            softness,
            view,
        })
    }

    /// The authored source colour.
    #[must_use]
    pub const fn from(&self) -> [f32; 3] {
        self.from
    }

    /// The authored target colour.
    #[must_use]
    pub const fn to(&self) -> [f32; 3] {
        self.to
    }

    /// The authored change menu.
    #[must_use]
    pub const fn change(&self) -> ChangeToColorChange {
        self.change
    }

    /// The authored change-by menu.
    #[must_use]
    pub const fn change_by(&self) -> ChangeToColorBy {
        self.by
    }

    /// The authored hue amount in `0..=2`.
    #[must_use]
    pub const fn hue(&self) -> f32 {
        self.hue
    }

    /// The authored lightness amount in `0..=2`.
    #[must_use]
    pub const fn lightness(&self) -> f32 {
        self.lightness
    }

    /// The authored saturation amount in `0..=2`.
    #[must_use]
    pub const fn saturation(&self) -> f32 {
        self.saturation
    }

    /// The authored softness in `0..=1`.
    #[must_use]
    pub const fn softness(&self) -> f32 {
        self.softness
    }

    /// The authored correction matte view in `0..=1`.
    #[must_use]
    pub const fn view_matte(&self) -> f32 {
        self.view
    }

    /// Whether the authored state leaves the frame unchanged.
    ///
    /// The matte view is a real operation whenever it is on, so it can never be
    /// the identity. Otherwise the state is the identity when every selected
    /// amount is zero, or when the two colours are equal and the shift is
    /// relative: an equal source and target shift by zero for every sample.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        if self.view != CHANGE_TO_COLOR_MIN_UNIT {
            return false;
        }
        if self.by.as_u32() == ChangeToColorBy::Relative.as_u32()
            && self.from[0] == self.to[0]
            && self.from[1] == self.to[1]
            && self.from[2] == self.to[2]
        {
            return true;
        }
        let flags = self.change.flags();
        let hue = if flags & 0b001 == 0 { 0.0 } else { self.hue };
        let lightness = if flags & 0b010 == 0 {
            0.0
        } else {
            self.lightness
        };
        let saturation = if flags & 0b100 == 0 {
            0.0
        } else {
            self.saturation
        };
        hue == 0.0 && lightness == 0.0 && saturation == 0.0
    }

    /// The uniform payload for the fused point shader.
    ///
    /// The header's first slot is the shader branch, which the renderer owns, so
    /// this payload carries the change bitfield and the change-by index and lets
    /// the caller write the branch. The four float vectors are `params` (the
    /// three amounts and the softness), `color` (the target colour), `extra0`
    /// (the source colour) and `extra1` (the matte view).
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [self.hue, self.lightness, self.saturation, self.softness],
            [self.to[0], self.to[1], self.to[2], 0.0],
        )
    }

    /// The `extra0` slot: the authored source colour.
    #[must_use]
    pub fn gpu_source(&self) -> [f32; 4] {
        [self.from[0], self.from[1], self.from[2], 0.0]
    }

    /// The `extra1.x` slot: the authored correction matte view.
    #[must_use]
    pub const fn gpu_view(&self) -> f32 {
        self.view
    }

    /// The `header.y` slot: the change bitfield.
    #[must_use]
    pub const fn change_flags(&self) -> u32 {
        self.change.flags()
    }

    /// The `header.z` slot: the zero-based change-by index.
    #[must_use]
    pub const fn change_by_index(&self) -> u32 {
        self.by.as_u32()
    }

    /// Applies the effect to one straight-alpha pixel.
    ///
    /// A pixel without coverage is returned unchanged: the compositor short
    /// circuits zero coverage, so mapping one would make the CPU and the GPU
    /// disagree about a pixel that is never visible. The effect never authors
    /// coverage, so the output alpha is the source alpha.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let rgb = [pixel[0], pixel[1], pixel[2]];
        let weight = self.weight(rgb);
        if weight <= 0.0 && self.view <= CHANGE_TO_COLOR_MIN_UNIT {
            return pixel;
        }
        let corrected = self.change_rgb(rgb, weight);
        [
            (corrected[0] + (weight - corrected[0]) * self.view).clamp(0.0, 1.0),
            (corrected[1] + (weight - corrected[1]) * self.view).clamp(0.0, 1.0),
            (corrected[2] + (weight - corrected[2]) * self.view).clamp(0.0, 1.0),
            pixel[3],
        ]
    }

    /// The retained weight for one colour sample.
    fn weight(&self, rgb: [f32; 3]) -> f32 {
        let sample = normalised_chroma(rgb);
        let source = normalised_chroma(self.from);
        let red = sample[0] - source[0];
        let green = sample[1] - source[1];
        let blue = sample[2] - source[2];
        let distance = (red * red + green * green + blue * blue).sqrt() / CHANGE_TO_COLOR_SQRT_3;
        let edge = self.softness + CHANGE_TO_COLOR_SOFTNESS_FLOOR;
        1.0 - smoothstep(0.0, edge, distance)
    }

    /// The reconstructed colour for one weighted sample.
    fn change_rgb(&self, rgb: [f32; 3], weight: f32) -> [f32; 3] {
        let h0 = hue(rgb);
        let s0 = saturation(rgb);
        let v0 = lightness(rgb);
        let kh = hue(self.from);
        let ks = saturation(self.from);
        let kv = lightness(self.from);
        let th = hue(self.to);
        let ts = saturation(self.to);
        let tv = lightness(self.to);
        let absolute = matches!(self.by, ChangeToColorBy::Absolute);
        let hue_delta = if absolute {
            wrap_delta(th - h0)
        } else {
            wrap_delta(th - kh)
        };
        let saturation_delta = if absolute { ts - s0 } else { ts - ks };
        let lightness_delta = if absolute { tv - v0 } else { tv - kv };
        let flags = self.change.flags();
        let hue_amount = if flags & 0b001 == 0 {
            0.0
        } else {
            weight * self.hue
        };
        let lightness_amount = if flags & 0b010 == 0 {
            0.0
        } else {
            weight * self.lightness
        };
        let saturation_amount = if flags & 0b100 == 0 {
            0.0
        } else {
            weight * self.saturation
        };
        hsv_to_rgb(
            fract(h0 + hue_amount * hue_delta),
            s0 + saturation_amount * saturation_delta,
            v0 + lightness_amount * lightness_delta,
        )
    }
}

fn validate_amount(value: f32, error: ChangeToColorError) -> Result<(), ChangeToColorError> {
    if !value.is_finite()
        || !(CHANGE_TO_COLOR_MIN_AMOUNT..=CHANGE_TO_COLOR_MAX_AMOUNT).contains(&value)
    {
        return Err(error);
    }
    Ok(())
}

fn validate_unit(value: f32, error: ChangeToColorError) -> Result<(), ChangeToColorError> {
    if !value.is_finite() || !(CHANGE_TO_COLOR_MIN_UNIT..=CHANGE_TO_COLOR_MAX_UNIT).contains(&value)
    {
        return Err(error);
    }
    Ok(())
}

/// Applies the effect to a straight-alpha float frame.
pub fn change_to_color_frame_f32(pixels: &mut [[f32; 4]], grade: &ChangeToColorGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_change_to_color_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a straight-alpha float frame with cooperative checkpoints.
///
/// The checkpoint runs once per chunk and once at the end, so a host can stop a
/// long frame between chunks without splitting the pixel walk.
pub(crate) fn apply_change_to_color_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &ChangeToColorGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            *pixel = grade.apply(*pixel);
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Pixels between cooperative checkpoints.
const CHECKPOINT_STRIDE: usize = 4_096;

/// The largest channel with the chroma floor applied.
fn chroma_limit(rgb: [f32; 3]) -> f32 {
    rgb[0].max(rgb[1]).max(rgb[2].max(CHANGE_TO_COLOR_CHROMA_FLOOR))
}

/// The chroma normalised sample.
fn normalised_chroma(rgb: [f32; 3]) -> [f32; 3] {
    let limit = chroma_limit(rgb);
    [rgb[0] / limit, rgb[1] / limit, rgb[2] / limit]
}

/// The HSV hue of one colour sample, in `0..1`.
///
/// Copied verbatim from the leave-colour kernel so the two point effects agree
/// at the red boundary; the two modules deliberately do not share it because a
/// shared helper would tie two independently authored readings together.
fn hue(rgb: [f32; 3]) -> f32 {
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    let delta = maximum - minimum;
    if delta == 0.0 {
        return 0.0;
    }
    if maximum == rgb[0] {
        let sector = (rgb[1] - rgb[2]) / delta;
        if sector < 0.0 {
            (sector + 6.0) / 6.0
        } else {
            sector / 6.0
        }
    } else if maximum == rgb[1] {
        ((rgb[2] - rgb[0]) / delta + 2.0) / 6.0
    } else {
        ((rgb[0] - rgb[1]) / delta + 4.0) / 6.0
    }
}

/// The HSV saturation of one colour sample.
fn saturation(rgb: [f32; 3]) -> f32 {
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    (maximum - minimum) / maximum.max(CHANGE_TO_COLOR_CHROMA_FLOOR)
}

/// The HSV lightness of one colour sample.
fn lightness(rgb: [f32; 3]) -> f32 {
    rgb[0].max(rgb[1]).max(rgb[2])
}

/// The shortest signed difference between two hues, in `-0.5..=0.5`.
fn wrap_delta(value: f32) -> f32 {
    value - (value + 0.5).floor()
}

/// The positive fractional part of one number.
fn fract(value: f32) -> f32 {
    value - value.floor()
}

/// The clamped Hermite ramp between two edges.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x <= edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The HSV sector table for one sextant.
fn hsv_channel(sextant: i32, chroma: f32, second: f32) -> [f32; 3] {
    match sextant {
        0 => [chroma, second, 0.0],
        1 => [second, chroma, 0.0],
        2 => [0.0, chroma, second],
        3 => [0.0, second, chroma],
        4 => [second, 0.0, chroma],
        _ => [chroma, 0.0, second],
    }
}

/// Reconstructs one colour sample from HSV.
///
/// The sector walk is the same five comparisons the fused shader spells as
/// `select` calls, so the CPU and the GPU agree at every sector boundary.
fn hsv_to_rgb(hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    if saturation <= 0.0 {
        return [lightness, lightness, lightness];
    }
    let sextant = fract(hue) * 6.0;
    let chroma = lightness * saturation;
    let second = chroma * (1.0 - (sextant - 2.0 * (sextant * 0.5).floor() - 1.0).abs());
    let base = hsv_channel(sextant.floor() as i32, chroma, second);
    let minimum = lightness - chroma;
    [base[0] + minimum, base[1] + minimum, base[2] + minimum]
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 3] = [1.0, 0.0, 0.0];
    const GREEN: [f32; 3] = [0.0, 1.0, 0.0];
    const TARGET: [f32; 3] = [0.25, 0.5, 0.75];

    fn default_grade() -> ChangeToColorGrade {
        ChangeToColorGrade::new(
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            0.1,
            1.0,
            1.0,
            0.5,
            0.0,
        )
        .expect("the documented defaults are valid")
    }

    fn grade(
        from: [f32; 3],
        to: [f32; 3],
        change: ChangeToColorChange,
        by: ChangeToColorBy,
        softness: f32,
        view: f32,
    ) -> ChangeToColorGrade {
        ChangeToColorGrade::new(from, to, change, by, 1.0, 1.0, 1.0, softness, view)
            .expect("the test controls are valid")
    }

    fn rgb(pixel: [f32; 4]) -> [f32; 3] {
        [pixel[0], pixel[1], pixel[2]]
    }

    fn assert_close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected} but got {actual} (tolerance {tolerance})"
        );
    }

    #[test]
    fn the_default_grade_is_the_identity() {
        let grade = default_grade();
        assert!(grade.is_identity());
        assert_eq!(grade.from(), grade.to());
        assert_eq!(grade.change(), ChangeToColorChange::Hue);
        assert_eq!(grade.change_by(), ChangeToColorBy::Relative);
        for sample in [
            [0.3_f32, 0.6, 0.9, 1.0],
            [2.5, -0.4, 0.1, 0.25],
            [1.0, 1.0, 1.0, 1.0],
        ] {
            let mapped = grade.apply(sample);
            for channel in 0..3 {
                assert_close(mapped[channel], sample[channel], 1.0e-6);
            }
            assert_eq!(mapped[3], sample[3]);
        }
    }

    #[test]
    fn an_exact_match_is_replaced_by_the_target_hue() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            0.0,
            0.0,
        );
        let mapped = grade.apply([1.0, 0.0, 0.0, 1.0]);
        assert_close(mapped[0], 0.0, 1.0e-6);
        assert_close(mapped[1], 1.0, 1.0e-6);
        assert_close(mapped[2], 0.0, 1.0e-6);
        assert_eq!(mapped[3], 1.0);
        assert_close(hue(rgb(mapped)), 1.0 / 3.0, 1.0e-6);
    }

    #[test]
    fn the_weight_is_one_at_the_source_and_zero_past_the_edge() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            0.5,
            0.0,
        );
        assert_close(grade.weight(RED), 1.0, 1.0e-6);
        // `(1, 0, 1.39443)` normalises to `(0.71716, 0, 1)`, which is
        // `sqrt(0.08 + 1) / sqrt(3) = 0.6` away from the normalised source.
        let far = [1.0, 0.0, 1.394_43, 1.0];
        assert_eq!(grade.weight(rgb(far)), 0.0);
        assert_eq!(grade.apply(far).map(f32::to_bits), far.map(f32::to_bits));
    }

    #[test]
    fn the_weight_falls_off_monotonically_with_the_distance() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            0.5,
            0.0,
        );
        let mut previous = f32::INFINITY;
        for step in 0..16 {
            let green = step as f32 / 15.0;
            let weight = grade.weight([1.0, green, 0.0]);
            assert!(
                weight <= previous + 1.0e-6,
                "the weight must not rise with the distance: {weight} after {previous}"
            );
            previous = weight;
        }
        assert_eq!(previous, 0.0);
    }

    #[test]
    fn the_ramp_halves_at_half_the_edge() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            0.5,
            0.0,
        );
        // `0.4330127 / sqrt(3) = 0.25`, half of the `0.5001` edge.
        let weight = grade.weight([1.0, 0.433_012_7, 0.0]);
        assert!(
            (weight - 0.5).abs() < 1.0e-3,
            "the ramp must halve at half the edge: {weight}"
        );
    }

    #[test]
    fn hue_wraps_through_red() {
        let from = [1.0, 0.0, 0.25];
        let to = [1.0, 0.1, 0.0];
        assert_close(hue(from), 0.958_333, 1.0e-5);
        assert_close(hue(to), 0.016_667, 1.0e-5);
        assert_close(wrap_delta(hue(to) - hue(from)), 0.058_333, 1.0e-5);
        let grade = ChangeToColorGrade::new(
            from,
            to,
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            1.0,
            1.0,
            1.0,
            1.0,
            0.0,
        )
        .expect("valid grade");
        let mapped = grade.apply([from[0], from[1], from[2], 1.0]);
        let mapped_hue = hue(rgb(mapped));
        assert_close(mapped_hue, hue(to), 1.0e-4);
        assert!(
            (mapped_hue - hue(from)).abs() > 0.5,
            "the shift must cross red on the short arc: {mapped_hue}"
        );
    }

    #[test]
    fn the_change_menu_selects_the_components() {
        let source = [1.0, 0.0, 0.0, 1.0];
        for change in ChangeToColorChange::ALL {
            let grade = grade(RED, TARGET, change, ChangeToColorBy::Relative, 0.0, 0.0);
            let mapped = grade.apply(source);
            let mapped_saturation = saturation(rgb(mapped));
            let mapped_lightness = lightness(rgb(mapped));
            let moves_lightness = matches!(
                change,
                ChangeToColorChange::HueLightness | ChangeToColorChange::HueLightnessSaturation
            );
            let moves_saturation = matches!(
                change,
                ChangeToColorChange::HueSaturation | ChangeToColorChange::HueLightnessSaturation
            );
            assert_close(
                mapped_saturation,
                if moves_saturation { 0.666_667 } else { 1.0 },
                1.0e-5,
            );
            assert_close(
                mapped_lightness,
                if moves_lightness { 0.75 } else { 1.0 },
                1.0e-5,
            );
            assert_close(hue(rgb(mapped)), 0.583_333, 1.0e-5);
        }
    }

    #[test]
    fn the_change_by_menu_selects_the_reference() {
        let dark_red = [0.5, 0.0, 0.0, 1.0];
        let absolute = grade(
            RED,
            TARGET,
            ChangeToColorChange::HueLightnessSaturation,
            ChangeToColorBy::Absolute,
            0.0,
            0.0,
        );
        let replaced = absolute.apply(dark_red);
        for channel in 0..3 {
            assert_close(replaced[channel], TARGET[channel], 1.0e-6);
        }
        let relative = grade(
            RED,
            TARGET,
            ChangeToColorChange::HueLightnessSaturation,
            ChangeToColorBy::Relative,
            0.0,
            0.0,
        );
        let shifted = relative.apply(dark_red);
        // The relative shift is the offset between the two authored colours, so
        // the sample keeps its own lightness instead of landing on the target.
        assert_close(hue(rgb(shifted)), 0.583_333, 1.0e-5);
        assert_close(saturation(rgb(shifted)), 0.666_667, 1.0e-5);
        assert_close(lightness(rgb(shifted)), 0.25, 1.0e-6);
    }

    #[test]
    fn the_matte_view_shows_the_weight() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            0.5,
            1.0,
        );
        let covered = grade.apply([1.0, 0.0, 0.0, 1.0]);
        for value in &covered[..3] {
            assert_close(*value, 1.0, 1.0e-6);
        }
        let uncovered = grade.apply([0.0, 1.0, 0.0, 0.8]);
        for value in &uncovered[..3] {
            assert_close(*value, 0.0, 1.0e-6);
        }
        assert_eq!(uncovered[3], 0.8);
        let partial = grade.apply([1.0, 0.433_012_7, 0.0, 1.0]);
        for value in &partial[..3] {
            assert!(
                (value - 0.5).abs() < 1.0e-3,
                "the matte must show the partial weight: {partial:?}"
            );
        }
    }

    #[test]
    fn a_transparent_pixel_is_returned_unchanged() {
        let grade = grade(
            RED,
            TARGET,
            ChangeToColorChange::HueLightnessSaturation,
            ChangeToColorBy::Absolute,
            0.5,
            1.0,
        );
        for sample in [
            [0.0_f32, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [2.5, -0.4, 0.9, -0.5],
        ] {
            assert_eq!(
                grade.apply(sample).map(f32::to_bits),
                sample.map(f32::to_bits)
            );
        }
    }

    #[test]
    fn alpha_is_preserved_for_partial_coverage() {
        let grade = grade(
            RED,
            TARGET,
            ChangeToColorChange::HueLightnessSaturation,
            ChangeToColorBy::Absolute,
            0.0,
            0.0,
        );
        let mapped = grade.apply([0.5, 0.0, 0.0, 0.5]);
        assert_eq!(mapped[3], 0.5);
        assert_close(mapped[0], TARGET[0], 1.0e-6);
    }

    #[test]
    fn extended_range_samples_stay_finite_and_clamped() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::HueLightnessSaturation,
            ChangeToColorBy::Absolute,
            0.5,
            0.0,
        );
        for sample in [[2.5_f32, -0.4, 0.9, 1.0], [1.4, 0.2, -0.3, 1.0]] {
            let mapped = grade.apply(sample);
            for channel in 0..4 {
                assert!(
                    mapped[channel].is_finite(),
                    "the output must stay finite: {mapped:?}"
                );
            }
            for channel in 0..3 {
                assert!(
                    (0.0..=1.0).contains(&mapped[channel]),
                    "the output must be clamped: {mapped:?}"
                );
            }
            assert_eq!(mapped[3], sample[3]);
        }
    }

    #[test]
    fn far_colours_are_returned_bit_identically() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::HueLightnessSaturation,
            ChangeToColorBy::Absolute,
            0.2,
            0.0,
        );
        for sample in [
            [0.0_f32, 1.0, 0.0, 1.0],
            [2.5, -0.4, 0.9, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ] {
            assert_eq!(
                grade.apply(sample).map(f32::to_bits),
                sample.map(f32::to_bits)
            );
        }
    }

    #[test]
    fn gpu_controls_pack_every_slot() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            0.0,
            0.0,
        );
        let (params, color) = grade.gpu_controls();
        assert_eq!(params, [1.0, 1.0, 1.0, 0.0]);
        assert_eq!(color, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(grade.gpu_source(), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(grade.gpu_view(), 0.0);
        assert_eq!(grade.change_flags(), 0b001);
        assert_eq!(grade.change_by_index(), 0);
        let absolute = grade_with_by(ChangeToColorBy::Absolute);
        assert_eq!(absolute.change_by_index(), 1);
    }

    fn grade_with_by(by: ChangeToColorBy) -> ChangeToColorGrade {
        ChangeToColorGrade::new(
            RED,
            GREEN,
            ChangeToColorChange::Hue,
            by,
            1.0,
            1.0,
            1.0,
            0.0,
            0.0,
        )
        .expect("valid grade")
    }

    #[test]
    fn menus_are_one_based_in_metadata_and_zero_based_in_uniforms() {
        for (index, change) in ChangeToColorChange::ALL.into_iter().enumerate() {
            assert_eq!(change.as_u32(), index as u32);
            assert_eq!(change.as_menu(), change.as_u32() + 1);
            assert_eq!(
                ChangeToColorChange::from_menu(change.as_menu()),
                Some(change)
            );
            assert_eq!(ChangeToColorChange::from_key(change.as_key()), Some(change));
            assert!(!change.label().is_empty());
        }
        assert_eq!(ChangeToColorChange::from_menu(0), None);
        assert_eq!(
            ChangeToColorChange::from_menu(CHANGE_TO_COLOR_CHANGE_COUNT as u32 + 1),
            None
        );
        assert_eq!(ChangeToColorChange::from_key("hue_and_more"), None);
        for (index, by) in ChangeToColorBy::ALL.into_iter().enumerate() {
            assert_eq!(by.as_u32(), index as u32);
            assert_eq!(by.as_menu(), by.as_u32() + 1);
            assert_eq!(ChangeToColorBy::from_menu(by.as_menu()), Some(by));
            assert_eq!(ChangeToColorBy::from_key(by.as_key()), Some(by));
            assert!(!by.label().is_empty());
        }
        assert_eq!(ChangeToColorBy::from_menu(0), None);
        assert_eq!(
            ChangeToColorBy::from_menu(CHANGE_TO_COLOR_BY_COUNT as u32 + 1),
            None
        );
        assert_eq!(ChangeToColorChange::Hue.flags(), 0b001);
        assert_eq!(ChangeToColorChange::HueLightness.flags(), 0b011);
        assert_eq!(ChangeToColorChange::HueSaturation.flags(), 0b101);
        assert_eq!(ChangeToColorChange::HueLightnessSaturation.flags(), 0b111);
    }

    #[test]
    fn the_grade_rejects_non_finite_and_out_of_range_values() {
        let build = |from: [f32; 3],
                     to: [f32; 3],
                     hue: f32,
                     lightness: f32,
                     saturation: f32,
                     softness: f32,
                     view: f32| {
            ChangeToColorGrade::new(
                from,
                to,
                ChangeToColorChange::Hue,
                ChangeToColorBy::Relative,
                hue,
                lightness,
                saturation,
                softness,
                view,
            )
        };
        for hue in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 2.1] {
            assert_eq!(
                build(RED, GREEN, hue, 1.0, 1.0, 0.5, 0.0),
                Err(ChangeToColorError::Hue)
            );
        }
        for lightness in [f32::NAN, -0.1, 2.1] {
            assert_eq!(
                build(RED, GREEN, 0.1, lightness, 1.0, 0.5, 0.0),
                Err(ChangeToColorError::Lightness)
            );
        }
        for saturation in [f32::NAN, -0.1, 2.1] {
            assert_eq!(
                build(RED, GREEN, 0.1, 1.0, saturation, 0.5, 0.0),
                Err(ChangeToColorError::Saturation)
            );
        }
        for softness in [f32::NAN, -0.1, 1.1] {
            assert_eq!(
                build(RED, GREEN, 0.1, 1.0, 1.0, softness, 0.0),
                Err(ChangeToColorError::Softness)
            );
        }
        for view in [f32::NAN, -0.1, 1.1] {
            assert_eq!(
                build(RED, GREEN, 0.1, 1.0, 1.0, 0.5, view),
                Err(ChangeToColorError::View)
            );
        }
        assert_eq!(
            build([1.0, f32::NAN, 0.0], GREEN, 0.1, 1.0, 1.0, 0.5, 0.0),
            Err(ChangeToColorError::From)
        );
        assert_eq!(
            build(RED, [0.0, f32::INFINITY, 0.0], 0.1, 1.0, 1.0, 0.5, 0.0),
            Err(ChangeToColorError::To)
        );
        assert!(build(RED, GREEN, 0.0, 0.0, 0.0, 0.0, 0.0).is_ok());
        assert!(build(RED, GREEN, 2.0, 2.0, 2.0, 1.0, 1.0).is_ok());
        assert_eq!(
            ChangeToColorError::To.to_string(),
            "change to color target colour must be a finite colour"
        );
    }

    #[test]
    fn the_frame_kernel_matches_the_pixel_mapping() {
        let grade = grade(
            RED,
            TARGET,
            ChangeToColorChange::HueLightnessSaturation,
            ChangeToColorBy::Absolute,
            0.5,
            0.0,
        );
        let source = [
            [1.0_f32, 0.0, 0.0, 1.0],
            [0.5, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 0.5],
            [0.0, 0.0, 0.0, 0.0],
            [2.5, -0.4, 0.9, 1.0],
            [0.2, 0.4, 0.6, 1.0],
        ];
        let mut pixels = source;
        assert!(change_to_color_frame_f32(&mut pixels, &grade));
        for (mapped, sample) in pixels.iter().zip(source.iter()) {
            assert_eq!(
                mapped.map(f32::to_bits),
                grade.apply(*sample).map(f32::to_bits)
            );
        }
    }

    #[test]
    fn checkpoints_run_once_per_chunk_and_once_at_the_end() {
        let grade = grade(
            RED,
            GREEN,
            ChangeToColorChange::Hue,
            ChangeToColorBy::Relative,
            0.0,
            0.0,
        );
        let mut counted = 0;
        {
            let mut checkpoint = || {
                counted += 1;
                Ok::<(), std::convert::Infallible>(())
            };
            let mut pixels = [[1.0_f32, 0.0, 0.0, 1.0]; 4];
            apply_change_to_color_f32_controlled(&mut pixels, &grade, &mut checkpoint)
                .expect("the infallible checkpoint never fails");
        }
        assert_eq!(counted, 2);
        let mut counted = 0;
        {
            let mut checkpoint = || {
                counted += 1;
                Ok::<(), std::convert::Infallible>(())
            };
            let mut pixels = vec![[1.0_f32, 0.0, 0.0, 1.0]; CHECKPOINT_STRIDE + 1];
            apply_change_to_color_f32_controlled(&mut pixels, &grade, &mut checkpoint)
                .expect("the infallible checkpoint never fails");
        }
        assert_eq!(counted, 3);
    }
}
