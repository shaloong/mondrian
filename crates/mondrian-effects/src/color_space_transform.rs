//! Colour space transform: decode an input transfer function, rotate the
//! primaries, optionally tone and gamut map, then encode the output transfer
//! function.
//!
//! The effect is pixel local and deterministic: every pixel runs the same
//! composed 3x3 matrix, so it lowers to the fused point pass and keeps its CPU
//! kernel as the reference implementation.

use thiserror::Error;

/// Primaries the frame is interpreted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpaceTransformSpace {
    /// Rec.709 primaries with the D65 white.
    Rec709,
    /// sRGB primaries with the D65 white.
    Srgb,
    /// Display P3 primaries with the D65 white.
    P3D65,
    /// Display P3 primaries with the DCI white.
    P3Dci,
    /// Rec.2020 primaries with the D65 white.
    Rec2020,
    /// ACES AP1 primaries with the ACES white.
    AcesAp1,
    /// ACES AP0 primaries with the ACES white.
    AcesAp0,
}

impl ColorSpaceTransformSpace {
    /// The documented default primaries.
    pub const DEFAULT: Self = Self::Rec709;

    /// Every primaries set, in menu order.
    pub const ALL: [Self; 7] = [
        Self::Rec709,
        Self::Srgb,
        Self::P3D65,
        Self::P3Dci,
        Self::Rec2020,
        Self::AcesAp1,
        Self::AcesAp0,
    ];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Rec709 => "rec709",
            Self::Srgb => "srgb",
            Self::P3D65 => "p3d65",
            Self::P3Dci => "p3dci",
            Self::Rec2020 => "rec2020",
            Self::AcesAp1 => "aces_ap1",
            Self::AcesAp0 => "aces_ap0",
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Rec709 => 0,
            Self::Srgb => 1,
            Self::P3D65 => 2,
            Self::P3Dci => 3,
            Self::Rec2020 => 4,
            Self::AcesAp1 => 5,
            Self::AcesAp0 => 6,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|space| space.as_key() == key)
    }

    /// The white point of the primaries, as CIE 1931 x and y.
    pub const fn white(self) -> (f32, f32) {
        match self {
            Self::Rec709 | Self::Srgb | Self::P3D65 | Self::Rec2020 => (D65_X, D65_Y),
            Self::P3Dci => (DCI_X, DCI_Y),
            Self::AcesAp1 | Self::AcesAp0 => (ACES_X, ACES_Y),
        }
    }
}

/// Transfer function the frame is decoded from and encoded back into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpaceTransformGamma {
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
    /// A pure 2.6 power curve.
    Gamma26,
}

impl ColorSpaceTransformGamma {
    /// The documented default transfer function.
    pub const DEFAULT: Self = Self::Linear;

    /// Every transfer function, in menu order.
    pub const ALL: [Self; 6] = [
        Self::Linear,
        Self::Srgb,
        Self::Rec709,
        Self::Gamma22,
        Self::Gamma24,
        Self::Gamma26,
    ];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Srgb => "srgb",
            Self::Rec709 => "rec709",
            Self::Gamma22 => "gamma_2_2",
            Self::Gamma24 => "gamma_2_4",
            Self::Gamma26 => "gamma_2_6",
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
            Self::Gamma26 => 5,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|gamma| gamma.as_key() == key)
    }
}

/// How the tone mapping curve rolls the highlights off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpaceTransformToneMapping {
    /// Leaves the values untouched.
    None,
    /// The Reinhard curve, `x / (1 + x)`.
    Reinhard,
    /// The Hable filmic curve, normalised so that one maps to one.
    Hable,
    /// The Narkowicz ACES approximation.
    Aces,
}

impl ColorSpaceTransformToneMapping {
    /// The documented default curve.
    pub const DEFAULT: Self = Self::None;

    /// Every curve, in menu order.
    pub const ALL: [Self; 4] = [Self::None, Self::Reinhard, Self::Hable, Self::Aces];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Reinhard => "reinhard",
            Self::Hable => "hable",
            Self::Aces => "aces",
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Reinhard => 1,
            Self::Hable => 2,
            Self::Aces => 3,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.as_key() == key)
    }
}

/// How out of gamut colours are brought back into the target primaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpaceTransformGamutMapping {
    /// Leaves the values untouched.
    None,
    /// Clamps every channel.
    Clip,
    /// Pulls saturated colours towards the grey axis.
    Desaturate,
}

impl ColorSpaceTransformGamutMapping {
    /// The documented default mapping.
    pub const DEFAULT: Self = Self::None;

    /// Every mapping, in menu order.
    pub const ALL: [Self; 3] = [Self::None, Self::Clip, Self::Desaturate];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Clip => "clip",
            Self::Desaturate => "desaturate",
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Clip => 1,
            Self::Desaturate => 2,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.as_key() == key)
    }
}

/// Documented default input primaries.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_INPUT_COLOR_SPACE: ColorSpaceTransformSpace =
    ColorSpaceTransformSpace::DEFAULT;
/// Documented default input transfer function.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_INPUT_GAMMA: ColorSpaceTransformGamma =
    ColorSpaceTransformGamma::DEFAULT;
/// Documented default output primaries.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_OUTPUT_COLOR_SPACE: ColorSpaceTransformSpace =
    ColorSpaceTransformSpace::DEFAULT;
/// Documented default output transfer function.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_OUTPUT_GAMMA: ColorSpaceTransformGamma =
    ColorSpaceTransformGamma::DEFAULT;
/// Documented default tone mapping curve.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_TONE_MAPPING_METHOD: ColorSpaceTransformToneMapping =
    ColorSpaceTransformToneMapping::DEFAULT;
/// Documented default gamut mapping method.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_GAMUT_MAPPING_METHOD: ColorSpaceTransformGamutMapping =
    ColorSpaceTransformGamutMapping::DEFAULT;
/// Documented default maximum input, in nits.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_MAX_INPUT_NITS: f32 = 100.0;
/// Documented default maximum output, in nits.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_MAX_OUTPUT_NITS: f32 = 100.0;
/// Documented default adaptation strength.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_ADAPTATION: f32 = 9.0;
/// Documented default saturation rolloff start, in nits.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_SATURATION_ROLLOFF_START: f32 = 100.0;
/// Documented default saturation rolloff limit, in nits.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_SATURATION_ROLLOFF_LIMIT: f32 = 10000.0;
/// Documented default saturation knee.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_SATURATION_KNEE: f32 = 0.9;
/// Documented default saturation maximum.
pub const COLOR_SPACE_TRANSFORM_DEFAULT_SATURATION_MAX: f32 = 1.0;

/// Smallest accepted maximum input, in nits.
pub const COLOR_SPACE_TRANSFORM_MIN_NITS: f32 = 1.0;
/// Largest accepted maximum input or output, in nits.
pub const COLOR_SPACE_TRANSFORM_MAX_NITS: f32 = 10000.0;
/// Smallest accepted adaptation strength.
pub const COLOR_SPACE_TRANSFORM_MIN_ADAPTATION: f32 = 0.0;
/// Largest accepted adaptation strength.
pub const COLOR_SPACE_TRANSFORM_MAX_ADAPTATION: f32 = 100.0;

const D65_X: f32 = 0.3127;
const D65_Y: f32 = 0.3290;
const DCI_X: f32 = 0.3140;
const DCI_Y: f32 = 0.3510;
const ACES_X: f32 = 0.321_68;
const ACES_Y: f32 = 0.337_67;
const REFERENCE_NITS: f32 = 100.0;
const DIFFUSE_WHITE_NITS: f32 = 203.0;
const IDENTITY_TOLERANCE: f32 = 1.0e-6;

type Matrix3 = [[f32; 3]; 3];

/// Authored parameters of the colour space transform effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorSpaceTransformParameters {
    /// Switches the whole transform on.
    pub color_space_transform: f32,
    /// Primaries the frame is read in.
    pub input_color_space: ColorSpaceTransformSpace,
    /// Transfer function the frame is decoded from.
    pub input_gamma: ColorSpaceTransformGamma,
    /// Primaries the frame is written in.
    pub output_color_space: ColorSpaceTransformSpace,
    /// Transfer function the frame is encoded into.
    pub output_gamma: ColorSpaceTransformGamma,
    /// Swaps the input and output pairs.
    pub swap: f32,
    /// Switches tone mapping on.
    pub tone_mapping: f32,
    /// Curve tone mapping uses.
    pub tone_mapping_method: ColorSpaceTransformToneMapping,
    /// Uses the authored maximum input instead of the reference.
    pub use_custom_max_input: f32,
    /// Maximum input, in nits.
    pub max_input_nits: f32,
    /// Uses the authored maximum output instead of the reference.
    pub use_custom_max_output: f32,
    /// Maximum output, in nits.
    pub max_output_nits: f32,
    /// Adaptation strength the host exposes.
    pub adaptation: f32,
    /// Saturation rolloff start, in nits.
    pub saturation_rolloff_start: f32,
    /// Saturation rolloff limit, in nits.
    pub saturation_rolloff_limit: f32,
    /// Switches gamut mapping on.
    pub gamut_mapping: f32,
    /// Method gamut mapping uses.
    pub gamut_mapping_method: ColorSpaceTransformGamutMapping,
    /// Saturation the desaturation starts at.
    pub saturation_knee: f32,
    /// Saturation the desaturation compresses down to.
    pub saturation_max: f32,
    /// Switches the advanced gamut controls on.
    pub advanced: f32,
    /// Uses the standard HDR conversions.
    pub use_hdr_standard_conversions: f32,
    /// Treats 203 nits as diffuse white.
    pub use_hdr_diffuse_white: f32,
    /// Applies the forward OOTF.
    pub apply_forward_ootf: f32,
    /// Applies the inverse OOTF.
    pub apply_inverse_ootf: f32,
    /// Adapts the white points between the two spaces.
    pub use_white_point_adaptation: f32,
}

impl Default for ColorSpaceTransformParameters {
    fn default() -> Self {
        Self {
            color_space_transform: 1.0,
            input_color_space: ColorSpaceTransformSpace::DEFAULT,
            input_gamma: ColorSpaceTransformGamma::DEFAULT,
            output_color_space: ColorSpaceTransformSpace::DEFAULT,
            output_gamma: ColorSpaceTransformGamma::DEFAULT,
            swap: 0.0,
            tone_mapping: 1.0,
            tone_mapping_method: ColorSpaceTransformToneMapping::DEFAULT,
            use_custom_max_input: 0.0,
            max_input_nits: COLOR_SPACE_TRANSFORM_DEFAULT_MAX_INPUT_NITS,
            use_custom_max_output: 0.0,
            max_output_nits: COLOR_SPACE_TRANSFORM_DEFAULT_MAX_OUTPUT_NITS,
            adaptation: COLOR_SPACE_TRANSFORM_DEFAULT_ADAPTATION,
            saturation_rolloff_start: COLOR_SPACE_TRANSFORM_DEFAULT_SATURATION_ROLLOFF_START,
            saturation_rolloff_limit: COLOR_SPACE_TRANSFORM_DEFAULT_SATURATION_ROLLOFF_LIMIT,
            gamut_mapping: 1.0,
            gamut_mapping_method: ColorSpaceTransformGamutMapping::DEFAULT,
            saturation_knee: COLOR_SPACE_TRANSFORM_DEFAULT_SATURATION_KNEE,
            saturation_max: COLOR_SPACE_TRANSFORM_DEFAULT_SATURATION_MAX,
            advanced: 1.0,
            use_hdr_standard_conversions: 1.0,
            use_hdr_diffuse_white: 1.0,
            apply_forward_ootf: 0.0,
            apply_inverse_ootf: 0.0,
            use_white_point_adaptation: 1.0,
        }
    }
}

/// Rejects an authored value that lies outside its documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("color_space_transform.{parameter} must be finite and within its documented range")]
pub struct ColorSpaceTransformError {
    parameter: &'static str,
}

impl ColorSpaceTransformError {
    /// The parameter that failed validation.
    pub const fn parameter(&self) -> &'static str {
        self.parameter
    }
}

/// The six uniform cells the fused point pass reads.
pub type ColorSpaceTransformUniforms = ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated colour space transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorSpaceTransformGrade {
    parameters: ColorSpaceTransformParameters,
    matrix: Matrix3,
    input_space: ColorSpaceTransformSpace,
    input_gamma: ColorSpaceTransformGamma,
    output_space: ColorSpaceTransformSpace,
    output_gamma: ColorSpaceTransformGamma,
}

impl ColorSpaceTransformGrade {
    /// Validates the parameters and composes the primaries matrix.
    pub fn new(
        parameters: ColorSpaceTransformParameters,
    ) -> Result<Self, ColorSpaceTransformError> {
        for (name, value, minimum, maximum) in [
            (
                "color_space_transform",
                parameters.color_space_transform,
                0.0,
                1.0,
            ),
            ("swap", parameters.swap, 0.0, 1.0),
            ("tone_mapping", parameters.tone_mapping, 0.0, 1.0),
            (
                "use_custom_max_input",
                parameters.use_custom_max_input,
                0.0,
                1.0,
            ),
            (
                "max_input_nits",
                parameters.max_input_nits,
                COLOR_SPACE_TRANSFORM_MIN_NITS,
                COLOR_SPACE_TRANSFORM_MAX_NITS,
            ),
            (
                "use_custom_max_output",
                parameters.use_custom_max_output,
                0.0,
                1.0,
            ),
            (
                "max_output_nits",
                parameters.max_output_nits,
                COLOR_SPACE_TRANSFORM_MIN_NITS,
                COLOR_SPACE_TRANSFORM_MAX_NITS,
            ),
            (
                "adaptation",
                parameters.adaptation,
                COLOR_SPACE_TRANSFORM_MIN_ADAPTATION,
                COLOR_SPACE_TRANSFORM_MAX_ADAPTATION,
            ),
            (
                "saturation_rolloff_start",
                parameters.saturation_rolloff_start,
                0.0,
                10000.0,
            ),
            (
                "saturation_rolloff_limit",
                parameters.saturation_rolloff_limit,
                0.0,
                10000.0,
            ),
            ("gamut_mapping", parameters.gamut_mapping, 0.0, 1.0),
            ("saturation_knee", parameters.saturation_knee, 0.0, 1.0),
            ("saturation_max", parameters.saturation_max, 0.0, 1.0),
            ("advanced", parameters.advanced, 0.0, 1.0),
            (
                "use_hdr_standard_conversions",
                parameters.use_hdr_standard_conversions,
                0.0,
                1.0,
            ),
            (
                "use_hdr_diffuse_white",
                parameters.use_hdr_diffuse_white,
                0.0,
                1.0,
            ),
            (
                "apply_forward_ootf",
                parameters.apply_forward_ootf,
                0.0,
                1.0,
            ),
            (
                "apply_inverse_ootf",
                parameters.apply_inverse_ootf,
                0.0,
                1.0,
            ),
            (
                "use_white_point_adaptation",
                parameters.use_white_point_adaptation,
                0.0,
                1.0,
            ),
        ] {
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(ColorSpaceTransformError { parameter: name });
            }
        }

        let (input_space, input_gamma, output_space, output_gamma) = if parameters.swap >= 0.5 {
            (
                parameters.output_color_space,
                parameters.output_gamma,
                parameters.input_color_space,
                parameters.input_gamma,
            )
        } else {
            (
                parameters.input_color_space,
                parameters.input_gamma,
                parameters.output_color_space,
                parameters.output_gamma,
            )
        };

        let mut matrix = matrix3_mul(
            space_xyz_to_rgb(output_space),
            space_rgb_to_xyz(input_space),
        );
        if parameters.use_white_point_adaptation >= 0.5 {
            let (source, target) = (input_space.white(), output_space.white());
            if (source.0 - target.0).abs() > IDENTITY_TOLERANCE
                || (source.1 - target.1).abs() > IDENTITY_TOLERANCE
            {
                matrix = matrix3_mul(space_xyz_to_rgb(output_space), cat02_matrix(source, target));
                matrix = matrix3_mul(matrix, space_rgb_to_xyz(input_space));
            }
        }

        Ok(Self {
            parameters,
            matrix,
            input_space,
            input_gamma,
            output_space,
            output_gamma,
        })
    }

    /// The documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(ColorSpaceTransformParameters::default()).expect("the defaults are in range")
    }

    /// The authored parameters.
    pub const fn parameters(&self) -> &ColorSpaceTransformParameters {
        &self.parameters
    }

    /// The primaries the frame is read in after applying the swap.
    pub const fn input_space(&self) -> ColorSpaceTransformSpace {
        self.input_space
    }

    /// The primaries the frame is written in after applying the swap.
    pub const fn output_space(&self) -> ColorSpaceTransformSpace {
        self.output_space
    }

    /// The composed primaries matrix.
    pub const fn matrix(&self) -> &Matrix3 {
        &self.matrix
    }

    /// Whether the authored configuration leaves every pixel untouched.
    pub fn is_identity(&self) -> bool {
        let p = &self.parameters;
        self.input_space == self.output_space
            && self.input_gamma == self.output_gamma
            && p.tone_mapping_method == ColorSpaceTransformToneMapping::None
            && p.gamut_mapping_method == ColorSpaceTransformGamutMapping::None
            && p.apply_forward_ootf < 0.5
            && p.apply_inverse_ootf < 0.5
    }

    /// Relative cost of the fused point operation.
    pub const fn cost(&self) -> u32 {
        4
    }

    /// Packs the six uniform cells the fused point pass reads.
    pub fn gpu_controls(&self) -> ColorSpaceTransformUniforms {
        let p = &self.parameters;
        let flags = f32::from(p.tone_mapping >= 0.5)
            + 2.0 * f32::from(p.gamut_mapping >= 0.5)
            + 4.0 * f32::from(p.advanced >= 0.5)
            + 8.0 * f32::from(p.use_hdr_diffuse_white >= 0.5)
            + 16.0 * f32::from(p.apply_forward_ootf >= 0.5)
            + 32.0 * f32::from(p.apply_inverse_ootf >= 0.5)
            + 64.0 * f32::from(p.color_space_transform >= 0.5);
        let rolloff = (p.saturation_rolloff_limit / 10000.0)
            .clamp(0.0, 1.0)
            .mul_add(1024.0, 0.0)
            .round()
            + (p.saturation_rolloff_start / 10000.0).clamp(0.0, 1.0).round() * 1024.0;
        let matrix = self.matrix;
        (
            [matrix[0][0], matrix[0][1], matrix[0][2], matrix[1][0]],
            [matrix[1][1], matrix[1][2], matrix[2][0], matrix[2][1]],
            [
                matrix[2][2],
                f32::from(self.input_gamma.as_u8()),
                f32::from(self.output_gamma.as_u8()),
                f32::from(p.tone_mapping_method.as_u8()),
            ],
            [
                f32::from(p.gamut_mapping_method.as_u8()),
                if p.use_custom_max_input >= 0.5 {
                    p.max_input_nits
                } else {
                    REFERENCE_NITS
                },
                if p.use_custom_max_output >= 0.5 {
                    p.max_output_nits
                } else {
                    REFERENCE_NITS
                },
                p.saturation_knee,
            ],
            [p.saturation_max, rolloff, flags, p.adaptation],
            [0.0; 4],
        )
    }

    /// Hashes the authored configuration into the render op.
    pub fn hash_state(&self, state: &mut impl std::hash::Hasher) {
        self.input_space.as_u8().hash(state);
        self.output_space.as_u8().hash(state);
        self.input_gamma.as_u8().hash(state);
        self.output_gamma.as_u8().hash(state);
        self.parameters.tone_mapping_method.as_u8().hash(state);
        self.parameters.gamut_mapping_method.as_u8().hash(state);
        for value in [
            self.parameters.color_space_transform,
            self.parameters.swap,
            self.parameters.tone_mapping,
            self.parameters.use_custom_max_input,
            self.parameters.max_input_nits,
            self.parameters.use_custom_max_output,
            self.parameters.max_output_nits,
            self.parameters.adaptation,
            self.parameters.saturation_rolloff_start,
            self.parameters.saturation_rolloff_limit,
            self.parameters.gamut_mapping,
            self.parameters.saturation_knee,
            self.parameters.saturation_max,
            self.parameters.advanced,
            self.parameters.use_hdr_standard_conversions,
            self.parameters.use_hdr_diffuse_white,
            self.parameters.apply_forward_ootf,
            self.parameters.apply_inverse_ootf,
            self.parameters.use_white_point_adaptation,
        ] {
            value.to_bits().hash(state);
        }
    }
}

impl std::hash::Hash for ColorSpaceTransformGrade {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.hash_state(state);
    }
}

use std::hash::Hash;

/// The primaries matrix from the space's RGB to XYZ, relative to its own white.
fn space_rgb_to_xyz(space: ColorSpaceTransformSpace) -> Matrix3 {
    match space {
        ColorSpaceTransformSpace::Rec709 | ColorSpaceTransformSpace::Srgb => [
            [0.412_390_8, 0.357_584_3, 0.180_480_8],
            [0.212_639, 0.715_168_7, 0.072_192_3],
            [0.019_330_8, 0.119_194_8, 0.950_532_2],
        ],
        ColorSpaceTransformSpace::P3D65 => [
            [0.486_570_9, 0.265_667_7, 0.198_217_3],
            [0.228_974_6, 0.691_738_5, 0.079_286_9],
            [0.000_000_0, 0.045_113_4, 1.043_944_4],
        ],
        ColorSpaceTransformSpace::P3Dci => matrix3_mul(
            cat02_matrix((D65_X, D65_Y), (DCI_X, DCI_Y)),
            [
                [0.486_570_9, 0.265_667_7, 0.198_217_3],
                [0.228_974_6, 0.691_738_5, 0.079_286_9],
                [0.000_000_0, 0.045_113_4, 1.043_944_4],
            ],
        ),
        ColorSpaceTransformSpace::Rec2020 => [
            [0.636_958, 0.144_616_9, 0.168_881],
            [0.262_700_2, 0.677_998_1, 0.059_301_7],
            [0.000_000_0, 0.028_072_7, 1.060_985_1],
        ],
        ColorSpaceTransformSpace::AcesAp1 => [
            [0.662_454_2, 0.134_004_2, 0.156_187_7],
            [0.272_228_7, 0.674_081_8, 0.053_689_5],
            [-0.005_574_6, 0.004_060_7, 1.010_339_1],
        ],
        ColorSpaceTransformSpace::AcesAp0 => [
            [0.952_552_4, 0.000_000_0, 0.000_093_7],
            [0.343_966_4, 0.728_166_1, -0.072_132_5],
            [0.000_000_0, 0.000_000_0, 1.008_825_2],
        ],
    }
}

/// The primaries matrix from XYZ to the space's RGB, relative to its own white.
fn space_xyz_to_rgb(space: ColorSpaceTransformSpace) -> Matrix3 {
    matrix3_inverse(space_rgb_to_xyz(space))
}

/// The row-major product of two matrices.
fn matrix3_mul(left: Matrix3, right: Matrix3) -> Matrix3 {
    let mut product = [[0.0; 3]; 3];
    for (row, product_row) in product.iter_mut().enumerate() {
        for (column, value) in product_row.iter_mut().enumerate() {
            *value = left[row][0] * right[0][column]
                + left[row][1] * right[1][column]
                + left[row][2] * right[2][column];
        }
    }
    product
}

/// The inverse of a matrix, or the identity when it is singular.
fn matrix3_inverse(matrix: Matrix3) -> Matrix3 {
    let a = matrix[0][0];
    let b = matrix[0][1];
    let c = matrix[0][2];
    let d = matrix[1][0];
    let e = matrix[1][1];
    let f = matrix[1][2];
    let g = matrix[2][0];
    let h = matrix[2][1];
    let i = matrix[2][2];
    let determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if determinant.abs() <= IDENTITY_TOLERANCE {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let scale = 1.0 / determinant;
    [
        [
            (e * i - f * h) * scale,
            (c * h - b * i) * scale,
            (b * f - c * e) * scale,
        ],
        [
            (f * g - d * i) * scale,
            (a * i - c * g) * scale,
            (c * d - a * f) * scale,
        ],
        [
            (d * h - e * g) * scale,
            (b * g - a * h) * scale,
            (a * e - b * d) * scale,
        ],
    ]
}

/// The CAT02 cone response matrix.
const CAT02: Matrix3 = [
    [0.732_8, 0.429_6, -0.162_4],
    [-0.703_6, 1.697_5, 0.006_1],
    [0.003_0, 0.013_6, 0.983_4],
];

/// The inverse CAT02 cone response matrix.
const CAT02_INVERSE: Matrix3 = [
    [1.096_124, -0.278_869, 0.182_745],
    [0.454_369, 0.473_533, 0.072_098],
    [-0.009_628, -0.005_698, 1.015_326],
];

/// The Von Kries adaptation between two white points, in XYZ.
fn cat02_matrix(source: (f32, f32), target: (f32, f32)) -> Matrix3 {
    let source = cone_white(source);
    let target = cone_white(target);
    let mut ratio = Matrix3::default();
    for index in 0..3 {
        let denominator = source[index];
        ratio[index][index] = if denominator.abs() <= 1.0e-6 {
            1.0
        } else {
            target[index] / denominator
        };
    }
    matrix3_mul(CAT02_INVERSE, matrix3_mul(ratio, CAT02))
}

/// The white point in cone response space.
fn cone_white(white: (f32, f32)) -> [f32; 3] {
    let xyz = [white.0 / white.1, 1.0, (1.0 - white.0 - white.1) / white.1];
    [
        CAT02[0][0] * xyz[0] + CAT02[0][1] * xyz[1] + CAT02[0][2] * xyz[2],
        CAT02[1][0] * xyz[0] + CAT02[1][1] * xyz[1] + CAT02[1][2] * xyz[2],
        CAT02[2][0] * xyz[0] + CAT02[2][1] * xyz[1] + CAT02[2][2] * xyz[2],
    ]
}

/// Decodes a transfer function to linear light.
fn decode_transfer(value: f32, gamma: ColorSpaceTransformGamma) -> f32 {
    match gamma {
        ColorSpaceTransformGamma::Linear => value,
        ColorSpaceTransformGamma::Srgb => {
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        ColorSpaceTransformGamma::Rec709 => {
            if value < 0.081 {
                value / 4.5
            } else {
                ((value + 0.099) / 1.099).powf(1.0 / 0.45)
            }
        }
        ColorSpaceTransformGamma::Gamma22 => signed_pow(value, 2.2),
        ColorSpaceTransformGamma::Gamma24 => signed_pow(value, 2.4),
        ColorSpaceTransformGamma::Gamma26 => signed_pow(value, 2.6),
    }
}

/// Encodes linear light into a transfer function.
fn encode_transfer(value: f32, gamma: ColorSpaceTransformGamma) -> f32 {
    match gamma {
        ColorSpaceTransformGamma::Linear => value,
        ColorSpaceTransformGamma::Srgb => {
            if value <= 0.003_130_8 {
                value * 12.92
            } else {
                1.055 * value.powf(1.0 / 2.4) - 0.055
            }
        }
        ColorSpaceTransformGamma::Rec709 => {
            if value < 0.018 {
                value * 4.5
            } else {
                1.099 * value.powf(0.45) - 0.099
            }
        }
        ColorSpaceTransformGamma::Gamma22 => signed_pow(value, 1.0 / 2.2),
        ColorSpaceTransformGamma::Gamma24 => signed_pow(value, 1.0 / 2.4),
        ColorSpaceTransformGamma::Gamma26 => signed_pow(value, 1.0 / 2.6),
    }
}

/// A power that keeps the sign of its input.
fn signed_pow(value: f32, exponent: f32) -> f32 {
    if value < 0.0 {
        -((-value).powf(exponent))
    } else {
        value.powf(exponent)
    }
}

/// The Hable filmic curve, normalised so that one maps to one.
fn hable_curve(value: f32) -> f32 {
    const A: f32 = 0.15;
    const B: f32 = 0.50;
    const C: f32 = 0.10;
    const D: f32 = 0.20;
    const E: f32 = 0.02;
    const F: f32 = 0.30;
    let numerator = value * (A * value + C) + D * E;
    let denominator = value * (A * value + B) + D * F - E / F;
    if denominator.abs() <= 1.0e-6 {
        0.0
    } else {
        numerator / denominator
    }
}

/// Applies the tone mapping curve to one normalised value.
fn tone_map(value: f32, method: ColorSpaceTransformToneMapping) -> f32 {
    match method {
        ColorSpaceTransformToneMapping::None => value,
        ColorSpaceTransformToneMapping::Reinhard => {
            if value <= 0.0 {
                0.0
            } else {
                value / (1.0 + value)
            }
        }
        ColorSpaceTransformToneMapping::Hable => {
            let white = hable_curve(1.0);
            if white.abs() <= 1.0e-6 {
                value
            } else {
                hable_curve(value.max(0.0)) / white
            }
        }
        ColorSpaceTransformToneMapping::Aces => {
            let value = value.max(0.0);
            ((value * (2.51 * value + 0.03)) / (value * (2.43 * value + 0.59) + 0.14))
                .clamp(0.0, 1.0)
        }
    }
}

/// Pulls a colour back into gamut.
fn gamut_map(
    rgb: [f32; 3],
    method: ColorSpaceTransformGamutMapping,
    knee: f32,
    maximum: f32,
) -> [f32; 3] {
    match method {
        ColorSpaceTransformGamutMapping::None => rgb,
        ColorSpaceTransformGamutMapping::Clip => [
            rgb[0].clamp(0.0, 1.0),
            rgb[1].clamp(0.0, 1.0),
            rgb[2].clamp(0.0, 1.0),
        ],
        ColorSpaceTransformGamutMapping::Desaturate => {
            let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
            let high = rgb[0].max(rgb[1]).max(rgb[2]);
            let low = rgb[0].min(rgb[1]).min(rgb[2]);
            let saturation = if high.abs() <= 1.0e-6 {
                0.0
            } else {
                (high - low) / high
            };
            let knee = knee.clamp(0.0, 1.0);
            let maximum = maximum.clamp(0.0, 1.0);
            let factor = if saturation <= knee || (1.0 - knee).abs() <= IDENTITY_TOLERANCE {
                1.0
            } else {
                let over = (saturation - knee) / (1.0 - knee);
                1.0 - over * (1.0 - maximum)
            };
            let factor = factor.max(0.0);
            [
                (luma + (rgb[0] - luma) * factor).clamp(0.0, 1.0),
                (luma + (rgb[1] - luma) * factor).clamp(0.0, 1.0),
                (luma + (rgb[2] - luma) * factor).clamp(0.0, 1.0),
            ]
        }
    }
}

/// Applies the forward or inverse OOTF to one colour.
fn apply_ootf(rgb: [f32; 3], inverse: bool) -> [f32; 3] {
    let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    if luma <= 0.0 {
        return rgb;
    }
    let gain = luma.powf(0.2);
    if inverse {
        [rgb[0] / gain, rgb[1] / gain, rgb[2] / gain]
    } else {
        [rgb[0] * gain, rgb[1] * gain, rgb[2] * gain]
    }
}

/// Transforms a whole frame in place.
pub fn color_space_transform_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ColorSpaceTransformGrade,
) -> bool {
    apply_color_space_transform_f32_controlled(pixels, width, height, grade, &mut || Ok(()))
        .unwrap_or_else(|error: std::convert::Infallible| match error {})
}

/// Transforms a whole frame in place, honouring a cancellation checkpoint.
pub(crate) fn apply_color_space_transform_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ColorSpaceTransformGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.parameters.color_space_transform < 0.5 || grade.is_identity() {
        return Ok(true);
    }
    checkpoint()?;

    let parameters = &grade.parameters;
    let matrix = grade.matrix;
    let tone_mapping = parameters.tone_mapping >= 0.5
        && parameters.tone_mapping_method != ColorSpaceTransformToneMapping::None;
    let gamut_mapping = parameters.gamut_mapping >= 0.5
        && parameters.gamut_mapping_method != ColorSpaceTransformGamutMapping::None;
    let diffuse_white = if parameters.use_hdr_diffuse_white >= 0.5 {
        DIFFUSE_WHITE_NITS
    } else {
        REFERENCE_NITS
    };
    let exposure = if parameters.use_custom_max_input >= 0.5 {
        parameters.max_input_nits
    } else {
        REFERENCE_NITS
    };
    let output_scale = if parameters.use_custom_max_output >= 0.5 {
        parameters.max_output_nits
    } else {
        REFERENCE_NITS
    };
    let gain = diffuse_white / exposure;
    let output_gain = output_scale / diffuse_white;
    // The two OOTF directions are reciprocal, so asking for both is a no-op.
    let ootf = match (
        parameters.apply_forward_ootf >= 0.5,
        parameters.apply_inverse_ootf >= 0.5,
    ) {
        (true, true) | (false, false) => 0,
        (true, false) => 1,
        (false, true) => -1,
    };

    for chunk in working[..pixel_count].chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            if pixel[3] <= 0.0 {
                continue;
            }
            let mut colour = [
                decode_transfer(pixel[0], grade.input_gamma),
                decode_transfer(pixel[1], grade.input_gamma),
                decode_transfer(pixel[2], grade.input_gamma),
            ];
            colour = [
                matrix[0][0] * colour[0] + matrix[0][1] * colour[1] + matrix[0][2] * colour[2],
                matrix[1][0] * colour[0] + matrix[1][1] * colour[1] + matrix[1][2] * colour[2],
                matrix[2][0] * colour[0] + matrix[2][1] * colour[1] + matrix[2][2] * colour[2],
            ];
            if tone_mapping {
                for channel in &mut colour {
                    let scaled = tone_map(*channel * gain, parameters.tone_mapping_method);
                    *channel = scaled * output_gain;
                }
            }
            if gamut_mapping {
                colour = gamut_map(
                    colour,
                    parameters.gamut_mapping_method,
                    parameters.saturation_knee,
                    parameters.saturation_max,
                );
            }
            if ootf > 0 {
                colour = apply_ootf(colour, false);
            } else if ootf < 0 {
                colour = apply_ootf(colour, true);
            }
            pixel[0] = encode_transfer(colour[0], grade.output_gamma);
            pixel[1] = encode_transfer(colour[1], grade.output_gamma);
            pixel[2] = encode_transfer(colour[2], grade.output_gamma);
        }
    }
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hasher;

    fn grade(parameters: ColorSpaceTransformParameters) -> ColorSpaceTransformGrade {
        ColorSpaceTransformGrade::new(parameters).expect("valid grade")
    }

    fn flat(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        for (parameter, parameters) in [
            (
                "max_input_nits",
                ColorSpaceTransformParameters {
                    max_input_nits: 0.5,
                    ..ColorSpaceTransformParameters::default()
                },
            ),
            (
                "max_output_nits",
                ColorSpaceTransformParameters {
                    max_output_nits: 20000.0,
                    ..ColorSpaceTransformParameters::default()
                },
            ),
            (
                "adaptation",
                ColorSpaceTransformParameters {
                    adaptation: 101.0,
                    ..ColorSpaceTransformParameters::default()
                },
            ),
            (
                "saturation_knee",
                ColorSpaceTransformParameters {
                    saturation_knee: 1.5,
                    ..ColorSpaceTransformParameters::default()
                },
            ),
            (
                "color_space_transform",
                ColorSpaceTransformParameters {
                    color_space_transform: f32::NAN,
                    ..ColorSpaceTransformParameters::default()
                },
            ),
        ] {
            let error = ColorSpaceTransformGrade::new(parameters).expect_err("out of range");
            assert_eq!(error.parameter(), parameter);
        }
    }

    #[test]
    fn documented_defaults_are_stable() {
        let grade = ColorSpaceTransformGrade::documented_defaults();
        let p = grade.parameters();
        assert_eq!(p.input_color_space, ColorSpaceTransformSpace::Rec709);
        assert_eq!(p.output_gamma, ColorSpaceTransformGamma::Linear);
        assert_eq!(p.max_input_nits, 100.0);
        assert_eq!(p.max_output_nits, 100.0);
        assert_eq!(p.adaptation, 9.0);
        assert_eq!(p.saturation_knee, 0.9);
        assert!(grade.is_identity(), "same space, linear, no mapping");
        assert_eq!(grade.cost(), 4);
    }

    #[test]
    fn menu_keys_round_trip() {
        for space in ColorSpaceTransformSpace::ALL {
            assert_eq!(
                ColorSpaceTransformSpace::from_key(space.as_key()),
                Some(space)
            );
        }
        for gamma in ColorSpaceTransformGamma::ALL {
            assert_eq!(
                ColorSpaceTransformGamma::from_key(gamma.as_key()),
                Some(gamma)
            );
        }
        for method in ColorSpaceTransformToneMapping::ALL {
            assert_eq!(
                ColorSpaceTransformToneMapping::from_key(method.as_key()),
                Some(method)
            );
        }
        for method in ColorSpaceTransformGamutMapping::ALL {
            assert_eq!(
                ColorSpaceTransformGamutMapping::from_key(method.as_key()),
                Some(method)
            );
        }
    }

    #[test]
    fn uniform_cells_carry_the_matrix_and_the_tags() {
        let grade = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::P3D65,
            input_gamma: ColorSpaceTransformGamma::Srgb,
            output_gamma: ColorSpaceTransformGamma::Gamma22,
            tone_mapping_method: ColorSpaceTransformToneMapping::Reinhard,
            gamut_mapping_method: ColorSpaceTransformGamutMapping::Clip,
            ..ColorSpaceTransformParameters::default()
        });
        let (params, color, extra0, extra1, extra2, extra3) = grade.gpu_controls();
        let matrix = grade.matrix();
        assert!((params[0] - matrix[0][0]).abs() <= 1.0e-6);
        assert!((params[3] - matrix[1][0]).abs() <= 1.0e-6);
        assert!((color[0] - matrix[1][1]).abs() <= 1.0e-6);
        assert!((color[3] - matrix[2][1]).abs() <= 1.0e-6);
        assert!((extra0[0] - matrix[2][2]).abs() <= 1.0e-6);
        assert_eq!(extra0[1], 1.0, "input gamma tag is sRGB");
        assert_eq!(extra0[2], 3.0, "output gamma tag is 2.2");
        assert_eq!(extra0[3], 1.0, "tone mapping tag is Reinhard");
        assert_eq!(extra1[0], 1.0, "gamut mapping tag is clip");
        assert_eq!(extra1[1], 100.0);
        assert_eq!(extra1[2], 100.0);
        assert!((extra1[3] - 0.9).abs() <= 1.0e-6);
        assert_eq!(extra2[0], 1.0);
        // tone + gamut + advanced + diffuse white + the master switch are on.
        assert_eq!(extra2[2], 1.0 + 2.0 + 4.0 + 8.0 + 64.0);
        assert_eq!(extra3, [0.0; 4]);
    }

    #[test]
    fn same_space_and_transfer_leave_the_frame_alone() {
        let grade = ColorSpaceTransformGrade::documented_defaults();
        let mut frame = flat(4, 1, [0.25, 0.5, 0.75, 1.0]);
        assert!(color_space_transform_frame_f32(&mut frame, 4, 1, &grade));
        assert_eq!(frame[0], [0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn switching_the_effect_off_leaves_the_frame_alone() {
        let grade = grade(ColorSpaceTransformParameters {
            color_space_transform: 0.0,
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::Rec2020,
            ..ColorSpaceTransformParameters::default()
        });
        let mut frame = flat(2, 1, [0.8, 0.2, 0.1, 1.0]);
        assert!(color_space_transform_frame_f32(&mut frame, 2, 1, &grade));
        assert_eq!(frame[0], [0.8, 0.2, 0.1, 1.0]);
    }

    #[test]
    fn rec709_primaries_move_towards_rec2020() {
        let grade = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::Rec2020,
            ..ColorSpaceTransformParameters::default()
        });
        let mut frame = flat(1, 1, [1.0, 0.0, 0.0, 1.0]);
        assert!(color_space_transform_frame_f32(&mut frame, 1, 1, &grade));
        let pixel = frame[0];
        assert!(
            pixel[0] < 0.75,
            "the red primary is less saturated: {pixel:?}"
        );
        assert!(pixel[1] > 0.05, "green picks up the wider gamut: {pixel:?}");
    }

    #[test]
    fn swapping_reverses_the_direction() {
        let forward = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::P3D65,
            ..ColorSpaceTransformParameters::default()
        });
        let backward = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::P3D65,
            swap: 1.0,
            ..ColorSpaceTransformParameters::default()
        });
        assert_eq!(backward.input_space(), ColorSpaceTransformSpace::P3D65);
        assert_eq!(backward.output_space(), ColorSpaceTransformSpace::Rec709);
        let mut left = flat(1, 1, [1.0, 0.2, 0.1, 1.0]);
        let mut right = left.clone();
        assert!(color_space_transform_frame_f32(&mut left, 1, 1, &forward));
        assert!(color_space_transform_frame_f32(&mut right, 1, 1, &backward));
        assert!((left[0][0] - right[0][0]).abs() > 1.0e-3);
    }

    #[test]
    fn transfer_functions_round_trip() {
        for gamma in ColorSpaceTransformGamma::ALL {
            for value in [0.05, 0.25, 0.5, 1.0] {
                let decoded = decode_transfer(value, gamma);
                let encoded = encode_transfer(decoded, gamma);
                assert!(
                    (encoded - value).abs() <= 2.0e-4,
                    "{gamma:?} {value} -> {encoded}"
                );
            }
        }
    }

    #[test]
    fn decoding_linear_input_is_a_passthrough() {
        for value in [0.1, 0.4, 0.9] {
            assert!(
                (decode_transfer(value, ColorSpaceTransformGamma::Linear) - value).abs() < 1e-6
            );
        }
    }

    #[test]
    fn srgb_and_rec709_curves_differ_below_the_knee() {
        let srgb = decode_transfer(0.05, ColorSpaceTransformGamma::Srgb);
        let rec709 = decode_transfer(0.05, ColorSpaceTransformGamma::Rec709);
        assert!(srgb < rec709, "{srgb} vs {rec709}");
    }

    #[test]
    fn tone_mapping_compresses_highlights() {
        let grade = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::Rec709,
            tone_mapping_method: ColorSpaceTransformToneMapping::Reinhard,
            max_input_nits: 400.0,
            use_custom_max_input: 1.0,
            max_output_nits: 100.0,
            use_custom_max_output: 1.0,
            ..ColorSpaceTransformParameters::default()
        });
        let mut frame = flat(1, 1, [0.9, 0.9, 0.9, 1.0]);
        assert!(color_space_transform_frame_f32(&mut frame, 1, 1, &grade));
        assert!(frame[0][0] < 0.9, "the highlight rolls off: {:?}", frame[0]);
    }

    #[test]
    fn the_aces_curve_is_monotonic() {
        let mut previous = f32::MIN;
        for step in 0..32 {
            let value = step as f32 / 16.0;
            let mapped = tone_map(value, ColorSpaceTransformToneMapping::Aces);
            assert!(mapped >= previous - 1.0e-6, "{value}");
            previous = mapped;
        }
    }

    #[test]
    fn gamut_desaturation_pulls_saturated_colour_towards_grey() {
        let mapped = gamut_map(
            [1.0, 0.0, 0.0],
            ColorSpaceTransformGamutMapping::Desaturate,
            0.5,
            0.5,
        );
        assert!(
            mapped[1] > 0.0,
            "green rises towards the grey axis: {mapped:?}"
        );
    }

    #[test]
    fn gamut_clipping_bounds_every_channel() {
        let mapped = gamut_map(
            [1.4, -0.2, 0.5],
            ColorSpaceTransformGamutMapping::Clip,
            0.9,
            1.0,
        );
        assert_eq!(mapped, [1.0, 0.0, 0.5]);
    }

    #[test]
    fn the_ootf_pair_cancels_itself() {
        let grade = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::Rec709,
            apply_forward_ootf: 1.0,
            apply_inverse_ootf: 1.0,
            ..ColorSpaceTransformParameters::default()
        });
        let mut frame = flat(1, 1, [0.3, 0.4, 0.5, 1.0]);
        assert!(color_space_transform_frame_f32(&mut frame, 1, 1, &grade));
        assert!((frame[0][0] - 0.3).abs() <= 2.0e-4, "{:?}", frame[0]);
        assert!((frame[0][1] - 0.4).abs() <= 2.0e-4, "{:?}", frame[0]);
    }

    #[test]
    fn alpha_is_passed_through() {
        let grade = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::AcesAp1,
            ..ColorSpaceTransformParameters::default()
        });
        let mut frame = flat(2, 1, [0.5, 0.5, 0.5, 0.25]);
        assert!(color_space_transform_frame_f32(&mut frame, 2, 1, &grade));
        assert_eq!(frame[0][3], 0.25);
        assert_eq!(frame[1][3], 0.25);
    }

    #[test]
    fn transparent_pixels_are_left_alone() {
        let grade = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Rec709,
            output_color_space: ColorSpaceTransformSpace::Rec2020,
            ..ColorSpaceTransformParameters::default()
        });
        let mut frame = flat(1, 1, [0.0, 0.0, 0.0, 0.0]);
        assert!(color_space_transform_frame_f32(&mut frame, 1, 1, &grade));
        assert_eq!(frame[0], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn short_and_empty_frames_are_rejected() {
        let grade = ColorSpaceTransformGrade::documented_defaults();
        let mut one = flat(1, 1, [0.5, 0.5, 0.5, 1.0]);
        assert!(!color_space_transform_frame_f32(&mut one, 2, 1, &grade));
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!color_space_transform_frame_f32(&mut empty, 0, 0, &grade));
    }

    #[test]
    fn the_frame_conversion_is_deterministic() {
        let grade = grade(ColorSpaceTransformParameters {
            input_color_space: ColorSpaceTransformSpace::Srgb,
            output_color_space: ColorSpaceTransformSpace::Rec2020,
            input_gamma: ColorSpaceTransformGamma::Srgb,
            output_gamma: ColorSpaceTransformGamma::Gamma24,
            ..ColorSpaceTransformParameters::default()
        });
        let mut first = flat(3, 2, [0.2, 0.4, 0.6, 0.8]);
        let mut second = first.clone();
        assert!(color_space_transform_frame_f32(&mut first, 3, 2, &grade));
        assert!(color_space_transform_frame_f32(&mut second, 3, 2, &grade));
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_stops_the_conversion() {
        let grade = grade(ColorSpaceTransformParameters {
            output_color_space: ColorSpaceTransformSpace::Rec2020,
            ..ColorSpaceTransformParameters::default()
        });
        let mut frame = flat(8, 8, [0.5, 0.5, 0.5, 1.0]);
        let mut calls = 0;
        let result: Result<bool, &'static str> =
            apply_color_space_transform_f32_controlled(&mut frame, 8, 8, &grade, &mut || {
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
    fn the_hash_separates_spaces_and_curves() {
        let mut first = std::collections::hash_map::DefaultHasher::new();
        grade(ColorSpaceTransformParameters {
            output_color_space: ColorSpaceTransformSpace::P3D65,
            ..ColorSpaceTransformParameters::default()
        })
        .hash_state(&mut first);
        let mut second = std::collections::hash_map::DefaultHasher::new();
        grade(ColorSpaceTransformParameters {
            output_color_space: ColorSpaceTransformSpace::Rec2020,
            ..ColorSpaceTransformParameters::default()
        })
        .hash_state(&mut second);
        assert_ne!(first.finish(), second.finish());
    }
}
