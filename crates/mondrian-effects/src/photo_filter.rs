//! Photo filter: tints the sample with a photographic gel.
//!
//! [`PhotoFilterGrade`] implements `builtin.photo_filter`, a pixel-local point
//! grade. A gel is a transmission medium, so the authored colour is a per-channel
//! transmittance `F` and the density `d` selects how much of it the sample sees:
//!
//! ```text
//! gain_c = 1 - d + d * F_c
//! C'     = C * gain
//! ```
//!
//! The gain is folded first and then multiplied in, which is what the fused
//! point shader receives as well. The other obvious spelling, interpolating
//! towards the gel (`C + d * (F - C)`), flattens the whole frame to one colour at
//! `d = 1` and loses both exact endpoints — `d = 0` and `d = 100` must reproduce
//! the source and the gel bit for bit, and only the gain form does.
//!
//! # Preserving luminosity
//!
//! A dense gel darkens: every gain is at most one, so a mid grey loses roughly a
//! tenth of its Rec.709 luma at the default 25%. With `preserve_luminosity` on,
//! the filtered colour is scaled back so that its luma matches the source:
//!
//! ```text
//! l  = 0.2126 * r  + 0.7152 * g  + 0.0722 * b
//! l' = luma(C')
//! C'' = C' * (l / l')
//! ```
//!
//! The scale is a single multiply, so the channel ratios — and with them the
//! direction of the tint — survive. Two guards keep the ratio meaningful: a
//! source luma at or below zero would flip the sign of the whole sample, and a
//! filtered luma at or below the epsilon would scale a nearly black sample up
//! to a meaningless value (a dense warm filter drives a saturated blue to zero,
//! see the kernel tests). Both sides are guarded here; `color_balance` only
//! guards the filtered side because it clamps its result first.
//!
//! # Deviations from a generic gel
//!
//! * Nothing is clamped. A gain in `[0, 1]` keeps the sign of every channel, so
//!   extended-range samples stay extended and HDR highlights darken
//!   proportionally. The authored colour is only required to be finite: `F > 1`
//!   brightens and `F < 0` inverts, exactly as the formula extrapolates.
//! * There is no colour-space conversion: the swatch is an eight-bit sRGB
//!   constant divided by 255 and multiplies whatever the working buffer holds,
//!   matching the other colour parameters in this crate.
//! * Only [`PhotoFilterPreset::Custom`] reads the authored colour. A named
//!   preset always uses its own swatch, so re-running a project reproduces the
//!   same pixels on every machine no matter what the colour widget shows.
//! * A sample without coverage (`a <= 0`) is returned unchanged rather than
//!   zeroed; there is nothing to tint.
//!
//! The default configuration is *not* the identity — the default preset at the
//! default density warms the frame — so the graph builder appends a node for the
//! default parameters. It does elide the node when the computed gain is exactly
//! one (density zero, or a white gel at full density), which keeps an effect
//! pulled back to zero from leaving a no-op in the chain.

/// How many pixels sit between two cooperative checkpoints.
///
/// A photo filter is three multiplies per pixel, so the stride is a
/// cancellation cadence rather than a throughput knob.
const CHECKPOINT_STRIDE: usize = 4_096;

/// The lowest accepted density, in percent.
pub const PHOTO_FILTER_MIN_DENSITY: f32 = 0.0;
/// The highest accepted density, in percent.
pub const PHOTO_FILTER_MAX_DENSITY: f32 = 100.0;
/// The default density, in percent.
pub const PHOTO_FILTER_DEFAULT_DENSITY: f32 = 25.0;
/// The default gel colour, `#EC8A00` — the same swatch as
/// [`PhotoFilterPreset::WarmingFilter85`].
pub const PHOTO_FILTER_DEFAULT_COLOR: [f32; 3] = [236.0 / 255.0, 138.0 / 255.0, 0.0];

/// Rec.709 luma coefficients, matching `color_balance`.
const REC_709_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// The luma below which the preserve-luminosity scale is skipped.
const LUMA_EPSILON: f32 = 1.0e-6;

/// Builds an eight-bit sRGB swatch.
const fn rgb(red: u8, green: u8, blue: u8) -> [f32; 3] {
    [
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
    ]
}

/// The twenty named gels plus the custom slot, in menu order.
///
/// The order follows After Effects: the LBA warming filter comes before 81, and
/// `Deep Emerald` keeps the AE name rather than Photoshop's `Deep Green`. The
/// swatch is the only source of the transmittance; the rest of the crate reads
/// it through [`PhotoFilterPreset::swatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PhotoFilterPreset {
    /// Warming filter 85, the default and the `#EC8A00` anchor.
    WarmingFilter85,
    /// Warming filter LBA, lighter than 85.
    WarmingFilterLba,
    /// Warming filter 81, close to neutral.
    WarmingFilter81,
    /// Cooling filter 80, a strong blue for white balance.
    CoolingFilter80,
    /// Cooling filter LBB, lighter than 80.
    CoolingFilterLbb,
    /// Cooling filter 82, a light cool.
    CoolingFilter82,
    /// Saturated red.
    Red,
    /// Saturated orange.
    Orange,
    /// Saturated yellow.
    Yellow,
    /// Saturated green.
    Green,
    /// Saturated cyan.
    Cyan,
    /// Saturated blue.
    Blue,
    /// Saturated violet.
    Violet,
    /// Saturated magenta.
    Magenta,
    /// Classic sepia.
    Sepia,
    /// Dark red.
    DeepRed,
    /// Dark blue.
    DeepBlue,
    /// Dark emerald green.
    DeepEmerald,
    /// Dark yellow.
    DeepYellow,
    /// The teal of an underwater shot.
    Underwater,
    /// The custom slot: the only preset that reads the authored colour.
    Custom,
}

impl PhotoFilterPreset {
    /// Every preset, in menu order.
    pub const ALL: [Self; 21] = [
        Self::WarmingFilter85,
        Self::WarmingFilterLba,
        Self::WarmingFilter81,
        Self::CoolingFilter80,
        Self::CoolingFilterLbb,
        Self::CoolingFilter82,
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Cyan,
        Self::Blue,
        Self::Violet,
        Self::Magenta,
        Self::Sepia,
        Self::DeepRed,
        Self::DeepBlue,
        Self::DeepEmerald,
        Self::DeepYellow,
        Self::Underwater,
        Self::Custom,
    ];

    /// Stable authored key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::WarmingFilter85 => "warming_85",
            Self::WarmingFilterLba => "warming_lba",
            Self::WarmingFilter81 => "warming_81",
            Self::CoolingFilter80 => "cooling_80",
            Self::CoolingFilterLbb => "cooling_lbb",
            Self::CoolingFilter82 => "cooling_82",
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Cyan => "cyan",
            Self::Blue => "blue",
            Self::Violet => "violet",
            Self::Magenta => "magenta",
            Self::Sepia => "sepia",
            Self::DeepRed => "deep_red",
            Self::DeepBlue => "deep_blue",
            Self::DeepEmerald => "deep_emerald",
            Self::DeepYellow => "deep_yellow",
            Self::Underwater => "underwater",
            Self::Custom => "custom",
        }
    }

    /// Resolves an authored key. Unknown keys are rejected, never defaulted.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.as_key() == key)
    }

    /// Chinese fallback label; the catalog carries the translated caption.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::WarmingFilter85 => "加温滤镜（85）",
            Self::WarmingFilterLba => "加温滤镜（LBA）",
            Self::WarmingFilter81 => "加温滤镜（81）",
            Self::CoolingFilter80 => "冷却滤镜（80）",
            Self::CoolingFilterLbb => "冷却滤镜（LBB）",
            Self::CoolingFilter82 => "冷却滤镜（82）",
            Self::Red => "红",
            Self::Orange => "橘红",
            Self::Yellow => "黄",
            Self::Green => "绿",
            Self::Cyan => "青",
            Self::Blue => "蓝",
            Self::Violet => "紫",
            Self::Magenta => "品红",
            Self::Sepia => "棕褐",
            Self::DeepRed => "深红",
            Self::DeepBlue => "深蓝",
            Self::DeepEmerald => "翡翠绿",
            Self::DeepYellow => "深黄",
            Self::Underwater => "水下",
            Self::Custom => "自定义",
        }
    }

    /// The gel transmission, or `None` for [`PhotoFilterPreset::Custom`].
    ///
    /// The eight-bit sRGB hex value is the only source of these numbers; there
    /// is deliberately no parallel `f32` table to drift away from it.
    #[must_use]
    pub const fn swatch(self) -> Option<[f32; 3]> {
        match self {
            Self::WarmingFilter85 => Some(PHOTO_FILTER_DEFAULT_COLOR),
            Self::WarmingFilterLba => Some(rgb(0xE8, 0xB2, 0x6E)),
            Self::WarmingFilter81 => Some(rgb(0xEC, 0xD2, 0xA4)),
            Self::CoolingFilter80 => Some(rgb(0x00, 0x48, 0xBA)),
            Self::CoolingFilterLbb => Some(rgb(0x7F, 0xA8, 0xD8)),
            Self::CoolingFilter82 => Some(rgb(0xA8, 0xCB, 0xEA)),
            Self::Red => Some(rgb(0xF2, 0x30, 0x30)),
            Self::Orange => Some(rgb(0xF2, 0x91, 0x30)),
            Self::Yellow => Some(rgb(0xF2, 0xF2, 0x30)),
            Self::Green => Some(rgb(0x30, 0xF2, 0x30)),
            Self::Cyan => Some(rgb(0x30, 0xF2, 0xF2)),
            Self::Blue => Some(rgb(0x30, 0x30, 0xF2)),
            Self::Violet => Some(rgb(0x91, 0x30, 0xF2)),
            Self::Magenta => Some(rgb(0xF2, 0x30, 0xF2)),
            Self::Sepia => Some(rgb(0x70, 0x42, 0x14)),
            Self::DeepRed => Some(rgb(0x8C, 0x15, 0x15)),
            Self::DeepBlue => Some(rgb(0x15, 0x15, 0x8C)),
            Self::DeepEmerald => Some(rgb(0x15, 0x8C, 0x51)),
            Self::DeepYellow => Some(rgb(0x8C, 0x78, 0x15)),
            Self::Underwater => Some(rgb(0x4B, 0x97, 0xA6)),
            Self::Custom => None,
        }
    }
}

/// Why a [`PhotoFilterGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoFilterError {
    /// The authored gel colour has a non-finite component.
    ///
    /// A non-finite transmittance poisons every pixel it multiplies, so the
    /// grade refuses to exist instead of shipping NaNs into the frame. The
    /// components are otherwise unbounded: the formula extrapolates.
    Color,
    /// The density is not finite or sits outside `0..=100`.
    ///
    /// The schema range rejects the same values at authoring time; this variant
    /// is the kernel-side counterpart for callers that build a grade directly.
    Density,
}

impl std::fmt::Display for PhotoFilterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Color => write!(formatter, "photo filter colour must be finite"),
            Self::Density => write!(
                formatter,
                "photo filter density must be finite and within 0..=100"
            ),
        }
    }
}

impl std::error::Error for PhotoFilterError {}

/// The gel parameters of one `builtin.photo_filter` node.
///
/// `colour` is stored as authored even for a named preset, where it takes no
/// part in the result: the grade then round-trips the property bag without a
/// lossy rewrite, and a later switch to [`PhotoFilterPreset::Custom`] picks the
/// stored colour back up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhotoFilterGrade {
    colour: [f32; 3],
    density: f32,
    preserve_luminosity: bool,
}

impl PhotoFilterGrade {
    /// Builds a grade from a gel colour, a density in percent and the switch.
    ///
    /// # Errors
    ///
    /// Returns [`PhotoFilterError::Density`] when `density` is not finite or
    /// outside `0..=100`, and [`PhotoFilterError::Color`] when any component of
    /// `colour` is not finite.
    pub fn new(
        colour: [f32; 3],
        density: f32,
        preserve_luminosity: bool,
    ) -> Result<Self, PhotoFilterError> {
        if !density.is_finite()
            || !(PHOTO_FILTER_MIN_DENSITY..=PHOTO_FILTER_MAX_DENSITY).contains(&density)
        {
            return Err(PhotoFilterError::Density);
        }
        if colour.iter().any(|component| !component.is_finite()) {
            return Err(PhotoFilterError::Color);
        }
        Ok(Self { colour, density, preserve_luminosity })
    }

    /// The authored gel colour, whether or not a preset overrides it.
    #[must_use]
    pub const fn colour(&self) -> [f32; 3] {
        self.colour
    }

    /// The authored density, in percent.
    #[must_use]
    pub const fn density(&self) -> f32 {
        self.density
    }

    /// Whether the filtered luma is rescaled back to the source luma.
    #[must_use]
    pub const fn preserve_luminosity(&self) -> bool {
        self.preserve_luminosity
    }

    /// The per-channel gain the density resolves the gel to.
    ///
    /// This is the single source of the arithmetic: the CPU kernel multiplies
    /// the sample by it, and [`Self::gpu_controls`] hands the same values to the
    /// fused point shader so both sides agree bit for bit.
    #[must_use]
    pub fn gain(&self) -> [f32; 3] {
        let density = self.density / 100.0;
        [
            1.0 - density + density * self.colour[0],
            1.0 - density + density * self.colour[1],
            1.0 - density + density * self.colour[2],
        ]
    }

    /// Whether this grade leaves every frame untouched.
    ///
    /// The test is the computed gain, not the parameters: a zero density always
    /// lands on exactly `[1, 1, 1]`, and a white gel at full density does too.
    /// `preserve_luminosity` is ignored because a unit gain scales by exactly
    /// one, which is bit-exact.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.gain() == [1.0, 1.0, 1.0]
    }

    /// Applies the grade to one straight-alpha sample.
    #[must_use]
    pub fn apply_pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        let alpha = pixel[3];
        // Spelled as the shader's `a > 0` guard, plus the explicit NaN arm so an
        // `a = NaN` sample takes the untouched branch on both sides.
        if alpha.is_nan() || alpha <= 0.0 {
            return pixel;
        }
        let gain = self.gain();
        let mut filtered = [pixel[0] * gain[0], pixel[1] * gain[1], pixel[2] * gain[2]];
        if self.preserve_luminosity {
            let source_luma = luma([pixel[0], pixel[1], pixel[2]]);
            let filtered_luma = luma(filtered);
            if source_luma > LUMA_EPSILON && filtered_luma > LUMA_EPSILON {
                let scale = source_luma / filtered_luma;
                filtered = [
                    filtered[0] * scale,
                    filtered[1] * scale,
                    filtered[2] * scale,
                ];
            }
        }
        [filtered[0], filtered[1], filtered[2], alpha]
    }

    /// Applies the grade to every sample of a frame.
    pub fn apply_to_frame(&self, frame: &mut [[f32; 4]]) {
        let _ = photo_filter_frame_f32(frame, self);
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `header = [44, preserve, 0, 0]`, `params = [gain.r, gain.g, gain.b, d]`
    /// and `color = [F.r, F.g, F.b, 0]`. The shader reads `params.xyz` and
    /// `header.y` only: the density and the gel are carried for debugging, so
    /// the shader never spells `1 - d + d * F` and cannot be fused into an
    /// expression that drifts from the CPU gain. The four extra vectors stay
    /// unused.
    #[must_use]
    pub fn gpu_controls(&self) -> ([u32; 4], [f32; 4], [f32; 4]) {
        let gain = self.gain();
        (
            [44, u32::from(self.preserve_luminosity), 0, 0],
            [gain[0], gain[1], gain[2], self.density / 100.0],
            [self.colour[0], self.colour[1], self.colour[2], 0.0],
        )
    }
}

/// Rec.709 luma of an RGB triple.
fn luma(rgb: [f32; 3]) -> f32 {
    REC_709_LUMA[0] * rgb[0] + REC_709_LUMA[1] * rgb[1] + REC_709_LUMA[2] * rgb[2]
}

/// Applies a grade to a frame, yielding to `checkpoint` between chunks.
///
/// Returns the "supported" flag the point dispatchers expect; the error type is
/// the caller's cancellation error, so a stop unwinds with its own type.
pub(crate) fn apply_photo_filter_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &PhotoFilterGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let gain = grade.gain();
    let preserve = grade.preserve_luminosity;
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            // Both sides treat `a <= 0` and `a = NaN` as uncovered and leave the
            // sample untouched, matching the WGSL guard.
            if pixel[3].is_nan() || pixel[3] <= 0.0 {
                continue;
            }
            let mut filtered = [pixel[0] * gain[0], pixel[1] * gain[1], pixel[2] * gain[2]];
            if preserve {
                let source_luma = luma([pixel[0], pixel[1], pixel[2]]);
                let filtered_luma = luma(filtered);
                if source_luma > LUMA_EPSILON && filtered_luma > LUMA_EPSILON {
                    let scale = source_luma / filtered_luma;
                    filtered = [
                        filtered[0] * scale,
                        filtered[1] * scale,
                        filtered[2] * scale,
                    ];
                }
            }
            *pixel = [filtered[0], filtered[1], filtered[2], pixel[3]];
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Applies a grade to a frame, with no cancellation point.
#[must_use]
pub fn photo_filter_frame_f32(pixels: &mut [[f32; 4]], grade: &PhotoFilterGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_photo_filter_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual}"
        );
    }

    fn bits(pixel: [f32; 4]) -> [u32; 4] {
        pixel.map(f32::to_bits)
    }

    fn grade(colour: [f32; 3], density: f32, preserve_luminosity: bool) -> PhotoFilterGrade {
        PhotoFilterGrade::new(colour, density, preserve_luminosity).expect("valid grade")
    }

    fn warming_85(density: f32, preserve_luminosity: bool) -> PhotoFilterGrade {
        grade(
            PhotoFilterPreset::WarmingFilter85.swatch().expect("85 has a swatch"),
            density,
            preserve_luminosity,
        )
    }

    /// K1: density zero is a bit-exact identity with either preserve setting.
    #[test]
    fn zero_density_is_a_bit_exact_identity() {
        let frame = [
            [0.0, 0.25, 0.5, 0.0],
            [1.0, 0.75, 0.125, 1.0e-9],
            [0.2, 0.4, 0.6, 0.25],
            [1.0, 1.0, 1.0, 0.5],
            [0.5, 0.5, 0.5, 1.0],
            [-0.25, 1.5, 2.0, 2.0],
        ];
        for preserve in [false, true] {
            for colour in [
                PHOTO_FILTER_DEFAULT_COLOR,
                [1.0, 1.0, 1.0],
                [0.0, 0.0, 0.0],
                [1.2, -0.5, 0.25],
            ] {
                let grade = PhotoFilterGrade::new(colour, 0.0, preserve).expect("valid grade");
                assert!(grade.is_identity(), "colour {colour:?} preserve {preserve}");
                let mut mapped = frame;
                assert!(photo_filter_frame_f32(&mut mapped, &grade));
                for (index, pixel) in mapped.iter().enumerate() {
                    assert_eq!(bits(*pixel), bits(frame[index]), "pixel {index}");
                }
            }
        }
    }

    /// K2: the preset table is complete, unique and round-trips through its keys.
    #[test]
    fn preset_table_is_complete_and_round_trips() {
        assert_eq!(PhotoFilterPreset::ALL.len(), 21);
        let mut keys = std::collections::BTreeSet::new();
        for preset in PhotoFilterPreset::ALL {
            assert!(
                keys.insert(preset.as_key()),
                "duplicate key {}",
                preset.as_key()
            );
            assert_eq!(PhotoFilterPreset::from_key(preset.as_key()), Some(preset));
        }
        assert_eq!(keys.len(), 21);
        assert_eq!(PhotoFilterPreset::from_key("nope"), None);
        assert_eq!(PhotoFilterPreset::from_key("Warming_85"), None);
        assert_eq!(PhotoFilterPreset::Custom.swatch(), None);
        assert_eq!(
            PhotoFilterPreset::WarmingFilter85.swatch(),
            Some(PHOTO_FILTER_DEFAULT_COLOR)
        );
        for preset in PhotoFilterPreset::ALL {
            if preset != PhotoFilterPreset::Custom {
                let swatch = preset.swatch().expect("a named preset has a swatch");
                assert!(
                    swatch.iter().all(|channel| (0.0..=1.0).contains(channel)),
                    "{} leaves the unit range",
                    preset.as_key()
                );
            }
        }
    }

    /// K3: full density transmits the gel bit for bit.
    #[test]
    fn full_density_transmits_the_swatch_exactly() {
        let source = [0.5, 0.5, 0.5, 0.75];
        let grade = warming_85(100.0, false);
        assert_eq!(grade.gain(), [236.0 / 255.0, 138.0 / 255.0, 0.0]);
        let mapped = grade.apply_pixel(source);
        let expected = [
            0.5 * (236.0 / 255.0),
            0.5 * (138.0 / 255.0),
            0.5 * 0.0,
            0.75,
        ];
        assert_eq!(bits(mapped), bits(expected));
    }

    /// K4: the default preset at the default density darkens a mid grey.
    #[test]
    fn default_grade_darkens_grey_by_the_expected_amount() {
        let grade = warming_85(PHOTO_FILTER_DEFAULT_DENSITY, false);
        let mapped = grade.apply_pixel([0.5, 0.5, 0.5, 1.0]);
        close(mapped[0], 0.490_686_3, 1.0e-7);
        close(mapped[1], 0.442_647_1, 1.0e-7);
        close(mapped[2], 0.375, 1.0e-7);
        close(mapped[3], 1.0, 0.0);
        for value in mapped.iter().take(3) {
            assert!(*value < 0.5, "every channel must darken: {value}");
        }
    }

    /// K5: preserve keeps the source luma and the gain ratio.
    #[test]
    fn preserve_luminosity_keeps_the_luma_and_the_ratio() {
        let source = [0.5, 0.5, 0.5, 1.0];
        let grade = warming_85(100.0, true);
        let gain = grade.gain();
        let mapped = grade.apply_pixel(source);
        close(luma([mapped[0], mapped[1], mapped[2]]), 0.5, 1.0e-6);
        close(mapped[0] / mapped[1], gain[0] / gain[1], 1.0e-6);
        // The blue gain is exactly zero, so the third channel stays at zero and
        // the ratio there is trivially the gain ratio as well.
        assert_eq!(mapped[2], 0.0);
        assert_eq!(mapped[3], 1.0);
    }

    /// K6: without preserve the luma follows the gain and drops visibly.
    #[test]
    fn without_preserve_the_luma_follows_the_gain() {
        let source = [0.5, 0.5, 0.5, 1.0];
        let grade = warming_85(100.0, false);
        let mapped = grade.apply_pixel(source);
        let expected = luma([
            source[0] * grade.gain()[0],
            source[1] * grade.gain()[1],
            source[2] * grade.gain()[2],
        ]);
        assert_eq!(
            luma([mapped[0], mapped[1], mapped[2]]).to_bits(),
            expected.to_bits()
        );
        assert!((expected - 0.5).abs() > 0.1);
    }

    /// K7: an uncovered sample is returned untouched.
    #[test]
    fn uncovered_samples_are_returned_untouched() {
        let grade = warming_85(100.0, true);
        for source in [
            [0.0, 0.25, 0.5, 0.0],
            [0.4, 0.5, 0.6, -0.0],
            [1.0, 0.0, 0.5, -1.5],
            [0.7, 0.8, 0.9, f32::NAN],
        ] {
            assert_eq!(bits(grade.apply_pixel(source)), bits(source));
        }
    }

    /// K8: alpha is never touched.
    #[test]
    fn alpha_passes_through_unchanged() {
        for preserve in [false, true] {
            let grade = warming_85(60.0, preserve);
            for alpha in [1.0e-6, 0.25, 0.5, 1.0, 2.0] {
                let mapped = grade.apply_pixel([0.1, 0.2, 0.3, alpha]);
                assert_eq!(bits([mapped[3]; 4]), bits([alpha; 4]), "alpha {alpha}");
            }
        }
    }

    /// K9: out-of-range samples are not clamped.
    #[test]
    fn out_of_range_samples_are_not_clamped() {
        let source = [1.5, 2.0, -0.25, 1.0];
        let grade = warming_85(PHOTO_FILTER_DEFAULT_DENSITY, false);
        let gain = grade.gain();
        let mapped = grade.apply_pixel(source);
        assert_eq!(
            bits(mapped),
            bits([
                source[0] * gain[0],
                source[1] * gain[1],
                source[2] * gain[2],
                source[3]
            ])
        );
        assert!(mapped[0] > 1.0);
        assert!(mapped[2] < 0.0);
    }

    /// K10: a fully absorbed channel reaches zero without producing a NaN.
    #[test]
    fn a_fully_absorbed_channel_reaches_zero_without_nan() {
        let source = [0.0, 0.0, 1.0, 1.0];
        for preserve in [false, true] {
            let mapped = warming_85(100.0, preserve).apply_pixel(source);
            assert_eq!(
                bits(mapped),
                bits([0.0, 0.0, 0.0, 1.0]),
                "preserve {preserve}"
            );
        }
    }

    /// K11: a negative source luma skips the scale instead of flipping the sign.
    #[test]
    fn a_negative_source_luma_skips_the_scale() {
        let source = [0.0, 0.0, -0.5, 1.0];
        let grade = warming_85(PHOTO_FILTER_DEFAULT_DENSITY, true);
        let gain = grade.gain();
        let mapped = grade.apply_pixel(source);
        assert_eq!(bits(mapped), bits([0.0, 0.0, source[2] * gain[2], 1.0]));
        assert!(mapped[2] < 0.0);
    }

    /// K12: invalid parameters are rejected with their own variant.
    #[test]
    fn invalid_parameters_are_rejected_with_their_own_variant() {
        for density in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.5, 100.5] {
            assert_eq!(
                PhotoFilterGrade::new(PHOTO_FILTER_DEFAULT_COLOR, density, false),
                Err(PhotoFilterError::Density),
                "density {density}"
            );
        }
        for colour in [
            [f32::NAN, 0.0, 0.0],
            [0.0, f32::INFINITY, 0.0],
            [0.0, 0.0, f32::NEG_INFINITY],
        ] {
            assert_eq!(
                PhotoFilterGrade::new(colour, PHOTO_FILTER_DEFAULT_DENSITY, false),
                Err(PhotoFilterError::Color),
                "colour {colour:?}"
            );
        }
        for density in [PHOTO_FILTER_MIN_DENSITY, PHOTO_FILTER_MAX_DENSITY] {
            assert!(PhotoFilterGrade::new(PHOTO_FILTER_DEFAULT_COLOR, density, false).is_ok());
        }
        assert!(PhotoFilterGrade::new([1.2, -0.5, 2.0], 50.0, true).is_ok());
        assert_eq!(
            PhotoFilterError::Color.to_string(),
            "photo filter colour must be finite"
        );
        assert_eq!(
            PhotoFilterError::Density.to_string(),
            "photo filter density must be finite and within 0..=100"
        );
        assert_ne!(PhotoFilterError::Color, PhotoFilterError::Density);
    }

    /// K13: identity is exactly a unit gain.
    #[test]
    fn identity_is_exactly_a_unit_gain() {
        assert!(grade(PHOTO_FILTER_DEFAULT_COLOR, 0.0, false).is_identity());
        assert!(grade(PHOTO_FILTER_DEFAULT_COLOR, 0.0, true).is_identity());
        assert!(!warming_85(PHOTO_FILTER_DEFAULT_DENSITY, false).is_identity());
        assert!(grade([1.0, 1.0, 1.0], 100.0, true).is_identity());
    }

    /// K14: checkpoints follow the chunk cadence.
    #[test]
    fn checkpoints_follow_the_chunk_cadence() {
        let grade = warming_85(PHOTO_FILTER_DEFAULT_DENSITY, false);

        let mut empty: [[f32; 4]; 0] = [];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        apply_photo_filter_f32_controlled(&mut empty, &grade, &mut checkpoint)
            .expect("no cancellation");
        assert_eq!(calls, 1, "an empty frame still yields once");

        let mut frame = vec![[0.5, 0.5, 0.5, 1.0]; CHECKPOINT_STRIDE + 1];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        apply_photo_filter_f32_controlled(&mut frame, &grade, &mut checkpoint)
            .expect("no cancellation");
        assert_eq!(calls, 3, "two chunks plus the trailing yield");
    }
}
