//! Unbounded working-RGB hue, saturation, and lightness adjustment.

/// Prepared controls for a scene-linear, unclamped HSL adjustment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HslGrade {
    hue_degrees: f32,
    saturation: f32,
    lightness: f32,
}

impl HslGrade {
    /// Validate authored controls before execution.
    pub fn new(hue_degrees: f32, saturation: f32, lightness: f32) -> Result<Self, HslGradeError> {
        if !hue_degrees.is_finite() || !(-180.0..=180.0).contains(&hue_degrees) {
            return Err(HslGradeError::Hue);
        }
        if !saturation.is_finite() || !(0.0..=2.0).contains(&saturation) {
            return Err(HslGradeError::Saturation);
        }
        if !lightness.is_finite() || !(-1.0..=1.0).contains(&lightness) {
            return Err(HslGradeError::Lightness);
        }
        Ok(Self { hue_degrees, saturation, lightness })
    }

    /// Whether the three controls leave every RGB value exactly unchanged.
    pub fn is_identity(self) -> bool {
        self.hue_degrees == 0.0 && self.saturation == 1.0 && self.lightness == 0.0
    }

    /// Apply the controls without clipping negative or extended-range RGB.
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        if self.is_identity() {
            return rgb;
        }
        let mut adjusted = rgb;
        if self.hue_degrees != 0.0 {
            let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
            let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
            let chroma = maximum - minimum;
            if chroma > f32::EPSILON && chroma.is_finite() {
                let sector = if maximum == rgb[0] {
                    ((rgb[1] - rgb[2]) / chroma).rem_euclid(6.0)
                } else if maximum == rgb[1] {
                    (rgb[2] - rgb[0]) / chroma + 2.0
                } else {
                    (rgb[0] - rgb[1]) / chroma + 4.0
                };
                let rotated = (sector + self.hue_degrees / 60.0).rem_euclid(6.0);
                let second = chroma * (1.0 - (rotated.rem_euclid(2.0) - 1.0).abs());
                let primary = match rotated.floor() as u32 {
                    0 => [chroma, second, 0.0],
                    1 => [second, chroma, 0.0],
                    2 => [0.0, chroma, second],
                    3 => [0.0, second, chroma],
                    4 => [second, 0.0, chroma],
                    _ => [chroma, 0.0, second],
                };
                adjusted = primary.map(|channel| channel + minimum);
            }
        }
        if self.saturation != 1.0 {
            let maximum = adjusted[0].max(adjusted[1]).max(adjusted[2]);
            let minimum = adjusted[0].min(adjusted[1]).min(adjusted[2]);
            let midpoint = (maximum + minimum) * 0.5;
            adjusted = adjusted.map(|channel| midpoint + (channel - midpoint) * self.saturation);
        }
        if self.lightness != 0.0 {
            adjusted = adjusted.map(|channel| channel + self.lightness);
        }
        adjusted
    }

    /// Exact author controls for graph and cache identity.
    pub const fn controls(self) -> [f32; 3] {
        [self.hue_degrees, self.saturation, self.lightness]
    }
}

/// Invalid authored HSL control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HslGradeError {
    /// Hue is non-finite or outside the supported half-turn range.
    #[error("HSL hue must be finite and within [-180, 180] degrees")]
    Hue,
    /// Saturation is non-finite or outside the supported multiplier range.
    #[error("HSL saturation must be finite and within [0, 2]")]
    Saturation,
    /// Lightness is non-finite or outside the supported additive range.
    #[error("HSL lightness must be finite and within [-1, 1]")]
    Lightness,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_turn_and_saturation_preserve_extended_range() {
        let rotated = HslGrade::new(180.0, 1.0, 0.0).expect("hue").apply([2.0, -1.0, -1.0]);
        assert_eq!(rotated, [-1.0, 2.0, 2.0]);
        let neutral = HslGrade::new(0.0, 0.0, 0.0).expect("saturation").apply([2.0, -1.0, -1.0]);
        assert_eq!(neutral, [0.5, 0.5, 0.5]);
    }

    #[test]
    fn identity_is_exact_and_invalid_controls_fail() {
        let input = [2.0, -0.25, 0.75];
        assert_eq!(
            HslGrade::new(0.0, 1.0, 0.0).expect("identity").apply(input),
            input
        );
        assert_eq!(HslGrade::new(f32::NAN, 1.0, 0.0), Err(HslGradeError::Hue));
        assert_eq!(HslGrade::new(0.0, 2.1, 0.0), Err(HslGradeError::Saturation));
        assert_eq!(
            HslGrade::new(0.0, 1.0, f32::INFINITY),
            Err(HslGradeError::Lightness)
        );
    }
}
