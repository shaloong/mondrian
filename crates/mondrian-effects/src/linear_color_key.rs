//! Linear colour key: removes a colour with a linear tolerance and softness ramp.
//!
//! The effect list gives five controls for this entry: a key colour, a "match
//! colours" mode, a tolerance, a softness, and a primary operation. The list does
//! not define the distance metric, so this module fixes one and documents it:
//!
//! * `rgb` matches on the largest per-channel difference, so a colour that is far
//!   away in any single channel is kept even when the other two match;
//! * `green` and `blue` match on that single channel only, which is what a spill
//!   key wants;
//! * `chroma` matches on the two-dimensional chromaticity `(r, g) / (r + g + b)`,
//!   so a brighter or darker version of the same hue keys out while a different
//!   hue at the same brightness does not.
//!
//! The alpha ramp is a smoothstep from `tolerance` to `tolerance + softness`, and
//! `keep_colours` inverts it. Coverage is scaled on the straight-alpha coverage
//! and the premultiplied colour follows it, so a keyed pixel stays consistently
//! premultiplied for the compositor.

/// Author-facing maximum for the tolerance and softness sliders, in percent.
pub const LINEAR_COLOR_KEY_MAX_PERCENT: f32 = 100.0;

/// How the key colour is compared with a pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchColours {
    /// Largest per-channel difference across red, green and blue.
    Rgb,
    /// Difference in the green channel only.
    Green,
    /// Difference in the blue channel only.
    Blue,
    /// Euclidean distance between normalised chromaticity vectors.
    Chroma,
}

impl MatchColours {
    /// Menu key used by the property schema.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Chroma => "chroma",
        }
    }

    /// Parses the menu key of the property schema.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "rgb" => Some(Self::Rgb),
            "green" => Some(Self::Green),
            "blue" => Some(Self::Blue),
            "chroma" => Some(Self::Chroma),
            _ => None,
        }
    }

    /// Menu value carried into the GPU uniform.
    pub fn as_u32(self) -> u32 {
        match self {
            Self::Rgb => 0,
            Self::Green => 1,
            Self::Blue => 2,
            Self::Chroma => 3,
        }
    }
}

/// Whether matching pixels are removed or protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOperation {
    /// Pixels close to the key colour become transparent.
    KeyColours,
    /// Pixels close to the key colour are kept, everything else is removed.
    KeepColours,
}

impl KeyOperation {
    /// Menu key used by the property schema.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::KeyColours => "key_colours",
            Self::KeepColours => "keep_colours",
        }
    }

    /// Parses the menu key of the property schema.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "key_colours" => Some(Self::KeyColours),
            "keep_colours" => Some(Self::KeepColours),
            _ => None,
        }
    }

    /// Menu value carried into the GPU uniform.
    pub fn as_u32(self) -> u32 {
        match self {
            Self::KeyColours => 0,
            Self::KeepColours => 1,
        }
    }
}

/// Rejected author state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinearColorKeyError {
    /// The key colour has a non-finite component.
    Colour,
    /// The tolerance or softness is outside `[0, 1]` after conversion.
    Amount,
}

impl std::fmt::Display for LinearColorKeyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Colour => formatter.write_str("linear colour key: the key colour must be finite"),
            Self::Amount => formatter.write_str(
                "linear colour key: the tolerance and softness must be finite and between 0 and 1",
            ),
        }
    }
}

impl std::error::Error for LinearColorKeyError {}

/// Fully resolved key parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearColorKeyGrade {
    key: [f32; 3],
    match_colours: MatchColours,
    tolerance: f32,
    softness: f32,
    operation: KeyOperation,
}

impl LinearColorKeyGrade {
    /// Builds a grade from author values.
    pub fn new(
        key: [f32; 3],
        match_colours: MatchColours,
        tolerance: f32,
        softness: f32,
        operation: KeyOperation,
    ) -> Result<Self, LinearColorKeyError> {
        if key.iter().any(|component| !component.is_finite()) {
            return Err(LinearColorKeyError::Colour);
        }
        for amount in [tolerance, softness] {
            if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
                return Err(LinearColorKeyError::Amount);
            }
        }
        Ok(Self { key, match_colours, tolerance, softness, operation })
    }

    /// The colour being keyed.
    pub fn key(&self) -> [f32; 3] {
        self.key
    }

    /// The comparison mode.
    pub fn match_colours(&self) -> MatchColours {
        self.match_colours
    }

    /// Lower edge of the alpha ramp.
    pub fn tolerance(&self) -> f32 {
        self.tolerance
    }

    /// Width of the alpha ramp.
    pub fn softness(&self) -> f32 {
        self.softness
    }

    /// Whether matching pixels are removed or protected.
    pub fn operation(&self) -> KeyOperation {
        self.operation
    }

    /// The effect always changes alpha, so a default node is not an identity.
    pub fn is_identity(&self) -> bool {
        false
    }

    /// A key with no tolerance and no softness still keys exact matches only.
    pub fn keys_exact_matches_only(&self) -> bool {
        self.tolerance <= 0.0 && self.softness <= 0.0
    }

    /// Distance between the key colour and `rgb`, in the selected metric.
    pub fn colour_distance(&self, rgb: [f32; 3]) -> f32 {
        let difference = [
            rgb[0] - self.key[0],
            rgb[1] - self.key[1],
            rgb[2] - self.key[2],
        ];
        match self.match_colours {
            MatchColours::Rgb => {
                difference.iter().fold(0.0_f32, |largest, value| largest.max(value.abs()))
            }
            MatchColours::Green => difference[1].abs(),
            MatchColours::Blue => difference[2].abs(),
            MatchColours::Chroma => {
                let key_chroma = chromaticity(self.key);
                let pixel_chroma = chromaticity(rgb);
                let dx = pixel_chroma[0] - key_chroma[0];
                let dy = pixel_chroma[1] - key_chroma[1];
                (dx * dx + dy * dy).sqrt()
            }
        }
    }

    /// Coverage the pixel keeps: `0` fully keyed away, `1` untouched.
    pub fn coverage(&self, rgb: [f32; 3]) -> f32 {
        let distance = self.colour_distance(rgb);
        let near = if self.softness <= 0.0 {
            if distance <= self.tolerance {
                0.0
            } else {
                1.0
            }
        } else {
            let ramp = ((distance - self.tolerance) / self.softness).clamp(0.0, 1.0);
            ramp * ramp * (3.0 - 2.0 * ramp)
        };
        match self.operation {
            KeyOperation::KeyColours => near,
            KeyOperation::KeepColours => 1.0 - near,
        }
    }

    /// Applies the key to one straight-alpha pixel.
    ///
    /// Only the coverage moves: the colour stays as authored so the effect composes
    /// with the straight-alpha frames the renderer samples.
    pub fn apply_pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let scale = self.coverage([pixel[0], pixel[1], pixel[2]]);
        [pixel[0], pixel[1], pixel[2], pixel[3] * scale]
    }

    /// Uniform payload: `[key rgb, tolerance]` and `[softness, mode, operation, 0]`.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [self.key[0], self.key[1], self.key[2], self.tolerance],
            [
                self.softness,
                self.match_colours.as_u32() as f32,
                self.operation.as_u32() as f32,
                0.0,
            ],
        )
    }
}

/// Normalised chromaticity `(r, g) / (r + g + b)` with a stable fallback.
fn chromaticity(rgb: [f32; 3]) -> [f32; 2] {
    let sum = rgb[0] + rgb[1] + rgb[2];
    if sum.abs() <= 1.0e-6 {
        return [0.0, 0.0];
    }
    [rgb[0] / sum, rgb[1] / sum]
}

/// Applies the key to a whole frame of straight-alpha pixels.
pub fn linear_color_key_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: LinearColorKeyGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_linear_color_key_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the key with a cooperative checkpoint.
pub(crate) fn apply_linear_color_key_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: LinearColorKeyGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || pixels.len() < pixel_count {
        return Ok(false);
    }
    let frame = &mut pixels[..pixel_count];
    for (index, pixel) in frame.iter_mut().enumerate() {
        *pixel = grade.apply_pixel(*pixel);
        if index % 4_096 == 0 {
            checkpoint()?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(
        key: [f32; 3],
        match_colours: MatchColours,
        tolerance: f32,
        softness: f32,
        operation: KeyOperation,
    ) -> LinearColorKeyGrade {
        LinearColorKeyGrade::new(key, match_colours, tolerance, softness, operation)
            .expect("valid key grade")
    }

    #[test]
    fn ranges_are_validated() {
        assert_eq!(
            LinearColorKeyGrade::new(
                [f32::NAN, 0.0, 0.0],
                MatchColours::Rgb,
                0.1,
                0.1,
                KeyOperation::KeyColours
            ),
            Err(LinearColorKeyError::Colour)
        );
        assert_eq!(
            LinearColorKeyGrade::new(
                [0.0, 1.0, 0.0],
                MatchColours::Rgb,
                1.5,
                0.1,
                KeyOperation::KeyColours
            ),
            Err(LinearColorKeyError::Amount)
        );
        assert_eq!(
            LinearColorKeyGrade::new(
                [0.0, 1.0, 0.0],
                MatchColours::Rgb,
                0.1,
                -0.1,
                KeyOperation::KeyColours
            ),
            Err(LinearColorKeyError::Amount)
        );
    }

    #[test]
    fn menu_keys_round_trip() {
        for mode in [
            MatchColours::Rgb,
            MatchColours::Green,
            MatchColours::Blue,
            MatchColours::Chroma,
        ] {
            assert_eq!(MatchColours::from_key(mode.as_key()), Some(mode));
            assert!(mode.as_u32() <= 3);
        }
        for operation in [KeyOperation::KeyColours, KeyOperation::KeepColours] {
            assert_eq!(KeyOperation::from_key(operation.as_key()), Some(operation));
            assert!(operation.as_u32() <= 1);
        }
    }

    #[test]
    fn the_rgb_metric_uses_the_largest_channel_difference() {
        let graded = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Rgb,
            0.0,
            0.0,
            KeyOperation::KeyColours,
        );
        assert!((graded.colour_distance([0.1, 1.0, 0.0]) - 0.1).abs() < 1.0e-6);
        assert!((graded.colour_distance([0.0, 0.9, 0.2]) - 0.2).abs() < 1.0e-6);
    }

    #[test]
    fn the_single_channel_metrics_ignore_the_other_channels() {
        let green = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Green,
            0.0,
            0.0,
            KeyOperation::KeyColours,
        );
        assert_eq!(green.colour_distance([0.9, 1.0, 0.9]), 0.0);
        let blue = grade(
            [0.0, 0.0, 1.0],
            MatchColours::Blue,
            0.0,
            0.0,
            KeyOperation::KeyColours,
        );
        assert_eq!(blue.colour_distance([0.9, 0.9, 1.0]), 0.0);
        assert!((blue.colour_distance([0.9, 0.9, 0.5]) - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn the_chroma_metric_ignores_brightness() {
        let graded = grade(
            [0.0, 0.5, 0.0],
            MatchColours::Chroma,
            0.0,
            0.0,
            KeyOperation::KeyColours,
        );
        let dim = graded.colour_distance([0.0, 0.25, 0.0]);
        assert!(
            dim < 1.0e-6,
            "expected a dimmer green to keep the same chroma, got {dim}"
        );
        let other = graded.colour_distance([0.0, 0.25, 0.25]);
        assert!(
            other > 0.1,
            "expected a different hue to stay distant, got {other}"
        );
    }

    #[test]
    fn coverage_ramps_from_zero_to_one_across_the_softness() {
        let graded = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Green,
            0.2,
            0.2,
            KeyOperation::KeyColours,
        );
        assert_eq!(graded.coverage([0.0, 1.0, 0.0]), 0.0);
        assert_eq!(graded.coverage([0.0, 0.8, 0.0]), 0.0);
        assert!((graded.coverage([0.0, 0.7, 0.0]) - 0.5).abs() < 1.0e-6);
        assert_eq!(graded.coverage([0.0, 0.6, 0.0]), 1.0);
        assert_eq!(graded.coverage([0.5, 0.0, 0.5]), 1.0);
    }

    #[test]
    fn keep_colours_inverts_the_ramp() {
        let keyed = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Green,
            0.2,
            0.2,
            KeyOperation::KeyColours,
        );
        let kept = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Green,
            0.2,
            0.2,
            KeyOperation::KeepColours,
        );
        for sample in [[0.0, 1.0, 0.0], [0.0, 0.7, 0.0], [0.3, 0.1, 0.9]] {
            assert!((keyed.coverage(sample) + kept.coverage(sample) - 1.0).abs() < 1.0e-6);
        }
    }

    #[test]
    fn a_far_colour_keeps_its_premultiplied_value() {
        let graded = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Rgb,
            0.1,
            0.1,
            KeyOperation::KeyColours,
        );
        let pixel = [0.4, 0.2, 0.6, 0.5];
        assert_eq!(graded.apply_pixel(pixel), pixel);
    }

    #[test]
    fn a_fully_keyed_pixel_becomes_fully_transparent() {
        let graded = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Rgb,
            0.1,
            0.1,
            KeyOperation::KeyColours,
        );
        assert_eq!(
            graded.apply_pixel([0.0, 0.95, 0.0, 0.5]),
            [0.0, 0.95, 0.0, 0.0]
        );
    }

    #[test]
    fn transparent_pixels_are_left_alone() {
        let graded = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Rgb,
            1.0,
            0.0,
            KeyOperation::KeyColours,
        );
        assert_eq!(graded.apply_pixel([0.0, 0.0, 0.0, 0.0]), [0.0; 4]);
    }

    #[test]
    fn the_frame_entry_matches_the_per_pixel_application() {
        let graded = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Green,
            0.3,
            0.3,
            KeyOperation::KeyColours,
        );
        let source = vec![
            [0.0_f32, 1.0, 0.0, 1.0],
            [0.2, 0.85, 0.1, 0.75],
            [0.6, 0.2, 0.9, 0.5],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let mut frame = source.clone();
        assert!(linear_color_key_frame_f32(&mut frame, 4, 1, graded));
        for (pixel, original) in frame.iter().zip(source.iter()) {
            let expected = if original[3] <= 0.0 {
                *original
            } else {
                let scale = graded.coverage([original[0], original[1], original[2]]);
                [original[0], original[1], original[2], original[3] * scale]
            };
            // The frame entry must match the per-pixel helper exactly.
            for channel in 0..4 {
                assert!(
                    (pixel[channel] - expected[channel]).abs() < 1.0e-6,
                    "channel {channel}: {pixel:?} vs {expected:?}"
                );
            }
        }
    }

    #[test]
    fn mismatched_geometry_is_rejected() {
        let graded = grade(
            [0.0, 1.0, 0.0],
            MatchColours::Rgb,
            0.1,
            0.1,
            KeyOperation::KeyColours,
        );
        let mut frame = vec![[0.0_f32, 0.0, 0.0, 1.0]; 3];
        assert!(!linear_color_key_frame_f32(&mut frame, 2, 2, graded));
        assert!(!linear_color_key_frame_f32(&mut frame, 0, 3, graded));
    }

    #[test]
    fn the_gpu_controls_carry_the_metric_and_the_operation() {
        let graded = grade(
            [0.1, 0.9, 0.2],
            MatchColours::Chroma,
            0.25,
            0.5,
            KeyOperation::KeepColours,
        );
        let (params, extra) = graded.gpu_controls();
        assert_eq!(params, [0.1, 0.9, 0.2, 0.25]);
        assert_eq!(extra, [0.5, 3.0, 1.0, 0.0]);
    }
}
