//! Backend-neutral primary color-grade mathematics.
//!
//! Author controls are compiled once per evaluated Effect graph into small,
//! validated values. CPU and GPU backends then consume the same matrices and
//! parameter vectors without reinterpreting UI ranges or working-space
//! chromaticities per pixel.

use mondrian_core::WorkingColorSpace;

const IDENTITY_EPSILON: f32 = 1.0e-6;
const WHITE_BALANCE_MIRED_SPAN: f64 = 100.0;
const WHITE_BALANCE_TINT_UV_SPAN: f64 = 0.025;
const ASC_CDL_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Invalid authored primary-grade controls.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrimaryGradeError {
    /// One scalar or vector component was non-finite or outside its semantic domain.
    #[error("invalid primary-grade control `{control}`")]
    InvalidControl { control: &'static str },
    /// A chromatic-adaptation matrix could not be derived from valid controls.
    #[error("white-balance chromatic adaptation is numerically singular")]
    SingularWhiteBalance,
}

/// A working-space RGB matrix compiled from creative temperature and tint.
///
/// Temperature is normalized to `[-1, 1]` and spans ±100 mired around the
/// working space's native white. Tint is normalized to `[-1, 1]` and moves
/// perpendicular to the Planckian locus in CIE 1960 UCS. Bradford adaptation
/// then maps the native white to that authored white without clipping
/// scene-linear or HDR values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WhiteBalanceGrade {
    matrix: [[f32; 3]; 3],
}

impl WhiteBalanceGrade {
    /// Compile normalized creative controls for one exact working space.
    pub fn new(
        temperature: f32,
        tint: f32,
        working_color_space: WorkingColorSpace,
    ) -> Result<Self, PrimaryGradeError> {
        if !temperature.is_finite() || !(-1.0..=1.0).contains(&temperature) {
            return Err(PrimaryGradeError::InvalidControl { control: "temperature" });
        }
        if !tint.is_finite() || !(-1.0..=1.0).contains(&tint) {
            return Err(PrimaryGradeError::InvalidControl { control: "tint" });
        }
        if temperature.abs() <= IDENTITY_EPSILON && tint.abs() <= IDENTITY_EPSILON {
            return Ok(Self::identity());
        }

        let colorimetry = working_colorimetry(working_color_space);
        let base_mired = 1_000_000.0 / colorimetry.nominal_white_kelvin;
        let target_mired = base_mired + f64::from(temperature) * WHITE_BALANCE_MIRED_SPAN;
        if !(40.0..=400.0).contains(&target_mired) {
            return Err(PrimaryGradeError::InvalidControl { control: "temperature" });
        }
        let target_kelvin = 1_000_000.0 / target_mired;
        let base_planck_uv = xy_to_uv(planckian_xy(colorimetry.nominal_white_kelvin))
            .ok_or(PrimaryGradeError::SingularWhiteBalance)?;
        let target_planck_uv =
            xy_to_uv(planckian_xy(target_kelvin)).ok_or(PrimaryGradeError::SingularWhiteBalance)?;
        let base_uv =
            xy_to_uv(colorimetry.white_xy).ok_or(PrimaryGradeError::SingularWhiteBalance)?;

        let warmer_uv = xy_to_uv(planckian_xy(1_000_000.0 / (base_mired + 1.0)))
            .ok_or(PrimaryGradeError::SingularWhiteBalance)?;
        let cooler_uv = xy_to_uv(planckian_xy(1_000_000.0 / (base_mired - 1.0)))
            .ok_or(PrimaryGradeError::SingularWhiteBalance)?;
        let tangent = normalize2([warmer_uv[0] - cooler_uv[0], warmer_uv[1] - cooler_uv[1]])
            .ok_or(PrimaryGradeError::SingularWhiteBalance)?;
        // This orientation makes positive tint move toward magenta rather than green.
        let tint_axis = [tangent[1], -tangent[0]];
        let target_uv = [
            base_uv[0] + target_planck_uv[0] - base_planck_uv[0]
                + tint_axis[0] * f64::from(tint) * WHITE_BALANCE_TINT_UV_SPAN,
            base_uv[1] + target_planck_uv[1] - base_planck_uv[1]
                + tint_axis[1] * f64::from(tint) * WHITE_BALANCE_TINT_UV_SPAN,
        ];
        let target_xy = uv_to_xy(target_uv).ok_or(PrimaryGradeError::SingularWhiteBalance)?;
        let source_xyz =
            xy_to_xyz(colorimetry.white_xy).ok_or(PrimaryGradeError::SingularWhiteBalance)?;
        let target_xyz = xy_to_xyz(target_xy).ok_or(PrimaryGradeError::SingularWhiteBalance)?;
        let source_lms = mul3_vec(BRADFORD_XYZ_TO_LMS, source_xyz);
        let target_lms = mul3_vec(BRADFORD_XYZ_TO_LMS, target_xyz);
        if source_lms.iter().any(|value| value.abs() <= f64::EPSILON) {
            return Err(PrimaryGradeError::SingularWhiteBalance);
        }
        let adaptation_lms = [
            [target_lms[0] / source_lms[0], 0.0, 0.0],
            [0.0, target_lms[1] / source_lms[1], 0.0],
            [0.0, 0.0, target_lms[2] / source_lms[2]],
        ];
        let xyz_adaptation = mul3(
            BRADFORD_LMS_TO_XYZ,
            mul3(adaptation_lms, BRADFORD_XYZ_TO_LMS),
        );
        let rgb_adaptation = mul3(
            colorimetry.xyz_to_rgb,
            mul3(xyz_adaptation, colorimetry.rgb_to_xyz),
        );
        if rgb_adaptation.iter().flatten().any(|value| !value.is_finite()) {
            return Err(PrimaryGradeError::SingularWhiteBalance);
        }
        Ok(Self {
            matrix: rgb_adaptation.map(|row| row.map(|value| value as f32)),
        })
    }

    /// Exact identity grade.
    pub const fn identity() -> Self {
        Self {
            matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    /// Return the row-major working-RGB adaptation matrix.
    pub const fn matrix(self) -> [[f32; 3]; 3] {
        self.matrix
    }

    /// Apply the compiled matrix without clamping extended values.
    #[inline]
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        mul3_vec_f32(self.matrix, rgb)
    }

    /// Whether the compiled transform is effectively identity.
    pub fn is_identity(self) -> bool {
        matrix_near(self.matrix, Self::identity().matrix, IDENTITY_EPSILON)
    }
}

/// Compiled Lift/Gamma/Gain/Offset primary correction.
///
/// Authored Lift remains centered at `[1, 1, 1]` to preserve Mondrian's
/// existing stable `builtin.color_wheel` project representation. Internally it
/// becomes a signed shadow delta. Lift preserves code value 1, Gain scales the
/// lifted signal, Gamma is a signed power around zero, and Offset is applied
/// last as a whole-signal printer-light adjustment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrimariesGrade {
    offset: [f32; 3],
    lift_delta: [f32; 3],
    inverse_gamma: [f32; 3],
    gain: [f32; 3],
}

impl PrimariesGrade {
    /// Compile one set of primary controls.
    pub fn new(
        offset: [f32; 3],
        lift: [f32; 3],
        gamma: [f32; 3],
        gain: [f32; 3],
    ) -> Result<Self, PrimaryGradeError> {
        validate_vec3("offset", offset)?;
        validate_vec3("lift", lift)?;
        validate_vec3("gamma", gamma)?;
        validate_vec3("gain", gain)?;
        if gamma.into_iter().any(|value| value <= 0.0) {
            return Err(PrimaryGradeError::InvalidControl { control: "gamma" });
        }
        Ok(Self {
            offset,
            lift_delta: lift.map(|value| value - 1.0),
            inverse_gamma: gamma.map(|value| value.recip()),
            gain,
        })
    }

    /// Return the final additive offset.
    pub const fn offset(self) -> [f32; 3] {
        self.offset
    }

    /// Return the additive shadow delta derived from authored Lift.
    pub const fn lift_delta(self) -> [f32; 3] {
        self.lift_delta
    }

    /// Return reciprocal Gamma used by the pixel kernel.
    pub const fn inverse_gamma(self) -> [f32; 3] {
        self.inverse_gamma
    }

    /// Return Gain used by the pixel kernel.
    pub const fn gain(self) -> [f32; 3] {
        self.gain
    }

    /// Apply the correction without clipping scene-linear extended values.
    #[inline]
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|channel| {
            let lifted = rgb[channel] + self.lift_delta[channel] * (1.0 - rgb[channel]);
            signed_pow(lifted * self.gain[channel], self.inverse_gamma[channel])
                + self.offset[channel]
        })
    }

    /// Whether every primary control is neutral.
    pub fn is_identity(self) -> bool {
        vec_near(self.offset, [0.0; 3], IDENTITY_EPSILON)
            && vec_near(self.lift_delta, [0.0; 3], IDENTITY_EPSILON)
            && vec_near(self.inverse_gamma, [1.0; 3], IDENTITY_EPSILON)
            && vec_near(self.gain, [1.0; 3], IDENTITY_EPSILON)
    }
}

/// Compiled ASC CDL v1.2 no-clamp SOP/Saturation correction.
///
/// SOP evaluates `max(input * slope + offset, 0) ^ power`, retaining positive
/// HDR values above 1. Saturation uses the standard ASC CDL Rec.709 luma
/// coefficients, independent of the Sequence working primaries, so imported
/// CDL values remain cross-application deterministic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AscCdlGrade {
    slope: [f32; 3],
    offset: [f32; 3],
    power: [f32; 3],
    saturation: f32,
}

impl AscCdlGrade {
    /// Compile one no-clamp ASC CDL correction.
    pub fn new(
        slope: [f32; 3],
        offset: [f32; 3],
        power: [f32; 3],
        saturation: f32,
    ) -> Result<Self, PrimaryGradeError> {
        validate_vec3("slope", slope)?;
        validate_vec3("offset", offset)?;
        validate_vec3("power", power)?;
        if power.into_iter().any(|value| value <= 0.0) {
            return Err(PrimaryGradeError::InvalidControl { control: "power" });
        }
        if !saturation.is_finite() || saturation < 0.0 {
            return Err(PrimaryGradeError::InvalidControl { control: "saturation" });
        }
        Ok(Self { slope, offset, power, saturation })
    }

    /// Return the ASC slope vector.
    pub const fn slope(self) -> [f32; 3] {
        self.slope
    }

    /// Return the ASC offset vector.
    pub const fn offset(self) -> [f32; 3] {
        self.offset
    }

    /// Return the ASC power vector.
    pub const fn power(self) -> [f32; 3] {
        self.power
    }

    /// Return the ASC saturation scalar.
    pub const fn saturation(self) -> f32 {
        self.saturation
    }

    /// Apply standard no-clamp SOP followed by saturation.
    #[inline]
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        let sop = std::array::from_fn(|channel| {
            (rgb[channel] * self.slope[channel] + self.offset[channel])
                .max(0.0)
                .powf(self.power[channel])
        });
        let luma = dot3(sop, ASC_CDL_LUMA);
        std::array::from_fn(|channel| luma + (sop[channel] - luma) * self.saturation)
    }

    /// Whether SOP and saturation are neutral.
    pub fn is_identity(self) -> bool {
        vec_near(self.slope, [1.0; 3], IDENTITY_EPSILON)
            && vec_near(self.offset, [0.0; 3], IDENTITY_EPSILON)
            && vec_near(self.power, [1.0; 3], IDENTITY_EPSILON)
            && (self.saturation - 1.0).abs() <= IDENTITY_EPSILON
    }
}

#[derive(Clone, Copy)]
struct WorkingColorimetry {
    rgb_to_xyz: [[f64; 3]; 3],
    xyz_to_rgb: [[f64; 3]; 3],
    white_xy: [f64; 2],
    nominal_white_kelvin: f64,
}

fn working_colorimetry(space: WorkingColorSpace) -> WorkingColorimetry {
    match space {
        WorkingColorSpace::LinearRec709 => WorkingColorimetry {
            rgb_to_xyz: REC709_TO_XYZ_D65,
            xyz_to_rgb: XYZ_D65_TO_REC709,
            white_xy: D65_XY,
            nominal_white_kelvin: 6504.0,
        },
        WorkingColorSpace::LinearRec2020 => WorkingColorimetry {
            rgb_to_xyz: REC2020_TO_XYZ_D65,
            xyz_to_rgb: XYZ_D65_TO_REC2020,
            white_xy: D65_XY,
            nominal_white_kelvin: 6504.0,
        },
        WorkingColorSpace::LinearP3D65 => WorkingColorimetry {
            rgb_to_xyz: P3_D65_TO_XYZ_D65,
            xyz_to_rgb: XYZ_D65_TO_P3_D65,
            white_xy: D65_XY,
            nominal_white_kelvin: 6504.0,
        },
        WorkingColorSpace::AcesCg => WorkingColorimetry {
            rgb_to_xyz: ACESCG_TO_XYZ_D60,
            xyz_to_rgb: XYZ_D60_TO_ACESCG,
            white_xy: D60_XY,
            nominal_white_kelvin: 6000.0,
        },
    }
}

fn validate_vec3(control: &'static str, value: [f32; 3]) -> Result<(), PrimaryGradeError> {
    if value.into_iter().all(f32::is_finite) {
        Ok(())
    } else {
        Err(PrimaryGradeError::InvalidControl { control })
    }
}

#[inline]
fn signed_pow(value: f32, exponent: f32) -> f32 {
    value.abs().powf(exponent).copysign(value)
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn vec_near(a: [f32; 3], b: [f32; 3], epsilon: f32) -> bool {
    a.into_iter().zip(b).all(|(left, right)| (left - right).abs() <= epsilon)
}

fn matrix_near(a: [[f32; 3]; 3], b: [[f32; 3]; 3], epsilon: f32) -> bool {
    a.into_iter()
        .flatten()
        .zip(b.into_iter().flatten())
        .all(|(left, right)| (left - right).abs() <= epsilon)
}

fn normalize2(value: [f64; 2]) -> Option<[f64; 2]> {
    let length = value[0].hypot(value[1]);
    (length.is_finite() && length > f64::EPSILON).then(|| [value[0] / length, value[1] / length])
}

fn planckian_xy(kelvin: f64) -> [f64; 2] {
    let temperature = kelvin.clamp(1667.0, 25_000.0);
    let x = if temperature <= 4000.0 {
        -0.266_123_9e9 / temperature.powi(3) - 0.234_358_0e6 / temperature.powi(2)
            + 0.877_695_6e3 / temperature
            + 0.179_910
    } else {
        -3.025_846_9e9 / temperature.powi(3)
            + 2.107_037_9e6 / temperature.powi(2)
            + 0.222_634_7e3 / temperature
            + 0.240_390
    };
    let y = if temperature <= 2222.0 {
        -1.106_381_4 * x.powi(3) - 1.348_110_20 * x.powi(2) + 2.185_558_32 * x - 0.202_196_83
    } else if temperature <= 4000.0 {
        -0.954_947_6 * x.powi(3) - 1.374_185_93 * x.powi(2) + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758_0 * x.powi(3) - 5.873_386_70 * x.powi(2) + 3.751_129_97 * x - 0.370_014_83
    };
    [x, y]
}

fn xy_to_uv(xy: [f64; 2]) -> Option<[f64; 2]> {
    let denominator = -2.0 * xy[0] + 12.0 * xy[1] + 3.0;
    (denominator.is_finite() && denominator.abs() > f64::EPSILON)
        .then(|| [4.0 * xy[0] / denominator, 6.0 * xy[1] / denominator])
}

fn uv_to_xy(uv: [f64; 2]) -> Option<[f64; 2]> {
    let denominator = 2.0 * uv[0] - 8.0 * uv[1] + 4.0;
    if !denominator.is_finite() || denominator.abs() <= f64::EPSILON {
        return None;
    }
    let xy = [3.0 * uv[0] / denominator, 2.0 * uv[1] / denominator];
    (xy[0].is_finite() && xy[1].is_finite() && xy[1] > 0.0).then_some(xy)
}

fn xy_to_xyz(xy: [f64; 2]) -> Option<[f64; 3]> {
    (xy[1].is_finite() && xy[1] > f64::EPSILON)
        .then(|| [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]])
}

fn mul3(left: [[f64; 3]; 3], right: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            (0..3).map(|index| left[row][index] * right[index][column]).sum()
        })
    })
}

fn mul3_vec(matrix: [[f64; 3]; 3], value: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|row| {
        matrix[row][0] * value[0] + matrix[row][1] * value[1] + matrix[row][2] * value[2]
    })
}

#[inline]
fn mul3_vec_f32(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| {
        matrix[row][0] * value[0] + matrix[row][1] * value[1] + matrix[row][2] * value[2]
    })
}

const D65_XY: [f64; 2] = [0.3127, 0.3290];
const D60_XY: [f64; 2] = [0.32168, 0.33767];
const BRADFORD_XYZ_TO_LMS: [[f64; 3]; 3] = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];
const BRADFORD_LMS_TO_XYZ: [[f64; 3]; 3] = [
    [0.986_992_9, -0.147_054_3, 0.159_962_7],
    [0.432_305_3, 0.518_360_3, 0.049_291_2],
    [-0.008_528_7, 0.040_042_8, 0.968_486_7],
];
const REC709_TO_XYZ_D65: [[f64; 3]; 3] = [
    [0.412_390_799, 0.357_584_339, 0.180_480_788],
    [0.212_639_006, 0.715_168_679, 0.072_192_315],
    [0.019_330_819, 0.119_194_780, 0.950_532_152],
];
const XYZ_D65_TO_REC709: [[f64; 3]; 3] = [
    [3.240_969_942, -1.537_383_178, -0.498_610_760],
    [-0.969_243_636, 1.875_967_502, 0.041_555_057],
    [0.055_630_080, -0.203_976_959, 1.056_971_514],
];
const REC2020_TO_XYZ_D65: [[f64; 3]; 3] = [
    [0.636_958_048, 0.144_616_904, 0.168_880_975],
    [0.262_700_212, 0.677_998_072, 0.059_301_716],
    [0.0, 0.028_072_693, 1.060_985_058],
];
const XYZ_D65_TO_REC2020: [[f64; 3]; 3] = [
    [1.716_651_188, -0.355_670_784, -0.253_366_281],
    [-0.666_684_352, 1.616_481_237, 0.015_768_546],
    [0.017_639_857, -0.042_770_613, 0.942_103_121],
];
const P3_D65_TO_XYZ_D65: [[f64; 3]; 3] = [
    [0.486_570_949, 0.265_667_694, 0.198_217_285],
    [0.228_974_564, 0.691_738_522, 0.079_286_914],
    [0.0, 0.045_113_382, 1.043_944_369],
];
const XYZ_D65_TO_P3_D65: [[f64; 3]; 3] = [
    [2.493_496_912, -0.931_383_618, -0.402_710_784],
    [-0.829_488_970, 1.762_664_060, 0.023_624_686],
    [0.035_845_830, -0.076_172_390, 0.956_884_520],
];
const ACESCG_TO_XYZ_D60: [[f64; 3]; 3] = [
    [0.662_454_181, 0.134_004_207, 0.156_187_688],
    [0.272_228_717, 0.674_081_766, 0.053_689_517],
    [-0.005_574_650, 0.004_060_733, 1.010_339_100],
];
const XYZ_D60_TO_ACESCG: [[f64; 3]; 3] = [
    [1.641_023_379, -0.324_803_294, -0.236_424_696],
    [-0.663_662_859, 1.615_331_592, 0.016_756_348],
    [0.011_721_894, -0.008_284_442, 0.988_394_859],
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rgb_near(actual: [f32; 3], expected: [f32; 3], tolerance: f32) {
        for channel in 0..3 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= tolerance,
                "channel {channel}: actual={} expected={} tolerance={tolerance}",
                actual[channel],
                expected[channel]
            );
        }
    }

    #[test]
    fn neutral_controls_are_exact_identities() {
        for space in [
            WorkingColorSpace::LinearRec709,
            WorkingColorSpace::LinearRec2020,
            WorkingColorSpace::LinearP3D65,
            WorkingColorSpace::AcesCg,
        ] {
            let white_balance = WhiteBalanceGrade::new(0.0, 0.0, space).expect("identity WB");
            assert_eq!(white_balance, WhiteBalanceGrade::identity());
        }
        let primaries = PrimariesGrade::new([0.0; 3], [1.0; 3], [1.0; 3], [1.0; 3])
            .expect("identity primaries");
        let cdl = AscCdlGrade::new([1.0; 3], [0.0; 3], [1.0; 3], 1.0).expect("identity CDL");
        let sample = [-0.25, 0.18, 4.0];
        assert_eq!(primaries.apply(sample), sample);
        assert_eq!(cdl.apply(sample), [0.0, 0.18, 4.0]);
        assert!(primaries.is_identity());
        assert!(cdl.is_identity());
    }

    #[test]
    fn white_balance_is_working_space_aware_and_preserves_neutral_luminance_scale() {
        let rec709 = WhiteBalanceGrade::new(0.5, -0.25, WorkingColorSpace::LinearRec709)
            .expect("Rec.709 WB");
        let acescg =
            WhiteBalanceGrade::new(0.5, -0.25, WorkingColorSpace::AcesCg).expect("ACEScg WB");
        assert_ne!(rec709.matrix(), acescg.matrix());
        let warmed = rec709.apply([1.0; 3]);
        assert!(
            warmed[0] > warmed[2],
            "positive temperature should warm neutral RGB: {warmed:?}"
        );
        assert!(warmed.into_iter().all(f32::is_finite));
    }

    #[test]
    fn positive_white_balance_tint_moves_neutral_toward_magenta() {
        let grade = WhiteBalanceGrade::new(0.0, 0.5, WorkingColorSpace::LinearRec709)
            .expect("positive-tint white balance");
        let tinted = grade.apply([1.0; 3]);
        let magenta_average = (tinted[0] + tinted[2]) * 0.5;

        assert!(
            tinted[1] < magenta_average,
            "positive tint must suppress green relative to red/blue: {tinted:?}"
        );
        assert!(tinted.into_iter().all(f32::is_finite));
    }

    #[test]
    fn primaries_use_shadow_lift_signed_gamma_gain_and_final_offset() {
        let grade = PrimariesGrade::new(
            [0.01, -0.02, 0.03],
            [1.1, 0.9, 1.0],
            [2.0, 1.0, 0.5],
            [1.2, 0.8, 1.0],
        )
        .expect("valid primaries");
        let actual = grade.apply([0.25, -0.25, 2.0]);
        let expected = [
            (0.25_f32 + 0.1 * 0.75).mul_add(1.2, 0.0).sqrt() + 0.01,
            (-0.25_f32 - 0.1 * 1.25) * 0.8 - 0.02,
            2.0_f32.powi(2) + 0.03,
        ];
        assert_rgb_near(actual, expected, 1.0e-6);
    }

    #[test]
    fn asc_cdl_matches_no_clamp_sop_and_standard_saturation() {
        let grade = AscCdlGrade::new([1.2, 0.8, 1.1], [-0.1, 0.05, 0.0], [2.0, 1.0, 0.5], 0.0)
            .expect("valid CDL");
        let sop = [0.5_f32.powi(2), 0.45, 1.1_f32.sqrt()];
        let luma = dot3(sop, ASC_CDL_LUMA);
        assert_rgb_near(grade.apply([0.5, 0.5, 1.0]), [luma; 3], 1.0e-6);
        assert!(
            grade.apply([2.0, 2.0, 2.0])[0] > 1.0,
            "no-clamp CDL must retain HDR values"
        );
    }

    #[test]
    fn invalid_nonfinite_or_nonpositive_controls_fail_before_execution() {
        assert!(matches!(
            WhiteBalanceGrade::new(f32::NAN, 0.0, WorkingColorSpace::LinearRec709),
            Err(PrimaryGradeError::InvalidControl { control: "temperature" })
        ));
        assert!(matches!(
            PrimariesGrade::new([0.0; 3], [1.0; 3], [0.0, 1.0, 1.0], [1.0; 3]),
            Err(PrimaryGradeError::InvalidControl { control: "gamma" })
        ));
        assert!(matches!(
            AscCdlGrade::new([1.0; 3], [0.0; 3], [1.0; 3], -0.1),
            Err(PrimaryGradeError::InvalidControl { control: "saturation" })
        ));
    }
}
