//! Sampled scene-linear HDR zone grading shared by CPU and GPU execution.
//!
//! Author controls remain compact and editable. Graph evaluation compiles the
//! six overlapping luminance zones into an immutable one-dimensional table so
//! per-pixel execution performs one bounded lookup instead of evaluating every
//! zone window. The table is also the exact GPU upload payload, keeping CPU and
//! GPU semantics aligned without expanding the compositor's generic uniform
//! record.

use mondrian_core::WorkingColorSpace;
use sha2::{Digest, Sha256};

/// Number of luminance zones in the professional HDR palette.
pub const HDR_GRADING_ZONE_COUNT: usize = 6;
/// Number of samples in the immutable luminance-domain grading table.
pub const HDR_GRADING_SAMPLE_COUNT: usize = 512;
/// RGBA rows stored for each table sample.
pub const HDR_GRADING_SAMPLE_ROWS: usize = 2;
/// Lowest scene-linear exposure represented by the table, relative to 18% grey.
pub const HDR_GRADING_MIN_STOPS: f32 = -12.0;
/// Highest scene-linear exposure represented by the table, relative to 18% grey.
pub const HDR_GRADING_MAX_STOPS: f32 = 12.0;

const REFERENCE_GREY: f32 = 0.18;
const MIN_LUMINANCE: f32 = 1.0e-12;
const BALANCE_GAIN_STOPS: f32 = 0.5;
const IDENTITY_EPSILON: f32 = 1.0e-6;

/// Stable product identity for one HDR luminance zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HdrGradingZone {
    Blacks,
    Dark,
    Shadows,
    Light,
    Highlights,
    Specular,
}

impl HdrGradingZone {
    /// Product order from the lowest to the highest luminance range.
    pub const ALL: [Self; HDR_GRADING_ZONE_COUNT] = [
        Self::Blacks,
        Self::Dark,
        Self::Shadows,
        Self::Light,
        Self::Highlights,
        Self::Specular,
    ];

    /// Stable parameter prefix persisted by the built-in effect definition.
    pub const fn parameter_prefix(self) -> &'static str {
        match self {
            Self::Blacks => "blacks",
            Self::Dark => "dark",
            Self::Shadows => "shadows",
            Self::Light => "light",
            Self::Highlights => "highlights",
            Self::Specular => "specular",
        }
    }

    /// Default center in stops relative to scene-linear 18% grey.
    pub const fn default_center_stops(self) -> f32 {
        match self {
            Self::Blacks => -10.0,
            Self::Dark => -6.0,
            Self::Shadows => -3.0,
            Self::Light => 0.0,
            Self::Highlights => 3.0,
            Self::Specular => 7.0,
        }
    }

    /// Default half-width in stops.
    pub const fn default_width_stops(self) -> f32 {
        match self {
            Self::Blacks => 4.0,
            Self::Dark => 4.0,
            Self::Shadows | Self::Light | Self::Highlights => 3.0,
            Self::Specular => 5.0,
        }
    }
}

/// One authored HDR luminance-zone correction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrZoneControl {
    pub center_stops: f32,
    pub width_stops: f32,
    pub exposure_stops: f32,
    pub saturation: f32,
    pub balance: [f32; 3],
}

impl HdrZoneControl {
    /// Neutral control with the product default range for `zone`.
    pub const fn neutral(zone: HdrGradingZone) -> Self {
        Self {
            center_stops: zone.default_center_stops(),
            width_stops: zone.default_width_stops(),
            exposure_stops: 0.0,
            saturation: 1.0,
            balance: [0.0; 3],
        }
    }
}

/// Complete authored HDR palette state evaluated for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrGradingAuthoring {
    pub global_exposure_stops: f32,
    pub global_saturation: f32,
    pub global_balance: [f32; 3],
    pub zones: [HdrZoneControl; HDR_GRADING_ZONE_COUNT],
}

impl Default for HdrGradingAuthoring {
    fn default() -> Self {
        Self {
            global_exposure_stops: 0.0,
            global_saturation: 1.0,
            global_balance: [0.0; 3],
            zones: HdrGradingZone::ALL.map(HdrZoneControl::neutral),
        }
    }
}

/// Invalid authored HDR grading state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HdrGradingError {
    #[error("HDR grading control `{control}` is non-finite or outside its semantic range")]
    InvalidControl { control: String },
}

/// Immutable sampled HDR grading resource shared by CPU and GPU backends.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedHdrGrading {
    samples: Box<[[f32; 4]]>,
    luminance_coefficients: [f32; 3],
    semantic_fingerprint: [u8; 32],
    identity: bool,
}

impl PreparedHdrGrading {
    /// Validate author controls and compile the exact sampled execution table.
    pub fn new(
        authoring: HdrGradingAuthoring,
        working_color_space: WorkingColorSpace,
    ) -> Result<Self, HdrGradingError> {
        validate_authoring(&authoring)?;
        let luminance_coefficients = luminance_coefficients(working_color_space);
        let identity = authoring_is_identity(&authoring);
        let mut samples = Vec::with_capacity(HDR_GRADING_SAMPLE_COUNT * HDR_GRADING_SAMPLE_ROWS);
        let mut secondary = Vec::with_capacity(HDR_GRADING_SAMPLE_COUNT);
        for index in 0..HDR_GRADING_SAMPLE_COUNT {
            let normalized = index as f32 / (HDR_GRADING_SAMPLE_COUNT - 1) as f32;
            let stops = HDR_GRADING_MIN_STOPS
                + normalized * (HDR_GRADING_MAX_STOPS - HDR_GRADING_MIN_STOPS);
            let sample = compile_sample(stops, &authoring, luminance_coefficients);
            samples.push([
                sample.exposure_gain,
                sample.saturation,
                sample.balance_gain[0],
                sample.balance_gain[1],
            ]);
            secondary.push([sample.balance_gain[2], 0.0, 0.0, 0.0]);
        }
        samples.extend(secondary);
        let semantic_fingerprint = semantic_fingerprint(&samples, luminance_coefficients);
        Ok(Self {
            samples: samples.into_boxed_slice(),
            luminance_coefficients,
            semantic_fingerprint,
            identity,
        })
    }

    /// Complete immutable semantic identity of the sampled transform.
    pub const fn semantic_fingerprint(&self) -> &[u8; 32] {
        &self.semantic_fingerprint
    }

    /// Exact table rows used by CPU execution and GPU upload.
    pub fn samples(&self) -> &[[f32; 4]] {
        &self.samples
    }

    /// CIE-Y coefficients for the authored working space.
    pub const fn luminance_coefficients(&self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// Whether all creative controls are neutral.
    pub const fn is_identity(&self) -> bool {
        self.identity
    }

    /// Apply the sampled grade without clamping extended scene-linear values.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.identity {
            return rgb;
        }
        let luminance = dot3(rgb, self.luminance_coefficients).max(MIN_LUMINANCE);
        let stops = (luminance / REFERENCE_GREY).log2();
        let sample = self.sample(stops);
        let balanced = [
            rgb[0] * sample.balance_gain[0],
            rgb[1] * sample.balance_gain[1],
            rgb[2] * sample.balance_gain[2],
        ];
        let exposed = balanced.map(|channel| channel * sample.exposure_gain);
        let exposed_luma = dot3(exposed, self.luminance_coefficients);
        std::array::from_fn(|channel| {
            exposed_luma + (exposed[channel] - exposed_luma) * sample.saturation
        })
    }

    /// Conservative retained host bytes charged to prepared-program ownership.
    pub fn retained_bytes_estimate(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.samples.len().saturating_mul(std::mem::size_of::<[f32; 4]>()))
    }

    fn sample(&self, stops: f32) -> CompiledSample {
        let coordinate = ((stops.clamp(HDR_GRADING_MIN_STOPS, HDR_GRADING_MAX_STOPS)
            - HDR_GRADING_MIN_STOPS)
            / (HDR_GRADING_MAX_STOPS - HDR_GRADING_MIN_STOPS))
            * (HDR_GRADING_SAMPLE_COUNT - 1) as f32;
        let lower = coordinate.floor() as usize;
        let upper = (lower + 1).min(HDR_GRADING_SAMPLE_COUNT - 1);
        let fraction = coordinate - lower as f32;
        let first0 = self.samples[lower];
        let next0 = self.samples[upper];
        let first1 = self.samples[HDR_GRADING_SAMPLE_COUNT + lower];
        let next1 = self.samples[HDR_GRADING_SAMPLE_COUNT + upper];
        CompiledSample {
            exposure_gain: lerp(first0[0], next0[0], fraction),
            saturation: lerp(first0[1], next0[1], fraction),
            balance_gain: [
                lerp(first0[2], next0[2], fraction),
                lerp(first0[3], next0[3], fraction),
                lerp(first1[0], next1[0], fraction),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CompiledSample {
    exposure_gain: f32,
    saturation: f32,
    balance_gain: [f32; 3],
}

fn validate_authoring(authoring: &HdrGradingAuthoring) -> Result<(), HdrGradingError> {
    validate_range(
        "global_exposure",
        authoring.global_exposure_stops,
        -8.0,
        8.0,
    )?;
    validate_range("global_saturation", authoring.global_saturation, 0.0, 4.0)?;
    validate_balance("global_balance", authoring.global_balance)?;
    for (zone, control) in HdrGradingZone::ALL.into_iter().zip(authoring.zones) {
        let prefix = zone.parameter_prefix();
        validate_range(
            &format!("{prefix}_center"),
            control.center_stops,
            -16.0,
            16.0,
        )?;
        validate_range(&format!("{prefix}_width"), control.width_stops, 0.25, 12.0)?;
        validate_range(
            &format!("{prefix}_exposure"),
            control.exposure_stops,
            -8.0,
            8.0,
        )?;
        validate_range(
            &format!("{prefix}_saturation"),
            control.saturation,
            0.0,
            4.0,
        )?;
        validate_balance(&format!("{prefix}_balance"), control.balance)?;
    }
    Ok(())
}

fn validate_range(
    control: &str,
    value: f32,
    minimum: f32,
    maximum: f32,
) -> Result<(), HdrGradingError> {
    if value.is_finite() && (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(HdrGradingError::InvalidControl { control: control.to_owned() })
    }
}

fn validate_balance(control: &str, value: [f32; 3]) -> Result<(), HdrGradingError> {
    if value
        .into_iter()
        .all(|channel| channel.is_finite() && (-1.0..=1.0).contains(&channel))
    {
        Ok(())
    } else {
        Err(HdrGradingError::InvalidControl { control: control.to_owned() })
    }
}

fn authoring_is_identity(authoring: &HdrGradingAuthoring) -> bool {
    authoring.global_exposure_stops.abs() <= IDENTITY_EPSILON
        && (authoring.global_saturation - 1.0).abs() <= IDENTITY_EPSILON
        && vector_near_zero(authoring.global_balance)
        && authoring.zones.iter().all(|zone| {
            zone.exposure_stops.abs() <= IDENTITY_EPSILON
                && (zone.saturation - 1.0).abs() <= IDENTITY_EPSILON
                && vector_near_zero(zone.balance)
        })
}

fn compile_sample(
    stops: f32,
    authoring: &HdrGradingAuthoring,
    luminance_coefficients: [f32; 3],
) -> CompiledSample {
    let mut exposure_stops = authoring.global_exposure_stops;
    let mut saturation = authoring.global_saturation;
    let mut balance = authoring.global_balance;
    for control in authoring.zones {
        let weight = zone_weight(stops, control.center_stops, control.width_stops);
        exposure_stops += control.exposure_stops * weight;
        saturation += (control.saturation - 1.0) * weight;
        for (accumulated, contribution) in balance.iter_mut().zip(control.balance) {
            *accumulated += contribution * weight;
        }
    }
    let mut balance_gain = balance.map(|channel| (channel * BALANCE_GAIN_STOPS).exp2());
    let gain_luma = dot3(balance_gain, luminance_coefficients);
    if gain_luma.is_finite() && gain_luma > f32::EPSILON {
        balance_gain = balance_gain.map(|channel| channel / gain_luma);
    }
    CompiledSample {
        exposure_gain: exposure_stops.clamp(-16.0, 16.0).exp2(),
        saturation: saturation.clamp(0.0, 8.0),
        balance_gain,
    }
}

#[inline]
fn zone_weight(stops: f32, center: f32, width: f32) -> f32 {
    let normalized = (1.0 - (stops - center).abs() / width).clamp(0.0, 1.0);
    normalized * normalized * normalized * (normalized * (normalized * 6.0 - 15.0) + 10.0)
}

fn luminance_coefficients(space: WorkingColorSpace) -> [f32; 3] {
    match space {
        WorkingColorSpace::LinearRec709 => [0.212_639, 0.715_169, 0.072_192],
        WorkingColorSpace::LinearRec2020 => [0.262_7, 0.678_0, 0.059_3],
        WorkingColorSpace::LinearP3D65 => [0.228_975, 0.691_739, 0.079_287],
        WorkingColorSpace::AcesCg => [0.272_229, 0.674_082, 0.053_689],
    }
}

fn semantic_fingerprint(samples: &[[f32; 4]], luminance: [f32; 3]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.prepared-hdr-grading.v1");
    hasher.update((HDR_GRADING_SAMPLE_COUNT as u64).to_le_bytes());
    hasher.update(HDR_GRADING_MIN_STOPS.to_bits().to_le_bytes());
    hasher.update(HDR_GRADING_MAX_STOPS.to_bits().to_le_bytes());
    for value in luminance {
        hasher.update(value.to_bits().to_le_bytes());
    }
    for sample in samples {
        for value in sample {
            hasher.update(value.to_bits().to_le_bytes());
        }
    }
    hasher.finalize().into()
}

#[inline]
fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

#[inline]
fn lerp(left: f32, right: f32, amount: f32) -> f32 {
    left + (right - left) * amount
}

fn vector_near_zero(value: [f32; 3]) -> bool {
    value.into_iter().all(|channel| channel.abs() <= IDENTITY_EPSILON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_grade_is_exact_identity_across_extended_values() {
        let grade = PreparedHdrGrading::new(
            HdrGradingAuthoring::default(),
            WorkingColorSpace::LinearRec2020,
        )
        .expect("neutral HDR grade");
        assert!(grade.is_identity());
        for rgb in [[0.0; 3], [0.18; 3], [4.0, 2.0, 1.0], [-0.1, 0.2, 0.3]] {
            assert_eq!(grade.apply(rgb), rgb);
        }
    }

    #[test]
    fn one_zone_is_local_and_smooth_in_log_luminance() {
        let mut authoring = HdrGradingAuthoring::default();
        authoring.zones[HdrGradingZone::Highlights as usize].exposure_stops = 1.0;
        let grade = PreparedHdrGrading::new(authoring, WorkingColorSpace::LinearRec2020)
            .expect("highlight grade");
        let at_center = grade.apply([0.18 * 8.0; 3])[0];
        let at_edge = grade.apply([0.18; 3])[0];
        let outside = grade.apply([0.18 / 16.0; 3])[0];
        assert!((at_center - 2.88).abs() < 0.02, "{at_center}");
        assert!((at_edge - 0.18).abs() < 0.002, "{at_edge}");
        assert!((outside - 0.01125).abs() < 0.0002, "{outside}");
    }

    #[test]
    fn balance_preserves_neutral_luminance_before_exposure() {
        let mut authoring = HdrGradingAuthoring {
            global_balance: [0.5, -0.25, 0.1],
            ..HdrGradingAuthoring::default()
        };
        authoring.global_saturation = 1.0;
        let grade = PreparedHdrGrading::new(authoring, WorkingColorSpace::LinearRec709)
            .expect("balanced grade");
        let input = [0.18; 3];
        let output = grade.apply(input);
        let coefficients = grade.luminance_coefficients();
        assert!((dot3(input, coefficients) - dot3(output, coefficients)).abs() < 2.0e-5);
        assert_ne!(output, input);
    }

    #[test]
    fn invalid_zone_ranges_fail_closed() {
        let mut authoring = HdrGradingAuthoring::default();
        authoring.zones[0].width_stops = 0.0;
        assert!(matches!(
            PreparedHdrGrading::new(authoring, WorkingColorSpace::LinearRec709),
            Err(HdrGradingError::InvalidControl { control }) if control == "blacks_width"
        ));
    }

    #[test]
    fn fingerprint_covers_working_space_and_every_sample() {
        let mut authoring = HdrGradingAuthoring::default();
        authoring.zones[2].saturation = 1.25;
        let rec709 = PreparedHdrGrading::new(authoring, WorkingColorSpace::LinearRec709)
            .expect("Rec.709 grade");
        let rec2020 = PreparedHdrGrading::new(authoring, WorkingColorSpace::LinearRec2020)
            .expect("Rec.2020 grade");
        assert_ne!(
            rec709.semantic_fingerprint(),
            rec2020.semantic_fingerprint()
        );
        assert_eq!(
            rec709.samples().len(),
            HDR_GRADING_SAMPLE_COUNT * HDR_GRADING_SAMPLE_ROWS
        );
    }
}
