//! Magnify effect (DST-07): a lens that enlarges a region of the frame.
//!
//! The kernel works in straight alpha. It snapshots the frame, premultiplies a
//! copy of that snapshot, samples the copy through the inverse `1 / M` lens map,
//! and composites the un-premultiplied average over the *original* straight
//! pixel. Sampling a premultiplied copy is what keeps transparent neighbours
//! from bleeding dark fringes into visible pixels, while compositing over the
//! untouched source keeps `alpha == 0` pixels bit-exact.
//!
//! The lens only reads inside its own radius, so the spatial pass declares a
//! finite halo instead of the whole-frame halo the other remap effects use.

use mondrian_core::BlendMode;

use crate::coverage::straight_rgba_from_premultiplied;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::premultiply_rgba_controlled;

/// Largest authored lens diameter, in frame pixels.
pub const MAGNIFY_MAX_DIAMETER_PIXELS: f32 = 4_000.0;
/// Smallest authored lens diameter, in frame pixels.
pub const MAGNIFY_MIN_DIAMETER_PIXELS: f32 = 1.0;
/// Largest authored feather width, in frame pixels.
pub const MAGNIFY_MAX_FEATHER_PIXELS: f32 = 1_000.0;
/// Smallest magnification factor (`100 %`).
const MAGNIFY_MIN_MAGNIFICATION: f32 = 1.0;
/// Largest magnification factor (`20 000 %`).
const MAGNIFY_MAX_MAGNIFICATION: f32 = 200.0;
/// Largest diameter a resolved grade may carry.
///
/// The `/ 链接` property folds the magnification into the diameter before the
/// grade exists, so the resolved diameter can exceed the authored maximum by
/// the largest magnification factor (`4 000 px × 200`).
pub const MAGNIFY_MAX_RESOLVED_DIAMETER_PIXELS: f32 =
    MAGNIFY_MAX_DIAMETER_PIXELS * MAGNIFY_MAX_MAGNIFICATION;

/// The reason a set of author values cannot describe an executable magnify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagnifyError {
    /// An authored number leaves the range its property declares.
    ///
    /// The payload names the offending property (`"size"`, `"magnification"`,
    /// `"feather"`, or `"opacity"`).
    Range(&'static str),
    /// The lens centre is not a finite point.
    NonFinite,
    /// The blend mode is not one of the declared lens modes.
    BlendMode,
}

impl std::fmt::Display for MagnifyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Range(parameter) => write!(
                formatter,
                "magnify {parameter} is outside its declared range"
            ),
            Self::NonFinite => write!(formatter, "magnify centre must be a finite point"),
            Self::BlendMode => write!(
                formatter,
                "magnify blend mode is not one of the declared lens modes"
            ),
        }
    }
}

impl std::error::Error for MagnifyError {}

/// Whether `mode` is one of the 22 blend modes the magnify lens declares.
///
/// The lens excludes [`BlendMode::Dissolve`] (it is a stochastic dither, not a
/// per-pixel composite) and the four HSL modes, which cannot be expressed by the
/// channel-wise `blend_channel` table the spatial shader ports.
#[must_use]
pub fn is_declared_blend_mode(mode: BlendMode) -> bool {
    !matches!(
        mode,
        BlendMode::Dissolve
            | BlendMode::Hue
            | BlendMode::Saturation
            | BlendMode::Color
            | BlendMode::Luminosity
    )
}

/// The silhouette of the lens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagnifyShape {
    /// A circular lens.
    Circle,
    /// An axis-aligned square lens.
    Square,
}

impl MagnifyShape {
    /// The authored option key of this shape.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Circle => "circle",
            Self::Square => "square",
        }
    }

    /// Parses an authored option key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "circle" => Some(Self::Circle),
            "square" => Some(Self::Square),
            _ => None,
        }
    }

    /// The shape flag the shader reads from `extra.z`.
    #[must_use]
    pub const fn flag(self) -> f32 {
        match self {
            Self::Circle => 0.0,
            Self::Square => 1.0,
        }
    }
}

/// How the lens diameter follows the magnification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagnifyLink {
    /// The authored size is the diameter.
    Independent,
    /// The diameter grows with the magnification.
    FollowMagnification,
    /// The diameter shrinks with the magnification.
    InverseMagnification,
}

impl MagnifyLink {
    /// The authored option key of this link mode.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::FollowMagnification => "follow_magnification",
            Self::InverseMagnification => "inverse_magnification",
        }
    }

    /// Parses an authored option key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "independent" => Some(Self::Independent),
            "follow_magnification" => Some(Self::FollowMagnification),
            "inverse_magnification" => Some(Self::InverseMagnification),
            _ => None,
        }
    }

    /// Resolves the authored diameter for this link mode.
    ///
    /// `magnification` is the factor (`1.0` is `100 %`), matching the grade.
    #[must_use]
    pub fn diameter_for(self, size_pixels: f32, magnification: f32) -> f32 {
        match self {
            Self::Independent => size_pixels,
            Self::FollowMagnification => size_pixels * magnification,
            Self::InverseMagnification => size_pixels / magnification,
        }
    }
}

/// How many samples the lens takes per output pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagnifyScaling {
    /// One bilinear sample per pixel.
    Bilinear,
    /// A `2 × 2` supersampled footprint per pixel.
    Supersample2,
    /// A `4 × 4` supersampled footprint per pixel.
    Supersample4,
}

impl MagnifyScaling {
    /// The authored option key of this scaling mode.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::Supersample2 => "supersample_2",
            Self::Supersample4 => "supersample_4",
        }
    }

    /// Parses an authored option key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "bilinear" => Some(Self::Bilinear),
            "supersample_2" => Some(Self::Supersample2),
            "supersample_4" => Some(Self::Supersample4),
            _ => None,
        }
    }

    /// The supersample density per axis: `1`, `2`, or `4`.
    #[must_use]
    pub const fn factor(self) -> u32 {
        match self {
            Self::Bilinear => 1,
            Self::Supersample2 => 2,
            Self::Supersample4 => 4,
        }
    }
}

/// The four uniform cells a magnify spatial pass uploads.
///
/// The alias keeps [`MagnifyGrade::gpu_controls`] readable and satisfies
/// `clippy::type_complexity`.
pub type MagnifyControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A fully resolved magnify lens.
///
/// `diameter_pixels` is already resolved through [`MagnifyLink`], because the
/// shader only needs the final diameter; changing the authored link mode still
/// changes the op hash, since it changes the resolved diameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MagnifyGrade {
    centre: [f32; 2],
    diameter_pixels: f32,
    magnification: f32,
    feather_pixels: f32,
    opacity_fraction: f32,
    shape: MagnifyShape,
    scaling: MagnifyScaling,
    blend_mode: BlendMode,
}

impl MagnifyGrade {
    /// Builds a lens from resolved author values.
    ///
    /// `magnification` is a factor (`1.0` is `100 %`), `opacity_fraction` is in
    /// `0..=1`, and `diameter_pixels` is the diameter after [`MagnifyLink`] was
    /// resolved, so it may exceed [`MAGNIFY_MAX_DIAMETER_PIXELS`].
    ///
    /// # Errors
    ///
    /// Returns [`MagnifyError::NonFinite`] for a non-finite centre,
    /// [`MagnifyError::Range`] for a number outside its declared range, and
    /// [`MagnifyError::BlendMode`] for an undeclared blend mode.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        centre: [f32; 2],
        diameter_pixels: f32,
        magnification: f32,
        feather_pixels: f32,
        opacity_fraction: f32,
        shape: MagnifyShape,
        scaling: MagnifyScaling,
        blend_mode: BlendMode,
    ) -> Result<Self, MagnifyError> {
        if !centre[0].is_finite() || !centre[1].is_finite() {
            return Err(MagnifyError::NonFinite);
        }
        if !magnification.is_finite()
            || !(MAGNIFY_MIN_MAGNIFICATION..=MAGNIFY_MAX_MAGNIFICATION).contains(&magnification)
        {
            return Err(MagnifyError::Range("magnification"));
        }
        if !feather_pixels.is_finite()
            || !(0.0..=MAGNIFY_MAX_FEATHER_PIXELS).contains(&feather_pixels)
        {
            return Err(MagnifyError::Range("feather"));
        }
        if !opacity_fraction.is_finite() || !(0.0..=1.0).contains(&opacity_fraction) {
            return Err(MagnifyError::Range("opacity"));
        }
        if !diameter_pixels.is_finite()
            || !(MAGNIFY_MIN_DIAMETER_PIXELS..=MAGNIFY_MAX_RESOLVED_DIAMETER_PIXELS)
                .contains(&diameter_pixels)
        {
            return Err(MagnifyError::Range("size"));
        }
        if !is_declared_blend_mode(blend_mode) {
            return Err(MagnifyError::BlendMode);
        }
        Ok(Self {
            centre,
            diameter_pixels,
            magnification,
            feather_pixels,
            opacity_fraction,
            shape,
            scaling,
            blend_mode,
        })
    }

    /// The lens centre, in frame pixels.
    #[must_use]
    pub const fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// The resolved lens diameter, in frame pixels.
    #[must_use]
    pub const fn diameter_pixels(&self) -> f32 {
        self.diameter_pixels
    }

    /// The lens radius, in frame pixels.
    #[must_use]
    pub fn radius_pixels(&self) -> f32 {
        self.diameter_pixels * 0.5
    }

    /// The magnification factor (`1.0` is `100 %`).
    #[must_use]
    pub const fn magnification(&self) -> f32 {
        self.magnification
    }

    /// The feather width, in frame pixels, measured inward from the rim.
    #[must_use]
    pub const fn feather_pixels(&self) -> f32 {
        self.feather_pixels
    }

    /// The authored opacity as a fraction in `0..=1`.
    #[must_use]
    pub const fn opacity_fraction(&self) -> f32 {
        self.opacity_fraction
    }

    /// The lens silhouette.
    #[must_use]
    pub const fn shape(&self) -> MagnifyShape {
        self.shape
    }

    /// The supersample density used for each output pixel.
    #[must_use]
    pub const fn scaling(&self) -> MagnifyScaling {
        self.scaling
    }

    /// The blend mode the lens composites with.
    #[must_use]
    pub const fn blend_mode(&self) -> BlendMode {
        self.blend_mode
    }

    /// Whether the lens leaves every pixel of the frame bit-exact.
    ///
    /// A `100 %` lens only stops being an identity when a non-normal blend mode
    /// can still square or screen the frame with itself.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.magnification == 1.0
            && (matches!(self.blend_mode, BlendMode::Normal) || self.opacity_fraction == 0.0)
    }

    /// The uniform cells the spatial pass uploads: `params`, `extra`, `aces`,
    /// and the unused `extra1`.
    #[must_use]
    pub fn gpu_controls(&self) -> MagnifyControls {
        (
            [
                self.centre[0],
                self.centre[1],
                self.radius_pixels(),
                self.magnification,
            ],
            [
                self.feather_pixels,
                self.opacity_fraction,
                self.shape.flag(),
                self.scaling.factor() as f32,
            ],
            [
                crate::glow::blend_mode_index(self.blend_mode) as f32,
                0.0,
                0.0,
                0.0,
            ],
            [0.0; 4],
        )
    }
}

/// Magnifies a straight-alpha `f32` frame in place.
///
/// Returns `false` when the buffer or the declared size cannot describe a frame.
#[must_use]
pub fn magnify_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: MagnifyGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_magnify_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed magnify kernel the render op drives.
///
/// # Errors
///
/// Propagates the checkpoint error. Size mismatches and identity grades are not
/// errors: they return `Ok(false)` and `Ok(true)` respectively.
pub(crate) fn apply_magnify_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: MagnifyGrade,
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

    let frame = &mut working[..pixel_count];
    let source = frame.to_vec();
    let mut staged = source.clone();
    premultiply_rgba_controlled(&mut staged, checkpoint)?;

    let rows = pixel_count / width;
    let centre_x = f64::from(grade.centre[0]);
    let centre_y = f64::from(grade.centre[1]);
    let radius = f64::from(grade.radius_pixels());
    let feather = f64::from(grade.feather_pixels);
    let opacity = f64::from(grade.opacity_fraction);
    let inverse_magnification = 1.0 / f64::from(grade.magnification);
    let supersample = grade.scaling.factor();
    let sample_count = f64::from(supersample * supersample);
    let offsets: Vec<f64> = (0..supersample)
        .map(|index| (f64::from(index) + 0.5) / f64::from(supersample) - 0.5)
        .collect();
    let square = matches!(grade.shape, MagnifyShape::Square);
    let blend_mode = grade.blend_mode;

    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            let delta_y = row as f64 - centre_y;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let delta_x = column as f64 - centre_x;
                let distance = if square {
                    delta_x.abs().max(delta_y.abs())
                } else {
                    delta_x.hypot(delta_y)
                };
                let weight = if feather <= 0.0 {
                    if distance < radius {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    let ramp = ((radius - distance) / feather).clamp(0.0, 1.0);
                    ramp * ramp * (3.0 - 2.0 * ramp)
                };
                if weight <= 0.0 {
                    continue;
                }

                let mut accumulator = [0.0_f64; 4];
                for offset_y in &offsets {
                    let sample_y = centre_y + (delta_y + offset_y) * inverse_magnification;
                    for offset_x in &offsets {
                        let sample_x = centre_x + (delta_x + offset_x) * inverse_magnification;
                        let sample =
                            sample_bilinear_clamped(&staged, width, height, sample_x, sample_y);
                        for channel in 0..4 {
                            accumulator[channel] += f64::from(sample[channel]);
                        }
                    }
                }
                let average = [
                    (accumulator[0] / sample_count) as f32,
                    (accumulator[1] / sample_count) as f32,
                    (accumulator[2] / sample_count) as f32,
                    (accumulator[3] / sample_count) as f32,
                ];
                let magnified = straight_rgba_from_premultiplied(average);
                let base = source[row * width + column];
                *pixel = crate::adjustment::blend_rgba_f32_pixel(
                    base,
                    magnified,
                    (opacity * weight) as f32,
                    blend_mode,
                );
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(
        centre: [f32; 2],
        diameter_pixels: f32,
        magnification: f32,
        feather_pixels: f32,
        opacity_fraction: f32,
        shape: MagnifyShape,
        scaling: MagnifyScaling,
        blend_mode: BlendMode,
    ) -> MagnifyGrade {
        MagnifyGrade::new(
            centre,
            diameter_pixels,
            magnification,
            feather_pixels,
            opacity_fraction,
            shape,
            scaling,
            blend_mode,
        )
        .expect("the fixture lens is inside the authored ranges")
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let value = 0.04 * (x + 2 * y) as f32;
                pixels.push([value, 0.5 - value * 0.2, value * 1.4, 1.0]);
            }
        }
        pixels
    }

    #[test]
    fn magnification_one_is_the_identity() {
        for (shape, scaling, blend_mode, opacity) in [
            (
                MagnifyShape::Circle,
                MagnifyScaling::Bilinear,
                BlendMode::Normal,
                1.0,
            ),
            (
                MagnifyShape::Square,
                MagnifyScaling::Supersample4,
                BlendMode::Normal,
                0.5,
            ),
            (
                MagnifyShape::Square,
                MagnifyScaling::Supersample2,
                BlendMode::Multiply,
                0.0,
            ),
        ] {
            let mut pixels = frame(19, 13);
            let before = pixels.clone();
            let lens = grade(
                [9.0, 6.0],
                12.0,
                1.0,
                3.0,
                opacity,
                shape,
                scaling,
                blend_mode,
            );
            assert!(lens.is_identity(), "a 100 % lens must be an identity");
            assert!(magnify_frame_f32(&mut pixels, 19, 13, lens));
            assert_eq!(pixels, before, "an identity lens must not move a bit");
        }
    }

    #[test]
    fn magnification_one_with_multiply_is_not_the_identity() {
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; 9 * 9];
        let before = pixels.clone();
        let lens = grade(
            [4.0, 4.0],
            4.0,
            1.0,
            0.0,
            1.0,
            MagnifyShape::Circle,
            MagnifyScaling::Bilinear,
            BlendMode::Multiply,
        );
        assert!(!lens.is_identity());
        assert!(magnify_frame_f32(&mut pixels, 9, 9, lens));

        let centre = 4 * 9 + 4;
        assert!(
            (pixels[centre][0] - 0.25).abs() < 1.0e-6,
            "multiplying the lens over itself must square 0.5 into 0.25, got {}",
            pixels[centre][0]
        );
        let corner = 0;
        assert_eq!(
            pixels[corner], before[corner],
            "outside the rim the multiply lens must be bit-exact"
        );
    }

    #[test]
    fn the_centre_pixel_maps_to_itself() {
        let mut pixels = frame(64, 64);
        let before = pixels.clone();
        let lens = grade(
            [32.0, 32.0],
            40.0,
            200.0,
            0.0,
            1.0,
            MagnifyShape::Circle,
            MagnifyScaling::Bilinear,
            BlendMode::Normal,
        );
        assert!(magnify_frame_f32(&mut pixels, 64, 64, lens));
        let centre = 32 * 64 + 32;
        assert_eq!(pixels[centre], before[centre]);
    }

    #[test]
    fn outside_the_rim_is_bit_exact() {
        let mut pixels = frame(33, 33);
        let before = pixels.clone();
        let lens = grade(
            [16.0, 16.0],
            10.0,
            3.0,
            0.0,
            1.0,
            MagnifyShape::Circle,
            MagnifyScaling::Bilinear,
            BlendMode::Normal,
        );
        assert!(magnify_frame_f32(&mut pixels, 33, 33, lens));

        let mut touched = 0;
        for y in 0..33 {
            for x in 0..33 {
                let delta_x = x as f64 - 16.0;
                let delta_y = y as f64 - 16.0;
                let index = y * 33 + x;
                if delta_x.hypot(delta_y) < 5.0 {
                    if pixels[index] != before[index] {
                        touched += 1;
                    }
                } else {
                    assert_eq!(
                        pixels[index], before[index],
                        "({x}, {y}) is outside the rim and must stay bit-exact"
                    );
                }
            }
        }
        assert!(touched > 0, "the lens must touch at least one pixel");
    }

    #[test]
    fn the_lens_magnifies_by_the_scale() {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; 41 * 41];
        pixels[20 * 41 + 30] = [1.0, 1.0, 1.0, 1.0];
        let lens = grade(
            [20.0, 20.0],
            60.0,
            2.0,
            0.0,
            1.0,
            MagnifyShape::Circle,
            MagnifyScaling::Bilinear,
            BlendMode::Normal,
        );
        assert!(magnify_frame_f32(&mut pixels, 41, 41, lens));

        let reflected = pixels[20 * 41 + 40];
        assert!(
            reflected[0] > 0.99,
            "an impulse at +10 px must appear at +20 px, got {}",
            reflected[0]
        );
        let abandoned = pixels[20 * 41 + 30];
        assert!(
            abandoned[0] < 0.01,
            "the lens must read from the source, leaving +10 px dark, got {}",
            abandoned[0]
        );
    }

    #[test]
    fn the_feather_ramps_between_one_and_zero() {
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; 41 * 41];
        let lens = grade(
            [16.0, 16.0],
            20.0,
            2.0,
            4.0,
            1.0,
            MagnifyShape::Circle,
            MagnifyScaling::Bilinear,
            BlendMode::Multiply,
        );
        assert!(magnify_frame_f32(&mut pixels, 41, 41, lens));

        let row = 16 * 41;
        let solid = pixels[row + 22][0];
        assert!(
            (solid - 0.25).abs() < 2.0e-6,
            "inside the feather the lens is fully applied, got {solid}"
        );
        let ramp = pixels[row + 24][0];
        assert!(
            (ramp - 0.375).abs() < 2.0e-6,
            "the feather midpoint must composite at weight 0.5, got {ramp}"
        );
        let outside = pixels[row + 28][0];
        assert!(
            (outside - 0.5).abs() < 2.0e-6,
            "beyond the rim the frame must be untouched, got {outside}"
        );
    }

    #[test]
    fn link_modes_change_the_lens_diameter() {
        let factors = [
            (MagnifyLink::Independent, 100.0_f32, 50.0_f32),
            (MagnifyLink::FollowMagnification, 200.0, 100.0),
            (MagnifyLink::InverseMagnification, 50.0, 25.0),
        ];
        for (link, expected_diameter, expected_radius) in factors {
            let diameter = link.diameter_for(100.0, 2.0);
            assert_eq!(diameter, expected_diameter, "link {}", link.key());
            let mut pixels = frame(257, 257);
            let before = pixels.clone();
            let lens = grade(
                [128.0, 128.0],
                diameter,
                2.0,
                0.0,
                1.0,
                MagnifyShape::Circle,
                MagnifyScaling::Bilinear,
                BlendMode::Normal,
            );
            assert_eq!(lens.radius_pixels(), expected_radius);
            assert!(magnify_frame_f32(&mut pixels, 257, 257, lens));

            let row = 128 * 257;
            let inside = (expected_radius as usize) - 1;
            assert_ne!(
                pixels[row + 128 + inside],
                before[row + 128 + inside],
                "link {} must magnify just inside its {expected_radius} px rim",
                link.key()
            );
            let outside = (expected_radius as usize) + 1;
            assert_eq!(
                pixels[row + 128 + outside],
                before[row + 128 + outside],
                "link {} must leave the frame untouched beyond {expected_radius} px",
                link.key()
            );
        }
    }

    #[test]
    fn supersampling_averages_the_footprint() {
        let mut samples = Vec::new();
        for scaling in [
            MagnifyScaling::Bilinear,
            MagnifyScaling::Supersample2,
            MagnifyScaling::Supersample4,
        ] {
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; 21 * 21];
            // The impulse sits one pixel off the lens centre, so the probed
            // output pixel stays transparent and only carries the footprint.
            pixels[10 * 21 + 11] = [1.0, 1.0, 1.0, 1.0];
            let lens = grade(
                [10.0, 10.0],
                20.0,
                2.0,
                0.0,
                1.0,
                MagnifyShape::Circle,
                scaling,
                BlendMode::Normal,
            );
            assert!(magnify_frame_f32(&mut pixels, 21, 21, lens));
            samples.push(pixels[10 * 21 + 12]);
        }

        assert!(
            (samples[0][3] - 1.0).abs() < 1.0e-6,
            "a bilinear lens samples the impulse exactly, got {}",
            samples[0][3]
        );
        for (index, sample) in samples.iter().enumerate().skip(1) {
            assert!(
                (sample[3] - 0.765_625).abs() < 1.0e-6,
                "supersample {index} must average the sub-pixel footprint, got {}",
                sample[3]
            );
            assert!(
                (sample[0] - 1.0).abs() < 1.0e-6,
                "the averaged footprint must stay white, got {}",
                sample[0]
            );
        }
    }

    #[test]
    fn transparent_neighbours_do_not_stain_visible_pixels() {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; 16 * 16];
        for y in 6..10 {
            for x in 6..10 {
                pixels[y * 16 + x] = [1.0, 1.0, 1.0, 1.0];
            }
        }
        let lens = grade(
            [8.0, 8.0],
            8.0,
            4.0,
            0.0,
            1.0,
            MagnifyShape::Circle,
            MagnifyScaling::Bilinear,
            BlendMode::Normal,
        );
        assert!(magnify_frame_f32(&mut pixels, 16, 16, lens));

        for pixel in &pixels {
            if pixel[3] > 1.0e-3 {
                assert!(
                    (pixel[0] - 1.0).abs() < 1.0e-5
                        && (pixel[1] - 1.0).abs() < 1.0e-5
                        && (pixel[2] - 1.0).abs() < 1.0e-5,
                    "a transparent neighbour must not darken a visible pixel, got {pixel:?}"
                );
            }
        }
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let mut pixels = vec![[4.0_f32, -0.5, 2.5, 1.0]; 5 * 5];
        let lens = grade(
            [2.0, 2.0],
            4.0,
            2.0,
            0.0,
            1.0,
            MagnifyShape::Circle,
            MagnifyScaling::Bilinear,
            BlendMode::Normal,
        );
        assert!(magnify_frame_f32(&mut pixels, 5, 5, lens));
        assert_eq!(pixels[2 * 5 + 2], [4.0, -0.5, 2.5, 1.0]);
    }

    #[test]
    fn frames_that_do_not_match_the_declared_size_fail() {
        let lens = grade(
            [2.0, 2.0],
            2.0,
            2.0,
            0.0,
            1.0,
            MagnifyShape::Circle,
            MagnifyScaling::Bilinear,
            BlendMode::Normal,
        );
        let mut pixels = frame(4, 4);
        assert!(!magnify_frame_f32(&mut pixels[..15], 4, 4, lens));
        assert!(!magnify_frame_f32(&mut pixels, 0, 4, lens));
        assert!(!magnify_frame_f32(&mut pixels, 4, 0, lens));
    }

    #[test]
    fn the_frame_entry_point_is_deterministic() {
        let mut first = frame(37, 23);
        let mut second = first.clone();
        let lens = grade(
            [18.0, 11.0],
            14.0,
            3.0,
            5.0,
            0.75,
            MagnifyShape::Square,
            MagnifyScaling::Supersample2,
            BlendMode::Screen,
        );
        assert!(magnify_frame_f32(&mut first, 37, 23, lens));
        assert!(magnify_frame_f32(&mut second, 37, 23, lens));
        assert_eq!(first, second);
    }

    #[test]
    fn gpu_controls_carry_the_authored_parameters() {
        let lens = grade(
            [12.5, 30.0],
            40.0,
            2.5,
            6.0,
            0.5,
            MagnifyShape::Square,
            MagnifyScaling::Supersample2,
            BlendMode::Screen,
        );
        let (params, extra, aces, extra1) = lens.gpu_controls();
        assert_eq!(params, [12.5, 30.0, 20.0, 2.5]);
        assert_eq!(extra, [6.0, 0.5, 1.0, 2.0]);
        assert_eq!(
            aces,
            [3.0, 0.0, 0.0, 0.0],
            "aces.x must carry the engine blend index, not the menu index"
        );
        assert_eq!(extra1, [0.0; 4]);
    }

    #[test]
    fn rejects_author_state_outside_the_declared_ranges() {
        let base = ([10.0_f32, 20.0_f32], 100.0_f32, 2.0_f32, 0.0_f32, 1.0_f32);
        let build = |centre: [f32; 2],
                     diameter: f32,
                     magnification: f32,
                     feather: f32,
                     opacity: f32,
                     blend_mode: BlendMode| {
            MagnifyGrade::new(
                centre,
                diameter,
                magnification,
                feather,
                opacity,
                MagnifyShape::Circle,
                MagnifyScaling::Bilinear,
                blend_mode,
            )
        };
        // The grade only sees the diameter resolved through `MagnifyLink`, so the
        // authored `1..=4000` size bound is enforced by the property layer and the
        // graph builder; the bound under test here is the resolved one.
        for (diameter, magnification, feather, opacity) in [
            (0.0, 2.0, 0.0, 1.0),
            (MAGNIFY_MAX_RESOLVED_DIAMETER_PIXELS * 2.0, 2.0, 0.0, 1.0),
            (100.0, 0.99, 0.0, 1.0),
            (100.0, 200.01, 0.0, 1.0),
            (100.0, 2.0, -0.1, 1.0),
            (100.0, 2.0, 0.0, 1.01),
        ] {
            assert!(
                build(
                    base.0,
                    diameter,
                    magnification,
                    feather,
                    opacity,
                    BlendMode::Normal
                )
                .is_err(),
                "diameter {diameter}, magnification {magnification}, feather {feather}, \
                 opacity {opacity} must be rejected"
            );
        }
        assert_eq!(
            build(
                [f32::NAN, 20.0],
                base.1,
                base.2,
                base.3,
                base.4,
                BlendMode::Normal
            ),
            Err(MagnifyError::NonFinite)
        );
        assert_eq!(
            build(
                [10.0, f32::INFINITY],
                base.1,
                base.2,
                base.3,
                base.4,
                BlendMode::Normal
            ),
            Err(MagnifyError::NonFinite)
        );
        assert_eq!(
            build(base.0, base.1, base.2, base.3, base.4, BlendMode::Dissolve),
            Err(MagnifyError::BlendMode),
            "dissolve has no lens meaning and is not a declared option"
        );
    }
}
