//! Film look: a single grade that reproduces one film stock's cast, contrast,
//! grain, halation, glow, and fade.
//!
//! The effect is a three-pass spatial operation. Pass one extracts the
//! highlight energy and blurs it horizontally, pass two blurs that plane
//! vertically, and pass three grades the source and adds the bloom.
//!
//! Extraction. The luminance of the straight source colour is
//!
//! ```text
//! luma = 0.2126 r + 0.7152 g + 0.0722 b
//! w    = smoothstep(0.6, 1.2, luma)
//! ```
//!
//! and the blurred plane carries `[rgb * w, w]`. Both the colour and the weight
//! are blurred with the same separable tent, so `plane.a` is the bloom amount
//! at that texel and `plane.rgb / plane.a` is its average colour.
//!
//! Tent. The radius is a fixed `12` pixels — a lens point-spread function does
//! not scale with the frame — with `w(d) = 1 / (1 + d*d)` for `d = 0..=12`,
//! clamped edge sampling, and each axis normalised by the weight sum.
//!
//! Grade. With `t = smoothstep(0.15, 0.85, luma)` the stock's split tone is
//!
//! ```text
//! gain = shadow_tint + (highlight_tint - shadow_tint) * t
//! rgb *= gain
//! rgb  = luma + (rgb - luma) * saturation
//! rgb  = (rgb - 0.18) * (1 + contrast) + 0.18
//! lift = fade / 100 * 0.35
//! rgb  = rgb * (1 - lift) + 0.12 * lift
//! ```
//!
//! and then the grain, glow, and halation are added:
//!
//! ```text
//! grain    = hash(x, y, seed) * grain / 100 * 0.06 * stock_grain * (1 - 0.6 * clamp(luma))
//! glow     = bloom_colour * glow / 100 * 0.6 * bloom_amount
//! halation = (1.0, 0.32, 0.26) * halation / 100 * 0.8 * stock_halation * bloom_amount
//! ```
//!
//! The last step mixes the graded colour with the source by `strength / 100`
//! and keeps the source alpha. Negative values are clamped to zero; the upper
//! end is left open so the grade stays valid in scene-linear HDR.
//!
//! Stocks are Mondrian's own six profiles: a split tone, a saturation, a
//! contrast bias, and two scale factors. They do not reproduce a vendor LUT.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Documented default stock.
pub const FILM_LOOK_DEFAULT_STOCK: FilmStock = FilmStock::KodakPortra;
/// Documented default strength, in percent.
pub const FILM_LOOK_DEFAULT_STRENGTH: f32 = 100.0;
/// Documented default grain amount, in percent.
pub const FILM_LOOK_DEFAULT_GRAIN: f32 = 0.0;
/// Documented default halation amount, in percent.
pub const FILM_LOOK_DEFAULT_HALATION: f32 = 0.0;
/// Documented default glow amount, in percent.
pub const FILM_LOOK_DEFAULT_GLOW: f32 = 0.0;
/// Documented default tint: neutral grey, i.e. a gain of one on every channel.
pub const FILM_LOOK_DEFAULT_TINT: [f32; 3] = [0.5, 0.5, 0.5];
/// Documented default contrast.
pub const FILM_LOOK_DEFAULT_CONTRAST: f32 = 0.0;
/// Documented default fade, in percent.
pub const FILM_LOOK_DEFAULT_FADE: f32 = 0.0;

/// Smallest accepted strength, in percent.
pub const FILM_LOOK_MIN_STRENGTH: f32 = 0.0;
/// Largest accepted strength, in percent.
pub const FILM_LOOK_MAX_STRENGTH: f32 = 100.0;
/// Smallest accepted amount for the three additive components, in percent.
pub const FILM_LOOK_MIN_AMOUNT: f32 = 0.0;
/// Largest accepted amount for the three additive components, in percent.
pub const FILM_LOOK_MAX_AMOUNT: f32 = 100.0;
/// Smallest accepted fade, in percent.
pub const FILM_LOOK_MIN_FADE: f32 = 0.0;
/// Largest accepted fade, in percent.
pub const FILM_LOOK_MAX_FADE: f32 = 100.0;
/// Smallest accepted contrast.
pub const FILM_LOOK_MIN_CONTRAST: f32 = -100.0;
/// Largest accepted contrast.
pub const FILM_LOOK_MAX_CONTRAST: f32 = 100.0;

/// Bloom tent radius in pixels.
pub const FILM_LOOK_BLOOM_RADIUS: u32 = 12;
/// Input halo in pixels: the tent radius plus one.
pub const FILM_LOOK_HALO: u32 = FILM_LOOK_BLOOM_RADIUS + 1;

/// Luminance above which a pixel feeds the bloom plane.
const BLOOM_LOW: f32 = 0.6;
/// Luminance at which a pixel feeds the bloom plane fully.
const BLOOM_HIGH: f32 = 1.2;
/// Mid grey pivot of the contrast control, in scene-linear light.
const MID_GREY: f32 = 0.18;
/// How far a full fade lifts the blacks toward the floor.
const FADE_DEPTH: f32 = 0.35;
/// The grey a full fade lifts the blacks toward.
const FADE_FLOOR: f32 = 0.12;
/// Peak grain amplitude at full amount.
const GRAIN_DEPTH: f32 = 0.06;
/// How much the grain fades out towards the highlights.
const GRAIN_HIGHLIGHT_FALLOFF: f32 = 0.6;
/// How much a full glow amount brightens the frame.
const GLOW_DEPTH: f32 = 0.6;
/// How much a full halation amount brightens the frame.
const HALATION_DEPTH: f32 = 0.8;
/// Colour of the halation bloom: a red-biased film-base flare.
const HALATION_TINT: [f32; 3] = [1.0, 0.32, 0.26];
/// Lower clamp applied to the authored contrast before the curve is built.
const CONTRAST_FLOOR: f32 = -0.95;
/// Upper clamp applied to the authored contrast before the curve is built.
const CONTRAST_CEILING: f32 = 2.0;
/// Never divide a bloom plane by a zero weight.
const BLOOM_EPSILON: f32 = 1e-4;
/// Neutral tint component; dividing by it turns the colour into a gain.
const NEUTRAL_TINT: f32 = 0.5;

/// One built-in film stock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilmStock {
    /// Warm, low-contrast portrait stock.
    KodakPortra,
    /// Saturated, punchy stock.
    KodakEktar,
    /// Very saturated slide stock with cool shadows.
    FujiVelvia,
    /// Tungsten-balanced stock with a strong red halation.
    Cinestill800T,
    /// Monochrome stock.
    IlfordHp5,
    /// Warm, contrasty reversal stock.
    Kodachrome64,
}

/// The look of one stock: split tone, saturation, contrast bias, and scales.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilmStockProfile {
    /// Multiplicative shadow tint.
    pub shadow_tint: [f32; 3],
    /// Multiplicative highlight tint.
    pub highlight_tint: [f32; 3],
    /// Saturation multiplier around the luminance axis.
    pub saturation: f32,
    /// Contrast bias added to the authored contrast, on the `-1..=1` scale.
    pub contrast: f32,
    /// Scale factor on the authored grain amount.
    pub grain_gain: f32,
    /// Scale factor on the authored halation amount.
    pub halation_gain: f32,
}

impl FilmStock {
    /// Every stock in display order.
    pub const ALL: [Self; 6] = [
        Self::KodakPortra,
        Self::KodakEktar,
        Self::FujiVelvia,
        Self::Cinestill800T,
        Self::IlfordHp5,
        Self::Kodachrome64,
    ];

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::KodakPortra => "kodak_portra",
            Self::KodakEktar => "kodak_ektar",
            Self::FujiVelvia => "fuji_velvia",
            Self::Cinestill800T => "cinestill_800t",
            Self::IlfordHp5 => "ilford_hp5",
            Self::Kodachrome64 => "kodachrome_64",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_key() == key)
    }

    /// Packed tag used by the spatial pass uniform.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::KodakPortra => 1,
            Self::KodakEktar => 2,
            Self::FujiVelvia => 3,
            Self::Cinestill800T => 4,
            Self::IlfordHp5 => 5,
            Self::Kodachrome64 => 6,
        }
    }

    /// The menu caption shown for this stock.
    pub const fn label(self) -> &'static str {
        match self {
            Self::KodakPortra => "柯达 Portra 400",
            Self::KodakEktar => "柯达 Ektar 100",
            Self::FujiVelvia => "富士 Velvia 50",
            Self::Cinestill800T => "CineStill 800T",
            Self::IlfordHp5 => "伊尔福 HP5 Plus",
            Self::Kodachrome64 => "柯达 Kodachrome 64",
        }
    }

    /// The tint, saturation, contrast, and scales of this stock.
    pub const fn profile(self) -> FilmStockProfile {
        match self {
            Self::KodakPortra => FilmStockProfile {
                shadow_tint: [0.99, 1.00, 1.03],
                highlight_tint: [1.02, 1.00, 0.98],
                saturation: 0.96,
                contrast: 0.0,
                grain_gain: 1.0,
                halation_gain: 1.0,
            },
            Self::KodakEktar => FilmStockProfile {
                shadow_tint: [0.98, 1.00, 1.04],
                highlight_tint: [1.02, 1.00, 0.97],
                saturation: 1.18,
                contrast: 0.15,
                grain_gain: 0.9,
                halation_gain: 0.9,
            },
            Self::FujiVelvia => FilmStockProfile {
                shadow_tint: [0.96, 1.00, 1.02],
                highlight_tint: [0.99, 1.00, 1.02],
                saturation: 1.35,
                contrast: 0.22,
                grain_gain: 1.1,
                halation_gain: 0.8,
            },
            Self::Cinestill800T => FilmStockProfile {
                shadow_tint: [0.92, 0.98, 1.12],
                highlight_tint: [1.03, 1.00, 0.98],
                saturation: 1.05,
                contrast: 0.05,
                grain_gain: 1.2,
                halation_gain: 2.0,
            },
            Self::IlfordHp5 => FilmStockProfile {
                shadow_tint: [1.00, 1.00, 1.00],
                highlight_tint: [1.00, 1.00, 1.00],
                saturation: 0.0,
                contrast: 0.18,
                grain_gain: 1.5,
                halation_gain: 0.6,
            },
            Self::Kodachrome64 => FilmStockProfile {
                shadow_tint: [1.02, 0.99, 0.96],
                highlight_tint: [1.00, 1.00, 0.92],
                saturation: 1.12,
                contrast: 0.12,
                grain_gain: 1.1,
                halation_gain: 0.7,
            },
        }
    }
}

impl Default for FilmStock {
    fn default() -> Self {
        FILM_LOOK_DEFAULT_STOCK
    }
}

/// The authored film look settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilmLookParameters {
    /// Which built-in stock is reproduced.
    pub stock: FilmStock,
    /// How much of the grade reaches the frame, in percent.
    pub strength: f32,
    /// Monochrome grain amount, in percent.
    pub grain: f32,
    /// Red-biased halation bloom amount, in percent.
    pub halation: f32,
    /// White bloom amount, in percent.
    pub glow: f32,
    /// Overall colour cast, where `(0.5, 0.5, 0.5)` is neutral.
    pub tint: [f32; 3],
    /// Contrast around the mid-grey pivot, in percent.
    pub contrast: f32,
    /// Black lift, in percent.
    pub fade: f32,
}

impl Default for FilmLookParameters {
    fn default() -> Self {
        Self {
            stock: FILM_LOOK_DEFAULT_STOCK,
            strength: FILM_LOOK_DEFAULT_STRENGTH,
            grain: FILM_LOOK_DEFAULT_GRAIN,
            halation: FILM_LOOK_DEFAULT_HALATION,
            glow: FILM_LOOK_DEFAULT_GLOW,
            tint: FILM_LOOK_DEFAULT_TINT,
            contrast: FILM_LOOK_DEFAULT_CONTRAST,
            fade: FILM_LOOK_DEFAULT_FADE,
        }
    }
}

/// Reports the first authored value that falls outside its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("film_look.{parameter} must be finite and within its documented range")]
pub struct FilmLookError {
    parameter: &'static str,
}

impl FilmLookError {
    /// Stable snake_case id of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// The four spatial-pass uniform cells produced by [`FilmLookGrade::gpu_controls`].
pub type FilmLookGpuControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated film look configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilmLookGrade {
    parameters: FilmLookParameters,
    grain_seed: u32,
}

impl FilmLookGrade {
    /// Validates one authored configuration with a frozen grain pattern.
    pub fn new(parameters: FilmLookParameters) -> Result<Self, FilmLookError> {
        let numeric = [
            (
                "strength",
                parameters.strength,
                FILM_LOOK_MIN_STRENGTH,
                FILM_LOOK_MAX_STRENGTH,
            ),
            (
                "grain",
                parameters.grain,
                FILM_LOOK_MIN_AMOUNT,
                FILM_LOOK_MAX_AMOUNT,
            ),
            (
                "halation",
                parameters.halation,
                FILM_LOOK_MIN_AMOUNT,
                FILM_LOOK_MAX_AMOUNT,
            ),
            (
                "glow",
                parameters.glow,
                FILM_LOOK_MIN_AMOUNT,
                FILM_LOOK_MAX_AMOUNT,
            ),
            (
                "contrast",
                parameters.contrast,
                FILM_LOOK_MIN_CONTRAST,
                FILM_LOOK_MAX_CONTRAST,
            ),
            (
                "fade",
                parameters.fade,
                FILM_LOOK_MIN_FADE,
                FILM_LOOK_MAX_FADE,
            ),
        ];
        for (parameter, value, minimum, maximum) in numeric {
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(FilmLookError { parameter });
            }
        }
        for component in parameters.tint {
            if !component.is_finite() || !(0.0..=1.0).contains(&component) {
                return Err(FilmLookError { parameter: "tint" });
            }
        }
        Ok(Self { parameters, grain_seed: 0 })
    }

    /// Documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(FilmLookParameters::default())
            .expect("the documented film look defaults are in range")
    }

    /// Seeds the grain pattern for one frame.
    ///
    /// Grain is the only time-varying component: the caller derives the seed
    /// from the frame coordinate so the pattern changes per frame but stays
    /// deterministic for a given frame. A graded frame with no grain keeps the
    /// fixed seed so its graph fingerprint does not churn.
    pub fn at_frame(mut self, frame_coordinate: f64) -> Self {
        if self.parameters.grain > 0.0 {
            self.grain_seed = Self::seed_for_frame(frame_coordinate);
        }
        self
    }

    /// Deterministic grain seed for one frame coordinate.
    pub fn seed_for_frame(frame_coordinate: f64) -> u32 {
        let frame = frame_coordinate as i64 as u64;
        let mixed = frame.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        ((mixed >> 32) as u32) ^ 0x85EB_CA6B
    }

    /// The authored values.
    pub const fn parameters(&self) -> &FilmLookParameters {
        &self.parameters
    }

    /// The frozen grain pattern seed.
    pub const fn grain_seed(&self) -> u32 {
        self.grain_seed
    }

    /// Whether the configuration leaves every pixel untouched.
    pub const fn is_identity(&self) -> bool {
        self.parameters.strength <= 0.0
    }

    /// Relative cost of the three spatial passes.
    pub const fn cost(&self) -> u8 {
        3
    }

    /// Radius of the bloom tent, in pixels.
    pub const fn bloom_radius(&self) -> u32 {
        FILM_LOOK_BLOOM_RADIUS
    }

    /// Halo the bloom reads around each pixel, in pixels.
    pub const fn input_halo(&self) -> u32 {
        FILM_LOOK_HALO
    }

    /// Packs the grade into the four spatial-pass uniform cells.
    ///
    /// The cells are `[strength, contrast, fade, saturation]`,
    /// `[glow gain, halation gain, radius, grain seed]`,
    /// `[shadow r, shadow g, shadow b, grain amplitude]`, and
    /// `[highlight r, highlight g, highlight b, 0]`. The stock's split tone and
    /// tint are multiplied out here so the shader never carries a stock table,
    /// and the grain amplitude already includes the authored amount.
    pub fn gpu_controls(&self) -> FilmLookGpuControls {
        let parameters = &self.parameters;
        let profile = parameters.stock.profile();
        let tint_gain = [
            parameters.tint[0] / NEUTRAL_TINT,
            parameters.tint[1] / NEUTRAL_TINT,
            parameters.tint[2] / NEUTRAL_TINT,
        ];
        let shadow = [
            profile.shadow_tint[0] * tint_gain[0],
            profile.shadow_tint[1] * tint_gain[1],
            profile.shadow_tint[2] * tint_gain[2],
        ];
        let highlight = [
            profile.highlight_tint[0] * tint_gain[0],
            profile.highlight_tint[1] * tint_gain[1],
            profile.highlight_tint[2] * tint_gain[2],
        ];
        let params = [
            parameters.strength / 100.0,
            parameters.contrast / 100.0 + profile.contrast,
            parameters.fade / 100.0,
            profile.saturation,
        ];
        let extra = [
            parameters.glow / 100.0 * GLOW_DEPTH,
            parameters.halation / 100.0 * HALATION_DEPTH * profile.halation_gain,
            FILM_LOOK_BLOOM_RADIUS as f32,
            self.grain_seed as f32,
        ];
        let aces = [
            shadow[0],
            shadow[1],
            shadow[2],
            GRAIN_DEPTH * profile.grain_gain * parameters.grain / 100.0,
        ];
        let extra1 = [highlight[0], highlight[1], highlight[2], 0.0];
        (params, extra, aces, extra1)
    }

    /// Feeds the whole configuration into a graph or plan fingerprint.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        self.parameters.stock.as_u8().hash(state);
        for value in [
            self.parameters.strength,
            self.parameters.grain,
            self.parameters.halation,
            self.parameters.glow,
            self.parameters.contrast,
            self.parameters.fade,
        ] {
            value.to_bits().hash(state);
        }
        for component in self.parameters.tint {
            component.to_bits().hash(state);
        }
        self.grain_seed.hash(state);
    }
}

/// Applies the film look to one tightly packed frame.
pub fn film_look_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FilmLookGrade,
) -> bool {
    apply_film_look_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Cancellation-aware film look kernel over one tightly packed frame.
pub(crate) fn apply_film_look_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FilmLookGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    checkpoint()?;

    let parameters = &grade.parameters;
    let radius = FILM_LOOK_BLOOM_RADIUS as usize;
    let weights = tent_weights(radius);
    let mut horizontal = vec![[0.0_f32; 4]; pixel_count];

    // Pass one: extract the highlight energy and blur it along the rows.
    crate::parallel::for_each_row_band(&mut horizontal, width, 0..height, |row_start, band| {
        for (index, target) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let mut accumulated = [0.0_f32; 4];
            for offset in 0..=radius * 2 {
                let weight = weights[offset.abs_diff(radius)];
                let tap = column as isize + offset as isize - radius as isize;
                let tap = tap.clamp(0, width as isize - 1) as usize;
                let source = working[row * width + tap];
                let straight = straight_colour(source);
                let luma = luma(straight);
                let energy = smoothstep(BLOOM_LOW, BLOOM_HIGH, luma);
                accumulated[0] += straight[0] * energy * weight;
                accumulated[1] += straight[1] * energy * weight;
                accumulated[2] += straight[2] * energy * weight;
                accumulated[3] += energy * weight;
            }
            *target = accumulated;
        }
    });
    checkpoint()?;

    // Pass two: blur the horizontal plane along the columns.
    let mut vertical = vec![[0.0_f32; 4]; pixel_count];
    crate::parallel::for_each_row_band(&mut vertical, width, 0..height, |row_start, band| {
        for (index, target) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let mut accumulated = [0.0_f32; 4];
            for offset in 0..=radius * 2 {
                let weight = weights[offset.abs_diff(radius)];
                let tap = row as isize + offset as isize - radius as isize;
                let tap = tap.clamp(0, height as isize - 1) as usize;
                let source = horizontal[tap * width + column];
                accumulated[0] += source[0] * weight;
                accumulated[1] += source[1] * weight;
                accumulated[2] += source[2] * weight;
                accumulated[3] += source[3] * weight;
            }
            *target = accumulated;
        }
    });
    checkpoint()?;

    // Pass three: grade the source and add the bloom in place.
    let profile = parameters.stock.profile();
    let contrast =
        (parameters.contrast / 100.0 + profile.contrast).clamp(CONTRAST_FLOOR, CONTRAST_CEILING);
    let contrast_scale = 1.0 + contrast;
    let fade = parameters.fade / 100.0;
    let lift = fade * FADE_DEPTH;
    let tint_gain = [
        parameters.tint[0] / NEUTRAL_TINT,
        parameters.tint[1] / NEUTRAL_TINT,
        parameters.tint[2] / NEUTRAL_TINT,
    ];
    let shadow = [
        profile.shadow_tint[0] * tint_gain[0],
        profile.shadow_tint[1] * tint_gain[1],
        profile.shadow_tint[2] * tint_gain[2],
    ];
    let highlight = [
        profile.highlight_tint[0] * tint_gain[0],
        profile.highlight_tint[1] * tint_gain[1],
        profile.highlight_tint[2] * tint_gain[2],
    ];
    let saturation = profile.saturation;
    let grain_amount = parameters.grain / 100.0 * GRAIN_DEPTH * profile.grain_gain;
    let glow_gain = parameters.glow / 100.0 * GLOW_DEPTH;
    let halation_gain = parameters.halation / 100.0 * HALATION_DEPTH * profile.halation_gain;
    let strength = parameters.strength / 100.0;
    let seed = grade.grain_seed;

    crate::parallel::for_each_row_band(working, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let alpha = pixel[3].clamp(0.0, 1.0);
            if !crate::coverage::has_positive_coverage(alpha) {
                continue;
            }
            let source = straight_colour(*pixel);
            let source_luma = luma(source);
            let tone = smoothstep(0.15, 0.85, source_luma);
            let mut graded = [0.0_f32; 3];
            for channel in 0..3 {
                let gain = shadow[channel] + (highlight[channel] - shadow[channel]) * tone;
                let toned = source[channel] * gain;
                let saturated = source_luma + (toned - source_luma) * saturation;
                let contrasted = (saturated - MID_GREY) * contrast_scale + MID_GREY;
                graded[channel] = contrasted * (1.0 - lift) + FADE_FLOOR * lift;
            }
            if grain_amount > 0.0 {
                let noise = pixel_noise(seed, column as u32, row as u32);
                let falloff = 1.0 - GRAIN_HIGHLIGHT_FALLOFF * source_luma.clamp(0.0, 1.0);
                let grain = noise * grain_amount * falloff;
                for channel in &mut graded {
                    *channel += grain;
                }
            }
            let bloom = vertical[row * width + column];
            let amount = bloom[3];
            if amount > 0.0 && (glow_gain > 0.0 || halation_gain > 0.0) {
                let colour = [
                    bloom[0] / amount.max(BLOOM_EPSILON),
                    bloom[1] / amount.max(BLOOM_EPSILON),
                    bloom[2] / amount.max(BLOOM_EPSILON),
                ];
                for channel in 0..3 {
                    graded[channel] += colour[channel] * glow_gain * amount
                        + HALATION_TINT[channel] * halation_gain * amount;
                }
            }
            for channel in 0..3 {
                let mixed = source[channel] + (graded[channel] - source[channel]) * strength;
                graded[channel] = mixed.max(0.0);
            }
            *pixel = [
                graded[0] * alpha,
                graded[1] * alpha,
                graded[2] * alpha,
                alpha,
            ];
        }
    });
    checkpoint()?;
    Ok(true)
}

/// Normalised tent weights for `d = 0..=radius`.
fn tent_weights(radius: usize) -> Vec<f32> {
    let mut weights: Vec<f32> = (0..=radius)
        .map(|offset| {
            let offset = offset as f32;
            1.0 / (1.0 + offset * offset)
        })
        .collect();
    let total = weights[0] + 2.0 * weights[1..].iter().sum::<f32>();
    for weight in &mut weights {
        *weight /= total;
    }
    weights
}

/// The straight colour of one premultiplied pixel.
fn straight_colour(pixel: [f32; 4]) -> [f32; 3] {
    let alpha = pixel[3].clamp(0.0, 1.0);
    if !crate::coverage::has_positive_coverage(alpha) {
        return [0.0; 3];
    }
    let scale = 1.0 / alpha;
    [pixel[0] * scale, pixel[1] * scale, pixel[2] * scale]
}

/// Rec.709 luminance of a straight colour.
fn luma(colour: [f32; 3]) -> f32 {
    0.2126 * colour[0] + 0.7152 * colour[1] + 0.0722 * colour[2]
}

/// Hermite ramp between two edges.
fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    if high <= low {
        return if value >= high { 1.0 } else { 0.0 };
    }
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Deterministic monochrome noise in `[-1, 1]` for one pixel and seed.
fn pixel_noise(seed: u32, x: u32, y: u32) -> f32 {
    let mut hash = seed ^ x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B);
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7FEB_352D);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x846C_A68B);
    hash ^= hash >> 16;
    (hash as f32 / u32::MAX as f32) * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(parameters: FilmLookParameters) -> FilmLookGrade {
        FilmLookGrade::new(parameters).expect("the parameters are in range")
    }

    fn rejected(parameters: FilmLookParameters) -> FilmLookError {
        FilmLookGrade::new(parameters).expect_err("the parameters must be rejected")
    }

    fn frame(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    fn render(width: usize, height: usize, parameters: FilmLookParameters) -> Vec<[f32; 4]> {
        let mut pixels = frame(width, height, [0.25, 0.25, 0.25, 1.0]);
        assert!(film_look_frame_f32(
            &mut pixels,
            width,
            height,
            &valid(parameters)
        ));
        pixels
    }

    #[test]
    fn documented_defaults_are_the_neutral_grade() {
        let grade = FilmLookGrade::documented_defaults();
        let parameters = grade.parameters();
        assert_eq!(parameters.stock, FilmStock::KodakPortra);
        assert_eq!(parameters.strength, 100.0);
        assert_eq!(parameters.grain, 0.0);
        assert_eq!(parameters.halation, 0.0);
        assert_eq!(parameters.glow, 0.0);
        assert_eq!(parameters.tint, [0.5, 0.5, 0.5]);
        assert_eq!(parameters.contrast, 0.0);
        assert_eq!(parameters.fade, 0.0);
        assert!(!grade.is_identity());
    }

    #[test]
    fn zero_strength_is_identity() {
        let parameters = FilmLookParameters { strength: 0.0, ..FilmLookParameters::default() };
        assert!(valid(parameters).is_identity());
        let mut pixels = frame(4, 3, [0.2, 0.4, 0.6, 0.8]);
        let before = pixels.clone();
        assert!(film_look_frame_f32(&mut pixels, 4, 3, &valid(parameters)));
        assert_eq!(pixels, before);
    }

    #[test]
    fn every_parameter_rejects_out_of_range_values() {
        for (field, parameters) in [
            (
                "strength",
                FilmLookParameters { strength: 100.5, ..Default::default() },
            ),
            (
                "grain",
                FilmLookParameters { grain: -1.0, ..Default::default() },
            ),
            (
                "halation",
                FilmLookParameters { halation: 101.0, ..Default::default() },
            ),
            (
                "glow",
                FilmLookParameters { glow: f32::NAN, ..Default::default() },
            ),
            (
                "contrast",
                FilmLookParameters { contrast: 100.5, ..Default::default() },
            ),
            (
                "fade",
                FilmLookParameters { fade: -0.5, ..Default::default() },
            ),
            (
                "tint",
                FilmLookParameters { tint: [0.5, 0.5, 1.5], ..Default::default() },
            ),
        ] {
            assert_eq!(rejected(parameters).parameter(), field);
        }
    }

    #[test]
    fn stock_keys_round_trip() {
        for stock in FilmStock::ALL {
            assert_eq!(FilmStock::from_key(stock.as_key()), Some(stock));
            assert!(!stock.label().is_empty());
            assert!(
                FilmStock::ALL.iter().filter(|other| other.as_u8() == stock.as_u8()).count() == 1
            );
        }
        assert_eq!(FilmStock::from_key("unknown"), None);
    }

    #[test]
    fn portra_warms_the_highlights_and_cools_the_shadows() {
        let grade = FilmLookGrade::documented_defaults();
        let dark = render(
            1,
            1,
            FilmLookParameters { strength: 100.0, ..Default::default() },
        );
        // A dark flat frame keeps the shadow tint.
        assert!(
            dark[0][2] > dark[0][0],
            "portra shadows trend blue: {dark:?}"
        );

        let mut bright = frame(1, 1, [0.8, 0.8, 0.8, 1.0]);
        assert!(film_look_frame_f32(&mut bright, 1, 1, &grade));
        assert!(
            bright[0][0] > bright[0][2],
            "portra highlights trend warm: {bright:?}"
        );
    }

    #[test]
    fn ilford_desaturates_every_channel_equally() {
        let parameters = FilmLookParameters {
            stock: FilmStock::IlfordHp5,
            contrast: 0.0,
            ..Default::default()
        };
        let pixels = render(1, 1, parameters);
        assert!((pixels[0][0] - pixels[0][1]).abs() < 1e-5);
        assert!((pixels[0][1] - pixels[0][2]).abs() < 1e-5);
    }

    #[test]
    fn stock_profiles_are_finite() {
        for stock in FilmStock::ALL {
            let profile = stock.profile();
            assert!(profile.saturation.is_finite());
            assert!(profile.contrast.is_finite());
            assert!(profile.grain_gain > 0.0);
            assert!(profile.halation_gain > 0.0);
        }
    }

    #[test]
    fn positive_contrast_pushes_away_from_mid_grey() {
        let parameters = FilmLookParameters {
            stock: FilmStock::IlfordHp5,
            contrast: 50.0,
            ..Default::default()
        };
        let mut bright = frame(1, 1, [0.4, 0.4, 0.4, 1.0]);
        assert!(film_look_frame_f32(&mut bright, 1, 1, &valid(parameters)));
        assert!(bright[0][0] > 0.4, "bright side rises: {bright:?}");

        let mut dark = frame(1, 1, [0.1, 0.1, 0.1, 1.0]);
        assert!(film_look_frame_f32(&mut dark, 1, 1, &valid(parameters)));
        assert!(dark[0][0] < 0.1, "dark side falls: {dark:?}");
    }

    #[test]
    fn fade_lifts_the_black_point() {
        let parameters = FilmLookParameters {
            stock: FilmStock::IlfordHp5,
            fade: 100.0,
            ..Default::default()
        };
        let pixels = render(1, 1, FilmLookParameters { ..parameters });
        assert!(pixels[0][0] > 0.03, "a full fade lifts black: {pixels:?}");
    }

    #[test]
    fn zero_grain_is_flat_and_grain_scales_with_the_amount() {
        let flat = render(8, 8, FilmLookParameters::default());
        let first = flat[0][0];
        for pixel in &flat {
            assert_eq!(pixel[0], first);
        }

        let noisy = render(
            8,
            8,
            FilmLookParameters { grain: 100.0, ..Default::default() },
        );
        let echoed = render(
            8,
            8,
            FilmLookParameters { grain: 100.0, ..Default::default() },
        );
        assert_eq!(noisy, echoed, "the same seed reproduces the same grain");
        let spread: f32 = noisy.iter().map(|pixel| (pixel[0] - first).abs()).sum();
        assert!(spread > 1e-3, "grain perturbs the plane: {spread}");
    }

    #[test]
    fn grain_seed_changes_the_pattern() {
        let parameters = FilmLookParameters { grain: 100.0, ..Default::default() };
        let left = valid(parameters).at_frame(1.0);
        let right = valid(parameters).at_frame(2.0);
        assert_ne!(left.grain_seed(), right.grain_seed());

        let mut first = frame(8, 8, [0.3, 0.3, 0.3, 1.0]);
        let mut second = first.clone();
        assert!(film_look_frame_f32(&mut first, 8, 8, &left));
        assert!(film_look_frame_f32(&mut second, 8, 8, &right));
        assert_ne!(first, second);
    }

    #[test]
    fn grain_averages_to_zero() {
        let pixels = render(
            64,
            64,
            FilmLookParameters { grain: 100.0, ..Default::default() },
        );
        let base = {
            let mut flat = frame(64, 64, [0.25, 0.25, 0.25, 1.0]);
            film_look_frame_f32(&mut flat, 64, 64, &valid(FilmLookParameters::default()));
            flat[0][0]
        };
        let mean: f32 = pixels.iter().map(|pixel| pixel[0] - base).sum::<f32>() / (64.0 * 64.0);
        assert!(mean.abs() < 0.01, "mean grain offset {mean}");
    }

    #[test]
    fn frozen_grain_does_not_move_the_seed() {
        let parameters = FilmLookParameters { grain: 40.0, ..Default::default() };
        let grade = valid(parameters);
        assert_eq!(grade.grain_seed(), 0);
        let frozen = grade.at_frame(0.0);
        assert_ne!(frozen.grain_seed(), 0, "at_frame seeds a grained grade");

        let without_grain = valid(FilmLookParameters { grain: 0.0, ..Default::default() });
        assert_eq!(without_grain.at_frame(12.0).grain_seed(), 0);
    }

    #[test]
    fn bright_pixels_bloom_into_their_neighbours() {
        let mut pixels = frame(16, 1, [0.0, 0.0, 0.0, 1.0]);
        pixels[8] = [2.0, 2.0, 2.0, 1.0];
        assert!(film_look_frame_f32(
            &mut pixels,
            16,
            1,
            &valid(FilmLookParameters { glow: 100.0, ..Default::default() }),
        ));
        assert!(pixels[7][0] > 0.0, "the highlight bleeds left: {pixels:?}");
        assert!(pixels[9][0] > 0.0, "the highlight bleeds right: {pixels:?}");
    }

    #[test]
    fn halation_is_red_biased() {
        let mut pixels = frame(16, 1, [0.0, 0.0, 0.0, 1.0]);
        pixels[8] = [2.0, 2.0, 2.0, 1.0];
        assert!(film_look_frame_f32(
            &mut pixels,
            16,
            1,
            &valid(FilmLookParameters { halation: 100.0, ..Default::default() }),
        ));
        let r = pixels[7][0];
        let g = pixels[7][1];
        let b = pixels[7][2];
        assert!(r > g, "red leads green: {r} vs {g}");
        assert!(g > b, "green leads blue: {g} vs {b}");
    }

    #[test]
    fn a_flat_frame_has_no_bloom() {
        let pixels = render(
            8,
            8,
            FilmLookParameters { glow: 100.0, halation: 100.0, ..Default::default() },
        );
        for pixel in &pixels {
            assert_eq!(pixel[0], pixels[0][0]);
        }
    }

    #[test]
    fn strength_blends_toward_the_source() {
        let muted = render(
            1,
            1,
            FilmLookParameters {
                stock: FilmStock::IlfordHp5,
                strength: 50.0,
                ..Default::default()
            },
        );
        let full = render(
            1,
            1,
            FilmLookParameters {
                stock: FilmStock::IlfordHp5,
                strength: 100.0,
                ..Default::default()
            },
        );
        assert!(muted[0][0] > 0.25 && muted[0][0] < full[0][0]);
    }

    #[test]
    fn alpha_is_preserved_and_premultiplied() {
        let mut pixels = frame(1, 1, [0.2, 0.3, 0.4, 0.5]);
        assert!(film_look_frame_f32(
            &mut pixels,
            1,
            1,
            &FilmLookGrade::documented_defaults()
        ));
        assert_eq!(pixels[0][3], 0.5);
        assert!(pixels[0][0] < 0.3, "the straight colour stays near 0.2");
    }

    #[test]
    fn empty_frames_are_rejected() {
        let mut pixels: Vec<[f32; 4]> = Vec::new();
        assert!(!film_look_frame_f32(
            &mut pixels,
            0,
            0,
            &FilmLookGrade::documented_defaults()
        ));
        let mut short = frame(2, 2, [0.0, 0.0, 0.0, 1.0]);
        short.truncate(3);
        assert!(!film_look_frame_f32(
            &mut short,
            2,
            2,
            &FilmLookGrade::documented_defaults()
        ));
    }

    #[test]
    fn gpu_controls_carry_the_stock_and_the_gains() {
        let grade = valid(FilmLookParameters { glow: 50.0, halation: 50.0, ..Default::default() });
        let (params, extra, aces, extra1) = grade.gpu_controls();
        assert_eq!(params[0], 1.0);
        assert_eq!(params[3], 0.96);
        assert!((extra[0] - 0.3).abs() < 1e-6);
        assert!((extra[1] - 0.4).abs() < 1e-6);
        assert_eq!(extra[2], FILM_LOOK_BLOOM_RADIUS as f32);
        assert_eq!(aces[0], 0.99);
        assert_eq!(aces[3], 0.0, "no grain means no amplitude");

        let grained =
            valid(FilmLookParameters { grain: 50.0, ..Default::default() }).gpu_controls();
        assert!((grained.2[3] - 0.03).abs() < 1e-6);
        assert_eq!(extra1[0], 1.02);
        assert_eq!(extra1[3], 0.0);
    }

    #[test]
    fn tint_is_folded_into_both_split_tones() {
        let neutral = valid(FilmLookParameters::default()).gpu_controls();
        let warm = valid(FilmLookParameters { tint: [0.6, 0.5, 0.4], ..Default::default() })
            .gpu_controls();
        assert!((warm.2[0] - neutral.2[0] * 1.2).abs() < 1e-6);
        assert!((warm.2[2] - neutral.2[2] * 0.8).abs() < 1e-6);
    }

    #[test]
    fn hash_state_separates_every_knob() {
        use std::collections::hash_map::DefaultHasher;

        fn digest(parameters: FilmLookParameters) -> u64 {
            let mut hasher = DefaultHasher::new();
            valid(parameters).hash_state(&mut hasher);
            hasher.finish()
        }

        let baseline = digest(FilmLookParameters::default());
        for parameters in [
            FilmLookParameters { stock: FilmStock::FujiVelvia, ..Default::default() },
            FilmLookParameters { strength: 40.0, ..Default::default() },
            FilmLookParameters { grain: 30.0, ..Default::default() },
            FilmLookParameters { halation: 30.0, ..Default::default() },
            FilmLookParameters { glow: 30.0, ..Default::default() },
            FilmLookParameters { tint: [0.4, 0.5, 0.6], ..Default::default() },
            FilmLookParameters { contrast: 20.0, ..Default::default() },
            FilmLookParameters { fade: 20.0, ..Default::default() },
        ] {
            assert_ne!(digest(parameters), baseline);
        }
    }

    #[test]
    fn tent_weights_are_normalised_and_symmetric() {
        let weights = tent_weights(FILM_LOOK_BLOOM_RADIUS as usize);
        assert_eq!(weights.len(), FILM_LOOK_BLOOM_RADIUS as usize + 1);
        let total = weights[0] + 2.0 * weights[1..].iter().sum::<f32>();
        assert!((total - 1.0).abs() < 1e-6);
        assert!(weights.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn cost_and_halo_describe_the_three_pass_shape() {
        let grade = FilmLookGrade::documented_defaults();
        assert_eq!(grade.cost(), 3);
        assert_eq!(grade.bloom_radius(), FILM_LOOK_BLOOM_RADIUS);
        assert_eq!(grade.input_halo(), FILM_LOOK_HALO);
        assert_eq!(FILM_LOOK_HALO, FILM_LOOK_BLOOM_RADIUS + 1);
    }
}
