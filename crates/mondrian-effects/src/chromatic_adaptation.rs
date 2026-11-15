//! Chromatic adaptation: moves a frame from one illuminant white point to
//! another so the colours keep looking the same.
//!
//! Material is often shot under a different light than the one the timeline is
//! graded in, so a white wall reads warm or cold while everything else drifts
//! with it. The effect builds a cone-response adaptation matrix from the two
//! authored white points, applies it in a linear working space and converts back
//! to the authored primaries. Both white points can be given as a standard
//! illuminant, as a colour temperature with a green tint, or as raw chromaticity
//! coordinates, and each side has a weight so the adaptation can be dialled back.
//!
//! The transfer functions only run when the current colour space and gamma
//! control is enabled: the input is decoded into linear light first, the matrix
//! runs in that space, and the result is encoded again. With the control off the
//! frame is treated as linear Rec.709, which is what the compositor already
//! works in.
//!
//! Every channel is transformed on its own and the alpha channel is passed
//! through, so the effect is pixel local and deterministic.

use thiserror::Error;

/// The chromatic adaptation transform the grade runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaticAdaptationMethod {
    /// Bradford, the transform most grading applications default to.
    Bradford,
    /// CAT02, from the CIECAM02 colour appearance model.
    Cat02,
    /// The von Kries transform.
    VonKries,
    /// Scaling of the XYZ components themselves.
    XyzScaling,
}

impl ChromaticAdaptationMethod {
    /// The documented default transform.
    pub const DEFAULT: Self = Self::Cat02;

    /// Every transform, in menu order.
    pub const ALL: [Self; 4] = [
        Self::Bradford,
        Self::Cat02,
        Self::VonKries,
        Self::XyzScaling,
    ];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Bradford => "bradford",
            Self::Cat02 => "cat02",
            Self::VonKries => "von_kries",
            Self::XyzScaling => "xyz_scaling",
        }
    }

    /// The compact tag the render op hashes.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Bradford => 0,
            Self::Cat02 => 1,
            Self::VonKries => 2,
            Self::XyzScaling => 3,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.as_key() == key)
    }
}

/// How one side of the adaptation names its white point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IlluminantMode {
    /// A standard illuminant from the preset table.
    Preset,
    /// A colour temperature on the Planckian locus plus a green tint.
    Kelvin,
    /// Chromaticity coordinates authored directly.
    Chromatic,
}

impl IlluminantMode {
    /// The documented default mode.
    pub const DEFAULT: Self = Self::Preset;

    /// Every mode, in menu order.
    pub const ALL: [Self; 3] = [Self::Preset, Self::Kelvin, Self::Chromatic];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Preset => "preset",
            Self::Kelvin => "kelvin",
            Self::Chromatic => "chromatic",
        }
    }

    /// The compact tag the render op hashes.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Preset => 0,
            Self::Kelvin => 1,
            Self::Chromatic => 2,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }
}

/// A standard illuminant from the CIE tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardIlluminant {
    /// Horizon light, 5000 K.
    D50,
    /// Mid-morning daylight, 5500 K.
    D55,
    /// Noon daylight, 6500 K.
    D65,
    /// Late daylight, 7500 K.
    D75,
    /// Incandescent light, 2856 K.
    A,
    /// Daylight that predates the D series, 6774 K.
    C,
    /// Equal-energy white.
    E,
}

impl StandardIlluminant {
    /// The documented default illuminant.
    pub const DEFAULT: Self = Self::D65;

    /// Every illuminant, in menu order.
    pub const ALL: [Self; 7] = [
        Self::D50,
        Self::D55,
        Self::D65,
        Self::D75,
        Self::A,
        Self::C,
        Self::E,
    ];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::D50 => "d50",
            Self::D55 => "d55",
            Self::D65 => "d65",
            Self::D75 => "d75",
            Self::A => "a",
            Self::C => "c",
            Self::E => "e",
        }
    }

    /// The compact tag the render op hashes.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::D50 => 0,
            Self::D55 => 1,
            Self::D65 => 2,
            Self::D75 => 3,
            Self::A => 4,
            Self::C => 5,
            Self::E => 6,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|illuminant| illuminant.as_key() == key)
    }

    /// The CIE 1931 chromaticity of the illuminant.
    const fn chromaticity(self) -> (f32, f32) {
        match self {
            Self::D50 => (0.345_67, 0.358_50),
            Self::D55 => (0.332_42, 0.347_43),
            Self::D65 => (0.312_71, 0.329_02),
            Self::D75 => (0.299_02, 0.314_85),
            Self::A => (0.447_57, 0.407_45),
            Self::C => (0.310_06, 0.316_16),
            Self::E => (1.0 / 3.0, 1.0 / 3.0),
        }
    }
}

/// The primaries of the working frame when the colour space control is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaticAdaptationSpace {
    /// Rec.709 and sRGB primaries with the D65 white.
    Rec709,
    /// Display P3 primaries with the D65 white.
    P3D65,
    /// Rec.2020 primaries with the D65 white.
    Rec2020,
}

impl ChromaticAdaptationSpace {
    /// The documented default primaries.
    pub const DEFAULT: Self = Self::Rec709;

    /// Every space, in menu order.
    pub const ALL: [Self; 3] = [Self::Rec709, Self::P3D65, Self::Rec2020];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Rec709 => "rec709",
            Self::P3D65 => "p3d65",
            Self::Rec2020 => "rec2020",
        }
    }

    /// The compact tag the render op hashes.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Rec709 => 0,
            Self::P3D65 => 1,
            Self::Rec2020 => 2,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|space| space.as_key() == key)
    }
}

/// The transfer function the frame is decoded from and encoded back into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaticAdaptationGamma {
    /// No transfer function at all.
    Linear,
    /// The piecewise sRGB curve.
    Srgb,
    /// The piecewise Rec.709 curve.
    Rec709,
    /// A pure 2.2 power curve.
    Gamma22,
    /// A pure 2.4 power curve.
    Gamma24,
}

impl ChromaticAdaptationGamma {
    /// The documented default transfer function.
    pub const DEFAULT: Self = Self::Linear;

    /// Every transfer function, in menu order.
    pub const ALL: [Self; 5] = [
        Self::Linear,
        Self::Srgb,
        Self::Rec709,
        Self::Gamma22,
        Self::Gamma24,
    ];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Srgb => "srgb",
            Self::Rec709 => "rec709",
            Self::Gamma22 => "gamma_2_2",
            Self::Gamma24 => "gamma_2_4",
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Linear => 0,
            Self::Srgb => 1,
            Self::Rec709 => 2,
            Self::Gamma22 => 3,
            Self::Gamma24 => 4,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|gamma| gamma.as_key() == key)
    }
}

/// Documented default method of the adaptation.
pub const CHROMATIC_ADAPTATION_DEFAULT_METHOD: ChromaticAdaptationMethod =
    ChromaticAdaptationMethod::DEFAULT;
/// Documented default source illuminant mode.
pub const CHROMATIC_ADAPTATION_DEFAULT_SOURCE_MODE: IlluminantMode = IlluminantMode::DEFAULT;
/// Documented default target illuminant mode.
pub const CHROMATIC_ADAPTATION_DEFAULT_TARGET_MODE: IlluminantMode = IlluminantMode::DEFAULT;
/// Documented default source standard illuminant.
pub const CHROMATIC_ADAPTATION_DEFAULT_SOURCE_PRESET: StandardIlluminant =
    StandardIlluminant::DEFAULT;
/// Documented default target standard illuminant.
pub const CHROMATIC_ADAPTATION_DEFAULT_TARGET_PRESET: StandardIlluminant =
    StandardIlluminant::DEFAULT;
/// Smallest adaptation weight accepted for either side.
pub const CHROMATIC_ADAPTATION_MIN_WEIGHT: f32 = 0.0;
/// Largest adaptation weight accepted for either side.
pub const CHROMATIC_ADAPTATION_MAX_WEIGHT: f32 = 1.0;
/// Documented default adaptation weight of the source side.
pub const CHROMATIC_ADAPTATION_DEFAULT_SOURCE_WEIGHT: f32 = 1.0;
/// Documented default adaptation weight of the target side.
pub const CHROMATIC_ADAPTATION_DEFAULT_TARGET_WEIGHT: f32 = 1.0;
/// Smallest colour temperature accepted.
pub const CHROMATIC_ADAPTATION_MIN_KELVIN: f32 = 2_000.0;
/// Largest colour temperature accepted.
pub const CHROMATIC_ADAPTATION_MAX_KELVIN: f32 = 20_000.0;
/// Documented default colour temperature of the source side.
pub const CHROMATIC_ADAPTATION_DEFAULT_SOURCE_KELVIN: f32 = 6_500.0;
/// Documented default colour temperature of the target side.
pub const CHROMATIC_ADAPTATION_DEFAULT_TARGET_KELVIN: f32 = 6_500.0;
/// Smallest green tint accepted.
pub const CHROMATIC_ADAPTATION_MIN_TINT: f32 = -1.0;
/// Largest green tint accepted.
pub const CHROMATIC_ADAPTATION_MAX_TINT: f32 = 1.0;
/// Documented default green tint of either side.
pub const CHROMATIC_ADAPTATION_DEFAULT_TINT: f32 = 0.0;
/// Smallest chromaticity coordinate accepted.
pub const CHROMATIC_ADAPTATION_MIN_CHROMATICITY: f32 = 0.02;
/// Largest chromaticity coordinate accepted.
pub const CHROMATIC_ADAPTATION_MAX_CHROMATICITY: f32 = 0.9;
/// Documented default x chromaticity of the source side.
pub const CHROMATIC_ADAPTATION_DEFAULT_SOURCE_X: f32 = 0.312_7;
/// Documented default y chromaticity of the source side.
pub const CHROMATIC_ADAPTATION_DEFAULT_SOURCE_Y: f32 = 0.329_0;
/// Documented default x chromaticity of the target side.
pub const CHROMATIC_ADAPTATION_DEFAULT_TARGET_X: f32 = 0.312_7;
/// Documented default y chromaticity of the target side.
pub const CHROMATIC_ADAPTATION_DEFAULT_TARGET_Y: f32 = 0.329_0;
/// Documented default of the colour space toggle.
pub const CHROMATIC_ADAPTATION_DEFAULT_USE_COLOUR_SPACE: bool = true;
/// Documented default primaries.
pub const CHROMATIC_ADAPTATION_DEFAULT_SPACE: ChromaticAdaptationSpace =
    ChromaticAdaptationSpace::DEFAULT;
/// Documented default transfer function.
pub const CHROMATIC_ADAPTATION_DEFAULT_GAMMA: ChromaticAdaptationGamma =
    ChromaticAdaptationGamma::DEFAULT;

/// The D65 white the weights interpolate from, in CIE 1931 chromaticity.
const D65_X: f32 = 0.312_71;
/// The D65 white the weights interpolate from, in CIE 1931 chromaticity.
const D65_Y: f32 = 0.329_02;
/// The green shift one unit of tint applies to the y coordinate.
const TINT_STEP: f32 = 0.02;
/// The smallest cone response the adaptation divides by.
const MINIMUM_CONE: f32 = 1.0e-6;
/// The slack allowed when deciding that two white points agree.
const IDENTITY_TOLERANCE: f32 = 1.0e-6;

/// Authoring values of the chromatic adaptation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromaticAdaptationParameters {
    /// Cone response transform.
    pub method: ChromaticAdaptationMethod,
    /// Adaptation weight of the source side, 0 to 1.
    pub source_weight: f32,
    /// How the source white point is named.
    pub source_mode: IlluminantMode,
    /// Standard illuminant used when the source mode is a preset.
    pub source_preset: StandardIlluminant,
    /// Colour temperature used when the source mode is Kelvin, 2000 to 20000.
    pub source_kelvin: f32,
    /// Green tint of the source white point, -1 to 1.
    pub source_tint: f32,
    /// x chromaticity of the source white point.
    pub source_x: f32,
    /// y chromaticity of the source white point.
    pub source_y: f32,
    /// Adaptation weight of the target side, 0 to 1.
    pub target_weight: f32,
    /// How the target white point is named.
    pub target_mode: IlluminantMode,
    /// Standard illuminant used when the target mode is a preset.
    pub target_preset: StandardIlluminant,
    /// Colour temperature used when the target mode is Kelvin.
    pub target_kelvin: f32,
    /// Green tint of the target white point, -1 to 1.
    pub target_tint: f32,
    /// x chromaticity of the target white point.
    pub target_x: f32,
    /// y chromaticity of the target white point.
    pub target_y: f32,
    /// Whether the frame is decoded from, and encoded back into, the colour
    /// space and gamma below.
    pub use_colour_space: bool,
    /// Primaries of the working frame.
    pub colour_space: ChromaticAdaptationSpace,
    /// Transfer function of the working frame.
    pub gamma: ChromaticAdaptationGamma,
}

impl Default for ChromaticAdaptationParameters {
    fn default() -> Self {
        Self {
            method: CHROMATIC_ADAPTATION_DEFAULT_METHOD,
            source_weight: CHROMATIC_ADAPTATION_DEFAULT_SOURCE_WEIGHT,
            source_mode: CHROMATIC_ADAPTATION_DEFAULT_SOURCE_MODE,
            source_preset: CHROMATIC_ADAPTATION_DEFAULT_SOURCE_PRESET,
            source_kelvin: CHROMATIC_ADAPTATION_DEFAULT_SOURCE_KELVIN,
            source_tint: CHROMATIC_ADAPTATION_DEFAULT_TINT,
            source_x: CHROMATIC_ADAPTATION_DEFAULT_SOURCE_X,
            source_y: CHROMATIC_ADAPTATION_DEFAULT_SOURCE_Y,
            target_weight: CHROMATIC_ADAPTATION_DEFAULT_TARGET_WEIGHT,
            target_mode: CHROMATIC_ADAPTATION_DEFAULT_TARGET_MODE,
            target_preset: CHROMATIC_ADAPTATION_DEFAULT_TARGET_PRESET,
            target_kelvin: CHROMATIC_ADAPTATION_DEFAULT_TARGET_KELVIN,
            target_tint: CHROMATIC_ADAPTATION_DEFAULT_TINT,
            target_x: CHROMATIC_ADAPTATION_DEFAULT_TARGET_X,
            target_y: CHROMATIC_ADAPTATION_DEFAULT_TARGET_Y,
            use_colour_space: CHROMATIC_ADAPTATION_DEFAULT_USE_COLOUR_SPACE,
            colour_space: CHROMATIC_ADAPTATION_DEFAULT_SPACE,
            gamma: CHROMATIC_ADAPTATION_DEFAULT_GAMMA,
        }
    }
}

/// A parameter value that fell outside its documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("chromatic_adaptation.{parameter} must be finite and within its documented range")]
pub struct ChromaticAdaptationError {
    parameter: &'static str,
}

impl ChromaticAdaptationError {
    /// Name of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// A row-major 3x3 matrix.
pub type Matrix3 = [[f32; 3]; 3];

/// Validated chromatic adaptation configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromaticAdaptationGrade {
    parameters: ChromaticAdaptationParameters,
    matrix: Matrix3,
}

impl ChromaticAdaptationGrade {
    /// Validates the documented ranges and builds the adaptation matrix.
    pub fn new(
        parameters: ChromaticAdaptationParameters,
    ) -> Result<Self, ChromaticAdaptationError> {
        let finite = |parameter: &'static str, value: f32, min: f32, max: f32| {
            if value.is_finite() && value >= min && value <= max {
                Ok(())
            } else {
                Err(ChromaticAdaptationError { parameter })
            }
        };
        finite(
            "source_weight",
            parameters.source_weight,
            CHROMATIC_ADAPTATION_MIN_WEIGHT,
            CHROMATIC_ADAPTATION_MAX_WEIGHT,
        )?;
        finite(
            "target_weight",
            parameters.target_weight,
            CHROMATIC_ADAPTATION_MIN_WEIGHT,
            CHROMATIC_ADAPTATION_MAX_WEIGHT,
        )?;
        finite(
            "source_kelvin",
            parameters.source_kelvin,
            CHROMATIC_ADAPTATION_MIN_KELVIN,
            CHROMATIC_ADAPTATION_MAX_KELVIN,
        )?;
        finite(
            "target_kelvin",
            parameters.target_kelvin,
            CHROMATIC_ADAPTATION_MIN_KELVIN,
            CHROMATIC_ADAPTATION_MAX_KELVIN,
        )?;
        finite(
            "source_tint",
            parameters.source_tint,
            CHROMATIC_ADAPTATION_MIN_TINT,
            CHROMATIC_ADAPTATION_MAX_TINT,
        )?;
        finite(
            "target_tint",
            parameters.target_tint,
            CHROMATIC_ADAPTATION_MIN_TINT,
            CHROMATIC_ADAPTATION_MAX_TINT,
        )?;
        finite(
            "source_x",
            parameters.source_x,
            CHROMATIC_ADAPTATION_MIN_CHROMATICITY,
            CHROMATIC_ADAPTATION_MAX_CHROMATICITY,
        )?;
        finite(
            "source_y",
            parameters.source_y,
            CHROMATIC_ADAPTATION_MIN_CHROMATICITY,
            CHROMATIC_ADAPTATION_MAX_CHROMATICITY,
        )?;
        finite(
            "target_x",
            parameters.target_x,
            CHROMATIC_ADAPTATION_MIN_CHROMATICITY,
            CHROMATIC_ADAPTATION_MAX_CHROMATICITY,
        )?;
        finite(
            "target_y",
            parameters.target_y,
            CHROMATIC_ADAPTATION_MIN_CHROMATICITY,
            CHROMATIC_ADAPTATION_MAX_CHROMATICITY,
        )?;

        let source = effective_white(
            parameters.source_mode,
            parameters.source_preset,
            parameters.source_kelvin,
            parameters.source_tint,
            parameters.source_x,
            parameters.source_y,
            parameters.source_weight,
        );
        let target = effective_white(
            parameters.target_mode,
            parameters.target_preset,
            parameters.target_kelvin,
            parameters.target_tint,
            parameters.target_x,
            parameters.target_y,
            parameters.target_weight,
        );
        let space = effective_space(&parameters);
        let cat = cat_matrix(parameters.method, white_xyz(source), white_xyz(target));
        let matrix = matrix3_mul(
            matrix3_mul(space_xyz_to_rgb(space), cat),
            space_rgb_to_xyz(space),
        );
        Ok(Self { parameters, matrix })
    }

    /// The documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(ChromaticAdaptationParameters::default()).expect("documented defaults are valid")
    }

    /// Borrows the validated parameters.
    pub const fn parameters(&self) -> &ChromaticAdaptationParameters {
        &self.parameters
    }

    /// The transform the grade runs.
    pub const fn method(&self) -> ChromaticAdaptationMethod {
        self.parameters.method
    }

    /// The effective primaries, with the disabled control folded to Rec.709.
    pub fn space(&self) -> ChromaticAdaptationSpace {
        effective_space(&self.parameters)
    }

    /// The effective transfer function, with the disabled control folded to
    /// linear.
    pub fn gamma(&self) -> ChromaticAdaptationGamma {
        if self.parameters.use_colour_space {
            self.parameters.gamma
        } else {
            ChromaticAdaptationGamma::Linear
        }
    }

    /// The complete adaptation matrix, primaries included.
    pub const fn matrix(&self) -> Matrix3 {
        self.matrix
    }

    /// True when the configuration leaves every channel unchanged.
    ///
    /// The documented defaults are already an identity because both sides name
    /// D65 and the frame is treated as linear Rec.709, so the builder only
    /// appends a node once the author actually moves a white point.
    pub fn is_identity(&self) -> bool {
        if self.gamma() != ChromaticAdaptationGamma::Linear {
            return false;
        }
        let parameters = &self.parameters;
        let source = effective_white(
            parameters.source_mode,
            parameters.source_preset,
            parameters.source_kelvin,
            parameters.source_tint,
            parameters.source_x,
            parameters.source_y,
            parameters.source_weight,
        );
        let target = effective_white(
            parameters.target_mode,
            parameters.target_preset,
            parameters.target_kelvin,
            parameters.target_tint,
            parameters.target_x,
            parameters.target_y,
            parameters.target_weight,
        );
        (source.0 - target.0).abs() <= IDENTITY_TOLERANCE
            && (source.1 - target.1).abs() <= IDENTITY_TOLERANCE
    }

    /// Authoring cost of the effect.
    pub const fn cost(&self) -> u8 {
        2
    }

    /// Packs the uniform cells for the fused point shader.
    ///
    /// `params` carries the first matrix row and the transfer function tag,
    /// `color` the second row and `extra0` the third row.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        let matrix = self.matrix;
        (
            [
                matrix[0][0],
                matrix[0][1],
                matrix[0][2],
                f32::from(self.gamma().as_u8()),
            ],
            [matrix[1][0], matrix[1][1], matrix[1][2], 0.0],
            [matrix[2][0], matrix[2][1], matrix[2][2], 0.0],
        )
    }

    /// Hashes the full authored identity.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        let parameters = &self.parameters;
        parameters.method.as_u8().hash(state);
        parameters.source_mode.as_u8().hash(state);
        parameters.source_preset.as_u8().hash(state);
        parameters.target_mode.as_u8().hash(state);
        parameters.target_preset.as_u8().hash(state);
        parameters.colour_space.as_u8().hash(state);
        parameters.gamma.as_u8().hash(state);
        parameters.use_colour_space.hash(state);
        for value in [
            parameters.source_weight,
            parameters.source_kelvin,
            parameters.source_tint,
            parameters.source_x,
            parameters.source_y,
            parameters.target_weight,
            parameters.target_kelvin,
            parameters.target_tint,
            parameters.target_x,
            parameters.target_y,
        ] {
            value.to_bits().hash(state);
        }
    }
}

/// The primaries the grade actually uses.
fn effective_space(parameters: &ChromaticAdaptationParameters) -> ChromaticAdaptationSpace {
    if parameters.use_colour_space {
        parameters.colour_space
    } else {
        ChromaticAdaptationSpace::Rec709
    }
}

/// The chromaticity of one side, after the weight has been applied.
fn effective_white(
    mode: IlluminantMode,
    preset: StandardIlluminant,
    kelvin: f32,
    tint: f32,
    x: f32,
    y: f32,
    weight: f32,
) -> (f32, f32) {
    let (mut x, mut y) = match mode {
        IlluminantMode::Preset => preset.chromaticity(),
        IlluminantMode::Kelvin => {
            let (x, y) = planckian_chromaticity(kelvin);
            (x, y + TINT_STEP * tint)
        }
        IlluminantMode::Chromatic => (x, y),
    };
    x = x.clamp(0.005, 0.995);
    y = y.clamp(0.01, 0.99);
    (D65_X + (x - D65_X) * weight, D65_Y + (y - D65_Y) * weight)
}

/// The Planckian locus chromaticity of a colour temperature.
///
/// The cubic fits are the CIE approximations and stay finite across the
/// documented range, which keeps the CPU and the shader on the same numbers.
fn planckian_chromaticity(kelvin: f32) -> (f32, f32) {
    let temperature = kelvin.clamp(
        CHROMATIC_ADAPTATION_MIN_KELVIN,
        CHROMATIC_ADAPTATION_MAX_KELVIN,
    );
    let cube = temperature * temperature * temperature;
    let square = temperature * temperature;
    let x = if temperature <= 4_000.0 {
        -0.266_123_9e9 / cube - 0.234_358_9e6 / square + 0.877_695_6e3 / temperature + 0.179_910
    } else {
        -3.0258468e+09 / cube + 2107038.0 / square + 222.6347 / temperature + 0.24039
    };
    let x2 = x * x;
    let x3 = x2 * x;
    let y = if temperature <= 2_222.0 {
        -1.1063814 * x3 - 1.3481102 * x2 + 2.1855583 * x - 0.20219684
    } else if temperature <= 4_000.0 {
        -0.9549476 * x3 - 1.3741859 * x2 + 2.09137 * x - 0.16748866
    } else {
        3.081758 * x3 - 5.873387 * x2 + 3.7511299 * x - 0.37001482
    };
    (x, y)
}

/// Converts a chromaticity pair into tristimulus values with `Y = 1`.
fn white_xyz(chromaticity: (f32, f32)) -> [f32; 3] {
    let (x, y) = chromaticity;
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// The cone response matrix and its inverse for one transform.
fn cone_matrix(method: ChromaticAdaptationMethod) -> (Matrix3, Matrix3) {
    match method {
        ChromaticAdaptationMethod::Bradford => (
            [
                [0.895_1, 0.266_4, -0.161_4],
                [-0.750_2, 1.713_5, 0.036_7],
                [0.038_9, -0.068_5, 1.029_6],
            ],
            [
                [0.986_992_9, -0.147_054_3, 0.159_962_7],
                [0.432_305_3, 0.518_360_3, 0.049_291_2],
                [-0.008_528_7, 0.040_042_8, 0.968_486_7],
            ],
        ),
        ChromaticAdaptationMethod::Cat02 => (
            [
                [0.732_8, 0.429_6, -0.162_4],
                [-0.703_6, 1.697_5, 0.006_1],
                [0.003_0, 0.013_6, 0.983_4],
            ],
            [
                [1.096_124, -0.278_869, 0.182_745],
                [0.454_369, 0.473_533, 0.072_098],
                [-0.009_628, -0.005_698, 1.015_326],
            ],
        ),
        ChromaticAdaptationMethod::VonKries => (
            [
                [0.400_24, 0.707_6, -0.080_81],
                [-0.226_3, 1.165_32, 0.045_7],
                [0.0, 0.0, 0.918_22],
            ],
            [
                [1.859_936_4, -1.129_381_6, 0.219_897_4],
                [0.361_191_4, 0.638_812_5, -0.000_006_4],
                [0.0, 0.0, 1.089_063_6],
            ],
        ),
        ChromaticAdaptationMethod::XyzScaling => (
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        ),
    }
}

/// Builds the adaptation matrix from the source and target whites.
fn cat_matrix(method: ChromaticAdaptationMethod, source: [f32; 3], target: [f32; 3]) -> Matrix3 {
    let (cone, inverse) = cone_matrix(method);
    let source = matrix3_vector(cone, source);
    let target = matrix3_vector(cone, target);
    let mut scale = [[0.0_f32; 3]; 3];
    for (index, cell) in scale.iter_mut().enumerate() {
        let ratio = if source[index].abs() <= MINIMUM_CONE {
            1.0
        } else {
            target[index] / source[index]
        };
        cell[index] = ratio;
    }
    matrix3_mul(matrix3_mul(inverse, scale), cone)
}

/// The RGB to XYZ matrix of a colour space, with a D65 white.
fn space_rgb_to_xyz(space: ChromaticAdaptationSpace) -> Matrix3 {
    match space {
        ChromaticAdaptationSpace::Rec709 => [
            [0.412_390_8, 0.357_584_3, 0.180_480_8],
            [0.212_639, 0.715_168_7, 0.072_192_3],
            [0.019_330_8, 0.119_194_8, 0.950_532_2],
        ],
        ChromaticAdaptationSpace::P3D65 => [
            [0.486_570_9, 0.265_667_7, 0.198_217_3],
            [0.228_974_6, 0.691_738_5, 0.079_286_9],
            [0.0, 0.045_113_4, 1.043_944_4],
        ],
        ChromaticAdaptationSpace::Rec2020 => [
            [0.636_958, 0.144_616_9, 0.168_881],
            [0.262_700_2, 0.677_998_1, 0.059_301_7],
            [0.0, 0.028_072_7, 1.060_985_1],
        ],
    }
}

/// The XYZ to RGB matrix of a colour space, with a D65 white.
fn space_xyz_to_rgb(space: ChromaticAdaptationSpace) -> Matrix3 {
    match space {
        ChromaticAdaptationSpace::Rec709 => [
            [3.24097, -1.5373832, -0.4986108],
            [-0.969_243_6, 1.875_967_5, 0.041_555_1],
            [0.055_630_1, -0.203_977, 1.056_971_5],
        ],
        ChromaticAdaptationSpace::P3D65 => [
            [2.493497, -0.9313836, -0.4027108],
            [-0.829_489, 1.762_664_1, 0.023_624_7],
            [0.035_845_8, -0.076_172_4, 0.956_884_5],
        ],
        ChromaticAdaptationSpace::Rec2020 => [
            [1.716_651_2, -0.355_670_8, -0.253_366_3],
            [-0.666_684_4, 1.616_481_2, 0.015_768_5],
            [0.017_639_9, -0.042_770_6, 0.942_103_1],
        ],
    }
}

/// Multiplies two row-major matrices.
fn matrix3_mul(left: Matrix3, right: Matrix3) -> Matrix3 {
    let mut product = [[0.0_f32; 3]; 3];
    for (row, cells) in product.iter_mut().enumerate() {
        for (column, cell) in cells.iter_mut().enumerate() {
            *cell = left[row][0] * right[0][column]
                + left[row][1] * right[1][column]
                + left[row][2] * right[2][column];
        }
    }
    product
}

/// Multiplies a matrix by a column vector.
fn matrix3_vector(matrix: Matrix3, vector: [f32; 3]) -> [f32; 3] {
    let mut product = [0.0_f32; 3];
    for (row, cell) in product.iter_mut().enumerate() {
        *cell =
            matrix[row][0] * vector[0] + matrix[row][1] * vector[1] + matrix[row][2] * vector[2];
    }
    product
}

/// A signed power so negative scene values stay finite.
fn signed_pow(value: f32, exponent: f32) -> f32 {
    if value < 0.0 {
        -(-value).powf(exponent)
    } else {
        value.powf(exponent)
    }
}

/// Decodes one channel from the authored transfer function into linear light.
fn decode_transfer(value: f32, gamma: ChromaticAdaptationGamma) -> f32 {
    match gamma {
        ChromaticAdaptationGamma::Linear => value,
        ChromaticAdaptationGamma::Srgb => {
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        ChromaticAdaptationGamma::Rec709 => {
            if value < 0.081 {
                value / 4.5
            } else {
                ((value + 0.099) / 1.099).powf(1.0 / 0.45)
            }
        }
        ChromaticAdaptationGamma::Gamma22 => signed_pow(value, 2.2),
        ChromaticAdaptationGamma::Gamma24 => signed_pow(value, 2.4),
    }
}

/// Encodes one linear channel back into the authored transfer function.
fn encode_transfer(value: f32, gamma: ChromaticAdaptationGamma) -> f32 {
    match gamma {
        ChromaticAdaptationGamma::Linear => value,
        ChromaticAdaptationGamma::Srgb => {
            if value <= 0.003_130_8 {
                value * 12.92
            } else {
                1.055 * value.powf(1.0 / 2.4) - 0.055
            }
        }
        ChromaticAdaptationGamma::Rec709 => {
            if value < 0.018 {
                value * 4.5
            } else {
                1.099 * value.powf(0.45) - 0.099
            }
        }
        ChromaticAdaptationGamma::Gamma22 => signed_pow(value, 1.0 / 2.2),
        ChromaticAdaptationGamma::Gamma24 => signed_pow(value, 1.0 / 2.4),
    }
}

/// Applies the adaptation to a straight-alpha scene-linear frame.
pub fn chromatic_adaptation_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ChromaticAdaptationGrade,
) -> bool {
    apply_chromatic_adaptation_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Applies the adaptation, reporting cancellation through `checkpoint`.
pub(crate) fn apply_chromatic_adaptation_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ChromaticAdaptationGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    checkpoint()?;

    let matrix = grade.matrix;
    let gamma = grade.gamma();
    for chunk in working[..pixel_count].chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            let decoded = [
                decode_transfer(pixel[0], gamma),
                decode_transfer(pixel[1], gamma),
                decode_transfer(pixel[2], gamma),
            ];
            let adapted = matrix3_vector(matrix, decoded);
            for (channel, value) in pixel.iter_mut().take(3).zip(adapted) {
                *channel = encode_transfer(value, gamma);
            }
        }
    }
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(parameters: ChromaticAdaptationParameters) -> ChromaticAdaptationGrade {
        ChromaticAdaptationGrade::new(parameters).expect("valid grade")
    }

    fn flat(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    fn frame(colour: [f32; 4]) -> Vec<[f32; 4]> {
        flat(3, 2, colour)
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        let base = ChromaticAdaptationParameters::default();
        let cases = [
            (
                "source_weight",
                ChromaticAdaptationParameters { source_weight: 1.5, ..base },
            ),
            (
                "target_weight",
                ChromaticAdaptationParameters { target_weight: -0.1, ..base },
            ),
            (
                "source_kelvin",
                ChromaticAdaptationParameters { source_kelvin: 1_000.0, ..base },
            ),
            (
                "target_kelvin",
                ChromaticAdaptationParameters { target_kelvin: 25_000.0, ..base },
            ),
            (
                "source_tint",
                ChromaticAdaptationParameters { source_tint: 2.0, ..base },
            ),
            (
                "target_tint",
                ChromaticAdaptationParameters { target_tint: -2.0, ..base },
            ),
            (
                "source_x",
                ChromaticAdaptationParameters { source_x: 0.0, ..base },
            ),
            (
                "source_y",
                ChromaticAdaptationParameters { source_y: 0.95, ..base },
            ),
            (
                "target_x",
                ChromaticAdaptationParameters { target_x: 0.95, ..base },
            ),
            (
                "target_y",
                ChromaticAdaptationParameters { target_y: 0.0, ..base },
            ),
        ];
        for (expected, parameters) in cases {
            let error = ChromaticAdaptationGrade::new(parameters).expect_err("out of range");
            assert_eq!(error.parameter(), expected);
        }
        let not_finite = ChromaticAdaptationParameters { source_kelvin: f32::NAN, ..base };
        assert_eq!(
            ChromaticAdaptationGrade::new(not_finite).expect_err("not finite").parameter(),
            "source_kelvin"
        );
    }

    #[test]
    fn documented_defaults_are_stable() {
        let grade = ChromaticAdaptationGrade::documented_defaults();
        let parameters = grade.parameters();
        assert_eq!(parameters.method, ChromaticAdaptationMethod::Cat02);
        assert_eq!(parameters.source_mode, IlluminantMode::Preset);
        assert_eq!(parameters.target_mode, IlluminantMode::Preset);
        assert_eq!(parameters.source_preset, StandardIlluminant::D65);
        assert_eq!(parameters.target_preset, StandardIlluminant::D65);
        assert_eq!(parameters.source_weight, 1.0);
        assert_eq!(parameters.target_weight, 1.0);
        assert_eq!(parameters.source_kelvin, 6_500.0);
        assert_eq!(parameters.source_tint, 0.0);
        assert_eq!(parameters.source_x, 0.312_7);
        assert_eq!(parameters.source_y, 0.329);
        assert!(parameters.use_colour_space);
        assert_eq!(grade.space(), ChromaticAdaptationSpace::Rec709);
        assert_eq!(grade.gamma(), ChromaticAdaptationGamma::Linear);
        assert!(grade.is_identity());
        assert_eq!(grade.cost(), 2);
    }

    #[test]
    fn menu_keys_round_trip() {
        for method in ChromaticAdaptationMethod::ALL {
            assert_eq!(
                ChromaticAdaptationMethod::from_key(method.as_key()),
                Some(method)
            );
        }
        for mode in IlluminantMode::ALL {
            assert_eq!(IlluminantMode::from_key(mode.as_key()), Some(mode));
        }
        for preset in StandardIlluminant::ALL {
            assert_eq!(StandardIlluminant::from_key(preset.as_key()), Some(preset));
        }
        for space in ChromaticAdaptationSpace::ALL {
            assert_eq!(
                ChromaticAdaptationSpace::from_key(space.as_key()),
                Some(space)
            );
        }
        for gamma in ChromaticAdaptationGamma::ALL {
            assert_eq!(
                ChromaticAdaptationGamma::from_key(gamma.as_key()),
                Some(gamma)
            );
        }
        assert_eq!(ChromaticAdaptationMethod::from_key("bogus"), None);
    }

    #[test]
    fn uniform_cells_carry_the_matrix_and_the_transfer_tag() {
        let defaults = ChromaticAdaptationGrade::documented_defaults();
        let (params, row, extra) = defaults.gpu_controls();
        let identity = [[1.0_f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let packed = [
            [params[0], params[1], params[2]],
            [row[0], row[1], row[2]],
            [extra[0], extra[1], extra[2]],
        ];
        for (packed_row, identity_row) in packed.iter().zip(identity.iter()) {
            for (cell, expected) in packed_row.iter().zip(identity_row.iter()) {
                assert!((cell - expected).abs() <= 2.0e-6);
            }
        }
        assert_eq!(params[3], 0.0);

        let srgb = grade(ChromaticAdaptationParameters {
            use_colour_space: true,
            gamma: ChromaticAdaptationGamma::Srgb,
            ..ChromaticAdaptationParameters::default()
        });
        assert_eq!(srgb.gpu_controls().0[3], 1.0);
    }

    #[test]
    fn a_disabled_colour_space_control_folds_to_rec709_linear() {
        let disabled = grade(ChromaticAdaptationParameters {
            use_colour_space: false,
            colour_space: ChromaticAdaptationSpace::P3D65,
            gamma: ChromaticAdaptationGamma::Gamma24,
            ..ChromaticAdaptationParameters::default()
        });
        assert_eq!(disabled.space(), ChromaticAdaptationSpace::Rec709);
        assert_eq!(disabled.gamma(), ChromaticAdaptationGamma::Linear);
        assert!(disabled.is_identity());
    }

    #[test]
    fn adapting_between_the_same_whites_leaves_the_frame_alone() {
        let defaults = ChromaticAdaptationGrade::documented_defaults();
        let mut pixels = frame([0.2, 0.5, 0.8, 1.0]);
        let expected = pixels.clone();
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 3, 2, &defaults));
        for (actual, expected) in pixels.iter().zip(&expected) {
            for channel in 0..4 {
                assert!((actual[channel] - expected[channel]).abs() <= 1.0e-5);
            }
        }
    }

    #[test]
    fn a_weight_of_zero_removes_the_adaptation() {
        let zeroed = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::D50,
            target_preset: StandardIlluminant::D65,
            source_weight: 0.0,
            ..ChromaticAdaptationParameters::default()
        });
        assert!(zeroed.is_identity());
        let mut pixels = flat(2, 2, [0.1, 0.4, 0.7, 1.0]);
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 2, 2, &zeroed));
        for pixel in &pixels {
            assert!((pixel[0] - 0.1).abs() <= 1.0e-5);
            assert!((pixel[1] - 0.4).abs() <= 1.0e-5);
            assert!((pixel[2] - 0.7).abs() <= 1.0e-5);
        }
    }

    #[test]
    fn a_neutral_cast_under_the_source_light_comes_out_neutral() {
        let adapted = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::D50,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        assert!(!adapted.is_identity());
        // The white of the source light, expressed in the working primaries.
        let source_white = matrix3_vector(
            space_xyz_to_rgb(ChromaticAdaptationSpace::Rec709),
            white_xyz(StandardIlluminant::D50.chromaticity()),
        );
        let mut pixels = frame([source_white[0], source_white[1], source_white[2], 1.0]);
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 3, 2, &adapted));
        for pixel in &pixels {
            for channel in 0..3 {
                assert!((pixel[channel] - 1.0).abs() <= 2.0e-4, "{pixel:?}");
            }
        }
    }

    #[test]
    fn adapting_from_d50_to_d65_cools_a_neutral_rec709_white() {
        let adapted = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::D50,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        let mut pixels = flat(2, 2, [1.0, 1.0, 1.0, 1.0]);
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 2, 2, &adapted));
        for pixel in &pixels {
            // A white that is already D65 reads too warm for D50 light, so it
            // gains blue and loses red.
            assert!(pixel[2] > 1.0);
            assert!(pixel[0] < 1.0);
            assert!(pixel[..3].iter().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn adaptation_is_reversible() {
        let forward = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::D50,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        let back = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::D65,
            target_preset: StandardIlluminant::D50,
            ..ChromaticAdaptationParameters::default()
        });
        let original = [0.15, 0.45, 0.75, 1.0];
        let mut pixels = flat(2, 2, original);
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 2, 2, &forward));
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 2, 2, &back));
        for pixel in &pixels {
            for channel in 0..3 {
                assert!((pixel[channel] - original[channel]).abs() <= 1.0e-4);
            }
        }
    }

    #[test]
    fn the_transform_method_changes_the_matrix() {
        let bradford = grade(ChromaticAdaptationParameters {
            method: ChromaticAdaptationMethod::Bradford,
            source_preset: StandardIlluminant::A,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        let cat02 = grade(ChromaticAdaptationParameters {
            method: ChromaticAdaptationMethod::Cat02,
            source_preset: StandardIlluminant::A,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        assert!((bradford.matrix()[0][0] - cat02.matrix()[0][0]).abs() > 1.0e-3);
    }

    #[test]
    fn the_kelvin_mode_and_its_tint_move_the_white_point() {
        let warm = grade(ChromaticAdaptationParameters {
            source_mode: IlluminantMode::Kelvin,
            source_kelvin: 4_000.0,
            source_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        let neutral = grade(ChromaticAdaptationParameters {
            source_mode: IlluminantMode::Kelvin,
            source_kelvin: 6_500.0,
            ..ChromaticAdaptationParameters::default()
        });
        assert!(!warm.is_identity());
        assert!(!neutral.is_identity());
        assert!((warm.matrix()[0][0] - neutral.matrix()[0][0]).abs() > 1.0e-3);

        let tinted = grade(ChromaticAdaptationParameters {
            source_mode: IlluminantMode::Kelvin,
            source_kelvin: 6_500.0,
            source_tint: 0.5,
            ..ChromaticAdaptationParameters::default()
        });
        assert!(!tinted.is_identity());
        assert!((tinted.matrix()[1][1] - neutral.matrix()[1][1]).abs() > 1.0e-3);
    }

    #[test]
    fn the_chromatic_mode_uses_the_authored_coordinates() {
        let authored = grade(ChromaticAdaptationParameters {
            source_mode: IlluminantMode::Chromatic,
            source_x: 0.4,
            source_y: 0.4,
            ..ChromaticAdaptationParameters::default()
        });
        assert!(!authored.is_identity());
        let mut pixels = frame([0.3, 0.4, 0.6, 1.0]);
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 3, 2, &authored));
        for pixel in &pixels {
            assert!(pixel[..3].iter().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn a_partial_weight_lands_between_no_adaptation_and_full_adaptation() {
        let full = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::A,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        let half = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::A,
            target_preset: StandardIlluminant::D65,
            source_weight: 0.5,
            ..ChromaticAdaptationParameters::default()
        });
        let untouched = [0.9, 0.5, 0.2, 1.0];
        let mut full_pixels = flat(2, 2, untouched);
        let mut half_pixels = flat(2, 2, untouched);
        assert!(chromatic_adaptation_frame_f32(
            &mut full_pixels,
            2,
            2,
            &full
        ));
        assert!(chromatic_adaptation_frame_f32(
            &mut half_pixels,
            2,
            2,
            &half
        ));
        for channel in 0..3 {
            let shifted = (full_pixels[0][channel] - untouched[channel]).abs();
            let between = (half_pixels[0][channel] - untouched[channel]).abs();
            assert!(between < shifted);
            assert!(between > 0.0);
        }
    }

    #[test]
    fn the_transfer_function_round_trips_when_the_whites_agree() {
        for gamma in [
            ChromaticAdaptationGamma::Srgb,
            ChromaticAdaptationGamma::Rec709,
            ChromaticAdaptationGamma::Gamma22,
            ChromaticAdaptationGamma::Gamma24,
        ] {
            let grade = grade(ChromaticAdaptationParameters {
                gamma,
                ..ChromaticAdaptationParameters::default()
            });
            let mut pixels = flat(2, 2, [0.05, 0.25, 0.6, 1.0]);
            assert!(chromatic_adaptation_frame_f32(&mut pixels, 2, 2, &grade));
            for pixel in &pixels {
                assert!((pixel[0] - 0.05).abs() <= 2.0e-4, "{gamma:?} {pixel:?}");
                assert!((pixel[1] - 0.25).abs() <= 2.0e-4, "{gamma:?} {pixel:?}");
                assert!((pixel[2] - 0.6).abs() <= 2.0e-4, "{gamma:?} {pixel:?}");
            }
        }
    }

    #[test]
    fn the_colour_space_changes_an_adaptation_but_not_an_identity() {
        let same_whites = grade(ChromaticAdaptationParameters {
            colour_space: ChromaticAdaptationSpace::P3D65,
            ..ChromaticAdaptationParameters::default()
        });
        assert!(same_whites.is_identity());

        let rec709 = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::D50,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        for space in [
            ChromaticAdaptationSpace::P3D65,
            ChromaticAdaptationSpace::Rec2020,
        ] {
            let other = grade(ChromaticAdaptationParameters {
                colour_space: space,
                source_preset: StandardIlluminant::D50,
                target_preset: StandardIlluminant::D65,
                ..ChromaticAdaptationParameters::default()
            });
            assert!(
                (other.matrix()[0][0] - rec709.matrix()[0][0]).abs() > 1.0e-3,
                "{space:?}"
            );
            let mut pixels = flat(2, 2, [0.0, 0.0, 1.0, 1.0]);
            assert!(chromatic_adaptation_frame_f32(&mut pixels, 2, 2, &other));
            for pixel in &pixels {
                assert!(pixel[..3].iter().all(|value| value.is_finite()));
            }
        }
    }

    #[test]
    fn alpha_is_passed_through_and_fully_transparent_pixels_still_adapt() {
        let adapted = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::D50,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        let mut pixels = flat(2, 1, [0.2, 0.4, 0.6, 0.0]);
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 2, 1, &adapted));
        for pixel in &pixels {
            assert_eq!(pixel[3], 0.0);
            assert!(pixel[0].is_finite() && pixel[1].is_finite() && pixel[2].is_finite());
        }
    }

    #[test]
    fn negative_scene_values_stay_finite() {
        let adapted = grade(ChromaticAdaptationParameters {
            gamma: ChromaticAdaptationGamma::Gamma22,
            source_preset: StandardIlluminant::D50,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        let mut pixels = flat(2, 1, [-0.1, 0.3, 0.5, 1.0]);
        assert!(chromatic_adaptation_frame_f32(&mut pixels, 2, 1, &adapted));
        for pixel in &pixels {
            assert!(pixel[0].is_finite() && pixel[1].is_finite() && pixel[2].is_finite());
        }
    }

    #[test]
    fn short_and_empty_frames_are_rejected() {
        let defaults = ChromaticAdaptationGrade::documented_defaults();
        let mut short = vec![[0.0_f32; 4]; 3];
        assert!(!chromatic_adaptation_frame_f32(&mut short, 2, 2, &defaults));
        let mut empty = Vec::new();
        assert!(!chromatic_adaptation_frame_f32(&mut empty, 0, 4, &defaults));
        assert!(!chromatic_adaptation_frame_f32(&mut empty, 4, 0, &defaults));
    }

    #[test]
    fn the_frame_conversion_is_deterministic() {
        let adapted = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::A,
            target_preset: StandardIlluminant::D55,
            ..ChromaticAdaptationParameters::default()
        });
        let mut first = flat(5, 3, [0.3, 0.55, 0.1, 0.8]);
        let mut second = first.clone();
        assert!(chromatic_adaptation_frame_f32(&mut first, 5, 3, &adapted));
        assert!(chromatic_adaptation_frame_f32(&mut second, 5, 3, &adapted));
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_stops_the_conversion() {
        let adapted = grade(ChromaticAdaptationParameters {
            source_preset: StandardIlluminant::D50,
            target_preset: StandardIlluminant::D65,
            ..ChromaticAdaptationParameters::default()
        });
        let mut pixels = flat(8, 8, [0.2, 0.3, 0.4, 1.0]);
        let mut calls = 0;
        let result =
            apply_chromatic_adaptation_f32_controlled(&mut pixels, 8, 8, &adapted, &mut || {
                calls += 1;
                if calls >= 2 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            });
        assert_eq!(result, Err("cancelled"));
    }

    #[test]
    fn the_hash_separates_methods_weights_and_spaces() {
        let base = ChromaticAdaptationGrade::documented_defaults();
        let variants = [
            grade(ChromaticAdaptationParameters {
                method: ChromaticAdaptationMethod::Bradford,
                ..ChromaticAdaptationParameters::default()
            }),
            grade(ChromaticAdaptationParameters {
                source_weight: 0.5,
                source_preset: StandardIlluminant::D50,
                ..ChromaticAdaptationParameters::default()
            }),
            grade(ChromaticAdaptationParameters {
                colour_space: ChromaticAdaptationSpace::P3D65,
                ..ChromaticAdaptationParameters::default()
            }),
            grade(ChromaticAdaptationParameters {
                gamma: ChromaticAdaptationGamma::Srgb,
                ..ChromaticAdaptationParameters::default()
            }),
        ];
        let hash = |grade: &ChromaticAdaptationGrade| {
            use std::hash::Hasher;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            grade.hash_state(&mut hasher);
            hasher.finish()
        };
        let base_hash = hash(&base);
        for variant in &variants {
            assert_ne!(hash(variant), base_hash);
        }
        assert_eq!(
            hash(&base),
            hash(&ChromaticAdaptationGrade::documented_defaults())
        );
    }
}
