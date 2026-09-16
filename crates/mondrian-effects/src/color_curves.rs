//! Backend-neutral compiled RGB/YRGB and secondary color-curve execution.

use mondrian_core::{NormalizedCurve, NormalizedCurvePoint, WorkingColorSpace};
use sha2::{Digest, Sha256};

/// Number of uniformly spaced samples compiled for every authored curve.
pub const COLOR_CURVE_SAMPLE_COUNT: usize = 256;
/// Number of RGBA rows used to pack the ten curve channels.
pub const COLOR_CURVE_SAMPLE_ROWS: usize = 3;

/// Master-curve interpretation used before the per-channel curves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorCurvesMode {
    /// Apply the master curve independently to red, green, and blue.
    Rgb,
    /// Apply the master curve to working-space luminance and add its delta to RGB.
    YRgb,
}

/// Borrowed authoring curves compiled into one immutable execution resource.
pub struct ColorCurvesAuthoring<'a> {
    /// Master-curve interpretation.
    pub mode: ColorCurvesMode,
    /// Master RGB or luminance curve.
    pub master: &'a NormalizedCurve,
    /// Red-channel curve.
    pub red: &'a NormalizedCurve,
    /// Green-channel curve.
    pub green: &'a NormalizedCurve,
    /// Blue-channel curve.
    pub blue: &'a NormalizedCurve,
    /// Hue-dependent hue delta curve, neutral at 0.5.
    pub hue_vs_hue: &'a NormalizedCurve,
    /// Hue-dependent saturation delta curve, neutral at 0.5.
    pub hue_vs_saturation: &'a NormalizedCurve,
    /// Hue-dependent value delta curve, neutral at 0.5.
    pub hue_vs_luma: &'a NormalizedCurve,
    /// Luminance-dependent saturation delta curve, neutral at 0.5.
    pub luma_vs_saturation: &'a NormalizedCurve,
    /// Saturation-dependent saturation delta curve, neutral at 0.5.
    pub saturation_vs_saturation: &'a NormalizedCurve,
    /// Saturation-dependent value delta curve, neutral at 0.5.
    pub saturation_vs_luma: &'a NormalizedCurve,
}

/// Immutable sampled color-curve payload shared by CPU and GPU execution.
#[derive(Clone)]
pub struct PreparedColorCurves {
    mode: ColorCurvesMode,
    luminance_coefficients: [f32; 3],
    samples: Box<[[[f32; 4]; COLOR_CURVE_SAMPLE_COUNT]; COLOR_CURVE_SAMPLE_ROWS]>,
    semantic_fingerprint: [u8; 32],
    identity: bool,
    secondary_identity: bool,
}

impl std::fmt::Debug for PreparedColorCurves {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedColorCurves")
            .field("mode", &self.mode)
            .field("luminance_coefficients", &self.luminance_coefficients)
            .field("semantic_fingerprint", &self.semantic_fingerprint)
            .field("identity", &self.identity)
            .field("secondary_identity", &self.secondary_identity)
            .finish_non_exhaustive()
    }
}

impl PartialEq for PreparedColorCurves {
    fn eq(&self, other: &Self) -> bool {
        self.semantic_fingerprint == other.semantic_fingerprint && self.samples == other.samples
    }
}

impl PreparedColorCurves {
    /// Compile normalized authoring curves for one working color space.
    pub fn new(authoring: ColorCurvesAuthoring<'_>, working: WorkingColorSpace) -> Self {
        let secondary_identity = [
            authoring.hue_vs_hue,
            authoring.hue_vs_saturation,
            authoring.hue_vs_luma,
            authoring.luma_vs_saturation,
            authoring.saturation_vs_saturation,
            authoring.saturation_vs_luma,
        ]
        .into_iter()
        .all(curve_is_neutral_delta);
        let identity = curve_is_identity(authoring.master)
            && curve_is_identity(authoring.red)
            && curve_is_identity(authoring.green)
            && curve_is_identity(authoring.blue)
            && secondary_identity;
        let curves = [
            authoring.master,
            authoring.red,
            authoring.green,
            authoring.blue,
            authoring.hue_vs_hue,
            authoring.hue_vs_saturation,
            authoring.hue_vs_luma,
            authoring.luma_vs_saturation,
            authoring.saturation_vs_saturation,
            authoring.saturation_vs_luma,
        ];
        let mut samples = Box::new([[[0.0; 4]; COLOR_CURVE_SAMPLE_COUNT]; COLOR_CURVE_SAMPLE_ROWS]);
        for sample_index in 0..COLOR_CURVE_SAMPLE_COUNT {
            let x = sample_index as f32 / (COLOR_CURVE_SAMPLE_COUNT - 1) as f32;
            for (curve_index, curve) in curves.into_iter().enumerate() {
                samples[curve_index / 4][sample_index][curve_index % 4] =
                    sample_author_curve(curve, x);
            }
        }
        let luminance_coefficients = working.luminance_coefficients();
        let semantic_fingerprint = fingerprint(authoring.mode, luminance_coefficients, &samples);
        Self {
            mode: authoring.mode,
            luminance_coefficients,
            samples,
            semantic_fingerprint,
            identity,
            secondary_identity,
        }
    }

    /// Return the master-curve interpretation.
    pub const fn mode(&self) -> ColorCurvesMode {
        self.mode
    }

    /// Return CIE Y coefficients for the authored working color space.
    pub const fn luminance_coefficients(&self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// Return the packed 256-sample execution table.
    pub fn samples(&self) -> &[[[f32; 4]; COLOR_CURVE_SAMPLE_COUNT]; COLOR_CURVE_SAMPLE_ROWS] {
        &self.samples
    }

    /// Return the complete semantic fingerprint used for resource residency.
    pub const fn semantic_fingerprint(&self) -> &[u8; 32] {
        &self.semantic_fingerprint
    }

    /// Return whether all primary and secondary curves are neutral.
    pub const fn is_identity(&self) -> bool {
        self.identity
    }

    /// Return whether all secondary curves are neutral.
    ///
    /// Backends use this to bypass RGB/HSV conversion exactly, preserving
    /// extended-range and negative scene-linear channel values.
    pub const fn secondary_identity(&self) -> bool {
        self.secondary_identity
    }

    /// Apply the compiled curves to one unbounded working-space RGB value.
    pub fn apply(&self, mut rgb: [f32; 3]) -> [f32; 3] {
        match self.mode {
            ColorCurvesMode::Rgb => {
                for channel in &mut rgb {
                    *channel = self.sample(0, 0, *channel);
                }
            }
            ColorCurvesMode::YRgb => {
                let luminance = dot3(rgb, self.luminance_coefficients);
                let delta = self.sample(0, 0, luminance) - luminance;
                rgb = [rgb[0] + delta, rgb[1] + delta, rgb[2] + delta];
            }
        }
        rgb = [
            self.sample(0, 1, rgb[0]),
            self.sample(0, 2, rgb[1]),
            self.sample(0, 3, rgb[2]),
        ];
        if self.secondary_identity {
            rgb
        } else {
            self.apply_secondary_curves(rgb)
        }
    }

    fn sample(&self, row: usize, component: usize, x: f32) -> f32 {
        sample_compiled_curve(&self.samples[row], component, x)
    }

    fn apply_secondary_curves(&self, rgb: [f32; 3]) -> [f32; 3] {
        let (hue, saturation, value) = rgb_to_hsv(rgb);
        let luminance = dot3(rgb, self.luminance_coefficients).clamp(0.0, 1.0);
        let hue_delta = self.sample(1, 0, hue) - 0.5;
        let saturation_delta = (self.sample(1, 1, hue) - 0.5)
            + (self.sample(1, 3, luminance) - 0.5)
            + (self.sample(2, 0, saturation) - 0.5);
        let value_delta = (self.sample(1, 2, hue) - 0.5) + (self.sample(2, 1, saturation) - 0.5);
        hsv_to_rgb(
            (hue + hue_delta).rem_euclid(1.0),
            (saturation + saturation_delta).clamp(0.0, 1.0),
            value + value_delta,
        )
    }
}

fn curve_is_identity(curve: &NormalizedCurve) -> bool {
    curve.points().iter().all(|point| (point.x - point.y).abs() <= f32::EPSILON)
}

fn curve_is_neutral_delta(curve: &NormalizedCurve) -> bool {
    curve.points().iter().all(|point| (point.y - 0.5).abs() <= f32::EPSILON)
}

fn sample_author_curve(curve: &NormalizedCurve, x: f32) -> f32 {
    let points = curve.points();
    let segment = points
        .windows(2)
        .position(|window| x <= window[1].x)
        .unwrap_or(points.len() - 2);
    let left = points[segment];
    let right = points[segment + 1];
    let width = right.x - left.x;
    let t = ((x - left.x) / width).clamp(0.0, 1.0);
    let slopes = monotone_slopes(points);
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * left.y
        + (t3 - 2.0 * t2 + t) * width * slopes[segment]
        + (-2.0 * t3 + 3.0 * t2) * right.y
        + (t3 - t2) * width * slopes[segment + 1]
}

fn monotone_slopes(points: &[NormalizedCurvePoint]) -> Vec<f32> {
    let secants = points
        .windows(2)
        .map(|pair| (pair[1].y - pair[0].y) / (pair[1].x - pair[0].x))
        .collect::<Vec<_>>();
    let mut slopes = vec![0.0; points.len()];
    slopes[0] = secants[0];
    slopes[points.len() - 1] = secants[secants.len() - 1];
    for index in 1..points.len() - 1 {
        let before = secants[index - 1];
        let after = secants[index];
        slopes[index] = if before * after <= 0.0 {
            0.0
        } else {
            2.0 / (1.0 / before + 1.0 / after)
        };
    }
    slopes
}

fn sample_compiled_curve(
    row: &[[f32; 4]; COLOR_CURVE_SAMPLE_COUNT],
    component: usize,
    x: f32,
) -> f32 {
    let scale = (COLOR_CURVE_SAMPLE_COUNT - 1) as f32;
    if x <= 0.0 {
        return row[0][component] + (row[1][component] - row[0][component]) * x * scale;
    }
    if x >= 1.0 {
        let last = COLOR_CURVE_SAMPLE_COUNT - 1;
        return row[last][component]
            + (row[last][component] - row[last - 1][component]) * (x - 1.0) * scale;
    }
    let position = x * scale;
    let lower = position.floor() as usize;
    let fraction = position - lower as f32;
    row[lower][component] + (row[lower + 1][component] - row[lower][component]) * fraction
}

fn rgb_to_hsv(rgb: [f32; 3]) -> (f32, f32, f32) {
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    let chroma = maximum - minimum;
    if chroma.abs() <= f32::EPSILON {
        return (0.0, 0.0, maximum);
    }
    let sector = if maximum == rgb[0] {
        ((rgb[1] - rgb[2]) / chroma).rem_euclid(6.0)
    } else if maximum == rgb[1] {
        (rgb[2] - rgb[0]) / chroma + 2.0
    } else {
        (rgb[0] - rgb[1]) / chroma + 4.0
    };
    let saturation = if maximum.abs() <= f32::EPSILON {
        0.0
    } else {
        (chroma / maximum).clamp(0.0, 1.0)
    };
    (sector / 6.0, saturation, maximum)
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    let sector = hue.rem_euclid(1.0) * 6.0;
    let chroma = value * saturation;
    let x = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let base = match sector.floor() as i32 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    let minimum = value - chroma;
    [base[0] + minimum, base[1] + minimum, base[2] + minimum]
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn fingerprint(
    mode: ColorCurvesMode,
    luminance: [f32; 3],
    samples: &[[[f32; 4]; COLOR_CURVE_SAMPLE_COUNT]; COLOR_CURVE_SAMPLE_ROWS],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.prepared-color-curves.v1");
    hasher.update([if matches!(mode, ColorCurvesMode::Rgb) {
        0
    } else {
        1
    }]);
    for value in luminance.into_iter().chain(samples.iter().flatten().flatten().copied()) {
        hasher.update(value.to_bits().to_le_bytes());
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral(mode: ColorCurvesMode) -> PreparedColorCurves {
        let identity = NormalizedCurve::identity();
        let flat = NormalizedCurve::flat(0.5).expect("neutral curve");
        PreparedColorCurves::new(
            ColorCurvesAuthoring {
                mode,
                master: &identity,
                red: &identity,
                green: &identity,
                blue: &identity,
                hue_vs_hue: &flat,
                hue_vs_saturation: &flat,
                hue_vs_luma: &flat,
                luma_vs_saturation: &flat,
                saturation_vs_saturation: &flat,
                saturation_vs_luma: &flat,
            },
            WorkingColorSpace::LinearRec2020,
        )
    }

    #[test]
    fn neutral_rgb_and_yrgb_curves_preserve_extended_values() {
        let sample = [-0.25, 0.18, 4.0];
        for mode in [ColorCurvesMode::Rgb, ColorCurvesMode::YRgb] {
            let grade = neutral(mode);
            assert!(grade.is_identity());
            let actual = grade.apply(sample);
            for channel in 0..3 {
                assert!(
                    (actual[channel] - sample[channel]).abs() <= 2.0e-5,
                    "{actual:?}"
                );
            }
        }
    }

    #[test]
    fn channel_and_hue_curves_change_intended_axes() {
        let identity = NormalizedCurve::identity();
        let flat = NormalizedCurve::flat(0.5).expect("neutral curve");
        let lifted_red = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.2),
            NormalizedCurvePoint::new(1.0, 1.0),
        ])
        .expect("red curve");
        let saturated_red = NormalizedCurve::new(vec![
            NormalizedCurvePoint::new(0.0, 0.8),
            NormalizedCurvePoint::new(1.0, 0.5),
        ])
        .expect("hue saturation curve");
        let grade = PreparedColorCurves::new(
            ColorCurvesAuthoring {
                mode: ColorCurvesMode::Rgb,
                master: &identity,
                red: &lifted_red,
                green: &identity,
                blue: &identity,
                hue_vs_hue: &flat,
                hue_vs_saturation: &saturated_red,
                hue_vs_luma: &flat,
                luma_vs_saturation: &flat,
                saturation_vs_saturation: &flat,
                saturation_vs_luma: &flat,
            },
            WorkingColorSpace::LinearRec709,
        );
        let actual = grade.apply([0.6, 0.2, 0.2]);
        assert!(actual[0] > 0.6, "{actual:?}");
        assert!(actual[0] - actual[1] > 0.4, "{actual:?}");
    }
}
