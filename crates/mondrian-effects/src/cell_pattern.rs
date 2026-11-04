//! Cell pattern (GEN-06): a pixel-local Worley generator with twelve shapes.
//!
//! Cell pattern is a *generator*, not a filter: it reads no source pixel and
//! always writes `alpha = 1`. Every pixel is evaluated analytically from the
//! frame layer index alone, so the kernel is layer-size independent and can
//! serve the fused point shader without any scratch traffic.
//!
//! # Semantics
//!
//! The pixel centre is `floor(max(position, 0.0)) + 0.5` (never
//! `position + 0.5`). It is mapped into cell space with
//! `p = (centre - offset) / size`. A three-by-three neighbourhood of jittered
//! cell sites is visited with the shared [`crate::noise::noise_hash`] PRNG; the
//! nearest (F1) and second-nearest (F2) distances feed one of twelve shaping
//! formulas. `contrast` is the unnormalised After Effects slider
//! (`g = contrast * 0.01`, `w = 8 / max(contrast, 1)`, `w_r = min(w, 0.5)`);
//! `overflow` folds out-of-range values for the five contrast-group patterns
//! and is accepted but inert for the six sharpness patterns; `invert` is the
//! final complement. The result is output as `[v, v, v, 1.0]`.
//!
//! # Deviations from the design notes
//!
//! * The algorithm is not After Effects' undisclosed implementation. It is a
//!   reproducible Worley F1/F2 family with L1/L2 metrics; only the observable
//!   distinctions documented for AE (round bubbles, angular crystals, smooth
//!   plates, hard-edged crystallize, static plates ignoring evolution, finer
//!   HQ octaves, exact tiling, disperse regularity) hold.
//! * The checklist's `上下文滑块` is the export artifact for AE's *Contrast*;
//!   the parameter is authored as `对比度` / `Contrast` (label-level deviation
//!   only, range and default are unchanged).
//! * The menu has the twelve documented items; the undocumented 13th AE slot
//!   fails closed through [`CellPatternType::from_menu_checked`].
//! * The contrast curve is this module's mapping, not AE's.
//! * The tiling period is `cells * size` pixels because the point path has no
//!   layer size; it is not tied to the layer dimensions.
//! * `柔和固定` (soft clamp) is asymptotic, so its range is the open interval
//!   `(0, 1)` rather than the exact 0/255 endpoints. The design's written
//!   negative shoulder (`-(-x) / (1 + (-x))`) keeps negative values negative,
//!   contradicting its own prose, so both shoulders fold towards zero here.
//! * The evolution phase splits its integer and fractional parts *before*
//!   rebasing into `u32`; the design's `s = clamp(e) + 2^20` would quantise the
//!   phase to `1/8` steps (the `f32` ulp at `2^20`) and freeze interpolation.
//! * Both jitter components come from one hash (low and high halves), because
//!   the design defines a single scalar jitter formula per axis pair.
//! * `演化` freezes beyond `±2^20` because that is the exactly representable
//!   integer range of the phase index.
//! * [`CellPatternGrade::gpu_controls`] returns four uniform cells
//!   (`params`, `color`, `extra0`, `extra1`) to match the crate's other
//!   four-cell point effects; the design note showed three.
//! * The default grade is not the identity, so adding the effect always
//!   changes the picture — that is the correct generator semantic.

use std::fmt;

/// Default cell edge length in pixels.
pub const CELL_PATTERN_DEFAULT_SIZE: f32 = 60.0;
/// Smallest authored cell edge length in pixels.
pub const CELL_PATTERN_MIN_SIZE: f32 = 2.0;
/// Largest authored cell edge length in pixels.
pub const CELL_PATTERN_MAX_SIZE: f32 = 2000.0;
/// Default pattern-space translation in pixels.
pub const CELL_PATTERN_DEFAULT_OFFSET: [f32; 2] = [128.0, 128.0];
/// Largest authored absolute pattern-space translation in pixels.
pub const CELL_PATTERN_MAX_COORDINATE: f32 = 1_000_000.0;
/// Largest authored contrast value (the AE slider scale, default 100).
pub const CELL_PATTERN_MAX_CONTRAST: f32 = 10_000.0;
/// Largest authored feature-point dispersion, in cell units.
pub const CELL_PATTERN_MAX_DISPERSE: f32 = 1.5;
/// Largest authored tiling cell count per axis.
pub const CELL_PATTERN_MAX_CELLS: f32 = 1_000.0;
/// Largest authored random seed.
pub const CELL_PATTERN_MAX_SEED: f32 = 100_000.0;
/// Largest authored absolute evolution phase.
pub const CELL_PATTERN_MAX_EVOLUTION: f32 = 1_048_576.0;
/// Largest authored evolution cycle length, in revolutions.
pub const CELL_PATTERN_MAX_CYCLE_REVOLUTIONS: f32 = 88.0;

/// Hash salt for the cell-site jitter components.
pub const CELL_PATTERN_SALT_JITTER: u32 = 0x6A09_E667;
/// Hash salt for the per-cell luminance.
pub const CELL_PATTERN_SALT_ID: u32 = 0xBB67_AE85;
/// Hash salt for the mixed-crystals per-cell pattern pick.
pub const CELL_PATTERN_SALT_MIXED: u32 = 0x3C6E_F372;
/// Hash salt stride for the evolution phase index.
pub const CELL_PATTERN_SALT_STRIDE: u32 = 0x9E37_79B9;

/// Default cell pattern type.
pub const CELL_PATTERN_DEFAULT_PATTERN: CellPatternType = CellPatternType::Bubbles;
/// Default overflow mode.
pub const CELL_PATTERN_DEFAULT_OVERFLOW: CellPatternOverflow = CellPatternOverflow::Clip;

/// The twelve documented cell pattern types, in menu order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellPatternType {
    /// Round bubbles, one per cell, radius driven by the cell luminance.
    Bubbles,
    /// Angular crystals: the bubble formula on the L1 metric.
    Crystals,
    /// Smooth plates with dark cracks between them.
    Plates,
    /// Plates whose luminance does not follow the evolution phase.
    StaticPlates,
    /// Hard-edged plates without the feathered crack ramp.
    Crystallize,
    /// Soft quadratic pillows with no per-cell variation.
    Pillow,
    /// Two octaves of [`CellPatternType::Crystals`].
    CrystalsHq,
    /// Two octaves of [`CellPatternType::Plates`].
    PlatesHq,
    /// Two octaves of [`CellPatternType::StaticPlates`].
    StaticPlatesHq,
    /// Two octaves of [`CellPatternType::Crystallize`].
    CrystallizeHq,
    /// Per-cell choice between plates and crystals.
    MixedCrystals,
    /// Bright tubes running along the cell borders.
    Tubular,
}

impl CellPatternType {
    /// Every documented type, in menu order.
    pub const ALL: [Self; 12] = [
        Self::Bubbles,
        Self::Crystals,
        Self::Plates,
        Self::StaticPlates,
        Self::Crystallize,
        Self::Pillow,
        Self::CrystalsHq,
        Self::PlatesHq,
        Self::StaticPlatesHq,
        Self::CrystallizeHq,
        Self::MixedCrystals,
        Self::Tubular,
    ];

    /// Zero-based index stored in the uniform header.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::Bubbles => 0,
            Self::Crystals => 1,
            Self::Plates => 2,
            Self::StaticPlates => 3,
            Self::Crystallize => 4,
            Self::Pillow => 5,
            Self::CrystalsHq => 6,
            Self::PlatesHq => 7,
            Self::StaticPlatesHq => 8,
            Self::CrystallizeHq => 9,
            Self::MixedCrystals => 10,
            Self::Tubular => 11,
        }
    }

    /// One-based menu position. Only the UI and the docs use it.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.index() + 1
    }

    /// Stable authored key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Bubbles => "bubbles",
            Self::Crystals => "crystals",
            Self::Plates => "plates",
            Self::StaticPlates => "static_plates",
            Self::Crystallize => "crystallize",
            Self::Pillow => "pillow",
            Self::CrystalsHq => "crystals_hq",
            Self::PlatesHq => "plates_hq",
            Self::StaticPlatesHq => "static_plates_hq",
            Self::CrystallizeHq => "crystallize_hq",
            Self::MixedCrystals => "mixed_crystals",
            Self::Tubular => "tubular",
        }
    }

    /// Resolves an authored key. Unknown keys are rejected, never defaulted.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_key() == key)
    }

    /// Resolves a one-based menu position; the undocumented 13th slot fails.
    #[must_use]
    pub fn from_menu(menu: u32) -> Option<Self> {
        let slot = usize::try_from(menu.checked_sub(1)?).ok()?;
        Self::ALL.get(slot).copied()
    }

    /// Resolves a one-based menu position, reporting unknown slots.
    ///
    /// # Errors
    ///
    /// Returns [`CellPatternError::UnsupportedPattern`] for any slot the
    /// documentation does not define, including the 13th AE slot.
    pub fn from_menu_checked(menu: u32) -> Result<Self, CellPatternError> {
        Self::from_menu(menu).ok_or(CellPatternError::UnsupportedPattern(menu))
    }

    /// Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bubbles => "气泡",
            Self::Crystals => "晶体",
            Self::Plates => "印板",
            Self::StaticPlates => "静态板",
            Self::Crystallize => "晶格化",
            Self::Pillow => "枕状",
            Self::CrystalsHq => "晶体 HQ",
            Self::PlatesHq => "印板 HQ",
            Self::StaticPlatesHq => "静态板 HQ",
            Self::CrystallizeHq => "晶格化 HQ",
            Self::MixedCrystals => "混合晶体",
            Self::Tubular => "管状",
        }
    }

    /// Whether the pattern freezes its evolution phase.
    const fn is_static(self) -> bool {
        matches!(self, Self::StaticPlates | Self::StaticPlatesHq)
    }
}

/// The three documented out-of-range fold modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellPatternOverflow {
    /// Saturate to the closed interval `[0, 1]`.
    Clip,
    /// Fold through a hyperbolic shoulder into the open interval `(0, 1)`.
    SoftClamp,
    /// Triangle-fold back into `[0, 1]`.
    WrapBack,
}

impl CellPatternOverflow {
    /// Every documented mode, in menu order.
    pub const ALL: [Self; 3] = [Self::Clip, Self::SoftClamp, Self::WrapBack];

    /// Zero-based index stored in the uniform header.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::Clip => 0,
            Self::SoftClamp => 1,
            Self::WrapBack => 2,
        }
    }

    /// Stable authored key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::SoftClamp => "soft_clamp",
            Self::WrapBack => "wrap_back",
        }
    }

    /// Resolves an authored key. Unknown keys are rejected, never defaulted.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }

    /// Chinese fallback label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clip => "剪切",
            Self::SoftClamp => "柔和固定",
            Self::WrapBack => "反绕",
        }
    }

    /// Folds one shaped value into range.
    #[must_use]
    pub fn apply(self, value: f32) -> f32 {
        match self {
            Self::Clip => clamp01(value),
            Self::SoftClamp => {
                // Both shoulders fold into the open interval `(0, 1)`: an
                // undershoot approaches 0 from above, an overshoot approaches 1
                // from below, so the value never lands on pure black or white.
                if value < 0.0 {
                    let distance = -value;
                    distance / (1.0 + distance)
                } else if value > 1.0 {
                    let distance = value - 1.0;
                    1.0 - distance / (1.0 + distance)
                } else {
                    value
                }
            }
            Self::WrapBack => {
                let folded = value - 2.0 * (value * 0.5).floor();
                clamp01(if folded <= 1.0 { folded } else { 2.0 - folded })
            }
        }
    }
}

/// A cell pattern parameter rejected by [`CellPatternGrade::new`].
#[derive(Clone, Debug, PartialEq)]
pub enum CellPatternError {
    /// The named parameter is NaN or infinite.
    NonFinite(&'static str),
    /// The named parameter left its documented closed range.
    OutOfRange {
        /// Parameter identifier.
        parameter: &'static str,
        /// Rejected value.
        value: f32,
        /// Inclusive lower bound.
        min: f32,
        /// Inclusive upper bound.
        max: f32,
    },
    /// A menu slot without documented semantics was requested.
    UnsupportedPattern(u32),
}

impl fmt::Display for CellPatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite(parameter) => {
                write!(formatter, "cell pattern `{parameter}` must be finite")
            }
            Self::OutOfRange { parameter, value, min, max } => write!(
                formatter,
                "cell pattern `{parameter}` must be within [{min}, {max}], got {value}"
            ),
            Self::UnsupportedPattern(menu) => {
                write!(formatter, "cell pattern menu slot {menu} is not supported")
            }
        }
    }
}

impl std::error::Error for CellPatternError {}

/// A validated cell pattern generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellPatternGrade {
    pattern: CellPatternType,
    invert: bool,
    contrast: f32,
    overflow: CellPatternOverflow,
    disperse: f32,
    size: f32,
    offset: [f32; 2],
    tiling: bool,
    cells_horizontal: f32,
    cells_vertical: f32,
    evolution: f32,
    cycle_evolution: bool,
    cycle_revolutions: f32,
    seed: f32,
}

impl CellPatternGrade {
    /// Validates every authored parameter.
    ///
    /// # Errors
    ///
    /// Returns [`CellPatternError::NonFinite`] for NaN or infinite values and
    /// [`CellPatternError::OutOfRange`] when a value leaves its documented
    /// closed range.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pattern: CellPatternType,
        invert: bool,
        contrast: f32,
        overflow: CellPatternOverflow,
        disperse: f32,
        size: f32,
        offset: [f32; 2],
        tiling: bool,
        cells_horizontal: f32,
        cells_vertical: f32,
        evolution: f32,
        cycle_evolution: bool,
        cycle_revolutions: f32,
        seed: f32,
    ) -> Result<Self, CellPatternError> {
        bounded("contrast", contrast, 0.0, CELL_PATTERN_MAX_CONTRAST)?;
        bounded("disperse", disperse, 0.0, CELL_PATTERN_MAX_DISPERSE)?;
        bounded("size", size, CELL_PATTERN_MIN_SIZE, CELL_PATTERN_MAX_SIZE)?;
        finite("offset", offset[0])?;
        finite("offset", offset[1])?;
        if offset[0].abs() > CELL_PATTERN_MAX_COORDINATE
            || offset[1].abs() > CELL_PATTERN_MAX_COORDINATE
        {
            return Err(CellPatternError::OutOfRange {
                parameter: "offset",
                value: offset[0].abs().max(offset[1].abs()),
                min: -CELL_PATTERN_MAX_COORDINATE,
                max: CELL_PATTERN_MAX_COORDINATE,
            });
        }
        bounded(
            "cells_horizontal",
            cells_horizontal,
            1.0,
            CELL_PATTERN_MAX_CELLS,
        )?;
        bounded(
            "cells_vertical",
            cells_vertical,
            1.0,
            CELL_PATTERN_MAX_CELLS,
        )?;
        bounded(
            "evolution",
            evolution,
            -CELL_PATTERN_MAX_EVOLUTION,
            CELL_PATTERN_MAX_EVOLUTION,
        )?;
        bounded(
            "cycle_revolutions",
            cycle_revolutions,
            1.0,
            CELL_PATTERN_MAX_CYCLE_REVOLUTIONS,
        )?;
        bounded("seed", seed, 0.0, CELL_PATTERN_MAX_SEED)?;
        Ok(Self {
            pattern,
            invert,
            contrast,
            overflow,
            disperse,
            size,
            offset,
            tiling,
            cells_horizontal,
            cells_vertical,
            evolution,
            cycle_evolution,
            cycle_revolutions,
            seed,
        })
    }

    /// Authored pattern type.
    #[must_use]
    pub const fn pattern(self) -> CellPatternType {
        self.pattern
    }

    /// Whether the final value is complemented.
    #[must_use]
    pub const fn invert(self) -> bool {
        self.invert
    }

    /// Authored contrast on the AE slider scale.
    #[must_use]
    pub const fn contrast(self) -> f32 {
        self.contrast
    }

    /// Authored overflow mode.
    #[must_use]
    pub const fn overflow(self) -> CellPatternOverflow {
        self.overflow
    }

    /// Authored feature-point dispersion.
    #[must_use]
    pub const fn disperse(self) -> f32 {
        self.disperse
    }

    /// Authored cell edge length in pixels.
    #[must_use]
    pub const fn size(self) -> f32 {
        self.size
    }

    /// Authored pattern-space translation in pixels.
    #[must_use]
    pub const fn offset(self) -> [f32; 2] {
        self.offset
    }

    /// Whether the pattern wraps at the tiling period.
    #[must_use]
    pub const fn tiling(self) -> bool {
        self.tiling
    }

    /// Authored tiling column count.
    #[must_use]
    pub const fn cells_horizontal(self) -> f32 {
        self.cells_horizontal
    }

    /// Authored tiling row count.
    #[must_use]
    pub const fn cells_vertical(self) -> f32 {
        self.cells_vertical
    }

    /// Authored evolution phase.
    #[must_use]
    pub const fn evolution(self) -> f32 {
        self.evolution
    }

    /// Whether the evolution phase wraps at `cycle_revolutions`.
    #[must_use]
    pub const fn cycle_evolution(self) -> bool {
        self.cycle_evolution
    }

    /// Authored evolution cycle length.
    #[must_use]
    pub const fn cycle_revolutions(self) -> f32 {
        self.cycle_revolutions
    }

    /// Authored random seed.
    #[must_use]
    pub const fn seed(self) -> f32 {
        self.seed
    }

    /// Three uniform cells plus the reserved fourth, in packing order:
    /// `params = [size, offset.x, offset.y, disperse]`,
    /// `color = [cells_h, cells_v, evolution, contrast]`,
    /// `extra0 = [seed, cycle_revolutions, 0, 0]`, `extra1 = [0; 4]`.
    #[must_use]
    pub fn gpu_controls(self) -> CellPatternControls {
        (
            [self.size, self.offset[0], self.offset[1], self.disperse],
            [
                self.cells_horizontal,
                self.cells_vertical,
                self.evolution,
                self.contrast,
            ],
            [self.seed, self.cycle_revolutions, 0.0, 0.0],
            [0.0; 4],
        )
    }

    /// Uniform-header flag bits: bit 0 invert, bit 1 tiling, bit 2 cycle.
    #[must_use]
    pub const fn header_flags(self) -> u32 {
        let mut flags = 0;
        if self.invert {
            flags |= 1 << 0;
        }
        if self.tiling {
            flags |= 1 << 1;
        }
        if self.cycle_evolution {
            flags |= 1 << 2;
        }
        flags
    }

    /// Cell pattern is always a visible generator, never the identity.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        false
    }

    /// Generated value at one pixel column and row, in `[0, 1]`.
    #[must_use]
    pub fn value(self, column: u32, row: u32) -> f32 {
        self.value_at([pixel_centre(column), pixel_centre(row)])
    }

    /// Generated straight-alpha pixel at one pixel column and row.
    #[must_use]
    pub fn pixel(self, column: u32, row: u32) -> [f32; 4] {
        let value = self.value(column, row);
        [value, value, value, 1.0]
    }

    /// Generator application: the source pixel is ignored, `alpha` is one.
    #[must_use]
    pub fn apply_pixel(self, _pixel: [f32; 4], column: u32, row: u32) -> [f32; 4] {
        self.pixel(column, row)
    }

    /// Writes the generator over a whole frame.
    ///
    /// Returns `false` when `width * height` overflows or the buffer is short.
    #[must_use]
    pub fn apply_to_frame(self, pixels: &mut [[f32; 4]], width: usize, height: usize) -> bool {
        cell_pattern_frame_f32(pixels, width, height, &self)
    }

    /// Evaluates one pixel centre.
    fn value_at(self, centre: [f32; 2]) -> f32 {
        let p = [
            (centre[0] - self.offset[0]) / self.size,
            (centre[1] - self.offset[1]) / self.size,
        ];
        let gain = self.contrast * 0.01;
        let width = 8.0 / self.contrast.max(1.0);
        let ramp = width.min(0.5);
        let (step, weight) = self.field_phase();
        let second = [2.0 * p[0] + 0.5, 2.0 * p[1] + 0.5];
        let shaped = match self.pattern {
            CellPatternType::Bubbles => {
                self.overflow
                    .apply(self.bubble_raw(p, 1.0, Metric::L2, step, weight, 4.0, gain))
            }
            CellPatternType::Crystals => {
                self.overflow
                    .apply(self.bubble_raw(p, 1.0, Metric::L1, step, weight, 6.0, gain))
            }
            CellPatternType::Plates => clamp01(self.plates_raw(p, 1.0, step, weight, ramp, false)),
            CellPatternType::StaticPlates => {
                clamp01(self.plates_raw(p, 1.0, step, weight, ramp, true))
            }
            CellPatternType::Crystallize => {
                clamp01(self.crystallize_raw(p, 1.0, step, weight, width, false))
            }
            CellPatternType::Pillow => {
                let field = self.field(p, 1.0, Metric::L2, step, weight);
                self.overflow.apply(0.5 + (0.62 - field.f1 * field.f1) * 2.5 * gain)
            }
            CellPatternType::CrystalsHq => {
                let first = self.overflow.apply(self.bubble_raw(
                    p,
                    1.0,
                    Metric::L1,
                    step,
                    weight,
                    6.0,
                    gain,
                ));
                let next = self.overflow.apply(self.bubble_raw(
                    second,
                    2.0,
                    Metric::L1,
                    step,
                    weight,
                    6.0,
                    gain,
                ));
                0.65 * first + 0.35 * next
            }
            CellPatternType::PlatesHq => {
                let first = self.plates_raw(p, 1.0, step, weight, ramp, false);
                let next = self.plates_raw(second, 2.0, step, weight, ramp, false);
                0.65 * first + 0.35 * next
            }
            CellPatternType::StaticPlatesHq => {
                let first = self.plates_raw(p, 1.0, step, weight, ramp, true);
                let next = self.plates_raw(second, 2.0, step, weight, ramp, true);
                0.65 * first + 0.35 * next
            }
            CellPatternType::CrystallizeHq => {
                let first = self.crystallize_raw(p, 1.0, step, weight, width, false);
                let next = self.crystallize_raw(second, 2.0, step, weight, width, false);
                0.65 * first + 0.35 * next
            }
            CellPatternType::MixedCrystals => {
                self.overflow.apply(self.mixed_crystals_raw(p, step, weight, ramp, gain))
            }
            CellPatternType::Tubular => {
                let field = self.field(p, 1.0, Metric::L2, step, weight);
                let gap = field.f2 - field.f1;
                self.overflow.apply(0.5 + (0.40 - (gap - 0.18).abs() * 5.0) * gain)
            }
        };
        let value = if self.invert { 1.0 - shaped } else { shaped };
        clamp01(value)
    }

    /// Evolution phase index and smoothstep weight for this grade.
    fn field_phase(self) -> (u32, f32) {
        if self.pattern.is_static() {
            return (EVOLUTION_BASE_STEP, 0.0);
        }
        self.evolution_phase()
    }

    /// Integer phase index plus interpolation weight, from the authored phase.
    fn evolution_phase(self) -> (u32, f32) {
        let phase = if self.cycle_evolution {
            wrap_period(self.evolution, self.cycle_revolutions)
        } else {
            self.evolution
        };
        let bounded = phase.clamp(-CELL_PATTERN_MAX_EVOLUTION, CELL_PATTERN_MAX_EVOLUTION);
        // Split the integer phase off *before* rebasing onto the non-negative
        // step range: `bounded + 2^20` alone would round every fractional phase
        // below one to the same f32 and freeze the interpolation staircase.
        let index = bounded.floor();
        let fraction = bounded - index;
        let weight = fraction * fraction * (3.0 - 2.0 * fraction);
        let shifted = index + CELL_PATTERN_MAX_EVOLUTION;
        (shifted as u32, weight)
    }

    /// Normalised seed salt, mixed into the hash salts only.
    fn seed_mix(self) -> u32 {
        let bits = self.seed.clamp(0.0, CELL_PATTERN_MAX_SEED).floor() as u32;
        bits.wrapping_mul(CELL_PATTERN_SALT_STRIDE)
    }

    /// Cell-site jitter for one site, interpolated across the evolution phase.
    fn jitter(self, cell_x: i32, cell_y: i32, step: u32, weight: f32) -> [f32; 2] {
        let salt = CELL_PATTERN_SALT_JITTER ^ self.seed_mix();
        let first = jitter_at(cell_x, cell_y, salt, step);
        if weight <= 0.0 {
            return first;
        }
        let next = jitter_at(cell_x, cell_y, salt, step.wrapping_add(1));
        [
            first[0] + (next[0] - first[0]) * weight,
            first[1] + (next[1] - first[1]) * weight,
        ]
    }

    /// One octave of the Worley field.
    fn field(
        self,
        q: [f32; 2],
        frequency: f32,
        metric: Metric,
        step: u32,
        weight: f32,
    ) -> CellPatternField {
        let base_x = clamp_to_i32(q[0].floor());
        let base_y = clamp_to_i32(q[1].floor());
        // Distances are measured inside the base cell so that a site one period
        // away produces bit-identical values: `q - base` is invariant under a
        // shift of both terms, while the absolute site position is not.
        let local_x = q[0] - base_x as f32;
        let local_y = q[1] - base_y as f32;
        let period_x = self.tiling_period(self.cells_horizontal, frequency);
        let period_y = self.tiling_period(self.cells_vertical, frequency);
        let mut f1 = 1.0e9;
        let mut f2 = 1.0e9;
        let mut winner_x = base_x;
        let mut winner_y = base_y;
        for offset_y in -1..=1 {
            for offset_x in -1..=1 {
                let cell_x = base_x + offset_x;
                let cell_y = base_y + offset_y;
                let (hash_x, hash_y) = if self.tiling {
                    (wrap_index(cell_x, period_x), wrap_index(cell_y, period_y))
                } else {
                    (cell_x, cell_y)
                };
                let jitter = self.jitter(hash_x, hash_y, step, weight);
                let site_x = offset_x as f32 + 0.5 + self.disperse * jitter[0];
                let site_y = offset_y as f32 + 0.5 + self.disperse * jitter[1];
                let distance = metric.distance(local_x - site_x, local_y - site_y);
                if distance < f1 {
                    f2 = f1;
                    f1 = distance;
                    winner_x = hash_x;
                    winner_y = hash_y;
                } else if distance < f2 {
                    f2 = distance;
                }
            }
        }
        CellPatternField { f1, f2, winner_x, winner_y }
    }

    /// Integer wrap period for one axis, or `0` when tiling is off.
    fn tiling_period(self, cells: f32, frequency: f32) -> i32 {
        if !self.tiling {
            return 0;
        }
        // The modulo is defined on cell indices, so a fractional authored cell
        // count is rounded to the nearest whole cell.
        (cells * frequency).round().max(1.0) as i32
    }

    /// Per-cell luminance for one evolution phase.
    fn luminance(self, cell_x: i32, cell_y: i32, step: u32) -> f32 {
        let salt =
            CELL_PATTERN_SALT_ID ^ self.seed_mix() ^ step.wrapping_mul(CELL_PATTERN_SALT_STRIDE);
        unit16(cell_hash(cell_x, cell_y, salt))
    }

    /// Per-cell luminance frozen outside the evolution phase.
    fn static_luminance(self, cell_x: i32, cell_y: i32) -> f32 {
        unit16(cell_hash(
            cell_x,
            cell_y,
            CELL_PATTERN_SALT_ID ^ self.seed_mix(),
        ))
    }

    /// Bubble and crystal formula, before overflow.
    fn bubble_raw(
        self,
        q: [f32; 2],
        frequency: f32,
        metric: Metric,
        step: u32,
        weight: f32,
        gain: f32,
        contrast_gain: f32,
    ) -> f32 {
        let field = self.field(q, frequency, metric, step, weight);
        let luminance = self.luminance(field.winner_x, field.winner_y, step);
        0.5 + (0.55 + 0.30 * luminance - field.f1) * gain * contrast_gain
    }

    /// Plate formula, before clamping.
    fn plates_raw(
        self,
        q: [f32; 2],
        frequency: f32,
        step: u32,
        weight: f32,
        ramp: f32,
        frozen: bool,
    ) -> f32 {
        let field = self.field(q, frequency, Metric::L2, step, weight);
        let luminance = if frozen {
            self.static_luminance(field.winner_x, field.winner_y)
        } else {
            self.luminance(field.winner_x, field.winner_y, step)
        };
        luminance * clamp01((field.f2 - field.f1) / ramp)
    }

    /// Hard-edged crystallize formula, before clamping.
    fn crystallize_raw(
        self,
        q: [f32; 2],
        frequency: f32,
        step: u32,
        weight: f32,
        width: f32,
        frozen: bool,
    ) -> f32 {
        let field = self.field(q, frequency, Metric::L2, step, weight);
        let luminance = if frozen {
            self.static_luminance(field.winner_x, field.winner_y)
        } else {
            self.luminance(field.winner_x, field.winner_y, step)
        };
        if field.f2 - field.f1 > width {
            luminance
        } else {
            0.0
        }
    }

    /// Per-cell choice between plates and crystals, before overflow.
    fn mixed_crystals_raw(
        self,
        q: [f32; 2],
        step: u32,
        weight: f32,
        ramp: f32,
        contrast_gain: f32,
    ) -> f32 {
        let plates = self.field(q, 1.0, Metric::L2, step, weight);
        let crystals = self.field(q, 1.0, Metric::L1, step, weight);
        let pick = (cell_hash(
            plates.winner_x,
            plates.winner_y,
            CELL_PATTERN_SALT_MIXED ^ self.seed_mix(),
        ) >> 16)
            & 1;
        if pick == 1 {
            let luminance = self.luminance(crystals.winner_x, crystals.winner_y, step);
            0.5 + (0.55 + 0.30 * luminance - crystals.f1) * 6.0 * contrast_gain
        } else {
            let luminance = self.luminance(plates.winner_x, plates.winner_y, step);
            luminance * clamp01((plates.f2 - plates.f1) / ramp)
        }
    }
}

/// Four uniform cells in packing order.
pub type CellPatternControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// Nearest and second-nearest cell-site distances plus the winning site.
#[derive(Clone, Copy)]
struct CellPatternField {
    f1: f32,
    f2: f32,
    winner_x: i32,
    winner_y: i32,
}

/// Distance metric selected by the pattern type.
#[derive(Clone, Copy)]
enum Metric {
    L1,
    L2,
}

impl Metric {
    /// Distance between a sample and one jittered cell site.
    fn distance(self, dx: f32, dy: f32) -> f32 {
        match self {
            Self::L2 => (dx * dx + dy * dy).sqrt(),
            Self::L1 => dx.abs() + dy.abs(),
        }
    }
}

/// Phase index used by the frozen static patterns.
const EVOLUTION_BASE_STEP: u32 = CELL_PATTERN_MAX_EVOLUTION as u32;
/// Checkpoint stride in pixels, shared with the other row-band kernels.
const CHECKPOINT_STRIDE: usize = 4_096;
/// Largest pattern coordinate whose floor still converts to `i32` exactly.
const MAX_CELL_COORDINATE: f32 = 8_388_607.0;

/// The shared hash, restricted to `i32` cell indices.
pub(crate) fn cell_hash(x: i32, y: i32, salt: u32) -> u32 {
    crate::noise::noise_hash(x as u32, y as u32, salt)
}

/// One jitter component pair from one hash: low half x, high half y.
fn jitter_at(cell_x: i32, cell_y: i32, salt: u32, step: u32) -> [f32; 2] {
    let hash = cell_hash(
        cell_x,
        cell_y,
        salt ^ step.wrapping_mul(CELL_PATTERN_SALT_STRIDE),
    );
    [unit16(hash) * 2.0 - 1.0, unit16(hash >> 16) * 2.0 - 1.0]
}

/// Normalised low 16 bits of a hash.
fn unit16(hash: u32) -> f32 {
    (hash & 0xFFFF) as f32 / 65535.0
}

/// Saturates to the closed unit interval.
fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// Pixel centre of a non-negative pixel index.
fn pixel_centre(index: u32) -> f32 {
    index as f32 + 0.5
}

/// Floors a pattern coordinate into the exactly representable `i32` range.
fn clamp_to_i32(coordinate: f32) -> i32 {
    coordinate.clamp(-MAX_CELL_COORDINATE - 1.0, MAX_CELL_COORDINATE) as i32
}

/// Positive integer modulo.
fn wrap_index(index: i32, period: i32) -> i32 {
    if period <= 0 {
        return index;
    }
    let wrapped = index % period;
    if wrapped < 0 {
        wrapped + period
    } else {
        wrapped
    }
}

/// Wraps a phase into `[0, period)`.
fn wrap_period(phase: f32, period: f32) -> f32 {
    phase - period * (phase / period).floor()
}

/// Applies the generator to a whole frame.
///
/// Returns `false` when `width * height` overflows or the buffer is too short.
#[must_use]
pub fn cell_pattern_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CellPatternGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_cell_pattern_f32_region_controlled(pixels, 0, 0, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the generator to a region whose origin is given in frame-absolute
/// pixel coordinates.
///
/// # Errors
///
/// Propagates the first error returned by `checkpoint`, leaving already written
/// chunks in place.
pub(crate) fn apply_cell_pattern_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region_x: usize,
    region_y: usize,
    width: usize,
    height: usize,
    grade: &CellPatternGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || pixels.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let rows_per_chunk = (CHECKPOINT_STRIDE / width).max(1);
    let mut start = 0usize;
    while start < height {
        checkpoint()?;
        let rows = rows_per_chunk.min(height - start);
        let end = start + rows;
        crate::parallel::for_each_row_band(
            &mut pixels[start * width..end * width],
            width,
            0..rows,
            |first_row, band| {
                for (offset, chunk) in band.chunks_mut(width).enumerate() {
                    let row = (region_y + start + first_row + offset) as u32;
                    for (column, pixel) in chunk.iter_mut().enumerate() {
                        let column = (region_x + column) as u32;
                        *pixel = grade.pixel(column, row);
                    }
                }
            },
        );
        start = end;
    }
    checkpoint()?;
    Ok(true)
}

/// Rejects non-finite parameters.
fn finite(parameter: &'static str, value: f32) -> Result<(), CellPatternError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(CellPatternError::NonFinite(parameter))
    }
}

/// Rejects parameters outside `[min, max]`.
fn bounded(
    parameter: &'static str,
    value: f32,
    min: f32,
    max: f32,
) -> Result<(), CellPatternError> {
    finite(parameter, value)?;
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(CellPatternError::OutOfRange { parameter, value, min, max })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.0, 0.0, 0.0, 0.0]; width * height]
    }

    fn grade(pattern: CellPatternType) -> CellPatternGrade {
        CellPatternGrade::new(
            pattern,
            false,
            100.0,
            CellPatternOverflow::Clip,
            1.0,
            CELL_PATTERN_DEFAULT_SIZE,
            CELL_PATTERN_DEFAULT_OFFSET,
            false,
            4.0,
            4.0,
            0.0,
            false,
            1.0,
            0.0,
        )
        .expect("default cell pattern grade is valid")
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() <= 1.0e-6
    }

    #[test]
    fn cell_hash_matches_noise_hash() {
        for salt in [0u32, 0x1234_5678, u32::MAX] {
            for y in -7..=7 {
                for x in -7..=7 {
                    assert_eq!(
                        cell_hash(x, y, salt),
                        crate::noise::noise_hash(x as u32, y as u32, salt)
                    );
                }
            }
        }
    }

    #[test]
    fn hash_halves_are_uniform() {
        let mut buckets = [0usize; 16];
        let mut low_sum = 0.0f64;
        let mut high_sum = 0.0f64;
        let mut low_squares = 0.0f64;
        let mut high_squares = 0.0f64;
        let mut count = 0usize;
        for y in 0..256 {
            for x in 0..256 {
                let hash = cell_hash(x, y, CELL_PATTERN_SALT_JITTER);
                let low = f64::from(unit16(hash));
                let high = f64::from(unit16(hash >> 16));
                low_sum += low;
                high_sum += high;
                low_squares += low * low;
                high_squares += high * high;
                let bucket = ((low * 16.0) as usize).min(15);
                buckets[bucket] += 1;
                count += 1;
            }
        }
        let mean_low = low_sum / count as f64;
        let mean_high = high_sum / count as f64;
        assert!((mean_low - 0.5).abs() < 0.01, "low mean {mean_low}");
        assert!((mean_high - 0.5).abs() < 0.01, "high mean {mean_high}");
        let variance_low = low_squares / count as f64 - mean_low * mean_low;
        let variance_high = high_squares / count as f64 - mean_high * mean_high;
        assert!(
            (variance_low - 1.0 / 12.0).abs() < 0.01,
            "low variance {variance_low}"
        );
        assert!(
            (variance_high - 1.0 / 12.0).abs() < 0.01,
            "high variance {variance_high}"
        );
        let expected = count as f64 / 16.0;
        let chi_square: f64 = buckets
            .iter()
            .map(|bucket| {
                let difference = *bucket as f64 - expected;
                difference * difference / expected
            })
            .sum();
        assert!(chi_square < 45.0, "chi square {chi_square}");
    }

    #[test]
    fn every_pattern_is_non_degenerate() {
        for pattern in CellPatternType::ALL {
            let grade = CellPatternGrade { size: 8.0, ..grade(pattern) };
            let mut values = Vec::with_capacity(64 * 64);
            for row in 0..64 {
                for column in 0..64 {
                    values.push(grade.value(column, row));
                }
            }
            let min = values.iter().copied().fold(f32::INFINITY, f32::min);
            let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let mean = values.iter().sum::<f32>() / values.len() as f32;
            let variance = values.iter().map(|value| (value - mean) * (value - mean)).sum::<f32>()
                / values.len() as f32;
            let deviation = variance.sqrt();
            assert!(min < 0.2, "{pattern:?} min {min}");
            assert!(max > 0.8, "{pattern:?} max {max}");
            assert!(deviation > 0.05, "{pattern:?} deviation {deviation}");
            assert!(
                values.iter().all(|value| (0.0..=1.0).contains(value)),
                "{pattern:?} left the unit interval"
            );
        }
        // Crystallize is hard-edged: every sample is either a cell luminance or
        // an exact zero, so the distinct value count stays near the cell count.
        let grade = CellPatternGrade { size: 8.0, ..grade(CellPatternType::Crystallize) };
        let mut distinct = Vec::new();
        for row in 0..64 {
            for column in 0..64 {
                let value = grade.value(column, row);
                if !distinct.contains(&value) {
                    distinct.push(value);
                }
            }
        }
        assert!(distinct.len() <= 128, "distinct {}", distinct.len());
        assert!(distinct.contains(&0.0));
    }

    #[test]
    fn static_plates_ignores_evolution() {
        let static_zero = CellPatternGrade {
            evolution: 0.0,
            ..grade(CellPatternType::StaticPlates)
        };
        let static_moved = CellPatternGrade {
            evolution: 0.37,
            ..grade(CellPatternType::StaticPlates)
        };
        let plates_moved = CellPatternGrade { evolution: 0.37, ..grade(CellPatternType::Plates) };
        let plates_zero = grade(CellPatternType::Plates);
        let mut differs = false;
        for row in 0..32 {
            for column in 0..32 {
                assert_eq!(
                    static_zero.pixel(column, row),
                    static_moved.pixel(column, row)
                );
                if plates_zero.pixel(column, row) != plates_moved.pixel(column, row) {
                    differs = true;
                }
            }
        }
        assert!(differs, "plates must follow the evolution phase");
    }

    #[test]
    fn tiling_is_exactly_periodic() {
        let tiled = CellPatternGrade {
            tiling: true,
            cells_horizontal: 4.0,
            cells_vertical: 4.0,
            size: 16.0,
            ..grade(CellPatternType::Bubbles)
        };
        let untiled = CellPatternGrade { tiling: false, ..tiled };
        let mut untiled_differs = false;
        for row in 0..64 {
            for column in 0..64 {
                let base = tiled.value(column, row);
                assert_eq!(base.to_bits(), tiled.value(column + 64, row).to_bits());
                assert_eq!(base.to_bits(), tiled.value(column, row + 64).to_bits());
                if untiled.value(column, row).to_bits() != untiled.value(column + 64, row).to_bits()
                {
                    untiled_differs = true;
                }
            }
        }
        assert!(untiled_differs, "tiling off must not be periodic");
    }

    #[test]
    fn invert_is_the_exact_complement() {
        let plain = grade(CellPatternType::Bubbles);
        let inverted = CellPatternGrade { invert: true, ..plain };
        for row in 0..32 {
            for column in 0..32 {
                assert_eq!(
                    inverted.value(column, row).to_bits(),
                    (1.0 - plain.value(column, row)).to_bits()
                );
            }
        }
    }

    #[test]
    fn size_and_offset_move_the_pattern() {
        let small = grade(CellPatternType::Bubbles);
        let large = CellPatternGrade { size: 2.0 * CELL_PATTERN_DEFAULT_SIZE, ..small };
        let crossings = |grade: CellPatternGrade| {
            let mut previous = grade.value(0, 32) - 0.5;
            let mut count = 0usize;
            for column in 1..256 {
                let current = grade.value(column, 32) - 0.5;
                if (previous < 0.0) != (current < 0.0) {
                    count += 1;
                }
                previous = current;
            }
            count
        };
        assert!(
            crossings(large) < crossings(small),
            "large {} small {}",
            crossings(large),
            crossings(small)
        );
        let shifted = CellPatternGrade { offset: [0.0, 0.0], ..small };
        for row in 0..64 {
            for column in 0..64 {
                assert_eq!(
                    small.value(column + 128, row + 128).to_bits(),
                    shifted.value(column, row).to_bits()
                );
            }
        }
    }

    #[test]
    fn seed_changes_pattern_not_layout() {
        let first = CellPatternGrade {
            disperse: 0.0,
            seed: 11.0,
            ..grade(CellPatternType::Crystallize)
        };
        let second = CellPatternGrade { seed: 4242.0, ..first };
        let plates_first = CellPatternGrade { pattern: CellPatternType::Plates, ..first };
        let plates_second = CellPatternGrade { pattern: CellPatternType::Plates, ..second };
        let mut brightness_differs = false;
        for row in 0..32 {
            for column in 0..32 {
                assert_eq!(
                    first.value(column, row) > 0.0,
                    second.value(column, row) > 0.0
                );
                if plates_first.pixel(column, row) != plates_second.pixel(column, row) {
                    brightness_differs = true;
                }
            }
        }
        assert!(brightness_differs, "seed must change cell luminance");
        let scattered_first = grade(CellPatternType::Bubbles);
        let scattered_second = CellPatternGrade { seed: 4242.0, ..scattered_first };
        let mut patterns_differ = false;
        for row in 0..32 {
            for column in 0..32 {
                if scattered_first.pixel(column, row) != scattered_second.pixel(column, row) {
                    patterns_differ = true;
                }
            }
        }
        assert!(patterns_differ, "seed must change the scattered pattern");
    }

    #[test]
    fn cycle_evolution_repeats_exactly() {
        for period in [1.0f32, 2.0, 3.0] {
            let grade = CellPatternGrade {
                cycle_evolution: true,
                cycle_revolutions: period,
                evolution: 0.25,
                ..grade(CellPatternType::Bubbles)
            };
            let shifted = CellPatternGrade { evolution: 0.25 + period, ..grade };
            for row in 0..16 {
                for column in 0..16 {
                    assert_eq!(
                        grade.value(column, row).to_bits(),
                        shifted.value(column, row).to_bits(),
                        "period {period}"
                    );
                }
            }
        }
        // The default contrast saturates most of the tile to pure black or pure
        // white, so compare a low-contrast grade where the pattern stays graded.
        let free = CellPatternGrade {
            contrast: 20.0,
            size: 12.0,
            ..grade(CellPatternType::Bubbles)
        };
        let later = CellPatternGrade { evolution: 10.25, ..free };
        let mut scattered = false;
        for row in 0..32 {
            for column in 0..32 {
                if free.pixel(column, row) != later.pixel(column, row) {
                    scattered = true;
                }
            }
        }
        assert!(scattered, "evolution must redraw the free-running pattern");
    }

    #[test]
    fn evolution_is_continuous() {
        // `Pillow` has no per-cell luminance term, so the field interpolation is
        // the only thing that moves; patterns that carry a random luminance step
        // at every cell border (bubbles, crystals, ...) are discontinuous there
        // by design, not flicker.
        let base = CellPatternGrade { evolution: 0.0, ..grade(CellPatternType::Pillow) };
        let mut previous = base.value(17, 23);
        let mut largest = 0.0f32;
        for step in 1..1000 {
            let grade = CellPatternGrade { evolution: step as f32 * 0.001, ..base };
            let current = grade.value(17, 23);
            largest = largest.max((current - previous).abs());
            previous = current;
        }
        assert!(largest < 0.02, "largest jump {largest}");
    }

    #[test]
    fn overflow_modes_match_the_reference_examples() {
        let wrapped = CellPatternOverflow::WrapBack.apply(258.0 / 255.0);
        assert!(close(wrapped, 252.0 / 255.0), "wrapped {wrapped}");
        let folded = CellPatternOverflow::WrapBack.apply(-3.0 / 255.0);
        assert!(close(folded, 3.0 / 255.0), "folded {folded}");
        assert!(close(CellPatternOverflow::Clip.apply(-2.0), 0.0));
        assert!(close(CellPatternOverflow::Clip.apply(3.0), 1.0));
        let low = CellPatternOverflow::SoftClamp.apply(-0.5);
        let high = CellPatternOverflow::SoftClamp.apply(1.5);
        assert!(low > 0.0 && low < 1.0, "soft low {low}");
        assert!(high > 0.0 && high < 1.0, "soft high {high}");
        assert!(close(CellPatternOverflow::SoftClamp.apply(0.5), 0.5));
    }

    #[test]
    fn overflow_is_inert_for_sharpness_patterns() {
        let patterns = [
            CellPatternType::Plates,
            CellPatternType::StaticPlates,
            CellPatternType::Crystallize,
            CellPatternType::PlatesHq,
            CellPatternType::StaticPlatesHq,
            CellPatternType::CrystallizeHq,
        ];
        for pattern in patterns {
            let clipped = CellPatternGrade {
                overflow: CellPatternOverflow::Clip,
                ..grade(pattern)
            };
            let soft = CellPatternGrade {
                overflow: CellPatternOverflow::SoftClamp,
                ..clipped
            };
            let wrapped = CellPatternGrade { overflow: CellPatternOverflow::WrapBack, ..clipped };
            for row in 0..16 {
                for column in 0..16 {
                    let expected = clipped.pixel(column, row);
                    assert_eq!(soft.pixel(column, row), expected, "{pattern:?}");
                    assert_eq!(wrapped.pixel(column, row), expected, "{pattern:?}");
                }
            }
        }
    }

    #[test]
    fn invalid_parameters_fail_closed() {
        assert_eq!(CellPatternType::from_menu(13), None);
        assert_eq!(CellPatternType::from_menu(0), None);
        assert_eq!(
            CellPatternType::from_menu_checked(13),
            Err(CellPatternError::UnsupportedPattern(13))
        );
        assert_eq!(
            CellPatternType::from_menu(12),
            Some(CellPatternType::Tubular)
        );
        assert_eq!(
            CellPatternType::from_key("static_plates_hq"),
            Some(CellPatternType::StaticPlatesHq)
        );
        assert_eq!(CellPatternType::from_key("plates_ultra"), None);
        assert_eq!(
            CellPatternOverflow::from_key("clip"),
            Some(CellPatternOverflow::Clip)
        );
        assert_eq!(CellPatternOverflow::from_key("bounce"), None);
        let defaults = grade(CellPatternType::Bubbles);
        assert!(matches!(
            CellPatternGrade::new(
                CellPatternType::Bubbles,
                false,
                f32::NAN,
                CellPatternOverflow::Clip,
                1.0,
                60.0,
                [0.0, 0.0],
                false,
                4.0,
                4.0,
                0.0,
                false,
                1.0,
                0.0,
            ),
            Err(CellPatternError::NonFinite("contrast"))
        ));
        assert!(matches!(
            CellPatternGrade::new(
                CellPatternType::Bubbles,
                false,
                100.0,
                CellPatternOverflow::Clip,
                1.0,
                1.9,
                [0.0, 0.0],
                false,
                4.0,
                4.0,
                0.0,
                false,
                1.0,
                0.0,
            ),
            Err(CellPatternError::OutOfRange { parameter: "size", .. })
        ));
        assert!(matches!(
            CellPatternGrade::new(
                CellPatternType::Bubbles,
                false,
                100.0,
                CellPatternOverflow::Clip,
                1.0,
                60.0,
                [f32::INFINITY, 0.0],
                false,
                4.0,
                4.0,
                0.0,
                false,
                1.0,
                0.0,
            ),
            Err(CellPatternError::NonFinite("offset"))
        ));
        assert!(!defaults.is_identity());
    }

    #[test]
    fn region_kernel_aborts_between_chunks() {
        let grade = grade(CellPatternType::Bubbles);
        let untouched = [0.25, 0.5, 0.75, 1.0];
        let mut pixels = vec![untouched; 4096 * 2];
        let mut calls = 0usize;
        let mut checkpoint = || {
            calls += 1;
            if calls == 2 {
                Err("checkpoint aborted")
            } else {
                Ok(())
            }
        };
        let result = apply_cell_pattern_f32_region_controlled(
            &mut pixels,
            0,
            0,
            4096,
            2,
            &grade,
            &mut checkpoint,
        );
        assert_eq!(result, Err("checkpoint aborted"));
        assert!(pixels[..4096].iter().any(|pixel| *pixel != untouched));
        assert!(pixels[4096..].iter().all(|pixel| *pixel == untouched));
    }

    #[test]
    fn frame_kernel_writes_every_pixel() {
        let grade = grade(CellPatternType::Crystals);
        let mut pixels = frame(8, 6);
        assert!(cell_pattern_frame_f32(&mut pixels, 8, 6, &grade));
        assert!(pixels.iter().all(|pixel| pixel[3] == 1.0));
        assert!(grade.apply_to_frame(&mut pixels, 8, 6));
        assert_eq!(grade.apply_pixel([0.0; 4], 3, 2), grade.pixel(3, 2));
        let mut short = frame(4, 6);
        assert!(!cell_pattern_frame_f32(&mut short, 8, 6, &grade));
    }
}
