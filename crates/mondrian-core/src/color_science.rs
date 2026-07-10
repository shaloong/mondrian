//! Device-independent color-science primitives used by validation paths.
//!
//! These APIs are deliberately explicit about the D50 PCS and encoded sRGB
//! boundary. They do not replace OCIO processors in the production color path.

use serde::Serialize;
use thiserror::Error;

/// A finite CIELAB sample relative to the D50 reference white.
///
/// D50 is part of the type name so display validation cannot silently compare
/// samples expressed relative to different reference whites.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CieLabD50 {
    lightness: f64,
    a: f64,
    b: f64,
}

impl CieLabD50 {
    /// Construct a finite D50 CIELAB sample without clamping extended coordinates.
    pub fn new(lightness: f64, a: f64, b: f64) -> Result<Self, ColorScienceError> {
        for (component, value) in [("lightness", lightness), ("a", a), ("b", b)] {
            if !value.is_finite() {
                return Err(ColorScienceError::NonFiniteLabComponent { component, value });
            }
        }
        Ok(Self { lightness, a, b })
    }

    /// CIE L* lightness coordinate.
    pub fn lightness(self) -> f64 {
        self.lightness
    }

    /// CIE a* opponent coordinate.
    pub fn a(self) -> f64 {
        self.a
    }

    /// CIE b* opponent coordinate.
    pub fn b(self) -> f64 {
        self.b
    }
}

/// Finite display-linear BT.2100 RGB expressed in candelas per square metre.
///
/// Components are not clamped: BT.2124 requires out-of-gamut values to remain
/// available while converting to ITP so the metric measures the actual error.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Bt2100DisplayLinearRgb {
    red_nits: f64,
    green_nits: f64,
    blue_nits: f64,
}

impl Bt2100DisplayLinearRgb {
    /// Construct finite absolute-luminance BT.2100 RGB without gamut clipping.
    pub fn new(red_nits: f64, green_nits: f64, blue_nits: f64) -> Result<Self, ColorScienceError> {
        validate_finite_triplet(
            "BT.2100 display-linear RGB",
            [
                ("red", red_nits),
                ("green", green_nits),
                ("blue", blue_nits),
            ],
        )?;
        Ok(Self { red_nits, green_nits, blue_nits })
    }

    /// Return `[R, G, B]` in candelas per square metre.
    pub fn components_nits(self) -> [f64; 3] {
        [self.red_nits, self.green_nits, self.blue_nits]
    }
}

/// Finite CIE XYZ tristimulus values using a D65 white, in cd/m².
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CieXyzD65Nits {
    x: f64,
    y: f64,
    z: f64,
}

impl CieXyzD65Nits {
    /// Construct finite absolute-luminance D65 XYZ without gamut clipping.
    pub fn new(x: f64, y: f64, z: f64) -> Result<Self, ColorScienceError> {
        validate_finite_triplet("CIE XYZ D65", [("x", x), ("y", y), ("z", z)])?;
        Ok(Self { x, y, z })
    }

    /// Return `[X, Y, Z]` in candelas per square metre.
    pub fn components_nits(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }
}

/// Finite display-referred ITP sample defined by ITU-R BT.2124.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ItpDisplay {
    intensity: f64,
    tritan: f64,
    protan: f64,
}

impl ItpDisplay {
    /// Construct a finite ITP sample.
    pub fn new(intensity: f64, tritan: f64, protan: f64) -> Result<Self, ColorScienceError> {
        validate_finite_triplet(
            "ITP",
            [
                ("intensity", intensity),
                ("tritan", tritan),
                ("protan", protan),
            ],
        )?;
        Ok(Self { intensity, tritan, protan })
    }

    /// Return `[I, T, P]` coordinates.
    pub fn components(self) -> [f64; 3] {
        [self.intensity, self.tritan, self.protan]
    }
}

/// Structured color-science validation failure.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ColorScienceError {
    /// A CIELAB coordinate was NaN or infinite.
    #[error("non-finite D50 CIELAB component {component}={value}")]
    NonFiniteLabComponent {
        /// Stable component name.
        component: &'static str,
        /// Rejected value.
        value: f64,
    },
    /// An encoded sRGB component was NaN or infinite.
    #[error("non-finite encoded sRGB component {channel}={value}")]
    NonFiniteSrgbComponent {
        /// Stable channel name.
        channel: &'static str,
        /// Rejected value.
        value: f64,
    },
    /// An encoded sRGB component was outside the normalized raster range.
    #[error("encoded sRGB component {channel}={value} is outside [0, 1]")]
    SrgbComponentOutOfRange {
        /// Stable channel name.
        channel: &'static str,
        /// Rejected value.
        value: f64,
    },
    /// A component of an explicitly named color representation was not finite.
    #[error("non-finite {representation} component {component}={value}")]
    NonFiniteComponent {
        /// Stable representation name including white point or luminance semantics.
        representation: &'static str,
        /// Stable component name.
        component: &'static str,
        /// Rejected value.
        value: f64,
    },
    /// A normalized PQ signal component was outside its full-range interval.
    #[error("normalized BT.2100 PQ component {channel}={value} is outside [0, 1]")]
    PqComponentOutOfRange {
        /// Stable channel name.
        channel: &'static str,
        /// Rejected value.
        value: f64,
    },
    /// CIEDE2000 arithmetic produced an invalid squared distance.
    #[error("CIEDE2000 produced invalid squared distance {value}")]
    InvalidDeltaESquared {
        /// Invalid intermediate value.
        value: f64,
    },
}

/// Decode normalized full-range BT.2100 PQ RGB to display-linear cd/m².
pub fn bt2100_pq_to_display_linear_rgb(
    encoded: [f64; 3],
) -> Result<Bt2100DisplayLinearRgb, ColorScienceError> {
    for (channel, value) in [
        ("red", encoded[0]),
        ("green", encoded[1]),
        ("blue", encoded[2]),
    ] {
        if !value.is_finite() {
            return Err(ColorScienceError::NonFiniteComponent {
                representation: "BT.2100 PQ",
                component: channel,
                value,
            });
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(ColorScienceError::PqComponentOutOfRange { channel, value });
        }
    }
    let linear = encoded.map(pq_eotf_nits);
    Bt2100DisplayLinearRgb::new(linear[0], linear[1], linear[2])
}

/// Convert absolute-luminance D65 XYZ to display-linear BT.2100 RGB.
pub fn cie_xyz_d65_nits_to_bt2100_display_linear_rgb(
    xyz: CieXyzD65Nits,
) -> Result<Bt2100DisplayLinearRgb, ColorScienceError> {
    let rgb = multiply_matrix_vector(XYZ_D65_TO_BT2100_RGB, xyz.components_nits());
    Bt2100DisplayLinearRgb::new(rgb[0], rgb[1], rgb[2])
}

/// Convert display-linear BT.2100 RGB in cd/m² to BT.2124 ITP.
pub fn bt2100_display_linear_rgb_to_itp(
    rgb: Bt2100DisplayLinearRgb,
) -> Result<ItpDisplay, ColorScienceError> {
    let lms = multiply_matrix_vector(BT2100_RGB_TO_LMS, rgb.components_nits());
    // BT.2124 says not to clamp out-of-gamut RGB. The sign-preserving extension
    // keeps finite negative cone responses measurable while matching ST 2084
    // exactly for its normative non-negative domain.
    let lms_prime = lms.map(pq_inverse_eotf_signed);
    let ictcp = multiply_matrix_vector(LMS_PRIME_TO_ICTCP, lms_prime);
    ItpDisplay::new(ictcp[0], 0.5 * ictcp[1], ictcp[2])
}

/// Compute the BT.2124 perceptual color difference between two ITP samples.
pub fn delta_e_itp(reference: ItpDisplay, sample: ItpDisplay) -> Result<f64, ColorScienceError> {
    let first = reference.components();
    let second = sample.components();
    let squared = first.into_iter().zip(second).map(|(a, b)| (a - b).powi(2)).sum::<f64>();
    if !squared.is_finite() || squared < 0.0 {
        return Err(ColorScienceError::InvalidDeltaESquared { value: squared });
    }
    Ok(720.0 * squared.sqrt())
}

/// Convert a normalized, display-referred encoded sRGB triplet to D50 CIELAB.
///
/// The conversion follows the W3C Color 4 reference sequence: decode the sRGB
/// transfer function, convert linear sRGB to XYZ D65, adapt D65 to D50 with the
/// Bradford matrix, then convert XYZ D50 to CIELAB. Raster inputs outside `[0, 1]`
/// are rejected instead of being silently clipped. This function is for SDR
/// display validation; PQ/HLG HDR outputs require an HDR-aware perceptual model.
pub fn srgb_to_cie_lab_d50(encoded: [f64; 3]) -> Result<CieLabD50, ColorScienceError> {
    for (channel, value) in [
        ("red", encoded[0]),
        ("green", encoded[1]),
        ("blue", encoded[2]),
    ] {
        if !value.is_finite() {
            return Err(ColorScienceError::NonFiniteSrgbComponent { channel, value });
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(ColorScienceError::SrgbComponentOutOfRange { channel, value });
        }
    }

    let linear = encoded.map(decode_srgb_channel);
    let xyz_d65 = multiply_matrix_vector(LINEAR_SRGB_TO_XYZ_D65, linear);
    let xyz_d50 = multiply_matrix_vector(XYZ_D65_TO_D50, xyz_d65);
    xyz_d50_to_lab(xyz_d50, D50_REFERENCE_FROM_SRGB_WHITE)
}

/// Compute the CIEDE2000 color difference between two D50 CIELAB samples.
///
/// Parametric factors are fixed at `kL = kC = kH = 1`, matching the reference
/// implementation and the normal graphic-arts viewing condition. The branch
/// handling follows Sharma, Wu, and Dalal's implementation notes and supplemental
/// test data, including the hue-angle discontinuity cases.
pub fn delta_e_2000_d50(reference: CieLabD50, sample: CieLabD50) -> Result<f64, ColorScienceError> {
    let c1 = reference.a.hypot(reference.b);
    let c2 = sample.a.hypot(sample.b);
    let mean_c = (c1 + c2) * 0.5;
    let g = 0.5 * (1.0 - chroma_seventh_ratio(mean_c).sqrt());

    let a1_prime = (1.0 + g) * reference.a;
    let a2_prime = (1.0 + g) * sample.a;
    let c1_prime = a1_prime.hypot(reference.b);
    let c2_prime = a2_prime.hypot(sample.b);
    let h1_prime = hue_degrees(a1_prime, reference.b);
    let h2_prime = hue_degrees(a2_prime, sample.b);

    let delta_l_prime = sample.lightness - reference.lightness;
    let delta_c_prime = c2_prime - c1_prime;
    let delta_h_angle = hue_delta_degrees(c1_prime, c2_prime, h1_prime, h2_prime);
    let delta_h_prime =
        2.0 * (c1_prime * c2_prime).sqrt() * (0.5 * delta_h_angle).to_radians().sin();

    let mean_l_prime = (reference.lightness + sample.lightness) * 0.5;
    let mean_c_prime = (c1_prime + c2_prime) * 0.5;
    let mean_h_prime = mean_hue_degrees(c1_prime, c2_prime, h1_prime, h2_prime);
    let t = 1.0 - 0.17 * (mean_h_prime - 30.0).to_radians().cos()
        + 0.24 * (2.0 * mean_h_prime).to_radians().cos()
        + 0.32 * (3.0 * mean_h_prime + 6.0).to_radians().cos()
        - 0.20 * (4.0 * mean_h_prime - 63.0).to_radians().cos();
    let delta_theta = 30.0 * (-((mean_h_prime - 275.0) / 25.0).powi(2)).exp();
    let r_c = 2.0 * chroma_seventh_ratio(mean_c_prime).sqrt();
    let lightness_offset = mean_l_prime - 50.0;
    let s_l = 1.0 + 0.015 * lightness_offset.powi(2) / (20.0 + lightness_offset.powi(2)).sqrt();
    let s_c = 1.0 + 0.045 * mean_c_prime;
    let s_h = 1.0 + 0.015 * mean_c_prime * t;
    let r_t = -(2.0 * delta_theta).to_radians().sin() * r_c;

    let lightness_term = delta_l_prime / s_l;
    let chroma_term = delta_c_prime / s_c;
    let hue_term = delta_h_prime / s_h;
    let squared = lightness_term.powi(2)
        + chroma_term.powi(2)
        + hue_term.powi(2)
        + r_t * chroma_term * hue_term;
    if !squared.is_finite() || squared < -1.0e-12 {
        return Err(ColorScienceError::InvalidDeltaESquared { value: squared });
    }
    Ok(squared.max(0.0).sqrt())
}

const LINEAR_SRGB_TO_XYZ_D65: [[f64; 3]; 3] = [
    [
        506_752.0 / 1_228_815.0,
        87_881.0 / 245_763.0,
        12_673.0 / 70_218.0,
    ],
    [
        87_098.0 / 409_605.0,
        175_762.0 / 245_763.0,
        12_673.0 / 175_545.0,
    ],
    [
        7_918.0 / 409_605.0,
        87_881.0 / 737_289.0,
        1_001_167.0 / 1_053_270.0,
    ],
];

const XYZ_D65_TO_D50: [[f64; 3]; 3] = [
    [
        1.047_929_820_840_548_8,
        0.022_946_793_341_019_088,
        -0.050_192_229_543_135_57,
    ],
    [
        0.029_627_815_688_159_344,
        0.990_434_484_573_249,
        -0.017_073_825_029_385_14,
    ],
    [
        -0.009_243_058_152_591_178,
        0.015_055_144_896_577_895,
        0.751_874_281_428_137_1,
    ],
];

const XYZ_D65_TO_BT2100_RGB: [[f64; 3]; 3] = [
    [
        1.716_651_187_971_268,
        -0.355_670_783_776_392,
        -0.253_366_281_373_660,
    ],
    [
        -0.666_684_351_832_489,
        1.616_481_236_634_939,
        0.015_768_545_813_911,
    ],
    [
        0.017_639_857_445_311,
        -0.042_770_613_257_809,
        0.942_103_121_235_474,
    ],
];

const BT2100_RGB_TO_LMS: [[f64; 3]; 3] = [
    [1688.0 / 4096.0, 2146.0 / 4096.0, 262.0 / 4096.0],
    [683.0 / 4096.0, 2951.0 / 4096.0, 462.0 / 4096.0],
    [99.0 / 4096.0, 309.0 / 4096.0, 3688.0 / 4096.0],
];

const LMS_PRIME_TO_ICTCP: [[f64; 3]; 3] = [
    [0.5, 0.5, 0.0],
    [6610.0 / 4096.0, -13_613.0 / 4096.0, 7003.0 / 4096.0],
    [17_933.0 / 4096.0, -17_390.0 / 4096.0, -543.0 / 4096.0],
];

const PQ_M1: f64 = 2610.0 / 16_384.0;
const PQ_M2: f64 = 2523.0 / 32.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 128.0;
const PQ_C3: f64 = 2392.0 / 128.0;

const D50_REFERENCE_FROM_SRGB_WHITE: [f64; 3] = multiply_matrix_vector(
    XYZ_D65_TO_D50,
    multiply_matrix_vector(LINEAR_SRGB_TO_XYZ_D65, [1.0; 3]),
);

fn decode_srgb_channel(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn pq_eotf_nits(encoded: f64) -> f64 {
    let signal = encoded.powf(1.0 / PQ_M2);
    let numerator = (signal - PQ_C1).max(0.0);
    let denominator = PQ_C2 - PQ_C3 * signal;
    10_000.0 * (numerator / denominator).powf(1.0 / PQ_M1)
}

fn pq_inverse_eotf_signed(nits: f64) -> f64 {
    nits.signum() * pq_inverse_eotf_non_negative(nits.abs())
}

fn pq_inverse_eotf_non_negative(nits: f64) -> f64 {
    let normalized = nits / 10_000.0;
    let powered = normalized.powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * powered) / (1.0 + PQ_C3 * powered)).powf(PQ_M2)
}

fn validate_finite_triplet(
    representation: &'static str,
    components: [(&'static str, f64); 3],
) -> Result<(), ColorScienceError> {
    for (component, value) in components {
        if !value.is_finite() {
            return Err(ColorScienceError::NonFiniteComponent { representation, component, value });
        }
    }
    Ok(())
}

fn xyz_d50_to_lab(
    xyz: [f64; 3],
    reference_white: [f64; 3],
) -> Result<CieLabD50, ColorScienceError> {
    let scaled = [
        xyz[0] / reference_white[0],
        xyz[1] / reference_white[1],
        xyz[2] / reference_white[2],
    ];
    let f = scaled.map(lab_response);
    CieLabD50::new(
        116.0 * f[1] - 16.0,
        500.0 * (f[0] - f[1]),
        200.0 * (f[1] - f[2]),
    )
}

fn lab_response(value: f64) -> f64 {
    const EPSILON: f64 = 216.0 / 24_389.0;
    const KAPPA: f64 = 24_389.0 / 27.0;
    if value > EPSILON {
        value.cbrt()
    } else {
        (KAPPA * value + 16.0) / 116.0
    }
}

const fn multiply_matrix_vector(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

fn chroma_seventh_ratio(chroma: f64) -> f64 {
    if chroma == 0.0 {
        return 0.0;
    }
    let ratio = 25.0 / chroma;
    1.0 / (1.0 + ratio.powi(7))
}

fn hue_degrees(a: f64, b: f64) -> f64 {
    if a == 0.0 && b == 0.0 {
        0.0
    } else {
        b.atan2(a).to_degrees().rem_euclid(360.0)
    }
}

fn hue_delta_degrees(c1: f64, c2: f64, h1: f64, h2: f64) -> f64 {
    if c1 == 0.0 || c2 == 0.0 {
        return 0.0;
    }
    let delta = h2 - h1;
    if delta.abs() <= 180.0 {
        delta
    } else if delta > 180.0 {
        delta - 360.0
    } else {
        delta + 360.0
    }
}

fn mean_hue_degrees(c1: f64, c2: f64, h1: f64, h2: f64) -> f64 {
    if c1 == 0.0 || c2 == 0.0 {
        return h1 + h2;
    }
    if (h1 - h2).abs() <= 180.0 {
        return (h1 + h2) * 0.5;
    }
    if h1 + h2 < 360.0 {
        (h1 + h2 + 360.0) * 0.5
    } else {
        (h1 + h2 - 360.0) * 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_reference_black_white_and_css_example_convert_to_d50_lab() {
        let black = srgb_to_cie_lab_d50([0.0, 0.0, 0.0]).expect("black");
        let white = srgb_to_cie_lab_d50([1.0, 1.0, 1.0]).expect("white");
        let css_example = srgb_to_cie_lab_d50([
            0x76 as f64 / 255.0,
            0x54 as f64 / 255.0,
            0xcd as f64 / 255.0,
        ])
        .expect("W3C #7654CD example");

        assert!(black.lightness().abs() < 1.0e-12);
        assert!((white.lightness() - 100.0).abs() < 1.0e-9);
        assert!(white.a().abs() < 1.0e-9);
        assert!(white.b().abs() < 1.0e-9);
        assert!((css_example.lightness() - 44.36).abs() < 0.03);
        assert!((css_example.a() - 36.05).abs() < 0.03);
        assert!((css_example.b() + 58.99).abs() < 0.03);
    }

    #[test]
    fn constructors_reject_non_finite_and_out_of_range_values() {
        assert!(matches!(
            CieLabD50::new(f64::NAN, 0.0, 0.0),
            Err(ColorScienceError::NonFiniteLabComponent { component: "lightness", .. })
        ));
        assert!(matches!(
            srgb_to_cie_lab_d50([1.1, 0.0, 0.0]),
            Err(ColorScienceError::SrgbComponentOutOfRange { channel: "red", .. })
        ));
    }

    #[test]
    fn delta_e_is_zero_for_identical_samples_and_symmetric_away_from_discontinuities() {
        let first = CieLabD50::new(55.0, 20.0, -30.0).expect("finite");
        let second = CieLabD50::new(60.0, 22.0, -27.0).expect("finite");

        assert_eq!(delta_e_2000_d50(first, first), Ok(0.0));
        let forward = delta_e_2000_d50(first, second).expect("finite delta");
        let reverse = delta_e_2000_d50(second, first).expect("finite delta");
        assert!((forward - reverse).abs() < 1.0e-12);
    }

    #[test]
    fn bt2124_annex_four_pq_reference_converts_to_display_linear_rgb_and_itp() {
        let rgb = bt2100_pq_to_display_linear_rgb([296.0 / 1023.0, 201.0 / 1023.0, 582.0 / 1023.0])
            .expect("BT.2124 PQ reference");
        let [red, green, blue] = rgb.components_nits();
        assert!((red - 8.753).abs() < 0.01, "red={red}");
        assert!((green - 2.291).abs() < 0.01, "green={green}");
        assert!((blue - 181.3).abs() < 0.1, "blue={blue}");
        let itp = bt2100_display_linear_rgb_to_itp(rgb).expect("BT.2124 ITP reference");
        let [intensity, tritan, protan] = itp.components();
        // Annex 4 publishes intermediate values at only four decimal places;
        // its accumulated display-linear rounding is larger than half a unit
        // in that last printed place.
        assert!((intensity - 0.3554).abs() < 0.0005, "intensity={intensity}");
        assert!((tritan - 0.1346).abs() < 0.0005, "tritan={tritan}");
        assert!((protan + 0.1613).abs() < 0.0005, "protan={protan}");
    }

    #[test]
    fn bt2124_annex_four_xyz_reference_reproduces_delta_e_itp() {
        let xyz = CieXyzD65Nits::new(36.0, 15.0, 190.0).expect("finite XYZ");
        let xyz_itp = bt2100_display_linear_rgb_to_itp(
            cie_xyz_d65_nits_to_bt2100_display_linear_rgb(xyz).expect("BT.2100 RGB"),
        )
        .expect("ITP");
        let [intensity, tritan, protan] = xyz_itp.components();
        assert!((intensity - 0.3568).abs() < 0.0001);
        assert!((tritan - 0.1321).abs() < 0.0001);
        assert!((protan + 0.1629).abs() < 0.0001);

        let published_rgb_itp = ItpDisplay::new(0.3554, 0.1346, -0.1613).expect("published ITP");
        let published_xyz_itp = ItpDisplay::new(0.3568, 0.1321, -0.1629).expect("published ITP");
        let delta = delta_e_itp(published_rgb_itp, published_xyz_itp).expect("finite delta");
        assert!((delta - 2.363).abs() < 0.001, "delta={delta}");
    }

    #[test]
    fn pq_eotf_preserves_normative_absolute_luminance_endpoints() {
        let black = bt2100_pq_to_display_linear_rgb([0.0; 3]).expect("PQ black");
        let peak = bt2100_pq_to_display_linear_rgb([1.0; 3]).expect("PQ peak");
        assert_eq!(black.components_nits(), [0.0; 3]);
        for component in peak.components_nits() {
            assert!((component - 10_000.0).abs() < 1.0e-8);
        }
    }

    #[test]
    fn hdr_color_science_rejects_implicit_clipping_and_non_finite_values() {
        assert!(matches!(
            bt2100_pq_to_display_linear_rgb([1.01, 0.0, 0.0]),
            Err(ColorScienceError::PqComponentOutOfRange { channel: "red", .. })
        ));
        assert!(matches!(
            CieXyzD65Nits::new(0.0, f64::INFINITY, 0.0),
            Err(ColorScienceError::NonFiniteComponent {
                representation: "CIE XYZ D65",
                component: "y",
                ..
            })
        ));

        let extended = Bt2100DisplayLinearRgb::new(-0.5, 10.0, 20.0)
            .expect("negative out-of-gamut values remain measurable");
        assert!(bt2100_display_linear_rgb_to_itp(extended).is_ok());
    }
}
