//! 发光（Glow）
//!
//! A three-stage spatial effect built exactly like the GPU chain it mirrors:
//!
//! 1. **Extract.** Each straight-alpha source pixel is thresholded into a
//!    straight-alpha glow layer `[tint, weight]`: the RGB channels are the glow
//!    colour and the alpha channel is the glow mask. Transparent source pixels
//!    contribute nothing, so hidden RGB cannot leak into the glow.
//! 2. **Blur.** The glow layer is box blurred with the shared
//!    [`crate::box_blur::box_blur_frame_f32`] kernel, which premultiplies its own
//!    input and returns straight alpha again. That is the same contract the GPU
//!    gets from the first/last `fs_box_blur` pass flags, so the two paths stay
//!    bit-comparable.
//! 3. **Composite.** The blurred layer becomes
//!    `contribution = glow.rgb * glow.a * intensity`, is combined with the
//!    authored [`BlendMode`] operation, and is written back with the source
//!    alpha untouched.
//!
//! # Uniform packing
//!
//! The GPU extract pass needs more scalars than the shared twelve-cell
//! [`SpatialPassUniform`], which is why the uniform gained its second spare cell
//! (`extra1`). [`GlowGrade::extract_gpu_controls`] packs:
//!
//! - `params = [threshold / 255, luma_r, luma_g, luma_b]`
//! - `extra  = [based_on_index, colours_index, midpoint, color_phase / 360]`
//! - `aces   = [color_a.rgb, color_loops]`
//! - `extra1 = [color_b.rgb, 0]`
//!
//! and [`GlowGrade::composite_gpu_controls`] packs the merge:
//!
//! - `params = [intensity, blend_mode_index, composite_index, 0]`, the remaining
//!   cells zero.
//!
//! The indices are zero-based [`GlowBasedOn::as_index`], [`GlowColors::as_index`]
//! and [`GlowComposite::as_index`] values; `blend_mode_index` replicates the
//! renderer compositor's numeric [`BlendMode`] ids (`gpu_compositor.rs`), which
//! differ from [`BlendMode`]'s declaration order only because `Dissolve` has no
//! glow menu entry.
//!
//! # Decisions left open by the design brief
//!
//! - The luminance coefficients live on the grade (like [`crate::ExtractGrade`])
//!   instead of being passed into the frame kernel. The CPU dispatch path has no
//!   working colour space available, so the grade must carry them; the GPU reads
//!   the same coefficients out of `params.yzw`.
//! - The authored 1–25 / 1–3 menus are plain float properties, so the graph
//!   builder rounds an animated value to the nearest menu entry and lets
//!   [`GlowOperation::from_menu`] (and its siblings) fail closed outside the
//!   authored range.
//! - The checklist lists `based_on` as implicit, so the authoring surface has no
//!   property for it and the builder always mirrors luminance. [`GlowGrade`] and
//!   the extract uniform still carry all three modes for callers that build a
//!   grade directly.
//! - `midpoint` is validated into `[0.009995, 0.99]` at construction, so the
//!   midpoint ramp never divides by zero and the CPU and the shader both use the
//!   stored value unclamped.
//! - The composite leaves the source alpha untouched and does not clamp the
//!   blended RGB: like every other spatial effect it preserves extended range.
//! - The kernel materialises one extract buffer and blurs it in place; the
//!   declared peak scratch of two frames (`render_op_f32_scratch_frames`) is the
//!   brief's conservative ordering bound.

use crate::box_blur::{box_blur_frame_f32, BoxBlurDirection};
use mondrian_core::BlendMode;

/// The largest authored threshold, in 8-bit target levels.
pub const GLOW_AUTHOR_MAX_THRESHOLD: f32 = 255.0;
/// The largest authored radius, in pixels.
pub const GLOW_AUTHOR_MAX_RADIUS_PIXELS: f32 = 1000.0;
/// The largest authored strength.
pub const GLOW_AUTHOR_MAX_STRENGTH: f32 = 255.0;
/// The divisor that turns the authored strength into the blend intensity.
pub const GLOW_INTENSITY_SCALE: f32 = 100.0;
/// The smallest authored colour loop count.
pub const GLOW_AUTHOR_MIN_COLOR_LOOPS: f32 = 1.0;
/// The largest authored colour loop count.
pub const GLOW_AUTHOR_MAX_COLOR_LOOPS: f32 = 4.0;
/// The smallest authored A/B midpoint. Kept strictly above zero so the ramp is
/// always defined.
pub const GLOW_AUTHOR_MIN_MIDPOINT: f32 = 0.009_995;
/// The largest authored A/B midpoint. Kept strictly below one so the ramp is
/// always defined.
pub const GLOW_AUTHOR_MAX_MIDPOINT: f32 = 0.99;
/// One full turn of the colour-cycle phase, in degrees.
pub const GLOW_AUTHOR_MAX_COLOR_PHASE_DEGREES: f32 = 360.0;
/// The default authored threshold.
pub const GLOW_DEFAULT_THRESHOLD: f32 = 153.0;
/// The default authored radius, in pixels.
pub const GLOW_DEFAULT_RADIUS_PIXELS: f32 = 10.0;
/// The default authored strength.
pub const GLOW_DEFAULT_STRENGTH: f32 = 1.0;
/// The default authored colour loop count.
pub const GLOW_DEFAULT_COLOR_LOOPS: f32 = 1.0;
/// The default authored colour phase, in degrees.
pub const GLOW_DEFAULT_COLOR_PHASE: f32 = 0.0;
/// The default authored A/B midpoint.
pub const GLOW_DEFAULT_MIDPOINT: f32 = 0.5;
/// The number of box blur iterations the glow layer receives.
pub const GLOW_BLUR_ITERATIONS: u32 = 2;

/// Which source channel drives the glow mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlowBasedOn {
    /// Threshold the luminance.
    Color,
    /// Use the source coverage (alpha) directly.
    Alpha,
    /// Take the stronger of the luminance threshold and the source coverage.
    ColorAndAlpha,
}

impl GlowBasedOn {
    /// Every variant, in menu order.
    pub const ALL: [Self; 3] = [Self::Color, Self::Alpha, Self::ColorAndAlpha];

    /// The stable serialised key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Alpha => "alpha",
            Self::ColorAndAlpha => "color_and_alpha",
        }
    }

    /// Parses a stable serialised key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "color" => Some(Self::Color),
            "alpha" => Some(Self::Alpha),
            "color_and_alpha" => Some(Self::ColorAndAlpha),
            _ => None,
        }
    }

    /// The Chinese parameter label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Color => "颜色",
            Self::Alpha => "透明度",
            Self::ColorAndAlpha => "颜色和透明度",
        }
    }

    /// The zero-based index packed into the shader's `extra.x` cell.
    #[must_use]
    pub const fn as_index(self) -> u32 {
        match self {
            Self::Color => 0,
            Self::Alpha => 1,
            Self::ColorAndAlpha => 2,
        }
    }
}

/// How the glow layer is combined with the original.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlowComposite {
    /// Blend the glow over the original.
    OnTop,
    /// Blend the original over the glow.
    Behind,
    /// Ignore the original colour; write the glow contribution alone.
    None,
}

impl GlowComposite {
    /// Every variant, in menu order.
    pub const ALL: [Self; 3] = [Self::OnTop, Self::Behind, Self::None];

    /// The one-based authored menu value.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        1 + self.as_index()
    }

    /// Parses a one-based authored menu value; anything outside `1..=3` yields
    /// `None`.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::OnTop),
            2 => Some(Self::Behind),
            3 => Some(Self::None),
            _ => None,
        }
    }

    /// The Chinese parameter label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::OnTop => "上层",
            Self::Behind => "下层",
            Self::None => "无",
        }
    }

    /// The zero-based index packed into the shader's `params.z` cell.
    #[must_use]
    pub const fn as_index(self) -> u32 {
        match self {
            Self::OnTop => 0,
            Self::Behind => 1,
            Self::None => 2,
        }
    }
}

/// Which colour the glow layer carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlowColors {
    /// Keep the source colour.
    Original,
    /// Ramp between colour A and colour B over the source luminance.
    AAndB,
    /// Cycle colour A and colour B over the luminance and the phase.
    Cycle,
}

impl GlowColors {
    /// Every variant, in menu order.
    pub const ALL: [Self; 3] = [Self::Original, Self::AAndB, Self::Cycle];

    /// The one-based authored menu value.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        1 + self.as_index()
    }

    /// Parses a one-based authored menu value; anything outside `1..=3` yields
    /// `None`.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Original),
            2 => Some(Self::AAndB),
            3 => Some(Self::Cycle),
            _ => None,
        }
    }

    /// The Chinese parameter label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Original => "原始颜色",
            Self::AAndB => "A和B颜色",
            Self::Cycle => "色彩循环",
        }
    }

    /// The zero-based index packed into the shader's `extra.y` cell.
    #[must_use]
    pub const fn as_index(self) -> u32 {
        match self {
            Self::Original => 0,
            Self::AAndB => 1,
            Self::Cycle => 2,
        }
    }
}

/// Which axes the glow layer is blurred along.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlowDimensions {
    /// Horizontal only.
    Horizontal,
    /// Vertical only.
    Vertical,
    /// Horizontal and vertical.
    Both,
}

impl GlowDimensions {
    /// Every variant, in menu order.
    pub const ALL: [Self; 3] = [Self::Horizontal, Self::Vertical, Self::Both];

    /// The one-based authored menu value.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        1 + self.as_index()
    }

    /// Parses a one-based authored menu value; anything outside `1..=3` yields
    /// `None`.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Horizontal),
            2 => Some(Self::Vertical),
            3 => Some(Self::Both),
            _ => None,
        }
    }

    /// The Chinese parameter label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "水平",
            Self::Vertical => "垂直",
            Self::Both => "水平+垂直",
        }
    }

    /// The zero-based index of the variant.
    #[must_use]
    pub const fn as_index(self) -> u32 {
        match self {
            Self::Horizontal => 0,
            Self::Vertical => 1,
            Self::Both => 2,
        }
    }

    /// Whether the horizontal axis is blurred.
    #[must_use]
    pub const fn blurs_horizontal(self) -> bool {
        matches!(self, Self::Horizontal | Self::Both)
    }

    /// Whether the vertical axis is blurred.
    #[must_use]
    pub const fn blurs_vertical(self) -> bool {
        matches!(self, Self::Vertical | Self::Both)
    }

    /// The number of blur passes one iteration expands into: two axes for
    /// [`Self::Both`], one otherwise.
    #[must_use]
    pub const fn axis_count(self) -> u32 {
        self.blurs_horizontal() as u32 + self.blurs_vertical() as u32
    }

    /// The matching shared box blur direction.
    #[must_use]
    pub const fn blur_direction(self) -> BoxBlurDirection {
        match self {
            Self::Horizontal => BoxBlurDirection::Horizontal,
            Self::Vertical => BoxBlurDirection::Vertical,
            Self::Both => BoxBlurDirection::HorizontalAndVertical,
        }
    }
}

/// One entry of the 25-slot glow operation menu, mapped onto [`BlendMode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlowOperation {
    /// Normal.
    Normal,
    /// Multiply.
    Multiply,
    /// Screen.
    Screen,
    /// Overlay.
    Overlay,
    /// Darken.
    Darken,
    /// Lighten.
    Lighten,
    /// Color dodge.
    ColorDodge,
    /// Color burn.
    ColorBurn,
    /// Hard light.
    HardLight,
    /// Soft light.
    SoftLight,
    /// Difference.
    Difference,
    /// Exclusion.
    Exclusion,
    /// Subtract.
    Subtract,
    /// Darker color.
    DarkerColor,
    /// Lighter color.
    LighterColor,
    /// Linear burn.
    LinearBurn,
    /// Linear dodge.
    LinearDodge,
    /// Vivid light.
    VividLight,
    /// Linear light.
    LinearLight,
    /// Pin light.
    PinLight,
    /// Hard mix.
    HardMix,
    /// Divide.
    Divide,
    /// Hue.
    Hue,
    /// Saturation.
    Saturation,
    /// Color.
    Color,
}

impl GlowOperation {
    /// Every variant, in authored menu order.
    pub const ALL: [Self; 25] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::Darken,
        Self::Lighten,
        Self::ColorDodge,
        Self::ColorBurn,
        Self::HardLight,
        Self::SoftLight,
        Self::Difference,
        Self::Exclusion,
        Self::Subtract,
        Self::DarkerColor,
        Self::LighterColor,
        Self::LinearBurn,
        Self::LinearDodge,
        Self::VividLight,
        Self::LinearLight,
        Self::PinLight,
        Self::HardMix,
        Self::Divide,
        Self::Hue,
        Self::Saturation,
        Self::Color,
    ];

    /// The one-based authored menu value.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self as u32 + 1
    }

    /// Parses a one-based authored menu value; anything outside `1..=25` yields
    /// `None`.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Normal),
            2 => Some(Self::Multiply),
            3 => Some(Self::Screen),
            4 => Some(Self::Overlay),
            5 => Some(Self::Darken),
            6 => Some(Self::Lighten),
            7 => Some(Self::ColorDodge),
            8 => Some(Self::ColorBurn),
            9 => Some(Self::HardLight),
            10 => Some(Self::SoftLight),
            11 => Some(Self::Difference),
            12 => Some(Self::Exclusion),
            13 => Some(Self::Subtract),
            14 => Some(Self::DarkerColor),
            15 => Some(Self::LighterColor),
            16 => Some(Self::LinearBurn),
            17 => Some(Self::LinearDodge),
            18 => Some(Self::VividLight),
            19 => Some(Self::LinearLight),
            20 => Some(Self::PinLight),
            21 => Some(Self::HardMix),
            22 => Some(Self::Divide),
            23 => Some(Self::Hue),
            24 => Some(Self::Saturation),
            25 => Some(Self::Color),
            _ => None,
        }
    }

    /// The blend operation this menu entry selects.
    #[must_use]
    pub const fn blend_mode(self) -> BlendMode {
        match self {
            Self::Normal => BlendMode::Normal,
            Self::Multiply => BlendMode::Multiply,
            Self::Screen => BlendMode::Screen,
            Self::Overlay => BlendMode::Overlay,
            Self::Darken => BlendMode::Darken,
            Self::Lighten => BlendMode::Lighten,
            Self::ColorDodge => BlendMode::ColorDodge,
            Self::ColorBurn => BlendMode::ColorBurn,
            Self::HardLight => BlendMode::HardLight,
            Self::SoftLight => BlendMode::SoftLight,
            Self::Difference => BlendMode::Difference,
            Self::Exclusion => BlendMode::Exclusion,
            Self::Subtract => BlendMode::Subtract,
            Self::DarkerColor => BlendMode::DarkerColor,
            Self::LighterColor => BlendMode::LighterColor,
            Self::LinearBurn => BlendMode::LinearBurn,
            Self::LinearDodge => BlendMode::LinearDodge,
            Self::VividLight => BlendMode::VividLight,
            Self::LinearLight => BlendMode::LinearLight,
            Self::PinLight => BlendMode::PinLight,
            Self::HardMix => BlendMode::HardMix,
            Self::Divide => BlendMode::Divide,
            Self::Hue => BlendMode::Hue,
            Self::Saturation => BlendMode::Saturation,
            Self::Color => BlendMode::Color,
        }
    }

    /// The blend operation's numeric shader id, matching the renderer
    /// compositor's `gpu_blend_mode_id` table.
    #[must_use]
    pub fn blend_mode_index(self) -> u32 {
        blend_mode_index(self.blend_mode())
    }
}

/// The numeric shader id of a [`BlendMode`].
///
/// Kept in sync with `gpu_compositor.rs`'s `gpu_blend_mode_id`; the spatial
/// composite shader ports the same table into `blend_channel`/`blend_rgb`.
pub(crate) fn blend_mode_index(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Dissolve => 1,
        BlendMode::Multiply => 2,
        BlendMode::Screen => 3,
        BlendMode::Overlay => 4,
        BlendMode::Darken => 5,
        BlendMode::Lighten => 6,
        BlendMode::ColorDodge => 7,
        BlendMode::ColorBurn => 8,
        BlendMode::HardLight => 9,
        BlendMode::SoftLight => 10,
        BlendMode::Difference => 11,
        BlendMode::Exclusion => 12,
        BlendMode::Subtract => 13,
        BlendMode::DarkerColor => 14,
        BlendMode::LighterColor => 15,
        BlendMode::LinearBurn => 16,
        BlendMode::LinearDodge => 17,
        BlendMode::VividLight => 18,
        BlendMode::LinearLight => 19,
        BlendMode::PinLight => 20,
        BlendMode::HardMix => 21,
        BlendMode::Divide => 22,
        BlendMode::Hue => 23,
        BlendMode::Saturation => 24,
        BlendMode::Color => 25,
        BlendMode::Luminosity => 26,
    }
}

/// The rejection reason of a [`GlowGrade`] field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlowError {
    /// The threshold is outside `[0, 255]` or not finite.
    Threshold,
    /// The radius is outside `[0, 1000]` or not finite.
    Radius,
    /// The strength is outside `[0, 255]` or not finite.
    Strength,
    /// The colour loop count is outside `[1, 4]` or not finite.
    ColorLoops,
    /// The colour phase is not finite.
    ColorPhase,
    /// The A/B midpoint is outside `[0.009995, 0.99]` or not finite.
    Midpoint,
    /// Colour A is not a finite RGB triple.
    ColorA,
    /// Colour B is not a finite RGB triple.
    ColorB,
    /// The luminance coefficients are not finite and positive.
    Luminance,
}

impl std::fmt::Display for GlowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Threshold => "glow threshold must be a finite value inside [0, 255]",
            Self::Radius => "glow radius must be a finite value inside [0, 1000]",
            Self::Strength => "glow strength must be a finite value inside [0, 255]",
            Self::ColorLoops => "glow colour loops must be a finite value inside [1, 4]",
            Self::ColorPhase => "glow colour phase must be a finite number of degrees",
            Self::Midpoint => "glow A/B midpoint must be a finite value inside [0.009995, 0.99]",
            Self::ColorA => "glow colour A must be a finite rgb triple",
            Self::ColorB => "glow colour B must be a finite rgb triple",
            Self::Luminance => "glow luminance coefficients must be finite and positive",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for GlowError {}

/// The authored parameters of one glow operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowGrade {
    based_on: GlowBasedOn,
    threshold: f32,
    radius: f32,
    strength: f32,
    composite_original: GlowComposite,
    operation: GlowOperation,
    colors: GlowColors,
    color_loops: f32,
    color_phase: f32,
    midpoint: f32,
    color_a: [f32; 3],
    color_b: [f32; 3],
    dimensions: GlowDimensions,
    luminance: [f32; 3],
}

impl GlowGrade {
    /// Builds a grade from authored parameters and explicit luminance
    /// coefficients.
    ///
    /// # Errors
    ///
    /// Returns the [`GlowError`] of the first field outside its documented
    /// range.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        based_on: GlowBasedOn,
        threshold: f32,
        radius: f32,
        strength: f32,
        composite_original: GlowComposite,
        operation: GlowOperation,
        colors: GlowColors,
        color_loops: f32,
        color_phase: f32,
        midpoint: f32,
        color_a: [f32; 3],
        color_b: [f32; 3],
        dimensions: GlowDimensions,
        luminance: [f32; 3],
    ) -> Result<Self, GlowError> {
        if !threshold.is_finite() || !(0.0..=GLOW_AUTHOR_MAX_THRESHOLD).contains(&threshold) {
            return Err(GlowError::Threshold);
        }
        if !radius.is_finite() || !(0.0..=GLOW_AUTHOR_MAX_RADIUS_PIXELS).contains(&radius) {
            return Err(GlowError::Radius);
        }
        if !strength.is_finite() || !(0.0..=GLOW_AUTHOR_MAX_STRENGTH).contains(&strength) {
            return Err(GlowError::Strength);
        }
        if !color_loops.is_finite()
            || !(GLOW_AUTHOR_MIN_COLOR_LOOPS..=GLOW_AUTHOR_MAX_COLOR_LOOPS).contains(&color_loops)
        {
            return Err(GlowError::ColorLoops);
        }
        if !color_phase.is_finite() {
            return Err(GlowError::ColorPhase);
        }
        if !midpoint.is_finite()
            || !(GLOW_AUTHOR_MIN_MIDPOINT..=GLOW_AUTHOR_MAX_MIDPOINT).contains(&midpoint)
        {
            return Err(GlowError::Midpoint);
        }
        if color_a.iter().any(|value| !value.is_finite()) {
            return Err(GlowError::ColorA);
        }
        if color_b.iter().any(|value| !value.is_finite()) {
            return Err(GlowError::ColorB);
        }
        let total: f32 = luminance.iter().sum();
        if luminance.iter().any(|value| !value.is_finite()) || total <= 0.0 {
            return Err(GlowError::Luminance);
        }
        Ok(Self {
            based_on,
            threshold,
            radius,
            strength,
            composite_original,
            operation,
            colors,
            color_loops,
            color_phase,
            midpoint,
            color_a,
            color_b,
            dimensions,
            luminance,
        })
    }

    /// Builds a grade whose luminance comes from the working colour space.
    ///
    /// # Errors
    ///
    /// Propagates the field checks of [`Self::new`].
    #[allow(clippy::too_many_arguments)]
    pub fn for_space(
        based_on: GlowBasedOn,
        threshold: f32,
        radius: f32,
        strength: f32,
        composite_original: GlowComposite,
        operation: GlowOperation,
        colors: GlowColors,
        color_loops: f32,
        color_phase: f32,
        midpoint: f32,
        color_a: [f32; 3],
        color_b: [f32; 3],
        dimensions: GlowDimensions,
        space: mondrian_core::WorkingColorSpace,
    ) -> Result<Self, GlowError> {
        Self::new(
            based_on,
            threshold,
            radius,
            strength,
            composite_original,
            operation,
            colors,
            color_loops,
            color_phase,
            midpoint,
            color_a,
            color_b,
            dimensions,
            space.luminance_coefficients(),
        )
    }

    /// The glow mask source.
    #[must_use]
    pub const fn based_on(&self) -> GlowBasedOn {
        self.based_on
    }

    /// The authored threshold in 8-bit target levels.
    #[must_use]
    pub const fn threshold(&self) -> f32 {
        self.threshold
    }

    /// The authored blur radius, in pixels.
    #[must_use]
    pub const fn radius(&self) -> f32 {
        self.radius
    }

    /// The authored strength.
    #[must_use]
    pub const fn strength(&self) -> f32 {
        self.strength
    }

    /// The blend intensity: `strength / 100`.
    #[must_use]
    pub fn intensity(&self) -> f32 {
        self.strength / GLOW_INTENSITY_SCALE
    }

    /// How the glow is combined with the original.
    #[must_use]
    pub const fn composite_original(&self) -> GlowComposite {
        self.composite_original
    }

    /// The blend operation.
    #[must_use]
    pub const fn operation(&self) -> GlowOperation {
        self.operation
    }

    /// The glow colour source.
    #[must_use]
    pub const fn colors(&self) -> GlowColors {
        self.colors
    }

    /// The authored colour loop count.
    #[must_use]
    pub const fn color_loops(&self) -> f32 {
        self.color_loops
    }

    /// The authored colour phase, in degrees.
    #[must_use]
    pub const fn color_phase(&self) -> f32 {
        self.color_phase
    }

    /// The authored A/B midpoint.
    #[must_use]
    pub const fn midpoint(&self) -> f32 {
        self.midpoint
    }

    /// Colour A.
    #[must_use]
    pub const fn color_a(&self) -> [f32; 3] {
        self.color_a
    }

    /// Colour B.
    #[must_use]
    pub const fn color_b(&self) -> [f32; 3] {
        self.color_b
    }

    /// The blurred axes.
    #[must_use]
    pub const fn dimensions(&self) -> GlowDimensions {
        self.dimensions
    }

    /// The luminance coefficients used by the mask and the A/B ramp.
    #[must_use]
    pub const fn luminance_coefficients(&self) -> [f32; 3] {
        self.luminance
    }

    /// Whether the grade is a no-op and the graph builder may elide it.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.radius == 0.0 || self.strength == 0.0
    }

    /// The luminance of one straight-alpha pixel under the grade's working
    /// space.
    #[must_use]
    pub fn luminance_of(&self, pixel: [f32; 4]) -> f32 {
        pixel[0] * self.luminance[0] + pixel[1] * self.luminance[1] + pixel[2] * self.luminance[2]
    }

    /// The A/B ramp position of one luminance.
    ///
    /// The midpoint is validated into `[0.009995, 0.99]`, so both branches are
    /// always defined.
    #[must_use]
    pub fn ab_ramp(&self, luminance: f32) -> f32 {
        if luminance < self.midpoint {
            0.5 * luminance / self.midpoint
        } else {
            0.5 + 0.5 * (luminance - self.midpoint) / (1.0 - self.midpoint)
        }
    }

    /// Extracts the straight-alpha glow layer of one source pixel.
    ///
    /// A pixel without coverage contributes nothing: hidden RGB must not bleed
    /// into the bloom.
    #[must_use]
    pub fn extract(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return [0.0; 4];
        }
        let luminance = self.luminance_of(pixel);
        let threshold = (self.threshold / GLOW_AUTHOR_MAX_THRESHOLD).clamp(0.0, 1.0);
        let colour_weight = if threshold >= 1.0 {
            0.0
        } else {
            ((luminance - threshold) / (1.0 - threshold)).clamp(0.0, 1.0)
        };
        let weight = match self.based_on {
            GlowBasedOn::Color => colour_weight,
            GlowBasedOn::Alpha => pixel[3],
            GlowBasedOn::ColorAndAlpha => colour_weight.max(pixel[3]),
        };
        let colour = match self.based_on {
            GlowBasedOn::Alpha => [1.0, 1.0, 1.0],
            GlowBasedOn::Color | GlowBasedOn::ColorAndAlpha => [pixel[0], pixel[1], pixel[2]],
        };
        let tint = match self.colors {
            GlowColors::Original => colour,
            GlowColors::AAndB => mix_rgb(self.color_a, self.color_b, self.ab_ramp(luminance)),
            GlowColors::Cycle => {
                let phase = self.color_phase / GLOW_AUTHOR_MAX_COLOR_PHASE_DEGREES;
                mix_rgb(
                    self.color_a,
                    self.color_b,
                    fract(phase + luminance * self.color_loops),
                )
            }
        };
        [tint[0], tint[1], tint[2], weight]
    }

    /// Composites one extracted-and-blurred glow pixel back onto the source.
    ///
    /// The source alpha is preserved exactly, and a source pixel without
    /// coverage is returned untouched.
    #[must_use]
    pub fn composite(&self, source: [f32; 4], glow: [f32; 4]) -> [f32; 4] {
        if source[3] <= 0.0 {
            return source;
        }
        let intensity = self.intensity();
        let contribution = [
            glow[0] * glow[3] * intensity,
            glow[1] * glow[3] * intensity,
            glow[2] * glow[3] * intensity,
        ];
        let source_rgb = [source[0], source[1], source[2]];
        let blended = match self.composite_original {
            GlowComposite::OnTop => crate::adjustment::blend_mode_rgb(
                self.operation.blend_mode(),
                source_rgb,
                contribution,
            ),
            GlowComposite::Behind => crate::adjustment::blend_mode_rgb(
                self.operation.blend_mode(),
                contribution,
                source_rgb,
            ),
            GlowComposite::None => contribution,
        };
        [blended[0], blended[1], blended[2], source[3]]
    }

    /// The uniform payload of the extract pass.
    ///
    /// See the module documentation for the exact cell layout.
    #[must_use]
    pub fn extract_gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.threshold / GLOW_AUTHOR_MAX_THRESHOLD,
                self.luminance[0],
                self.luminance[1],
                self.luminance[2],
            ],
            [
                self.based_on.as_index() as f32,
                self.colors.as_index() as f32,
                self.midpoint,
                self.color_phase / GLOW_AUTHOR_MAX_COLOR_PHASE_DEGREES,
            ],
            [
                self.color_a[0],
                self.color_a[1],
                self.color_a[2],
                self.color_loops,
            ],
            [self.color_b[0], self.color_b[1], self.color_b[2], 0.0],
        )
    }

    /// The uniform payload of the composite pass.
    ///
    /// `params = [intensity, blend_mode_index, composite_index, 0]`; the second
    /// spare cell carries colour B for the extract pass, so the merge leaves it
    /// zero.
    #[must_use]
    pub fn composite_gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.intensity(),
                self.operation.blend_mode_index() as f32,
                self.composite_original.as_index() as f32,
                0.0,
            ],
            [0.0; 4],
            [0.0; 4],
            [0.0; 4],
        )
    }

    /// Hashes every authored scalar so a cache key cannot alias two grades.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.based_on.as_index().hash(state);
        self.threshold.to_bits().hash(state);
        self.radius.to_bits().hash(state);
        self.strength.to_bits().hash(state);
        self.composite_original.as_index().hash(state);
        self.operation.as_menu().hash(state);
        self.colors.as_index().hash(state);
        self.color_loops.to_bits().hash(state);
        self.color_phase.to_bits().hash(state);
        self.midpoint.to_bits().hash(state);
        for value in self.color_a {
            value.to_bits().hash(state);
        }
        for value in self.color_b {
            value.to_bits().hash(state);
        }
        self.dimensions.as_index().hash(state);
        for value in self.luminance {
            value.to_bits().hash(state);
        }
    }
}

/// Straight-alpha rgb mix, written as the shader's `mix` so both sides evaluate
/// the same expression.
fn mix_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] * (1.0 - t) + b[0] * t,
        a[1] * (1.0 - t) + b[1] * t,
        a[2] * (1.0 - t) + b[2] * t,
    ]
}

/// The fractional part of a value, mirroring the shader's `fract`.
fn fract(value: f32) -> f32 {
    value - value.floor()
}

/// Apply glow to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn glow_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &GlowGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_glow_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed kernel behind [`glow_frame_f32`].
pub(crate) fn apply_glow_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &GlowGrade,
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
    let mut glow = Vec::with_capacity(pixel_count);
    for (index, pixel) in frame.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        glow.push(grade.extract(*pixel));
    }
    if !box_blur_frame_f32(
        &mut glow,
        width,
        height,
        grade.radius,
        GLOW_BLUR_ITERATIONS,
        grade.dimensions.blur_direction(),
        true,
    ) {
        return Ok(false);
    }
    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let index = row * width + column;
                *pixel = grade.composite(*pixel, glow[index]);
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

const CHECKPOINT_STRIDE: usize = 4096;

#[cfg(test)]
mod tests {
    use super::{
        glow_frame_f32, GlowBasedOn, GlowColors, GlowComposite, GlowDimensions, GlowError,
        GlowGrade, GlowOperation, GLOW_AUTHOR_MAX_COLOR_LOOPS, GLOW_AUTHOR_MAX_COLOR_PHASE_DEGREES,
        GLOW_AUTHOR_MAX_MIDPOINT, GLOW_AUTHOR_MAX_RADIUS_PIXELS, GLOW_AUTHOR_MAX_STRENGTH,
        GLOW_AUTHOR_MAX_THRESHOLD, GLOW_AUTHOR_MIN_COLOR_LOOPS, GLOW_AUTHOR_MIN_MIDPOINT,
        GLOW_DEFAULT_COLOR_LOOPS, GLOW_DEFAULT_COLOR_PHASE, GLOW_DEFAULT_MIDPOINT,
        GLOW_DEFAULT_RADIUS_PIXELS, GLOW_DEFAULT_STRENGTH, GLOW_DEFAULT_THRESHOLD,
    };

    const WORKING: [f32; 3] = [0.2126, 0.7152, 0.0722];

    /// Build a frame from a per-pixel recipe, row by row.
    fn frame<F: Fn(usize, usize) -> [f32; 4]>(
        width: usize,
        height: usize,
        fill: F,
    ) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                pixels.push(fill(column, row));
            }
        }
        pixels
    }

    fn bits(pixels: &[[f32; 4]]) -> Vec<[u32; 4]> {
        pixels.iter().map(|pixel| pixel.map(f32::to_bits)).collect()
    }

    fn default_grade() -> GlowGrade {
        GlowGrade::new(
            GlowBasedOn::Color,
            GLOW_DEFAULT_THRESHOLD,
            GLOW_DEFAULT_RADIUS_PIXELS,
            GLOW_DEFAULT_STRENGTH,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            GLOW_DEFAULT_COLOR_LOOPS,
            GLOW_DEFAULT_COLOR_PHASE,
            GLOW_DEFAULT_MIDPOINT,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            GlowDimensions::Both,
            WORKING,
        )
        .expect("the documented defaults form a valid grade")
    }

    #[allow(clippy::too_many_arguments)]
    fn custom_grade(
        based_on: GlowBasedOn,
        threshold: f32,
        radius: f32,
        strength: f32,
        composite_original: GlowComposite,
        operation: GlowOperation,
        colors: GlowColors,
        color_loops: f32,
        color_phase: f32,
        midpoint: f32,
        color_a: [f32; 3],
        color_b: [f32; 3],
        dimensions: GlowDimensions,
    ) -> GlowGrade {
        GlowGrade::new(
            based_on,
            threshold,
            radius,
            strength,
            composite_original,
            operation,
            colors,
            color_loops,
            color_phase,
            midpoint,
            color_a,
            color_b,
            dimensions,
            WORKING,
        )
        .expect("a valid grade")
    }

    #[test]
    fn the_documented_defaults_are_stored_verbatim() {
        let grade = default_grade();
        assert_eq!(grade.based_on(), GlowBasedOn::Color);
        assert_eq!(grade.threshold(), GLOW_DEFAULT_THRESHOLD);
        assert_eq!(grade.radius(), GLOW_DEFAULT_RADIUS_PIXELS);
        assert_eq!(grade.strength(), GLOW_DEFAULT_STRENGTH);
        assert_eq!(grade.composite_original(), GlowComposite::OnTop);
        assert_eq!(grade.operation(), GlowOperation::Screen);
        assert_eq!(grade.colors(), GlowColors::Original);
        assert_eq!(grade.color_loops(), GLOW_DEFAULT_COLOR_LOOPS);
        assert_eq!(grade.color_phase(), GLOW_DEFAULT_COLOR_PHASE);
        assert_eq!(grade.midpoint(), GLOW_DEFAULT_MIDPOINT);
        assert_eq!(grade.color_a(), [1.0, 1.0, 1.0]);
        assert_eq!(grade.color_b(), [0.0, 0.0, 0.0]);
        assert_eq!(grade.dimensions(), GlowDimensions::Both);
        assert_eq!(grade.luminance_coefficients(), WORKING);
        assert_eq!(grade.intensity(), 0.01);
        assert!(!grade.is_identity());
    }

    #[test]
    fn identity_is_a_zero_radius_or_a_zero_strength() {
        assert!(custom_grade(
            GlowBasedOn::Color,
            153.0,
            0.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        )
        .is_identity());
        assert!(custom_grade(
            GlowBasedOn::Color,
            153.0,
            10.0,
            0.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        )
        .is_identity());
    }

    #[test]
    fn identity_grade_leaves_the_frame_bit_identical() {
        let grade = custom_grade(
            GlowBasedOn::Color,
            153.0,
            0.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        let before = frame(9, 5, |column, row| {
            [column as f32 * 0.05, row as f32 * 0.1, 0.3, 1.0]
        });
        let mut after = before.clone();
        assert!(glow_frame_f32(&mut after, 9, 5, &grade));
        assert_eq!(bits(&after), bits(&before));
    }

    #[test]
    fn the_colour_mask_thresholds_the_source_luminance() {
        // threshold 153 -> t = 0.6; a 0.8 grey is halfway between 0.6 and 1.0.
        let grade = default_grade();
        let extracted = grade.extract([0.8, 0.8, 0.8, 1.0]);
        assert!((extracted[0] - 0.8).abs() <= 1.0e-6);
        assert!((extracted[3] - 0.5).abs() <= 1.0e-5);
        // A black pixel is below the threshold and contributes no mask.
        assert_eq!(grade.extract([0.0, 0.0, 0.0, 1.0]), [0.0, 0.0, 0.0, 0.0]);
        // A threshold of 255 saturates the mask off for every pixel.
        let saturated = custom_grade(
            GlowBasedOn::Color,
            GLOW_AUTHOR_MAX_THRESHOLD,
            1.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        assert_eq!(
            saturated.extract([1.0, 1.0, 1.0, 1.0]),
            [1.0, 1.0, 1.0, 0.0]
        );
    }

    #[test]
    fn a_zero_threshold_exposes_the_whole_luminance() {
        let grade = custom_grade(
            GlowBasedOn::Color,
            0.0,
            1.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        let extracted = grade.extract([0.25, 0.25, 0.25, 1.0]);
        assert!((extracted[3] - 0.25).abs() <= 1.0e-6);
    }

    #[test]
    fn the_alpha_mask_uses_coverage_and_a_white_glow() {
        let grade = custom_grade(
            GlowBasedOn::Alpha,
            153.0,
            1.0,
            100.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        assert_eq!(grade.extract([0.3, 0.4, 0.5, 0.25]), [1.0, 1.0, 1.0, 0.25]);
        assert_eq!(grade.extract([0.3, 0.4, 0.5, 0.0]), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn colour_and_alpha_keeps_the_stronger_mask_and_the_source_colour() {
        let grade = custom_grade(
            GlowBasedOn::ColorAndAlpha,
            128.0,
            1.0,
            100.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        // Luminance 0.1 is below the 0.502 threshold, coverage is 1.0.
        let extracted = grade.extract([0.1, 0.1, 0.1, 1.0]);
        assert_eq!(extracted[0], 0.1);
        assert_eq!(extracted[3], 1.0);
    }

    #[test]
    fn the_a_and_b_ramp_follows_the_midpoint() {
        let midpoint = custom_grade(
            GlowBasedOn::Color,
            0.0,
            1.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::AAndB,
            1.0,
            0.0,
            0.5,
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            GlowDimensions::Both,
        );
        assert_eq!(midpoint.extract([0.0, 0.0, 0.0, 1.0]), [1.0, 0.0, 0.0, 0.0]);
        let half = midpoint.extract([0.5, 0.5, 0.5, 1.0]);
        assert!((half[0] - 0.5).abs() <= 1.0e-6);
        assert!((half[2] - 0.5).abs() <= 1.0e-6);
        let top = midpoint.extract([1.0, 1.0, 1.0, 1.0]);
        assert!((top[2] - 1.0).abs() <= 1.0e-6);
        // The ramp is continuous at the midpoint.
        assert!((midpoint.ab_ramp(0.5) - 0.5).abs() <= f32::EPSILON);
    }

    #[test]
    fn the_cycle_tint_wraps_the_phase() {
        let grade = custom_grade(
            GlowBasedOn::Color,
            0.0,
            1.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Cycle,
            1.0,
            0.0,
            0.5,
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            GlowDimensions::Both,
        );
        let quarter = grade.extract([0.25, 0.25, 0.25, 1.0]);
        assert!((quarter[0] - 0.75).abs() <= 1.0e-6);
        assert!((quarter[2] - 0.25).abs() <= 1.0e-6);
        // A half turn moves the ramp back to colour A.
        let wrapped = custom_grade(
            GlowBasedOn::Color,
            0.0,
            1.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Cycle,
            1.0,
            GLOW_AUTHOR_MAX_COLOR_PHASE_DEGREES / 2.0,
            0.5,
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            GlowDimensions::Both,
        );
        let turned = wrapped.extract([0.5, 0.5, 0.5, 1.0]);
        assert!((turned[0] - 1.0).abs() <= 1.0e-6);
        assert!(turned[2].abs() <= 1.0e-6);
    }

    #[test]
    fn the_composite_blends_on_top_regardless_of_alpha() {
        let grade = custom_grade(
            GlowBasedOn::Color,
            0.0,
            1.0,
            100.0,
            GlowComposite::OnTop,
            GlowOperation::Subtract,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        let composited = grade.composite([0.5, 0.5, 0.5, 0.25], [0.5, 0.5, 0.5, 0.6]);
        assert!((composited[0] - 0.2).abs() <= 1.0e-6);
        assert_eq!(composited[3], 0.25);
    }

    #[test]
    fn the_composite_behind_swaps_the_blend_operands() {
        let grade = custom_grade(
            GlowBasedOn::Color,
            0.0,
            1.0,
            100.0,
            GlowComposite::Behind,
            GlowOperation::Subtract,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        let composited = grade.composite([0.5, 0.5, 0.5, 1.0], [0.5, 0.5, 0.5, 0.6]);
        assert_eq!(composited[0], 0.0);
        assert_eq!(composited[3], 1.0);
    }

    #[test]
    fn the_composite_none_writes_the_contribution_alone() {
        let grade = custom_grade(
            GlowBasedOn::Color,
            0.0,
            1.0,
            50.0,
            GlowComposite::None,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        let composited = grade.composite([0.2, 0.3, 0.4, 1.0], [0.5, 0.5, 0.5, 1.0]);
        assert!((composited[0] - 0.25).abs() <= 1.0e-6);
        assert!((composited[1] - 0.25).abs() <= 1.0e-6);
        assert!((composited[2] - 0.25).abs() <= 1.0e-6);
        assert_eq!(composited[3], 1.0);
    }

    #[test]
    fn transparent_source_pixels_stay_bit_identical() {
        let grade = custom_grade(
            GlowBasedOn::ColorAndAlpha,
            0.0,
            2.0,
            255.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        let hidden = [7.5, -3.0, 0.25, 0.0];
        let composited = grade.composite(hidden, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(composited.map(f32::to_bits), hidden.map(f32::to_bits));
    }

    #[test]
    fn a_bright_pixel_spreads_into_its_neighbours() {
        // A nine-pixel row keeps the five-tap kernel away from the clamped
        // border, so the two nearest neighbours receive different fractions of
        // the mask.
        let width = 9;
        let mut pixels = frame(width, 1, |column, _| {
            if column == 4 {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                [0.0, 0.0, 0.0, 1.0]
            }
        });
        let grade = custom_grade(
            GlowBasedOn::Color,
            0.5,
            2.0,
            100.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
        );
        assert!(glow_frame_f32(&mut pixels, width, 1, &grade));
        // The source pixel keeps its full white, and the neighbours pick up a
        // fraction of the blurred mask through the Screen blend.
        assert!((pixels[4][0] - 1.0).abs() <= 1.0e-6);
        assert!(pixels[3][0] > 0.0);
        assert!(pixels[3][0] < 1.0);
        assert!(pixels[2][0] > 0.0);
        // The mask falls off with distance: the nearer neighbour is brighter.
        assert!(pixels[2][0] < pixels[3][0]);
        // The composite never invents coverage.
        assert_eq!(pixels[3][3], 1.0);
    }

    #[test]
    fn a_vertical_glow_does_not_blur_sideways() {
        let width = 3;
        let mut pixels = frame(width, 3, |column, row| {
            if column == 1 && row == 1 {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                [0.0, 0.0, 0.0, 1.0]
            }
        });
        let grade = custom_grade(
            GlowBasedOn::Color,
            0.5,
            1.0,
            100.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Vertical,
        );
        assert!(glow_frame_f32(&mut pixels, width, 3, &grade));
        // The same-row neighbour keeps zero coverage contribution ...
        assert_eq!(pixels[3][0], 0.0);
        // ... while the pixel above and below the source picks up the bloom.
        assert!(pixels[1][0] > 0.0);
        assert!(pixels[7][0] > 0.0);
    }

    #[test]
    fn the_frame_kernel_rejects_an_unaddressable_geometry() {
        let grade = default_grade();
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!glow_frame_f32(&mut empty, 0, 0, &grade));
        let mut short = [[0.0; 4]; 3];
        assert!(!glow_frame_f32(&mut short, 2, 2, &grade));
        let mut narrow = [[0.0; 4]; 4];
        assert!(!glow_frame_f32(&mut narrow, 0, 4, &grade));
    }

    #[test]
    fn out_of_range_thresholds_are_rejected() {
        for threshold in [-1.0, 255.5, f32::NAN, f32::INFINITY] {
            let error = GlowGrade::new(
                GlowBasedOn::Color,
                threshold,
                1.0,
                1.0,
                GlowComposite::OnTop,
                GlowOperation::Screen,
                GlowColors::Original,
                1.0,
                0.0,
                0.5,
                [1.0; 3],
                [0.0; 3],
                GlowDimensions::Both,
                WORKING,
            )
            .expect_err("an out-of-range threshold must be rejected");
            assert_eq!(error, GlowError::Threshold);
            assert_eq!(
                error.to_string(),
                "glow threshold must be a finite value inside [0, 255]"
            );
        }
    }

    #[test]
    fn out_of_range_radius_and_strength_are_rejected() {
        let radius = GlowGrade::new(
            GlowBasedOn::Color,
            153.0,
            GLOW_AUTHOR_MAX_RADIUS_PIXELS + 1.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
            WORKING,
        )
        .expect_err("an oversized radius must be rejected");
        assert_eq!(radius, GlowError::Radius);
        let strength = GlowGrade::new(
            GlowBasedOn::Color,
            153.0,
            1.0,
            GLOW_AUTHOR_MAX_STRENGTH + 0.5,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
            WORKING,
        )
        .expect_err("an oversized strength must be rejected");
        assert_eq!(strength, GlowError::Strength);
    }

    #[test]
    fn out_of_range_loops_and_midpoints_are_rejected() {
        for loops in [
            GLOW_AUTHOR_MIN_COLOR_LOOPS - 0.5,
            GLOW_AUTHOR_MAX_COLOR_LOOPS + 0.5,
        ] {
            let error = GlowGrade::new(
                GlowBasedOn::Color,
                153.0,
                1.0,
                1.0,
                GlowComposite::OnTop,
                GlowOperation::Screen,
                GlowColors::Cycle,
                loops,
                0.0,
                0.5,
                [1.0; 3],
                [0.0; 3],
                GlowDimensions::Both,
                WORKING,
            )
            .expect_err("an out-of-range loop count must be rejected");
            assert_eq!(error, GlowError::ColorLoops);
        }
        for midpoint in [
            GLOW_AUTHOR_MIN_MIDPOINT / 2.0,
            GLOW_AUTHOR_MAX_MIDPOINT + 0.001,
        ] {
            let error = GlowGrade::new(
                GlowBasedOn::Color,
                153.0,
                1.0,
                1.0,
                GlowComposite::OnTop,
                GlowOperation::Screen,
                GlowColors::AAndB,
                1.0,
                0.0,
                midpoint,
                [1.0; 3],
                [0.0; 3],
                GlowDimensions::Both,
                WORKING,
            )
            .expect_err("an out-of-range midpoint must be rejected");
            assert_eq!(error, GlowError::Midpoint);
        }
    }

    #[test]
    fn non_finite_colours_and_coefficients_are_rejected() {
        let colour = GlowGrade::new(
            GlowBasedOn::Color,
            153.0,
            1.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::AAndB,
            1.0,
            0.0,
            0.5,
            [f32::NAN, 1.0, 1.0],
            [0.0; 3],
            GlowDimensions::Both,
            WORKING,
        )
        .expect_err("a non-finite colour must be rejected");
        assert_eq!(colour, GlowError::ColorA);
        let coefficients = GlowGrade::new(
            GlowBasedOn::Color,
            153.0,
            1.0,
            1.0,
            GlowComposite::OnTop,
            GlowOperation::Screen,
            GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0; 3],
            [0.0; 3],
            GlowDimensions::Both,
            [0.0, 0.0, 0.0],
        )
        .expect_err("all-zero coefficients must be rejected");
        assert_eq!(coefficients, GlowError::Luminance);
    }

    #[test]
    fn the_operation_menu_is_one_based_and_fails_closed() {
        for (index, operation) in GlowOperation::ALL.into_iter().enumerate() {
            assert_eq!(operation.as_menu(), index as u32 + 1);
            assert_eq!(
                GlowOperation::from_menu(operation.as_menu()),
                Some(operation)
            );
        }
        assert_eq!(GlowOperation::from_menu(0), None);
        assert_eq!(GlowOperation::from_menu(26), None);
        assert_eq!(GlowOperation::Normal.blend_mode_index(), 0);
        assert_eq!(GlowOperation::Multiply.blend_mode_index(), 2);
        assert_eq!(GlowOperation::Screen.blend_mode_index(), 3);
        assert_eq!(GlowOperation::Subtract.blend_mode_index(), 13);
        assert_eq!(GlowOperation::Color.blend_mode_index(), 25);
        assert_eq!(GlowOperation::Color.as_menu(), 25);
    }

    #[test]
    fn the_composite_and_colour_menus_are_one_based() {
        for (index, composite) in GlowComposite::ALL.into_iter().enumerate() {
            assert_eq!(composite.as_menu(), index as u32 + 1);
            assert_eq!(
                GlowComposite::from_menu(composite.as_menu()),
                Some(composite)
            );
        }
        for (index, colours) in GlowColors::ALL.into_iter().enumerate() {
            assert_eq!(colours.as_menu(), index as u32 + 1);
            assert_eq!(GlowColors::from_menu(colours.as_menu()), Some(colours));
        }
        for (index, dimensions) in GlowDimensions::ALL.into_iter().enumerate() {
            assert_eq!(dimensions.as_menu(), index as u32 + 1);
            assert_eq!(
                GlowDimensions::from_menu(dimensions.as_menu()),
                Some(dimensions)
            );
        }
        assert_eq!(GlowComposite::from_menu(4), None);
        assert_eq!(GlowColors::from_menu(0), None);
        assert_eq!(GlowDimensions::from_menu(4), None);
        assert_eq!(
            GlowBasedOn::from_key("color_and_alpha"),
            Some(GlowBasedOn::ColorAndAlpha)
        );
        assert_eq!(GlowBasedOn::from_key("colour"), None);
        assert_eq!(GlowBasedOn::ColorAndAlpha.as_key(), "color_and_alpha");
    }

    #[test]
    fn the_dimension_axes_match_the_box_blur_directions() {
        assert_eq!(GlowDimensions::Horizontal.axis_count(), 1);
        assert_eq!(GlowDimensions::Vertical.axis_count(), 1);
        assert_eq!(GlowDimensions::Both.axis_count(), 2);
        assert!(GlowDimensions::Both.blurs_horizontal());
        assert!(GlowDimensions::Both.blurs_vertical());
        assert!(!GlowDimensions::Horizontal.blurs_vertical());
    }

    #[test]
    fn the_gpu_controls_pack_the_documented_extract_cells() {
        let (params, extra, aces, extra1) = default_grade().extract_gpu_controls();
        assert_eq!(
            params,
            [
                GLOW_DEFAULT_THRESHOLD / 255.0,
                WORKING[0],
                WORKING[1],
                WORKING[2]
            ]
        );
        assert_eq!(
            extra,
            [
                0.0,
                0.0,
                GLOW_DEFAULT_MIDPOINT,
                GLOW_DEFAULT_COLOR_PHASE / 360.0
            ]
        );
        assert_eq!(aces, [1.0, 1.0, 1.0, GLOW_DEFAULT_COLOR_LOOPS]);
        assert_eq!(extra1, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn the_gpu_controls_pack_the_documented_composite_cells() {
        let grade = custom_grade(
            GlowBasedOn::ColorAndAlpha,
            90.0,
            12.0,
            120.0,
            GlowComposite::Behind,
            GlowOperation::Color,
            GlowColors::AAndB,
            3.0,
            120.0,
            0.25,
            [0.5, 0.25, 0.0],
            [0.0, 0.25, 0.5],
            GlowDimensions::Horizontal,
        );
        let (params, extra, aces, extra1) = grade.extract_gpu_controls();
        assert_eq!(params, [90.0 / 255.0, WORKING[0], WORKING[1], WORKING[2]]);
        assert_eq!(extra, [2.0, 1.0, 0.25, 120.0 / 360.0]);
        assert_eq!(aces, [0.5, 0.25, 0.0, 3.0]);
        assert_eq!(extra1, [0.0, 0.25, 0.5, 0.0]);
        let (composite, rest, also_rest, last) = grade.composite_gpu_controls();
        assert_eq!(composite, [1.2, 25.0, 1.0, 0.0]);
        assert_eq!(rest, [0.0; 4]);
        assert_eq!(also_rest, [0.0; 4]);
        assert_eq!(last, [0.0; 4]);
    }
}
