//! Spill suppressor: neutralises the backdrop colour that bounced onto the subject.
//!
//! # Documented reading
//!
//! 1. The key colour names the backdrop. Its dominant channel is the largest of
//!    red, green and blue, and a tie prefers red, then green, then blue. The host
//!    computes that channel once and both the float kernel and the shader read
//!    it, so the two paths cannot disagree about which colour is spilling.
//! 2. A pixel carries spill when its own dominant channel sits above both other
//!    channels. The excess over the larger of the two is divided by the dominant
//!    channel, so a saturated green pixel measures `1.0` and a colour with no
//!    excess measures `0.0`.
//! 3. The spill only counts where the pixel colour resembles the key. Chroma is
//!    normalised by the largest channel and the two colours are compared as
//!    points in that plane; the distance is divided by `sqrt(3)` so a red key and
//!    a blue pixel land on `1.0`. A smoothstep between the 容差 (tolerance) and the
//!    softness derived from 溢出范围 (range) turns that distance into a gate
//!    between zero and one, and the gate multiplies the spill measure: a skin
//!    tone far from the key keeps its colour even when its own green channel is
//!    the largest.
//! 4. 抑制 (suppression) scales the resulting weight. Zero is the identity, and
//!    the builder then elides the operation entirely.
//! 5. The correction runs in a fixed order. Despill pulls the dominant channel
//!    towards the larger of the other two, and 溢出颜色校正 (colour correction)
//!    controls how far it travels. Desaturation then mixes the whole colour
//!    towards its Rec.709 grey (降低饱和度). Luminance correction finally
//!    rescales the colour back to the source luma (亮度校正). 仅颜色 stops after
//!    the despill step. Desaturation preserves luma by construction, so the last
//!    step only sees the luma the despill removed.
//! 6. Alpha passes through untouched, a pixel whose alpha is not positive is
//!    returned unchanged, and every colour channel is clamped into `0..=1` at the
//!    end.
//!
//! # Uniform packing
//!
//! `params` carries suppression, tolerance, desaturation and range, all divided
//! by 100. `color` carries the key red, green and blue plus the colour correction
//! divided by 100. `extra0` carries the luminance correction divided by 100, the
//! method (`0` = 最终结果, `1` = 仅颜色) and the dominant channel as a float. The
//! host computes the dominant channel, so the shader never re-derives it.

/// Lowest authored amount for the six percentage controls.
pub const SPILL_SUPPRESSOR_MIN_AMOUNT: f32 = 0.0;

/// Highest authored amount for the six percentage controls.
pub const SPILL_SUPPRESSOR_MAX_AMOUNT: f32 = 100.0;

/// Authored suppression default: neutralise as much spill as the gate finds.
pub const SPILL_SUPPRESSOR_DEFAULT: f32 = 100.0;

/// Default chroma tolerance.
pub const SPILL_SUPPRESSOR_DEFAULT_TOLERANCE: f32 = 50.0;

/// Default desaturation applied on top of the despill.
pub const SPILL_SUPPRESSOR_DEFAULT_DESATURATION: f32 = 50.0;

/// Default softness of the chroma gate.
pub const SPILL_SUPPRESSOR_DEFAULT_RANGE: f32 = 50.0;

/// Default strength of the despill itself.
pub const SPILL_SUPPRESSOR_DEFAULT_COLOUR_CORRECTION: f32 = 50.0;

/// Default luminance correction, off so the despill leaves the exposure alone.
pub const SPILL_SUPPRESSOR_DEFAULT_LUMINANCE_CORRECTION: f32 = 0.0;

/// Number of entries in [`SpillSuppressorMethod::ALL`].
pub const SPILL_SUPPRESSOR_METHOD_COUNT: usize = 2;

/// Smallest divisor the kernel accepts, so a black pixel cannot blow up.
const CHANNEL_EPSILON: f32 = 1.0e-4;

/// Smallest luma the luminance correction rescales from.
const LUMA_EPSILON: f32 = 1.0e-6;

/// Fraction of the chroma plane the widest 溢出范围 covers as soft edge.
const GATE_SOFTNESS_SCALE: f32 = 0.5;

/// Rec.709 luma weights shared by the desaturation and luminance steps.
const REC_709_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// `1 / sqrt(3)`: the widest distance between two normalised chroma points.
const CHROMA_DISTANCE_SCALE: f32 = 0.577_350_3;

/// Pixels between cooperative checkpoints in [`apply_spill_suppressor_f32_controlled`].
const CHECKPOINT_STRIDE: usize = 4_096;

/// Which correction chain the 方法 menu selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpillSuppressorMethod {
    /// Despill, then desaturate, then restore the source luma.
    FinalResult,
    /// Despill only: the desaturation and luminance steps stay out of the way.
    ColorOnly,
}

impl SpillSuppressorMethod {
    /// Every method in menu order.
    pub const ALL: [Self; SPILL_SUPPRESSOR_METHOD_COUNT] = [Self::FinalResult, Self::ColorOnly];

    /// Persisted key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::FinalResult => "final_result",
            Self::ColorOnly => "color_only",
        }
    }

    /// Parse a persisted key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.as_key() == key)
    }

    /// One-based authored menu value.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Parse a one-based authored menu value.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::FinalResult),
            2 => Some(Self::ColorOnly),
            _ => None,
        }
    }

    /// Zero-based value the uniform carries.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::FinalResult => 0,
            Self::ColorOnly => 1,
        }
    }

    /// Author-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FinalResult => "最终结果",
            Self::ColorOnly => "仅颜色",
        }
    }
}

/// Why a spill suppressor grade could not be authored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpillSuppressorError {
    /// A percentage control left `0..=100`.
    Amount,
    /// A key colour component left `0..=1`.
    Colour,
    /// A control was not a finite number.
    NonFinite,
}

impl std::fmt::Display for SpillSuppressorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Amount => "spill suppressor amounts must stay within 0 to 100",
            Self::Colour => "the spill suppressor key colour must stay within 0 to 1",
            Self::NonFinite => "spill suppressor parameters must be finite",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SpillSuppressorError {}

/// Prepared spill suppressor controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpillSuppressorGrade {
    method: SpillSuppressorMethod,
    suppression: f32,
    key: [f32; 3],
    tolerance: f32,
    desaturation: f32,
    range: f32,
    colour_correction: f32,
    luminance_correction: f32,
}

impl SpillSuppressorGrade {
    /// Validate authored controls before execution.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        method: SpillSuppressorMethod,
        suppression: f32,
        key: [f32; 3],
        tolerance: f32,
        desaturation: f32,
        range: f32,
        colour_correction: f32,
        luminance_correction: f32,
    ) -> Result<Self, SpillSuppressorError> {
        let amounts = [
            suppression,
            tolerance,
            desaturation,
            range,
            colour_correction,
            luminance_correction,
        ];
        if amounts.iter().any(|amount| !amount.is_finite())
            || key.iter().any(|component| !component.is_finite())
        {
            return Err(SpillSuppressorError::NonFinite);
        }
        if amounts.iter().any(|amount| {
            !(SPILL_SUPPRESSOR_MIN_AMOUNT..=SPILL_SUPPRESSOR_MAX_AMOUNT).contains(amount)
        }) {
            return Err(SpillSuppressorError::Amount);
        }
        if key.iter().any(|component| !(0.0..=1.0).contains(component)) {
            return Err(SpillSuppressorError::Colour);
        }
        Ok(Self {
            method,
            suppression,
            key,
            tolerance,
            desaturation,
            range,
            colour_correction,
            luminance_correction,
        })
    }

    /// Which correction chain runs.
    #[must_use]
    pub const fn method(&self) -> SpillSuppressorMethod {
        self.method
    }

    /// Authored suppression percent.
    #[must_use]
    pub const fn suppression(&self) -> f32 {
        self.suppression
    }

    /// Authored key colour, straight RGB in `0..=1`.
    #[must_use]
    pub const fn key(&self) -> [f32; 3] {
        self.key
    }

    /// Authored chroma tolerance percent.
    #[must_use]
    pub const fn tolerance(&self) -> f32 {
        self.tolerance
    }

    /// Authored desaturation percent.
    #[must_use]
    pub const fn desaturation(&self) -> f32 {
        self.desaturation
    }

    /// Authored chroma gate range percent.
    #[must_use]
    pub const fn range(&self) -> f32 {
        self.range
    }

    /// Authored despill strength percent.
    #[must_use]
    pub const fn colour_correction(&self) -> f32 {
        self.colour_correction
    }

    /// Authored luminance correction percent.
    #[must_use]
    pub const fn luminance_correction(&self) -> f32 {
        self.luminance_correction
    }

    /// Zero suppression is the documented identity.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.suppression == 0.0
    }

    /// The key's largest channel, with ties preferring red, then green, then blue.
    #[must_use]
    pub fn dominant_channel(&self) -> usize {
        let [red, green, blue] = self.key;
        if red >= green && red >= blue {
            0
        } else if green >= blue {
            1
        } else {
            2
        }
    }

    /// The three vec4s the fused point uniform carries.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.normalised(self.suppression),
                self.normalised(self.tolerance),
                self.normalised(self.desaturation),
                self.normalised(self.range),
            ],
            [
                self.key[0],
                self.key[1],
                self.key[2],
                self.normalised(self.colour_correction),
            ],
            [
                self.normalised(self.luminance_correction),
                self.method.as_u32() as f32,
                self.dominant_channel() as f32,
                0.0,
            ],
        )
    }

    /// Suppress spill in one straight-alpha pixel.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let source = [pixel[0], pixel[1], pixel[2]];
        let dominant = self.dominant_channel();
        let [first, second] = match dominant {
            0 => [1, 2],
            1 => [0, 2],
            _ => [0, 1],
        };
        let peak = source[dominant];
        let floor = source[first].max(source[second]);
        let spill = ((peak - floor) / peak.max(CHANNEL_EPSILON)).clamp(0.0, 1.0);
        let weight =
            (spill * self.chroma_gate(source)).clamp(0.0, 1.0) * self.normalised(self.suppression);
        let mut colour = source;
        let target = colour[first].max(colour[second]);
        colour[dominant] +=
            (target - colour[dominant]) * weight * self.normalised(self.colour_correction);
        if self.method == SpillSuppressorMethod::FinalResult {
            let grey = luma(colour);
            let desaturation = weight * self.normalised(self.desaturation);
            for channel in &mut colour {
                *channel += (grey - *channel) * desaturation;
            }
            colour = self.correct_luminance(source, colour);
        }
        [
            colour[0].clamp(0.0, 1.0),
            colour[1].clamp(0.0, 1.0),
            colour[2].clamp(0.0, 1.0),
            pixel[3],
        ]
    }

    /// Gate the spill by how close the pixel chroma sits to the key chroma.
    fn chroma_gate(&self, source: [f32; 3]) -> f32 {
        let distance = chroma_distance(source, self.key) * CHROMA_DISTANCE_SCALE;
        let tolerance = self.normalised(self.tolerance);
        let softness = (self.normalised(self.range) * GATE_SOFTNESS_SCALE).max(CHANNEL_EPSILON);
        let edge = ((distance - tolerance) / softness).clamp(0.0, 1.0);
        1.0 - edge * edge * (3.0 - 2.0 * edge)
    }

    /// Rescale the corrected colour back to the source Rec.709 luma.
    fn correct_luminance(&self, source: [f32; 3], colour: [f32; 3]) -> [f32; 3] {
        let source_luma = luma(source);
        let corrected_luma = luma(colour);
        if corrected_luma <= LUMA_EPSILON {
            return colour;
        }
        let amount = self.normalised(self.luminance_correction);
        let scale = 1.0 + (source_luma / corrected_luma - 1.0) * amount;
        [colour[0] * scale, colour[1] * scale, colour[2] * scale]
    }

    /// Turn an authored percentage into the `0..=1` uniform fraction.
    fn normalised(&self, amount: f32) -> f32 {
        amount / SPILL_SUPPRESSOR_MAX_AMOUNT
    }
}

/// Rec.709 luma of a straight RGB triple.
fn luma(colour: [f32; 3]) -> f32 {
    REC_709_LUMA[0] * colour[0] + REC_709_LUMA[1] * colour[1] + REC_709_LUMA[2] * colour[2]
}

/// Euclidean distance between two chroma points normalised by the largest channel.
fn chroma_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    let left = normalised_chroma(left);
    let right = normalised_chroma(right);
    let red = left[0] - right[0];
    let green = left[1] - right[1];
    let blue = left[2] - right[2];
    (red * red + green * green + blue * blue).sqrt()
}

/// Divide a colour by its largest channel, floored so black stays finite.
fn normalised_chroma(colour: [f32; 3]) -> [f32; 3] {
    let scale = colour[0].max(colour[1]).max(colour[2]).max(CHANNEL_EPSILON);
    [colour[0] / scale, colour[1] / scale, colour[2] / scale]
}

/// Suppress spill across a straight-alpha float frame.
pub fn spill_suppressor_frame_f32(pixels: &mut [[f32; 4]], grade: &SpillSuppressorGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_spill_suppressor_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Suppress spill across a frame, yielding to `checkpoint` between chunks.
pub(crate) fn apply_spill_suppressor_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &SpillSuppressorGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            *pixel = grade.apply(*pixel);
        }
    }
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        luma, spill_suppressor_frame_f32, SpillSuppressorError, SpillSuppressorGrade,
        SpillSuppressorMethod, SPILL_SUPPRESSOR_METHOD_COUNT,
    };

    fn close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "expected {expected}, got {actual}"
        );
    }

    fn close_pixel(actual: [f32; 4], expected: [f32; 4]) {
        for channel in 0..4 {
            close(actual[channel], expected[channel]);
        }
    }

    fn green_key_grade(
        method: SpillSuppressorMethod,
        suppression: f32,
        tolerance: f32,
        desaturation: f32,
        range: f32,
        colour_correction: f32,
        luminance_correction: f32,
    ) -> SpillSuppressorGrade {
        SpillSuppressorGrade::new(
            method,
            suppression,
            [0.0, 1.0, 0.0],
            tolerance,
            desaturation,
            range,
            colour_correction,
            luminance_correction,
        )
        .expect("the test grade is in range")
    }

    #[test]
    fn zero_suppression_is_the_identity() {
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            0.0,
            50.0,
            50.0,
            50.0,
            50.0,
            100.0,
        );
        assert!(grade.is_identity());
        let pixel = [0.3, 0.9, 0.2, 0.75];
        assert_eq!(
            grade.apply(pixel).map(f32::to_bits),
            pixel.map(f32::to_bits)
        );
    }

    #[test]
    fn a_pure_green_pixel_is_fully_despilled_under_a_green_key() {
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            0.0,
            50.0,
            100.0,
            0.0,
        );
        close_pixel(grade.apply([0.0, 1.0, 0.0, 1.0]), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_pixel_away_from_the_key_keeps_its_colour() {
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            100.0,
            50.0,
            100.0,
            100.0,
        );
        let pixel = [0.8, 0.2, 0.7, 1.0];
        // Green is nowhere near the dominant channel here, so the spill measure
        // is zero whatever the gate says.
        assert_eq!(
            grade.apply(pixel).map(f32::to_bits),
            pixel.map(f32::to_bits)
        );
    }

    #[test]
    fn color_only_stops_after_the_despill() {
        let pixel = [0.2, 1.0, 0.2, 1.0];
        let color_only = green_key_grade(
            SpillSuppressorMethod::ColorOnly,
            100.0,
            50.0,
            100.0,
            50.0,
            100.0,
            100.0,
        )
        .apply(pixel);
        // Spill 0.8 with a fully open gate pulls green from 1.0 down to the
        // larger neighbour, 0.2: 1.0 + (0.2 - 1.0) * 0.8 = 0.36.
        close_pixel(color_only, [0.2, 0.36, 0.2, 1.0]);
        let final_result = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            100.0,
            50.0,
            100.0,
            100.0,
        )
        .apply(pixel);
        // The full chain desaturates and then restores the source luma, so it
        // must leave the green channel somewhere else entirely.
        assert_ne!(final_result, color_only);
        close_pixel(final_result, [0.715_957_2, 0.794_540_5, 0.715_957_2, 1.0]);
    }

    #[test]
    fn tolerance_widens_the_gate_coverage() {
        let pixel = [0.35, 1.0, 0.0, 1.0];
        let tight = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            0.0,
            0.0,
            50.0,
            100.0,
            0.0,
        )
        .apply(pixel);
        let wide = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            30.0,
            0.0,
            50.0,
            100.0,
            0.0,
        )
        .apply(pixel);
        // At tolerance zero the pixel sits 0.20 chroma away from green, which is
        // inside the soft edge: only a fraction of the spill is removed.
        close(tight[1], 0.959_369_8);
        // A thirty percent tolerance covers that distance completely.
        close(wide[1], 0.577_5);
        assert!(wide[1] < tight[1]);
    }

    #[test]
    fn range_widens_the_gate_soft_edge() {
        let pixel = [0.35, 1.0, 0.0, 1.0];
        let hard = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            10.0,
            0.0,
            0.0,
            100.0,
            0.0,
        )
        .apply(pixel);
        // A zero range collapses the soft edge, so the pixel outside the
        // tolerance is rejected outright and stays untouched.
        assert_eq!(hard.map(f32::to_bits), pixel.map(f32::to_bits));
        let soft = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            10.0,
            0.0,
            100.0,
            100.0,
            0.0,
        )
        .apply(pixel);
        close(soft[1], 0.623_134_3);
    }

    #[test]
    fn zero_colour_correction_leaves_the_dominant_channel_to_desaturation() {
        let pixel = [0.2, 1.0, 0.2, 1.0];
        let untouched = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            0.0,
            50.0,
            0.0,
            0.0,
        )
        .apply(pixel);
        assert_eq!(
            untouched.map(f32::to_bits),
            pixel.map(f32::to_bits),
            "weight alone must not move a channel"
        );
        let desaturated = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            100.0,
            50.0,
            0.0,
            0.0,
        )
        .apply(pixel);
        // The despill did nothing, but the desaturation still pulls every
        // channel towards the grey the pixel already owned, weighted by 0.8.
        close_pixel(desaturated, [0.657_728, 0.817_728, 0.657_728, 1.0]);
    }

    #[test]
    fn luminance_correction_restores_the_source_luma() {
        let pixel = [0.2, 1.0, 0.2, 1.0];
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            50.0,
            50.0,
            100.0,
            100.0,
        );
        let corrected = grade.apply(pixel);
        close(
            luma([corrected[0], corrected[1], corrected[2]]),
            luma([0.2, 1.0, 0.2]),
        );
        close_pixel(corrected, [0.603_551_6, 0.839_301_6, 0.603_551_6, 1.0]);
    }

    #[test]
    fn blue_and_red_keys_are_symmetric() {
        for (channel, key) in [(0_usize, [1.0, 0.0, 0.0]), (2, [0.0, 0.0, 1.0])] {
            let grade = SpillSuppressorGrade::new(
                SpillSuppressorMethod::FinalResult,
                100.0,
                key,
                50.0,
                0.0,
                50.0,
                100.0,
                0.0,
            )
            .expect("the test grade is in range");
            assert_eq!(grade.dominant_channel(), channel);
            let mut pixel = [0.0, 0.0, 0.0, 1.0];
            pixel[channel] = 1.0;
            close_pixel(grade.apply(pixel), [0.0, 0.0, 0.0, 1.0]);
        }
    }

    #[test]
    fn dominant_channel_breaks_ties_red_then_green_then_blue() {
        let dominant = |key: [f32; 3]| {
            SpillSuppressorGrade::new(
                SpillSuppressorMethod::FinalResult,
                100.0,
                key,
                50.0,
                50.0,
                50.0,
                50.0,
                0.0,
            )
            .expect("the test grade is in range")
            .dominant_channel()
        };
        assert_eq!(dominant([1.0, 1.0, 1.0]), 0);
        assert_eq!(dominant([0.6, 0.6, 0.0]), 0);
        assert_eq!(dominant([0.0, 1.0, 1.0]), 1);
        assert_eq!(dominant([0.0, 0.9, 0.9]), 1);
        assert_eq!(dominant([0.4, 0.2, 0.9]), 2);
        assert_eq!(dominant([0.0, 0.0, 1.0]), 2);
    }

    #[test]
    fn a_green_blue_tie_reads_the_green_channel() {
        let grade = SpillSuppressorGrade::new(
            SpillSuppressorMethod::FinalResult,
            100.0,
            [0.0, 1.0, 1.0],
            50.0,
            0.0,
            50.0,
            100.0,
            0.0,
        )
        .expect("the test grade is in range");
        // Green wins the tie, so spill is 1.0 - 0.5 and the green channel lands
        // on the larger neighbour 0.5. A blue reading would have found no spill.
        let actual = grade.apply([0.5, 1.0, 0.5, 1.0]);
        close_pixel(actual, [0.5, 0.75, 0.5, 1.0]);
    }

    #[test]
    fn despill_pulls_towards_the_larger_of_the_other_channels() {
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            0.0,
            50.0,
            100.0,
            0.0,
        );
        // The other channels are 0.6 and 0.4; the target is 0.6, not their mean
        // 0.5, so green lands on 1.0 + (0.6 - 1.0) * 0.4 = 0.84.
        let actual = grade.apply([0.6, 1.0, 0.4, 1.0]);
        close_pixel(actual, [0.6, 0.84, 0.4, 1.0]);
    }

    #[test]
    fn amounts_outside_their_range_are_rejected() {
        let key = [0.0, 1.0, 0.0];
        for amount in [-0.5, 100.5] {
            let cases = [
                SpillSuppressorGrade::new(
                    SpillSuppressorMethod::FinalResult,
                    amount,
                    key,
                    50.0,
                    50.0,
                    50.0,
                    50.0,
                    0.0,
                ),
                SpillSuppressorGrade::new(
                    SpillSuppressorMethod::FinalResult,
                    100.0,
                    key,
                    amount,
                    50.0,
                    50.0,
                    50.0,
                    0.0,
                ),
                SpillSuppressorGrade::new(
                    SpillSuppressorMethod::FinalResult,
                    100.0,
                    key,
                    50.0,
                    amount,
                    50.0,
                    50.0,
                    0.0,
                ),
                SpillSuppressorGrade::new(
                    SpillSuppressorMethod::FinalResult,
                    100.0,
                    key,
                    50.0,
                    50.0,
                    amount,
                    50.0,
                    0.0,
                ),
                SpillSuppressorGrade::new(
                    SpillSuppressorMethod::FinalResult,
                    100.0,
                    key,
                    50.0,
                    50.0,
                    50.0,
                    amount,
                    0.0,
                ),
                SpillSuppressorGrade::new(
                    SpillSuppressorMethod::FinalResult,
                    100.0,
                    key,
                    50.0,
                    50.0,
                    50.0,
                    50.0,
                    amount,
                ),
            ];
            for case in cases {
                assert_eq!(case, Err(SpillSuppressorError::Amount));
            }
        }
    }

    #[test]
    fn a_key_colour_outside_the_unit_cube_is_rejected() {
        for key in [[1.5, 0.0, 0.0], [0.0, -0.1, 0.0], [0.0, 0.0, 1.2]] {
            assert_eq!(
                SpillSuppressorGrade::new(
                    SpillSuppressorMethod::FinalResult,
                    100.0,
                    key,
                    50.0,
                    50.0,
                    50.0,
                    50.0,
                    0.0,
                ),
                Err(SpillSuppressorError::Colour)
            );
        }
    }

    #[test]
    fn non_finite_controls_are_rejected() {
        assert_eq!(
            SpillSuppressorGrade::new(
                SpillSuppressorMethod::FinalResult,
                f32::NAN,
                [0.0, 1.0, 0.0],
                50.0,
                50.0,
                50.0,
                50.0,
                0.0,
            ),
            Err(SpillSuppressorError::NonFinite)
        );
        assert_eq!(
            SpillSuppressorGrade::new(
                SpillSuppressorMethod::FinalResult,
                100.0,
                [0.0, 1.0, 0.0],
                f32::NEG_INFINITY,
                50.0,
                50.0,
                50.0,
                0.0,
            ),
            Err(SpillSuppressorError::NonFinite)
        );
        assert_eq!(
            SpillSuppressorGrade::new(
                SpillSuppressorMethod::FinalResult,
                100.0,
                [0.0, f32::INFINITY, 0.0],
                50.0,
                50.0,
                50.0,
                50.0,
                0.0,
            ),
            Err(SpillSuppressorError::NonFinite)
        );
    }

    #[test]
    fn alpha_passes_through_and_transparent_pixels_are_untouched() {
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            100.0,
            50.0,
            100.0,
            100.0,
        );
        let actual = grade.apply([0.2, 1.0, 0.2, 0.37]);
        close(actual[3], 0.37);
        for transparent in [[0.9, 0.1, 0.4, 0.0], [0.9, 0.1, 0.4, -0.5]] {
            assert_eq!(
                grade.apply(transparent).map(f32::to_bits),
                transparent.map(f32::to_bits)
            );
        }
    }

    #[test]
    fn every_channel_stays_inside_the_unit_interval() {
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            100.0,
            50.0,
            100.0,
            100.0,
        );
        let mut frame = vec![
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [2.0, 3.0, 2.0, 1.0],
            [0.05, 1.0, 0.05, 1.0],
        ];
        assert!(spill_suppressor_frame_f32(&mut frame, &grade));
        for pixel in &frame {
            for channel in &pixel[..3] {
                assert!(
                    (0.0..=1.0).contains(channel),
                    "channel {channel} left the unit interval"
                );
            }
        }
    }

    #[test]
    fn the_frame_kernel_visits_every_pixel() {
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            0.0,
            50.0,
            100.0,
            0.0,
        );
        let mut frame = vec![[0.0, 1.0, 0.0, 1.0]; 6];
        frame[3] = [0.8, 0.2, 0.7, 1.0];
        assert!(spill_suppressor_frame_f32(&mut frame, &grade));
        for (index, pixel) in frame.iter().enumerate() {
            if index == 3 {
                assert_eq!(
                    pixel.map(f32::to_bits),
                    [0.8, 0.2, 0.7, 1.0].map(f32::to_bits)
                );
            } else {
                close_pixel(*pixel, [0.0, 0.0, 0.0, 1.0]);
            }
        }
    }

    #[test]
    fn gpu_controls_pack_the_documented_uniform() {
        let grade = SpillSuppressorGrade::new(
            SpillSuppressorMethod::ColorOnly,
            60.0,
            [0.0, 0.0, 1.0],
            25.0,
            40.0,
            75.0,
            80.0,
            10.0,
        )
        .expect("the test grade is in range");
        assert_eq!(
            grade.gpu_controls(),
            (
                [0.6, 0.25, 0.4, 0.75],
                [0.0, 0.0, 1.0, 0.8],
                [0.1, 1.0, 2.0, 0.0],
            )
        );
    }

    #[test]
    fn methods_are_one_based_in_the_menu_and_zero_based_in_the_uniform() {
        assert_eq!(
            SpillSuppressorMethod::ALL.len(),
            SPILL_SUPPRESSOR_METHOD_COUNT
        );
        assert_eq!(SpillSuppressorMethod::FinalResult.as_menu(), 1);
        assert_eq!(SpillSuppressorMethod::ColorOnly.as_menu(), 2);
        assert_eq!(SpillSuppressorMethod::FinalResult.as_u32(), 0);
        assert_eq!(SpillSuppressorMethod::ColorOnly.as_u32(), 1);
        assert_eq!(
            SpillSuppressorMethod::from_menu(1),
            Some(SpillSuppressorMethod::FinalResult)
        );
        assert_eq!(
            SpillSuppressorMethod::from_menu(2),
            Some(SpillSuppressorMethod::ColorOnly)
        );
        assert_eq!(SpillSuppressorMethod::from_menu(0), None);
        assert_eq!(SpillSuppressorMethod::from_menu(3), None);
        assert_eq!(
            SpillSuppressorMethod::from_key("final_result"),
            Some(SpillSuppressorMethod::FinalResult)
        );
        assert_eq!(
            SpillSuppressorMethod::from_key("color_only"),
            Some(SpillSuppressorMethod::ColorOnly)
        );
        assert_eq!(SpillSuppressorMethod::from_key("FinalResult"), None);
        assert_eq!(SpillSuppressorMethod::ColorOnly.label(), "仅颜色");
    }

    #[test]
    fn apply_leaves_the_grade_and_its_input_alone() {
        let grade = green_key_grade(
            SpillSuppressorMethod::FinalResult,
            100.0,
            50.0,
            100.0,
            50.0,
            100.0,
            100.0,
        );
        let before = grade;
        let pixel = [0.2, 1.0, 0.2, 1.0];
        let _ = grade.apply(pixel);
        assert_eq!(grade, before);
        assert_eq!(pixel, [0.2, 1.0, 0.2, 1.0]);
    }
}
