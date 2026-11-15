//! ACES transform: decode the source transfer function, work in ACES AP1,
//! compress the three gamut excursions toward the neutral axis, run the fitted
//! RRT/ODT tone curve, then encode the output transfer function.
//!
//! The effect is pixel local and deterministic: every pixel runs the same
//! composed matrices plus a one dimensional tone curve, so it lowers to the
//! fused point pass and keeps its CPU kernel as the reference implementation.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Smallest documented gamut compression limit and threshold.
pub const ACES_TRANSFORM_MIN_LIMIT: f32 = 0.0;
/// Largest documented gamut compression limit and threshold.
pub const ACES_TRANSFORM_MAX_LIMIT: f32 = 4.0;
/// Smallest documented gamut compression threshold.
pub const ACES_TRANSFORM_MIN_THRESHOLD: f32 = 0.0;
/// Largest documented gamut compression threshold.
pub const ACES_TRANSFORM_MAX_THRESHOLD: f32 = 4.0;
/// Smallest documented highlight roll-off.
pub const ACES_TRANSFORM_MIN_ROLLOFF: f32 = 0.0;
/// Largest documented highlight roll-off.
pub const ACES_TRANSFORM_MAX_ROLLOFF: f32 = 10.0;
/// Smallest documented output mid grey.
pub const ACES_TRANSFORM_MIN_MID_GRAY: f32 = 0.0;
/// Largest documented output mid grey.
pub const ACES_TRANSFORM_MAX_MID_GRAY: f32 = 100.0;

/// The documented master switch value.
pub const ACES_TRANSFORM_DEFAULT_TRANSFORM: f32 = 1.0;
/// The documented output mid grey percentage.
pub const ACES_TRANSFORM_DEFAULT_OUTPUT_MID_GRAY: f32 = 15.0;
/// The documented gamut compression strength.
pub const ACES_TRANSFORM_DEFAULT_GAMUT_COMPRESS: f32 = 1.0;
/// The documented cyan limit.
pub const ACES_TRANSFORM_DEFAULT_CYAN_LIMIT: f32 = 1.147;
/// The documented magenta limit.
pub const ACES_TRANSFORM_DEFAULT_MAGENTA_LIMIT: f32 = 1.264;
/// The documented yellow limit.
pub const ACES_TRANSFORM_DEFAULT_YELLOW_LIMIT: f32 = 1.312;
/// The documented cyan threshold.
pub const ACES_TRANSFORM_DEFAULT_CYAN_THRESHOLD: f32 = 0.815;
/// The documented magenta threshold.
pub const ACES_TRANSFORM_DEFAULT_MAGENTA_THRESHOLD: f32 = 0.803;
/// The documented yellow threshold.
pub const ACES_TRANSFORM_DEFAULT_YELLOW_THRESHOLD: f32 = 0.88;
/// The documented highlight roll-off.
pub const ACES_TRANSFORM_DEFAULT_ROLLOFF: f32 = 1.2;

/// The exposure index the effect treats as neutral.
pub const ACES_TRANSFORM_REFERENCE_EXPOSURE_INDEX: f32 = 800.0;

/// The scene grey the output mid grey anchors.
pub const ACES_TRANSFORM_MID_GRAY_REFERENCE: f32 = 0.18;

const IDENTITY_TOLERANCE: f32 = 1.0e-6;

/// A 3x3 row major colour matrix.
pub type Matrix3 = [[f32; 3]; 3];

const REC709_TO_AP1: Matrix3 = [
    [0.613_097_4, 0.339_523_22, 0.047_379_37],
    [0.070_193_7, 0.916_353_79, 0.013_452_47],
    [0.020_615_61, 0.109_569_81, 0.869_814_63],
];

const AP1_TO_REC709: Matrix3 = [
    [1.705_051, -0.621_792_3, -0.083_258_65],
    [-0.130_256_38, 1.140_804_9, -0.010_548_43],
    [-0.024_003_38, -0.128_969_02, 1.152_972_3],
];

const AP1_TO_P3D65: Matrix3 = [
    [1.379_214_2, -0.308_864_3, -0.070_349_82],
    [-0.069_334_82, 1.082_296_8, -0.012_961_99],
    [-0.002_159_03, -0.045_459_36, 1.047_618_4],
];

const AP1_TO_REC2020: Matrix3 = [
    [1.025_824_8, -0.020_053_28, -0.005_771_45],
    [-0.002_234_34, 1.004_586_6, -0.002_352_22],
    [-0.005_013_37, -0.025_290_11, 1.030_303_5],
];

/// Which ACES release the transform emulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcesVersion {
    /// ACES 1.0.
    Aces10,
    /// ACES 1.1.
    Aces11,
    /// ACES 2.0.
    Aces20,
}

impl AcesVersion {
    /// The documented default version.
    pub const DEFAULT: Self = Self::Aces20;

    /// Every version, in menu order.
    pub const ALL: [Self; 3] = [Self::Aces10, Self::Aces11, Self::Aces20];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Aces10 => "aces_1_0",
            Self::Aces11 => "aces_1_1",
            Self::Aces20 => "aces_2_0",
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Aces10 => 0,
            Self::Aces11 => 1,
            Self::Aces20 => 2,
        }
    }

    /// The work curve exposure pre-scale, an approximation of the 2.0 DRT.
    pub const fn exposure_scale(self) -> f32 {
        match self {
            Self::Aces10 | Self::Aces11 => 1.0,
            Self::Aces20 => 1.05,
        }
    }

    /// Resolves a stored menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_key() == key)
    }
}

/// The transfer function the incoming frame is encoded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcesInputTransform {
    /// The frame is already in AP1 scene linear.
    None,
    /// sRGB encoded Rec.709 primaries.
    Srgb,
    /// Rec.709 encoded Rec.709 primaries.
    Rec709,
    /// ACEScc encoded AP1.
    Acescc,
    /// ACEScct encoded AP1.
    Acescct,
}

impl AcesInputTransform {
    /// The documented default input transform.
    pub const DEFAULT: Self = Self::None;

    /// Every input transform, in menu order.
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Srgb,
        Self::Rec709,
        Self::Acescc,
        Self::Acescct,
    ];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Srgb => "srgb",
            Self::Rec709 => "rec709",
            Self::Acescc => "acescc",
            Self::Acescct => "acescct",
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Srgb => 1,
            Self::Rec709 => 2,
            Self::Acescc => 3,
            Self::Acescct => 4,
        }
    }

    /// Resolves a stored menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_key() == key)
    }
}

/// The display the transform renders to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcesOutputTransform {
    /// Rec.709 primaries with the Rec.709 transfer function.
    Rec709,
    /// Rec.709 primaries with the sRGB transfer function.
    Srgb,
    /// Display P3 primaries with the D65 white and the Rec.709 transfer.
    P3D65,
    /// Rec.2020 primaries with the D65 white and the Rec.709 transfer.
    Rec2020,
}

impl AcesOutputTransform {
    /// The documented default output transform.
    pub const DEFAULT: Self = Self::Rec709;

    /// Every output transform, in menu order.
    pub const ALL: [Self; 4] = [Self::Rec709, Self::Srgb, Self::P3D65, Self::Rec2020];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Rec709 => "rec709",
            Self::Srgb => "srgb",
            Self::P3D65 => "p3d65",
            Self::Rec2020 => "rec2020",
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Rec709 => 0,
            Self::Srgb => 1,
            Self::P3D65 => 2,
            Self::Rec2020 => 3,
        }
    }

    /// Resolves a stored menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_key() == key)
    }
}

/// The exposure index the transform is told the frame was shot at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcesExposureIndex {
    /// 100.
    E100,
    /// 200.
    E200,
    /// 400.
    E400,
    /// 800.
    E800,
    /// 1600.
    E1600,
    /// 3200.
    E3200,
}

impl AcesExposureIndex {
    /// The documented default exposure index.
    pub const DEFAULT: Self = Self::E800;

    /// Every exposure index, in menu order.
    pub const ALL: [Self; 6] = [
        Self::E100,
        Self::E200,
        Self::E400,
        Self::E800,
        Self::E1600,
        Self::E3200,
    ];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::E100 => "100",
            Self::E200 => "200",
            Self::E400 => "400",
            Self::E800 => "800",
            Self::E1600 => "1600",
            Self::E3200 => "3200",
        }
    }

    /// The numeric index.
    pub const fn value(self) -> f32 {
        match self {
            Self::E100 => 100.0,
            Self::E200 => 200.0,
            Self::E400 => 400.0,
            Self::E800 => 800.0,
            Self::E1600 => 1600.0,
            Self::E3200 => 3200.0,
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::E100 => 0,
            Self::E200 => 1,
            Self::E400 => 2,
            Self::E800 => 3,
            Self::E1600 => 4,
            Self::E3200 => 5,
        }
    }

    /// Resolves a stored menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_key() == key)
    }
}

/// How out-of-gamut excursions are pulled back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcesGamutCompression {
    /// No compression.
    None,
    /// Smooth tangent compression that asymptotes at the limit.
    Aces,
    /// Hard clip at the limit.
    Clip,
}

impl AcesGamutCompression {
    /// The documented default compression type.
    pub const DEFAULT: Self = Self::None;

    /// Every compression type, in menu order.
    pub const ALL: [Self; 3] = [Self::None, Self::Aces, Self::Clip];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Aces => "aces",
            Self::Clip => "clip",
        }
    }

    /// The compact tag the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Aces => 1,
            Self::Clip => 2,
        }
    }

    /// Resolves a stored menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_key() == key)
    }
}

/// The fifteen authored parameters of the effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcesTransformParameters {
    /// The master switch.
    pub aces_transform: f32,
    /// The emulated ACES release.
    pub aces_version: AcesVersion,
    /// How the incoming frame is encoded.
    pub input_transform: AcesInputTransform,
    /// How the frame is exposed.
    pub exposure_index: AcesExposureIndex,
    /// Which display the frame is rendered to.
    pub output_transform: AcesOutputTransform,
    /// Where 18% scene grey lands in output linear light, in percent.
    pub output_mid_gray: f32,
    /// The gamut compression strength.
    pub gamut_compress: f32,
    /// How out-of-gamut excursions are pulled back.
    pub gamut_compress_type: AcesGamutCompression,
    /// The cyan compression limit.
    pub cyan_limit: f32,
    /// The magenta compression limit.
    pub magenta_limit: f32,
    /// The yellow compression limit.
    pub yellow_limit: f32,
    /// The cyan compression threshold.
    pub cyan_threshold: f32,
    /// The magenta compression threshold.
    pub magenta_threshold: f32,
    /// The yellow compression threshold.
    pub yellow_threshold: f32,
    /// The highlight roll-off strength.
    pub rolloff: f32,
}

impl Default for AcesTransformParameters {
    fn default() -> Self {
        Self {
            aces_transform: ACES_TRANSFORM_DEFAULT_TRANSFORM,
            aces_version: AcesVersion::DEFAULT,
            input_transform: AcesInputTransform::DEFAULT,
            exposure_index: AcesExposureIndex::DEFAULT,
            output_transform: AcesOutputTransform::DEFAULT,
            output_mid_gray: ACES_TRANSFORM_DEFAULT_OUTPUT_MID_GRAY,
            gamut_compress: ACES_TRANSFORM_DEFAULT_GAMUT_COMPRESS,
            gamut_compress_type: AcesGamutCompression::DEFAULT,
            cyan_limit: ACES_TRANSFORM_DEFAULT_CYAN_LIMIT,
            magenta_limit: ACES_TRANSFORM_DEFAULT_MAGENTA_LIMIT,
            yellow_limit: ACES_TRANSFORM_DEFAULT_YELLOW_LIMIT,
            cyan_threshold: ACES_TRANSFORM_DEFAULT_CYAN_THRESHOLD,
            magenta_threshold: ACES_TRANSFORM_DEFAULT_MAGENTA_THRESHOLD,
            yellow_threshold: ACES_TRANSFORM_DEFAULT_YELLOW_THRESHOLD,
            rolloff: ACES_TRANSFORM_DEFAULT_ROLLOFF,
        }
    }
}

/// A parameter that is not finite or lies outside its documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("aces_transform.{parameter} must be finite and within its documented range")]
pub struct AcesTransformError {
    parameter: &'static str,
}

impl AcesTransformError {
    /// The parameter that failed validation.
    pub const fn parameter(&self) -> &'static str {
        self.parameter
    }
}

/// The four uniform cells the fused point pass reads.
pub type AcesTransformUniforms = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated ACES transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcesTransformGrade {
    parameters: AcesTransformParameters,
    exposure_scale: f32,
    mid_gray_gain: f32,
}

impl AcesTransformGrade {
    /// Validates the parameters and derives the two composed scalars.
    pub fn new(parameters: AcesTransformParameters) -> Result<Self, AcesTransformError> {
        for (name, value, minimum, maximum) in [
            ("aces_transform", parameters.aces_transform, 0.0, 1.0),
            (
                "output_mid_gray",
                parameters.output_mid_gray,
                ACES_TRANSFORM_MIN_MID_GRAY,
                ACES_TRANSFORM_MAX_MID_GRAY,
            ),
            ("gamut_compress", parameters.gamut_compress, 0.0, 1.0),
            (
                "cyan_limit",
                parameters.cyan_limit,
                ACES_TRANSFORM_MIN_LIMIT,
                ACES_TRANSFORM_MAX_LIMIT,
            ),
            (
                "magenta_limit",
                parameters.magenta_limit,
                ACES_TRANSFORM_MIN_LIMIT,
                ACES_TRANSFORM_MAX_LIMIT,
            ),
            (
                "yellow_limit",
                parameters.yellow_limit,
                ACES_TRANSFORM_MIN_LIMIT,
                ACES_TRANSFORM_MAX_LIMIT,
            ),
            (
                "cyan_threshold",
                parameters.cyan_threshold,
                ACES_TRANSFORM_MIN_THRESHOLD,
                ACES_TRANSFORM_MAX_THRESHOLD,
            ),
            (
                "magenta_threshold",
                parameters.magenta_threshold,
                ACES_TRANSFORM_MIN_THRESHOLD,
                ACES_TRANSFORM_MAX_THRESHOLD,
            ),
            (
                "yellow_threshold",
                parameters.yellow_threshold,
                ACES_TRANSFORM_MIN_THRESHOLD,
                ACES_TRANSFORM_MAX_THRESHOLD,
            ),
            (
                "rolloff",
                parameters.rolloff,
                ACES_TRANSFORM_MIN_ROLLOFF,
                ACES_TRANSFORM_MAX_ROLLOFF,
            ),
        ] {
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(AcesTransformError { parameter: name });
            }
        }
        let exposure_scale = ACES_TRANSFORM_REFERENCE_EXPOSURE_INDEX
            / parameters.exposure_index.value()
            * parameters.aces_version.exposure_scale();
        let anchor = tone_curve(
            ACES_TRANSFORM_MID_GRAY_REFERENCE * parameters.aces_version.exposure_scale(),
        )
        .max(IDENTITY_TOLERANCE);
        let mid_gray_gain = (parameters.output_mid_gray / 100.0) / anchor;
        Ok(Self { parameters, exposure_scale, mid_gray_gain })
    }

    /// The documented defaults, spelled out once for the schema and the tests.
    pub fn documented_defaults() -> Self {
        Self::new(AcesTransformParameters::default()).expect("the defaults are valid")
    }

    /// The authored parameters.
    pub const fn parameters(&self) -> &AcesTransformParameters {
        &self.parameters
    }

    /// The derived exposure gain, exposure index and version combined.
    pub const fn exposure_scale(&self) -> f32 {
        self.exposure_scale
    }

    /// The gain that anchors scene 18% grey onto the authored output mid grey.
    pub const fn mid_gray_gain(&self) -> f32 {
        self.mid_gray_gain
    }

    /// Whether the master switch leaves every pixel untouched.
    pub fn is_identity(&self) -> bool {
        self.parameters.aces_transform < 0.5
    }

    /// Relative cost of the fused point operation.
    pub const fn cost(&self) -> u32 {
        5
    }

    /// Packs the four uniform cells the fused point pass reads.
    pub fn gpu_controls(&self) -> AcesTransformUniforms {
        let p = &self.parameters;
        (
            [
                p.aces_transform,
                f32::from(p.aces_version.as_u8()),
                f32::from(p.input_transform.as_u8()),
                f32::from(p.output_transform.as_u8()),
            ],
            [
                self.exposure_scale,
                p.output_mid_gray,
                p.gamut_compress,
                f32::from(p.gamut_compress_type.as_u8()),
            ],
            [
                p.cyan_limit,
                p.magenta_limit,
                p.yellow_limit,
                p.cyan_threshold,
            ],
            [
                p.magenta_threshold,
                p.yellow_threshold,
                p.rolloff,
                self.mid_gray_gain,
            ],
        )
    }

    /// Hashes the authored configuration into the render op.
    pub fn hash_state(&self, state: &mut impl Hasher) {
        let p = &self.parameters;
        p.aces_version.as_u8().hash(state);
        p.input_transform.as_u8().hash(state);
        p.output_transform.as_u8().hash(state);
        p.exposure_index.as_u8().hash(state);
        p.gamut_compress_type.as_u8().hash(state);
        for value in [
            p.aces_transform,
            p.output_mid_gray,
            p.gamut_compress,
            p.cyan_limit,
            p.magenta_limit,
            p.yellow_limit,
            p.cyan_threshold,
            p.magenta_threshold,
            p.yellow_threshold,
            p.rolloff,
        ] {
            value.to_bits().hash(state);
        }
    }
}

/// Applies a matrix to an RGB triple.
fn multiply(matrix: &Matrix3, rgb: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * rgb[0] + matrix[0][1] * rgb[1] + matrix[0][2] * rgb[2],
        matrix[1][0] * rgb[0] + matrix[1][1] * rgb[1] + matrix[1][2] * rgb[2],
        matrix[2][0] * rgb[0] + matrix[2][1] * rgb[1] + matrix[2][2] * rgb[2],
    ]
}

/// Decodes one sRGB encoded channel.
fn decode_srgb(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Decodes one Rec.709 encoded channel.
fn decode_rec709(value: f32) -> f32 {
    if value < 0.081 {
        value / 4.5
    } else {
        ((value + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

/// Decodes one ACEScc channel.
///
/// ACEScc is logarithmic above the 2^-16 toe; values below that toe are clamped
/// onto it so the curve stays monotone without a second linear segment.
fn decode_acescc(value: f32) -> f32 {
    (value * 17.52 - 9.72).max(-16.0).exp2()
}

/// Decodes one ACEScct channel, including its documented toe.
fn decode_acescct(value: f32) -> f32 {
    if value <= 0.155_251_14 {
        (value - 0.072_905_53) / 10.540_237
    } else {
        (value * 17.52 - 9.72).exp2()
    }
}

/// Encodes one Rec.709 channel.
fn encode_rec709(value: f32) -> f32 {
    if value < 0.018 {
        value * 4.5
    } else {
        1.099 * value.powf(0.45) - 0.099
    }
}

/// Encodes one sRGB channel.
fn encode_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// Decodes the incoming frame into AP1 scene linear.
fn decode_input(rgb: [f32; 3], transform: AcesInputTransform) -> [f32; 3] {
    match transform {
        AcesInputTransform::None => rgb,
        AcesInputTransform::Srgb => multiply(
            &REC709_TO_AP1,
            [
                decode_srgb(rgb[0]),
                decode_srgb(rgb[1]),
                decode_srgb(rgb[2]),
            ],
        ),
        AcesInputTransform::Rec709 => multiply(
            &REC709_TO_AP1,
            [
                decode_rec709(rgb[0]),
                decode_rec709(rgb[1]),
                decode_rec709(rgb[2]),
            ],
        ),
        AcesInputTransform::Acescc => [
            decode_acescc(rgb[0]),
            decode_acescc(rgb[1]),
            decode_acescc(rgb[2]),
        ],
        AcesInputTransform::Acescct => [
            decode_acescct(rgb[0]),
            decode_acescct(rgb[1]),
            decode_acescct(rgb[2]),
        ],
    }
}

/// Encodes AP1 scene linear into the authored display space.
fn encode_output(rgb: [f32; 3], transform: AcesOutputTransform) -> [f32; 3] {
    let linear = match transform {
        AcesOutputTransform::Rec709 | AcesOutputTransform::Srgb => multiply(&AP1_TO_REC709, rgb),
        AcesOutputTransform::P3D65 => multiply(&AP1_TO_P3D65, rgb),
        AcesOutputTransform::Rec2020 => multiply(&AP1_TO_REC2020, rgb),
    };
    match transform {
        AcesOutputTransform::Srgb => [
            encode_srgb(linear[0]),
            encode_srgb(linear[1]),
            encode_srgb(linear[2]),
        ],
        AcesOutputTransform::Rec709 | AcesOutputTransform::P3D65 | AcesOutputTransform::Rec2020 => {
            [
                encode_rec709(linear[0]),
                encode_rec709(linear[1]),
                encode_rec709(linear[2]),
            ]
        }
    }
}

/// The excess a channel shows toward one of the three opposed hues.
fn hue_excess(rgb: [f32; 3], compression: AcesGamutCompression) -> [f32; 3] {
    let _ = compression;
    [
        rgb[1].min(rgb[2]) - rgb[0],
        rgb[0].min(rgb[2]) - rgb[1],
        rgb[0].min(rgb[1]) - rgb[2],
    ]
}

/// Pulls one excursion back toward its threshold and limit.
fn compress_excess(
    excess: f32,
    threshold: f32,
    limit: f32,
    strength: f32,
    kind: AcesGamutCompression,
) -> f32 {
    if excess <= threshold || kind == AcesGamutCompression::None {
        return excess;
    }
    let limit = limit.max(threshold);
    let target = match kind {
        AcesGamutCompression::None => excess,
        AcesGamutCompression::Clip => excess.min(limit),
        AcesGamutCompression::Aces => {
            let span = (limit - threshold).max(1.0e-4);
            threshold + span * ((excess - threshold) / span).tanh()
        }
    };
    excess + (target - excess) * strength
}

/// Compresses the three hue excursions toward the neutral axis.
fn compress_gamut(rgb: [f32; 3], parameters: &AcesTransformParameters) -> [f32; 3] {
    if parameters.gamut_compress <= 0.0
        || parameters.gamut_compress_type == AcesGamutCompression::None
    {
        return rgb;
    }
    let excess = hue_excess(rgb, parameters.gamut_compress_type);
    let limits = [
        parameters.cyan_limit,
        parameters.magenta_limit,
        parameters.yellow_limit,
    ];
    let thresholds = [
        parameters.cyan_threshold,
        parameters.magenta_threshold,
        parameters.yellow_threshold,
    ];
    let mut result = rgb;
    for channel in 0..3 {
        let compressed = compress_excess(
            excess[channel],
            thresholds[channel],
            limits[channel],
            parameters.gamut_compress,
            parameters.gamut_compress_type,
        );
        if compressed < excess[channel] {
            result[channel] += excess[channel] - compressed;
        }
    }
    result
}

/// The highlight roll-off applied before the tone curve.
fn soft_clip(value: f32, rolloff: f32) -> f32 {
    if value <= 1.0 || rolloff <= 0.0 {
        return value;
    }
    let over = value - 1.0;
    1.0 + over / (1.0 + rolloff * over)
}

/// The fitted RRT/ODT tone curve.
fn tone_curve(value: f32) -> f32 {
    let value = value.max(0.0);
    let numerator = value * (value + 0.024_578_6) - 0.000_090_537;
    let denominator = value * (0.983_729 * value + 0.432_951) + 0.238_081;
    (numerator / denominator).max(0.0)
}

/// Transforms one pixel in place.
fn transform_pixel(pixel: &mut [f32; 4], grade: &AcesTransformGrade) {
    let p = &grade.parameters;
    if p.aces_transform < 0.5 {
        return;
    }
    let mut rgb = decode_input([pixel[0], pixel[1], pixel[2]], p.input_transform);
    let scale = grade.exposure_scale;
    rgb = [rgb[0] * scale, rgb[1] * scale, rgb[2] * scale];
    rgb = compress_gamut(rgb, p);
    rgb = [
        tone_curve(soft_clip(rgb[0], p.rolloff)) * grade.mid_gray_gain,
        tone_curve(soft_clip(rgb[1], p.rolloff)) * grade.mid_gray_gain,
        tone_curve(soft_clip(rgb[2], p.rolloff)) * grade.mid_gray_gain,
    ];
    rgb = encode_output(rgb, p.output_transform);
    pixel[0] = rgb[0];
    pixel[1] = rgb[1];
    pixel[2] = rgb[2];
}

/// Transforms a whole frame in place.
pub fn aces_transform_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &AcesTransformGrade,
) -> bool {
    apply_aces_transform_f32_controlled(pixels, width, height, grade, &mut || Ok(()))
        .unwrap_or_else(|error: std::convert::Infallible| match error {})
}

/// Transforms a whole frame in place, honouring a cancellation checkpoint.
pub(crate) fn apply_aces_transform_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &AcesTransformGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    let parameters = grade.parameters;
    if ![
        parameters.aces_transform,
        parameters.output_mid_gray,
        parameters.gamut_compress,
        parameters.cyan_limit,
        parameters.magenta_limit,
        parameters.yellow_limit,
        parameters.cyan_threshold,
        parameters.magenta_threshold,
        parameters.yellow_threshold,
        parameters.rolloff,
    ]
    .into_iter()
    .all(f32::is_finite)
    {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(false);
    }
    for pixel in working.iter_mut().take(pixel_count) {
        transform_pixel(pixel, grade);
    }
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(parameters: AcesTransformParameters) -> AcesTransformGrade {
        AcesTransformGrade::new(parameters).expect("valid grade")
    }

    fn default_grade() -> AcesTransformGrade {
        AcesTransformGrade::documented_defaults()
    }

    fn single(colour: [f32; 4]) -> [f32; 4] {
        let mut pixels = [colour];
        assert!(aces_transform_frame_f32(
            &mut pixels,
            1,
            1,
            &default_grade()
        ));
        pixels[0]
    }

    fn with(parameters: AcesTransformParameters, colour: [f32; 4]) -> [f32; 4] {
        let mut pixels = [colour];
        aces_transform_frame_f32(&mut pixels, 1, 1, &grade(parameters));
        pixels[0]
    }

    fn hash_of(parameters: AcesTransformParameters) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        grade(parameters).hash_state(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        for (parameter, parameters) in [
            (
                "aces_transform",
                AcesTransformParameters {
                    aces_transform: 1.5,
                    ..AcesTransformParameters::default()
                },
            ),
            (
                "output_mid_gray",
                AcesTransformParameters {
                    output_mid_gray: -1.0,
                    ..AcesTransformParameters::default()
                },
            ),
            (
                "gamut_compress",
                AcesTransformParameters {
                    gamut_compress: 2.0,
                    ..AcesTransformParameters::default()
                },
            ),
            (
                "cyan_limit",
                AcesTransformParameters {
                    cyan_limit: 5.0,
                    ..AcesTransformParameters::default()
                },
            ),
            (
                "magenta_threshold",
                AcesTransformParameters {
                    magenta_threshold: f32::NAN,
                    ..AcesTransformParameters::default()
                },
            ),
            (
                "rolloff",
                AcesTransformParameters {
                    rolloff: 11.0,
                    ..AcesTransformParameters::default()
                },
            ),
        ] {
            let error = AcesTransformGrade::new(parameters).expect_err("rejects");
            assert_eq!(error.parameter(), parameter);
        }
    }

    #[test]
    fn documented_defaults_are_stable() {
        let grade = default_grade();
        let parameters = grade.parameters();
        assert_eq!(parameters.aces_transform, 1.0);
        assert_eq!(parameters.aces_version, AcesVersion::Aces20);
        assert_eq!(parameters.input_transform, AcesInputTransform::None);
        assert_eq!(parameters.exposure_index, AcesExposureIndex::E800);
        assert_eq!(parameters.output_transform, AcesOutputTransform::Rec709);
        assert_eq!(parameters.output_mid_gray, 15.0);
        assert_eq!(parameters.gamut_compress, 1.0);
        assert_eq!(parameters.gamut_compress_type, AcesGamutCompression::None);
        assert_eq!(parameters.cyan_limit, 1.147);
        assert_eq!(parameters.magenta_limit, 1.264);
        assert_eq!(parameters.yellow_limit, 1.312);
        assert_eq!(parameters.cyan_threshold, 0.815);
        assert_eq!(parameters.magenta_threshold, 0.803);
        assert_eq!(parameters.yellow_threshold, 0.88);
        assert_eq!(parameters.rolloff, 1.2);
        assert_eq!(grade.cost(), 5);
        assert!(!grade.is_identity());
        assert!((grade.exposure_scale() - 1.05).abs() < 1.0e-6);
        let anchor = tone_curve(0.18 * 1.05);
        assert!((grade.mid_gray_gain() - 0.15 / anchor).abs() < 1.0e-6);
        assert!((grade.mid_gray_gain() - 1.322_3).abs() < 1.0e-2);
    }

    #[test]
    fn menu_keys_round_trip() {
        for item in AcesVersion::ALL {
            assert_eq!(AcesVersion::from_key(item.as_key()), Some(item));
            assert_eq!(item == AcesVersion::DEFAULT, item == AcesVersion::Aces20);
        }
        for item in AcesInputTransform::ALL {
            assert_eq!(AcesInputTransform::from_key(item.as_key()), Some(item));
        }
        for item in AcesOutputTransform::ALL {
            assert_eq!(AcesOutputTransform::from_key(item.as_key()), Some(item));
        }
        for item in AcesExposureIndex::ALL {
            assert_eq!(AcesExposureIndex::from_key(item.as_key()), Some(item));
        }
        for item in AcesGamutCompression::ALL {
            assert_eq!(AcesGamutCompression::from_key(item.as_key()), Some(item));
        }
        assert_eq!(AcesVersion::from_key("unknown"), None);
        assert_eq!(AcesInputTransform::from_key(""), None);
        assert_eq!(AcesExposureIndex::E800.value(), 800.0);
    }

    #[test]
    fn uniform_cells_follow_the_documented_layout() {
        let grade = grade(AcesTransformParameters {
            aces_version: AcesVersion::Aces10,
            input_transform: AcesInputTransform::Srgb,
            exposure_index: AcesExposureIndex::E1600,
            output_transform: AcesOutputTransform::P3D65,
            gamut_compress_type: AcesGamutCompression::Clip,
            ..AcesTransformParameters::default()
        });
        let (params, colour, extra0, extra1) = grade.gpu_controls();
        assert_eq!(params, [1.0, 0.0, 1.0, 2.0]);
        assert_eq!(colour[0], 0.5);
        assert_eq!(colour[1], 15.0);
        assert_eq!(colour[2], 1.0);
        assert_eq!(colour[3], 2.0);
        assert_eq!(extra0, [1.147, 1.264, 1.312, 0.815]);
        assert_eq!(extra1[0], 0.803);
        assert_eq!(extra1[1], 0.88);
        assert_eq!(extra1[2], 1.2);
        assert_eq!(extra1[3], grade.mid_gray_gain());
    }

    #[test]
    fn master_switch_off_is_identity() {
        let parameters = AcesTransformParameters {
            aces_transform: 0.0,
            ..AcesTransformParameters::default()
        };
        assert!(grade(parameters).is_identity());
        let mut pixels = [[0.2, 0.4, 0.6, 0.8]];
        let before = pixels;
        assert!(!aces_transform_frame_f32(
            &mut pixels,
            1,
            1,
            &grade(parameters)
        ));
        assert_eq!(pixels, before);
    }

    #[test]
    fn neutral_grey_stays_neutral() {
        let pixel = single([0.18, 0.18, 0.18, 1.0]);
        assert!((pixel[0] - pixel[1]).abs() < 1.0e-5);
        assert!((pixel[1] - pixel[2]).abs() < 1.0e-5);
        assert!((pixel[3] - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn higher_exposure_index_darkens_the_frame() {
        let neutral = AcesExposureIndex::E800;
        let darker = with(
            AcesTransformParameters {
                exposure_index: AcesExposureIndex::E3200,
                ..AcesTransformParameters::default()
            },
            [0.18, 0.18, 0.18, 1.0],
        );
        let reference = with(
            AcesTransformParameters {
                exposure_index: neutral,
                ..AcesTransformParameters::default()
            },
            [0.18, 0.18, 0.18, 1.0],
        );
        assert!(darker[1] < reference[1] - 1.0e-3);
    }

    #[test]
    fn lower_exposure_index_brightens_the_frame() {
        let brighter = with(
            AcesTransformParameters {
                exposure_index: AcesExposureIndex::E200,
                ..AcesTransformParameters::default()
            },
            [0.18, 0.18, 0.18, 1.0],
        );
        let reference = single([0.18, 0.18, 0.18, 1.0]);
        assert!(brighter[1] > reference[1] + 1.0e-3);
    }

    #[test]
    fn srgb_input_decodes_into_the_linear_work_space() {
        let encoded = encode_srgb(0.18);
        let linear = decode_srgb(encoded);
        assert!((linear - 0.18).abs() < 2.0e-4);
        assert!((encoded - 0.18).abs() > 1.0e-2);
        let from_srgb = with(
            AcesTransformParameters {
                input_transform: AcesInputTransform::Srgb,
                ..AcesTransformParameters::default()
            },
            [encode_srgb(0.18), encode_srgb(0.18), encode_srgb(0.18), 1.0],
        );
        let reference = single([0.18, 0.18, 0.18, 1.0]);
        assert!((from_srgb[1] - reference[1]).abs() < 2.0e-4);
    }

    #[test]
    fn acescc_and_acescct_decode_their_documented_curves() {
        assert!((decode_acescc(0.413_737) - 0.18).abs() < 5.0e-4);
        let cct = 10.540_237 * 0.003 + 0.072_905_53;
        assert!((decode_acescct(cct) - 0.003).abs() < 5.0e-5);
        assert!((decode_acescct(cct) - decode_acescc(cct)).abs() > 1.0e-3);
    }

    #[test]
    fn output_transforms_rotate_the_primaries() {
        let colour = [0.9, 0.05, 0.05, 1.0];
        let rec709 = single(colour);
        let rec2020 = with(
            AcesTransformParameters {
                output_transform: AcesOutputTransform::Rec2020,
                ..AcesTransformParameters::default()
            },
            colour,
        );
        let p3 = with(
            AcesTransformParameters {
                output_transform: AcesOutputTransform::P3D65,
                ..AcesTransformParameters::default()
            },
            colour,
        );
        assert!((rec709[0] - rec2020[0]).abs() > 1.0e-2);
        assert!((rec709[0] - p3[0]).abs() > 1.0e-2);
        assert!((rec709[1] - rec2020[1]).abs() > 1.0e-3);
    }

    #[test]
    fn aces_gamut_compression_reduces_cyan_excess() {
        let parameters = AcesTransformParameters {
            gamut_compress_type: AcesGamutCompression::Aces,
            cyan_threshold: 0.2,
            cyan_limit: 0.4,
            ..AcesTransformParameters::default()
        };
        let grade = grade(parameters);
        let start = [0.05, 0.9, 0.9];
        let compressed = compress_gamut(start, grade.parameters());
        let before = start[1].min(start[2]) - start[0];
        let after = compressed[1].min(compressed[2]) - compressed[0];
        assert!(after < before);
        assert!(after <= 0.4 + 1.0e-4);
        assert!(compressed[0] > start[0]);
        assert!((compressed[1] - start[1]).abs() < 1.0e-6);
        assert!((compressed[2] - start[2]).abs() < 1.0e-6);
    }

    #[test]
    fn clip_gamut_compression_limits_at_the_limit() {
        let parameters = AcesTransformParameters {
            gamut_compress_type: AcesGamutCompression::Clip,
            cyan_threshold: 0.2,
            cyan_limit: 0.4,
            ..AcesTransformParameters::default()
        };
        let grade = grade(parameters);
        let compressed = compress_gamut([0.05, 0.9, 0.9], grade.parameters());
        let after = compressed[1].min(compressed[2]) - compressed[0];
        assert!((after - 0.4).abs() < 1.0e-5);
        assert!((compressed[0] - 0.5).abs() < 1.0e-5);
    }

    #[test]
    fn rolloff_compresses_highlights() {
        assert!((soft_clip(0.5, 1.2) - 0.5).abs() < 1.0e-6);
        let clipped = soft_clip(4.0, 1.2);
        assert!(clipped < 4.0);
        assert!((clipped - (1.0 + 3.0 / 4.6)).abs() < 1.0e-5);
        assert!((soft_clip(4.0, 0.0) - 4.0).abs() < 1.0e-6);
    }

    #[test]
    fn mid_gray_anchors_eighteen_percent() {
        let pixel = single([0.18, 0.18, 0.18, 1.0]);
        assert!((pixel[1] - encode_rec709(0.15)).abs() < 1.0e-4);
        let dark = with(
            AcesTransformParameters {
                output_mid_gray: 7.2,
                ..AcesTransformParameters::default()
            },
            [0.18, 0.18, 0.18, 1.0],
        );
        assert!((dark[1] - encode_rec709(0.072)).abs() < 1.0e-4);
    }

    #[test]
    fn alpha_is_untouched() {
        let pixel = single([0.4, 0.2, 0.1, 0.37]);
        assert!((pixel[3] - 0.37).abs() < 1.0e-6);
    }

    #[test]
    fn version_scale_brightens_the_aces_2_0_path() {
        let colour = [0.05, 0.05, 0.05, 1.0];
        let one = with(
            AcesTransformParameters {
                aces_version: AcesVersion::Aces10,
                ..AcesTransformParameters::default()
            },
            colour,
        );
        let two = with(
            AcesTransformParameters {
                aces_version: AcesVersion::Aces20,
                ..AcesTransformParameters::default()
            },
            colour,
        );
        assert!(two[1] > one[1]);
    }

    #[test]
    fn invalid_frame_shapes_are_ignored() {
        let mut pixels = [[0.2, 0.3, 0.4, 1.0]; 2];
        let before = pixels;
        assert!(!aces_transform_frame_f32(
            &mut pixels,
            3,
            1,
            &default_grade()
        ));
        assert!(!aces_transform_frame_f32(
            &mut pixels,
            0,
            0,
            &default_grade()
        ));
        assert_eq!(pixels, before);
    }

    #[test]
    fn hashes_separate_every_menu_and_value() {
        let base = AcesTransformParameters::default();
        let variants = vec![
            AcesTransformParameters { aces_version: AcesVersion::Aces10, ..base },
            AcesTransformParameters {
                input_transform: AcesInputTransform::Acescct,
                ..base
            },
            AcesTransformParameters {
                output_transform: AcesOutputTransform::Rec2020,
                ..base
            },
            AcesTransformParameters {
                gamut_compress_type: AcesGamutCompression::Clip,
                ..base
            },
            AcesTransformParameters { rolloff: 3.0, ..base },
        ];
        let mut seen = std::collections::HashSet::new();
        assert!(seen.insert(hash_of(base)));
        for variant in variants {
            assert!(seen.insert(hash_of(variant)));
        }
    }

    #[test]
    fn same_parameters_produce_the_same_bytes() {
        let mut first = vec![[0.1, 0.2, 0.3, 1.0]; 5];
        let mut second = first.clone();
        assert!(aces_transform_frame_f32(&mut first, 5, 1, &default_grade()));
        assert!(aces_transform_frame_f32(
            &mut second,
            5,
            1,
            &default_grade()
        ));
        assert_eq!(first, second);
        assert!(first.iter().all(|pixel| pixel[0].is_finite()));
    }
}
