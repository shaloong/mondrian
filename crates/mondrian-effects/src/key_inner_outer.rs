//! Inner/Outer key (KEY-03): keys a semi-transparent subject against a sampled
//! foreground and background estimate.
//!
//! # Estimated colours
//!
//! The two authored points are normalised frame coordinates. Each is sampled
//! with a texel-centre clamped bilinear filter of the **source** frame, so the
//! estimate refreshes on every rendered frame instead of pinning the colour that
//! was authored. When a point is dragged outside `0..=1` the sample has no
//! subject, and the estimate falls back to the authored colour of that side
//! (前景色 for the foreground point, 背景色 for the background point). The
//! authored colours are also what the UI shows, so the fallback is visible.
//!
//! # Matte
//!
//! `d` is the largest absolute difference of the three straight-alpha colour
//! channels between the pixel and the estimate of the active 遮罩来源 side (no
//! square root, same measure as KEY-06):
//!
//! ```text
//! m_raw = 1 - smoothstep(tolerance, tolerance + softness, d)
//! softness == 0  ->  m_raw = if d <= tolerance { 1 } else { 0 }
//! ```
//!
//! The tolerance and softness come from 颜色纯度: 0 宽松 `0.30/0.30`,
//! 1 标准 `0.20/0.15`, 2 严格 `0.10/0.06`. A source pixel with no coverage
//! (`a <= 0`) counts as non-foreground (`m = 0`), so it can never leak into the
//! dilated window.
//!
//! # Morphology
//!
//! 收缩扩张 is authored in pixels and rounded like the shader:
//! `R = round(clamp(v, -20, 20))`. `R > 0` **dilates** the matte (window
//! maximum) and `R < 0` **erodes** it (window minimum); note that this is the
//! opposite sign convention of the standalone choker, where a negative amount
//! dilates. Both axes are separable, the window is a square, and the taps clamp
//! at the frame edge. 边缘羽化 is a separable box average of radius
//! `F = round(clamp(v, 0, 50))` with `2F + 1` taps of weight `1 / (2F + 1)`.
//!
//! # Composite
//!
//! 去溢色 pulls the colour towards the estimate of the matte-source side by
//! `k_d * (1 - m)`, with `k_d` in `{0, 0.35, 0.7}` for tiers 0, 1 and 2. The
//! output keeps the source colour (unclamped) and writes
//! `alpha_out = clamp(source.a, 0, 1) * m`; a non-positive output coverage
//! becomes the fully transparent `(0, 0, 0, 0)`. Straight alpha is preserved, so
//! the compositor premultiplies the result later.
//!
//! # No identity
//!
//! The documented defaults already key the screen colour out of the frame, so
//! [`InnerOuterKeyGrade::is_identity`] is always false and the graph builder
//! always appends the node.
//!
//! # Scratch budget
//!
//! The chain keeps one destination plus two ping-pong planes, so the planner
//! pools four RGBA planes of 16 bytes per pixel. At UHD that is
//! `3840 * 2160 * 16 * 4 = 530,841,600` bytes, inside the 512 MiB spatial
//! scratch budget; only DCI 4K (`4096 * 2160 * 16 * 4 = 566,231,040` bytes)
//! exceeds it, exactly like KEY-06 KeyCleaner.

/// Documented default foreground colour: green screen.
pub const KEY_INNER_OUTER_DEFAULT_FOREGROUND_COLOUR: [f32; 3] = [0.0, 1.0, 0.0];
/// Documented default background colour: black.
pub const KEY_INNER_OUTER_DEFAULT_BACKGROUND_COLOUR: [f32; 3] = [0.0, 0.0, 0.0];
/// Documented default foreground sample point: frame centre.
pub const KEY_INNER_OUTER_DEFAULT_FOREGROUND_POINT: [f32; 2] = [0.5, 0.5];
/// Documented default background sample point: frame corner.
pub const KEY_INNER_OUTER_DEFAULT_BACKGROUND_POINT: [f32; 2] = [0.0, 0.0];
/// Documented default choke amount: no morphology.
pub const KEY_INNER_OUTER_DEFAULT_CHOKE_PIXELS: f32 = 0.0;
/// Lowest authored choke amount in pixels: erosion.
pub const KEY_INNER_OUTER_MIN_CHOKE_PIXELS: f32 = -20.0;
/// Highest authored choke amount in pixels: dilation.
pub const KEY_INNER_OUTER_MAX_CHOKE_PIXELS: f32 = 20.0;
/// Documented default feather radius: no box average.
pub const KEY_INNER_OUTER_DEFAULT_FEATHER_PIXELS: f32 = 0.0;
/// Highest authored feather radius in pixels.
pub const KEY_INNER_OUTER_MAX_FEATHER_PIXELS: f32 = 50.0;
/// Documented default 去溢色 tier.
pub const KEY_INNER_OUTER_DEFAULT_DESPILL: u8 = 1;
/// Highest authored 去溢色 tier.
pub const KEY_INNER_OUTER_MAX_DESPILL: u8 = 2;
/// Texture-pass count the renderer reserves planes for.
///
/// The drawing chain pings between `scratch[1]` and `scratch[2]` on top of the
/// destination `scratch[0]`, so every configuration indexes at most three
/// planes; the planner then pools `largest_pass_count + 1` planes.
pub const KEY_INNER_OUTER_TEXTURE_PASS_COUNT: u32 = 3;
/// Estimated CPU cost of the effect, in the same unit as the other keyers.
pub const KEY_INNER_OUTER_COST: u8 = 4;

/// Pixels between cooperative checkpoints in the CPU kernel.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Which side of the key supplies the estimate the matte is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MatteSource {
    /// Key the foreground estimate: keep the pixels that do not match it.
    Foreground,
    /// Key the background estimate: keep the pixels that do not match it.
    Background,
}

impl MatteSource {
    /// Every authored source, in menu order.
    pub const ALL: [Self; 2] = [Self::Foreground, Self::Background];

    /// Stable schema key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }

    /// Inverse of [`Self::as_key`].
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.as_key() == key)
    }

    /// Zero-based code carried in the GPU uniform cell.
    #[must_use]
    pub const fn as_code(self) -> u32 {
        match self {
            Self::Foreground => 0,
            Self::Background => 1,
        }
    }

    /// Inverse of [`Self::as_code`].
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Foreground),
            1 => Some(Self::Background),
            _ => None,
        }
    }

    /// Author-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Foreground => "前景",
            Self::Background => "背景",
        }
    }
}

/// How tightly a pixel must match the estimate before it counts as matte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColourPurity {
    /// Widest tolerance and softest ramp.
    Loose,
    /// Balanced tolerance and ramp.
    Standard,
    /// Narrowest tolerance and hardest ramp.
    Strict,
}

impl ColourPurity {
    /// Every authored purity, in menu order.
    pub const ALL: [Self; 3] = [Self::Loose, Self::Standard, Self::Strict];

    /// Stable schema key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Loose => "loose",
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }

    /// Inverse of [`Self::as_key`].
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|purity| purity.as_key() == key)
    }

    /// Zero-based code carried in the GPU uniform cell.
    #[must_use]
    pub const fn as_code(self) -> u32 {
        match self {
            Self::Loose => 0,
            Self::Standard => 1,
            Self::Strict => 2,
        }
    }

    /// Inverse of [`Self::as_code`].
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Loose),
            1 => Some(Self::Standard),
            2 => Some(Self::Strict),
            _ => None,
        }
    }

    /// Author-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Loose => "宽松",
            Self::Standard => "标准",
            Self::Strict => "严格",
        }
    }

    /// Largest channel distance that still counts as a full matte.
    #[must_use]
    pub const fn tolerance(self) -> f32 {
        match self {
            Self::Loose => 0.30,
            Self::Standard => 0.20,
            Self::Strict => 0.10,
        }
    }

    /// Width of the ramp above the tolerance.
    #[must_use]
    pub const fn softness(self) -> f32 {
        match self {
            Self::Loose => 0.30,
            Self::Standard => 0.15,
            Self::Strict => 0.06,
        }
    }
}

/// How far 去溢色 pulls a pixel towards the estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DespillStrength {
    /// Keep the source colour.
    Off,
    /// Pull the colour a third of the way towards the estimate.
    Light,
    /// Pull the colour most of the way towards the estimate.
    Strong,
}

impl DespillStrength {
    /// Every authored tier, in menu order.
    pub const ALL: [Self; 3] = [Self::Off, Self::Light, Self::Strong];

    /// Zero-based code carried in the GPU uniform cell.
    #[must_use]
    pub const fn as_code(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Light => 1,
            Self::Strong => 2,
        }
    }

    /// Inverse of [`Self::as_code`].
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Off),
            1 => Some(Self::Light),
            2 => Some(Self::Strong),
            _ => None,
        }
    }

    /// How far the colour travels towards the estimate at a zero matte.
    #[must_use]
    pub const fn factor(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Light => 0.35,
            Self::Strong => 0.7,
        }
    }
}

/// Why a set of authored inner/outer key controls was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InnerOuterKeyError {
    /// The foreground colour left `0..=1` or is not finite.
    #[error("inner/outer key foreground colour must be finite and within [0, 1] per channel")]
    ForegroundColour,
    /// The background colour left `0..=1` or is not finite.
    #[error("inner/outer key background colour must be finite and within [0, 1] per channel")]
    BackgroundColour,
    /// The foreground point is not finite.
    #[error("inner/outer key foreground point must be finite")]
    ForegroundPoint,
    /// The background point is not finite.
    #[error("inner/outer key background point must be finite")]
    BackgroundPoint,
    /// The choke amount left `-20..=20` or is not finite.
    #[error("inner/outer key choke must be finite and within [-20, 20] pixels")]
    Choke,
    /// The feather radius left `0..=50` or is not finite.
    #[error("inner/outer key feather must be finite and within [0, 50] pixels")]
    Feather,
    /// The 去溢色 tier left `0..=2`.
    #[error("inner/outer key despill must be within [0, 2]")]
    Despill,
}

impl InnerOuterKeyError {
    /// Authored parameter that produced this rejection.
    #[must_use]
    pub const fn parameter(self) -> &'static str {
        match self {
            Self::ForegroundColour => "foreground_colour",
            Self::BackgroundColour => "background_colour",
            Self::ForegroundPoint => "foreground_point",
            Self::BackgroundPoint => "background_point",
            Self::Choke => "choke",
            Self::Feather => "feather",
            Self::Despill => "despill",
        }
    }
}

/// The authored controls of one inner/outer key operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InnerOuterKeyParameters {
    /// Straight-alpha RGB of the foreground estimate fallback.
    pub foreground_colour: [f32; 3],
    /// Straight-alpha RGB of the background estimate fallback.
    pub background_colour: [f32; 3],
    /// Normalised foreground sample point.
    pub foreground_point: [f32; 2],
    /// Normalised background sample point.
    pub background_point: [f32; 2],
    /// Which estimate the matte is built from.
    pub matte_source: MatteSource,
    /// Tolerance/softness tier.
    pub colour_purity: ColourPurity,
    /// 收缩扩张 in pixels: positive dilates, negative erodes.
    pub choke: f32,
    /// 边缘羽化 radius in pixels.
    pub feather: f32,
    /// 去溢色 tier.
    pub despill: DespillStrength,
}

impl Default for InnerOuterKeyParameters {
    fn default() -> Self {
        Self {
            foreground_colour: KEY_INNER_OUTER_DEFAULT_FOREGROUND_COLOUR,
            background_colour: KEY_INNER_OUTER_DEFAULT_BACKGROUND_COLOUR,
            foreground_point: KEY_INNER_OUTER_DEFAULT_FOREGROUND_POINT,
            background_point: KEY_INNER_OUTER_DEFAULT_BACKGROUND_POINT,
            matte_source: MatteSource::Foreground,
            colour_purity: ColourPurity::Loose,
            choke: KEY_INNER_OUTER_DEFAULT_CHOKE_PIXELS,
            feather: KEY_INNER_OUTER_DEFAULT_FEATHER_PIXELS,
            despill: DespillStrength::from_code(KEY_INNER_OUTER_DEFAULT_DESPILL)
                .expect("the documented despill default is a declared tier"),
        }
    }
}

/// A validated inner/outer key grade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InnerOuterKeyGrade {
    parameters: InnerOuterKeyParameters,
}

impl InnerOuterKeyGrade {
    /// Validates the authored controls.
    ///
    /// # Errors
    ///
    /// Returns the field-specific variant of [`InnerOuterKeyError`] for the
    /// first control that is not finite or leaves its authored range.
    pub fn new(parameters: InnerOuterKeyParameters) -> Result<Self, InnerOuterKeyError> {
        let colours = [
            (
                parameters.foreground_colour,
                InnerOuterKeyError::ForegroundColour,
            ),
            (
                parameters.background_colour,
                InnerOuterKeyError::BackgroundColour,
            ),
        ];
        for (colour, error) in colours {
            if colour
                .iter()
                .any(|channel| !channel.is_finite() || !(0.0..=1.0).contains(channel))
            {
                return Err(error);
            }
        }
        for (point, error) in [
            (
                parameters.foreground_point,
                InnerOuterKeyError::ForegroundPoint,
            ),
            (
                parameters.background_point,
                InnerOuterKeyError::BackgroundPoint,
            ),
        ] {
            if point.iter().any(|coordinate| !coordinate.is_finite()) {
                return Err(error);
            }
        }
        if !parameters.choke.is_finite()
            || !(KEY_INNER_OUTER_MIN_CHOKE_PIXELS..=KEY_INNER_OUTER_MAX_CHOKE_PIXELS)
                .contains(&parameters.choke)
        {
            return Err(InnerOuterKeyError::Choke);
        }
        if !parameters.feather.is_finite()
            || !(KEY_INNER_OUTER_DEFAULT_FEATHER_PIXELS..=KEY_INNER_OUTER_MAX_FEATHER_PIXELS)
                .contains(&parameters.feather)
        {
            return Err(InnerOuterKeyError::Feather);
        }
        if parameters.despill.as_code() > KEY_INNER_OUTER_MAX_DESPILL {
            return Err(InnerOuterKeyError::Despill);
        }
        Ok(Self { parameters })
    }

    /// The documented default grade.
    ///
    /// # Errors
    ///
    /// Never fails: the documented defaults are inside every range. The result
    /// is a `Result` so the builder can share one parsing path.
    pub fn documented_defaults() -> Result<Self, InnerOuterKeyError> {
        Self::new(InnerOuterKeyParameters::default())
    }

    /// Every authored control.
    #[must_use]
    pub const fn parameters(&self) -> InnerOuterKeyParameters {
        self.parameters
    }

    /// The 遮罩来源 side.
    #[must_use]
    pub const fn matte_source(&self) -> MatteSource {
        self.parameters.matte_source
    }

    /// The 颜色纯度 tier.
    #[must_use]
    pub const fn colour_purity(&self) -> ColourPurity {
        self.parameters.colour_purity
    }

    /// The authored choke amount in pixels.
    #[must_use]
    pub const fn choke(&self) -> f32 {
        self.parameters.choke
    }

    /// The authored feather radius in pixels.
    #[must_use]
    pub const fn feather(&self) -> f32 {
        self.parameters.feather
    }

    /// The 去溢色 tier.
    #[must_use]
    pub const fn despill(&self) -> DespillStrength {
        self.parameters.despill
    }

    /// The integer choke radius: the morphology cannot read a fractional tap, so
    /// the authored magnitude is rounded here and in the shader.
    #[must_use]
    pub fn choke_radius(&self) -> u32 {
        self.parameters.choke.abs().round() as u32
    }

    /// Whether a choke morphology pass runs at all.
    #[must_use]
    pub fn chokes(&self) -> bool {
        self.choke_radius() > 0
    }

    /// Whether the choke dilates the matte. A positive authored amount dilates;
    /// this is the opposite sign of the standalone choker.
    #[must_use]
    pub const fn dilates(&self) -> bool {
        self.parameters.choke > 0.0
    }

    /// The integer feather radius.
    #[must_use]
    pub fn feather_radius(&self) -> u32 {
        self.parameters.feather.round() as u32
    }

    /// Whether a feather pass runs at all.
    #[must_use]
    pub fn feathers(&self) -> bool {
        self.feather_radius() > 0
    }

    /// The spatial halo on both axes, in pixels.
    ///
    /// The morphology reads `ceil(|choke|)` pixels and the feather reads
    /// `ceil(feather)` pixels; the extra pixel covers the bilinear tap.
    #[must_use]
    pub fn halo(&self) -> u32 {
        self.parameters.choke.abs().ceil() as u32 + self.parameters.feather.ceil() as u32 + 1
    }

    /// The number of GPU draw passes: the matte, an optional two-axis choke, an
    /// optional first feather axis, and the composite (which folds the second
    /// feather axis).
    #[must_use]
    pub fn draw_pass_count(&self) -> u32 {
        let choke_extra = if self.chokes() { 2 } else { 0 };
        let feather_extra = if self.feathers() { 1 } else { 0 };
        2 + choke_extra + feather_extra
    }

    /// The texture-pass count the renderer sizes its scratch pool from.
    ///
    /// This is not the draw count: the chain pings between `scratch[1]` and
    /// `scratch[2]` on top of the destination `scratch[0]`, so it never indexes
    /// more than three planes. The `spatial_effect_passes` diagnostic still
    /// reports [`Self::draw_pass_count`].
    #[must_use]
    pub const fn pass_count(&self) -> u32 {
        KEY_INNER_OUTER_TEXTURE_PASS_COUNT
    }

    /// Estimated cost in the shared keyer unit.
    #[must_use]
    pub const fn cost(&self) -> u8 {
        KEY_INNER_OUTER_COST
    }

    /// The documented non-identity: the defaults already key the screen colour.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        false
    }

    /// The matte-pass uniform cells.
    ///
    /// `params` is `[purity_code, source_code, 0, 0]`; `extra` is the authored
    /// foreground colour, `aces` the authored background colour, and `extra1`
    /// both points. `params.w` is overwritten by the renderer's linear-filter
    /// flag and carries no authored value. The renderer fills the `size` cell
    /// with `[width, height, 0, 0]`.
    #[must_use]
    pub fn matte_gpu_controls(&self) -> InnerOuterKeyGpuControls {
        InnerOuterKeyGpuControls {
            params: [
                self.parameters.colour_purity.as_code() as f32,
                self.parameters.matte_source.as_code() as f32,
                0.0,
                0.0,
            ],
            extra: [
                self.parameters.foreground_colour[0],
                self.parameters.foreground_colour[1],
                self.parameters.foreground_colour[2],
                0.0,
            ],
            aces: [
                self.parameters.background_colour[0],
                self.parameters.background_colour[1],
                self.parameters.background_colour[2],
                0.0,
            ],
            extra1: [
                self.parameters.foreground_point[0],
                self.parameters.foreground_point[1],
                self.parameters.background_point[0],
                self.parameters.background_point[1],
            ],
        }
    }

    /// The choke-pass uniform cell: `[radius, dialates, 0, 0]`.
    ///
    /// The pass reads the matte plane's `.r` channel.
    #[must_use]
    pub fn choke_gpu_controls(&self) -> [f32; 4] {
        [
            self.choke_radius() as f32,
            f32::from(u8::from(self.dilates())),
            0.0,
            0.0,
        ]
    }

    /// The feather-pass uniform cell: `[radius, 0, 0, 0]`.
    #[must_use]
    pub fn feather_gpu_controls(&self) -> [f32; 4] {
        [self.feather_radius() as f32, 0.0, 0.0, 0.0]
    }

    /// The composite-pass uniform cells.
    ///
    /// `params` is `[despill_code, source_code, 0, 0]`. The estimate cells, the
    /// point cells and the size cell repeat the matte pass so the shader can
    /// refresh the estimate and pick the despill target from the same source
    /// sample; the design's literal `aces = [0; 4]` would leave the composite
    /// unable to sample the frame.
    #[must_use]
    pub fn composite_gpu_controls(&self) -> InnerOuterKeyGpuControls {
        let mut controls = self.matte_gpu_controls();
        controls.params = [
            f32::from(self.parameters.despill.as_code()),
            self.parameters.matte_source.as_code() as f32,
            0.0,
            0.0,
        ];
        controls
    }

    /// Hashes every authored scalar so a cache key cannot alias two grades.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        for channel in self.parameters.foreground_colour {
            channel.to_bits().hash(state);
        }
        for channel in self.parameters.background_colour {
            channel.to_bits().hash(state);
        }
        for coordinate in self.parameters.foreground_point {
            coordinate.to_bits().hash(state);
        }
        for coordinate in self.parameters.background_point {
            coordinate.to_bits().hash(state);
        }
        self.parameters.matte_source.as_key().hash(state);
        self.parameters.colour_purity.as_key().hash(state);
        self.parameters.choke.to_bits().hash(state);
        self.parameters.feather.to_bits().hash(state);
        self.parameters.despill.as_code().hash(state);
    }
}

/// One `<SpatialPassUniform>` layout for the matte and composite passes.
///
/// The `size` cell is not part of the grade: the renderer writes
/// `[width, height, 0, 0]` for the frame being dispatched.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InnerOuterKeyGpuControls {
    /// `params` cell, `params.w` overwritten by the linear-filter flag.
    pub params: [f32; 4],
    /// Authored foreground colour in `rgb`, `a = 0`.
    pub extra: [f32; 4],
    /// Authored background colour in `rgb`, `a = 0`.
    pub aces: [f32; 4],
    /// Foreground point in `xy`, background point in `zw`.
    pub extra1: [f32; 4],
}

/// Apply the inner/outer key to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn inner_outer_key_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &InnerOuterKeyGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_inner_outer_key_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// The checkpointed kernel behind [`inner_outer_key_frame_f32`].
pub fn apply_inner_outer_key_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &InnerOuterKeyGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    let frame = &mut working[..pixel_count];
    let parameters = grade.parameters;
    let foreground = estimate_colour(
        frame,
        width,
        height,
        parameters.foreground_point,
        parameters.foreground_colour,
    );
    let background = estimate_colour(
        frame,
        width,
        height,
        parameters.background_point,
        parameters.background_colour,
    );
    let estimate = match parameters.matte_source {
        MatteSource::Foreground => foreground,
        MatteSource::Background => background,
    };
    checkpoint()?;

    // Stage 1: the raw matte. A pixel with no coverage counts as non-foreground
    // so a later dilation can never resurrect it from the colour alone.
    let purity = parameters.colour_purity;
    let tolerance = purity.tolerance();
    let softness = purity.softness();
    let mut matte = Vec::with_capacity(pixel_count);
    for (index, pixel) in frame.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        if pixel[3] <= 0.0 {
            matte.push(0.0);
            continue;
        }
        let distance = (0..3)
            .map(|channel| (pixel[channel] - estimate[channel]).abs())
            .fold(0.0_f32, f32::max);
        matte.push(matte_from_distance(distance, tolerance, softness));
    }
    checkpoint()?;

    // Stage 2: the separable morphology and feather. Each axis writes a fresh
    // plane so the taps always read the unmodified input.
    let choke_radius = grade.choke_radius() as usize;
    if choke_radius > 0 {
        let dilates = grade.dilates();
        let mut horizontal = vec![0.0_f32; pixel_count];
        morph_horizontal(
            &matte,
            &mut horizontal,
            width,
            height,
            choke_radius,
            dilates,
        );
        checkpoint()?;
        let mut morphed = vec![0.0_f32; pixel_count];
        morph_vertical(
            &horizontal,
            &mut morphed,
            width,
            height,
            choke_radius,
            dilates,
        );
        checkpoint()?;
        matte = morphed;
    }

    let feather_radius = grade.feather_radius() as usize;
    if feather_radius > 0 {
        let mut horizontal = vec![0.0_f32; pixel_count];
        box_horizontal(&matte, &mut horizontal, width, height, feather_radius);
        checkpoint()?;
        let mut vertical = vec![0.0_f32; pixel_count];
        box_vertical(&horizontal, &mut vertical, width, height, feather_radius);
        checkpoint()?;
        matte = vertical;
    }

    // Stage 3: the composite. Straight alpha is preserved; the compositor
    // premultiplies the result later. The despill target is the estimate of the
    // active 遮罩来源 side, which is the colour the matte removed.
    let despill = parameters.despill.factor();
    for index in 0..pixel_count {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        let pixel = &mut frame[index];
        let coverage = pixel[3].clamp(0.0, 1.0) * matte[index];
        if pixel[3] <= 0.0 || coverage <= 0.0 {
            *pixel = [0.0; 4];
            continue;
        }
        if despill > 0.0 {
            let weight = despill * (1.0 - matte[index]);
            if weight > 0.0 {
                for channel in 0..3 {
                    pixel[channel] = mix(pixel[channel], estimate[channel], weight);
                }
            }
        }
        pixel[3] = coverage;
    }
    checkpoint()?;
    Ok(true)
}

/// The raw matte of one channel distance.
fn matte_from_distance(distance: f32, tolerance: f32, softness: f32) -> f32 {
    if softness <= 0.0 {
        return if distance <= tolerance { 1.0 } else { 0.0 };
    }
    1.0 - smoothstep(tolerance, tolerance + softness, distance)
}

/// Hermite ramp; a degenerate range becomes a hard step.
fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear interpolation; the weight is not clamped so a caller can extrapolate.
fn mix(from: f32, to: f32, weight: f32) -> f32 {
    from + (to - from) * weight
}

/// Whether a normalised point lies inside the frame.
fn point_in_frame(point: [f32; 2]) -> bool {
    (0.0..=1.0).contains(&point[0]) && (0.0..=1.0).contains(&point[1])
}

/// Texel-centre clamped bilinear sample of the source frame at a normalised
/// point. A point outside the frame falls back to the authored colour, which is
/// the visible value the UI shows.
fn estimate_colour(
    frame: &[[f32; 4]],
    width: usize,
    height: usize,
    point: [f32; 2],
    authored: [f32; 3],
) -> [f32; 3] {
    if !point_in_frame(point) {
        return authored;
    }
    sample_bilinear(frame, width, height, point)
}

/// The clamped bilinear colour sample behind [`estimate_colour`].
fn sample_bilinear(frame: &[[f32; 4]], width: usize, height: usize, point: [f32; 2]) -> [f32; 3] {
    let x = point[0] * width as f32 - 0.5;
    let y = point[1] * height as f32 - 0.5;
    let x = x.clamp(0.0, (width - 1) as f32);
    let y = y.clamp(0.0, (height - 1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let top_left = frame[y0 * width + x0];
    let top_right = frame[y0 * width + x1];
    let bottom_left = frame[y1 * width + x0];
    let bottom_right = frame[y1 * width + x1];
    let mut rgb = [0.0_f32; 3];
    for (channel, value) in rgb.iter_mut().enumerate() {
        let top = mix(top_left[channel], top_right[channel], fx);
        let bottom = mix(bottom_left[channel], bottom_right[channel], fx);
        *value = mix(top, bottom, fy);
    }
    rgb
}

/// Runs one horizontal min or max filter over a scalar plane.
fn morph_horizontal(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
    dialates: bool,
) {
    for row in 0..height {
        for column in 0..width {
            let mut value = if dialates { 0.0_f32 } else { 1.0_f32 };
            for offset in 0..=(2 * radius) {
                let sample = column as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, width as isize - 1) as usize;
                let candidate = source[row * width + clamped];
                value = if dialates {
                    value.max(candidate)
                } else {
                    value.min(candidate)
                };
            }
            destination[row * width + column] = value;
        }
    }
}

/// Runs one vertical min or max filter over a scalar plane.
fn morph_vertical(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
    dialates: bool,
) {
    for row in 0..height {
        for column in 0..width {
            let mut value = if dialates { 0.0_f32 } else { 1.0_f32 };
            for offset in 0..=(2 * radius) {
                let sample = row as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, height as isize - 1) as usize;
                let candidate = source[clamped * width + column];
                value = if dialates {
                    value.max(candidate)
                } else {
                    value.min(candidate)
                };
            }
            destination[row * width + column] = value;
        }
    }
}

/// Runs one horizontal box average over a scalar plane.
fn box_horizontal(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
) {
    let taps = 2 * radius + 1;
    let weight = 1.0 / taps as f32;
    for row in 0..height {
        for column in 0..width {
            let mut value = 0.0_f32;
            for offset in 0..taps {
                let sample = column as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, width as isize - 1) as usize;
                value += source[row * width + clamped];
            }
            destination[row * width + column] = value * weight;
        }
    }
}

/// Runs one vertical box average over a scalar plane.
fn box_vertical(
    source: &[f32],
    destination: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
) {
    let taps = 2 * radius + 1;
    let weight = 1.0 / taps as f32;
    for row in 0..height {
        for column in 0..width {
            let mut value = 0.0_f32;
            for offset in 0..taps {
                let sample = row as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, height as isize - 1) as usize;
                value += source[clamped * width + column];
            }
            destination[row * width + column] = value * weight;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_inner_outer_key_f32_controlled, inner_outer_key_frame_f32, matte_from_distance,
        ColourPurity, DespillStrength, InnerOuterKeyError, InnerOuterKeyGrade,
        InnerOuterKeyParameters, MatteSource, KEY_INNER_OUTER_DEFAULT_BACKGROUND_COLOUR,
        KEY_INNER_OUTER_DEFAULT_BACKGROUND_POINT, KEY_INNER_OUTER_DEFAULT_CHOKE_PIXELS,
        KEY_INNER_OUTER_DEFAULT_DESPILL, KEY_INNER_OUTER_DEFAULT_FEATHER_PIXELS,
        KEY_INNER_OUTER_DEFAULT_FOREGROUND_COLOUR, KEY_INNER_OUTER_DEFAULT_FOREGROUND_POINT,
        KEY_INNER_OUTER_MAX_CHOKE_PIXELS, KEY_INNER_OUTER_MAX_FEATHER_PIXELS,
    };

    fn defaults() -> InnerOuterKeyGrade {
        InnerOuterKeyGrade::documented_defaults().expect("documented defaults are valid")
    }

    fn grade(parameters: InnerOuterKeyParameters) -> InnerOuterKeyGrade {
        InnerOuterKeyGrade::new(parameters).expect("test parameters stay inside the ranges")
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for index in 0..width * height {
            let value = (index % 7) as f32 * 0.1;
            pixels.push([value, value, value, 1.0]);
        }
        pixels
    }

    fn alphas(pixels: &[[f32; 4]]) -> Vec<f32> {
        pixels.iter().map(|pixel| pixel[3]).collect()
    }

    fn with_source(parameters: &mut InnerOuterKeyParameters, source: MatteSource) {
        parameters.matte_source = source;
    }

    /// The margin a float comparison in these tests allows.
    const EPSILON: f32 = 1.0e-5;

    #[test]
    fn defaults_are_the_documented_parameters() {
        let grade = defaults();
        assert_eq!(
            grade.parameters().foreground_colour,
            KEY_INNER_OUTER_DEFAULT_FOREGROUND_COLOUR
        );
        assert_eq!(
            grade.parameters().background_colour,
            KEY_INNER_OUTER_DEFAULT_BACKGROUND_COLOUR
        );
        assert_eq!(
            grade.parameters().foreground_point,
            KEY_INNER_OUTER_DEFAULT_FOREGROUND_POINT
        );
        assert_eq!(
            grade.parameters().background_point,
            KEY_INNER_OUTER_DEFAULT_BACKGROUND_POINT
        );
        assert_eq!(grade.matte_source(), MatteSource::Foreground);
        assert_eq!(grade.colour_purity(), ColourPurity::Loose);
        assert_eq!(grade.choke(), KEY_INNER_OUTER_DEFAULT_CHOKE_PIXELS);
        assert_eq!(grade.feather(), KEY_INNER_OUTER_DEFAULT_FEATHER_PIXELS);
        assert_eq!(grade.despill().as_code(), KEY_INNER_OUTER_DEFAULT_DESPILL);
        assert!(grade.choke_radius() == 0);
        assert!(grade.feather_radius() == 0);
        assert_eq!(grade.halo(), 1);
        assert_eq!(grade.pass_count(), 3);
        assert_eq!(grade.draw_pass_count(), 2);
        assert!(!grade.is_identity());
    }

    #[test]
    fn authored_parameters_outside_the_documented_ranges_are_rejected() {
        let rejected = |parameters: InnerOuterKeyParameters, expected: InnerOuterKeyError| {
            assert_eq!(InnerOuterKeyGrade::new(parameters), Err(expected));
        };
        rejected(
            InnerOuterKeyParameters {
                foreground_colour: [0.0, 1.5, 0.0],
                ..Default::default()
            },
            InnerOuterKeyError::ForegroundColour,
        );
        rejected(
            InnerOuterKeyParameters {
                background_colour: [f32::NAN, 0.0, 0.0],
                ..Default::default()
            },
            InnerOuterKeyError::BackgroundColour,
        );
        rejected(
            InnerOuterKeyParameters {
                foreground_point: [f32::INFINITY, 0.0],
                ..Default::default()
            },
            InnerOuterKeyError::ForegroundPoint,
        );
        rejected(
            InnerOuterKeyParameters {
                background_point: [0.0, f32::NEG_INFINITY],
                ..Default::default()
            },
            InnerOuterKeyError::BackgroundPoint,
        );
        rejected(
            InnerOuterKeyParameters {
                choke: KEY_INNER_OUTER_MAX_CHOKE_PIXELS + 0.1,
                ..Default::default()
            },
            InnerOuterKeyError::Choke,
        );
        rejected(
            InnerOuterKeyParameters {
                choke: -KEY_INNER_OUTER_MAX_CHOKE_PIXELS - 0.1,
                ..Default::default()
            },
            InnerOuterKeyError::Choke,
        );
        rejected(
            InnerOuterKeyParameters { choke: f32::NAN, ..Default::default() },
            InnerOuterKeyError::Choke,
        );
        rejected(
            InnerOuterKeyParameters {
                feather: KEY_INNER_OUTER_MAX_FEATHER_PIXELS + 0.1,
                ..Default::default()
            },
            InnerOuterKeyError::Feather,
        );
        rejected(
            InnerOuterKeyParameters { feather: -0.1, ..Default::default() },
            InnerOuterKeyError::Feather,
        );
        // A point may leave 0..=1: that is the documented fallback to the
        // authored colour, so it is not a rejection.
        assert!(InnerOuterKeyGrade::new(InnerOuterKeyParameters {
            foreground_point: [1.5, -0.5],
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn error_messages_name_the_documented_ranges() {
        assert_eq!(
            InnerOuterKeyError::Choke.to_string(),
            "inner/outer key choke must be finite and within [-20, 20] pixels"
        );
        assert_eq!(InnerOuterKeyError::Choke.parameter(), "choke");
        assert_eq!(InnerOuterKeyError::Despill.parameter(), "despill");
    }

    #[test]
    fn the_estimate_samples_the_source_at_the_authored_point() {
        // Four texels with a distinct colour each. The centre point samples the
        // average of all four, so a pixel of that average is a full matte while
        // the authored green fallback would give no matte at all.
        let mut pixels = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
        ];
        let grade = grade(InnerOuterKeyParameters {
            foreground_point: [0.5, 0.5],
            despill: DespillStrength::Off,
            ..Default::default()
        });
        assert!(inner_outer_key_frame_f32(&mut pixels, 2, 2, &grade));
        // Every texel is 0.5..1.0 away from the [0.5, 0.5, 0.5] estimate, so the
        // loose ramp keeps only a sliver of coverage.
        assert!(alphas(&pixels).iter().all(|alpha| *alpha < 1.0));
        assert!(alphas(&pixels).iter().all(|alpha| *alpha > 0.0));
    }

    #[test]
    fn the_estimate_falls_back_to_the_authored_colour_outside_the_frame() {
        // Pixel 0 is red and pixel 1 is the authored green. With a point inside
        // the frame the estimate lands between red and green, so the red pixel
        // keeps partial coverage; with a point outside the frame the estimate is
        // the authored green and the red pixel is fully keyed out.
        let inside = {
            let mut pixels = vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]];
            let parameters = InnerOuterKeyParameters {
                foreground_point: [0.5, 0.0],
                despill: DespillStrength::Off,
                ..Default::default()
            };
            assert!(inner_outer_key_frame_f32(
                &mut pixels,
                2,
                1,
                &grade(parameters)
            ));
            pixels[0][3]
        };
        let outside = {
            let mut pixels = vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]];
            let parameters = InnerOuterKeyParameters {
                foreground_point: [-0.25, 0.0],
                ..Default::default()
            };
            assert!(inner_outer_key_frame_f32(
                &mut pixels,
                2,
                1,
                &grade(parameters)
            ));
            pixels[0][3]
        };
        assert!(inside > 0.0);
        assert_eq!(outside, 0.0);
        assert!((inside - matte_from_distance(0.5, 0.30, 0.30)).abs() < EPSILON);
    }

    #[test]
    fn purity_tiers_widen_and_narrow_the_gate() {
        let alpha_for = |purity: ColourPurity| {
            let mut pixels = vec![[0.32, 0.68, 0.32, 1.0]];
            let parameters = InnerOuterKeyParameters {
                foreground_point: [5.0, 5.0],
                colour_purity: purity,
                despill: DespillStrength::Off,
                ..Default::default()
            };
            assert!(inner_outer_key_frame_f32(
                &mut pixels,
                1,
                1,
                &grade(parameters)
            ));
            pixels[0][3]
        };
        let loose = alpha_for(ColourPurity::Loose);
        let standard = alpha_for(ColourPurity::Standard);
        let strict = alpha_for(ColourPurity::Strict);
        assert!(loose > standard);
        assert!(standard > strict);
        assert!((loose - matte_from_distance(0.32, 0.30, 0.30)).abs() < EPSILON);
        assert!((standard - matte_from_distance(0.32, 0.20, 0.15)).abs() < EPSILON);
        assert_eq!(strict, 0.0);
    }

    #[test]
    fn the_background_source_keys_the_background_estimate() {
        // The foreground estimate is green, the background estimate is black.
        // A black pixel keeps full coverage when the background side is keyed
        // and is fully keyed out when the foreground side is keyed.
        let alpha_for = |source: MatteSource| {
            let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]];
            let mut parameters = InnerOuterKeyParameters {
                foreground_point: [5.0, 5.0],
                background_point: [-5.0, -5.0],
                despill: DespillStrength::Off,
                ..Default::default()
            };
            with_source(&mut parameters, source);
            assert!(inner_outer_key_frame_f32(
                &mut pixels,
                1,
                1,
                &grade(parameters)
            ));
            pixels[0][3]
        };
        assert_eq!(alpha_for(MatteSource::Foreground), 0.0);
        assert_eq!(alpha_for(MatteSource::Background), 1.0);
    }

    #[test]
    fn positive_choke_dilates_and_negative_choke_erodes() {
        let keyed = || {
            vec![
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        };
        let parameters = |choke: f32| InnerOuterKeyParameters {
            foreground_point: [5.0, 5.0],
            choke,
            despill: DespillStrength::Off,
            ..Default::default()
        };
        let mut dilated = keyed();
        assert!(inner_outer_key_frame_f32(
            &mut dilated,
            3,
            1,
            &grade(parameters(1.0))
        ));
        assert_eq!(alphas(&dilated), vec![1.0, 1.0, 1.0]);

        let mut eroded = keyed();
        assert!(inner_outer_key_frame_f32(
            &mut eroded,
            3,
            1,
            &grade(parameters(-1.0))
        ));
        assert_eq!(alphas(&eroded), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn choke_and_feather_radii_round_like_the_shader() {
        let mut parameters = InnerOuterKeyParameters { choke: 0.4, ..Default::default() };
        assert_eq!(grade(parameters).choke_radius(), 0);
        assert!(!grade(parameters).chokes());
        assert_eq!(grade(parameters).draw_pass_count(), 2);
        parameters.choke = 0.5;
        assert_eq!(grade(parameters).choke_radius(), 1);
        parameters.choke = -2.5;
        assert_eq!(grade(parameters).choke_radius(), 3);
        assert!(grade(parameters).chokes());
        assert!(!grade(parameters).dilates());
        parameters.choke = KEY_INNER_OUTER_MAX_CHOKE_PIXELS;
        assert_eq!(grade(parameters).choke_radius(), 20);
        assert_eq!(grade(parameters).draw_pass_count(), 4);
        parameters.choke = 0.0;
        parameters.feather = 0.4;
        assert_eq!(grade(parameters).feather_radius(), 0);
        parameters.feather = 0.5;
        assert_eq!(grade(parameters).feather_radius(), 1);
        parameters.feather = KEY_INNER_OUTER_MAX_FEATHER_PIXELS;
        assert_eq!(grade(parameters).feather_radius(), 50);
        assert_eq!(grade(parameters).draw_pass_count(), 3);
        parameters.choke = 2.0;
        assert_eq!(grade(parameters).draw_pass_count(), 5);
    }

    #[test]
    fn feather_weights_are_a_box_average_with_clamped_edges() {
        // A one-pixel-wide matte spike in a five-pixel row, feathered radius 1:
        // each tap weighs 1/3 and the edge windows repeat the border sample.
        let mut pixels = vec![
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let parameters = InnerOuterKeyParameters {
            foreground_point: [5.0, 5.0],
            feather: 1.0,
            despill: DespillStrength::Off,
            ..Default::default()
        };
        assert!(inner_outer_key_frame_f32(
            &mut pixels,
            5,
            1,
            &grade(parameters)
        ));
        let alphas = alphas(&pixels);
        assert!((alphas[0] - 0.0).abs() < EPSILON);
        assert!((alphas[1] - 1.0 / 3.0).abs() < EPSILON);
        assert!((alphas[2] - 1.0 / 3.0).abs() < EPSILON);
        assert!((alphas[3] - 1.0 / 3.0).abs() < EPSILON);
        assert!((alphas[4] - 0.0).abs() < EPSILON);
    }

    #[test]
    fn despill_tiers_pull_the_colour_towards_the_estimate() {
        // The pixel sits at d = 0.45 from the authored green, so the loose ramp
        // leaves it at exactly half coverage and the despill weight is k_d / 2.
        let colour_for = |tier: DespillStrength| {
            let mut pixels = vec![[0.35, 0.55, 0.35, 1.0]];
            let parameters = InnerOuterKeyParameters {
                foreground_point: [5.0, 5.0],
                despill: tier,
                ..Default::default()
            };
            assert!(inner_outer_key_frame_f32(
                &mut pixels,
                1,
                1,
                &grade(parameters)
            ));
            pixels[0]
        };
        let off = colour_for(DespillStrength::Off);
        let light = colour_for(DespillStrength::Light);
        let strong = colour_for(DespillStrength::Strong);
        assert!((off[0] - 0.35).abs() < EPSILON);
        assert!((light[0] - (0.35 + (0.0 - 0.35) * 0.35 * 0.5)).abs() < EPSILON);
        assert!((strong[0] - (0.35 + (0.0 - 0.35) * 0.7 * 0.5)).abs() < EPSILON);
        assert!(strong[0] < light[0]);
        assert!((strong[1] - (0.55 + (1.0 - 0.55) * 0.7 * 0.5)).abs() < EPSILON);
    }

    #[test]
    fn a_transparent_source_pixel_stays_fully_transparent() {
        let mut pixels = vec![[0.0, 1.0, 0.0, 0.0]];
        assert!(inner_outer_key_frame_f32(&mut pixels, 1, 1, &defaults()));
        assert_eq!(pixels, vec![[0.0, 0.0, 0.0, 0.0]]);
    }

    #[test]
    fn the_matte_scales_the_source_alpha() {
        let mut pixels = vec![[0.0, 1.0, 0.0, 0.6]];
        assert!(inner_outer_key_frame_f32(&mut pixels, 1, 1, &defaults()));
        assert!((pixels[0][3] - 0.6).abs() < EPSILON);
    }

    #[test]
    fn the_defaults_are_not_an_identity() {
        // The default green estimate is sampled between a white and a black texel,
        // so both keep only a sliver of coverage: the defaults are not a no-op.
        let mut pixels = vec![[1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]];
        let source = pixels.clone();
        assert!(inner_outer_key_frame_f32(&mut pixels, 2, 1, &defaults()));
        assert_ne!(pixels, source);
        assert!(alphas(&pixels).iter().all(|alpha| *alpha < 1.0));
    }

    #[test]
    fn one_by_one_frames_sample_and_composite() {
        let mut pixels = vec![[0.0, 1.0, 0.0, 1.0]];
        assert!(inner_outer_key_frame_f32(&mut pixels, 1, 1, &defaults()));
        assert_eq!(pixels, vec![[0.0, 1.0, 0.0, 1.0]]);
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
            apply_inner_outer_key_f32_controlled(&mut pixels, 4, 2, &defaults(), &mut checkpoint),
            Ok(true)
        );
        // Entry, the first mask pixel, the end of the mask, the first composite
        // pixel and the end of the composite.
        assert_eq!(calls, 5);
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
            apply_inner_outer_key_f32_controlled(&mut pixels, 4, 2, &defaults(), &mut checkpoint),
            Err("cancelled")
        );
    }

    #[test]
    fn unaddressable_frames_are_rejected() {
        let mut pixels = frame(2, 2);
        assert!(!inner_outer_key_frame_f32(&mut pixels, 0, 2, &defaults()));
        assert!(!inner_outer_key_frame_f32(&mut pixels, 2, 0, &defaults()));
        assert!(!inner_outer_key_frame_f32(&mut pixels, 3, 2, &defaults()));
        assert!(!inner_outer_key_frame_f32(
            &mut pixels,
            usize::MAX,
            2,
            &defaults()
        ));
    }

    #[test]
    fn every_authored_parameter_changes_the_hash() {
        use std::hash::{Hash as _, Hasher as _};
        let hash = |grade: &InnerOuterKeyGrade| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            grade.hash_state(&mut hasher);
            "inner_outer_key".hash(&mut hasher);
            hasher.finish()
        };
        let baseline = hash(&defaults());
        assert_eq!(baseline, hash(&defaults()));
        let cases = [
            InnerOuterKeyParameters {
                foreground_colour: [0.1, 0.9, 0.1],
                ..Default::default()
            },
            InnerOuterKeyParameters {
                background_colour: [0.1, 0.1, 0.1],
                ..Default::default()
            },
            InnerOuterKeyParameters {
                foreground_point: [0.25, 0.75],
                ..Default::default()
            },
            InnerOuterKeyParameters {
                background_point: [0.75, 0.25],
                ..Default::default()
            },
            InnerOuterKeyParameters {
                matte_source: MatteSource::Background,
                ..Default::default()
            },
            InnerOuterKeyParameters {
                colour_purity: ColourPurity::Strict,
                ..Default::default()
            },
            InnerOuterKeyParameters { choke: 3.0, ..Default::default() },
            InnerOuterKeyParameters { feather: 4.0, ..Default::default() },
            InnerOuterKeyParameters {
                despill: DespillStrength::Strong,
                ..Default::default()
            },
        ];
        for parameters in cases {
            assert_ne!(baseline, hash(&grade(parameters)), "{parameters:?}");
        }
    }

    #[test]
    fn the_halo_grows_with_both_morphologies() {
        let mut parameters = InnerOuterKeyParameters::default();
        assert_eq!(grade(parameters).halo(), 1);
        parameters.choke = -4.0;
        assert_eq!(grade(parameters).halo(), 5);
        parameters.feather = 6.0;
        assert_eq!(grade(parameters).halo(), 11);
        parameters.choke = 0.4;
        parameters.feather = 0.4;
        assert_eq!(grade(parameters).halo(), 3);
    }

    #[test]
    fn gpu_controls_pack_every_uniform_cell() {
        let grade = grade(InnerOuterKeyParameters {
            foreground_point: [0.25, 0.75],
            background_point: [0.1, 0.2],
            colour_purity: ColourPurity::Strict,
            matte_source: MatteSource::Background,
            choke: -3.0,
            feather: 2.0,
            despill: DespillStrength::Strong,
            ..Default::default()
        });
        let matte = grade.matte_gpu_controls();
        assert_eq!(matte.params, [2.0, 1.0, 0.0, 0.0]);
        assert_eq!(matte.extra, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(matte.aces, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(matte.extra1, [0.25, 0.75, 0.1, 0.2]);
        assert_eq!(grade.choke_gpu_controls(), [3.0, 0.0, 0.0, 0.0]);
        assert_eq!(grade.feather_gpu_controls(), [2.0, 0.0, 0.0, 0.0]);
        let composite = grade.composite_gpu_controls();
        assert_eq!(composite.params, [2.0, 1.0, 0.0, 0.0]);
        assert_eq!(composite.extra, matte.extra);
        assert_eq!(composite.aces, matte.aces);
        assert_eq!(composite.extra1, matte.extra1);
    }

    #[test]
    fn the_enum_keys_round_trip() {
        for source in MatteSource::ALL {
            assert_eq!(MatteSource::from_key(source.as_key()), Some(source));
            assert_eq!(MatteSource::from_code(source.as_code()), Some(source));
        }
        for purity in ColourPurity::ALL {
            assert_eq!(ColourPurity::from_key(purity.as_key()), Some(purity));
            assert_eq!(ColourPurity::from_code(purity.as_code()), Some(purity));
        }
        for tier in DespillStrength::ALL {
            assert_eq!(DespillStrength::from_code(tier.as_code()), Some(tier));
        }
        assert_eq!(ColourPurity::Loose.tolerance(), 0.30);
        assert_eq!(ColourPurity::Loose.softness(), 0.30);
        assert_eq!(ColourPurity::Standard.tolerance(), 0.20);
        assert_eq!(ColourPurity::Standard.softness(), 0.15);
        assert_eq!(ColourPurity::Strict.tolerance(), 0.10);
        assert_eq!(ColourPurity::Strict.softness(), 0.06);
        assert_eq!(DespillStrength::Off.factor(), 0.0);
        assert_eq!(DespillStrength::Light.factor(), 0.35);
        assert_eq!(DespillStrength::Strong.factor(), 0.7);
    }
}
