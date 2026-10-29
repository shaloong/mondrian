//! Hue, saturation, and lightness adjustment scoped to one authored hue band.
//!
//! The grade is the effect-list "Hue/Saturation" shape: one set of hue,
//! saturation, and lightness controls plus an optional colorize stage, applied
//! either to the whole frame or to one of six 60-degree hue bands.
//!
//! Band scoping uses a smooth circular mask: the band carries full weight within
//! 30 degrees of its center and fades to zero at 60 degrees, so neighbouring
//! bands overlap without a visible seam. Every operation stays in unclamped
//! working-space RGB -- extended-range and negative channels survive, and a
//! neutral pixel (chroma at or below [`hsl_space::NEUTRAL_CHROMA`]) only receives
//! the lightness offset.

use crate::hsl_space::{
    from_hue_chroma_lightness, hue_distance_turns, hue_turns, smoothstep, NEUTRAL_CHROMA,
};

/// Half-width of the full-weight part of a hue band, in turns (30 degrees).
const BAND_PLATEAU_TURNS: f32 = 1.0 / 12.0;

/// Half-width at which a hue band has faded to zero, in turns (60 degrees).
const BAND_EDGE_TURNS: f32 = 1.0 / 6.0;

/// Hue band targeted by the grade's color controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HueBand {
    /// Every hue is affected at full weight.
    Master,
    /// Reds, centered on 0 degrees.
    Reds,
    /// Yellows, centered on 60 degrees.
    Yellows,
    /// Greens, centered on 120 degrees.
    Greens,
    /// Cyans, centered on 180 degrees.
    Cyans,
    /// Blues, centered on 240 degrees.
    Blues,
    /// Magentas, centered on 300 degrees.
    Magentas,
}

impl HueBand {
    /// Every band in authoring order.
    pub const ALL: [Self; 7] = [
        Self::Master,
        Self::Reds,
        Self::Yellows,
        Self::Greens,
        Self::Cyans,
        Self::Blues,
        Self::Magentas,
    ];

    /// Stable persistence key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Reds => "reds",
            Self::Yellows => "yellows",
            Self::Greens => "greens",
            Self::Cyans => "cyans",
            Self::Blues => "blues",
            Self::Magentas => "magentas",
        }
    }

    /// Parse a persisted key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|band| band.key() == key)
    }

    /// Author-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Master => "主色",
            Self::Reds => "红色",
            Self::Yellows => "黄色",
            Self::Greens => "绿色",
            Self::Cyans => "青色",
            Self::Blues => "蓝色",
            Self::Magentas => "洋红",
        }
    }

    /// Band center in turns, or `None` for the unbounded master band.
    pub const fn center_turns(self) -> Option<f32> {
        match self {
            Self::Master => None,
            Self::Reds => Some(0.0),
            Self::Yellows => Some(1.0 / 6.0),
            Self::Greens => Some(2.0 / 6.0),
            Self::Cyans => Some(3.0 / 6.0),
            Self::Blues => Some(4.0 / 6.0),
            Self::Magentas => Some(5.0 / 6.0),
        }
    }
}

/// Prepared controls for a hue-band-limited HSL adjustment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HueSaturationGrade {
    band: HueBand,
    hue_degrees: f32,
    saturation_percent: f32,
    lightness_percent: f32,
    colorize_amount: f32,
    tint_hue_degrees: f32,
    tint_saturation_percent: f32,
    tint_lightness_percent: f32,
}

impl HueSaturationGrade {
    /// Validate authored controls before execution.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        band: HueBand,
        hue_degrees: f32,
        saturation_percent: f32,
        lightness_percent: f32,
        colorize_amount: f32,
        tint_hue_degrees: f32,
        tint_saturation_percent: f32,
        tint_lightness_percent: f32,
    ) -> Result<Self, HueSaturationError> {
        if !hue_degrees.is_finite() || !(-180.0..=180.0).contains(&hue_degrees) {
            return Err(HueSaturationError::Hue);
        }
        if !saturation_percent.is_finite() || !(-100.0..=100.0).contains(&saturation_percent) {
            return Err(HueSaturationError::Saturation);
        }
        if !lightness_percent.is_finite() || !(-100.0..=100.0).contains(&lightness_percent) {
            return Err(HueSaturationError::Lightness);
        }
        if !colorize_amount.is_finite() || !(0.0..=1.0).contains(&colorize_amount) {
            return Err(HueSaturationError::Colorize);
        }
        if !tint_hue_degrees.is_finite() || !(-180.0..=180.0).contains(&tint_hue_degrees) {
            return Err(HueSaturationError::TintHue);
        }
        if !tint_saturation_percent.is_finite() || !(0.0..=100.0).contains(&tint_saturation_percent)
        {
            return Err(HueSaturationError::TintSaturation);
        }
        if !tint_lightness_percent.is_finite()
            || !(-100.0..=100.0).contains(&tint_lightness_percent)
        {
            return Err(HueSaturationError::TintLightness);
        }
        Ok(Self {
            band,
            hue_degrees,
            saturation_percent,
            lightness_percent,
            colorize_amount,
            tint_hue_degrees,
            tint_saturation_percent,
            tint_lightness_percent,
        })
    }

    /// Whether the grade leaves every working-space pixel exactly unchanged.
    pub fn is_identity(self) -> bool {
        self.hue_degrees == 0.0
            && self.saturation_percent == 0.0
            && self.lightness_percent == 0.0
            && self.colorize_amount == 0.0
    }

    /// Band selection.
    pub const fn band(self) -> HueBand {
        self.band
    }

    /// Weight of the authored band for one working-space pixel.
    pub fn band_weight(self, rgb: [f32; 3]) -> f32 {
        let Some(center) = self.band.center_turns() else {
            return 1.0;
        };
        let (high, low) = crate::hsl_space::extremes(rgb);
        let chroma = high - low;
        if !chroma.is_finite() || chroma <= NEUTRAL_CHROMA {
            return 0.0;
        }
        let hue = hue_turns(rgb, high, chroma);
        let distance = hue_distance_turns(hue, center);
        1.0 - smoothstep(BAND_PLATEAU_TURNS, BAND_EDGE_TURNS, distance)
    }

    /// Apply the grade without clipping negative or extended-range RGB.
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        if self.is_identity() {
            return rgb;
        }
        let mut adjusted = rgb;
        let weight = self.band_weight(rgb);
        if weight > 0.0 {
            let shifted = self.shifted(rgb);
            adjusted = mix3(rgb, shifted, weight);
        }
        if self.colorize_amount > 0.0 {
            let tinted = self.tinted(adjusted);
            adjusted = mix3(adjusted, tinted, self.colorize_amount);
        }
        adjusted
    }

    /// Hue rotation, chroma scale, and lightness offset before band mixing.
    fn shifted(self, rgb: [f32; 3]) -> [f32; 3] {
        let (high, low) = crate::hsl_space::extremes(rgb);
        let lightness = (high + low) * 0.5 + self.lightness_percent / 100.0;
        let chroma = high - low;
        if !chroma.is_finite() || chroma <= NEUTRAL_CHROMA {
            return [
                rgb[0] + self.lightness_percent / 100.0,
                rgb[1] + self.lightness_percent / 100.0,
                rgb[2] + self.lightness_percent / 100.0,
            ];
        }
        let hue = hue_turns(rgb, high, chroma) + self.hue_degrees / 360.0;
        let scale = 1.0 + self.saturation_percent / 100.0;
        from_hue_chroma_lightness(hue, chroma * scale, lightness)
    }

    /// Tint the pixel toward the authored tint while keeping its lightness.
    fn tinted(self, rgb: [f32; 3]) -> [f32; 3] {
        let (high, low) = crate::hsl_space::extremes(rgb);
        let lightness = (high + low) * 0.5 + self.tint_lightness_percent / 100.0;
        let range = (1.0 - (2.0 * lightness - 1.0).abs()).max(0.0);
        let chroma = range * (self.tint_saturation_percent / 100.0);
        from_hue_chroma_lightness(self.tint_hue_degrees / 360.0, chroma, lightness)
    }

    /// Exact authored controls in turns and normalized units.
    ///
    /// Layout: hue shift (turns), saturation scale, lightness offset, colorize
    /// amount, tint hue (turns), tint saturation fraction, tint lightness offset,
    /// band center (turns, negative for the master band). This is the exact
    /// payload the GPU point op carries, so the two backends cannot diverge.
    pub fn gpu_controls(self) -> [f32; 8] {
        [
            self.hue_degrees / 360.0,
            1.0 + self.saturation_percent / 100.0,
            self.lightness_percent / 100.0,
            self.colorize_amount,
            self.tint_hue_degrees / 360.0,
            self.tint_saturation_percent / 100.0,
            self.tint_lightness_percent / 100.0,
            self.band.center_turns().unwrap_or(-1.0),
        ]
    }

    /// Exact author controls for graph and cache identity.
    pub fn author_controls(self) -> [f32; 7] {
        [
            self.hue_degrees,
            self.saturation_percent,
            self.lightness_percent,
            self.colorize_amount,
            self.tint_hue_degrees,
            self.tint_saturation_percent,
            self.tint_lightness_percent,
        ]
    }
}

/// Linear interpolation between two working-space triples.
fn mix3(from: [f32; 3], to: [f32; 3], amount: f32) -> [f32; 3] {
    [
        from[0] + (to[0] - from[0]) * amount,
        from[1] + (to[1] - from[1]) * amount,
        from[2] + (to[2] - from[2]) * amount,
    ]
}

/// Invalid authored hue/saturation control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HueSaturationError {
    /// Hue is non-finite or outside the supported half-turn range.
    #[error("hue/saturation hue must be finite and within [-180, 180] degrees")]
    Hue,
    /// Saturation is non-finite or outside the supported percent range.
    #[error("hue/saturation saturation must be finite and within [-100, 100] percent")]
    Saturation,
    /// Lightness is non-finite or outside the supported percent range.
    #[error("hue/saturation lightness must be finite and within [-100, 100] percent")]
    Lightness,
    /// Colorize amount is non-finite or outside `[0, 1]`.
    #[error("hue/saturation colorize amount must be finite and within [0, 1]")]
    Colorize,
    /// Tint hue is non-finite or outside the supported half-turn range.
    #[error("hue/saturation tint hue must be finite and within [-180, 180] degrees")]
    TintHue,
    /// Tint saturation is non-finite or outside `[0, 100]` percent.
    #[error("hue/saturation tint saturation must be finite and within [0, 100] percent")]
    TintSaturation,
    /// Tint lightness is non-finite or outside the supported percent range.
    #[error("hue/saturation tint lightness must be finite and within [-100, 100] percent")]
    TintLightness,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(band: HueBand, hue: f32, saturation: f32, lightness: f32) -> HueSaturationGrade {
        HueSaturationGrade::new(band, hue, saturation, lightness, 0.0, 0.0, 25.0, 0.0)
            .expect("valid grade")
    }

    #[test]
    fn band_keys_are_stable_and_unique() {
        let mut keys = Vec::new();
        for band in HueBand::ALL {
            assert_eq!(HueBand::from_key(band.key()), Some(band));
            assert!(!keys.contains(&band.key()));
            keys.push(band.key());
        }
        assert_eq!(HueBand::from_key("unknown"), None);
    }

    #[test]
    fn master_band_is_the_unbounded_identity_of_the_mask() {
        let master = grade(HueBand::Master, 0.0, 0.0, 0.0);
        assert_eq!(master.band_weight([0.9, 0.1, 0.1]), 1.0);
        assert_eq!(master.band_weight([0.2, 0.2, 0.2]), 1.0);
        let reds = grade(HueBand::Reds, 0.0, 0.0, 0.0);
        assert_eq!(reds.band_weight([0.2, 0.2, 0.2]), 0.0);
        assert!(reds.band_weight([0.9, 0.1, 0.1]) > 0.999);
    }

    #[test]
    fn band_mask_fades_between_thirty_and_sixty_degrees() {
        let reds = grade(HueBand::Reds, 0.0, 0.0, 0.0);
        let near = reds.band_weight(from_hue_chroma_lightness(1.0 / 24.0, 0.5, 0.5));
        let mid = reds.band_weight(from_hue_chroma_lightness(1.0 / 8.0, 0.5, 0.5));
        let far = reds.band_weight(from_hue_chroma_lightness(1.0 / 3.0, 0.5, 0.5));
        assert_eq!(near, 1.0);
        assert!(mid > 0.0 && mid < 1.0);
        assert!(far.abs() <= 1.0e-6);
    }

    #[test]
    fn identity_grade_leaves_extended_range_untouched() {
        let identity = grade(HueBand::Master, 0.0, 0.0, 0.0);
        assert!(identity.is_identity());
        assert_eq!(identity.apply([2.0, -0.5, 0.25]), [2.0, -0.5, 0.25]);
    }

    #[test]
    fn master_hue_and_saturation_match_the_plain_hsl_grade() {
        let input = [0.8, 0.3, 0.1];
        let expected = crate::HslGrade::new(90.0, 1.5, 0.0).expect("hsl").apply(input);
        let produced = grade(HueBand::Master, 90.0, 50.0, 0.0).apply(input);
        for channel in 0..3 {
            assert!((produced[channel] - expected[channel]).abs() < 1.0e-6);
        }
    }

    #[test]
    fn band_grade_leaves_pixels_outside_the_band_untouched() {
        let greens = grade(HueBand::Greens, 120.0, -100.0, 0.5);
        let red = [0.8, 0.1, 0.1];
        assert_eq!(greens.apply(red), red);
        let green = [0.1, 0.8, 0.1];
        assert_ne!(greens.apply(green), green);
    }

    #[test]
    fn lightness_offset_applies_to_neutral_pixels_under_the_master_band() {
        let boost = grade(HueBand::Master, 0.0, 0.0, 50.0);
        for channel in boost.apply([0.2, 0.2, 0.2]) {
            assert!((channel - 0.7).abs() < 1.0e-6, "neutral boost {channel}");
        }
    }

    #[test]
    fn band_grade_leaves_neutral_pixels_untouched() {
        let boost = grade(HueBand::Reds, 0.0, 0.0, 50.0);
        assert_eq!(boost.apply([0.2, 0.2, 0.2]), [0.2, 0.2, 0.2]);
    }

    #[test]
    fn colorize_moves_the_pixel_toward_the_authored_tint() {
        let colorize =
            HueSaturationGrade::new(HueBand::Master, 0.0, 0.0, 0.0, 1.0, 180.0, 100.0, 0.0)
                .expect("valid colorize");
        let tinted = colorize.apply([0.5, 0.5, 0.5]);
        assert!(
            tinted[0] < tinted[1] && (tinted[1] - tinted[2]).abs() < 1.0e-6,
            "cyan tint should raise green and blue above red: {tinted:?}"
        );
        let high = tinted[0].max(tinted[1]).max(tinted[2]);
        let low = tinted[0].min(tinted[1]).min(tinted[2]);
        assert!(
            ((high + low) * 0.5 - 0.5).abs() < 1.0e-6,
            "colorize keeps lightness: {tinted:?}"
        );
        let half = HueSaturationGrade::new(HueBand::Master, 0.0, 0.0, 0.0, 0.5, 180.0, 100.0, 0.0)
            .expect("valid colorize");
        let mixed = half.apply([0.5, 0.5, 0.5]);
        for channel in 0..3 {
            let expected = 0.5 + (tinted[channel] - 0.5) * 0.5;
            assert!(
                (mixed[channel] - expected).abs() < 1.0e-6,
                "half colorize {mixed:?} should be halfway to {tinted:?}"
            );
        }
    }

    #[test]
    fn gpu_controls_round_trip_the_grade_fields() {
        let authored =
            HueSaturationGrade::new(HueBand::Cyans, 45.0, -25.0, 10.0, 0.75, -90.0, 40.0, -20.0)
                .expect("valid");
        let controls = authored.gpu_controls();
        assert!((controls[0] - 45.0 / 360.0).abs() < 1.0e-6);
        assert!((controls[1] - 0.75).abs() < 1.0e-6);
        assert!((controls[2] - 0.1).abs() < 1.0e-6);
        assert_eq!(controls[3], 0.75);
        assert!((controls[4] + 0.25).abs() < 1.0e-6);
        assert!((controls[5] - 0.4).abs() < 1.0e-6);
        assert!((controls[6] + 0.2).abs() < 1.0e-6);
        assert!((controls[7] - 0.5).abs() < 1.0e-6);
        assert!((authored.author_controls()[0] - 45.0).abs() < 1.0e-6);
        let master_controls = grade(HueBand::Master, 0.0, 0.0, 0.0).gpu_controls();
        assert!(master_controls[7] < 0.0);
    }

    #[test]
    fn invalid_controls_fail_closed() {
        assert_eq!(
            HueSaturationGrade::new(HueBand::Master, f32::NAN, 0.0, 0.0, 0.0, 0.0, 25.0, 0.0),
            Err(HueSaturationError::Hue)
        );
        assert_eq!(
            HueSaturationGrade::new(HueBand::Master, 0.0, 101.0, 0.0, 0.0, 0.0, 25.0, 0.0),
            Err(HueSaturationError::Saturation)
        );
        assert_eq!(
            HueSaturationGrade::new(HueBand::Master, 0.0, 0.0, -101.0, 0.0, 0.0, 25.0, 0.0),
            Err(HueSaturationError::Lightness)
        );
        assert_eq!(
            HueSaturationGrade::new(HueBand::Master, 0.0, 0.0, 0.0, 1.5, 0.0, 25.0, 0.0),
            Err(HueSaturationError::Colorize)
        );
        assert_eq!(
            HueSaturationGrade::new(HueBand::Master, 0.0, 0.0, 0.0, 0.0, 200.0, 25.0, 0.0),
            Err(HueSaturationError::TintHue)
        );
        assert_eq!(
            HueSaturationGrade::new(HueBand::Master, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0),
            Err(HueSaturationError::TintSaturation)
        );
        assert_eq!(
            HueSaturationGrade::new(
                HueBand::Master,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                25.0,
                f32::INFINITY
            ),
            Err(HueSaturationError::TintLightness)
        );
    }
}
