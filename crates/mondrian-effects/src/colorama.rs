//! The colour-map effect from the built-in effect list.
//!
//! The list authors nineteen controls: two phase sources (the primary one and a
//! weighted secondary one), four add modes, a phase shift, a preset palette, a
//! cycle count, an interpolation switch, a modify target, an alpha switch, an
//! empty-pixel switch, a colour match (colour, tolerance, softness, mode), a
//! matte gate (layer, mode), a layer-composite switch and a blend-back amount.
//! The effect is a point operation, so it runs on the fused GPU point path as
//! well as the CPU kernel.
//!
//! # Documented reading
//!
//! Each pixel is turned into a phase in `0..1`, the phase is wrapped onto a
//! five-stop palette wheel, and the palette colour is written back through the
//! selected channels or HSL components. The exact pipeline is:
//!
//! ```text
//! p1 = phase(pixel, get_phase_from)
//! p2 = add_phase_from == zero ? skip : max(add_phase, 0) * phase(pixel, add_phase_from)
//! p  = add(p1, p2, add_mode)
//! t  = fract(p * cycle_repetitions + phase_shift_turns)
//! ```
//!
//! `add_mode` is `wrap` (`fract(p1 + p2)`), `clamp` (`min(1, p1 + p2)`),
//! `average` (`0.5 * (p1 + p2)`) or `screen` (`1 - (1 - p1) * (1 - p2)`).
//! Skipping the second phase leaves `p = p1`. `cycle_repetitions = 0` is an
//! explicit identity: the palette is never sampled.
//!
//! # Palette
//!
//! Every palette is five HSV stops at `t = 0, 0.2, 0.4, 0.6, 0.8`, with the sixth
//! segment wrapping from `0.8` back to `0`. Hue uses the shortest arc, saturation
//! and value interpolate linearly, and the segment is eased by
//! `w = f * f * (3 - 2f)`. With interpolation off the phase is quantised to
//! `floor(t * 256) / 256` before the lookup, which is the documented 256-colour
//! mode. Only [`ColoramaPalette::ALPHA_RAMP`] has a per-stop alpha (`t`); every
//! other palette is opaque.
//!
//! # Matching and masking
//!
//! `matching_mode = off` selects the whole frame. `rgb` uses the Euclidean
//! distance in the unit cube normalised by `sqrt(3)`, `hue` the circular hue
//! distance `2 * min(|dh|, 1 - |dh|)` and `chroma` the combined
//! `sqrt(hue_distance^2 + delta_saturation^2) / sqrt(2)`. A pixel whose distance
//! is inside the tolerance is selected; softness eases the edge with
//! `1 - smoothstep(tolerance, tolerance + softness, d)`. There is no epsilon in
//! the hard branch.
//!
//! The matte gate only applies when a mask layer is selected (`mask_layer > 0`)
//! and `mask_mode` is not `off`; it reads the current pixel's luma or alpha. The
//! weight is clamped to `0..=1` so a near-white luma cannot extrapolate the mix.
//!
//! # Output
//!
//! With `composite_on_layer` on, `rgb` and alpha are mixed from the source to the
//! target by `sel`; with it off the target rgb is shown directly and
//! `alpha = source_alpha * sel`. The final `mix(out, source, blend / 100)` uses
//! the explicit `x * (1 - t) + y * t` form on both the CPU and the GPU, so the
//! two documented identity configurations are bit-exact. A pixel without
//! coverage is returned unchanged unless `change_empty_pixels` is on.
//!
//! The default palette is `hue_cycle`, whose two documented anchors are black at
//! the top and cyan at `t = 0.5`; the remaining palette values are reconstructed
//! in the design document and are not externally anchored.

/// Fused-point kind the GPU compositor uses for this effect.
pub const COLORAMA_KIND: u32 = 46;

/// Number of phase sources per phase menu.
pub const COLORAMA_PHASE_SOURCE_COUNT: usize = 10;

/// Number of add modes.
pub const COLORAMA_ADD_MODE_COUNT: usize = 4;

/// Number of interpolation options.
pub const COLORAMA_INTERPOLATION_COUNT: usize = 2;

/// Number of modify options.
pub const COLORAMA_MODIFY_COUNT: usize = 14;

/// Number of match modes.
pub const COLORAMA_MATCH_MODE_COUNT: usize = 4;

/// Number of mask modes.
pub const COLORAMA_MASK_MODE_COUNT: usize = 5;

/// Number of preset palettes.
pub const COLORAMA_PALETTE_COUNT: usize = 35;

/// Number of stops in a preset palette.
pub const COLORAMA_STOP_COUNT: usize = 5;

/// Highest authored cycle count.
pub const COLORAMA_MAX_REPETITIONS: f32 = 64.0;

/// Highest authored blend percentage.
pub const COLORAMA_MAX_BLEND: f32 = 100.0;

/// Rows per checkpoint in the streaming kernel.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Default phase source.
pub const COLORAMA_DEFAULT_PHASE_SOURCE: ColoramaPhaseSource = ColoramaPhaseSource::Intensity;

/// Default phase shift in degrees.
pub const COLORAMA_DEFAULT_PHASE_SHIFT: f32 = 0.0;

/// Default palette.
pub const COLORAMA_DEFAULT_PALETTE: ColoramaPalette = ColoramaPalette::HUE_CYCLE;

/// Default cycle repetitions.
pub const COLORAMA_DEFAULT_REPETITIONS: f32 = 1.0;

/// Default modify target.
pub const COLORAMA_DEFAULT_MODIFY: ColoramaModify = ColoramaModify::All;

/// Default match colour.
pub const COLORAMA_DEFAULT_MATCH_COLOR: [f32; 3] = [1.0, 0.0, 0.0];

/// Default match tolerance.
pub const COLORAMA_DEFAULT_TOLERANCE: f32 = 0.5;

/// Default match softness.
pub const COLORAMA_DEFAULT_SOFTNESS: f32 = 0.0;

/// Default mask layer gate.
pub const COLORAMA_DEFAULT_MASK_LAYER: f32 = 0.0;

/// Default blend percentage.
pub const COLORAMA_DEFAULT_BLEND: f32 = 0.0;

/// Source a phase value is derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColoramaPhaseSource {
    /// Rec.709 luma.
    Intensity,
    /// Red channel.
    Red,
    /// Green channel.
    Green,
    /// Blue channel.
    Blue,
    /// HSI hue in turns.
    Hue,
    /// HSI lightness.
    Lightness,
    /// HSI saturation.
    Saturation,
    /// HSV value (the maximum channel).
    Value,
    /// Straight coverage.
    Alpha,
    /// Constant zero.
    Zero,
}

impl ColoramaPhaseSource {
    /// Every phase source in menu order.
    pub const ALL: [Self; COLORAMA_PHASE_SOURCE_COUNT] = [
        Self::Intensity,
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Hue,
        Self::Lightness,
        Self::Saturation,
        Self::Value,
        Self::Alpha,
        Self::Zero,
    ];

    /// The zero-based code the uniform carries.
    #[must_use]
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// The stable key stored by the effect list.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Intensity => "intensity",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Hue => "hue",
            Self::Lightness => "lightness",
            Self::Saturation => "saturation",
            Self::Value => "value",
            Self::Alpha => "alpha",
            Self::Zero => "zero",
        }
    }

    /// The Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Intensity => "强度",
            Self::Red => "红色",
            Self::Green => "绿色",
            Self::Blue => "蓝色",
            Self::Hue => "色相",
            Self::Lightness => "亮度",
            Self::Saturation => "饱和度",
            Self::Value => "值",
            Self::Alpha => "Alpha",
            Self::Zero => "零",
        }
    }

    /// Resolves a stored key, or `None` when the key is not declared.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.as_key() == key)
    }

    /// Resolves the uniform code.
    ///
    /// # Errors
    ///
    /// [`ColoramaError::InvalidPhaseSource`] when the code is not one of the ten
    /// declared sources.
    pub fn from_code(code: u32) -> Result<Self, ColoramaError> {
        Self::ALL
            .get(code as usize)
            .copied()
            .ok_or(ColoramaError::InvalidPhaseSource { code })
    }
}

/// How the two phase values are combined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColoramaAddMode {
    /// Wrap around the wheel.
    Wrap,
    /// Clamp to one.
    Clamp,
    /// Average the two phases.
    Average,
    /// Screen the two phases.
    Screen,
}

impl ColoramaAddMode {
    /// Every add mode in menu order.
    pub const ALL: [Self; COLORAMA_ADD_MODE_COUNT] =
        [Self::Wrap, Self::Clamp, Self::Average, Self::Screen];

    /// The zero-based code the uniform carries.
    #[must_use]
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// The stable key stored by the effect list.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Wrap => "wrap",
            Self::Clamp => "clamp",
            Self::Average => "average",
            Self::Screen => "screen",
        }
    }

    /// The Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Wrap => "回绕",
            Self::Clamp => "固定",
            Self::Average => "平均",
            Self::Screen => "滤色",
        }
    }

    /// Resolves a stored key, or `None` when the key is not declared.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }
}

/// Palette interpolation switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColoramaInterpolation {
    /// Blend continuously between stops.
    On,
    /// Quantise the phase to 256 steps first.
    Off,
}

impl ColoramaInterpolation {
    /// Every option in menu order.
    pub const ALL: [Self; COLORAMA_INTERPOLATION_COUNT] = [Self::On, Self::Off];

    /// The stable key stored by the effect list.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// The Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::On => "开",
            Self::Off => "关",
        }
    }

    /// Resolves a stored key, or `None` when the key is not declared.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }
}

/// Which components of the palette colour are written back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColoramaModify {
    /// All three channels.
    All,
    /// Red only.
    Red,
    /// Green only.
    Green,
    /// Blue only.
    Blue,
    /// Red and green.
    Rg,
    /// Green and blue.
    Gb,
    /// Red and blue.
    Rb,
    /// None (the source rgb is kept).
    None,
    /// Hue only.
    Hue,
    /// Lightness only.
    Lightness,
    /// Saturation only.
    Saturation,
    /// Hue and lightness.
    Hl,
    /// Lightness and saturation.
    Ls,
    /// Hue and saturation.
    Hs,
}

impl ColoramaModify {
    /// Every modify option in menu order.
    pub const ALL: [Self; COLORAMA_MODIFY_COUNT] = [
        Self::All,
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Rg,
        Self::Gb,
        Self::Rb,
        Self::None,
        Self::Hue,
        Self::Lightness,
        Self::Saturation,
        Self::Hl,
        Self::Ls,
        Self::Hs,
    ];

    /// The zero-based code the uniform carries.
    #[must_use]
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// The stable key stored by the effect list.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Rg => "rg",
            Self::Gb => "gb",
            Self::Rb => "rb",
            Self::None => "none",
            Self::Hue => "hue",
            Self::Lightness => "lightness",
            Self::Saturation => "saturation",
            Self::Hl => "hl",
            Self::Ls => "ls",
            Self::Hs => "hs",
        }
    }

    /// The Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Red => "红色",
            Self::Green => "绿色",
            Self::Blue => "蓝色",
            Self::Rg => "红绿",
            Self::Gb => "绿蓝",
            Self::Rb => "红蓝",
            Self::None => "无",
            Self::Hue => "色相",
            Self::Lightness => "亮度",
            Self::Saturation => "饱和度",
            Self::Hl => "色相与亮度",
            Self::Ls => "亮度与饱和度",
            Self::Hs => "色相与饱和度",
        }
    }

    /// Resolves a stored key, or `None` when the key is not declared.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }

    /// Resolves the uniform code.
    ///
    /// # Errors
    ///
    /// [`ColoramaError::InvalidModifyCode`] when the code is not one of the
    /// fourteen declared options.
    pub fn from_code(code: u32) -> Result<Self, ColoramaError> {
        Self::ALL
            .get(code as usize)
            .copied()
            .ok_or(ColoramaError::InvalidModifyCode { code })
    }
}

/// Distance measure used by the colour match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColoramaMatchMode {
    /// No restriction; the whole frame is selected.
    Off,
    /// Euclidean distance in the RGB cube.
    Rgb,
    /// Circular hue distance.
    Hue,
    /// Circular hue distance combined with the saturation difference.
    Chroma,
}

impl ColoramaMatchMode {
    /// Every match mode in menu order.
    pub const ALL: [Self; COLORAMA_MATCH_MODE_COUNT] =
        [Self::Off, Self::Rgb, Self::Hue, Self::Chroma];

    /// The zero-based code the uniform carries.
    #[must_use]
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// The stable key stored by the effect list.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Rgb => "rgb",
            Self::Hue => "hue",
            Self::Chroma => "chroma",
        }
    }

    /// The Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "关",
            Self::Rgb => "RGB",
            Self::Hue => "色相",
            Self::Chroma => "色度",
        }
    }

    /// Resolves a stored key, or `None` when the key is not declared.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }
}

/// Matte channel read by the mask gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColoramaMaskMode {
    /// No matte.
    Off,
    /// Rec.709 luma.
    Intensity,
    /// Straight coverage.
    Alpha,
    /// One minus the luma.
    InvertedIntensity,
    /// One minus the coverage.
    InvertedAlpha,
}

impl ColoramaMaskMode {
    /// Every mask mode in menu order.
    pub const ALL: [Self; COLORAMA_MASK_MODE_COUNT] = [
        Self::Off,
        Self::Intensity,
        Self::Alpha,
        Self::InvertedIntensity,
        Self::InvertedAlpha,
    ];

    /// The zero-based code the uniform carries.
    #[must_use]
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// The stable key stored by the effect list.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Intensity => "intensity",
            Self::Alpha => "alpha",
            Self::InvertedIntensity => "inverted_intensity",
            Self::InvertedAlpha => "inverted_alpha",
        }
    }

    /// The Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "关",
            Self::Intensity => "强度",
            Self::Alpha => "Alpha",
            Self::InvertedIntensity => "反转强度",
            Self::InvertedAlpha => "反转 Alpha",
        }
    }

    /// Resolves a stored key, or `None` when the key is not declared.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }
}

/// One of the thirty-five preset palettes.
///
/// The palette is a small integer wrapper rather than a thirty-five-arm enum so
/// the stop table can be indexed directly; the named constants keep every call
/// site readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColoramaPalette(u8);

impl ColoramaPalette {
    /// Hue cycle: black at the top, cyan at `t = 0.5`.
    pub const HUE_CYCLE: Self = Self(0);
    /// Alpha ramp: white, alpha `t`.
    pub const ALPHA_RAMP: Self = Self(1);
    /// Negative.
    pub const NEGATIVE: Self = Self(2);
    /// RGB.
    pub const RGB: Self = Self(3);
    /// Ramp red.
    pub const RAMP_RED: Self = Self(4);
    /// Ramp green.
    pub const RAMP_GREEN: Self = Self(5);
    /// Ramp blue.
    pub const RAMP_BLUE: Self = Self(6);
    /// Ramp grey.
    pub const RAMP_GREY: Self = Self(7);
    /// Saturation ramp.
    pub const SATURATION_RAMP: Self = Self(8);
    /// Solarize red.
    pub const SOLARIZE_RED: Self = Self(9);
    /// Solarize green.
    pub const SOLARIZE_GREEN: Self = Self(10);
    /// Solarize blue.
    pub const SOLARIZE_BLUE: Self = Self(11);
    /// Solarize grey.
    pub const SOLARIZE_GREY: Self = Self(12);
    /// Caribbean.
    pub const CARIBBEAN: Self = Self(13);
    /// Clay.
    pub const CLAY: Self = Self(14);
    /// Copper.
    pub const COPPER: Self = Self(15);
    /// Deep ocean.
    pub const DEEP_OCEAN: Self = Self(16);
    /// Earthenware.
    pub const EARTHENWARE: Self = Self(17);
    /// Fire.
    pub const FIRE: Self = Self(18);
    /// Fire and smoke.
    pub const FIRE_AND_SMOKE: Self = Self(19);
    /// Golden 1.
    pub const GOLDEN_1: Self = Self(20);
    /// Golden 2.
    pub const GOLDEN_2: Self = Self(21);
    /// Granite.
    pub const GRANITE: Self = Self(22);
    /// Horizon.
    pub const HORIZON: Self = Self(23);
    /// Leather.
    pub const LEATHER: Self = Self(24);
    /// Moldy.
    pub const MOLDY: Self = Self(25);
    /// Mossy.
    pub const MOSSY: Self = Self(26);
    /// Old glory.
    pub const OLD_GLORY: Self = Self(27);
    /// Rusty.
    pub const RUSTY: Self = Self(28);
    /// Sepia 1.
    pub const SEPIA_1: Self = Self(29);
    /// Sepia 2.
    pub const SEPIA_2: Self = Self(30);
    /// Skink.
    pub const SKINK: Self = Self(31);
    /// Sunset.
    pub const SUNSET: Self = Self(32);
    /// Ramp cyan (reconstructed name and values).
    pub const RAMP_CYAN: Self = Self(33);
    /// No palette: the kernel returns the source pixel.
    pub const NONE: Self = Self(34);

    /// Every palette in menu order.
    pub const ALL: [Self; COLORAMA_PALETTE_COUNT] = [
        Self(0),
        Self(1),
        Self(2),
        Self(3),
        Self(4),
        Self(5),
        Self(6),
        Self(7),
        Self(8),
        Self(9),
        Self(10),
        Self(11),
        Self(12),
        Self(13),
        Self(14),
        Self(15),
        Self(16),
        Self(17),
        Self(18),
        Self(19),
        Self(20),
        Self(21),
        Self(22),
        Self(23),
        Self(24),
        Self(25),
        Self(26),
        Self(27),
        Self(28),
        Self(29),
        Self(30),
        Self(31),
        Self(32),
        Self(33),
        Self(34),
    ];

    /// The zero-based code the uniform carries.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0 as u32
    }

    /// The one-based menu number the effect list shows.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.0 as u32 + 1
    }

    /// The stable key stored by the effect list.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self.0 {
            0 => "hue_cycle",
            1 => "alpha_ramp",
            2 => "negative",
            3 => "rgb",
            4 => "ramp_red",
            5 => "ramp_green",
            6 => "ramp_blue",
            7 => "ramp_grey",
            8 => "saturation_ramp",
            9 => "solarize_red",
            10 => "solarize_green",
            11 => "solarize_blue",
            12 => "solarize_grey",
            13 => "caribbean",
            14 => "clay",
            15 => "copper",
            16 => "deep_ocean",
            17 => "earthenware",
            18 => "fire",
            19 => "fire_and_smoke",
            20 => "golden_1",
            21 => "golden_2",
            22 => "granite",
            23 => "horizon",
            24 => "leather",
            25 => "moldy",
            26 => "mossy",
            27 => "old_glory",
            28 => "rusty",
            29 => "sepia_1",
            30 => "sepia_2",
            31 => "skink",
            32 => "sunset",
            33 => "ramp_cyan",
            _ => "none",
        }
    }

    /// The Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self.0 {
            0 => "色相循环",
            1 => "Alpha 渐变",
            2 => "反向",
            3 => "RGB",
            4 => "渐变红色",
            5 => "渐变绿色",
            6 => "渐变蓝色",
            7 => "渐变灰色",
            8 => "饱和度渐变",
            9 => "曝光过度红色",
            10 => "曝光过度绿色",
            11 => "曝光过度蓝色",
            12 => "曝光过度灰色",
            13 => "Carribean",
            14 => "粘土",
            15 => "铜色",
            16 => "深海洋",
            17 => "陶器",
            18 => "火焰",
            19 => "火和烟",
            20 => "金色 1",
            21 => "金色 2",
            22 => "花岗岩",
            23 => "地平线",
            24 => "皮革",
            25 => "发霉",
            26 => "生苔",
            27 => "星条光辉",
            28 => "锈",
            29 => "深褐色 1",
            30 => "深褐色 2",
            31 => "石龙子",
            32 => "夕阳余晖",
            33 => "渐变青色",
            _ => "无",
        }
    }

    /// Resolves a stored key, or `None` when the key is not declared.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|palette| palette.as_key() == key)
    }

    /// Resolves the uniform code.
    ///
    /// # Errors
    ///
    /// [`ColoramaError::InvalidPaletteCode`] when the code is outside `0..=34`.
    pub fn from_code(code: u32) -> Result<Self, ColoramaError> {
        Self::ALL
            .get(code as usize)
            .copied()
            .ok_or(ColoramaError::InvalidPaletteCode { code })
    }

    /// Resolves a one-based menu number.
    ///
    /// # Errors
    ///
    /// [`ColoramaError::InvalidPaletteCode`] when the menu number is outside
    /// `1..=35`; the reported code is the corresponding zero-based code.
    pub fn from_menu(menu: u32) -> Result<Self, ColoramaError> {
        match menu.checked_sub(1) {
            Some(code) => Self::from_code(code),
            None => Err(ColoramaError::InvalidPaletteCode { code: 0 }),
        }
    }

    /// The five `(h, s, v)` stops of this palette.
    #[must_use]
    pub const fn stops(self) -> [[f32; 3]; COLORAMA_STOP_COUNT] {
        PALETTE_STOPS[self.0 as usize]
    }

    /// Whether the palette's alpha is the phase instead of one.
    #[must_use]
    pub const fn is_alpha_ramp(self) -> bool {
        self.0 == Self::ALPHA_RAMP.0
    }
}

/// The five-stop `(h, s, v)` tables, in menu order.
const PALETTE_STOPS: [[[f32; 3]; COLORAMA_STOP_COUNT]; COLORAMA_PALETTE_COUNT] = [
    // 1 hue_cycle
    [
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 1.0],
        [0.5, 1.0, 1.0],
        [0.5, 1.0, 1.0],
        [0.5, 0.0, 1.0],
    ],
    // 2 alpha_ramp
    [
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
    ],
    // 3 negative
    [
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.66],
        [0.0, 0.0, 0.34],
        [0.0, 0.0, 0.17],
        [0.0, 0.0, 0.0],
    ],
    // 4 rgb
    [
        [0.0, 1.0, 1.0],
        [0.33, 1.0, 1.0],
        [0.66, 1.0, 1.0],
        [0.83, 1.0, 1.0],
        [0.92, 1.0, 1.0],
    ],
    // 5 ramp_red
    [
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 1.0],
        [0.0, 0.5, 1.0],
        [0.0, 0.25, 1.0],
        [0.0, 0.0, 1.0],
    ],
    // 6 ramp_green
    [
        [0.0, 0.0, 0.0],
        [0.33, 1.0, 1.0],
        [0.33, 0.5, 1.0],
        [0.33, 0.25, 1.0],
        [0.0, 0.0, 1.0],
    ],
    // 7 ramp_blue
    [
        [0.0, 0.0, 0.0],
        [0.66, 1.0, 1.0],
        [0.66, 0.5, 1.0],
        [0.66, 0.25, 1.0],
        [0.0, 0.0, 1.0],
    ],
    // 8 ramp_grey
    [
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.25],
        [0.0, 0.0, 0.5],
        [0.0, 0.0, 0.75],
        [0.0, 0.0, 1.0],
    ],
    // 9 saturation_ramp
    [
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 1.0],
        [0.33, 1.0, 1.0],
        [0.66, 1.0, 1.0],
        [0.5, 1.0, 1.0],
    ],
    // 10 solarize_red
    [
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 0.35],
        [0.0, 0.6, 0.15],
    ],
    // 11 solarize_green
    [
        [0.0, 0.0, 0.0],
        [0.33, 1.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.33, 1.0, 0.35],
        [0.33, 0.6, 0.15],
    ],
    // 12 solarize_blue
    [
        [0.0, 0.0, 0.0],
        [0.66, 1.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.66, 1.0, 0.35],
        [0.66, 0.6, 0.15],
    ],
    // 13 solarize_grey
    [
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.6],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.2],
        [0.0, 0.0, 0.45],
    ],
    // 14 caribbean
    [
        [0.5, 1.0, 0.45],
        [0.46, 1.0, 0.9],
        [0.55, 0.85, 1.0],
        [0.52, 1.0, 0.2],
        [0.5, 1.0, 0.6],
    ],
    // 15 clay
    [
        [0.06, 0.6, 0.55],
        [0.08, 0.45, 0.78],
        [0.04, 0.7, 0.35],
        [0.09, 0.35, 0.9],
        [0.07, 0.5, 0.66],
    ],
    // 16 copper
    [
        [0.06, 0.78, 0.6],
        [0.05, 0.6, 0.85],
        [0.08, 0.85, 0.4],
        [0.03, 0.5, 0.95],
        [0.07, 0.7, 0.7],
    ],
    // 17 deep_ocean
    [
        [0.58, 1.0, 0.15],
        [0.62, 1.0, 0.45],
        [0.55, 0.85, 0.75],
        [0.66, 1.0, 0.3],
        [0.6, 1.0, 0.55],
    ],
    // 18 earthenware
    [
        [0.09, 0.55, 0.5],
        [0.11, 0.4, 0.7],
        [0.07, 0.65, 0.35],
        [0.13, 0.3, 0.85],
        [0.1, 0.45, 0.62],
    ],
    // 19 fire
    [
        [0.0, 0.9, 0.5],
        [0.08, 1.0, 1.0],
        [0.02, 0.95, 0.75],
        [0.12, 1.0, 0.35],
        [0.0, 1.0, 0.7],
    ],
    // 20 fire_and_smoke
    [
        [0.0, 0.9, 0.4],
        [0.1, 1.0, 1.0],
        [0.0, 0.0, 0.62],
        [0.0, 0.6, 0.15],
        [0.0, 0.85, 0.3],
    ],
    // 21 golden_1
    [
        [0.12, 0.8, 0.6],
        [0.13, 1.0, 1.0],
        [0.1, 0.6, 0.85],
        [0.15, 0.4, 0.95],
        [0.1, 0.7, 0.78],
    ],
    // 22 golden_2
    [
        [0.09, 0.7, 0.55],
        [0.11, 1.0, 0.95],
        [0.08, 0.5, 0.8],
        [0.13, 0.3, 0.9],
        [0.1, 0.6, 0.72],
    ],
    // 23 granite
    [
        [0.58, 0.08, 0.45],
        [0.0, 0.05, 0.75],
        [0.6, 0.12, 0.3],
        [0.08, 0.06, 0.6],
        [0.3, 0.07, 0.5],
    ],
    // 24 horizon
    [
        [0.55, 0.7, 0.6],
        [0.08, 0.35, 0.95],
        [0.02, 0.6, 0.85],
        [0.6, 0.5, 0.25],
        [0.55, 0.6, 0.5],
    ],
    // 25 leather
    [
        [0.07, 0.6, 0.35],
        [0.06, 0.5, 0.55],
        [0.08, 0.7, 0.25],
        [0.05, 0.4, 0.65],
        [0.07, 0.55, 0.45],
    ],
    // 26 moldy
    [
        [0.22, 0.5, 0.35],
        [0.18, 0.4, 0.55],
        [0.28, 0.6, 0.25],
        [0.2, 0.3, 0.45],
        [0.23, 0.45, 0.4],
    ],
    // 27 mossy
    [
        [0.3, 0.6, 0.25],
        [0.25, 0.5, 0.45],
        [0.35, 0.7, 0.18],
        [0.28, 0.4, 0.35],
        [0.31, 0.55, 0.3],
    ],
    // 28 old_glory
    [
        [0.0, 1.0, 0.8],
        [0.0, 0.0, 1.0],
        [0.6, 1.0, 0.5],
        [0.0, 0.0, 0.2],
        [0.0, 0.4, 0.55],
    ],
    // 29 rusty
    [
        [0.04, 0.65, 0.5],
        [0.06, 0.8, 0.35],
        [0.03, 0.5, 0.65],
        [0.08, 0.7, 0.25],
        [0.05, 0.6, 0.45],
    ],
    // 30 sepia_1
    [
        [0.09, 0.35, 0.45],
        [0.08, 0.25, 0.65],
        [0.1, 0.45, 0.3],
        [0.07, 0.15, 0.8],
        [0.08, 0.3, 0.55],
    ],
    // 31 sepia_2
    [
        [0.08, 0.5, 0.4],
        [0.07, 0.35, 0.6],
        [0.09, 0.6, 0.28],
        [0.06, 0.2, 0.75],
        [0.08, 0.42, 0.5],
    ],
    // 32 skink
    [
        [0.45, 0.55, 0.5],
        [0.5, 0.4, 0.75],
        [0.4, 0.7, 0.3],
        [0.55, 0.3, 0.9],
        [0.47, 0.5, 0.6],
    ],
    // 33 sunset
    [
        [0.95, 0.7, 0.85],
        [0.05, 1.0, 0.95],
        [0.02, 0.85, 0.55],
        [0.75, 0.6, 0.35],
        [0.97, 0.7, 0.7],
    ],
    // 34 ramp_cyan
    [
        [0.0, 0.0, 0.0],
        [0.5, 1.0, 1.0],
        [0.5, 0.5, 1.0],
        [0.5, 0.25, 1.0],
        [0.0, 0.0, 1.0],
    ],
    // 35 none
    [
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
    ],
];

/// Why a colour-map grade could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColoramaError {
    /// A numeric control is not finite.
    NonFiniteParameter {
        /// The offending parameter key.
        parameter: &'static str,
    },
    /// The cycle count is negative.
    NegativeRepetitions {
        /// The authored value.
        value: f32,
    },
    /// A palette code is not one of the thirty-five presets.
    InvalidPaletteCode {
        /// The offending code.
        code: u32,
    },
    /// A phase-source code is not one of the ten sources.
    InvalidPhaseSource {
        /// The offending code.
        code: u32,
    },
    /// A modify code is not one of the fourteen options.
    InvalidModifyCode {
        /// The offending code.
        code: u32,
    },
}

impl std::fmt::Display for ColoramaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteParameter { parameter } => {
                write!(formatter, "colorama parameter {parameter} must be finite")
            }
            Self::NegativeRepetitions { value } => write!(
                formatter,
                "colorama cycle repetitions must be >= 0, got {value}"
            ),
            Self::InvalidPaletteCode { code } => {
                write!(
                    formatter,
                    "colorama palette code {code} is not a declared preset"
                )
            }
            Self::InvalidPhaseSource { code } => {
                write!(
                    formatter,
                    "colorama phase source code {code} is not a declared source"
                )
            }
            Self::InvalidModifyCode { code } => {
                write!(
                    formatter,
                    "colorama modify code {code} is not a declared option"
                )
            }
        }
    }
}

impl std::error::Error for ColoramaError {}

/// The nineteen authored controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColoramaSettings {
    /// Primary phase source.
    pub get_phase_from: ColoramaPhaseSource,
    /// Weight of the secondary phase.
    pub add_phase: f32,
    /// Secondary phase source.
    pub add_phase_from: ColoramaPhaseSource,
    /// How the two phases are combined.
    pub add_mode: ColoramaAddMode,
    /// Phase shift in degrees.
    pub phase_shift: f32,
    /// Palette preset.
    pub preset_palette: ColoramaPalette,
    /// Number of turns over the source range.
    pub cycle_repetitions: f32,
    /// Whether the palette is interpolated continuously.
    pub interpolate: bool,
    /// Which components are written back.
    pub modify: ColoramaModify,
    /// Whether the palette alpha is written back.
    pub modify_alpha: f32,
    /// Whether transparent pixels are painted.
    pub change_empty_pixels: f32,
    /// Colour the match compares against.
    pub matching_color: [f32; 3],
    /// Match tolerance.
    pub matching_tolerance: f32,
    /// Match edge softness.
    pub matching_softness: f32,
    /// Match distance measure.
    pub matching_mode: ColoramaMatchMode,
    /// Mask layer gate; only `> 0` matters.
    pub mask_layer: f32,
    /// Matte channel.
    pub mask_mode: ColoramaMaskMode,
    /// Whether the target is mixed over the source.
    pub composite_on_layer: f32,
    /// Blend-back percentage.
    pub blend_with_original: f32,
}

impl Default for ColoramaSettings {
    fn default() -> Self {
        Self {
            get_phase_from: COLORAMA_DEFAULT_PHASE_SOURCE,
            add_phase: 0.0,
            add_phase_from: COLORAMA_DEFAULT_PHASE_SOURCE,
            add_mode: ColoramaAddMode::Wrap,
            phase_shift: COLORAMA_DEFAULT_PHASE_SHIFT,
            preset_palette: COLORAMA_DEFAULT_PALETTE,
            cycle_repetitions: COLORAMA_DEFAULT_REPETITIONS,
            interpolate: true,
            modify: COLORAMA_DEFAULT_MODIFY,
            modify_alpha: 1.0,
            change_empty_pixels: 0.0,
            matching_color: COLORAMA_DEFAULT_MATCH_COLOR,
            matching_tolerance: COLORAMA_DEFAULT_TOLERANCE,
            matching_softness: COLORAMA_DEFAULT_SOFTNESS,
            matching_mode: ColoramaMatchMode::Off,
            mask_layer: COLORAMA_DEFAULT_MASK_LAYER,
            mask_mode: ColoramaMaskMode::Off,
            composite_on_layer: 1.0,
            blend_with_original: COLORAMA_DEFAULT_BLEND,
        }
    }
}

impl ColoramaSettings {
    /// The phase shift folded into turns.
    #[must_use]
    pub fn phase_shift_turns(&self) -> f32 {
        self.phase_shift / 360.0
    }
}

/// The seven uniform cells the fused point shader receives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColoramaGpuControls {
    /// Effect kind and the packed enums plus flags.
    pub header: [u32; 4],
    /// Cycle count, shift in turns, secondary weight and blend.
    pub params: [f32; 4],
    /// Match rgb and the fourth palette stop's hue.
    pub color: [f32; 4],
    /// Softness, tolerance and the fourth stop's saturation and value.
    pub extra0: [f32; 4],
    /// Stops zero and one.
    pub extra1: [f32; 4],
    /// Stops one and two.
    pub extra2: [f32; 4],
    /// Stops two and three.
    pub extra3: [f32; 4],
}

/// A resolved colour-map grade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColoramaGrade {
    get_phase_from: ColoramaPhaseSource,
    add_phase: f32,
    add_phase_from: ColoramaPhaseSource,
    add_mode: ColoramaAddMode,
    phase_shift: f32,
    preset_palette: ColoramaPalette,
    cycle_repetitions: f32,
    interpolate: bool,
    modify: ColoramaModify,
    modify_alpha: f32,
    change_empty_pixels: f32,
    matching_color: [f32; 3],
    matching_tolerance: f32,
    matching_softness: f32,
    matching_mode: ColoramaMatchMode,
    mask_layer: f32,
    mask_mode: ColoramaMaskMode,
    composite_on_layer: f32,
    blend_with_original: f32,
}

impl ColoramaGrade {
    /// Resolves the authored controls.
    ///
    /// # Errors
    ///
    /// [`ColoramaError::NonFiniteParameter`] when any numeric control (including
    /// the match colour) is not finite, and
    /// [`ColoramaError::NegativeRepetitions`] when the cycle count is negative.
    /// Ranges are enforced by the property schema; this constructor only rejects
    /// values a kernel could not evaluate.
    pub fn new(settings: ColoramaSettings) -> Result<Self, ColoramaError> {
        for (value, parameter) in [
            (settings.add_phase, "add_phase"),
            (settings.phase_shift, "phase_shift"),
            (settings.cycle_repetitions, "cycle_repetitions"),
            (settings.modify_alpha, "modify_alpha"),
            (settings.change_empty_pixels, "change_empty_pixels"),
            (settings.matching_tolerance, "matching_tolerance"),
            (settings.matching_softness, "matching_softness"),
            (settings.mask_layer, "mask_layer"),
            (settings.composite_on_layer, "composite_on_layer"),
            (settings.blend_with_original, "blend_with_original"),
        ] {
            if !value.is_finite() {
                return Err(ColoramaError::NonFiniteParameter { parameter });
            }
        }
        if settings.matching_color.iter().any(|channel| !channel.is_finite()) {
            return Err(ColoramaError::NonFiniteParameter { parameter: "matching_color" });
        }
        if settings.cycle_repetitions < 0.0 {
            return Err(ColoramaError::NegativeRepetitions { value: settings.cycle_repetitions });
        }
        Ok(Self {
            get_phase_from: settings.get_phase_from,
            add_phase: settings.add_phase,
            add_phase_from: settings.add_phase_from,
            add_mode: settings.add_mode,
            phase_shift: settings.phase_shift,
            preset_palette: settings.preset_palette,
            cycle_repetitions: settings.cycle_repetitions,
            interpolate: settings.interpolate,
            modify: settings.modify,
            modify_alpha: settings.modify_alpha,
            change_empty_pixels: settings.change_empty_pixels,
            matching_color: settings.matching_color,
            matching_tolerance: settings.matching_tolerance,
            matching_softness: settings.matching_softness,
            matching_mode: settings.matching_mode,
            mask_layer: settings.mask_layer,
            mask_mode: settings.mask_mode,
            composite_on_layer: settings.composite_on_layer,
            blend_with_original: settings.blend_with_original,
        })
    }

    /// The primary phase source.
    #[must_use]
    pub const fn get_phase_from(&self) -> ColoramaPhaseSource {
        self.get_phase_from
    }

    /// The secondary phase weight.
    #[must_use]
    pub const fn add_phase(&self) -> f32 {
        self.add_phase
    }

    /// The secondary phase source.
    #[must_use]
    pub const fn add_phase_from(&self) -> ColoramaPhaseSource {
        self.add_phase_from
    }

    /// The add mode.
    #[must_use]
    pub const fn add_mode(&self) -> ColoramaAddMode {
        self.add_mode
    }

    /// The phase shift in degrees.
    #[must_use]
    pub const fn phase_shift(&self) -> f32 {
        self.phase_shift
    }

    /// The palette.
    #[must_use]
    pub const fn preset_palette(&self) -> ColoramaPalette {
        self.preset_palette
    }

    /// The cycle count.
    #[must_use]
    pub const fn cycle_repetitions(&self) -> f32 {
        self.cycle_repetitions
    }

    /// Whether the palette is interpolated.
    #[must_use]
    pub const fn interpolate(&self) -> bool {
        self.interpolate
    }

    /// The modify target.
    #[must_use]
    pub const fn modify(&self) -> ColoramaModify {
        self.modify
    }

    /// The authored alpha switch in `0..=1`.
    #[must_use]
    pub const fn modify_alpha(&self) -> f32 {
        self.modify_alpha
    }

    /// The authored empty-pixel switch in `0..=1`.
    #[must_use]
    pub const fn change_empty_pixels(&self) -> f32 {
        self.change_empty_pixels
    }

    /// The match colour.
    #[must_use]
    pub const fn matching_color(&self) -> [f32; 3] {
        self.matching_color
    }

    /// The match tolerance.
    #[must_use]
    pub const fn matching_tolerance(&self) -> f32 {
        self.matching_tolerance
    }

    /// The match softness.
    #[must_use]
    pub const fn matching_softness(&self) -> f32 {
        self.matching_softness
    }

    /// The match mode.
    #[must_use]
    pub const fn matching_mode(&self) -> ColoramaMatchMode {
        self.matching_mode
    }

    /// The mask layer gate.
    #[must_use]
    pub const fn mask_layer(&self) -> f32 {
        self.mask_layer
    }

    /// The mask mode.
    #[must_use]
    pub const fn mask_mode(&self) -> ColoramaMaskMode {
        self.mask_mode
    }

    /// The authored layer-composite switch in `0..=1`.
    #[must_use]
    pub const fn composite_on_layer(&self) -> f32 {
        self.composite_on_layer
    }

    /// The authored blend percentage.
    #[must_use]
    pub const fn blend_with_original(&self) -> f32 {
        self.blend_with_original
    }

    /// Whether the alpha switch is on.
    #[must_use]
    pub const fn writes_alpha(&self) -> bool {
        self.modify_alpha >= 0.5
    }

    /// Whether transparent pixels are painted.
    #[must_use]
    pub const fn paints_empty_pixels(&self) -> bool {
        self.change_empty_pixels >= 0.5
    }

    /// Whether the target is mixed over the source rather than shown alone.
    #[must_use]
    pub const fn composites_on_layer(&self) -> bool {
        self.composite_on_layer >= 0.5
    }

    /// Whether a mask layer is selected and the matte is not off.
    #[must_use]
    pub fn mask_enabled(&self) -> bool {
        self.mask_mode != ColoramaMaskMode::Off && self.mask_layer > 0.0
    }

    /// Whether this grade is the exact identity.
    ///
    /// The four documented configurations short-circuit: zero repetitions, a
    /// hundred percent blend, the `none` palette, and `modify = none` with the
    /// alpha switch off while the target is composited over the source.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        if self.cycle_repetitions == 0.0
            || self.blend_with_original >= COLORAMA_MAX_BLEND
            || self.preset_palette == ColoramaPalette::NONE
        {
            return true;
        }
        self.modify == ColoramaModify::None && !self.writes_alpha() && self.composites_on_layer()
    }

    /// The seven uniform cells, packed exactly as the design's lane table.
    #[must_use]
    pub fn gpu_controls(&self) -> ColoramaGpuControls {
        let stops = self.preset_palette.stops();
        let flags = u32::from(self.composites_on_layer())
            | (u32::from(self.writes_alpha()) << 1)
            | (u32::from(self.paints_empty_pixels()) << 2)
            | (u32::from(self.interpolate) << 3)
            | (u32::from(self.mask_enabled()) << 4);
        ColoramaGpuControls {
            header: [
                COLORAMA_KIND,
                self.get_phase_from.index()
                    | (self.add_phase_from.index() << 8)
                    | (self.add_mode.index() << 16)
                    | (self.matching_mode.index() << 24),
                self.modify.index()
                    | (self.mask_mode.index() << 8)
                    | (self.preset_palette.index() << 16)
                    | (flags << 24),
                0,
            ],
            params: [
                self.cycle_repetitions,
                self.phase_shift / 360.0,
                self.add_phase,
                self.blend_with_original / 100.0,
            ],
            color: [
                self.matching_color[0],
                self.matching_color[1],
                self.matching_color[2],
                stops[4][0],
            ],
            extra0: [
                self.matching_softness,
                self.matching_tolerance,
                stops[4][1],
                stops[4][2],
            ],
            extra1: [stops[0][0], stops[0][1], stops[0][2], stops[1][0]],
            extra2: [stops[1][1], stops[1][2], stops[2][0], stops[2][1]],
            extra3: [stops[2][2], stops[3][0], stops[3][1], stops[3][2]],
        }
    }

    /// Applies the grade to one straight-alpha pixel.
    #[must_use]
    pub fn apply_pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        if self.cycle_repetitions == 0.0 || self.preset_palette == ColoramaPalette::NONE {
            return pixel;
        }
        if self.modify == ColoramaModify::None && !self.writes_alpha() && self.composites_on_layer()
        {
            return pixel;
        }
        if pixel[3] <= 0.0 && !self.paints_empty_pixels() {
            return pixel;
        }

        let p1 = phase_of(pixel, self.get_phase_from);
        let p = if self.add_phase_from == ColoramaPhaseSource::Zero {
            p1
        } else {
            let p2 = self.add_phase.max(0.0) * phase_of(pixel, self.add_phase_from);
            add_phases(p1, p2, self.add_mode)
        };
        let mut t = fract(p * self.cycle_repetitions + self.phase_shift / 360.0);
        if !self.interpolate {
            t = (t * 256.0).floor() / 256.0;
        }
        let palette = palette_colour(self.preset_palette, t);

        let mut selection = self.match_weight(pixel);
        if self.mask_enabled() {
            selection *= self.matte(pixel);
        }
        selection = selection.clamp(0.0, 1.0);

        let target = modify_colour(pixel, palette, self.modify);
        let target_alpha = if self.writes_alpha() {
            palette[3]
        } else {
            pixel[3]
        };

        let (mut rgb, mut alpha) = if self.composites_on_layer() {
            (
                [
                    mix(pixel[0], target[0], selection),
                    mix(pixel[1], target[1], selection),
                    mix(pixel[2], target[2], selection),
                ],
                mix(pixel[3], target_alpha, selection),
            )
        } else {
            (target, pixel[3] * selection)
        };

        let blend = self.blend_with_original / 100.0;
        rgb = [
            mix(rgb[0], pixel[0], blend),
            mix(rgb[1], pixel[1], blend),
            mix(rgb[2], pixel[2], blend),
        ];
        alpha = mix(alpha, pixel[3], blend);
        [rgb[0], rgb[1], rgb[2], alpha]
    }

    /// Applies the grade to a whole frame, returning whether any pixel changed.
    pub fn apply_to_frame(&self, pixels: &mut [[f32; 4]]) -> bool {
        let mut changed = false;
        for pixel in pixels.iter_mut() {
            let source = *pixel;
            let result = self.apply_pixel(source);
            if result.map(f32::to_bits) != source.map(f32::to_bits) {
                changed = true;
            }
            *pixel = result;
        }
        changed
    }

    fn match_weight(&self, pixel: [f32; 4]) -> f32 {
        if self.matching_mode == ColoramaMatchMode::Off {
            return 1.0;
        }
        let distance = match self.matching_mode {
            ColoramaMatchMode::Off => 0.0,
            ColoramaMatchMode::Rgb => {
                let dr = pixel[0] - self.matching_color[0];
                let dg = pixel[1] - self.matching_color[1];
                let db = pixel[2] - self.matching_color[2];
                ((dr * dr + dg * dg + db * db).sqrt() / 3.0_f32.sqrt()).min(1.0)
            }
            ColoramaMatchMode::Hue => {
                let distance = (rgb_hue(pixel) - rgb_hue_of(self.matching_color)).abs();
                circular_hue_distance(distance)
            }
            ColoramaMatchMode::Chroma => {
                let distance = (rgb_hue(pixel) - rgb_hue_of(self.matching_color)).abs();
                let hue_distance = circular_hue_distance(distance);
                let saturation_distance =
                    (rgb_saturation(pixel) - rgb_saturation_of(self.matching_color)).abs();
                ((hue_distance * hue_distance + saturation_distance * saturation_distance).sqrt()
                    / 2.0_f32.sqrt())
                .min(1.0)
            }
        };
        if self.matching_softness <= 0.0 {
            if distance <= self.matching_tolerance {
                1.0
            } else {
                0.0
            }
        } else {
            1.0 - smoothstep(
                self.matching_tolerance,
                self.matching_tolerance + self.matching_softness,
                distance,
            )
        }
    }

    fn matte(&self, pixel: [f32; 4]) -> f32 {
        match self.mask_mode {
            ColoramaMaskMode::Off => 1.0,
            ColoramaMaskMode::Intensity => rec709_luma(pixel),
            ColoramaMaskMode::Alpha => pixel[3],
            ColoramaMaskMode::InvertedIntensity => 1.0 - rec709_luma(pixel),
            ColoramaMaskMode::InvertedAlpha => 1.0 - pixel[3],
        }
        .clamp(0.0, 1.0)
    }
}

/// Streaming kernel used by the CPU execution path.
///
/// # Errors
///
/// Forwards the checkpoint's error when it fires between chunks.
pub(crate) fn apply_colorama_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &ColoramaGrade,
    mut checkpoint: impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let mut changed = false;
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk {
            let source = *pixel;
            let result = grade.apply_pixel(source);
            if result.map(f32::to_bits) != source.map(f32::to_bits) {
                changed = true;
            }
            *pixel = result;
        }
    }
    checkpoint()?;
    Ok(changed)
}

/// Applies a resolved grade to a whole frame.
pub fn colorama_frame_f32(pixels: &mut [[f32; 4]], grade: &ColoramaGrade) -> bool {
    grade.apply_to_frame(pixels)
}

fn fract(value: f32) -> f32 {
    value - value.floor()
}

fn mix(from: f32, to: f32, weight: f32) -> f32 {
    from * (1.0 - weight) + to * weight
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn rec709_luma(pixel: [f32; 4]) -> f32 {
    0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2]
}

fn circular_hue_distance(distance: f32) -> f32 {
    2.0 * distance.min(1.0 - distance)
}

fn phase_of(pixel: [f32; 4], source: ColoramaPhaseSource) -> f32 {
    match source {
        ColoramaPhaseSource::Intensity => rec709_luma(pixel),
        ColoramaPhaseSource::Red => pixel[0],
        ColoramaPhaseSource::Green => pixel[1],
        ColoramaPhaseSource::Blue => pixel[2],
        ColoramaPhaseSource::Hue => rgb_hue(pixel),
        ColoramaPhaseSource::Lightness => rgb_lightness(pixel),
        ColoramaPhaseSource::Saturation => rgb_saturation(pixel),
        ColoramaPhaseSource::Value => pixel[0].max(pixel[1]).max(pixel[2]),
        ColoramaPhaseSource::Alpha => pixel[3].clamp(0.0, 1.0),
        ColoramaPhaseSource::Zero => 0.0,
    }
}

fn add_phases(p1: f32, p2: f32, mode: ColoramaAddMode) -> f32 {
    match mode {
        ColoramaAddMode::Wrap => fract(p1 + p2),
        ColoramaAddMode::Clamp => (p1 + p2).min(1.0),
        ColoramaAddMode::Average => 0.5 * (p1 + p2),
        ColoramaAddMode::Screen => 1.0 - (1.0 - p1) * (1.0 - p2),
    }
}

/// The maximum, middle and minimum channel addresses of the hue sector.
fn rgb_hue(pixel: [f32; 4]) -> f32 {
    rgb_hue_of([pixel[0], pixel[1], pixel[2]])
}

fn rgb_hue_of(rgb: [f32; 3]) -> f32 {
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

fn rgb_saturation(pixel: [f32; 4]) -> f32 {
    rgb_saturation_of([pixel[0], pixel[1], pixel[2]])
}

fn rgb_saturation_of(rgb: [f32; 3]) -> f32 {
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    if maximum > 0.0 {
        1.0 - minimum / maximum
    } else {
        0.0
    }
}

fn rgb_lightness(pixel: [f32; 4]) -> f32 {
    rgb_lightness_of([pixel[0], pixel[1], pixel[2]])
}

fn rgb_lightness_of(rgb: [f32; 3]) -> f32 {
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    (maximum + minimum) / 2.0
}

/// Writes the selected components of `palette` into `pixel`.
fn modify_colour(pixel: [f32; 4], palette: [f32; 4], modify: ColoramaModify) -> [f32; 3] {
    match modify {
        ColoramaModify::All => [palette[0], palette[1], palette[2]],
        ColoramaModify::Red => [palette[0], pixel[1], pixel[2]],
        ColoramaModify::Green => [pixel[0], palette[1], pixel[2]],
        ColoramaModify::Blue => [pixel[0], pixel[1], palette[2]],
        ColoramaModify::Rg => [palette[0], palette[1], pixel[2]],
        ColoramaModify::Gb => [pixel[0], palette[1], palette[2]],
        ColoramaModify::Rb => [palette[0], pixel[1], palette[2]],
        ColoramaModify::None => [pixel[0], pixel[1], pixel[2]],
        ColoramaModify::Hue => hue_lightness_saturation_to_rgb(
            rgb_hue_of([palette[0], palette[1], palette[2]]),
            rgb_saturation(pixel),
            rgb_lightness(pixel),
        ),
        ColoramaModify::Lightness => hue_lightness_saturation_to_rgb(
            rgb_hue(pixel),
            rgb_saturation(pixel),
            rgb_lightness_of([palette[0], palette[1], palette[2]]),
        ),
        ColoramaModify::Saturation => hue_lightness_saturation_to_rgb(
            rgb_hue(pixel),
            rgb_saturation_of([palette[0], palette[1], palette[2]]),
            rgb_lightness(pixel),
        ),
        ColoramaModify::Hl => hue_lightness_saturation_to_rgb(
            rgb_hue_of([palette[0], palette[1], palette[2]]),
            rgb_saturation(pixel),
            rgb_lightness_of([palette[0], palette[1], palette[2]]),
        ),
        ColoramaModify::Ls => hue_lightness_saturation_to_rgb(
            rgb_hue(pixel),
            rgb_saturation_of([palette[0], palette[1], palette[2]]),
            rgb_lightness_of([palette[0], palette[1], palette[2]]),
        ),
        ColoramaModify::Hs => hue_lightness_saturation_to_rgb(
            rgb_hue_of([palette[0], palette[1], palette[2]]),
            rgb_saturation_of([palette[0], palette[1], palette[2]]),
            rgb_lightness(pixel),
        ),
    }
}

/// Evaluates a palette at `t`, returning straight-alpha `rgba`.
fn palette_colour(palette: ColoramaPalette, t: f32) -> [f32; 4] {
    let stops = palette.stops();
    let x = fract(t) * 5.0;
    let index = x.floor();
    let f = x - index;
    let first = stops[(index as usize).min(COLORAMA_STOP_COUNT - 1)];
    let second = stops[((index as usize) + 1) % COLORAMA_STOP_COUNT];
    let weight = f * f * (3.0 - 2.0 * f);
    let hue = fract(first[0] + shortest_hue_delta(first[0], second[0]) * weight);
    let saturation = mix(first[1], second[1], weight);
    let value = mix(first[2], second[2], weight);
    let rgb = hsv_to_rgb(hue, saturation, value);
    let alpha = if palette.is_alpha_ramp() { t } else { 1.0 };
    [rgb[0], rgb[1], rgb[2], alpha]
}

fn shortest_hue_delta(from: f32, to: f32) -> f32 {
    fract(to - from + 1.5) - 0.5
}

/// Converts HSV (hue in turns) to straight rgb without any transcendental call.
fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    if saturation <= 0.0 {
        return [value, value, value];
    }
    let h6 = fract(hue) * 6.0;
    let sector = h6.floor();
    let f = h6 - sector;
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * f);
    let t = value * (1.0 - saturation * (1.0 - f));
    if sector < 1.0 {
        [value, t, p]
    } else if sector < 2.0 {
        [q, value, p]
    } else if sector < 3.0 {
        [p, value, t]
    } else if sector < 4.0 {
        [p, q, value]
    } else if sector < 5.0 {
        [t, p, value]
    } else {
        [value, p, q]
    }
}

/// Converts the documented `(hue, lightness, saturation)` triple back to rgb.
///
/// This is the HSL inverse of the forward HSI-flavoured definitions the phase
/// menu uses: `lightness = (max + min) / 2` and `saturation = 1 - min / max`.
/// A true HSI inverse needs a cosine, which the shader may not call, and it
/// leaves the displayable cube; the design's own K10 anchor (`(1, 0, 0)`) is the
/// HSL result.
fn hue_lightness_saturation_to_rgb(hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    if saturation <= 0.0 {
        return [lightness, lightness, lightness];
    }
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h6 = fract(hue) * 6.0;
    let sector = h6.floor();
    let second = chroma * (1.0 - (h6 - 2.0 * (h6 / 2.0).floor() - 1.0).abs());
    let (r, g, b) = if sector < 1.0 {
        (chroma, second, 0.0)
    } else if sector < 2.0 {
        (second, chroma, 0.0)
    } else if sector < 3.0 {
        (0.0, chroma, second)
    } else if sector < 4.0 {
        (0.0, second, chroma)
    } else if sector < 5.0 {
        (second, 0.0, chroma)
    } else {
        (chroma, 0.0, second)
    };
    let base = lightness - chroma / 2.0;
    [r + base, g + base, b + base]
}

#[cfg(test)]
// Test settings are clearer as a default plus the fields the case cares about.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn frame(width: u32, height: u32, colour: impl Fn(u32, u32) -> [f32; 4]) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.push(colour(x, y));
            }
        }
        pixels
    }

    fn same_bits(left: &[f32; 4], right: &[f32; 4]) -> bool {
        left.map(f32::to_bits) == right.map(f32::to_bits)
    }

    fn render(settings: ColoramaSettings, pixels: &mut [[f32; 4]]) -> ColoramaGrade {
        let grade = ColoramaGrade::new(settings).expect("the settings are finite");
        colorama_frame_f32(pixels, &grade);
        grade
    }

    fn intensity(pixel: [f32; 4]) -> f32 {
        0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2]
    }

    fn reference_palette(palette: ColoramaPalette, t: f32) -> [f32; 3] {
        let stops = palette.stops();
        let x = (t - t.floor()) * 5.0;
        let index = x.floor() as usize;
        let f = x - index as f32;
        let first = stops[index];
        let second = stops[(index + 1) % COLORAMA_STOP_COUNT];
        let weight = f * f * (3.0 - 2.0 * f);
        let mut delta = second[0] - first[0] + 1.5;
        delta -= delta.floor();
        delta -= 0.5;
        let hue = first[0] + delta * weight;
        let hue = hue - hue.floor();
        let saturation = first[1] * (1.0 - weight) + second[1] * weight;
        let value = first[2] * (1.0 - weight) + second[2] * weight;
        reference_hsv_to_rgb(hue, saturation, value)
    }

    fn reference_hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
        if saturation <= 0.0 {
            return [value, value, value];
        }
        let h6 = (hue - hue.floor()) * 6.0;
        let sector = h6.floor();
        let f = h6 - sector;
        let p = value * (1.0 - saturation);
        let q = value * (1.0 - saturation * f);
        let t = value * (1.0 - saturation * (1.0 - f));
        if sector < 1.0 {
            [value, t, p]
        } else if sector < 2.0 {
            [q, value, p]
        } else if sector < 3.0 {
            [p, value, t]
        } else if sector < 4.0 {
            [p, q, value]
        } else if sector < 5.0 {
            [t, p, value]
        } else {
            [value, p, q]
        }
    }

    #[test]
    fn colorama_k1_defaults_map_each_pixel_through_the_palette() {
        let mut pixels = frame(4, 1, |x, _| match x {
            0 => [0.0, 0.0, 0.0, 1.0],
            1 => [0.25, 0.5, 0.75, 1.0],
            2 => [1.0, 1.0, 1.0, 1.0],
            _ => [0.1, 0.2, 0.3, 1.0],
        });
        let sources = pixels.clone();
        render(ColoramaSettings::default(), &mut pixels);
        for (source, output) in sources.iter().zip(pixels.iter()) {
            let t = fract(intensity(*source));
            let expected = reference_palette(ColoramaPalette::HUE_CYCLE, t);
            assert_eq!(output[0].to_bits(), expected[0].to_bits());
            assert_eq!(output[1].to_bits(), expected[1].to_bits());
            assert_eq!(output[2].to_bits(), expected[2].to_bits());
            assert_eq!(output[3].to_bits(), 1.0f32.to_bits());
        }
    }

    #[test]
    fn colorama_k2_zero_repetitions_is_a_bit_exact_passthrough() {
        let sources = vec![
            [f32::NAN, 0.5, 0.25, 1.0],
            [f32::INFINITY, f32::NEG_INFINITY, 0.0, 1.0],
            [0.2, 0.4, 0.6, 1.0],
        ];
        let mut pixels = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.cycle_repetitions = 0.0;
        render(settings, &mut pixels);
        for (source, output) in sources.iter().zip(pixels.iter()) {
            assert!(
                same_bits(source, output),
                "zero repetitions must pass through"
            );
        }

        let mut invalid = ColoramaSettings::default();
        invalid.cycle_repetitions = -1.0;
        let error = ColoramaGrade::new(invalid).expect_err("negative repetitions are rejected");
        assert_eq!(error, ColoramaError::NegativeRepetitions { value: -1.0 });
        assert_eq!(
            error.to_string(),
            "colorama cycle repetitions must be >= 0, got -1"
        );
    }

    #[test]
    fn colorama_k3_full_blend_is_a_bit_exact_passthrough() {
        let sources = frame(3, 2, |x, y| [x as f32 / 4.0, y as f32 / 3.0, 0.5, 1.0]);
        let mut pixels = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.blend_with_original = 100.0;
        render(settings, &mut pixels);
        for (source, output) in sources.iter().zip(pixels.iter()) {
            assert!(
                same_bits(source, output),
                "a hundred percent blend is the source"
            );
        }
    }

    #[test]
    fn colorama_k4_disabled_modify_with_composite_is_a_bit_exact_passthrough() {
        let sources = frame(3, 2, |x, y| [x as f32 / 5.0, y as f32 / 4.0, 0.25, 0.75]);
        let mut pixels = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.modify = ColoramaModify::None;
        settings.modify_alpha = 0.0;
        settings.composite_on_layer = 1.0;
        render(settings, &mut pixels);
        for (source, output) in sources.iter().zip(pixels.iter()) {
            assert!(
                same_bits(source, output),
                "the documented identity must be exact"
            );
        }
    }

    #[test]
    fn colorama_k5_phase_shift_is_degrees_and_a_full_turn_is_exact() {
        let sources = vec![[0.25, 0.3, 0.2, 1.0], [0.75, 0.2, 0.4, 1.0]];
        let mut zero = sources.clone();
        let mut full = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.get_phase_from = ColoramaPhaseSource::Red;
        settings.phase_shift = 0.0;
        render(settings, &mut zero);
        settings.phase_shift = 360.0;
        render(settings, &mut full);
        for (left, right) in zero.iter().zip(full.iter()) {
            assert!(
                same_bits(left, right),
                "a full turn must not change the phase"
            );
        }
        settings.phase_shift = 90.0;
        assert_eq!(settings.phase_shift_turns().to_bits(), 0.25f32.to_bits());
    }

    #[test]
    fn colorama_k6_palette_seam_wraps_from_white_to_black() {
        let sources = vec![[1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]];
        let mut pixels = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.get_phase_from = ColoramaPhaseSource::Red;
        render(settings, &mut pixels);
        assert!(
            same_bits(&pixels[0], &pixels[1]),
            "phase one and phase zero must land on the same stop"
        );
    }

    #[test]
    fn colorama_k7_add_modes_combine_the_two_phases() {
        let source = [0.0, 1.0, 1.0, 0.8];
        let p1 = 0.8f32;
        let p2 = 0.5f32;
        let expected = [
            (ColoramaAddMode::Wrap, fract(p1 + p2)),
            (ColoramaAddMode::Clamp, 1.0),
            (ColoramaAddMode::Average, 0.5 * (p1 + p2)),
            (ColoramaAddMode::Screen, 1.0 - (1.0 - p1) * (1.0 - p2)),
        ];
        for (mode, combined) in expected {
            let mut pixels = vec![source];
            let mut settings = ColoramaSettings::default();
            settings.get_phase_from = ColoramaPhaseSource::Alpha;
            settings.add_phase_from = ColoramaPhaseSource::Hue;
            settings.add_phase = 1.0;
            settings.add_mode = mode;
            render(settings, &mut pixels);
            let t = fract(combined);
            let palette = reference_palette(ColoramaPalette::HUE_CYCLE, t);
            assert_eq!(pixels[0][0].to_bits(), palette[0].to_bits(), "{mode:?}");
            assert_eq!(pixels[0][1].to_bits(), palette[1].to_bits(), "{mode:?}");
            assert_eq!(pixels[0][2].to_bits(), palette[2].to_bits(), "{mode:?}");
        }
    }

    #[test]
    fn colorama_k8_zero_secondary_source_skips_the_combination() {
        let sources = vec![[0.3, 0.6, 0.2, 0.8], [0.9, 0.1, 0.4, 0.5]];
        let mut skipped = sources.clone();
        let mut neutral = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.get_phase_from = ColoramaPhaseSource::Red;
        settings.add_phase_from = ColoramaPhaseSource::Zero;
        settings.add_phase = 1.0;
        render(settings, &mut skipped);
        settings.add_phase_from = ColoramaPhaseSource::Hue;
        settings.add_phase = 0.0;
        render(settings, &mut neutral);
        for (left, right) in skipped.iter().zip(neutral.iter()) {
            assert!(
                same_bits(left, right),
                "the zero source must skip the combination"
            );
        }
    }

    #[test]
    fn colorama_k9_stepped_palette_uses_exactly_256_colours() {
        let count = 1024usize;
        let sources: Vec<[f32; 4]> = (0..count)
            .map(|index| {
                let value = index as f32 / (count - 1) as f32;
                [value, value, value, 1.0]
            })
            .collect();
        let mut pixels = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.get_phase_from = ColoramaPhaseSource::Red;
        settings.interpolate = false;
        render(settings, &mut pixels);
        let mut colours = HashSet::new();
        for (source, output) in sources.iter().zip(pixels.iter()) {
            let t = fract(fract(source[0]) * 1.0);
            let quantised = (t * 256.0).floor() / 256.0;
            let expected = reference_palette(ColoramaPalette::HUE_CYCLE, quantised);
            assert_eq!(output[0].to_bits(), expected[0].to_bits());
            assert_eq!(output[1].to_bits(), expected[1].to_bits());
            assert_eq!(output[2].to_bits(), expected[2].to_bits());
            colours.insert([output[0], output[1], output[2]].map(f32::to_bits));
        }
        assert!(colours.len() > 5, "the stepped wheel must keep its stops");
        assert!(
            colours.len() <= 256,
            "stepped mode quantises to 256 colours"
        );
    }

    #[test]
    fn colorama_k10_saturation_write_back_round_trips_through_the_documented_space() {
        let mut pixels = vec![[0.5, 0.5, 0.5, 1.0]];
        let mut settings = ColoramaSettings::default();
        settings.get_phase_from = ColoramaPhaseSource::Red;
        settings.modify = ColoramaModify::Saturation;
        render(settings, &mut pixels);
        let output = pixels[0];
        assert!((output[0] - 1.0).abs() <= 1e-6);
        assert!(output[1].abs() <= 1e-6);
        assert!(output[2].abs() <= 1e-6);
        assert!(rgb_hue(output).abs() <= 1e-6);
        assert!((rgb_saturation(output) - 1.0).abs() <= 1e-6);
        assert!((rgb_lightness(output) - 0.5).abs() <= 1e-6);
    }

    #[test]
    fn colorama_k11_match_endpoints_select_inside_the_tolerance() {
        let sources = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.999, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
        ];
        let mut exact = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.get_phase_from = ColoramaPhaseSource::Red;
        settings.matching_mode = ColoramaMatchMode::Rgb;
        settings.matching_color = [1.0, 0.0, 0.0];
        settings.matching_tolerance = 0.0;
        settings.matching_softness = 0.0;
        render(settings, &mut exact);
        let t = fract(sources[0][0]);
        let expected = reference_palette(ColoramaPalette::HUE_CYCLE, t);
        assert_eq!(exact[0][0].to_bits(), expected[0].to_bits());
        assert!(
            same_bits(&exact[1], &sources[1]),
            "a near miss must stay untouched"
        );
        assert!(
            same_bits(&exact[2], &sources[2]),
            "a far miss must stay untouched"
        );

        let mut wide = sources.clone();
        settings.matching_tolerance = 1.0;
        render(settings, &mut wide);
        for (source, output) in sources.iter().zip(wide.iter()) {
            let t = fract(source[0]);
            let expected = reference_palette(ColoramaPalette::HUE_CYCLE, t);
            assert_eq!(
                output[0].to_bits(),
                expected[0].to_bits(),
                "tolerance one selects all"
            );
            assert_eq!(output[1].to_bits(), expected[1].to_bits());
            assert_eq!(output[2].to_bits(), expected[2].to_bits());
        }
    }

    #[test]
    fn colorama_k12_hue_distance_takes_the_short_arc() {
        let match_colour = hsv_to_rgb(0.98, 1.0, 1.0);
        let pixel = hsv_to_rgb(0.02, 1.0, 1.0);
        let delta = (rgb_hue_of(pixel) - rgb_hue_of(match_colour)).abs();
        let distance = circular_hue_distance(delta);
        assert!(
            (0.07..0.09).contains(&distance),
            "the short arc is 0.08, got {distance}"
        );

        let mut rejected = vec![[pixel[0], pixel[1], pixel[2], 1.0]];
        let mut settings = ColoramaSettings::default();
        settings.get_phase_from = ColoramaPhaseSource::Red;
        settings.matching_mode = ColoramaMatchMode::Hue;
        settings.matching_color = match_colour;
        settings.matching_tolerance = 0.05;
        settings.matching_softness = 0.0;
        render(settings, &mut rejected);
        assert!(same_bits(
            &rejected[0],
            &[pixel[0], pixel[1], pixel[2], 1.0]
        ));

        let mut accepted = vec![[pixel[0], pixel[1], pixel[2], 1.0]];
        settings.matching_tolerance = 0.10;
        render(settings, &mut accepted);
        let expected = reference_palette(ColoramaPalette::HUE_CYCLE, fract(pixel[0]));
        assert_eq!(accepted[0][0].to_bits(), expected[0].to_bits());
        assert_eq!(accepted[0][1].to_bits(), expected[1].to_bits());
        assert_eq!(accepted[0][2].to_bits(), expected[2].to_bits());
    }

    #[test]
    fn colorama_k13_mask_layer_zero_is_the_same_as_a_disabled_matte() {
        let sources = frame(3, 2, |x, y| [x as f32 / 4.0, y as f32 / 3.0, 0.5, 0.8]);
        let mut disabled = sources.clone();
        let mut gated = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.mask_mode = ColoramaMaskMode::Off;
        render(settings, &mut disabled);
        settings.mask_mode = ColoramaMaskMode::Intensity;
        settings.mask_layer = 0.0;
        render(settings, &mut gated);
        for (left, right) in disabled.iter().zip(gated.iter()) {
            assert!(
                same_bits(left, right),
                "an unselected mask layer must not gate"
            );
        }
    }

    #[test]
    fn colorama_k14_mask_intensity_keeps_dark_pixels_and_selects_bright_ones() {
        let sources = vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]];
        let mut pixels = sources.clone();
        let mut settings = ColoramaSettings::default();
        settings.mask_mode = ColoramaMaskMode::Intensity;
        settings.mask_layer = 1.0;
        render(settings, &mut pixels);
        assert!(
            same_bits(&pixels[0], &sources[0]),
            "a zero luma selects nothing"
        );
        let t = fract(intensity(sources[1]));
        let expected = reference_palette(ColoramaPalette::HUE_CYCLE, t);
        assert_eq!(pixels[1][0].to_bits(), expected[0].to_bits());
        assert_eq!(pixels[1][1].to_bits(), expected[1].to_bits());
        assert_eq!(pixels[1][2].to_bits(), expected[2].to_bits());
    }

    #[test]
    fn colorama_k15_isolated_mode_scales_alpha_by_the_selection() {
        let mut pixels = vec![[0.2, 0.4, 0.6, 0.5]];
        let source = pixels[0];
        let mut settings = ColoramaSettings::default();
        settings.get_phase_from = ColoramaPhaseSource::Red;
        settings.composite_on_layer = 0.0;
        settings.mask_mode = ColoramaMaskMode::Alpha;
        settings.mask_layer = 1.0;
        render(settings, &mut pixels);
        let t = fract(source[0]);
        let expected = reference_palette(ColoramaPalette::HUE_CYCLE, t);
        assert_eq!(pixels[0][3].to_bits(), 0.25f32.to_bits());
        assert_eq!(pixels[0][0].to_bits(), expected[0].to_bits());
        assert_eq!(pixels[0][1].to_bits(), expected[1].to_bits());
        assert_eq!(pixels[0][2].to_bits(), expected[2].to_bits());
    }

    #[test]
    fn colorama_k16_alpha_ramp_and_empty_pixels() {
        let source = [0.5, 0.5, 0.5, 0.0];
        let mut untouched = vec![source];
        let mut painted = vec![source];
        let mut settings = ColoramaSettings::default();
        settings.preset_palette = ColoramaPalette::ALPHA_RAMP;
        settings.get_phase_from = ColoramaPhaseSource::Red;
        settings.change_empty_pixels = 0.0;
        render(settings, &mut untouched);
        assert!(
            same_bits(&untouched[0], &source),
            "empty pixels are skipped by default"
        );
        settings.change_empty_pixels = 1.0;
        render(settings, &mut painted);
        assert_eq!(painted[0][3].to_bits(), 0.5f32.to_bits());
        assert_eq!(painted[0][0].to_bits(), 1.0f32.to_bits());
        assert_eq!(painted[0][1].to_bits(), 1.0f32.to_bits());
        assert_eq!(painted[0][2].to_bits(), 1.0f32.to_bits());

        assert_eq!(
            ColoramaPalette::from_menu(36),
            Err(ColoramaError::InvalidPaletteCode { code: 35 })
        );
        assert_eq!(
            ColoramaPalette::from_code(35),
            Err(ColoramaError::InvalidPaletteCode { code: 35 })
        );
        assert_eq!(ColoramaPalette::from_menu(35), Ok(ColoramaPalette::NONE));
    }
}
