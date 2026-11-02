//! Dent distortion (DST-16 in the effect list).
//!
//! The effect pulls pixels towards a circular control point, or pushes them
//! away when the knob is set to a bump, so the region reads as a small pit (or
//! a pimple) in the picture. The width of the pit is authored as a fraction of
//! the frame's short side and the strength of the pull as a dimensionless
//! multiplier of the local radius, exactly like the reference effect list.
//!
//! The mapping is a pure inverse resampling — destination pixel in, source
//! pixel out — so the kernel holds no state and its result is independent of
//! the processing order. For a pixel at distance `d` from the centre, a disc of
//! radius `R` and the smooth falloff
//!
//! ```text
//! falloff = (1 - (d / R)^2)^2
//! scale   = 1 + direction * strength * falloff
//! sample  = centre + (pixel - centre) * scale
//! ```
//!
//! the scale is `1` at the centre of the disc and `1` again at the rim, so the
//! pit joins the untouched image with a continuous tangent (`f'(1) = 0`); the
//! falloff is monotone in between, which keeps the mapping free of folds for
//! `strength <= 1`. A dent reads as `scale >= 1` (content is compressed into
//! the centre) and a bump as `scale <= 1` (content is magnified). The hard edge
//! mode replaces the falloff with a constant, which turns the rim into a step
//! of at most `strength * R` pixels.
//!
//! All geometry uses `f32` and the literal expression order of the WGSL pass,
//! so the CPU kernel and [`crate::EffectGpuSpatialPass::Dent`] agree per pixel;
//! the shared bilinear helper is the only seam that widens to `f64`.
//!
//! The kernel works on premultiplied colour in the working scene-linear space
//! and never clamps colour; coverage is capped at one by the shared
//! unpremultiply helper.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Smallest authored dent size, as a fraction of the frame's short side.
pub const DENT_MIN_SIZE_FRACTION: f32 = 0.0;

/// Largest authored dent size, as a fraction of the frame's short side.
pub const DENT_MAX_SIZE_FRACTION: f32 = 1.0;

/// Smallest authored strength multiplier.
pub const DENT_MIN_STRENGTH: f32 = 0.0;

/// Largest authored strength multiplier.
pub const DENT_MAX_STRENGTH: f32 = 1.0;

/// Default dent size, as a fraction of the frame's short side.
pub const DENT_DEFAULT_SIZE_FRACTION: f32 = 0.4;

/// Default strength multiplier.
pub const DENT_DEFAULT_STRENGTH: f32 = 0.5;

/// Default control point, in frame pixels.
pub const DENT_DEFAULT_CENTRE_PIXELS: f32 = 128.0;

/// Strength below which the effect provably cannot move a pixel.
const DENT_IDENTITY_EPSILON: f32 = 1.0e-6;

/// Distance below which the sample is treated as the exact centre.
const DENT_CENTRE_EPSILON_PIXELS: f32 = 1.0e-6;

/// Which way the disc displaces content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DentKind {
    /// Pull content towards the centre: a pit.
    Dent,
    /// Push content away from the centre: a bump.
    Bulge,
}

impl DentKind {
    /// Every kind in authoring menu order.
    pub const ALL: [Self; 2] = [Self::Dent, Self::Bulge];

    /// Stable persistence key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Dent => "dent",
            Self::Bulge => "bulge",
        }
    }

    /// Parses a persistence key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "dent" => Some(Self::Dent),
            "bulge" => Some(Self::Bulge),
            _ => None,
        }
    }

    /// Zero-based value used by uniforms and hashes.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Dent => 0,
            Self::Bulge => 1,
        }
    }

    /// One-based authoring menu value.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        self.as_u32() as i64 + 1
    }

    /// Parses a one-based authoring menu value.
    #[must_use]
    pub fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Dent),
            2 => Some(Self::Bulge),
            _ => None,
        }
    }

    /// Menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dent => "凹陷",
            Self::Bulge => "凸起",
        }
    }
}

/// How the displacement fades out towards the rim of the disc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DentEdge {
    /// Smooth `(1 - u^2)^2` falloff that meets the rim with a zero tangent.
    Smooth,
    /// Constant gain, which leaves a visible step at the rim.
    Hard,
}

impl DentEdge {
    /// Every edge mode in authoring menu order.
    pub const ALL: [Self; 2] = [Self::Smooth, Self::Hard];

    /// Stable persistence key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Smooth => "smooth",
            Self::Hard => "hard",
        }
    }

    /// Parses a persistence key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "smooth" => Some(Self::Smooth),
            "hard" => Some(Self::Hard),
            _ => None,
        }
    }

    /// Zero-based value used by uniforms and hashes.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Smooth => 0,
            Self::Hard => 1,
        }
    }

    /// One-based authoring menu value.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        self.as_u32() as i64 + 1
    }

    /// Parses a one-based authoring menu value.
    #[must_use]
    pub fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Smooth),
            2 => Some(Self::Hard),
            _ => None,
        }
    }

    /// Menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Smooth => "平滑",
            Self::Hard => "硬边",
        }
    }
}

/// Errors raised when author values cannot describe an executable dent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DentError {
    /// The size is not finite.
    NonFiniteSize,
    /// The size is outside the declared range.
    SizeOutOfRange,
    /// The strength is not finite.
    NonFiniteStrength,
    /// The strength is outside the declared range.
    StrengthOutOfRange,
    /// The centre is not a finite point.
    NonFiniteCentre,
}

impl std::fmt::Display for DentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteSize => formatter.write_str("dent size must be a finite fraction"),
            Self::SizeOutOfRange => write!(
                formatter,
                "dent size must be within {DENT_MIN_SIZE_FRACTION}..{DENT_MAX_SIZE_FRACTION} of the frame's short side"
            ),
            Self::NonFiniteStrength => formatter.write_str("dent strength must be finite"),
            Self::StrengthOutOfRange => write!(
                formatter,
                "dent strength must be within {DENT_MIN_STRENGTH}..{DENT_MAX_STRENGTH}"
            ),
            Self::NonFiniteCentre => formatter.write_str("dent centre must be a finite point"),
        }
    }
}

impl std::error::Error for DentError {}

/// Authored dent state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DentGrade {
    size_fraction: f32,
    strength: f32,
    centre: [f32; 2],
    kind: DentKind,
    edge: DentEdge,
}

impl DentGrade {
    /// Builds a grade, rejecting state the declared ranges cannot express.
    ///
    /// The validation order is fixed — size finiteness, size range, strength
    /// finiteness, strength range, centre finiteness — so callers can rely on
    /// the reported error.
    pub fn new(
        size_fraction: f32,
        strength: f32,
        centre: [f32; 2],
        kind: DentKind,
        edge: DentEdge,
    ) -> Result<Self, DentError> {
        if !size_fraction.is_finite() {
            return Err(DentError::NonFiniteSize);
        }
        if !(DENT_MIN_SIZE_FRACTION..=DENT_MAX_SIZE_FRACTION).contains(&size_fraction) {
            return Err(DentError::SizeOutOfRange);
        }
        if !strength.is_finite() {
            return Err(DentError::NonFiniteStrength);
        }
        if !(DENT_MIN_STRENGTH..=DENT_MAX_STRENGTH).contains(&strength) {
            return Err(DentError::StrengthOutOfRange);
        }
        if !centre[0].is_finite() || !centre[1].is_finite() {
            return Err(DentError::NonFiniteCentre);
        }
        Ok(Self { size_fraction, strength, centre, kind, edge })
    }

    /// Authored size, as a fraction of the frame's short side.
    #[must_use]
    pub const fn size_fraction(self) -> f32 {
        self.size_fraction
    }

    /// Authored strength multiplier.
    #[must_use]
    pub const fn strength(self) -> f32 {
        self.strength
    }

    /// Control point in frame pixels.
    #[must_use]
    pub const fn centre(self) -> [f32; 2] {
        self.centre
    }

    /// Which way the disc displaces content.
    #[must_use]
    pub const fn kind(self) -> DentKind {
        self.kind
    }

    /// How the displacement fades out towards the rim.
    #[must_use]
    pub const fn edge(self) -> DentEdge {
        self.edge
    }

    /// Disc radius in frame pixels: the short side times the authored fraction.
    ///
    /// `size_fraction` is only ever allowed to touch the frame here; every
    /// consumer downstream works in pixels.
    #[must_use]
    pub fn radius_pixels(self, width: usize, height: usize) -> f32 {
        // [1] * [px] = [px]
        self.size_fraction * width.min(height) as f32
    }

    /// `true` when the effect provably cannot change any pixel.
    ///
    /// The builder elides the render op in this case; the kernel still maps a
    /// successfully executed identity to `Ok(true)` without touching the frame.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        self.strength <= DENT_IDENTITY_EPSILON || self.size_fraction <= 0.0
    }

    /// Maps one destination pixel to its source position.
    ///
    /// The expression order is the contract the WGSL pass mirrors; do not
    /// reorder the arithmetic or introduce `mul_add`/`hypot`. Pixels outside the
    /// disc, on the rim and at the exact centre are returned bit-identically so
    /// the kernel can skip them.
    #[must_use]
    pub fn sample_position(self, column: f32, row: f32, width: usize, height: usize) -> [f32; 2] {
        // [1] * [px] = [px]
        let radius = self.radius_pixels(width, height);
        let delta_x = column - self.centre[0];
        let delta_y = row - self.centre[1];
        // sqrt(x^2 + y^2) rather than hypot: WGSL has no hypot and the two can
        // differ by an ulp.
        let distance = (delta_x * delta_x + delta_y * delta_y).sqrt();
        if radius <= 0.0 || distance >= radius || distance <= DENT_CENTRE_EPSILON_PIXELS {
            return [column, row];
        }
        // [px] / [px] = [1]
        let u = distance / radius;
        let base = 1.0 - u * u;
        let falloff = match self.edge {
            DentEdge::Smooth => base * base,
            DentEdge::Hard => 1.0,
        };
        let direction = match self.kind {
            DentKind::Dent => 1.0,
            DentKind::Bulge => -1.0,
        };
        // [1] * [1] = [1]; `(direction * strength)` binds first so the sign is
        // exact and matches the left-associative WGSL expression.
        let gain = (direction * self.strength) * falloff;
        // [1]
        let scale = 1.0 + gain;
        debug_assert!(scale >= 0.0, "dent scale must stay non-negative");
        // [px] + [px] * [1] = [px]
        let sample_x = self.centre[0] + delta_x * scale;
        let sample_y = self.centre[1] + delta_y * scale;
        debug_assert!(
            sample_x.is_finite() && sample_y.is_finite(),
            "dent sample must stay finite"
        );
        [sample_x, sample_y]
    }

    /// Uniform packing for the GPU pass: `(params, extra)` for [`SpatialPassUniform`].
    ///
    /// `params` carries the control point, the size fraction and the strength;
    /// `extra` carries the kind and edge discriminants, zero-based, with the
    /// unused cells explicitly zeroed. The authoring menu is one-based; the
    /// uniform is zero-based.
    ///
    /// [`SpatialPassUniform`]: crate::EffectGpuSpatialPass
    #[must_use]
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.centre[0],
                self.centre[1],
                self.size_fraction,
                self.strength,
            ],
            [
                self.kind.as_u32() as f32,
                self.edge.as_u32() as f32,
                0.0,
                0.0,
            ],
        )
    }
}

/// Applies the dent to a whole frame, returning `false` when the frame cannot
/// be processed.
pub fn dent_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: DentGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_dent_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the dent in place with an explicit cancellation checkpoint.
///
/// `Ok(true)` means the channel supports and executed the effect (including the
/// identity), `Ok(false)` means the frame geometry is not executable.
pub(crate) fn apply_dent_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: DentGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || width == 0 || height == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }

    let frame = &mut working[..pixel_count];
    // Premultiply exactly once, immediately before the resampling: the shared
    // bilinear helper filters colour and coverage together.
    premultiply_rgba_controlled(frame, checkpoint)?;
    let source = frame.to_vec();
    let radius = grade.radius_pixels(width, height);
    debug_assert!(radius <= width.min(height) as f32);
    checkpoint()?;

    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let source_x = column as f32;
                let source_y = row as f32;
                let [sample_x, sample_y] = grade.sample_position(source_x, source_y, width, height);
                // Bitwise, not an epsilon: the identity branch returns its
                // inputs unchanged, so this is exactly the pixels the mapping
                // leaves alone.
                if sample_x == source_x && sample_y == source_y {
                    continue;
                }
                *pixel = sample_bilinear_clamped(
                    &source,
                    width,
                    height,
                    f64::from(sample_x),
                    f64::from(sample_y),
                );
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(
        size_fraction: f32,
        strength: f32,
        centre: [f32; 2],
        kind: DentKind,
        edge: DentEdge,
    ) -> DentGrade {
        DentGrade::new(size_fraction, strength, centre, kind, edge).expect("valid dent grade")
    }

    fn smooth(size_fraction: f32, strength: f32, centre: [f32; 2]) -> DentGrade {
        grade(
            size_fraction,
            strength,
            centre,
            DentKind::Dent,
            DentEdge::Smooth,
        )
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for y in 0..height {
            for x in 0..width {
                let value = 0.04 * (x + 2 * y) as f32;
                pixels[y * width + x] = [value, 0.5 - value * 0.2, value * 1.4, 1.0];
            }
        }
        pixels
    }

    /// An independent `f64` evaluation of the documented mapping.
    fn reference_position(
        applied: DentGrade,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> [f64; 2] {
        let radius = f64::from(applied.size_fraction()) * width.min(height) as f64;
        let centre_x = f64::from(applied.centre()[0]);
        let centre_y = f64::from(applied.centre()[1]);
        let delta_x = column - centre_x;
        let delta_y = row - centre_y;
        let distance_squared = delta_x.powi(2) + delta_y.powi(2);
        let distance = distance_squared.sqrt();
        if radius <= 0.0 || distance_squared >= radius.powi(2) || distance <= 1.0e-6 {
            return [column, row];
        }
        let ratio = distance / radius;
        let falloff = match applied.edge() {
            DentEdge::Smooth => (1.0 - ratio.powi(2)).powi(2),
            DentEdge::Hard => 1.0,
        };
        let direction = match applied.kind() {
            DentKind::Dent => 1.0,
            DentKind::Bulge => -1.0,
        };
        let scale = 1.0 + direction * f64::from(applied.strength()) * falloff;
        [centre_x + delta_x * scale, centre_y + delta_y * scale]
    }

    fn bits(pixel: [f32; 4]) -> [u32; 4] {
        pixel.map(f32::to_bits)
    }

    #[test]
    fn zero_strength_is_a_bitwise_no_op() {
        let width = 13;
        let height = 11;
        let mut pixels = frame(width, height);
        let original = pixels.clone();
        let applied = smooth(0.4, 0.0, [6.0, 5.0]);
        assert!(applied.is_identity());
        assert!(dent_frame_f32(&mut pixels, width, height, applied));
        for (index, (pixel, original)) in pixels.iter().zip(original.iter()).enumerate() {
            assert_eq!(bits(*pixel), bits(*original), "pixel {index} changed");
        }

        let mut pixels = frame(width, height);
        let original = pixels.clone();
        let zero_size = smooth(0.0, 0.5, [6.0, 5.0]);
        assert!(zero_size.is_identity());
        assert!(dent_frame_f32(&mut pixels, width, height, zero_size));
        for (index, (pixel, original)) in pixels.iter().zip(original.iter()).enumerate() {
            assert_eq!(bits(*pixel), bits(*original), "pixel {index} changed");
        }
    }

    #[test]
    fn the_mapping_matches_an_independent_evaluation() {
        let applied = smooth(0.4, 0.5, [6.0, 5.0]);
        let radius = applied.radius_pixels(13, 11);
        // Twelve probes: the centre, u = 1/sqrt(5), u = 0.9, the four rim
        // points, two corners, and two points well outside the disc.
        let probes = [
            (6.0_f64, 5.0_f64),
            (6.0 + f64::from(radius) / 5.0_f64.sqrt(), 5.0),
            (6.0 + 0.9 * f64::from(radius), 5.0),
            (6.0 + f64::from(radius), 5.0),
            (6.0 - f64::from(radius), 5.0),
            (6.0, 5.0 + f64::from(radius)),
            (6.0, 5.0 - f64::from(radius)),
            (0.0, 0.0),
            (12.0, 10.0),
            (0.0, 10.0),
            (12.0, 0.0),
            (6.0, 0.0),
        ];
        for (column, row) in probes {
            let expected = reference_position(applied, column, row, 13, 11);
            let [actual_x, actual_y] = applied.sample_position(column as f32, row as f32, 13, 11);
            assert!(
                (expected[0] - f64::from(actual_x)).abs() <= 2.0e-6,
                "probe ({column},{row}) x: reference {} kernel {actual_x}",
                expected[0]
            );
            assert!(
                (expected[1] - f64::from(actual_y)).abs() <= 2.0e-6,
                "probe ({column},{row}) y: reference {} kernel {actual_y}",
                expected[1]
            );
        }

        // The exact-centre probe is bit-identical, not merely close.
        assert_eq!(applied.sample_position(6.0, 5.0, 13, 11), [6.0, 5.0]);
        // R = 0.4 * 11 = 4.4 px, so a probe three pixels right of the centre
        // sits at u = 3 / 4.4 and the sample is the hand-computed scale.
        let radius = 0.4_f32 * 11.0;
        let u = 3.0_f32 / radius;
        let base = 1.0_f32 - u * u;
        let scale = 1.0_f32 + 0.5_f32 * (base * base);
        assert_eq!(
            applied.sample_position(9.0, 5.0, 13, 11),
            [6.0 + 3.0 * scale, 5.0]
        );
    }

    #[test]
    fn the_falloff_is_monotonic_and_the_rim_is_fixed() {
        let applied = smooth(0.4, 0.5, [6.0, 5.0]);
        let radius = applied.radius_pixels(13, 11);
        let mut previous = f32::INFINITY;
        for u in [0.05_f32, 0.2, 0.4, 0.6, 0.8, 0.95] {
            let column = 6.0 + u * radius;
            let [sample_x, _] = applied.sample_position(column, 5.0, 13, 11);
            let scale = (sample_x - 6.0) / (column - 6.0);
            assert!(
                scale < previous,
                "u {u}: scale {scale} not below {previous}"
            );
            previous = scale;
        }

        // Just inside the rim the curve has already reached the identity.
        let column = 6.0 + 0.999 * radius;
        let [sample_x, _] = applied.sample_position(column, 5.0, 13, 11);
        assert!(
            (sample_x - column).abs() <= 1.0e-3,
            "rim displacement {}",
            sample_x - column
        );

        // The peak displacement sits at u = 1/sqrt(5) and is 0.2862167 * s * R.
        let peak_column = 6.0 + radius / 5.0_f32.sqrt();
        let [peak_sample, _] = applied.sample_position(peak_column, 5.0, 13, 11);
        let displacement = (peak_sample - peak_column).abs();
        let expected = 0.2862167 * applied.strength() * radius;
        assert!(
            (displacement - expected).abs() <= 1.0e-3,
            "peak {displacement} vs {expected}"
        );

        // The hard edge keeps the full gain across the whole disc.
        let hard = grade(0.4, 0.5, [6.0, 5.0], DentKind::Dent, DentEdge::Hard);
        for u in [0.05_f32, 0.2, 0.4, 0.6, 0.8, 0.95] {
            let column = 6.0 + u * radius;
            let [sample_x, sample_y] = hard.sample_position(column, 5.0, 13, 11);
            // scale = 1 + (1 * 0.5) * 1 is exactly 1.5, so the sample is exact.
            assert_eq!(
                [sample_x, sample_y],
                [6.0 + (column - 6.0) * 1.5, 5.0],
                "hard u {u}"
            );
        }
    }

    #[test]
    fn pixels_outside_the_region_are_bit_identical() {
        let width = 13;
        let height = 11;
        let mut pixels = frame(width, height);
        // Zero-coverage pixels must be canonical, otherwise the premultiply
        // round trip would report a change the mapping did not make.
        pixels[2 * width + 3] = [0.0, 0.0, 0.0, 0.0];
        // An extended-range pixel with full coverage must survive untouched.
        pixels[5 * width + 6] = [2.5, -0.75, 1.25, 1.0];
        let original = pixels.clone();
        let applied = smooth(3.0 / 11.0, 1.0, [6.0, 5.0]);
        let radius = applied.radius_pixels(width, height);
        assert_eq!(radius, 3.0);
        assert!(dent_frame_f32(&mut pixels, width, height, applied));
        let mut changed = 0;
        for row in 0..height {
            for column in 0..width {
                let index = row * width + column;
                let delta_x = column as f32 - 6.0;
                let delta_y = row as f32 - 5.0;
                let distance = (delta_x * delta_x + delta_y * delta_y).sqrt();
                if distance >= radius {
                    assert_eq!(
                        bits(pixels[index]),
                        bits(original[index]),
                        "pixel ({column},{row}) outside the disc changed"
                    );
                } else if bits(pixels[index]) != bits(original[index]) {
                    changed += 1;
                }
            }
        }
        assert!(
            changed >= 3,
            "only {changed} pixels inside the disc changed"
        );
    }

    #[test]
    fn a_transparent_neighbour_does_not_bleed_into_colour() {
        let width = 11;
        let height = 11;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; width * height];
        for y in 4..=6 {
            for x in 4..=6 {
                pixels[y * width + x] = [1.0, 0.5, 0.25, 1.0];
            }
        }
        // R = 3 px, strength 1: pixel (4,5) samples at
        // 5 - 1 * (1 + (1 - 1/9)^2) = 3.209876..., between the transparent
        // column 3 and the opaque column 4.
        let applied = smooth(3.0 / 11.0, 1.0, [5.0, 5.0]);
        assert_eq!(applied.radius_pixels(width, height), 3.0);
        assert!(dent_frame_f32(&mut pixels, width, height, applied));
        let sampled = pixels[5 * width + 4];
        assert!((sampled[0] - 1.0).abs() <= 1.0e-6, "red {}", sampled[0]);
        assert!((sampled[1] - 0.5).abs() <= 1.0e-6, "green {}", sampled[1]);
        assert!((sampled[2] - 0.25).abs() <= 1.0e-6, "blue {}", sampled[2]);
        assert!(
            sampled[3] > 0.0 && sampled[3] < 1.0,
            "alpha {} must be partial",
            sampled[3]
        );
        assert!(
            (sampled[3] - 0.20987654).abs() <= 1.0e-5,
            "alpha {}",
            sampled[3]
        );
        for (index, pixel) in pixels.iter().enumerate() {
            assert!(
                (-1.0e-6..=1.0 + 1.0e-6).contains(&pixel[0])
                    && (-1.0e-6..=0.5 + 1.0e-6).contains(&pixel[1])
                    && (-1.0e-6..=0.25 + 1.0e-6).contains(&pixel[2]),
                "pixel {index} left the source colour hull: {pixel:?}"
            );
        }
    }

    #[test]
    fn hard_edge_steps_at_the_rim() {
        let applied = grade(
            10.0 / 21.0,
            0.5,
            [10.0, 10.0],
            DentKind::Dent,
            DentEdge::Hard,
        );
        let radius = applied.radius_pixels(21, 21);
        assert!((radius - 10.0).abs() <= 1.0e-6, "radius {radius}");
        // delta = 9, hard gain = 0.5, scale = 1.5: 10 + 13.5.
        assert_eq!(applied.sample_position(19.0, 10.0, 21, 21), [23.5, 10.0]);
        // The rim belongs to the identity side of the branch.
        assert_eq!(applied.sample_position(20.0, 10.0, 21, 21), [20.0, 10.0]);
        let stepped = smooth(10.0 / 21.0, 0.5, [10.0, 10.0]);
        let [smooth_x, smooth_y] = stepped.sample_position(19.0, 10.0, 21, 21);
        assert!((smooth_y - 10.0).abs() <= 1.0e-6, "smooth row {smooth_y}");
        assert!(
            (smooth_x - 19.16245).abs() <= 1.0e-4,
            "smooth sample {smooth_x}"
        );
        assert!(23.5 - smooth_x > 4.0, "the step must exceed four pixels");
    }

    #[test]
    fn size_is_a_fraction_of_the_short_side() {
        let applied = smooth(0.4, 0.5, [6.0, 5.0]);
        assert!((applied.radius_pixels(13, 11) - 4.4_f32).abs() <= 1.0e-6);
        assert!((applied.radius_pixels(512, 512) - 204.8_f32).abs() <= 1.0e-6);
        assert!((applied.radius_pixels(1000, 100) - 40.0_f32).abs() <= 1.0e-6);
        assert_eq!(applied.radius_pixels(0, 0), 0.0);
        // Default parameters at 512x512: the peak displacement of the smooth
        // curve is 0.2862167 * s * R.
        let radius = f64::from(applied.radius_pixels(512, 512));
        let displacement = 0.2862167 * f64::from(applied.strength()) * radius;
        assert!(
            (displacement - 29.3).abs() <= 0.01,
            "default peak displacement {displacement}"
        );
    }

    #[test]
    fn the_grade_rejects_non_finite_and_out_of_range_controls() {
        let centre = [128.0, 128.0];
        assert_eq!(
            DentGrade::new(f32::NAN, 0.5, centre, DentKind::Dent, DentEdge::Smooth),
            Err(DentError::NonFiniteSize)
        );
        assert_eq!(
            DentGrade::new(f32::INFINITY, 0.5, centre, DentKind::Dent, DentEdge::Smooth),
            Err(DentError::NonFiniteSize)
        );
        assert_eq!(
            DentGrade::new(-0.1, 0.5, centre, DentKind::Dent, DentEdge::Smooth),
            Err(DentError::SizeOutOfRange)
        );
        assert_eq!(
            DentGrade::new(1.1, 0.5, centre, DentKind::Dent, DentEdge::Smooth),
            Err(DentError::SizeOutOfRange)
        );
        assert_eq!(
            DentGrade::new(0.4, f32::NAN, centre, DentKind::Dent, DentEdge::Smooth),
            Err(DentError::NonFiniteStrength)
        );
        assert_eq!(
            DentGrade::new(0.4, f32::INFINITY, centre, DentKind::Dent, DentEdge::Smooth),
            Err(DentError::NonFiniteStrength)
        );
        assert_eq!(
            DentGrade::new(0.4, -0.1, centre, DentKind::Dent, DentEdge::Smooth),
            Err(DentError::StrengthOutOfRange)
        );
        assert_eq!(
            DentGrade::new(0.4, 1.1, centre, DentKind::Dent, DentEdge::Smooth),
            Err(DentError::StrengthOutOfRange)
        );
        assert_eq!(
            DentGrade::new(0.4, 0.5, [f32::NAN, 1.0], DentKind::Dent, DentEdge::Smooth),
            Err(DentError::NonFiniteCentre)
        );
        assert_eq!(
            DentGrade::new(
                0.4,
                0.5,
                [1.0, f32::INFINITY],
                DentKind::Dent,
                DentEdge::Smooth
            ),
            Err(DentError::NonFiniteCentre)
        );
        // The first failing check wins: size is reported before strength.
        assert_eq!(
            DentGrade::new(2.0, 2.0, [f32::NAN, 0.0], DentKind::Dent, DentEdge::Smooth),
            Err(DentError::SizeOutOfRange)
        );
        assert!(DentGrade::new(0.4, 0.5, centre, DentKind::Dent, DentEdge::Smooth).is_ok());
    }

    #[test]
    fn mismatched_frames_are_reported_as_unsupported() {
        let applied = smooth(0.4, 0.5, [2.0, 2.0]);
        let mut pixels = frame(4, 4);
        assert!(!dent_frame_f32(&mut pixels[..15], 4, 4, applied));
        assert!(!dent_frame_f32(&mut pixels, 0, 4, applied));
        assert!(!dent_frame_f32(&mut pixels, 4, 0, applied));
        assert!(!dent_frame_f32(&mut pixels, usize::MAX, 2, applied));
    }

    #[test]
    fn gpu_controls_packs_the_documented_cells() {
        let dent = smooth(0.4, 0.5, [6.0, 5.0]);
        let (params, extra) = dent.gpu_controls();
        assert_eq!(params, [6.0, 5.0, 0.4, 0.5]);
        assert_eq!(extra, [0.0, 0.0, 0.0, 0.0]);

        let bulge = grade(1.0, 1.0, [0.0, 0.0], DentKind::Bulge, DentEdge::Hard);
        let (params, extra) = bulge.gpu_controls();
        assert_eq!(params, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(extra, [1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn hdr_colour_is_not_clamped() {
        let width = 9;
        let height = 9;
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for y in 0..height {
            for x in 0..width {
                let value = 3.0 + 0.1 * x as f32;
                pixels[y * width + x] = [value, -0.25 - 0.05 * y as f32, 2.0, 1.0];
            }
        }
        let applied = smooth(0.4, 1.0, [4.0, 4.0]);
        assert!(dent_frame_f32(&mut pixels, width, height, applied));
        for (index, pixel) in pixels.iter().enumerate() {
            assert!(pixel[0] > 1.0, "pixel {index} was clamped: {pixel:?}");
            assert!(pixel[1] < 0.0, "pixel {index} was clamped: {pixel:?}");
        }
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let applied = smooth(0.4, 0.5, [6.0, 5.0]);
        let mut first = frame(13, 11);
        let mut second = first.clone();
        assert!(dent_frame_f32(&mut first, 13, 11, applied));
        assert!(dent_frame_f32(&mut second, 13, 11, applied));
        for (index, (first, second)) in first.iter().zip(second.iter()).enumerate() {
            assert_eq!(bits(*first), bits(*second), "pixel {index} is not stable");
        }
    }
}
