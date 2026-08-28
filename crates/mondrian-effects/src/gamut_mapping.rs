//! Backend-neutral scene-linear gamut and highlight reconstruction mathematics.
//!
//! Author controls compile once into immutable point operators. The reference
//! gamut compressor executes the ACES 1.3 Reference Gamut Compression in AP1;
//! non-ACES working spaces use fixed chromatically-adapted RGB matrices. The
//! highlight operator reconstructs clipped-channel chroma toward the neutral
//! axis while preserving CIE Y. It does not claim to recover missing RAW or
//! spatial detail.

use mondrian_core::WorkingColorSpace;

const IDENTITY_EPSILON: f32 = 1.0e-6;
const ACES_LIMITS: [f32; 3] = [1.147, 1.264, 1.312];
const ACES_THRESHOLDS: [f32; 3] = [0.815, 0.803, 0.880];
const ACES_POWER: f32 = 1.2;

/// Invalid authored gamut/highlight control.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GamutMappingError {
    /// One scalar was non-finite or outside its semantic domain.
    #[error("invalid gamut/highlight control `{control}`")]
    InvalidControl { control: &'static str },
}

/// ACES 1.3 Reference Gamut Compression compiled for one working space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GamutCompressionGrade {
    amount: f32,
    working_color_space: WorkingColorSpace,
    working_to_ap1: [[f32; 3]; 3],
    ap1_to_working: [[f32; 3]; 3],
}

impl GamutCompressionGrade {
    /// Compile an amount in `[0, 1]` for one exact scene-linear working space.
    pub fn new(
        amount: f32,
        working_color_space: WorkingColorSpace,
    ) -> Result<Self, GamutMappingError> {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(GamutMappingError::InvalidControl { control: "amount" });
        }
        let (working_to_ap1, ap1_to_working) = working_ap1_matrices(working_color_space);
        Ok(Self {
            amount,
            working_color_space,
            working_to_ap1,
            ap1_to_working,
        })
    }

    /// Authored blend amount.
    pub const fn amount(self) -> f32 {
        self.amount
    }

    /// Working-space identity used by CPU and GPU implementations.
    pub const fn working_color_space(self) -> WorkingColorSpace {
        self.working_color_space
    }

    /// Apply the reference transform without clamping extended HDR values.
    #[inline]
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        if self.amount <= IDENTITY_EPSILON || rgb.iter().any(|value| !value.is_finite()) {
            return rgb;
        }
        let maximum = rgb[0].abs().max(rgb[1].abs()).max(rgb[2].abs());
        let normalization = if maximum > 1.0e20 { maximum } else { 1.0 };
        let normalized_rgb = rgb.map(|value| value / normalization);
        let ap1 = mul3_vec(self.working_to_ap1, normalized_rgb);
        let compressed = aces_13_reference_gamut_compress(ap1);
        let normalized_working = mul3_vec(self.ap1_to_working, compressed);
        let output_maximum = normalized_working[0]
            .abs()
            .max(normalized_working[1].abs())
            .max(normalized_working[2].abs());
        let safe_normalization = if output_maximum > 1.0 {
            normalization.min(f32::MAX / output_maximum)
        } else {
            normalization
        };
        let working = normalized_working.map(|value| value * safe_normalization);
        std::array::from_fn(|channel| {
            rgb[channel] * (1.0 - self.amount) + working[channel] * self.amount
        })
    }

    /// Whether the authored blend is neutral.
    pub fn is_identity(self) -> bool {
        self.amount <= IDENTITY_EPSILON
    }
}

/// Scene-linear highlight chroma reconstruction compiled for one working space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HighlightRecoveryGrade {
    threshold: f32,
    rolloff: f32,
    strength: f32,
    luminance_coefficients: [f32; 3],
}

impl HighlightRecoveryGrade {
    /// Compile highlight controls.
    ///
    /// `threshold` is the scene-linear onset in `[0, 16]`, `rolloff` the
    /// strictly-positive transition width in `(0, 16]`, and `strength` the
    /// reconstruction blend in `[0, 1]`.
    pub fn new(
        threshold: f32,
        rolloff: f32,
        strength: f32,
        working_color_space: WorkingColorSpace,
    ) -> Result<Self, GamutMappingError> {
        if !threshold.is_finite() || !(0.0..=16.0).contains(&threshold) {
            return Err(GamutMappingError::InvalidControl { control: "threshold" });
        }
        if !rolloff.is_finite() || rolloff <= 0.0 || rolloff > 16.0 {
            return Err(GamutMappingError::InvalidControl { control: "rolloff" });
        }
        if !strength.is_finite() || !(0.0..=1.0).contains(&strength) {
            return Err(GamutMappingError::InvalidControl { control: "strength" });
        }
        Ok(Self {
            threshold,
            rolloff,
            strength,
            luminance_coefficients: working_color_space.luminance_coefficients(),
        })
    }

    /// Scene-linear onset.
    pub const fn threshold(self) -> f32 {
        self.threshold
    }

    /// Scene-linear transition width.
    pub const fn rolloff(self) -> f32 {
        self.rolloff
    }

    /// Authored reconstruction strength.
    pub const fn strength(self) -> f32 {
        self.strength
    }

    /// Exact working-space CIE Y coefficients.
    pub const fn luminance_coefficients(self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// Reconstruct clipped-channel chroma while preserving scene-linear CIE Y.
    #[inline]
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        if self.strength <= IDENTITY_EPSILON || rgb.iter().any(|value| !value.is_finite()) {
            return rgb;
        }
        let peak = rgb[0].max(rgb[1]).max(rgb[2]);
        if peak <= self.threshold {
            return rgb;
        }
        let transition = ((peak - self.threshold) / self.rolloff).clamp(0.0, 1.0);
        let weight = transition * transition * (3.0 - 2.0 * transition) * self.strength;
        let luminance = dot3(rgb, self.luminance_coefficients);
        std::array::from_fn(|channel| rgb[channel] + (luminance - rgb[channel]) * weight)
    }

    /// Whether the authored strength is neutral.
    pub fn is_identity(self) -> bool {
        self.strength <= IDENTITY_EPSILON
    }
}

/// ACES 1.3 Reference Gamut Compression in ACEScg/AP1.
#[inline]
fn aces_13_reference_gamut_compress(rgb: [f32; 3]) -> [f32; 3] {
    let achromatic = rgb[0].max(rgb[1]).max(rgb[2]);
    let achromatic_abs = achromatic.abs();
    if achromatic_abs <= f32::MIN_POSITIVE {
        return rgb;
    }
    let scales: [f32; 3] = std::array::from_fn(|channel| {
        let limit = ACES_LIMITS[channel];
        let threshold = ACES_THRESHOLDS[channel];
        let ratio = (1.0 - threshold) / (limit - threshold);
        (limit - threshold) / ((ratio.powf(-ACES_POWER) - 1.0).powf(1.0 / ACES_POWER))
    });
    std::array::from_fn(|channel| {
        let distance = (achromatic - rgb[channel]) / achromatic_abs;
        if distance < ACES_THRESHOLDS[channel] {
            return rgb[channel];
        }
        let normalized = (distance - ACES_THRESHOLDS[channel]) / scales[channel];
        // The reference curve tends to threshold + scale at extreme distance.
        // Taking that limit avoids pow overflow for otherwise-finite HDR values.
        let compressed_distance = if normalized > 1.0e20 {
            ACES_THRESHOLDS[channel] + scales[channel]
        } else {
            let powered = normalized.powf(ACES_POWER);
            ACES_THRESHOLDS[channel]
                + scales[channel] * normalized / (1.0 + powered).powf(1.0 / ACES_POWER)
        };
        achromatic - compressed_distance * achromatic_abs
    })
}

#[inline]
fn dot3(value: [f32; 3], coefficients: [f32; 3]) -> f32 {
    value[0] * coefficients[0] + value[1] * coefficients[1] + value[2] * coefficients[2]
}

#[inline]
fn mul3_vec(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| {
        matrix[row][0] * value[0] + matrix[row][1] * value[1] + matrix[row][2] * value[2]
    })
}

const IDENTITY_MATRIX: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn working_ap1_matrices(space: WorkingColorSpace) -> ([[f32; 3]; 3], [[f32; 3]; 3]) {
    match space {
        WorkingColorSpace::LinearRec709 => (REC709_TO_AP1, AP1_TO_REC709),
        WorkingColorSpace::LinearRec2020 => (REC2020_TO_AP1, AP1_TO_REC2020),
        WorkingColorSpace::LinearP3D65 => (P3_D65_TO_AP1, AP1_TO_P3_D65),
        WorkingColorSpace::AcesCg => (IDENTITY_MATRIX, IDENTITY_MATRIX),
    }
}

// D65 spaces are Bradford-adapted to AP1's D60 white. Values are fixed here
// and mirrored in the renderer shader so backend parity is reviewable.
const REC709_TO_AP1: [[f32; 3]; 3] = [
    [0.613_097_4, 0.339_523_08, 0.047_379_527],
    [0.070_193_75, 0.916_353_94, 0.013_452_331],
    [0.020_615_578, 0.109_569_736, 0.869_814_63],
];
const AP1_TO_REC709: [[f32; 3]; 3] = [
    [1.705_051, -0.621_791_9, -0.083_259_076],
    [-0.130_256_46, 1.140_804_6, -0.010_548_215],
    [-0.024_003_327, -0.128_968_92, 1.152_972_3],
];
const REC2020_TO_AP1: [[f32; 3]; 3] = [
    [0.974_895, 0.019_599_026, 0.005_506_001],
    [0.002_179_594, 0.995_535_55, 0.002_284_89],
    [0.004_797_217, 0.024_531_983, 0.970_670_76],
];
const AP1_TO_REC2020: [[f32; 3]; 3] = [
    [1.025_824_8, -0.020_053_102, -0.005_771_651],
    [-0.002_234_402, 1.004_586_5, -0.002_352_051],
    [-0.005_013_327, -0.025_290_035, 1.030_303_4],
];
const P3_D65_TO_AP1: [[f32; 3]; 3] = [
    [0.735_797_9, 0.212_166_41, 0.052_035_686],
    [0.047_179_915, 0.938_045_8, 0.014_774_34],
    [0.003_563_646, 0.041_141_85, 0.955_294_43],
];
const AP1_TO_P3_D65: [[f32; 3]; 3] = [
    [1.379_214_2, -0.308_864, -0.070_350_13],
    [-0.069_334_894, 1.082_296_6, -0.012_961_794],
    [-0.002_158_984, -0.045_459_285, 1.047_618_3],
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rgb_near(actual: [f32; 3], expected: [f32; 3], epsilon: f32) {
        for channel in 0..3 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= epsilon,
                "channel {channel}: actual={} expected={}",
                actual[channel],
                expected[channel]
            );
        }
    }

    #[test]
    fn aces_reference_compression_matches_independent_ctl_vectors() {
        // OpenColorIO FixedFunctionOpCPU `aces_gamut_map_13`; expected values
        // were produced by the official ACES 1.3 CTL transform with AP0/AP1
        // conversion disabled, not by the implementation under test.
        let vectors = [
            (
                [0.966_634_1, 0.048_190_45, 0.007_193],
                [0.966_634_1, 0.086_100_88, 0.046_986_88],
            ),
            (
                [0.001_423_957, 1.312_399_1, -0.223_322_99],
                [0.070_700_29, 1.312_399_1, 0.015_419_126],
            ),
            (
                [-0.081_868_97, -0.279_064_9, 1.386_940_2],
                [0.039_410_233, 0.014_827_847, 1.386_940_2],
            ),
        ];
        let grade = GamutCompressionGrade::new(1.0, WorkingColorSpace::AcesCg).expect("grade");
        for (input, expected) in vectors {
            assert_rgb_near(grade.apply(input), expected, 1.0e-6);
        }
    }

    #[test]
    fn gamut_compression_preserves_neutral_axis_and_identity_amount() {
        for space in [
            WorkingColorSpace::LinearRec709,
            WorkingColorSpace::LinearRec2020,
            WorkingColorSpace::LinearP3D65,
            WorkingColorSpace::AcesCg,
        ] {
            let active = GamutCompressionGrade::new(1.0, space).expect("active grade");
            assert_rgb_near(active.apply([4.0; 3]), [4.0; 3], 2.0e-5);
            let identity = GamutCompressionGrade::new(0.0, space).expect("identity grade");
            assert_eq!(identity.apply([2.0, -0.4, 0.7]), [2.0, -0.4, 0.7]);
        }
        let extreme = GamutCompressionGrade::new(1.0, WorkingColorSpace::LinearRec709)
            .expect("extreme grade")
            .apply([f32::MAX, -f32::MAX, f32::MAX * 0.5]);
        assert!(extreme.into_iter().all(f32::is_finite));
    }

    #[test]
    fn highlight_recovery_preserves_luminance_alpha_independent_math() {
        let grade = HighlightRecoveryGrade::new(1.0, 1.0, 1.0, WorkingColorSpace::LinearRec2020)
            .expect("highlight grade");
        let input = [3.0, 1.0, 0.2];
        let output = grade.apply(input);
        let luma = WorkingColorSpace::LinearRec2020.luminance_coefficients();
        assert!((dot3(input, luma) - dot3(output, luma)).abs() <= 2.0e-6);
        assert!((output[0] - output[2]).abs() < (input[0] - input[2]).abs());
        assert_eq!(grade.apply([0.8, 0.4, 0.2]), [0.8, 0.4, 0.2]);
    }

    #[test]
    fn authored_controls_fail_closed() {
        assert!(GamutCompressionGrade::new(f32::NAN, WorkingColorSpace::AcesCg).is_err());
        assert!(HighlightRecoveryGrade::new(1.0, 0.0, 1.0, WorkingColorSpace::AcesCg).is_err());
        assert!(HighlightRecoveryGrade::new(1.0, 1.0, 1.1, WorkingColorSpace::AcesCg).is_err());
    }
}
