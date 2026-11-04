//! Fractal (GEN-03): an escape-time generator with six set modes.
//!
//! Fractal is a *generator*, not a filter: it reads no source pixel and always
//! writes `alpha = 1` outside the two transparency configurations. Every pixel
//! is evaluated analytically from frame-absolute coordinates and the 21 authored
//! scalars, so the fused point shader can paint it without scratch traffic.
//!
//! # Semantics
//!
//! The pixel centre is `column + 0.5, row + 0.5`. It is mapped through a
//! short-axis-normalised viewport (`half = max(min(width, height) * 0.5, 1)`)
//! and iterated as `z <- z^p + c` with `p` in `{2, 3, 5, 7}`. Escape is
//! `|z|^2 > max(4, |c|^2)`; the smooth term is the linear band-position
//! `(r2 - bailout) / max(r2 - last_r2, 1e-30)` (no logarithms). Seven closed-form
//! palettes turn escaped pixels into colour; the set interior stays black except
//! for the solid-colour palette, which swaps interior/exterior visibility when
//! `transparency` is enabled.
//!
//! # Effective limits
//!
//! Schema ranges and execution limits differ deliberately:
//!
//! * `Mandelbrot Escape Limit` / `Julia Escape Limit` are authored over
//!   `1..=1_000_000` but execution clamps the active family's limit to
//!   [`FRACTAL_MAX_ITERATIONS`] (2048). Values above that are accepted, not an
//!   error, and stop changing the image.
//! * Oversampling is capped at [`FRACTAL_MAX_SAMPLES`] (64), and the product
//!   `iterations * samples` is capped at
//!   [`FRACTAL_ITERATION_SAMPLE_BUDGET`] (8192 iter-px). A request above the
//!   product budget is degraded samples-first, then iterations.
//!
//! The clamp lives only in [`FractalBudget::resolve`], called from
//! [`FractalGrade::new`]. The CPU kernel reads the resolved struct and WGSL
//! reads the same resolved values from the uniform header; neither path clamps
//! again.
//!
//! # Deviations from the design notes
//!
//! * The design calls the tuple returned by [`FractalGrade::gpu_controls`]
//!   "19 f32"; the layout it specifies (`params/color/extra0/extra1/extra2`)
//!   contains 18 (`extra3` is unused). The 18-value implementation follows the
//!   arithmetic in §4, not the prose count.
//! * Design §2.1 clamps oversampling to `2..=16` before squaring, so
//!   `oversample_factor = 4` means a 4x4 grid (16 samples). The T3 row that
//!   expects `(300, 4)` to resolve to `side = 2` contradicts that formula and
//!   the default `300 x 16` budget; the formula wins here.
//! * The design's `header.y/z` values are the one-based menu positions (1..6,
//!   1..4), so this module's enum `index()` methods are one-based.
//! * The design's T3 mentions four rows but the active-family normalisation
//!   means an inactive family's escape limit is not the one that is clamped.

use std::fmt;

/// Half-extent of the viewport at zero magnification on the short axis.
pub const FRACTAL_BASE_EXTENT: f32 = 1.4;
/// Universal escape radius squared for every power and additive constant.
pub const FRACTAL_MIN_BAILOUT_R2: f32 = 4.0;
/// Execution ceiling for one escape-field iteration count.
pub const FRACTAL_MAX_ITERATIONS: u32 = 2_048;
/// Execution ceiling for per-pixel oversampling.
pub const FRACTAL_MAX_SAMPLES: u32 = 64;
/// Execution ceiling for `iterations * samples` (iter-px per pixel).
pub const FRACTAL_ITERATION_SAMPLE_BUDGET: u32 = 8_192;
/// Lower bound on overlay ghost iterations.
pub const FRACTAL_GHOST_MIN_ITERATIONS: u32 = 16;
/// The only guard used by [`apply_inversion`]'s reciprocal.
pub const FRACTAL_MIN_INVERSION_R2: f32 = 1.0e-30;
/// Denominator floor for the edge-highlight phase distance.
pub const FRACTAL_EDGE_PHASE_EPSILON: f32 = 0.02;
/// Maximum edge-highlight width, in band position units.
pub const FRACTAL_MAX_EDGE_WIDTH: f32 = 0.05;

/// Default menu position for `set_choice`.
pub const FRACTAL_DEFAULT_SET_CHOICE: u32 = 1;
/// Default menu position for `equation`.
pub const FRACTAL_DEFAULT_EQUATION: u32 = 1;
/// Default real coordinate of the Mandelbrot viewport centre.
pub const FRACTAL_DEFAULT_MANDELBROT_X: f32 = -0.703;
/// Default imaginary coordinate of the Mandelbrot viewport centre.
pub const FRACTAL_DEFAULT_MANDELBROT_Y: f32 = 0.0;
/// Default Mandelbrot magnification exponent.
pub const FRACTAL_DEFAULT_MANDELBROT_MAGNIFICATION: f32 = 0.0;
/// Default active-family escape limit.
pub const FRACTAL_DEFAULT_ESCAPE_LIMIT: f32 = 300.0;
/// Default Julia real component.
pub const FRACTAL_DEFAULT_JULIA_X: f32 = 0.0;
/// Default Julia imaginary component.
pub const FRACTAL_DEFAULT_JULIA_Y: f32 = 0.0;
/// Default Julia magnification exponent.
pub const FRACTAL_DEFAULT_JULIA_MAGNIFICATION: f32 = 0.0;
/// Default post-inversion offset.
pub const FRACTAL_DEFAULT_POST_INVERSION_OFFSET: [f32; 2] = [0.0, 0.0];
/// Default overlay amount.
pub const FRACTAL_DEFAULT_OVERLAY: f32 = 0.0;
/// Default transparency switch.
pub const FRACTAL_DEFAULT_TRANSPARENCY: f32 = 0.0;
/// Default menu position for `palette`.
pub const FRACTAL_DEFAULT_PALETTE: u32 = 1;
/// Default palette hue, in degrees.
pub const FRACTAL_DEFAULT_HUE: f32 = 0.0;
/// Default number of palette bands.
pub const FRACTAL_DEFAULT_CYCLE_STEPS: f32 = 29.0;
/// Default band phase offset, in degrees.
pub const FRACTAL_DEFAULT_CYCLE_OFFSET: f32 = 0.0;
/// Default edge-highlight amount.
pub const FRACTAL_DEFAULT_EDGE_HIGHLIGHT: f32 = 0.0;
/// Default menu position for `oversample_method`.
pub const FRACTAL_DEFAULT_OVERSAMPLE_METHOD: u32 = 1;
/// Default oversampling factor (interpreted as a grid side).
pub const FRACTAL_DEFAULT_OVERSAMPLE_FACTOR: f32 = 4.0;

/// Invalid fractal parameters, rejected before any pixel is written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FractalError {
    /// One of the scalar parameters was NaN or infinite.
    NonFinite {
        /// Stable parameter id.
        parameter: &'static str,
    },
    /// A finite value fell outside its authored range.
    OutOfRange {
        /// Stable parameter id.
        parameter: &'static str,
        /// Rejected value.
        value: f32,
        /// Inclusive lower bound.
        min: f32,
        /// Inclusive upper bound.
        max: f32,
    },
    /// `set_choice` was outside the six documented menu slots.
    InvalidSetChoice(u32),
    /// `equation` was outside the four documented menu slots.
    InvalidEquation(u32),
    /// `palette` was outside the seven documented menu slots.
    InvalidPalette(u32),
    /// `oversample_method` was outside the two documented menu slots.
    InvalidOversampleMethod(u32),
}

impl fmt::Display for FractalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite { parameter } => {
                write!(formatter, "fractal {parameter} must be finite")
            }
            Self::OutOfRange { parameter, value, min, max } => write!(
                formatter,
                "fractal {parameter} must be in [{min}, {max}], got {value}"
            ),
            Self::InvalidSetChoice(index) => {
                write!(
                    formatter,
                    "fractal set choice {index} is not a declared option"
                )
            }
            Self::InvalidEquation(index) => {
                write!(
                    formatter,
                    "fractal equation {index} is not a declared option"
                )
            }
            Self::InvalidPalette(index) => {
                write!(
                    formatter,
                    "fractal palette {index} is not a declared option"
                )
            }
            Self::InvalidOversampleMethod(index) => {
                write!(
                    formatter,
                    "fractal oversample method {index} is not a declared option"
                )
            }
        }
    }
}

impl std::error::Error for FractalError {}

/// The six set combinations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FractalSetChoice {
    /// Mandelbrot, `z0 = 0`.
    Mandelbrot,
    /// Mandelbrot with the viewport coordinate inverted.
    MandelbrotInverse,
    /// Mandelbrot over Julia, `z0 = julia`.
    MandelbrotOverJulia,
    /// Inverted Mandelbrot over Julia.
    MandelbrotInverseOverJulia,
    /// Julia, `c = julia`, `z0 = viewport`.
    Julia,
    /// Inverted Julia.
    JuliaInverse,
}

impl FractalSetChoice {
    /// Every set choice, in menu order.
    pub const ALL: [Self; 6] = [
        Self::Mandelbrot,
        Self::MandelbrotInverse,
        Self::MandelbrotOverJulia,
        Self::MandelbrotInverseOverJulia,
        Self::Julia,
        Self::JuliaInverse,
    ];

    /// One-based menu position, shared with the GPU uniform header.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::Mandelbrot => 1,
            Self::MandelbrotInverse => 2,
            Self::MandelbrotOverJulia => 3,
            Self::MandelbrotInverseOverJulia => 4,
            Self::Julia => 5,
            Self::JuliaInverse => 6,
        }
    }

    /// Stable authored key (underscore form).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Mandelbrot => "mandelbrot",
            Self::MandelbrotInverse => "mandelbrot_inverse",
            Self::MandelbrotOverJulia => "mandelbrot_over_julia",
            Self::MandelbrotInverseOverJulia => "mandelbrot_inverse_over_julia",
            Self::Julia => "julia",
            Self::JuliaInverse => "julia_inverse",
        }
    }

    /// Stable localisation suffix (hyphen form).
    #[must_use]
    pub const fn label_suffix(self) -> &'static str {
        match self {
            Self::Mandelbrot => "mandelbrot",
            Self::MandelbrotInverse => "mandelbrot-inverse",
            Self::MandelbrotOverJulia => "mandelbrot-over-julia",
            Self::MandelbrotInverseOverJulia => "mandelbrot-inverse-over-julia",
            Self::Julia => "julia",
            Self::JuliaInverse => "julia-inverse",
        }
    }

    /// Schema fallback label.
    #[must_use]
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Mandelbrot => "曼德布罗特",
            Self::MandelbrotInverse => "曼德布罗特反向",
            Self::MandelbrotOverJulia => "曼德布罗特在朱莉娅上",
            Self::MandelbrotInverseOverJulia => "曼德布罗特反向在朱莉娅上",
            Self::Julia => "朱莉娅",
            Self::JuliaInverse => "朱莉娅反向",
        }
    }

    /// Parses a stable authored key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// Resolves a one-based menu position, failing closed.
    ///
    /// # Errors
    ///
    /// Returns [`FractalError::InvalidSetChoice`] for any other position.
    pub fn from_index(index: u32) -> Result<Self, FractalError> {
        Self::ALL
            .get(usize::try_from(index.saturating_sub(1)).unwrap_or(usize::MAX))
            .copied()
            .ok_or(FractalError::InvalidSetChoice(index))
    }

    /// Whether this mode routes the viewport coordinate through inversion.
    #[must_use]
    pub const fn is_inverse(self) -> bool {
        matches!(
            self,
            Self::MandelbrotInverse | Self::MandelbrotInverseOverJulia | Self::JuliaInverse
        )
    }

    /// Whether the Mandelbrot family owns the viewport centre and budget.
    #[must_use]
    pub const fn uses_mandelbrot_family(self) -> bool {
        matches!(
            self,
            Self::Mandelbrot
                | Self::MandelbrotInverse
                | Self::MandelbrotOverJulia
                | Self::MandelbrotInverseOverJulia
        )
    }

    /// Whether the Julia constant is the starting point rather than `c`.
    #[must_use]
    pub const fn starts_at_julia(self) -> bool {
        matches!(
            self,
            Self::MandelbrotOverJulia | Self::MandelbrotInverseOverJulia
        )
    }

    /// Whether the Julia constant is `c` rather than `z0`.
    #[must_use]
    pub const fn uses_julia_constant(self) -> bool {
        matches!(self, Self::Julia | Self::JuliaInverse)
    }
}

/// The integer power of the iteration equation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FractalEquation {
    /// `z = z^2 + c`.
    Power2,
    /// `z = z^3 + c`.
    Power3,
    /// `z = z^5 + c`.
    Power5,
    /// `z = z^7 + c`.
    Power7,
}

impl FractalEquation {
    /// Every equation, in menu order.
    pub const ALL: [Self; 4] = [Self::Power2, Self::Power3, Self::Power5, Self::Power7];

    /// One-based menu position, shared with the GPU uniform header.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::Power2 => 1,
            Self::Power3 => 2,
            Self::Power5 => 3,
            Self::Power7 => 4,
        }
    }

    /// The integer exponent.
    #[must_use]
    pub const fn power(self) -> u32 {
        match self {
            Self::Power2 => 2,
            Self::Power3 => 3,
            Self::Power5 => 5,
            Self::Power7 => 7,
        }
    }

    /// Stable authored key (underscore form).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Power2 => "power_2",
            Self::Power3 => "power_3",
            Self::Power5 => "power_5",
            Self::Power7 => "power_7",
        }
    }

    /// Stable localisation suffix (hyphen form).
    #[must_use]
    pub const fn label_suffix(self) -> &'static str {
        match self {
            Self::Power2 => "power-2",
            Self::Power3 => "power-3",
            Self::Power5 => "power-5",
            Self::Power7 => "power-7",
        }
    }

    /// Schema fallback label.
    #[must_use]
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Power2 => "z = z² + c",
            Self::Power3 => "z = z³ + c",
            Self::Power5 => "z = z⁵ + c",
            Self::Power7 => "z = z⁷ + c",
        }
    }

    /// Parses a stable authored key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|equation| equation.key() == key)
    }

    /// Resolves a one-based menu position, failing closed.
    ///
    /// # Errors
    ///
    /// Returns [`FractalError::InvalidEquation`] for any other position.
    pub fn from_index(index: u32) -> Result<Self, FractalError> {
        Self::ALL
            .get(usize::try_from(index.saturating_sub(1)).unwrap_or(usize::MAX))
            .copied()
            .ok_or(FractalError::InvalidEquation(index))
    }
}

/// The seven output palettes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FractalPalette {
    /// Black-to-bright band gradient.
    LightnessGradient,
    /// Full-saturation hue wheel.
    HueWheel,
    /// Alternating black and white bands.
    BlackWhite,
    /// Solid hue with interior/exterior transparency.
    SolidColor,
    /// Hue of the escaping complex number.
    EscapeAngle,
    /// Triangle-wave grayscale.
    Grayscale,
    /// Four-colour Apple ][ palette.
    AppleII,
}

impl FractalPalette {
    /// Every palette, in menu order.
    pub const ALL: [Self; 7] = [
        Self::LightnessGradient,
        Self::HueWheel,
        Self::BlackWhite,
        Self::SolidColor,
        Self::EscapeAngle,
        Self::Grayscale,
        Self::AppleII,
    ];

    /// One-based menu position, shared with the GPU uniform.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::LightnessGradient => 1,
            Self::HueWheel => 2,
            Self::BlackWhite => 3,
            Self::SolidColor => 4,
            Self::EscapeAngle => 5,
            Self::Grayscale => 6,
            Self::AppleII => 7,
        }
    }

    /// Stable authored key (underscore form).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::LightnessGradient => "lightness_gradient",
            Self::HueWheel => "hue_wheel",
            Self::BlackWhite => "black_white",
            Self::SolidColor => "solid_color",
            Self::EscapeAngle => "escape_angle",
            Self::Grayscale => "grayscale",
            Self::AppleII => "apple_ii",
        }
    }

    /// Stable localisation suffix (hyphen form).
    #[must_use]
    pub const fn label_suffix(self) -> &'static str {
        match self {
            Self::LightnessGradient => "lightness-gradient",
            Self::HueWheel => "hue-wheel",
            Self::BlackWhite => "black-white",
            Self::SolidColor => "solid-color",
            Self::EscapeAngle => "escape-angle",
            Self::Grayscale => "grayscale",
            Self::AppleII => "apple-ii",
        }
    }

    /// Schema fallback label.
    #[must_use]
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::LightnessGradient => "亮度渐变",
            Self::HueWheel => "色相轮",
            Self::BlackWhite => "黑白",
            Self::SolidColor => "纯色",
            Self::EscapeAngle => "扩展角度",
            Self::Grayscale => "灰度",
            Self::AppleII => "苹果 ][",
        }
    }

    /// Whether the palette ignores `hue`.
    #[must_use]
    pub const fn ignores_hue(self) -> bool {
        matches!(self, Self::BlackWhite | Self::Grayscale | Self::AppleII)
    }

    /// Parses a stable authored key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|palette| palette.key() == key)
    }

    /// Resolves a one-based menu position, failing closed.
    ///
    /// # Errors
    ///
    /// Returns [`FractalError::InvalidPalette`] for any other position.
    pub fn from_index(index: u32) -> Result<Self, FractalError> {
        Self::ALL
            .get(usize::try_from(index.saturating_sub(1)).unwrap_or(usize::MAX))
            .copied()
            .ok_or(FractalError::InvalidPalette(index))
    }
}

/// Oversampling method menu. The execution path is the same for both; the two
/// options are documented and authored, and the property remains part of the
/// cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FractalOversampleMethod {
    /// Edge-detect (fast, may miss pixels).
    EdgeDetect,
    /// Brute-force per subpixel.
    BruteForce,
}

impl FractalOversampleMethod {
    /// Every method, in menu order.
    pub const ALL: [Self; 2] = [Self::EdgeDetect, Self::BruteForce];

    /// One-based menu position, shared with the GPU uniform.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::EdgeDetect => 1,
            Self::BruteForce => 2,
        }
    }

    /// Stable authored key (underscore form).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::EdgeDetect => "edge_detect",
            Self::BruteForce => "brute_force",
        }
    }

    /// Stable localisation suffix (hyphen form).
    #[must_use]
    pub const fn label_suffix(self) -> &'static str {
        match self {
            Self::EdgeDetect => "edge-detect",
            Self::BruteForce => "brute-force",
        }
    }

    /// Schema fallback label.
    #[must_use]
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::EdgeDetect => "边缘检测",
            Self::BruteForce => "强力",
        }
    }

    /// Parses a stable authored key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.key() == key)
    }

    /// Resolves a one-based menu position, failing closed.
    ///
    /// # Errors
    ///
    /// Returns [`FractalError::InvalidOversampleMethod`] for any other position.
    pub fn from_index(index: u32) -> Result<Self, FractalError> {
        Self::ALL
            .get(usize::try_from(index.saturating_sub(1)).unwrap_or(usize::MAX))
            .copied()
            .ok_or(FractalError::InvalidOversampleMethod(index))
    }
}

/// The resolved iteration and oversampling budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FractalBudget {
    /// Effective escape iteration count.
    pub iterations: u32,
    /// Square oversampling grid side: 1, 2, 4 or 8.
    pub sample_side: u32,
}

impl FractalBudget {
    /// The one place iteration and oversampling are bounded.
    ///
    /// Both execution paths read the result out of the grade: the CPU kernel
    /// from the struct, the shader from `header.w` and `extra2.x`. There is
    /// deliberately no second clamp in the shader.
    #[must_use]
    pub fn resolve(escape_limit: f32, oversample_factor: f32, edge_highlight: f32) -> Self {
        let mut iterations =
            (escape_limit.max(1.0).round() as u32).clamp(1, FRACTAL_MAX_ITERATIONS);
        let mut samples = if edge_highlight > 0.0 {
            1
        } else {
            let side = (oversample_factor.round() as u32).clamp(2, 16);
            side * side
        };
        samples = samples.clamp(1, FRACTAL_MAX_SAMPLES);
        while iterations.saturating_mul(samples) > FRACTAL_ITERATION_SAMPLE_BUDGET {
            let next = if samples >= 4 { samples / 4 } else { 1 };
            if next == samples {
                iterations = (iterations / 2).max(1);
            } else {
                samples = next;
            }
        }
        Self {
            iterations,
            sample_side: (samples as f32).sqrt() as u32,
        }
    }
}

/// Controls in GPU uniform order: `params`, `color`, `extra0`, `extra1`,
/// `extra2`. The unused `extra3` cell is omitted, as required by design §4.
pub type FractalControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 2]);

/// Validated, normalised fractal state shared by the CPU and GPU kernels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FractalGrade {
    set_choice: FractalSetChoice,
    equation: FractalEquation,
    palette: FractalPalette,
    oversample_method: FractalOversampleMethod,
    mandelbrot: [f32; 2],
    mandelbrot_magnification: f32,
    mandelbrot_escape_limit: f32,
    julia: [f32; 2],
    julia_magnification: f32,
    julia_escape_limit: f32,
    post_inversion_offset: [f32; 2],
    overlay: f32,
    transparency: f32,
    hue: f32,
    cycle_steps: f32,
    cycle_offset: f32,
    edge_highlight: f32,
    oversample_factor: f32,
    effective_iterations: u32,
    sample_side: u32,
    view_scale: f32,
    sample_weight: f32,
}

impl FractalGrade {
    /// Validates and normalises an authored parameter set.
    ///
    /// # Errors
    ///
    /// Returns [`FractalError`] for non-finite values, finite values outside
    /// their schema range, or menu positions the design does not define.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        set_choice: u32,
        equation: u32,
        mandelbrot: [f32; 2],
        mandelbrot_magnification: f32,
        mandelbrot_escape_limit: f32,
        julia: [f32; 2],
        julia_magnification: f32,
        julia_escape_limit: f32,
        post_inversion_offset: [f32; 2],
        overlay: f32,
        transparency: f32,
        palette: u32,
        hue: f32,
        cycle_steps: f32,
        cycle_offset: f32,
        edge_highlight: f32,
        oversample_method: u32,
        oversample_factor: f32,
    ) -> Result<Self, FractalError> {
        finite("mandelbrot.x", mandelbrot[0])?;
        finite("mandelbrot.y", mandelbrot[1])?;
        finite("mandelbrot.magnification", mandelbrot_magnification)?;
        finite("mandelbrot.escape_limit", mandelbrot_escape_limit)?;
        finite("julia.x", julia[0])?;
        finite("julia.y", julia[1])?;
        finite("julia.magnification", julia_magnification)?;
        finite("julia.escape_limit", julia_escape_limit)?;
        finite("post_inversion_offset.x", post_inversion_offset[0])?;
        finite("post_inversion_offset.y", post_inversion_offset[1])?;
        finite("overlay", overlay)?;
        finite("transparency", transparency)?;
        finite("hue", hue)?;
        finite("cycle_steps", cycle_steps)?;
        finite("cycle_offset", cycle_offset)?;
        finite("edge_highlight", edge_highlight)?;
        finite("oversample_factor", oversample_factor)?;

        bounded(
            "mandelbrot.magnification",
            mandelbrot_magnification,
            -29.0,
            100.0,
        )?;
        bounded(
            "mandelbrot.escape_limit",
            mandelbrot_escape_limit,
            1.0,
            1_000_000.0,
        )?;
        bounded("julia.magnification", julia_magnification, -29.0, 100.0)?;
        bounded("julia.escape_limit", julia_escape_limit, 1.0, 1_000_000.0)?;
        bounded("overlay", overlay, 0.0, 1.0)?;
        bounded("transparency", transparency, 0.0, 1.0)?;
        bounded("cycle_steps", cycle_steps, 1.0, 32_768.0)?;
        bounded("edge_highlight", edge_highlight, 0.0, 1.0)?;
        bounded("oversample_factor", oversample_factor, 2.0, 16.0)?;

        let set_choice = FractalSetChoice::from_index(set_choice)?;
        let equation = FractalEquation::from_index(equation)?;
        let palette = FractalPalette::from_index(palette)?;
        let oversample_method = FractalOversampleMethod::from_index(oversample_method)?;

        let uses_mandelbrot = set_choice.uses_mandelbrot_family();
        let uses_julia = !matches!(
            set_choice,
            FractalSetChoice::Mandelbrot | FractalSetChoice::MandelbrotInverse
        );

        let mandelbrot = if uses_mandelbrot {
            mandelbrot
        } else {
            [0.0, 0.0]
        };
        let mandelbrot_magnification = if uses_mandelbrot {
            mandelbrot_magnification
        } else {
            0.0
        };
        let mandelbrot_escape_limit = if uses_mandelbrot {
            mandelbrot_escape_limit
        } else {
            0.0
        };
        let julia = if uses_julia { julia } else { [0.0, 0.0] };
        let julia_magnification = if set_choice.uses_julia_constant() {
            julia_magnification
        } else {
            0.0
        };
        let julia_escape_limit = if set_choice.uses_julia_constant() {
            julia_escape_limit
        } else {
            0.0
        };
        let post_inversion_offset = if set_choice.is_inverse() {
            post_inversion_offset
        } else {
            [0.0, 0.0]
        };
        let hue = if palette.ignores_hue() {
            0.0
        } else {
            hue.rem_euclid(360.0)
        };

        let active_escape_limit = if uses_mandelbrot {
            mandelbrot_escape_limit
        } else {
            julia_escape_limit
        };
        let active_magnification = if uses_mandelbrot {
            mandelbrot_magnification
        } else {
            julia_magnification
        };
        let budget = FractalBudget::resolve(active_escape_limit, oversample_factor, edge_highlight);
        let view_scale = FRACTAL_BASE_EXTENT * (-f64::from(active_magnification)).exp2() as f32;
        let sample_count = budget.sample_side * budget.sample_side;
        let sample_weight = 1.0 / sample_count as f32;

        Ok(Self {
            set_choice,
            equation,
            palette,
            oversample_method,
            mandelbrot,
            mandelbrot_magnification,
            mandelbrot_escape_limit,
            julia,
            julia_magnification,
            julia_escape_limit,
            post_inversion_offset,
            overlay,
            transparency,
            hue,
            cycle_steps,
            cycle_offset,
            edge_highlight,
            oversample_factor,
            effective_iterations: budget.iterations,
            sample_side: budget.sample_side,
            view_scale,
            sample_weight,
        })
    }

    /// The selected set mode.
    #[must_use]
    pub const fn set_choice(self) -> FractalSetChoice {
        self.set_choice
    }

    /// The selected iteration equation.
    #[must_use]
    pub const fn equation(self) -> FractalEquation {
        self.equation
    }

    /// The selected palette.
    #[must_use]
    pub const fn palette(self) -> FractalPalette {
        self.palette
    }

    /// The selected oversampling method.
    #[must_use]
    pub const fn oversample_method(self) -> FractalOversampleMethod {
        self.oversample_method
    }

    /// Normalised Mandelbrot viewport centre.
    #[must_use]
    pub const fn mandelbrot(self) -> [f32; 2] {
        self.mandelbrot
    }

    /// Normalised Mandelbrot magnification exponent.
    #[must_use]
    pub const fn mandelbrot_magnification(self) -> f32 {
        self.mandelbrot_magnification
    }

    /// Normalised Mandelbrot escape limit (0 when the family is inactive).
    #[must_use]
    pub const fn mandelbrot_escape_limit(self) -> f32 {
        self.mandelbrot_escape_limit
    }

    /// Normalised Julia constant / viewport centre.
    #[must_use]
    pub const fn julia(self) -> [f32; 2] {
        self.julia
    }

    /// Normalised Julia magnification exponent.
    #[must_use]
    pub const fn julia_magnification(self) -> f32 {
        self.julia_magnification
    }

    /// Normalised Julia escape limit (0 when inactive).
    #[must_use]
    pub const fn julia_escape_limit(self) -> f32 {
        self.julia_escape_limit
    }

    /// Normalised post-inversion offset.
    #[must_use]
    pub const fn post_inversion_offset(self) -> [f32; 2] {
        self.post_inversion_offset
    }

    /// Overlay amount.
    #[must_use]
    pub const fn overlay(self) -> f32 {
        self.overlay
    }

    /// Transparency switch.
    #[must_use]
    pub const fn transparency(self) -> f32 {
        self.transparency
    }

    /// Palette hue in degrees.
    #[must_use]
    pub const fn hue(self) -> f32 {
        self.hue
    }

    /// Number of palette bands.
    #[must_use]
    pub const fn cycle_steps(self) -> f32 {
        self.cycle_steps
    }

    /// Palette phase offset in degrees.
    #[must_use]
    pub const fn cycle_offset(self) -> f32 {
        self.cycle_offset
    }

    /// Edge-highlight amount.
    #[must_use]
    pub const fn edge_highlight(self) -> f32 {
        self.edge_highlight
    }

    /// Authored oversampling factor.
    #[must_use]
    pub const fn oversample_factor(self) -> f32 {
        self.oversample_factor
    }

    /// Clamped escape iteration count.
    #[must_use]
    pub const fn effective_iterations(self) -> u32 {
        self.effective_iterations
    }

    /// Clamped oversampling grid side.
    #[must_use]
    pub const fn oversample_side(self) -> u32 {
        self.sample_side
    }

    /// Precomputed `1.4 * 2^-magnification`.
    #[must_use]
    pub const fn view_scale(self) -> f32 {
        self.view_scale
    }

    /// A fractal always paints something; see design §5.4.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        false
    }

    /// Packs the controls in uniform order. Header fields are not included.
    #[must_use]
    pub fn gpu_controls(self) -> FractalControls {
        (
            [
                self.mandelbrot[0],
                self.mandelbrot[1],
                self.mandelbrot_magnification,
                self.julia[0],
            ],
            [
                self.julia[1],
                self.julia_magnification,
                self.post_inversion_offset[0],
                self.post_inversion_offset[1],
            ],
            [
                self.overlay,
                self.transparency,
                self.palette.index() as f32,
                self.hue,
            ],
            [
                self.cycle_steps,
                self.cycle_offset,
                self.edge_highlight,
                self.oversample_method.index() as f32,
            ],
            [self.sample_side as f32, self.view_scale],
        )
    }

    /// The authored escape count at one pixel centre, before oversampling.
    #[must_use]
    pub fn escape_count(self, column: u32, row: u32, width: u32, height: u32) -> u32 {
        self.escape_at(column, row, width, height).iterations
    }

    /// The escape count plus how far into that final iteration the sample sat.
    ///
    /// Parity tests need both halves. A difference of one whole colour band is
    /// only legitimate where the sample sits on an escape edge, and the
    /// fraction is the observable that names that edge: it tends to zero or one
    /// exactly when one ulp of coordinate can flip the integer count.
    #[doc(hidden)]
    #[must_use]
    pub fn escape_band(self, column: u32, row: u32, width: u32, height: u32) -> (u32, f32) {
        let escape = self.escape_at(column, row, width, height);
        (escape.iterations, escape.fraction)
    }

    /// The full escape record at one pixel centre.
    fn escape_at(self, column: u32, row: u32, width: u32, height: u32) -> FractalEscape {
        let x = column as f32 + 0.5;
        let y = row as f32 + 0.5;
        let view = self.view_at(x, y, width, height);
        let (constant, start) = self.constant_and_start(view);
        fractal_escape(constant, start, self.equation, self.effective_iterations)
    }

    /// Evaluates one output pixel, including oversampling and overlay.
    #[must_use]
    pub fn pixel(self, column: u32, row: u32, width: u32, height: u32) -> [f32; 4] {
        let side = self.sample_side;
        if side <= 1 {
            return self.pixel_at(column as f32 + 0.5, row as f32 + 0.5, width, height);
        }
        let step = 1.0 / side as f32;
        let mut sum = [0.0f32; 4];
        for sub_y in 0..side {
            for sub_x in 0..side {
                let x = column as f32 + (sub_x as f32 + 0.5) * step;
                let y = row as f32 + (sub_y as f32 + 0.5) * step;
                let sample = self.pixel_at(x, y, width, height);
                for (accumulator, value) in sum.iter_mut().zip(sample) {
                    *accumulator += value;
                }
            }
        }
        for value in &mut sum {
            *value *= self.sample_weight;
        }
        sum
    }

    /// Evaluates one analytical sample at a complex viewport coordinate.
    fn pixel_at(self, x: f32, y: f32, width: u32, height: u32) -> [f32; 4] {
        self.pixel_view(self.view_at(x, y, width, height))
    }

    /// Whether a few floating-point steps of the complex view leave this pixel's
    /// colour alone.
    ///
    /// Only such samples are fair game for a GPU parity comparison: everywhere
    /// else a sub-ulp rounding difference legitimately changes the escape count
    /// and with it a whole colour band. The window is a few ulp rather than one
    /// because the shader rebuilds the view through its own rounding path.
    #[doc(hidden)]
    #[must_use]
    pub fn sample_is_stable(self, column: u32, row: u32, width: u32, height: u32) -> bool {
        // Oversampled pixels average `side * side` analytical samples, and a
        // single unstable sample moves the average by up to a full band over
        // sixteen. So every subsample has to be settled, not just the centre.
        let side = self.sample_side.max(1);
        let step = 1.0 / side as f32;
        for sub_y in 0..side {
            for sub_x in 0..side {
                let x = column as f32 + (sub_x as f32 + 0.5) * step;
                let y = row as f32 + (sub_y as f32 + 0.5) * step;
                if !self.view_is_stable(self.view_at(x, y, width, height)) {
                    return false;
                }
            }
        }
        true
    }

    /// Whether a few floating-point steps of `view` leave the colour alone.
    fn view_is_stable(self, view: [f32; 2]) -> bool {
        let base = self.pixel_view(view);
        [-4_i32, -1, 1, 4].into_iter().all(|step| {
            [(step, 0), (0, step)].into_iter().all(|(step_x, step_y)| {
                let sample = self.pixel_view([
                    f32::from_bits((view[0].to_bits() as i32 + step_x) as u32),
                    f32::from_bits((view[1].to_bits() as i32 + step_y) as u32),
                ]);
                (0..4).all(|channel| (sample[channel] - base[channel]).abs() <= 1.0e-6)
            })
        })
    }

    /// One analytical sample with the complex view moved by `step_x`/`step_y`
    /// floating-point steps.
    ///
    /// Parity tests use this to decide whether a disagreement is chaos (the CPU
    /// produces the same colour a few ulp away) or a genuine difference.
    #[doc(hidden)]
    #[must_use]
    pub fn sample_nudged(
        self,
        column: u32,
        row: u32,
        width: u32,
        height: u32,
        step_x: i32,
        step_y: i32,
    ) -> [f32; 4] {
        let mut view = self.view_at(column as f32 + 0.5, row as f32 + 0.5, width, height);
        view[0] = f32::from_bits((view[0].to_bits() as i32 + step_x) as u32);
        view[1] = f32::from_bits((view[1].to_bits() as i32 + step_y) as u32);
        self.pixel_view(view)
    }

    /// Evaluates one analytical sample at a complex viewport coordinate.
    fn pixel_view(self, view: [f32; 2]) -> [f32; 4] {
        let (constant, start) = self.constant_and_start(view);
        let escape = fractal_escape(constant, start, self.equation, self.effective_iterations);
        let inside = !escape.escaped;
        let u = if escape.escaped {
            (escape.iterations as f32 + escape.fraction) * (1.0 / self.effective_iterations as f32)
        } else {
            1.0
        };

        let hue_turns = self.hue * (1.0 / 360.0);
        let (mut rgb, alpha) = if self.palette == FractalPalette::SolidColor {
            let visible = if self.transparency > 0.5 {
                !inside
            } else {
                inside
            };
            (fractal_hue_rgb(hue_turns), if visible { 1.0 } else { 0.0 })
        } else {
            let rgb = if inside {
                [0.0; 3]
            } else {
                let phase = u * self.cycle_steps + self.cycle_offset * (1.0 / 360.0);
                let band = phase.floor();
                let w = phase - band;
                palette_rgb(
                    self.palette,
                    w,
                    band,
                    hue_turns,
                    escape.last_z,
                    self.cycle_steps,
                    self.edge_highlight,
                )
            };
            let alpha = if self.transparency > 0.5 && inside {
                0.0
            } else {
                1.0
            };
            (rgb, alpha)
        };

        if self.overlay > 0.0 && !inside {
            let ghost_iterations = FRACTAL_GHOST_MIN_ITERATIONS.max(self.effective_iterations / 4);
            let (ghost_constant, ghost_start) = self.ghost_constant_and_start(view);
            let ghost =
                fractal_escape(ghost_constant, ghost_start, self.equation, ghost_iterations);
            let ghost_inside = !ghost.escaped;
            let ghost_u = if ghost.escaped {
                (ghost.iterations as f32 + ghost.fraction) * (1.0 / ghost_iterations as f32)
            } else {
                1.0
            };
            let ghost_rgb = if ghost_inside {
                [0.0; 3]
            } else {
                let phase = ghost_u * self.cycle_steps + self.cycle_offset * (1.0 / 360.0);
                let band = phase.floor();
                let w = phase - band;
                palette_rgb(
                    self.palette,
                    w,
                    band,
                    hue_turns,
                    ghost.last_z,
                    self.cycle_steps,
                    0.0,
                )
            };
            for channel in 0..3 {
                rgb[channel] = rgb[channel].max(ghost_rgb[channel] * self.overlay);
            }
        }

        [rgb[0], rgb[1], rgb[2], alpha]
    }

    /// Maps a frame-absolute pixel centre into the complex viewport.
    fn view_at(self, x: f32, y: f32, width: u32, height: u32) -> [f32; 2] {
        let width = width.max(1) as f32;
        let height = height.max(1) as f32;
        let half = (width.min(height) * 0.5).max(1.0);
        let centre = if self.set_choice.uses_mandelbrot_family() {
            self.mandelbrot
        } else {
            self.julia
        };
        let u = (x - width * 0.5) / half;
        let v = (y - height * 0.5) / half;
        [
            centre[0] + u * self.view_scale,
            centre[1] + v * self.view_scale,
        ]
    }

    /// Main-family additive constant and starting point at `view`.
    fn constant_and_start(self, view: [f32; 2]) -> ([f32; 2], [f32; 2]) {
        if self.set_choice.uses_mandelbrot_family() {
            let constant = if self.set_choice.is_inverse() {
                apply_inversion(view, self.post_inversion_offset)
            } else {
                view
            };
            let start = if self.set_choice.starts_at_julia() {
                self.julia
            } else {
                [0.0, 0.0]
            };
            (constant, start)
        } else {
            let start = if self.set_choice.is_inverse() {
                apply_inversion(view, self.post_inversion_offset)
            } else {
                view
            };
            (self.julia, start)
        }
    }

    /// The opposite family's constant and starting point, for overlay.
    fn ghost_constant_and_start(self, view: [f32; 2]) -> ([f32; 2], [f32; 2]) {
        if self.set_choice.uses_mandelbrot_family() {
            (self.julia, view)
        } else {
            (view, [0.0, 0.0])
        }
    }
}

/// The one place any inverse mode inverts anything.
///
/// `post` is the post-inversion offset pair from the schema. Modes 2/4/6
/// funnel through here; the CPU kernel calls it directly and the WGSL branch
/// carries the same four lines. No other code may take a reciprocal.
#[must_use]
pub fn apply_inversion(value: [f32; 2], post: [f32; 2]) -> [f32; 2] {
    let r2 = value[0] * value[0] + value[1] * value[1];
    let r2c = r2.max(FRACTAL_MIN_INVERSION_R2);
    [value[0] / r2c + post[0], -(value[1] / r2c) + post[1]]
}

/// Six-segment hue wheel in `[0, 1)`, with no trigonometric calls.
#[must_use]
pub fn fractal_hue_rgb(hue: f32) -> [f32; 3] {
    let t = (hue - hue.floor()) * 6.0;
    [
        clamp01((t - 3.0).abs() - 1.0),
        clamp01(2.0 - (t - 2.0).abs()),
        clamp01(2.0 - (t - 4.0).abs()),
    ]
}

/// Octant-folded arctangent in turns `[0, 1)`, with no trigonometric calls.
///
/// Maximum error is about 0.0006 turns (A&S 4.4.47's one rational step).
#[must_use]
pub fn fractal_atan_turns(y: f32, x: f32) -> f32 {
    let ax = x.abs();
    let ay = y.abs();
    let steep = ay > ax;
    let t = if steep {
        ax / ay.max(1.0e-30)
    } else {
        ay / ax.max(1.0e-30)
    };
    let a = t * (0.125 + 0.04345 * (1.0 - t));
    let quarter = if steep { 0.25 - a } else { a };
    let half = if y < 0.0 { 1.0 - quarter } else { quarter };
    let turn = if x < 0.0 { 0.5 - half } else { half };
    turn - turn.floor()
}

/// Colours one escaped sample for the selected palette.
fn palette_rgb(
    palette: FractalPalette,
    w: f32,
    band: f32,
    hue_turns: f32,
    last_z: [f32; 2],
    cycle_steps: f32,
    edge_highlight: f32,
) -> [f32; 3] {
    if palette == FractalPalette::SolidColor {
        return fractal_hue_rgb(hue_turns);
    }
    let k = hue_turns + band * (1.0 / cycle_steps);
    let mut rgb = match palette {
        FractalPalette::LightnessGradient => {
            let hue = fractal_hue_rgb(k);
            [
                w * (0.5 + 0.5 * hue[0]),
                w * (0.5 + 0.5 * hue[1]),
                w * (0.5 + 0.5 * hue[2]),
            ]
        }
        FractalPalette::HueWheel => fractal_hue_rgb(k),
        FractalPalette::BlackWhite => {
            let value = band - 2.0 * (band * 0.5).floor();
            [value, value, value]
        }
        FractalPalette::SolidColor => fractal_hue_rgb(hue_turns),
        FractalPalette::EscapeAngle => {
            fractal_hue_rgb(fractal_atan_turns(last_z[1], last_z[0]) + hue_turns)
        }
        FractalPalette::Grayscale => {
            let value = 1.0 - (2.0 * w - 1.0).abs();
            [value, value, value]
        }
        FractalPalette::AppleII => {
            let q = (w * 4.0).floor().min(3.0);
            let c0 = [0.15, 0.85, 0.25];
            let c1 = [0.85, 0.30, 0.85];
            let c2 = [0.95, 0.95, 0.95];
            let c3 = [0.95, 0.55, 0.10];
            if q > 2.5 {
                c3
            } else if q > 1.5 {
                c2
            } else if q > 0.5 {
                c1
            } else {
                c0
            }
        }
    };
    if edge_highlight > 0.0 {
        let distance = w.min(1.0 - w);
        let width = edge_highlight * FRACTAL_MAX_EDGE_WIDTH;
        let edge = ((width - distance) / width.max(FRACTAL_EDGE_PHASE_EPSILON)).clamp(0.0, 1.0);
        for channel in &mut rgb {
            *channel += (1.0 - *channel) * edge;
        }
    }
    rgb
}

/// One escape-field result.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FractalEscape {
    escaped: bool,
    iterations: u32,
    fraction: f32,
    last_z: [f32; 2],
}

/// Runs the escape iteration with explicit integer powers.
fn fractal_escape(
    constant: [f32; 2],
    start: [f32; 2],
    equation: FractalEquation,
    max_iterations: u32,
) -> FractalEscape {
    // The bailout is derived from the constant itself, so `|z| == bailout` is a
    // common exact tie: the first iterate is the constant. Left exact, one ulp
    // of codegen difference between the two squared-norm sums decides whether
    // the tie escapes, which is not portable across compilers. A few ulp of
    // headroom makes the decision deterministic without moving a real band.
    let bailout_r2 = (constant[0] * constant[0] + constant[1] * constant[1])
        .max(FRACTAL_MIN_BAILOUT_R2)
        * 1.000_001;
    let mut z = start;
    let mut last_r2 = 0.0f32;
    let mut iterations = 0u32;
    let mut fraction = 0.0f32;
    let mut escaped = false;
    for _ in 0..max_iterations {
        let r2 = z[0] * z[0] + z[1] * z[1];
        if r2 > bailout_r2 {
            fraction =
                ((r2 - bailout_r2) / (r2 - last_r2).max(FRACTAL_MIN_INVERSION_R2)).clamp(0.0, 1.0);
            escaped = true;
            break;
        }
        last_r2 = r2;
        let squared = complex_mul(z, z);
        z = match equation {
            FractalEquation::Power2 => complex_add(squared, constant),
            FractalEquation::Power3 => complex_add(complex_mul(squared, z), constant),
            FractalEquation::Power5 => {
                let fourth = complex_mul(squared, squared);
                complex_add(complex_mul(fourth, z), constant)
            }
            FractalEquation::Power7 => {
                let fourth = complex_mul(squared, squared);
                let sixth = complex_mul(fourth, squared);
                complex_add(complex_mul(sixth, z), constant)
            }
        };
        iterations += 1;
    }
    if !escaped {
        iterations = max_iterations;
    }
    FractalEscape { escaped, iterations, fraction, last_z: z }
}

/// Complex multiplication in a fixed operation order.
fn complex_mul(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] * b[0] - a[1] * b[1], a[0] * b[1] + a[1] * b[0]]
}

/// Complex addition in a fixed operation order.
fn complex_add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

/// Clamps to `[0, 1]` without a generic bound.
fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// Rejects non-finite parameters.
fn finite(parameter: &'static str, value: f32) -> Result<(), FractalError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(FractalError::NonFinite { parameter })
    }
}

/// Rejects finite parameters outside an inclusive range.
fn bounded(parameter: &'static str, value: f32, min: f32, max: f32) -> Result<(), FractalError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(FractalError::OutOfRange { parameter, value, min, max })
    }
}

const CHECKPOINT_STRIDE: usize = 4_096;

/// Applies the generator to a whole frame.
///
/// Returns `false` when `width * height` overflows or the buffer is too short.
#[must_use]
pub fn fractal_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FractalGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_fractal_f32_region_controlled(
        pixels,
        0,
        0,
        width,
        height,
        width,
        height,
        grade,
        &mut checkpoint,
    )
    .unwrap_or(false)
}

/// Applies the generator to a region whose origin is frame-absolute.
///
/// `frame_width`/`frame_height` are the full layer extent; the viewport is
/// always derived from those, never from the region size.
///
/// # Errors
///
/// Propagates the first error returned by `checkpoint`, leaving already written
/// chunks in place.
pub(crate) fn apply_fractal_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region_x: usize,
    region_y: usize,
    width: usize,
    height: usize,
    frame_width: usize,
    frame_height: usize,
    grade: &FractalGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || pixels.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let frame_width = u32::try_from(frame_width).unwrap_or(u32::MAX);
    let frame_height = u32::try_from(frame_height).unwrap_or(u32::MAX);
    let region_x = u32::try_from(region_x).unwrap_or(u32::MAX);
    let region_y = u32::try_from(region_y).unwrap_or(u32::MAX);
    let rows_per_chunk = (CHECKPOINT_STRIDE / width).max(1);
    let mut start = 0usize;
    while start < height {
        checkpoint()?;
        let rows = rows_per_chunk.min(height - start);
        let end = start + rows;
        crate::parallel::for_each_row_band(
            &mut pixels[start * width..end * width],
            width,
            0..rows,
            |first_row, band| {
                for (offset, chunk) in band.chunks_mut(width).enumerate() {
                    let row = region_y.saturating_add(
                        u32::try_from(start + first_row + offset).unwrap_or(u32::MAX),
                    );
                    for (column, pixel) in chunk.iter_mut().enumerate() {
                        let column =
                            region_x.saturating_add(u32::try_from(column).unwrap_or(u32::MAX));
                        *pixel = grade.pixel(column, row, frame_width, frame_height);
                    }
                }
            },
        );
        start = end;
    }
    checkpoint()?;
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectExecutionError, EffectRenderOp};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;

    fn grade(mode: u32, equation: u32, palette: u32) -> FractalGrade {
        FractalGrade::new(
            mode,
            equation,
            [-0.703, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            0.0,
            palette,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            4.0,
        )
        .expect("default fractal grade")
    }

    fn grade_with_post(post: [f32; 2]) -> FractalGrade {
        FractalGrade::new(
            2,
            1,
            [-0.703, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            300.0,
            post,
            0.0,
            0.0,
            1,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            4.0,
        )
        .expect("inverse fractal grade")
    }

    fn op_hash(grade: FractalGrade) -> u64 {
        let mut hasher = DefaultHasher::new();
        EffectRenderOp::Fractal { grade }.hash_signature(&mut hasher);
        hasher.finish()
    }

    fn palette(kind: FractalPalette, w: f32, band: f32, hue: f32, last_z: [f32; 2]) -> [f32; 3] {
        palette_rgb(kind, w, band, hue, last_z, 29.0, 0.0)
    }

    #[test]
    fn t1_escape_counts_match_the_documented_probes() {
        let zero = fractal_escape([0.0, 0.0], [0.0, 0.0], FractalEquation::Power2, 300);
        assert!(!zero.escaped, "c = 0 is in the Mandelbrot set");
        assert_eq!(zero.iterations, 300);

        let two = fractal_escape([2.0, 0.0], [0.0, 0.0], FractalEquation::Power2, 300);
        assert!(two.escaped);
        assert_eq!(two.iterations, 2);

        let minus_two = fractal_escape([-2.0, 0.0], [0.0, 0.0], FractalEquation::Power2, 300);
        assert!(!minus_two.escaped);
        assert_eq!(minus_two.iterations, 300);

        let half = fractal_escape([0.5, 0.0], [0.0, 0.0], FractalEquation::Power2, 300);
        assert!(half.escaped);
        assert_eq!(half.iterations, 5);
    }

    #[test]
    fn t2_escape_counts_are_conjugate_symmetric() {
        for y_index in -16..=16 {
            for x_index in -16..=16 {
                let x = x_index as f32 * 0.15625;
                let y = y_index as f32 * 0.09375;
                let upper = fractal_escape([x, y], [0.0, 0.0], FractalEquation::Power2, 96);
                let lower = fractal_escape([x, -y], [0.0, 0.0], FractalEquation::Power2, 96);
                assert_eq!(upper.iterations, lower.iterations, "x={x} y={y}");
                assert_eq!(upper.escaped, lower.escaped, "x={x} y={y}");
            }
        }
    }

    #[test]
    fn t3_budget_degrades_samples_before_iterations() {
        assert_eq!(
            FractalBudget::resolve(1_000_000.0, 16.0, 0.0),
            FractalBudget { iterations: 2_048, sample_side: 2 }
        );
        assert_eq!(
            FractalBudget::resolve(300.0, 16.0, 0.0),
            FractalBudget { iterations: 300, sample_side: 4 }
        );
        assert_eq!(
            FractalBudget::resolve(300.0, 4.0, 0.0),
            FractalBudget { iterations: 300, sample_side: 4 }
        );
        assert_eq!(
            FractalBudget::resolve(300.0, 16.0, 0.5),
            FractalBudget { iterations: 300, sample_side: 1 }
        );
    }

    #[test]
    fn t4_palette_endpoints_are_exact() {
        assert_eq!(
            palette(FractalPalette::LightnessGradient, 0.0, 0.0, 0.0, [1.0, 0.0]),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            palette(FractalPalette::LightnessGradient, 0.5, 0.0, 0.0, [1.0, 0.0]),
            [0.5, 0.25, 0.25]
        );
        assert_eq!(
            palette(
                FractalPalette::LightnessGradient,
                0.999,
                0.0,
                0.0,
                [1.0, 0.0]
            ),
            [0.999, 0.4995, 0.4995]
        );

        assert_eq!(
            palette(FractalPalette::HueWheel, 0.0, 0.0, 0.0, [1.0, 0.0]),
            [1.0, 0.0, 0.0]
        );
        assert_eq!(
            palette(FractalPalette::HueWheel, 0.999, 0.0, 0.0, [1.0, 0.0]),
            [1.0, 0.0, 0.0]
        );

        assert_eq!(
            palette(FractalPalette::BlackWhite, 0.25, 0.0, 0.0, [1.0, 0.0]),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            palette(FractalPalette::BlackWhite, 0.25, 2.0, 0.0, [1.0, 0.0]),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            palette(FractalPalette::BlackWhite, 0.25, 1.0, 0.0, [1.0, 0.0]),
            [1.0, 1.0, 1.0]
        );

        assert_eq!(
            palette(FractalPalette::SolidColor, 0.0, 0.0, 0.0, [1.0, 0.0]),
            [1.0, 0.0, 0.0]
        );
        assert_eq!(
            palette(FractalPalette::EscapeAngle, 0.0, 0.0, 0.0, [1.0, 0.0]),
            [1.0, 0.0, 0.0]
        );

        assert_eq!(
            palette(FractalPalette::Grayscale, 0.0, 0.0, 0.0, [1.0, 0.0]),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            palette(FractalPalette::Grayscale, 0.5, 0.0, 0.0, [1.0, 0.0]),
            [1.0, 1.0, 1.0]
        );
        let grayscale_edge = 1.0 - (2.0f32 * 0.999 - 1.0).abs();
        assert_eq!(
            palette(FractalPalette::Grayscale, 0.999, 0.0, 0.0, [1.0, 0.0]),
            [grayscale_edge; 3]
        );

        assert_eq!(
            palette(FractalPalette::AppleII, 0.0, 0.0, 0.0, [1.0, 0.0]),
            [0.15, 0.85, 0.25]
        );
        assert_eq!(
            palette(FractalPalette::AppleII, 0.5, 0.0, 0.0, [1.0, 0.0]),
            [0.95, 0.95, 0.95]
        );
        assert_eq!(
            palette(FractalPalette::AppleII, 0.999, 0.0, 0.0, [1.0, 0.0]),
            [0.95, 0.55, 0.10]
        );
    }

    #[test]
    fn t5_hue_zero_and_360_are_identical() {
        let zero = FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            96.0,
            [0.0, 0.0],
            0.0,
            96.0,
            [0.0, 0.0],
            0.0,
            0.0,
            2,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            2.0,
        )
        .unwrap();
        let full = FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            96.0,
            [0.0, 0.0],
            0.0,
            96.0,
            [0.0, 0.0],
            0.0,
            0.0,
            2,
            360.0,
            29.0,
            0.0,
            0.0,
            1,
            2.0,
        )
        .unwrap();
        for row in 0..16 {
            for column in 0..24 {
                assert_eq!(
                    zero.pixel(column, row, 24, 16).map(f32::to_bits),
                    full.pixel(column, row, 24, 16).map(f32::to_bits),
                    "pixel {column},{row}"
                );
            }
        }
    }

    #[test]
    fn t6_atan_matches_eight_directions() {
        let cases = [
            ([0.0, 1.0], 0.0),
            ([1.0, 1.0], 0.125),
            ([1.0, 0.0], 0.25),
            ([1.0, -1.0], 0.375),
            ([0.0, -1.0], 0.5),
            ([-1.0, -1.0], 0.625),
            ([-1.0, 0.0], 0.75),
            ([-1.0, 1.0], 0.875),
        ];
        for (value, expected) in cases {
            let actual = fractal_atan_turns(value[0], value[1]);
            assert!(
                (actual - expected).abs() <= 0.0007,
                "{value:?} -> {actual}, want {expected}"
            );
        }
    }

    #[test]
    fn t7_flat_subsamples_average_to_the_single_sample() {
        let grade = grade(1, 1, 2);
        let side = grade.oversample_side();
        assert_eq!(side, 4);
        let step = 1.0 / side as f32;
        let mut checked = 0usize;
        for row in 0..8 {
            for column in 0..8 {
                let mut samples = Vec::new();
                for sub_y in 0..side {
                    for sub_x in 0..side {
                        let x = column as f32 + (sub_x as f32 + 0.5) * step;
                        let y = row as f32 + (sub_y as f32 + 0.5) * step;
                        samples.push(grade.pixel_at(x, y, 8, 8));
                    }
                }
                if samples.windows(2).all(|pair| pair[0] == pair[1]) {
                    assert_eq!(
                        grade.pixel(column, row, 8, 8).map(f32::to_bits),
                        samples[0].map(f32::to_bits)
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "test frame had no flat pixel");
    }

    #[test]
    fn t8_inactive_parameters_do_not_change_pixels_or_hash() {
        let base = FractalGrade::new(
            5,
            1,
            [0.0, 0.0],
            0.0,
            96.0,
            [0.0, 0.0],
            0.0,
            96.0,
            [0.0, 0.0],
            0.0,
            0.0,
            2,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            2.0,
        )
        .unwrap();
        let changed = FractalGrade::new(
            5,
            1,
            [5.0, -3.0],
            2.0,
            16.0,
            [0.0, 0.0],
            0.0,
            96.0,
            [4.0, 3.0],
            0.0,
            0.0,
            2,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            2.0,
        )
        .unwrap();
        assert_eq!(base.gpu_controls(), changed.gpu_controls());
        assert_eq!(op_hash(base), op_hash(changed));
        for row in 0..12 {
            for column in 0..12 {
                assert_eq!(
                    base.pixel(column, row, 12, 12),
                    changed.pixel(column, row, 12, 12)
                );
            }
        }
    }

    #[test]
    fn t9_no_mode_combination_is_an_identity() {
        for set_choice in 1..=6 {
            for palette in 1..=7 {
                for method in 1..=2 {
                    let grade = FractalGrade::new(
                        set_choice,
                        1,
                        [0.0, 0.0],
                        0.0,
                        32.0,
                        [0.0, 0.0],
                        0.0,
                        32.0,
                        [0.0, 0.0],
                        0.0,
                        0.0,
                        palette,
                        0.0,
                        29.0,
                        0.0,
                        0.0,
                        method,
                        2.0,
                    )
                    .unwrap();
                    assert!(
                        !grade.is_identity(),
                        "set {set_choice} palette {palette} method {method}"
                    );
                }
            }
        }
    }

    #[test]
    fn t10_edge_zero_is_the_default_and_edge_forces_one_sample() {
        let default = grade(1, 1, 1);
        let edge_zero = FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            0.0,
            1,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            4.0,
        )
        .unwrap();
        assert_eq!(default.gpu_controls(), edge_zero.gpu_controls());
        for row in 0..8 {
            for column in 0..8 {
                assert_eq!(
                    default.pixel(column, row, 8, 8),
                    edge_zero.pixel(column, row, 8, 8)
                );
            }
        }
        let edge = FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            0.0,
            1,
            0.0,
            29.0,
            0.0,
            0.5,
            1,
            4.0,
        )
        .unwrap();
        assert_eq!(edge.oversample_side(), 1);
    }

    #[test]
    fn t11_overlay_is_a_nonnegative_maximum() {
        let base = grade(1, 1, 2);
        let overlay = FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            0.0,
            300.0,
            [0.0, 0.0],
            1.0,
            0.0,
            2,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            4.0,
        )
        .unwrap();
        let mut changed = 0usize;
        for row in 0..24 {
            for column in 0..32 {
                let plain = base.pixel(column, row, 32, 24);
                let with_overlay = overlay.pixel(column, row, 32, 24);
                assert_eq!(plain[3], with_overlay[3]);
                for channel in 0..3 {
                    assert!(
                        with_overlay[channel] >= plain[channel],
                        "pixel {column},{row} channel {channel}: {plain:?} vs {with_overlay:?}"
                    );
                }
                if with_overlay[..3] != plain[..3] {
                    changed += 1;
                }
            }
        }
        assert!(changed > 0, "overlay did not alter any pixel");
    }

    #[test]
    fn t12_equations_change_escapes_and_power_three_stays_inside_at_zero() {
        let two = fractal_escape([0.5, 0.0], [0.0, 0.0], FractalEquation::Power2, 96);
        let three = fractal_escape([0.5, 0.0], [0.0, 0.0], FractalEquation::Power3, 96);
        assert!(two.escaped);
        assert!(three.escaped);
        assert_ne!(two.iterations, three.iterations);
        let three_zero = fractal_escape([0.0, 0.0], [0.0, 0.0], FractalEquation::Power3, 96);
        assert!(!three_zero.escaped);
    }

    #[test]
    fn t13_region_render_matches_the_same_region_of_a_4k_frame() {
        let width = 4_096usize;
        let height = 384usize;
        let region_x = 500usize;
        let region_y = 300usize;
        let region_width = 64usize;
        let region_height = 64usize;
        let grade = FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            64.0,
            [0.0, 0.0],
            0.0,
            64.0,
            [0.0, 0.0],
            0.0,
            0.0,
            1,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            2.0,
        )
        .unwrap();
        let mut full = vec![[0.0f32; 4]; width * height];
        assert!(fractal_frame_f32(&mut full, width, height, &grade));
        let mut region = vec![[0.0f32; 4]; region_width * region_height];
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert_eq!(
            apply_fractal_f32_region_controlled(
                &mut region,
                region_x,
                region_y,
                region_width,
                region_height,
                width,
                height,
                &grade,
                &mut checkpoint,
            ),
            Ok(true)
        );
        for row in 0..region_height {
            for column in 0..region_width {
                let expected = full[(region_y + row) * width + region_x + column];
                assert_eq!(
                    region[row * region_width + column],
                    expected,
                    "region pixel {column},{row}"
                );
            }
        }
    }

    #[test]
    fn t14_u8_dispatch_rejects_the_generator() {
        let mut pixels = vec![0u8; 4 * 4 * 4];
        let error = crate::adjustment::apply_render_op(
            &mut pixels,
            4,
            4,
            &EffectRenderOp::Fractal { grade: grade(1, 1, 1) },
            0,
        )
        .expect_err("u8 dispatch must reject fractal");
        match error {
            EffectExecutionError::InvalidRenderParameter { parameter, .. } => {
                assert_eq!(parameter, "requires_float32");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn t15_rendering_is_deterministic_and_checkpoints_every_4096_pixels() {
        let grade = FractalGrade::new(
            1,
            1,
            [-0.703, 0.0],
            0.0,
            64.0,
            [0.0, 0.0],
            0.0,
            64.0,
            [0.0, 0.0],
            0.0,
            0.0,
            1,
            0.0,
            29.0,
            0.0,
            0.0,
            1,
            2.0,
        )
        .unwrap();
        let width = 64usize;
        let height = 128usize;
        let mut first = vec![[0.0f32; 4]; width * height];
        let mut second = vec![[0.0f32; 4]; width * height];
        let mut checkpoints = 0usize;
        {
            let mut checkpoint = || {
                checkpoints += 1;
                Ok::<(), std::convert::Infallible>(())
            };
            apply_fractal_f32_region_controlled(
                &mut first,
                0,
                0,
                width,
                height,
                width,
                height,
                &grade,
                &mut checkpoint,
            )
            .unwrap();
        }
        assert_eq!(checkpoints, 3);
        assert!(fractal_frame_f32(&mut second, width, height, &grade));
        assert_eq!(first, second);
    }

    #[test]
    fn t16_budget_is_idempotent_and_never_overshoots() {
        for (escape, factor, edge) in [
            (1_000_000.0f32, 16.0f32, 0.0f32),
            (300.0, 16.0, 0.0),
            (300.0, 4.0, 0.0),
            (300.0, 16.0, 0.5),
            (1.0, 2.0, 0.0),
        ] {
            let budget = FractalBudget::resolve(escape, factor, edge);
            let again =
                FractalBudget::resolve(budget.iterations as f32, budget.sample_side as f32, edge);
            assert_eq!(budget, again);
            assert!(
                budget.iterations.saturating_mul(budget.sample_side * budget.sample_side) <= 8_192
            );
        }
    }

    #[test]
    fn t17_inversion_is_the_only_reciprocal_and_is_well_defined_at_zero() {
        let post = [0.25, -0.5];
        let value = [2.0, 3.0];
        let inverted = apply_inversion(value, post);
        assert_eq!(inverted[0], 2.0f32 / 13.0f32 + post[0]);
        assert_eq!(inverted[1], -(3.0f32 / 13.0f32) + post[1]);

        let near_zero = apply_inversion([1.0e-31, 0.0], [0.0, 0.0]);
        assert!((near_zero[0] - 0.1).abs() < 1.0e-7, "{near_zero:?}");
        assert_eq!(near_zero[1], 0.0);

        assert_eq!(apply_inversion([1.0, 0.0], [0.0, 0.0]), [1.0, 0.0]);

        let conjugate = apply_inversion([2.0, -3.0], [0.25, 0.5]);
        assert_eq!(conjugate[0], inverted[0]);
        assert_eq!(conjugate[1], -inverted[1]);

        let mode = grade_with_post(post);
        assert_eq!(mode.constant_and_start([2.0, 3.0]).0, inverted);
        let plain = grade_with_post([0.0, 0.0]);
        let mut differs = false;
        for row in 0..12 {
            for column in 0..12 {
                if mode.pixel(column, row, 12, 12) != plain.pixel(column, row, 12, 12) {
                    differs = true;
                }
            }
        }
        assert!(differs, "post-inversion offset did not change the image");
    }
}
