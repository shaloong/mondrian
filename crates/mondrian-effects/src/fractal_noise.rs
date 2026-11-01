//! 分形噪波（Fractal Noise）
//!
//! A one-pass **generator** spatial effect: every output pixel evaluates a
//! fractal value-noise field built from the shared [`crate::noise::noise_hash`]
//! and composites the resulting grey value over the source pixel with the
//! authored [`BlendMode`] and opacity. The field never reads a neighbour, so the
//! CPU kernel needs no scratch frame and the GPU pass records a single draw from
//! the source texture.
//!
//! # Domain
//!
//! Everything runs in *noise units*, with [`FRACTAL_FEATURE_PIXELS`] pixels per
//! unit at a scale of 100 %:
//!
//! ```text
//! sx = scale / 100 * (uniform_scale ? 1 : scale_width / 100)
//! sy = scale / 100 * (uniform_scale ? 1 : scale_height / 100)
//! p  = ((x - cx) / (FEATURE_PIXELS * sx), (y - cy) / (FEATURE_PIXELS * sy))
//! p += centre_auxiliary / 10 * (0.5, 0.5)
//! p  = rotate(p, rotation)
//! p += offset_turbulence / FEATURE_PIXELS
//! if perspective_offset { p.x += p.y * 0.5 }
//! ```
//!
//! The octave loop then accumulates `complexity` layers: each layer rotates
//! `p * frequency + offset` by the running angle, samples the shared hash with
//! the per-octave salt `seed_hash ^ (i * 0x9E37_79B9)`, and folds the sample into
//! the field according to the [`FractalType`]. Afterwards the field is divided by
//! the accumulated amplitude (every type except [`FractalType::Multiply`]), run
//! through the [`FractalOverflow`] mode, inverted when asked, and contrast and
//! brightness are applied around the 0.5 midpoint. The grey value finally
//! replaces the source colour through [`crate::adjustment::blend_mode_rgb`] and
//! the authored opacity; the source alpha is preserved and a fully transparent
//! source pixel is returned untouched.
//!
//! # Uniform packing
//!
//! The pass needs 26 floats, more than the shared `SpatialPassUniform` carries,
//! so it owns a dedicated [`FractalNoiseUniform`] of eight `vec4` cells:
//!
//! - `size   = [w, h, 0, 0]`
//! - `params = [sx, sy, rotation_cos, rotation_sin]`
//! - `extra0 = [offset.x, offset.y, centre_auxiliary, perspective]`, the offset
//!   already divided by [`FRACTAL_FEATURE_PIXELS`]
//! - `extra1 = [evolution_turns, complexity, sub_influence / 100, lacunarity]`
//!   with `lacunarity = 1 + sub_scale / 50`
//! - `extra2 = [sub_rotation_cos, sub_rotation_sin, sub_offset.x, sub_offset.y]`
//! - `extra3 = [contrast / 100, brightness / 100, fractal_type, noise_type]`
//! - `extra4 = [overflow, flags, random_seed, opacity / 100]` with flags bit 0
//!   for the inversion and bit 1 for `cycle_evolution`
//! - `extra5 = [blend_mode_index, 0, 0, 0]`
//!
//! [`FractalNoiseGrade::gpu_controls`] derives every cell from the same private
//! accessors the CPU kernel uses, so the two paths cannot drift apart.
//!
//! # Decisions left open by the design brief
//!
//! - [`FractalType::Multiply`] seeds the field with `1.0`. The brief's literal
//!   `field = 0` start would leave every multiply frame black, because `0` is
//!   absorbing under `field *= 0.5 + 0.5 * n`.
//! - [`FractalType::TurbulentBasic`] sharpens its octave weights by squaring the
//!   per-octave decay: `amp *= (sub_influence / 100)²` instead of
//!   `amp *= sub_influence / 100`. The brief only asked for "sharper octave
//!   weights".
//! - Rotations are applied with the `cos`/`sin` pairs the uniform already packs,
//!   so the accumulated angle is advanced by complex multiplication rather than
//!   by `cos(angle)` per octave. The Swirly extra step uses the literal pair
//!   `(0.866_025_4, 0.5)`, which keeps both paths on exactly the same arithmetic.
//! - `cycle_evolution` wraps the authored evolution in
//!   [`FractalNoiseGrade::evolution_turns`], which the kernel and the uniform
//!   packing share, instead of in the graph builder. The builder only folds the
//!   timeline into `evolution`; the wrap is idempotent, so a builder that did
//!   wrap would not change the result.
//! - `sub_offset` is validated against ±[`FRACTAL_MAX_OFFSET_PIXELS`] like
//!   `offset_turbulence`, but its unit is a noise unit: it accumulates into the
//!   octave offset, which lives in noise space.
//! - The spline sampler uses uniform Catmull-Rom (tension ½) weights over the
//!   four neighbouring cells and may overshoot `[-1, 1]`; the overflow mode is
//!   the only place that bounds the field.
//! - The kernel evaluates the field serially, polling the checkpoint every
//!   [`CHECKPOINT_STRIDE`] pixels, and composites in parallel row bands. That is
//!   the [`crate::glow`] shape: the checkpoint callback is `FnMut` and the row
//!   band helper requires `Fn`, so the polling pass has to stay serial.
//! - [`crate::glow::blend_mode_index`] is shared with this module (it is
//!   `pub(crate)` now) instead of copying the 27-arm compositor tag table a
//!   third time.

use std::hash::{Hash, Hasher};

use mondrian_core::BlendMode;

use crate::adjustment::blend_mode_rgb;
use crate::glow::blend_mode_index;
use crate::noise::noise_sample;

/// Pixels covered by one noise unit at a scale of 100 %.
pub const FRACTAL_FEATURE_PIXELS: f32 = 100.0;

/// Smallest authored contrast, as a percentage of the source contrast.
pub const FRACTAL_MIN_CONTRAST: f32 = 0.0;
/// Largest authored contrast, as a percentage of the source contrast.
pub const FRACTAL_MAX_CONTRAST: f32 = 400.0;
/// Smallest authored brightness, as a percentage of the full range.
pub const FRACTAL_MIN_BRIGHTNESS: f32 = -100.0;
/// Largest authored brightness, as a percentage of the full range.
pub const FRACTAL_MAX_BRIGHTNESS: f32 = 100.0;
/// Largest authored rotation magnitude, in degrees.
pub const FRACTAL_MAX_ROTATION_DEGREES: f32 = 360_000.0;
/// Smallest authored scale, as a percentage.
pub const FRACTAL_MIN_SCALE: f32 = 1.0;
/// Largest authored scale, as a percentage.
pub const FRACTAL_MAX_SCALE: f32 = 1_000.0;
/// Largest authored centre offset, in pixels.
pub const FRACTAL_MAX_OFFSET_PIXELS: f32 = 10_000.0;
/// Smallest authored octave count.
pub const FRACTAL_MIN_COMPLEXITY: u32 = 1;
/// Largest authored octave count.
pub const FRACTAL_MAX_COMPLEXITY: u32 = 15;
/// Smallest authored sub influence, as a percentage.
pub const FRACTAL_MIN_SUB_INFLUENCE: f32 = 0.0;
/// Largest authored sub influence, as a percentage.
pub const FRACTAL_MAX_SUB_INFLUENCE: f32 = 100.0;
/// Smallest authored sub scale, as a percentage.
pub const FRACTAL_MIN_SUB_SCALE: f32 = 0.0;
/// Largest authored sub scale, as a percentage.
pub const FRACTAL_MAX_SUB_SCALE: f32 = 1_000.0;
/// Smallest authored centre auxiliary ratio.
pub const FRACTAL_MIN_CENTRE_AUXILIARY: f32 = -5.0;
/// Largest authored centre auxiliary ratio.
pub const FRACTAL_MAX_CENTRE_AUXILIARY: f32 = 5.0;
/// Smallest authored cycle length, in turns.
pub const FRACTAL_MIN_CYCLE: u32 = 1;
/// Largest authored cycle length, in turns.
pub const FRACTAL_MAX_CYCLE: u32 = 88;
/// Largest authored random seed.
pub const FRACTAL_MAX_RANDOM_SEED: u32 = 100_000;
/// Smallest authored opacity, as a percentage.
pub const FRACTAL_MIN_OPACITY: f32 = 0.0;
/// Largest authored opacity, as a percentage.
pub const FRACTAL_MAX_OPACITY: f32 = 100.0;

/// Number of fractal shapes.
pub const FRACTAL_TYPE_COUNT: usize = 9;
/// Number of noise interpolations.
pub const FRACTAL_NOISE_TYPE_COUNT: usize = 3;
/// Number of overflow modes.
pub const FRACTAL_OVERFLOW_COUNT: usize = 3;

/// Base of the per-effect seed hash, shared with
/// [`crate::turbulent_displace`]: `seed_hash = FRACTAL_SEED_BASE ^ random_seed`.
const FRACTAL_SEED_BASE: u32 = 0x85EB_CA6B;
/// Salt step between octaves, the same constant the shared hash uses.
const FRACTAL_OCTAVE_SALT_STEP: u32 = 0x9E37_79B9;
/// Degrees-to-radians factor, written as one literal so the shader can repeat it
/// bit for bit.
const DEGREES_TO_RADIANS: f32 = 0.017_453_292;
/// Cosine of the extra 30° the Swirly shape advances per octave.
const SWIRLY_STEP_COS: f32 = 0.866_025_4;
/// Sine of the extra 30° the Swirly shape advances per octave.
const SWIRLY_STEP_SIN: f32 = 0.5;
/// Pixels between two checkpoint polls in the serial field pass.
const CHECKPOINT_STRIDE: usize = 4096;

/// The nine fractal shapes the field can accumulate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FractalType {
    /// Unipolar signed sum: the plainest fractal cloud.
    Basic,
    /// Sum of folded (absolute) octaves, the classic smooth turbulence.
    TurbulentSmooth,
    /// Largest folded octave instead of their sum.
    TurbulentSharp,
    /// Folded octaves with a sharper amplitude decay.
    TurbulentBasic,
    /// Largest signed octave.
    Max,
    /// Rotating sum: the angle advances by 30° per octave.
    Swirly,
    /// Ter-raced sum, with quantised plateaus.
    Rock,
    /// Folded sum inverted around the octave midpoint.
    Threads,
    /// Multiplicative accumulation seeded with 1.
    Multiply,
}

impl FractalType {
    /// Every shape, in menu order.
    pub const ALL: [FractalType; FRACTAL_TYPE_COUNT] = [
        Self::Basic,
        Self::TurbulentSmooth,
        Self::TurbulentSharp,
        Self::TurbulentBasic,
        Self::Max,
        Self::Swirly,
        Self::Rock,
        Self::Threads,
        Self::Multiply,
    ];

    /// The stable authoring key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::TurbulentSmooth => "turbulent_smooth",
            Self::TurbulentSharp => "turbulent_sharp",
            Self::TurbulentBasic => "turbulent_basic",
            Self::Max => "max",
            Self::Swirly => "swirly",
            Self::Rock => "rock",
            Self::Threads => "threads",
            Self::Multiply => "multiply",
        }
    }

    /// Resolves an authoring key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "basic" => Some(Self::Basic),
            "turbulent_smooth" => Some(Self::TurbulentSmooth),
            "turbulent_sharp" => Some(Self::TurbulentSharp),
            "turbulent_basic" => Some(Self::TurbulentBasic),
            "max" => Some(Self::Max),
            "swirly" => Some(Self::Swirly),
            "rock" => Some(Self::Rock),
            "threads" => Some(Self::Threads),
            "multiply" => Some(Self::Multiply),
            _ => None,
        }
    }

    /// The zero-based index packed into the uniform.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Basic => 0,
            Self::TurbulentSmooth => 1,
            Self::TurbulentSharp => 2,
            Self::TurbulentBasic => 3,
            Self::Max => 4,
            Self::Swirly => 5,
            Self::Rock => 6,
            Self::Threads => 7,
            Self::Multiply => 8,
        }
    }

    /// The Chinese menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Basic => "基本",
            Self::TurbulentSmooth => "湍流较平滑",
            Self::TurbulentSharp => "湍流锐利",
            Self::TurbulentBasic => "湍流基本",
            Self::Max => "最大值",
            Self::Swirly => "旋转",
            Self::Rock => "岩石",
            Self::Threads => "线条",
            Self::Multiply => "相乘",
        }
    }
}

/// The interpolation used when the hash is sampled between cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FractalNoiseType {
    /// Bilinear with a smoothstep weight: the softest of the three.
    SoftLinear,
    /// Plain bilinear.
    Linear,
    /// Four-tap Catmull-Rom, which can overshoot the cell samples.
    Spline,
}

impl FractalNoiseType {
    /// Every interpolation, in menu order.
    pub const ALL: [FractalNoiseType; FRACTAL_NOISE_TYPE_COUNT] =
        [Self::SoftLinear, Self::Linear, Self::Spline];

    /// The stable authoring key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::SoftLinear => "soft_linear",
            Self::Linear => "linear",
            Self::Spline => "spline",
        }
    }

    /// Resolves an authoring key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "soft_linear" => Some(Self::SoftLinear),
            "linear" => Some(Self::Linear),
            "spline" => Some(Self::Spline),
            _ => None,
        }
    }

    /// The zero-based index packed into the uniform.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::SoftLinear => 0,
            Self::Linear => 1,
            Self::Spline => 2,
        }
    }

    /// The Chinese menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SoftLinear => "较平滑",
            Self::Linear => "线性",
            Self::Spline => "样条",
        }
    }
}

/// How a field value outside `[0, 1]` is brought back into range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FractalOverflow {
    /// Clamp at both ends.
    Clip,
    /// Bend both ends with a scaled exponential saturation.
    SoftClamp,
    /// Fold the fractional part.
    Wrap,
}

impl FractalOverflow {
    /// Every overflow mode, in menu order.
    pub const ALL: [FractalOverflow; FRACTAL_OVERFLOW_COUNT] =
        [Self::Clip, Self::SoftClamp, Self::Wrap];

    /// The stable authoring key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::SoftClamp => "soft_clamp",
            Self::Wrap => "wrap",
        }
    }

    /// Resolves an authoring key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "clip" => Some(Self::Clip),
            "soft_clamp" => Some(Self::SoftClamp),
            "wrap" => Some(Self::Wrap),
            _ => None,
        }
    }

    /// The zero-based index packed into the uniform.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Clip => 0,
            Self::SoftClamp => 1,
            Self::Wrap => 2,
        }
    }

    /// The Chinese menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clip => "裁剪",
            Self::SoftClamp => "柔和限幅",
            Self::Wrap => "环绕",
        }
    }
}

/// The stable authoring keys of the twenty-seven blend modes, in
/// [`BlendMode`] variant order.
///
/// The property schema derives the menu keys from this table and the graph
/// builder maps them back, so a stored `blend_mode` value never depends on the
/// capitalised display values [`mondrian_core::display_labels::blend_mode_options`]
/// carries for the clip schema.
pub const BLEND_MODE_KEYS: [(&str, BlendMode); 27] = [
    ("normal", BlendMode::Normal),
    ("dissolve", BlendMode::Dissolve),
    ("multiply", BlendMode::Multiply),
    ("screen", BlendMode::Screen),
    ("overlay", BlendMode::Overlay),
    ("darken", BlendMode::Darken),
    ("lighten", BlendMode::Lighten),
    ("color_dodge", BlendMode::ColorDodge),
    ("color_burn", BlendMode::ColorBurn),
    ("hard_light", BlendMode::HardLight),
    ("soft_light", BlendMode::SoftLight),
    ("difference", BlendMode::Difference),
    ("exclusion", BlendMode::Exclusion),
    ("subtract", BlendMode::Subtract),
    ("darker_color", BlendMode::DarkerColor),
    ("lighter_color", BlendMode::LighterColor),
    ("linear_burn", BlendMode::LinearBurn),
    ("linear_dodge", BlendMode::LinearDodge),
    ("vivid_light", BlendMode::VividLight),
    ("linear_light", BlendMode::LinearLight),
    ("pin_light", BlendMode::PinLight),
    ("hard_mix", BlendMode::HardMix),
    ("divide", BlendMode::Divide),
    ("hue", BlendMode::Hue),
    ("saturation", BlendMode::Saturation),
    ("color", BlendMode::Color),
    ("luminosity", BlendMode::Luminosity),
];

/// The stable authoring key of `mode`.
#[must_use]
pub fn blend_mode_key(mode: BlendMode) -> &'static str {
    BLEND_MODE_KEYS
        .iter()
        .find(|(_, candidate)| *candidate == mode)
        .map_or("normal", |(key, _)| *key)
}

/// Resolves the typed blend mode stored under `key`.
#[must_use]
pub fn blend_mode_from_key(key: &str) -> Option<BlendMode> {
    BLEND_MODE_KEYS
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, mode)| *mode)
}

/// Why a [`FractalNoiseGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FractalNoiseError {
    /// Contrast outside `[0, 400]`.
    Contrast,
    /// Brightness outside `[-100, 100]`.
    Brightness,
    /// Rotation outside `[-360000, 360000]`.
    Rotation,
    /// Scale outside `[1, 1000]`.
    Scale,
    /// Scale width outside `[1, 1000]`.
    ScaleWidth,
    /// Scale height outside `[1, 1000]`.
    ScaleHeight,
    /// Turbulence offset outside `[-10000, 10000]`.
    OffsetTurbulence,
    /// Complexity outside `[1, 15]`.
    Complexity,
    /// Sub influence outside `[0, 100]`.
    SubInfluence,
    /// Sub scale outside `[0, 1000]`.
    SubScale,
    /// Sub rotation outside `[-360000, 360000]`.
    SubRotation,
    /// Sub offset outside `[-10000, 10000]`.
    SubOffset,
    /// Centre auxiliary ratio outside `[-5, 5]`.
    CentreAuxiliary,
    /// Evolution outside `[-360000, 360000]`.
    Evolution,
    /// Cycle outside `[1, 88]`.
    Cycle,
    /// Random seed above `100000`.
    RandomSeed,
    /// Opacity outside `[0, 100]`.
    Opacity,
}

impl std::fmt::Display for FractalNoiseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contrast => write!(
                formatter,
                "fractal noise contrast must be a finite value inside [{FRACTAL_MIN_CONTRAST}, {FRACTAL_MAX_CONTRAST}]"
            ),
            Self::Brightness => write!(
                formatter,
                "fractal noise brightness must be a finite value inside [{FRACTAL_MIN_BRIGHTNESS}, {FRACTAL_MAX_BRIGHTNESS}]"
            ),
            Self::Rotation => write!(
                formatter,
                "fractal noise rotation must be a finite value inside [-{FRACTAL_MAX_ROTATION_DEGREES}, {FRACTAL_MAX_ROTATION_DEGREES}]"
            ),
            Self::Scale => write!(
                formatter,
                "fractal noise scale must be a finite value inside [{FRACTAL_MIN_SCALE}, {FRACTAL_MAX_SCALE}]"
            ),
            Self::ScaleWidth => write!(
                formatter,
                "fractal noise scale width must be a finite value inside [{FRACTAL_MIN_SCALE}, {FRACTAL_MAX_SCALE}]"
            ),
            Self::ScaleHeight => write!(
                formatter,
                "fractal noise scale height must be a finite value inside [{FRACTAL_MIN_SCALE}, {FRACTAL_MAX_SCALE}]"
            ),
            Self::OffsetTurbulence => write!(
                formatter,
                "fractal noise offset turbulence must contain finite values inside [-{FRACTAL_MAX_OFFSET_PIXELS}, {FRACTAL_MAX_OFFSET_PIXELS}]"
            ),
            Self::Complexity => write!(
                formatter,
                "fractal noise complexity must be inside [{FRACTAL_MIN_COMPLEXITY}, {FRACTAL_MAX_COMPLEXITY}]"
            ),
            Self::SubInfluence => write!(
                formatter,
                "fractal noise sub influence must be a finite value inside [{FRACTAL_MIN_SUB_INFLUENCE}, {FRACTAL_MAX_SUB_INFLUENCE}]"
            ),
            Self::SubScale => write!(
                formatter,
                "fractal noise sub scale must be a finite value inside [{FRACTAL_MIN_SUB_SCALE}, {FRACTAL_MAX_SUB_SCALE}]"
            ),
            Self::SubRotation => write!(
                formatter,
                "fractal noise sub rotation must be a finite value inside [-{FRACTAL_MAX_ROTATION_DEGREES}, {FRACTAL_MAX_ROTATION_DEGREES}]"
            ),
            Self::SubOffset => write!(
                formatter,
                "fractal noise sub offset must contain finite values inside [-{FRACTAL_MAX_OFFSET_PIXELS}, {FRACTAL_MAX_OFFSET_PIXELS}]"
            ),
            Self::CentreAuxiliary => write!(
                formatter,
                "fractal noise centre auxiliary must be a finite value inside [{FRACTAL_MIN_CENTRE_AUXILIARY}, {FRACTAL_MAX_CENTRE_AUXILIARY}]"
            ),
            Self::Evolution => write!(
                formatter,
                "fractal noise evolution must be a finite value inside [-{FRACTAL_MAX_ROTATION_DEGREES}, {FRACTAL_MAX_ROTATION_DEGREES}]"
            ),
            Self::Cycle => write!(
                formatter,
                "fractal noise cycle must be inside [{FRACTAL_MIN_CYCLE}, {FRACTAL_MAX_CYCLE}]"
            ),
            Self::RandomSeed => write!(
                formatter,
                "fractal noise random seed must be inside [0, {FRACTAL_MAX_RANDOM_SEED}]"
            ),
            Self::Opacity => write!(
                formatter,
                "fractal noise opacity must be a finite value inside [{FRACTAL_MIN_OPACITY}, {FRACTAL_MAX_OPACITY}]"
            ),
        }
    }
}

impl std::error::Error for FractalNoiseError {}

/// The eight `vec4` cells the dedicated fractal-noise pass uniform carries.
///
/// Kept as a plain copyable struct in this crate so the renderer can translate
/// it into its own `#[repr(C)]` uniform without depending on the layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FractalNoiseUniform {
    /// `[w, h, 0, 0]`.
    pub size: [f32; 4],
    /// `[sx, sy, rotation_cos, rotation_sin]`.
    pub params: [f32; 4],
    /// `[offset.x, offset.y, centre_auxiliary, perspective]`.
    pub extra0: [f32; 4],
    /// `[evolution_turns, complexity, sub_influence / 100, lacunarity]`.
    pub extra1: [f32; 4],
    /// `[sub_rotation_cos, sub_rotation_sin, sub_offset.x, sub_offset.y]`.
    pub extra2: [f32; 4],
    /// `[contrast / 100, brightness / 100, fractal_type, noise_type]`.
    pub extra3: [f32; 4],
    /// `[overflow, flags, random_seed, opacity / 100]`.
    pub extra4: [f32; 4],
    /// `[blend_mode_index, 0, 0, 0]`.
    pub extra5: [f32; 4],
}

/// A validated fractal-noise configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FractalNoiseGrade {
    fractal_type: FractalType,
    noise_type: FractalNoiseType,
    invert: bool,
    contrast: f32,
    brightness: f32,
    overflow: FractalOverflow,
    rotation: f32,
    uniform_scale: bool,
    scale: f32,
    scale_width: f32,
    scale_height: f32,
    offset_turbulence: [f32; 2],
    perspective_offset: bool,
    complexity: u32,
    sub_influence: f32,
    sub_scale: f32,
    sub_rotation: f32,
    sub_offset: [f32; 2],
    centre_auxiliary: f32,
    evolution: f32,
    cycle_evolution: bool,
    cycle: u32,
    random_seed: u32,
    opacity: f32,
    blend_mode: BlendMode,
}

impl FractalNoiseGrade {
    /// Builds a fractal-noise configuration.
    ///
    /// The graph builder converts the authored percentages into the values this
    /// type stores; every authored range is validated here so the kernel and the
    /// shader can use the stored values unclamped.
    ///
    /// # Errors
    ///
    /// Returns the matching [`FractalNoiseError`] for the first field outside its
    /// authored range.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fractal_type: FractalType,
        noise_type: FractalNoiseType,
        invert: bool,
        contrast: f32,
        brightness: f32,
        overflow: FractalOverflow,
        rotation: f32,
        uniform_scale: bool,
        scale: f32,
        scale_width: f32,
        scale_height: f32,
        offset_turbulence: [f32; 2],
        perspective_offset: bool,
        complexity: u32,
        sub_influence: f32,
        sub_scale: f32,
        sub_rotation: f32,
        sub_offset: [f32; 2],
        centre_auxiliary: f32,
        evolution: f32,
        cycle_evolution: bool,
        cycle: u32,
        random_seed: u32,
        opacity: f32,
        blend_mode: BlendMode,
    ) -> Result<Self, FractalNoiseError> {
        let grade = Self {
            fractal_type,
            noise_type,
            invert,
            contrast,
            brightness,
            overflow,
            rotation,
            uniform_scale,
            scale,
            scale_width,
            scale_height,
            offset_turbulence,
            perspective_offset,
            complexity,
            sub_influence,
            sub_scale,
            sub_rotation,
            sub_offset,
            centre_auxiliary,
            evolution,
            cycle_evolution,
            cycle,
            random_seed,
            opacity,
            blend_mode,
        };
        grade.validate()?;
        Ok(grade)
    }

    /// The fractal shape.
    #[must_use]
    pub const fn fractal_type(&self) -> FractalType {
        self.fractal_type
    }

    /// The hash interpolation.
    #[must_use]
    pub const fn noise_type(&self) -> FractalNoiseType {
        self.noise_type
    }

    /// Whether the field is inverted before grading.
    #[must_use]
    pub const fn invert(&self) -> bool {
        self.invert
    }

    /// The contrast percentage.
    #[must_use]
    pub const fn contrast(&self) -> f32 {
        self.contrast
    }

    /// The brightness percentage.
    #[must_use]
    pub const fn brightness(&self) -> f32 {
        self.brightness
    }

    /// The overflow mode.
    #[must_use]
    pub const fn overflow(&self) -> FractalOverflow {
        self.overflow
    }

    /// The authored rotation, in degrees.
    #[must_use]
    pub const fn rotation(&self) -> f32 {
        self.rotation
    }

    /// Whether both scale axes use [`Self::scale`].
    #[must_use]
    pub const fn uniform_scale(&self) -> bool {
        self.uniform_scale
    }

    /// The uniform scale percentage.
    #[must_use]
    pub const fn scale(&self) -> f32 {
        self.scale
    }

    /// The horizontal scale percentage.
    #[must_use]
    pub const fn scale_width(&self) -> f32 {
        self.scale_width
    }

    /// The vertical scale percentage.
    #[must_use]
    pub const fn scale_height(&self) -> f32 {
        self.scale_height
    }

    /// The authored turbulence offset, in pixels.
    #[must_use]
    pub const fn offset_turbulence(&self) -> [f32; 2] {
        self.offset_turbulence
    }

    /// Whether the perspective shear is applied.
    #[must_use]
    pub const fn perspective_offset(&self) -> bool {
        self.perspective_offset
    }

    /// The octave count.
    #[must_use]
    pub const fn complexity(&self) -> u32 {
        self.complexity
    }

    /// The sub influence percentage.
    #[must_use]
    pub const fn sub_influence(&self) -> f32 {
        self.sub_influence
    }

    /// The sub scale percentage.
    #[must_use]
    pub const fn sub_scale(&self) -> f32 {
        self.sub_scale
    }

    /// The authored sub rotation, in degrees.
    #[must_use]
    pub const fn sub_rotation(&self) -> f32 {
        self.sub_rotation
    }

    /// The authored sub offset, in noise units.
    #[must_use]
    pub const fn sub_offset(&self) -> [f32; 2] {
        self.sub_offset
    }

    /// The centre auxiliary ratio.
    #[must_use]
    pub const fn centre_auxiliary(&self) -> f32 {
        self.centre_auxiliary
    }

    /// The authored evolution, in degrees.
    #[must_use]
    pub const fn evolution(&self) -> f32 {
        self.evolution
    }

    /// Whether the evolution loops instead of drifting.
    #[must_use]
    pub const fn cycle_evolution(&self) -> bool {
        self.cycle_evolution
    }

    /// The cycle length, in turns.
    #[must_use]
    pub const fn cycle(&self) -> u32 {
        self.cycle
    }

    /// The authored random seed.
    #[must_use]
    pub const fn random_seed(&self) -> u32 {
        self.random_seed
    }

    /// The opacity percentage.
    #[must_use]
    pub const fn opacity(&self) -> f32 {
        self.opacity
    }

    /// The compositing blend mode.
    #[must_use]
    pub const fn blend_mode(&self) -> BlendMode {
        self.blend_mode
    }

    /// Whether the grade leaves the frame untouched.
    ///
    /// Only a zero opacity is the identity: every other parameter still changes
    /// the field, and a zero contrast is a real (flat) grade.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.opacity == 0.0
    }

    /// The horizontal noise-space scale.
    fn scale_x(&self) -> f32 {
        self.scale / 100.0
            * if self.uniform_scale {
                1.0
            } else {
                self.scale_width / 100.0
            }
    }

    /// The vertical noise-space scale.
    fn scale_y(&self) -> f32 {
        self.scale / 100.0
            * if self.uniform_scale {
                1.0
            } else {
                self.scale_height / 100.0
            }
    }

    /// The turbulence offset in noise units.
    fn offset_noise_units(&self) -> [f32; 2] {
        [
            self.offset_turbulence[0] / FRACTAL_FEATURE_PIXELS,
            self.offset_turbulence[1] / FRACTAL_FEATURE_PIXELS,
        ]
    }

    /// The diagonal origin shift in noise units.
    fn centre_shift(&self) -> f32 {
        self.centre_auxiliary / 10.0 * 0.5
    }

    /// The main rotation as a `(cos, sin)` pair.
    fn rotation_cos_sin(&self) -> (f32, f32) {
        let radians = self.rotation * DEGREES_TO_RADIANS;
        (radians.cos(), radians.sin())
    }

    /// The sub rotation as a `(cos, sin)` pair.
    fn sub_rotation_cos_sin(&self) -> (f32, f32) {
        let radians = self.sub_rotation * DEGREES_TO_RADIANS;
        (radians.cos(), radians.sin())
    }

    /// The resolved evolution, in turns, wrapped into `[0, cycle)` when the
    /// authored evolution loops.
    fn evolution_turns(&self) -> f32 {
        let turns = self.evolution / 360.0;
        if self.cycle_evolution {
            wrap_turns(turns, self.cycle as f32)
        } else {
            turns
        }
    }

    /// The evolution shift as a `(cos, sin)` pair, scaled by the caller.
    fn evolution_cos_sin(&self) -> (f32, f32) {
        let radians = self.evolution_turns() * 360.0 * DEGREES_TO_RADIANS;
        (radians.cos(), radians.sin())
    }

    /// The per-octave amplitude multiplier.
    fn sub_influence_fraction(&self) -> f32 {
        self.sub_influence / 100.0
    }

    /// The per-octave frequency multiplier.
    fn lacunarity(&self) -> f32 {
        1.0 + self.sub_scale / 50.0
    }

    /// The contrast gain applied around the midpoint.
    fn contrast_gain(&self) -> f32 {
        self.contrast / 100.0
    }

    /// The brightness offset applied after the contrast.
    fn brightness_offset(&self) -> f32 {
        self.brightness / 100.0
    }

    /// The opacity as a fraction.
    fn opacity_fraction(&self) -> f32 {
        self.opacity / 100.0
    }

    /// The packed uniform flags: bit 0 inverts, bit 1 loops the evolution.
    fn flags(&self) -> u32 {
        u32::from(self.invert) | (u32::from(self.cycle_evolution) << 1)
    }

    /// Packs the eight cells of [`FractalNoiseUniform`].
    ///
    /// `width` and `height` only feed the frame size; the centre is derived from
    /// them at both call sites, exactly like the CPU kernel does.
    #[must_use]
    pub fn gpu_controls(&self, width: u32, height: u32) -> FractalNoiseUniform {
        let offset = self.offset_noise_units();
        let (rotation_cos, rotation_sin) = self.rotation_cos_sin();
        let (sub_rotation_cos, sub_rotation_sin) = self.sub_rotation_cos_sin();
        FractalNoiseUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params: [self.scale_x(), self.scale_y(), rotation_cos, rotation_sin],
            extra0: [
                offset[0],
                offset[1],
                self.centre_auxiliary,
                if self.perspective_offset { 1.0 } else { 0.0 },
            ],
            extra1: [
                self.evolution_turns(),
                self.complexity as f32,
                self.sub_influence_fraction(),
                self.lacunarity(),
            ],
            extra2: [
                sub_rotation_cos,
                sub_rotation_sin,
                self.sub_offset[0],
                self.sub_offset[1],
            ],
            extra3: [
                self.contrast_gain(),
                self.brightness_offset(),
                self.fractal_type.as_u32() as f32,
                self.noise_type.as_u32() as f32,
            ],
            extra4: [
                self.overflow.as_u32() as f32,
                self.flags() as f32,
                self.random_seed as f32,
                self.opacity_fraction(),
            ],
            extra5: [blend_mode_index(self.blend_mode) as f32, 0.0, 0.0, 0.0],
        }
    }

    /// Hashes every field, used by the render-operation content hash.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        self.fractal_type.as_u32().hash(state);
        self.noise_type.as_u32().hash(state);
        self.overflow.as_u32().hash(state);
        self.invert.hash(state);
        self.contrast.to_bits().hash(state);
        self.brightness.to_bits().hash(state);
        self.rotation.to_bits().hash(state);
        self.uniform_scale.hash(state);
        self.scale.to_bits().hash(state);
        self.scale_width.to_bits().hash(state);
        self.scale_height.to_bits().hash(state);
        self.offset_turbulence.map(f32::to_bits).hash(state);
        self.perspective_offset.hash(state);
        self.complexity.hash(state);
        self.sub_influence.to_bits().hash(state);
        self.sub_scale.to_bits().hash(state);
        self.sub_rotation.to_bits().hash(state);
        self.sub_offset.map(f32::to_bits).hash(state);
        self.centre_auxiliary.to_bits().hash(state);
        self.evolution.to_bits().hash(state);
        self.cycle_evolution.hash(state);
        self.cycle.hash(state);
        self.random_seed.hash(state);
        self.opacity.to_bits().hash(state);
        self.blend_mode.hash(state);
    }

    /// Validates every authored range.
    fn validate(&self) -> Result<(), FractalNoiseError> {
        if !self.contrast.is_finite()
            || !(FRACTAL_MIN_CONTRAST..=FRACTAL_MAX_CONTRAST).contains(&self.contrast)
        {
            return Err(FractalNoiseError::Contrast);
        }
        if !self.brightness.is_finite()
            || !(FRACTAL_MIN_BRIGHTNESS..=FRACTAL_MAX_BRIGHTNESS).contains(&self.brightness)
        {
            return Err(FractalNoiseError::Brightness);
        }
        if !self.rotation.is_finite() || self.rotation.abs() > FRACTAL_MAX_ROTATION_DEGREES {
            return Err(FractalNoiseError::Rotation);
        }
        if !self.scale.is_finite() || !(FRACTAL_MIN_SCALE..=FRACTAL_MAX_SCALE).contains(&self.scale)
        {
            return Err(FractalNoiseError::Scale);
        }
        if !self.scale_width.is_finite()
            || !(FRACTAL_MIN_SCALE..=FRACTAL_MAX_SCALE).contains(&self.scale_width)
        {
            return Err(FractalNoiseError::ScaleWidth);
        }
        if !self.scale_height.is_finite()
            || !(FRACTAL_MIN_SCALE..=FRACTAL_MAX_SCALE).contains(&self.scale_height)
        {
            return Err(FractalNoiseError::ScaleHeight);
        }
        if !self.offset_turbulence.iter().all(|value| value.is_finite())
            || self
                .offset_turbulence
                .iter()
                .any(|value| value.abs() > FRACTAL_MAX_OFFSET_PIXELS)
        {
            return Err(FractalNoiseError::OffsetTurbulence);
        }
        if !(FRACTAL_MIN_COMPLEXITY..=FRACTAL_MAX_COMPLEXITY).contains(&self.complexity) {
            return Err(FractalNoiseError::Complexity);
        }
        if !self.sub_influence.is_finite()
            || !(FRACTAL_MIN_SUB_INFLUENCE..=FRACTAL_MAX_SUB_INFLUENCE)
                .contains(&self.sub_influence)
        {
            return Err(FractalNoiseError::SubInfluence);
        }
        if !self.sub_scale.is_finite()
            || !(FRACTAL_MIN_SUB_SCALE..=FRACTAL_MAX_SUB_SCALE).contains(&self.sub_scale)
        {
            return Err(FractalNoiseError::SubScale);
        }
        if !self.sub_rotation.is_finite() || self.sub_rotation.abs() > FRACTAL_MAX_ROTATION_DEGREES
        {
            return Err(FractalNoiseError::SubRotation);
        }
        if !self.sub_offset.iter().all(|value| value.is_finite())
            || self.sub_offset.iter().any(|value| value.abs() > FRACTAL_MAX_OFFSET_PIXELS)
        {
            return Err(FractalNoiseError::SubOffset);
        }
        if !self.centre_auxiliary.is_finite()
            || !(FRACTAL_MIN_CENTRE_AUXILIARY..=FRACTAL_MAX_CENTRE_AUXILIARY)
                .contains(&self.centre_auxiliary)
        {
            return Err(FractalNoiseError::CentreAuxiliary);
        }
        if !self.evolution.is_finite() || self.evolution.abs() > FRACTAL_MAX_ROTATION_DEGREES {
            return Err(FractalNoiseError::Evolution);
        }
        if !(FRACTAL_MIN_CYCLE..=FRACTAL_MAX_CYCLE).contains(&self.cycle) {
            return Err(FractalNoiseError::Cycle);
        }
        if self.random_seed > FRACTAL_MAX_RANDOM_SEED {
            return Err(FractalNoiseError::RandomSeed);
        }
        if !self.opacity.is_finite()
            || !(FRACTAL_MIN_OPACITY..=FRACTAL_MAX_OPACITY).contains(&self.opacity)
        {
            return Err(FractalNoiseError::Opacity);
        }
        Ok(())
    }
}

/// Rotates a point by a precomputed `(cos, sin)` pair.
fn rotate(x: f32, y: f32, cos: f32, sin: f32) -> (f32, f32) {
    (x * cos - y * sin, x * sin + y * cos)
}

/// Advances a `(cos, sin)` pair by another pair, in the same order the shader
/// uses.
fn advance_rotation(cos: f32, sin: f32, step_cos: f32, step_sin: f32) -> (f32, f32) {
    (
        cos * step_cos - sin * step_sin,
        cos * step_sin + sin * step_cos,
    )
}

/// Wraps `turns` into `[0, cycle)` with a formula that is identical in Rust and
/// WGSL (`floor`, never a remainder with a signed dividend).
fn wrap_turns(turns: f32, cycle: f32) -> f32 {
    turns - cycle * (turns / cycle).floor()
}

/// The Hermite ramp used by the soft-linear interpolation and by the terracing.
fn smooth_step(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Quantises a sample into plateaus with a short ramp between them.
fn terraced(value: f32) -> f32 {
    let scaled = value * 4.0;
    let floor = scaled.floor();
    let fraction = scaled - floor;
    (floor / 4.0 + smooth_step(0.0, 0.25, fraction) * 0.125).clamp(0.0, 1.0)
}

/// Bends the ends of an unbounded value back towards `[0, 1]`.
fn soft_clamp(value: f32) -> f32 {
    let distance = value - 0.5;
    let sign = if distance < 0.0 { -1.0 } else { 1.0 };
    let denominator = 1.0 - (-2.0_f32).exp();
    let saturation = 1.0 - (-2.0 * distance.abs()).exp();
    0.5 + 0.5 * sign * saturation / denominator
}

/// The address of the cell containing `value`, plus the loop-friendly form of
/// the same cast the shader uses (`bitcast<u32>(i32(floor(value)))`).
fn cell_of(value: f32) -> (u32, f32) {
    let floor = value.floor();
    (floor as i32 as u32, value - floor)
}

/// The interpolation weight for one axis.
fn interpolation_weight(noise_type: FractalNoiseType, fraction: f32) -> f32 {
    match noise_type {
        FractalNoiseType::SoftLinear => fraction * fraction * (3.0 - 2.0 * fraction),
        FractalNoiseType::Linear | FractalNoiseType::Spline => fraction,
    }
}

/// The four uniform Catmull-Rom (tension ½) weights of one axis.
fn catmull_rom_weights(fraction: f32) -> [f32; 4] {
    let squared = fraction * fraction;
    let cubed = squared * fraction;
    [
        -0.5 * cubed + squared - 0.5 * fraction,
        1.5 * cubed - 2.5 * squared + 1.0,
        -1.5 * cubed + 2.0 * squared + 0.5 * fraction,
        0.5 * cubed - 0.5 * squared,
    ]
}

fn neighbour(cell: u32, offset: i32) -> u32 {
    (cell as i32).wrapping_add(offset) as u32
}

/// Samples the shared hash between cells with the authored interpolation.
fn sample_noise(x: f32, y: f32, noise_type: FractalNoiseType, salt: u32) -> f32 {
    let (cell_x, fraction_x) = cell_of(x);
    let (cell_y, fraction_y) = cell_of(y);
    if noise_type == FractalNoiseType::Spline {
        let weights_x = catmull_rom_weights(fraction_x);
        let weights_y = catmull_rom_weights(fraction_y);
        let mut sum = 0.0;
        for (row, weight_y) in weights_y.iter().enumerate() {
            for (column, weight_x) in weights_x.iter().enumerate() {
                let sample = noise_sample(
                    neighbour(cell_x, column as i32 - 1),
                    neighbour(cell_y, row as i32 - 1),
                    salt,
                );
                sum += weight_x * weight_y * sample;
            }
        }
        return sum;
    }
    let weight_x = interpolation_weight(noise_type, fraction_x);
    let weight_y = interpolation_weight(noise_type, fraction_y);
    let top_left = noise_sample(cell_x, cell_y, salt);
    let top_right = noise_sample(neighbour(cell_x, 1), cell_y, salt);
    let bottom_left = noise_sample(cell_x, neighbour(cell_y, 1), salt);
    let bottom_right = noise_sample(neighbour(cell_x, 1), neighbour(cell_y, 1), salt);
    let top = top_left + (top_right - top_left) * weight_x;
    let bottom = bottom_left + (bottom_right - bottom_left) * weight_x;
    top + (bottom - top) * weight_y
}

/// The hoisted per-frame state of one fractal-noise evaluation.
struct FractalSetup {
    fractal_type: FractalType,
    noise_type: FractalNoiseType,
    invert: bool,
    overflow: FractalOverflow,
    contrast_gain: f32,
    brightness_offset: f32,
    opacity: f32,
    blend_mode: BlendMode,
    scale_x: f32,
    scale_y: f32,
    centre_x: f32,
    centre_y: f32,
    centre_shift: f32,
    offset: [f32; 2],
    rotation_cos: f32,
    rotation_sin: f32,
    perspective_offset: bool,
    evolution_cos: f32,
    evolution_sin: f32,
    complexity: u32,
    sub_influence: f32,
    lacunarity: f32,
    sub_rotation_cos: f32,
    sub_rotation_sin: f32,
    sub_offset: [f32; 2],
    seed_hash: u32,
}

impl FractalSetup {
    fn new(grade: &FractalNoiseGrade, width: usize, height: usize) -> Self {
        let (rotation_cos, rotation_sin) = grade.rotation_cos_sin();
        let (evolution_cos, evolution_sin) = grade.evolution_cos_sin();
        let (sub_rotation_cos, sub_rotation_sin) = grade.sub_rotation_cos_sin();
        Self {
            fractal_type: grade.fractal_type,
            noise_type: grade.noise_type,
            invert: grade.invert,
            overflow: grade.overflow,
            contrast_gain: grade.contrast_gain(),
            brightness_offset: grade.brightness_offset(),
            opacity: grade.opacity_fraction(),
            blend_mode: grade.blend_mode,
            scale_x: grade.scale_x(),
            scale_y: grade.scale_y(),
            centre_x: 0.5 * (width as f32 - 1.0),
            centre_y: 0.5 * (height as f32 - 1.0),
            centre_shift: grade.centre_shift(),
            offset: grade.offset_noise_units(),
            rotation_cos,
            rotation_sin,
            perspective_offset: grade.perspective_offset,
            evolution_cos,
            evolution_sin,
            complexity: grade.complexity,
            sub_influence: grade.sub_influence_fraction(),
            lacunarity: grade.lacunarity(),
            sub_rotation_cos,
            sub_rotation_sin,
            sub_offset: grade.sub_offset,
            seed_hash: FRACTAL_SEED_BASE ^ grade.random_seed,
        }
    }

    /// Evaluates the field and grades it into a display value.
    fn value_at(&self, column: usize, row: usize) -> f32 {
        let mut x = (column as f32 - self.centre_x) / (FRACTAL_FEATURE_PIXELS * self.scale_x);
        let mut y = (row as f32 - self.centre_y) / (FRACTAL_FEATURE_PIXELS * self.scale_y);
        x += self.centre_shift;
        y += self.centre_shift;
        let (rotated_x, rotated_y) = rotate(x, y, self.rotation_cos, self.rotation_sin);
        x = rotated_x + self.offset[0];
        y = rotated_y + self.offset[1];
        if self.perspective_offset {
            x += y * 0.5;
        }
        self.grade(self.field(x, y))
    }

    /// Runs the octave loop over one noise-space point.
    fn field(&self, x: f32, y: f32) -> f32 {
        let mut amplitude = 1.0;
        let mut total = 0.0;
        let mut frequency = 1.0;
        let mut offset = [0.0_f32; 2];
        let mut rotation_cos = 1.0;
        let mut rotation_sin = 0.0;
        let mut field = if self.fractal_type == FractalType::Multiply {
            1.0
        } else {
            0.0
        };
        for octave in 0..self.complexity {
            let (local_x, local_y) = rotate(
                x * frequency + offset[0],
                y * frequency + offset[1],
                rotation_cos,
                rotation_sin,
            );
            let index = octave as f32;
            let salt = self.seed_hash ^ octave.wrapping_mul(FRACTAL_OCTAVE_SALT_STEP);
            let sample = sample_noise(
                local_x + self.evolution_cos * 0.5 * index,
                local_y + self.evolution_sin * 0.5 * index,
                self.noise_type,
                salt,
            );
            field = match self.fractal_type {
                FractalType::Basic | FractalType::Swirly => field + amplitude * sample,
                FractalType::TurbulentSmooth | FractalType::TurbulentBasic => {
                    field + amplitude * (sample * 2.0 - 1.0).abs()
                }
                FractalType::TurbulentSharp => field.max(amplitude * (sample * 2.0 - 1.0).abs()),
                FractalType::Max => field.max(amplitude * sample),
                FractalType::Rock => field + amplitude * terraced(sample),
                FractalType::Threads => field + amplitude * (1.0 - (sample * 2.0 - 1.0).abs()),
                FractalType::Multiply => field * (0.5 + 0.5 * sample),
            };
            total += amplitude;
            amplitude = if self.fractal_type == FractalType::TurbulentBasic {
                amplitude * (self.sub_influence * self.sub_influence)
            } else {
                amplitude * self.sub_influence
            };
            frequency *= self.lacunarity;
            let (next_cos, next_sin) = advance_rotation(
                rotation_cos,
                rotation_sin,
                self.sub_rotation_cos,
                self.sub_rotation_sin,
            );
            rotation_cos = next_cos;
            rotation_sin = next_sin;
            if self.fractal_type == FractalType::Swirly {
                let (swirled_cos, swirled_sin) =
                    advance_rotation(rotation_cos, rotation_sin, SWIRLY_STEP_COS, SWIRLY_STEP_SIN);
                rotation_cos = swirled_cos;
                rotation_sin = swirled_sin;
            }
            offset[0] += self.sub_offset[0];
            offset[1] += self.sub_offset[1];
        }
        if self.fractal_type == FractalType::Multiply {
            return field;
        }
        field / total.max(1e-6)
    }

    /// Applies the overflow mode, the inversion and the contrast/brightness.
    fn grade(&self, value: f32) -> f32 {
        let mut value = match self.overflow {
            FractalOverflow::Clip => value.clamp(0.0, 1.0),
            FractalOverflow::SoftClamp => soft_clamp(value),
            FractalOverflow::Wrap => value - value.floor(),
        };
        if self.invert {
            value = 1.0 - value;
        }
        (value - 0.5) * self.contrast_gain + 0.5 + self.brightness_offset
    }

    /// Blends one grey value over the source pixel.
    fn composite(&self, pixel: [f32; 4], value: f32) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let blended = blend_mode_rgb(
            self.blend_mode,
            [pixel[0], pixel[1], pixel[2]],
            [value, value, value],
        );
        [
            pixel[0] + (blended[0] - pixel[0]) * self.opacity,
            pixel[1] + (blended[1] - pixel[1]) * self.opacity,
            pixel[2] + (blended[2] - pixel[2]) * self.opacity,
            pixel[3],
        ]
    }
}

/// Apply fractal noise to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn fractal_noise_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FractalNoiseGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_fractal_noise_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// The checkpointed kernel behind [`fractal_noise_frame_f32`].
pub(crate) fn apply_fractal_noise_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FractalNoiseGrade,
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
    let setup = FractalSetup::new(grade, width, height);
    let mut values = Vec::with_capacity(pixel_count);
    for index in 0..pixel_count {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        values.push(setup.value_at(index % width, index / width));
    }
    checkpoint()?;
    let frame = &mut working[..pixel_count];
    let rows = pixel_count / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, row) in band.chunks_mut(width).enumerate() {
            let row_index = row_start + row_offset;
            for (column, pixel) in row.iter_mut().enumerate() {
                *pixel = setup.composite(*pixel, values[row_index * width + column]);
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        catmull_rom_weights, fractal_noise_frame_f32, interpolation_weight, sample_noise,
        FractalNoiseError, FractalNoiseGrade, FractalNoiseType, FractalOverflow, FractalSetup,
        FractalType,
    };
    use crate::noise::noise_sample;
    use mondrian_core::BlendMode;

    const WIDTH: usize = 13;
    const HEIGHT: usize = 11;

    fn defaults() -> FractalNoiseGrade {
        FractalNoiseGrade::new(
            FractalType::Basic,
            FractalNoiseType::SoftLinear,
            false,
            100.0,
            0.0,
            FractalOverflow::Clip,
            0.0,
            true,
            100.0,
            100.0,
            100.0,
            [0.0, 0.0],
            false,
            6,
            50.0,
            50.0,
            0.0,
            [0.0, 0.0],
            0.0,
            0.0,
            false,
            1,
            0,
            100.0,
            BlendMode::Normal,
        )
        .expect("the checked-in defaults must validate")
    }

    fn mutated(mutate: impl FnOnce(&mut FractalNoiseGrade)) -> FractalNoiseGrade {
        let mut grade = defaults();
        mutate(&mut grade);
        grade
    }

    fn field_values(grade: &FractalNoiseGrade) -> Vec<f32> {
        let setup = FractalSetup::new(grade, WIDTH, HEIGHT);
        let mut values = Vec::with_capacity(WIDTH * HEIGHT);
        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                values.push(setup.value_at(column, row));
            }
        }
        values
    }

    fn source_frame() -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; WIDTH * HEIGHT];
        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                let value = 0.01 * (3 * column + 2 * row) as f32;
                pixels[row * WIDTH + column] = [value, 0.5 - value * 0.25, 0.25 + value, 1.0];
            }
        }
        pixels
    }

    fn run(grade: &FractalNoiseGrade) -> Vec<[f32; 4]> {
        let mut pixels = source_frame();
        assert!(fractal_noise_frame_f32(&mut pixels, WIDTH, HEIGHT, grade));
        pixels
    }

    fn mean(values: &[f32]) -> f32 {
        values.iter().sum::<f32>() / values.len() as f32
    }

    #[test]
    fn defaults_build_a_non_identity_grade() {
        let grade = defaults();
        assert_eq!(grade.fractal_type(), FractalType::Basic);
        assert_eq!(grade.noise_type(), FractalNoiseType::SoftLinear);
        assert_eq!(grade.overflow(), FractalOverflow::Clip);
        assert_eq!(grade.complexity(), 6);
        assert_eq!(grade.cycle(), 1);
        assert_eq!(grade.random_seed(), 0);
        assert_eq!(grade.blend_mode(), BlendMode::Normal);
        assert!(!grade.is_identity());
    }

    #[test]
    fn basic_field_is_continuous_and_varies() {
        let values = field_values(&defaults());
        assert!(values.iter().all(|value| value.is_finite()));
        let minimum = values.iter().copied().fold(f32::INFINITY, f32::min);
        let maximum = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            maximum - minimum > 0.1,
            "the field must vary: {minimum} .. {maximum}"
        );
        for row in 0..HEIGHT {
            for column in 1..WIDTH {
                let previous = values[row * WIDTH + column - 1];
                let current = values[row * WIDTH + column];
                assert!(
                    (current - previous).abs() < 0.1,
                    "row {row} column {column} jumped from {previous} to {current}"
                );
            }
        }
    }

    #[test]
    fn each_fractal_type_has_a_distinct_field() {
        // A scale of 1 % stretches the frame across many noise cells, so the
        // shapes cannot coincide just because the frame sits inside one cell.
        let mut fields = Vec::new();
        for fractal_type in FractalType::ALL {
            let grade = mutated(|grade| {
                grade.fractal_type = fractal_type;
                grade.scale = 1.0;
            });
            let values = field_values(&grade);
            assert!(
                values.iter().all(|value| value.is_finite()),
                "{fractal_type:?} produced a non-finite value"
            );
            if matches!(
                fractal_type,
                FractalType::TurbulentSmooth
                    | FractalType::TurbulentSharp
                    | FractalType::TurbulentBasic
                    | FractalType::Max
                    | FractalType::Rock
                    | FractalType::Threads
            ) {
                assert!(
                    values.iter().all(|value| *value >= 0.0),
                    "{fractal_type:?} must stay unipolar"
                );
            }
            if fractal_type == FractalType::Multiply {
                assert!(
                    values.iter().all(|value| (0.0..=1.0).contains(value)),
                    "multiply must stay inside the unit interval"
                );
            }
            fields.push((fractal_type, values));
        }
        for (index, (left_type, left)) in fields.iter().enumerate() {
            for (right_type, right) in fields.iter().skip(index + 1) {
                let difference = left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| (left - right).abs())
                    .fold(0.0_f32, f32::max);
                assert!(
                    difference > 1.0e-3,
                    "{left_type:?} and {right_type:?} collided (max difference {difference})"
                );
            }
        }
    }

    #[test]
    fn turbulent_basic_decays_harder_than_turbulent_smooth() {
        let smooth = field_values(&mutated(|grade| {
            grade.fractal_type = FractalType::TurbulentSmooth;
        }));
        let basic = field_values(&mutated(|grade| {
            grade.fractal_type = FractalType::TurbulentBasic;
        }));
        assert!(
            smooth.iter().zip(&basic).any(|(left, right)| (left - right).abs() > 1.0e-4),
            "the sharper decay must change the field"
        );
        // Both are sums of non-negative folded octaves, so the sharper decay can
        // only reduce the mean.
        assert!(mean(&basic) < mean(&smooth));
    }

    #[test]
    fn turbulent_sharp_keeps_the_strongest_octave() {
        let smooth = field_values(&mutated(|grade| {
            grade.fractal_type = FractalType::TurbulentSmooth;
        }));
        let sharp = field_values(&mutated(|grade| {
            grade.fractal_type = FractalType::TurbulentSharp;
        }));
        // A maximum over the same non-negative terms can never exceed their sum.
        assert!(sharp.iter().zip(&smooth).all(|(sharp, smooth)| *sharp <= smooth + 1.0e-6));
        assert!(sharp.iter().zip(&smooth).any(|(sharp, smooth)| *sharp < smooth - 1.0e-4));
    }

    #[test]
    fn multiply_starts_from_one() {
        let values = field_values(&mutated(|grade| {
            grade.fractal_type = FractalType::Multiply;
        }));
        // Seeding the field with zero would make every octave vanish.
        assert!(values.iter().any(|value| *value > 0.0));
        assert!(values.iter().all(|value| (0.0..=1.0).contains(value)));
    }

    #[test]
    fn soft_linear_interpolation_is_smoothstep() {
        assert_eq!(interpolation_weight(FractalNoiseType::SoftLinear, 0.0), 0.0);
        assert_eq!(interpolation_weight(FractalNoiseType::SoftLinear, 1.0), 1.0);
        assert!((interpolation_weight(FractalNoiseType::SoftLinear, 0.5) - 0.5).abs() < 1.0e-7);
        assert!(
            (interpolation_weight(FractalNoiseType::SoftLinear, 0.25) - 0.156_25).abs() < 1.0e-7
        );
        // At an integer coordinate the sample is exactly the shared hash sample.
        assert_eq!(
            sample_noise(3.0, -2.0, FractalNoiseType::SoftLinear, 7),
            noise_sample(3, (-2_i32) as u32, 7)
        );
        // At the cell centre the soft weight is a plain average of the corners.
        let centre = sample_noise(0.5, 0.5, FractalNoiseType::SoftLinear, 9);
        let corners = [
            noise_sample(0, 0, 9),
            noise_sample(1, 0, 9),
            noise_sample(0, 1, 9),
            noise_sample(1, 1, 9),
        ];
        let average = corners.iter().sum::<f32>() / 4.0;
        assert!((centre - average).abs() < 1.0e-6, "{centre} vs {average}");
    }

    #[test]
    fn linear_interpolation_uses_the_raw_fraction() {
        assert_eq!(interpolation_weight(FractalNoiseType::Linear, 0.25), 0.25);
        assert_eq!(interpolation_weight(FractalNoiseType::Linear, 0.5), 0.5);
        let quarter = sample_noise(0.25, 0.0, FractalNoiseType::Linear, 11);
        let left = noise_sample(0, 0, 11);
        let right = noise_sample(1, 0, 11);
        assert!((quarter - (left + (right - left) * 0.25)).abs() < 1.0e-6);
        // The linear weight differs from the smoothstep weight off the endpoints.
        assert_ne!(
            interpolation_weight(FractalNoiseType::Linear, 0.25),
            interpolation_weight(FractalNoiseType::SoftLinear, 0.25)
        );
    }

    #[test]
    fn spline_interpolation_uses_catmull_rom_weights() {
        assert_eq!(catmull_rom_weights(0.0), [0.0, 1.0, 0.0, 0.0]);
        let weights = catmull_rom_weights(0.5);
        assert_eq!(weights, [-0.0625, 0.5625, 0.5625, -0.0625]);
        let total: f32 = weights.iter().sum();
        assert!((total - 1.0).abs() < 1.0e-7);
        // At an integer coordinate the four taps collapse onto the centre cell.
        assert_eq!(
            sample_noise(4.0, 5.0, FractalNoiseType::Spline, 3),
            noise_sample(4, 5, 3)
        );
        // A half-cell sample mixes all four taps, so the spline differs from the
        // linear sampler away from the integer grid.
        assert_ne!(
            sample_noise(4.5, 5.0, FractalNoiseType::Spline, 3),
            sample_noise(4.5, 5.0, FractalNoiseType::Linear, 3)
        );
    }

    #[test]
    fn evolution_moves_the_field() {
        let still = field_values(&defaults());
        let drifting = field_values(&mutated(|grade| grade.evolution = 90.0));
        assert!(
            still.iter().zip(&drifting).any(|(left, right)| (left - right).abs() > 1.0e-4),
            "evolution must move the field"
        );
    }

    #[test]
    fn cycle_evolution_wraps_at_the_authored_turns() {
        let start = field_values(&defaults());
        let one_turn = field_values(&mutated(|grade| {
            grade.evolution = 360.0;
            grade.cycle_evolution = true;
            grade.cycle = 1;
        }));
        // A full turn with a one-turn cycle lands back on the unwrapped start.
        assert_eq!(start, one_turn);
        // Without the cycle the evolution keeps drifting past the first turn.
        let unwrapped = field_values(&mutated(|grade| grade.evolution = 360.0));
        assert_ne!(start, unwrapped);
        // A two-turn cycle sees the half-turn field instead of the start.
        let half_turn = field_values(&mutated(|grade| {
            grade.evolution = 360.0;
            grade.cycle_evolution = true;
            grade.cycle = 2;
        }));
        assert_ne!(start, half_turn);
    }

    #[test]
    fn clip_overflow_saturates_the_field() {
        // The small scale spreads the frame over many cells, so the signed basic
        // field reaches the negative clip somewhere.
        let clipped = field_values(&mutated(|grade| grade.scale = 1.0));
        assert!(clipped.iter().all(|value| (0.0..=1.0).contains(value)));
        assert!(
            clipped.contains(&0.0),
            "the signed basic field must reach the clip at zero"
        );
        // Everything above one saturates at one; the normalized basic field stays
        // below the upper clip on this frame, so only the lower end is observable.
        assert!(clipped.iter().all(|value| *value <= 1.0));
    }

    #[test]
    fn soft_clamp_bends_the_saturated_ends() {
        let clamped = field_values(&mutated(|grade| {
            grade.overflow = FractalOverflow::SoftClamp
        }));
        assert!(clamped.iter().all(|value| (0.0..=1.0).contains(value)));
        // The saturation never reaches the hard ends, so no pixel is exactly 0 or 1.
        assert!(clamped.iter().all(|value| *value > 0.0 && *value < 1.0));
        // The two modes disagree wherever the clip would have saturated.
        let clipped = field_values(&defaults());
        assert!(clamped.iter().zip(&clipped).any(|(left, right)| (left - right).abs() > 1.0e-3));
    }

    #[test]
    fn wrap_overflow_folds_the_field() {
        let wrapped = field_values(&mutated(|grade| {
            grade.overflow = FractalOverflow::Wrap;
            grade.scale = 1.0;
        }));
        assert!(wrapped.iter().all(|value| (0.0..1.0).contains(value)));
        let clipped = field_values(&mutated(|grade| grade.scale = 1.0));
        assert!(wrapped.iter().zip(&clipped).any(|(left, right)| (left - right).abs() > 1.0e-3));
    }

    #[test]
    fn invert_mirrors_the_field() {
        let normal = run(&defaults());
        let inverted = run(&mutated(|grade| grade.invert = true));
        for (index, (normal_pixel, inverted_pixel)) in normal.iter().zip(&inverted).enumerate() {
            // A Normal blend at full opacity writes the graded value itself into
            // all three channels, so the mirror is exact.
            for channel in 0..3 {
                let sum = normal_pixel[channel] + inverted_pixel[channel];
                assert!(
                    (sum - 1.0).abs() < 1.0e-6,
                    "pixel {index} channel {channel}: {sum}"
                );
            }
            assert_eq!(normal_pixel[3], inverted_pixel[3]);
        }
    }

    #[test]
    fn contrast_and_brightness_reshape_the_field() {
        let plain = field_values(&defaults());
        let contrasted = field_values(&mutated(|grade| grade.contrast = 200.0));
        for (index, (plain, contrasted)) in plain.iter().zip(&contrasted).enumerate() {
            let expected = (plain - 0.5) * 2.0 + 0.5;
            assert!(
                (contrasted - expected).abs() < 1.0e-6,
                "pixel {index}: {contrasted} vs {expected}"
            );
        }
        let brightened = field_values(&mutated(|grade| grade.brightness = 25.0));
        for (index, (plain, brightened)) in plain.iter().zip(&brightened).enumerate() {
            assert!(
                (brightened - (plain + 0.25)).abs() < 1.0e-6,
                "pixel {index}: {brightened} vs {}",
                plain + 0.25
            );
        }
    }

    #[test]
    fn opacity_mixes_back_towards_the_source() {
        let source = source_frame();
        let opaque = run(&defaults());
        let half = run(&mutated(|grade| grade.opacity = 50.0));
        for (index, (source_pixel, half_pixel)) in source.iter().zip(&half).enumerate() {
            for channel in 0..3 {
                let expected =
                    source_pixel[channel] + (opaque[index][channel] - source_pixel[channel]) * 0.5;
                assert!(
                    (half_pixel[channel] - expected).abs() < 1.0e-6,
                    "pixel {index} channel {channel}: {} vs {expected}",
                    half_pixel[channel]
                );
            }
            assert_eq!(half_pixel[3], source_pixel[3]);
        }
    }

    #[test]
    fn blend_mode_composites_against_the_source() {
        let source = source_frame();
        // At full opacity a Normal blend writes the graded value into every
        // channel; use it as the field reading the other modes composite with.
        let normal = run(&defaults());
        let multiplied = run(&mutated(|grade| grade.blend_mode = BlendMode::Multiply));
        for (index, (source_pixel, multiplied_pixel)) in source.iter().zip(&multiplied).enumerate()
        {
            for channel in 0..3 {
                let expected = source_pixel[channel] * normal[index][channel];
                assert!(
                    (multiplied_pixel[channel] - expected).abs() < 1.0e-6,
                    "pixel {index} channel {channel}: {} vs {expected}",
                    multiplied_pixel[channel]
                );
            }
            assert_eq!(multiplied_pixel[3], source_pixel[3]);
        }
    }

    #[test]
    fn transparent_pixels_are_returned_untouched() {
        let mut pixels = source_frame();
        pixels[7] = [0.25, 0.5, 0.75, 0.0];
        pixels[19] = [0.1, 0.2, 0.3, 0.5];
        let grade = defaults();
        assert!(fractal_noise_frame_f32(&mut pixels, WIDTH, HEIGHT, &grade));
        assert_eq!(pixels[7], [0.25, 0.5, 0.75, 0.0]);
        assert_eq!(pixels[19][3], 0.5);
        assert_ne!(pixels[19], [0.1, 0.2, 0.3, 0.5]);
    }

    #[test]
    fn zero_opacity_is_the_identity() {
        let grade = mutated(|grade| grade.opacity = 0.0);
        assert!(grade.is_identity());
        let mut pixels = source_frame();
        let untouched = pixels.clone();
        assert!(fractal_noise_frame_f32(&mut pixels, WIDTH, HEIGHT, &grade));
        assert_eq!(pixels, untouched);
        // The controlled kernel reports success without polling for empty frames.
        let mut work = pixels.clone();
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert_eq!(
            super::apply_fractal_noise_f32_controlled(
                &mut work,
                WIDTH,
                HEIGHT,
                &grade,
                &mut checkpoint
            ),
            Ok(true)
        );
    }

    /// One invalid-field mutation paired with the error it must produce.
    type RejectionCase = (FractalNoiseError, fn(&mut FractalNoiseGrade));

    #[test]
    fn out_of_range_parameters_are_rejected() {
        assert!(defaults().validate().is_ok());
        let cases: [RejectionCase; 17] = [
            (FractalNoiseError::Contrast, |grade| grade.contrast = -1.0),
            (FractalNoiseError::Brightness, |grade| {
                grade.brightness = 101.0
            }),
            (FractalNoiseError::Rotation, |grade| {
                grade.rotation = 360_001.0
            }),
            (FractalNoiseError::Scale, |grade| grade.scale = 0.5),
            (FractalNoiseError::ScaleWidth, |grade| {
                grade.scale_width = 1_001.0
            }),
            (FractalNoiseError::ScaleHeight, |grade| {
                grade.scale_height = f32::NAN
            }),
            (FractalNoiseError::OffsetTurbulence, |grade| {
                grade.offset_turbulence = [0.0, 10_001.0]
            }),
            (FractalNoiseError::Complexity, |grade| grade.complexity = 0),
            (FractalNoiseError::SubInfluence, |grade| {
                grade.sub_influence = 101.0
            }),
            (FractalNoiseError::SubScale, |grade| grade.sub_scale = -1.0),
            (FractalNoiseError::SubRotation, |grade| {
                grade.sub_rotation = -360_001.0
            }),
            (FractalNoiseError::SubOffset, |grade| {
                grade.sub_offset = [f32::INFINITY, 0.0]
            }),
            (FractalNoiseError::CentreAuxiliary, |grade| {
                grade.centre_auxiliary = 5.5
            }),
            (FractalNoiseError::Evolution, |grade| {
                grade.evolution = 360_001.0
            }),
            (FractalNoiseError::Cycle, |grade| grade.cycle = 89),
            (FractalNoiseError::RandomSeed, |grade| {
                grade.random_seed = 100_001
            }),
            (FractalNoiseError::Opacity, |grade| grade.opacity = 100.1),
        ];
        for (expected, mutate) in cases {
            let mut grade = defaults();
            mutate(&mut grade);
            assert_eq!(
                grade.validate(),
                Err(expected),
                "{expected:?} was not rejected"
            );
        }
    }

    #[test]
    fn gpu_controls_pack_every_cell() {
        let grade = mutated(|grade| {
            grade.fractal_type = FractalType::Threads;
            grade.noise_type = FractalNoiseType::Spline;
            grade.invert = true;
            grade.contrast = 175.0;
            grade.brightness = -40.0;
            grade.overflow = FractalOverflow::Wrap;
            grade.rotation = 90.0;
            grade.uniform_scale = false;
            grade.scale = 200.0;
            grade.scale_width = 300.0;
            grade.scale_height = 400.0;
            grade.offset_turbulence = [500.0, -300.0];
            grade.perspective_offset = true;
            grade.complexity = 4;
            grade.sub_influence = 25.0;
            grade.sub_scale = 100.0;
            grade.sub_rotation = 180.0;
            grade.sub_offset = [1.5, -2.5];
            grade.centre_auxiliary = 2.0;
            grade.evolution = 90.0;
            grade.cycle_evolution = true;
            grade.cycle = 4;
            grade.random_seed = 42;
            grade.opacity = 60.0;
            grade.blend_mode = BlendMode::Screen;
        });
        let uniform = grade.gpu_controls(21, 17);
        assert_eq!(uniform.size, [21.0, 17.0, 0.0, 0.0]);
        assert_eq!(uniform.params[0], 6.0);
        assert_eq!(uniform.params[1], 8.0);
        assert!(uniform.params[2].abs() < 1.0e-6);
        assert_eq!(uniform.params[3], 1.0);
        assert_eq!(uniform.extra0, [5.0, -3.0, 2.0, 1.0]);
        assert_eq!(uniform.extra1, [0.25, 4.0, 0.25, 3.0]);
        assert!((uniform.extra2[0] + 1.0).abs() < 1.0e-6);
        assert!(uniform.extra2[1].abs() < 1.0e-6);
        assert_eq!([uniform.extra2[2], uniform.extra2[3]], [1.5, -2.5]);
        assert_eq!(uniform.extra3, [1.75, -0.4, 7.0, 2.0]);
        assert_eq!(uniform.extra4, [2.0, 3.0, 42.0, 0.6]);
        assert_eq!(uniform.extra5, [3.0, 0.0, 0.0, 0.0]);
    }
}
