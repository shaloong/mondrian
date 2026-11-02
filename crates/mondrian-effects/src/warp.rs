//! Warp distortion (DST-14 in the effect list).
//!
//! The effect bends the layer along a horizontal or vertical arc and lets the
//! author stretch the picture across (and along) that arc with two distortion
//! knobs. The fifteen styles are the classic authoring presets — arc, arch,
//! bulge, shell, flag, wave, fish, rise, fisheye, inflate, squeeze and twist —
//! each of which is a cubic profile over the normalised coordinate, normalised
//! so that a bend of `100` always moves the midpoint by half the frame.
//!
//! The mapping is a pure inverse resampling — destination pixel in, source
//! pixel out — so the kernel holds no state and its result is independent of
//! the processing order. For an axis-aligned frame the across coordinate `t`
//! is first stretched by the half-cosine gain
//!
//! ```text
//! gain = 1 + (distortion / 100) * 0.5 * (1 - v^2)
//! ```
//!
//! which is `1` at the rim and `1.5` at the centre for a distortion of `100`,
//! then the along coordinate `s` receives the arc displacement
//!
//! ```text
//! shape   = (1 - v^2) * (n0 + v * (n1 + v * n2))
//! sample  = centre + (s - centre) * gain - (bend / 100) * half_extent * shape
//! ```
//!
//! with `v` the normalised along coordinate after the across gain. Every
//! profile vanishes at `v = +-1` and peaks at `1` in between, so the effect
//! joins the untouched image at the rim and `bend = 0` reduces the mapping to
//! the two separable gains.
//!
//! All geometry uses `f32` and the literal expression order of the WGSL pass,
//! so the CPU kernel and [`crate::EffectGpuSpatialPass::Warp`] agree per pixel;
//! the shared bilinear helper is the only seam that widens to `f64`.
//!
//! The kernel works on premultiplied colour in the working scene-linear space
//! and never clamps colour; coverage is capped at one by the shared
//! unpremultiply helper.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Smallest authored bend, in percent of half the frame extent.
pub const WARP_MIN_BEND: f32 = -100.0;

/// Largest authored bend, in percent of half the frame extent.
pub const WARP_MAX_BEND: f32 = 100.0;

/// Default bend, in percent of half the frame extent.
pub const WARP_DEFAULT_BEND: f32 = 50.0;

/// Smallest authored distortion, in percent.
pub const WARP_MIN_DISTORTION: f32 = -100.0;

/// Largest authored distortion, in percent.
pub const WARP_MAX_DISTORTION: f32 = 100.0;

/// Default distortion, in percent.
pub const WARP_DEFAULT_DISTORTION: f32 = 0.0;

/// Default bend style.
pub const WARP_DEFAULT_STYLE: WarpStyle = WarpStyle::Arc;

/// Default bend axis.
pub const WARP_DEFAULT_AXIS: WarpAxis = WarpAxis::Horizontal;

/// Control magnitude below which the effect provably cannot move a pixel.
pub const WARP_IDENTITY_EPSILON: f32 = 1.0e-6;

/// Pixels copied per cancellation check while staging the source frame.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Which authoring preset shapes the arc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WarpStyle {
    /// Arc: the classic symmetric bow.
    Arc,
    /// Arc Lower: the bow pushes only the lower half.
    ArcLower,
    /// Arc Upper: the bow pulls only the upper half.
    ArcUpper,
    /// Arch: a symmetrical arch across the whole frame.
    Arch,
    /// Bulge: a rounded bulge centred on the frame.
    Bulge,
    /// Shell Lower: a shell that opens downwards.
    ShellLower,
    /// Shell Upper: a shell that opens upwards.
    ShellUpper,
    /// Flag: a travelling sine-like wave.
    Flag,
    /// Wave: one full wave across the frame.
    Wave,
    /// Fish: the body of the fish curve.
    Fish,
    /// Rise: the mirrored fish curve.
    Rise,
    /// Fisheye: a lens-like fisheye curve.
    Fisheye,
    /// Inflate: a soft inflate around the centre.
    Inflate,
    /// Squeeze: a squeeze concentrated at the centre.
    Squeeze,
    /// Twist: a sheared twist profile.
    Twist,
}

impl WarpStyle {
    /// Every style in authoring menu order.
    pub const ALL: [Self; 15] = [
        Self::Arc,
        Self::ArcLower,
        Self::ArcUpper,
        Self::Arch,
        Self::Bulge,
        Self::ShellLower,
        Self::ShellUpper,
        Self::Flag,
        Self::Wave,
        Self::Fish,
        Self::Rise,
        Self::Fisheye,
        Self::Inflate,
        Self::Squeeze,
        Self::Twist,
    ];

    /// Stable persistence key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Arc => "arc",
            Self::ArcLower => "arc_lower",
            Self::ArcUpper => "arc_upper",
            Self::Arch => "arch",
            Self::Bulge => "bulge",
            Self::ShellLower => "shell_lower",
            Self::ShellUpper => "shell_upper",
            Self::Flag => "flag",
            Self::Wave => "wave",
            Self::Fish => "fish",
            Self::Rise => "rise",
            Self::Fisheye => "fisheye",
            Self::Inflate => "inflate",
            Self::Squeeze => "squeeze",
            Self::Twist => "twist",
        }
    }

    /// Parses a persistence key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "arc" => Some(Self::Arc),
            "arc_lower" => Some(Self::ArcLower),
            "arc_upper" => Some(Self::ArcUpper),
            "arch" => Some(Self::Arch),
            "bulge" => Some(Self::Bulge),
            "shell_lower" => Some(Self::ShellLower),
            "shell_upper" => Some(Self::ShellUpper),
            "flag" => Some(Self::Flag),
            "wave" => Some(Self::Wave),
            "fish" => Some(Self::Fish),
            "rise" => Some(Self::Rise),
            "fisheye" => Some(Self::Fisheye),
            "inflate" => Some(Self::Inflate),
            "squeeze" => Some(Self::Squeeze),
            "twist" => Some(Self::Twist),
            _ => None,
        }
    }

    /// Zero-based value used by uniforms and hashes.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Arc => 0,
            Self::ArcLower => 1,
            Self::ArcUpper => 2,
            Self::Arch => 3,
            Self::Bulge => 4,
            Self::ShellLower => 5,
            Self::ShellUpper => 6,
            Self::Flag => 7,
            Self::Wave => 8,
            Self::Fish => 9,
            Self::Rise => 10,
            Self::Fisheye => 11,
            Self::Inflate => 12,
            Self::Squeeze => 13,
            Self::Twist => 14,
        }
    }

    /// One-based authoring menu value.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Parses a one-based authoring menu value.
    #[must_use]
    pub fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Arc),
            2 => Some(Self::ArcLower),
            3 => Some(Self::ArcUpper),
            4 => Some(Self::Arch),
            5 => Some(Self::Bulge),
            6 => Some(Self::ShellLower),
            7 => Some(Self::ShellUpper),
            8 => Some(Self::Flag),
            9 => Some(Self::Wave),
            10 => Some(Self::Fish),
            11 => Some(Self::Rise),
            12 => Some(Self::Fisheye),
            13 => Some(Self::Inflate),
            14 => Some(Self::Squeeze),
            15 => Some(Self::Twist),
            _ => None,
        }
    }

    /// Chinese authoring label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Arc => "弧形",
            Self::ArcLower => "下弧形",
            Self::ArcUpper => "上弧形",
            Self::Arch => "拱形",
            Self::Bulge => "凸起",
            Self::ShellLower => "下壳形",
            Self::ShellUpper => "上壳形",
            Self::Flag => "旗帜",
            Self::Wave => "波浪",
            Self::Fish => "鱼形",
            Self::Rise => "上升",
            Self::Fisheye => "鱼眼",
            Self::Inflate => "膨胀",
            Self::Squeeze => "挤压",
            Self::Twist => "扭转",
        }
    }

    /// Cubic profile coefficients `(n0, n1, n2)` of `S(v)`.
    ///
    /// The profile is `(1 - v^2) * (n0 + v * (n1 + v * n2))`, scaled so that
    /// its peak magnitude is `1` over `v` in `[-1, 1]`.
    #[must_use]
    pub const fn coefficients(self) -> [f32; 3] {
        match self {
            Self::Arc => [1.0, 0.0, 0.0],
            Self::ArcLower => [0.84375, -0.84375, 0.0],
            Self::ArcUpper => [0.84375, 0.84375, 0.0],
            Self::Arch => [1.0, 0.0, 1.0],
            Self::Bulge => [1.0, 0.0, 0.5],
            Self::ShellLower => [0.8969201, -0.8969201, -0.8969201],
            Self::ShellUpper => [0.8969201, 0.8969201, -0.8969201],
            Self::Flag => [0.0, 2.598_076, 0.0],
            Self::Wave => [1.0, 0.0, -3.0],
            Self::Fish => [0.8802741, -0.6602056, 0.2200685],
            Self::Rise => [0.8802741, 0.6602056, 0.2200685],
            Self::Fisheye => [0.8888889, 0.0, 1.7777778],
            Self::Inflate => [0.9807181, 0.2451795, 0.2451795],
            Self::Squeeze => [1.0, 0.0, -2.0],
            Self::Twist => [0.4825708, 1.9302833, -0.4825708],
        }
    }
}

/// Direction the arc runs across the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WarpAxis {
    /// The arc runs left to right; content is displaced vertically.
    Horizontal,
    /// The arc runs top to bottom; content is displaced horizontally.
    Vertical,
}

impl WarpAxis {
    /// Every axis in authoring menu order.
    pub const ALL: [Self; 2] = [Self::Horizontal, Self::Vertical];

    /// Stable persistence key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }

    /// Parses a persistence key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "horizontal" => Some(Self::Horizontal),
            "vertical" => Some(Self::Vertical),
            _ => None,
        }
    }

    /// Zero-based value used by uniforms and hashes.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Horizontal => 0,
            Self::Vertical => 1,
        }
    }

    /// One-based authoring menu value.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Parses a one-based authoring menu value.
    #[must_use]
    pub fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Horizontal),
            2 => Some(Self::Vertical),
            _ => None,
        }
    }

    /// Chinese authoring label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "水平",
            Self::Vertical => "垂直",
        }
    }
}

/// Why a [`WarpGrade`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpError {
    /// The bend was `NaN` or infinite.
    NonFiniteBend,
    /// The bend was outside `WARP_MIN_BEND..=WARP_MAX_BEND`.
    BendOutOfRange,
    /// The horizontal distortion was `NaN` or infinite.
    NonFiniteHorizontalDistortion,
    /// The horizontal distortion was outside `WARP_MIN_DISTORTION..=WARP_MAX_DISTORTION`.
    HorizontalDistortionOutOfRange,
    /// The vertical distortion was `NaN` or infinite.
    NonFiniteVerticalDistortion,
    /// The vertical distortion was outside `WARP_MIN_DISTORTION..=WARP_MAX_DISTORTION`.
    VerticalDistortionOutOfRange,
}

impl std::fmt::Display for WarpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteBend => formatter.write_str("warp bend must be finite"),
            Self::BendOutOfRange => {
                formatter.write_str("warp bend must be between -100 and 100 percent")
            }
            Self::NonFiniteHorizontalDistortion => {
                formatter.write_str("warp horizontal distortion must be finite")
            }
            Self::HorizontalDistortionOutOfRange => formatter
                .write_str("warp horizontal distortion must be between -100 and 100 percent"),
            Self::NonFiniteVerticalDistortion => {
                formatter.write_str("warp vertical distortion must be finite")
            }
            Self::VerticalDistortionOutOfRange => {
                formatter.write_str("warp vertical distortion must be between -100 and 100 percent")
            }
        }
    }
}

impl std::error::Error for WarpError {}

/// Validated, ready-to-render warp parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarpGrade {
    bend: f32,
    horizontal_distortion: f32,
    vertical_distortion: f32,
    style: WarpStyle,
    axis: WarpAxis,
}

impl WarpGrade {
    /// Validates and packages the authored warp parameters.
    ///
    /// # Errors
    ///
    /// Returns [`WarpError`] when a control is not finite or outside the
    /// documented authoring range.
    pub fn new(
        bend: f32,
        horizontal_distortion: f32,
        vertical_distortion: f32,
        style: WarpStyle,
        axis: WarpAxis,
    ) -> Result<Self, WarpError> {
        if !bend.is_finite() {
            return Err(WarpError::NonFiniteBend);
        }
        if !(WARP_MIN_BEND..=WARP_MAX_BEND).contains(&bend) {
            return Err(WarpError::BendOutOfRange);
        }
        if !horizontal_distortion.is_finite() {
            return Err(WarpError::NonFiniteHorizontalDistortion);
        }
        if !(WARP_MIN_DISTORTION..=WARP_MAX_DISTORTION).contains(&horizontal_distortion) {
            return Err(WarpError::HorizontalDistortionOutOfRange);
        }
        if !vertical_distortion.is_finite() {
            return Err(WarpError::NonFiniteVerticalDistortion);
        }
        if !(WARP_MIN_DISTORTION..=WARP_MAX_DISTORTION).contains(&vertical_distortion) {
            return Err(WarpError::VerticalDistortionOutOfRange);
        }
        Ok(Self {
            bend,
            horizontal_distortion,
            vertical_distortion,
            style,
            axis,
        })
    }

    /// Authored bend, in percent of half the frame extent.
    #[must_use]
    pub const fn bend(self) -> f32 {
        self.bend
    }

    /// Authored horizontal distortion, in percent.
    #[must_use]
    pub const fn horizontal_distortion(self) -> f32 {
        self.horizontal_distortion
    }

    /// Authored vertical distortion, in percent.
    #[must_use]
    pub const fn vertical_distortion(self) -> f32 {
        self.vertical_distortion
    }

    /// Authored bend style.
    #[must_use]
    pub const fn style(self) -> WarpStyle {
        self.style
    }

    /// Authored bend axis.
    #[must_use]
    pub const fn axis(self) -> WarpAxis {
        self.axis
    }

    /// Whether the grade provably cannot move a pixel.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.bend.abs() <= WARP_IDENTITY_EPSILON
            && self.horizontal_distortion.abs() <= WARP_IDENTITY_EPSILON
            && self.vertical_distortion.abs() <= WARP_IDENTITY_EPSILON
    }

    /// Uniform cells consumed by the GPU pass.
    ///
    /// `params` is `[bend, horizontal_distortion, vertical_distortion, style]`
    /// and `extra` is `[axis, 0, 0, 0]`; both enums travel as their zero-based
    /// uniform values.
    #[must_use]
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.bend,
                self.horizontal_distortion,
                self.vertical_distortion,
                self.style.as_u32() as f32,
            ],
            [self.axis.as_u32() as f32, 0.0, 0.0, 0.0],
        )
    }

    /// Source position for a destination pixel.
    ///
    /// The expression order is part of the contract: the GPU pass evaluates
    /// the same operations in the same order so both sides agree per pixel.
    #[must_use]
    pub fn sample_position(
        self,
        column: usize,
        row: usize,
        width: usize,
        height: usize,
    ) -> [f32; 2] {
        let column_pixels = column as f32;
        let row_pixels = row as f32;
        if self.is_identity() {
            return [column_pixels, row_pixels];
        }
        let centre_x = (width as f32 - 1.0) * 0.5;
        let centre_y = (height as f32 - 1.0) * 0.5;
        let swap = self.axis == WarpAxis::Vertical;
        let (s, t) = if swap {
            (column_pixels, row_pixels)
        } else {
            (row_pixels, column_pixels)
        };
        let (centre_s, centre_t) = if swap {
            (centre_x, centre_y)
        } else {
            (centre_y, centre_x)
        };
        let (distortion_t, distortion_s) = if swap {
            (self.vertical_distortion, self.horizontal_distortion)
        } else {
            (self.horizontal_distortion, self.vertical_distortion)
        };
        // Half extents in pixels: `hs` is the along axis, `ht` the across one.
        let (half_s, half_t) = (centre_s, centre_t);
        if half_s <= 0.0 || half_t <= 0.0 {
            return [column_pixels, row_pixels];
        }
        let normalised_s = (s - centre_s) / half_s;
        debug_assert!(normalised_s.abs() <= 1.0 + 1.0e-4);
        let gain_t = 1.0 + (distortion_t / 100.0) * 0.5 * (1.0 - normalised_s * normalised_s);
        debug_assert!((0.5..=1.5).contains(&gain_t));
        let warped_t = centre_t + (t - centre_t) * gain_t;
        let normalised_t = (warped_t - centre_t) / half_t;
        debug_assert!(normalised_t.abs() <= 1.5 + 1.0e-4);
        let gain_s = 1.0 + (distortion_s / 100.0) * 0.5 * (1.0 - normalised_t * normalised_t);
        debug_assert!((0.375..=1.625).contains(&gain_s));
        let [n0, n1, n2] = self.style.coefficients();
        let weight = 1.0 - normalised_t * normalised_t;
        let shape = weight * (n0 + normalised_t * (n1 + normalised_t * n2));
        let warped_s = centre_s + (s - centre_s) * gain_s - ((self.bend / 100.0) * half_s) * shape;
        // The along-axis gain moves a pixel by at most 0.625 half-extents and
        // the widest profile (`wave` at |vt| = 1.5) contributes 7.1875 more, so
        // the total displacement stays inside eight half-extents.
        debug_assert!((warped_s - s).abs() <= 8.0 * half_s);
        if swap {
            [warped_s, warped_t]
        } else {
            [warped_t, warped_s]
        }
    }
}

/// Applies a warp to a CPU frame, discarding cancellation results.
///
/// Returns `false` when the buffers do not describe a renderable frame.
#[must_use]
pub fn warp_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: WarpGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_warp_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies a warp to a CPU frame with cooperative cancellation.
///
/// The frame is premultiplied once, remapped from a staged copy and
/// unpremultiplied once, so transparent neighbours contribute no colour and
/// the result stays in the working scene-linear space. Returns `false` when
/// the buffer does not describe a renderable frame.
///
/// # Errors
///
/// Propagates whatever `checkpoint` returns.
pub(crate) fn apply_warp_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: WarpGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let frame = &mut working[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;
    let mut source = Vec::with_capacity(pixel_count);
    for chunk in frame.chunks(CHECKPOINT_STRIDE) {
        checkpoint()?;
        source.extend_from_slice(chunk);
    }
    crate::parallel::for_each_row_band(frame, width, 0..height, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let [source_x, source_y] = grade.sample_position(column, row, width, height);
                if source_x == column as f32 && source_y == row as f32 {
                    continue;
                }
                *pixel = sample_bilinear_clamped(
                    &source,
                    width,
                    height,
                    f64::from(source_x),
                    f64::from(source_y),
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
    use super::{
        apply_warp_f32_controlled, warp_frame_f32, WarpAxis, WarpError, WarpGrade, WarpStyle,
        WARP_DEFAULT_AXIS, WARP_DEFAULT_BEND, WARP_DEFAULT_DISTORTION, WARP_DEFAULT_STYLE,
        WARP_IDENTITY_EPSILON, WARP_MAX_BEND, WARP_MAX_DISTORTION, WARP_MIN_BEND,
        WARP_MIN_DISTORTION,
    };

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                let value = 0.04 * (column + 2 * row) as f32;
                pixels.push([value, 0.5 - value * 0.2, value * 1.4, 1.0]);
            }
        }
        pixels
    }

    fn bits(pixel: [f32; 4]) -> [u32; 4] {
        pixel.map(f32::to_bits)
    }

    fn grade(
        bend: f32,
        horizontal_distortion: f32,
        vertical_distortion: f32,
        style: WarpStyle,
        axis: WarpAxis,
    ) -> WarpGrade {
        WarpGrade::new(
            bend,
            horizontal_distortion,
            vertical_distortion,
            style,
            axis,
        )
        .expect("valid warp grade")
    }

    /// Independent `f64` evaluation of the documented mapping.
    #[allow(clippy::too_many_arguments)]
    fn reference_position(
        bend: f32,
        horizontal_distortion: f32,
        vertical_distortion: f32,
        style: WarpStyle,
        axis: WarpAxis,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> [f64; 2] {
        let centre_x = (width as f64 - 1.0) * 0.5;
        let centre_y = (height as f64 - 1.0) * 0.5;
        let swap = axis == WarpAxis::Vertical;
        let (s, t) = if swap { (column, row) } else { (row, column) };
        let (centre_s, centre_t) = if swap {
            (centre_x, centre_y)
        } else {
            (centre_y, centre_x)
        };
        let (distortion_t, distortion_s) = if swap {
            (vertical_distortion, horizontal_distortion)
        } else {
            (horizontal_distortion, vertical_distortion)
        };
        let normalised_s = (s - centre_s) / centre_s;
        let gain_t = 1.0 + (f64::from(distortion_t) / 100.0) * 0.5 * (1.0 - normalised_s.powi(2));
        let warped_t = centre_t + (t - centre_t) * gain_t;
        let normalised_t = (warped_t - centre_t) / centre_t;
        let gain_s = 1.0 + (f64::from(distortion_s) / 100.0) * 0.5 * (1.0 - normalised_t.powi(2));
        let [n0, n1, n2] = style.coefficients().map(f64::from);
        let weight = 1.0 - normalised_t.powi(2);
        let shape = weight * (n0 + normalised_t * (n1 + normalised_t * n2));
        let warped_s =
            centre_s + (s - centre_s) * gain_s - ((f64::from(bend) / 100.0) * centre_s) * shape;
        if swap {
            [warped_s, warped_t]
        } else {
            [warped_t, warped_s]
        }
    }

    #[test]
    fn identity_grade_leaves_every_pixel_bit_identical() {
        let (width, height) = (9, 7);
        let original = frame(width, height);
        let mut pixels = original.clone();
        let applied = grade(0.0, 0.0, 0.0, WarpStyle::Twist, WarpAxis::Vertical);
        assert!(applied.is_identity());
        assert!(warp_frame_f32(&mut pixels, width, height, applied));
        for (index, (before, after)) in original.iter().zip(pixels.iter()).enumerate() {
            assert_eq!(bits(*before), bits(*after), "pixel {index} changed");
        }
    }

    #[test]
    fn zero_bend_never_moves_a_pixel_across_the_axis() {
        let (width, height) = (9, 7);
        let applied = grade(0.0, 80.0, 0.0, WarpStyle::Arc, WarpAxis::Horizontal);
        for row in 0..height {
            for column in 0..width {
                let [_, sample_y] = applied.sample_position(column, row, width, height);
                assert_eq!(
                    sample_y.to_bits(),
                    (row as f32).to_bits(),
                    "column {column} row {row} left its row"
                );
            }
        }
    }

    #[test]
    fn bend_is_monotonic_in_the_centre_displacement() {
        let (width, height) = (13, 11);
        let centre = (6, 5);
        let half = (height as f32 - 1.0) * 0.5;
        let mut previous = f32::INFINITY;
        for step in -10..=10 {
            let bend = step as f32 * 10.0;
            let applied = grade(bend, 0.0, 0.0, WarpStyle::Arc, WarpAxis::Horizontal);
            let [sample_x, sample_y] = applied.sample_position(centre.0, centre.1, width, height);
            assert_eq!(
                sample_x.to_bits(),
                (centre.0 as f32).to_bits(),
                "bend {bend} moved the centre sideways"
            );
            assert!(sample_y < previous, "bend {bend} was not monotonic");
            previous = sample_y;
            if bend == WARP_MAX_BEND {
                assert_eq!(
                    (sample_y - centre.1 as f32).abs(),
                    half,
                    "bend {bend} did not move the centre by one half extent"
                );
            }
        }
    }

    #[test]
    fn distortion_only_keeps_the_centre_fixed() {
        let (width, height) = (13, 11);
        let centre = [6_usize, 5_usize];
        for horizontal_distortion in [-100.0_f32, 100.0] {
            for vertical_distortion in [-100.0_f32, 100.0] {
                let applied = grade(
                    0.0,
                    horizontal_distortion,
                    vertical_distortion,
                    WarpStyle::Wave,
                    WarpAxis::Horizontal,
                );
                let sample = applied.sample_position(centre[0], centre[1], width, height);
                assert_eq!(
                    sample.map(f32::to_bits),
                    [centre[0] as f32, centre[1] as f32].map(f32::to_bits),
                    "distortions {horizontal_distortion} {vertical_distortion} moved the centre"
                );
            }
        }
    }

    #[test]
    fn the_axis_swaps_the_mapping() {
        let (width, height) = (11, 11);
        let horizontal = grade(60.0, 40.0, 40.0, WarpStyle::Fish, WarpAxis::Horizontal);
        let vertical = grade(60.0, 40.0, 40.0, WarpStyle::Fish, WarpAxis::Vertical);
        for row in 0..height {
            for column in 0..width {
                let swapped = vertical.sample_position(column, row, width, height);
                let [sample_x, sample_y] = horizontal.sample_position(row, column, width, height);
                assert_eq!(
                    swapped.map(f32::to_bits),
                    [sample_y, sample_x].map(f32::to_bits),
                    "column {column} row {row} did not swap"
                );
            }
        }
    }

    #[test]
    fn tiny_frames_are_the_identity() {
        let applied = grade(50.0, 100.0, -100.0, WarpStyle::Flag, WarpAxis::Horizontal);
        for (width, height) in [(1_usize, 1_usize), (2, 1), (1, 2), (2, 2)] {
            let original = frame(width, height);
            let mut pixels = original.clone();
            assert!(
                warp_frame_f32(&mut pixels, width, height, applied),
                "size {width}x{height} was rejected"
            );
            for (index, (before, after)) in original.iter().zip(pixels.iter()).enumerate() {
                assert_eq!(
                    bits(*before),
                    bits(*after),
                    "size {width}x{height} pixel {index} changed"
                );
            }
        }
    }

    #[test]
    fn the_rim_columns_are_fixed() {
        let (width, height) = (15, 9);
        let applied = grade(50.0, 0.0, 0.0, WarpStyle::Twist, WarpAxis::Horizontal);
        for row in 0..height {
            for column in [0, width - 1] {
                let [sample_x, sample_y] = applied.sample_position(column, row, width, height);
                assert_eq!(
                    (sample_x.to_bits(), sample_y.to_bits()),
                    ((column as f32).to_bits(), (row as f32).to_bits()),
                    "column {column} row {row} moved"
                );
            }
        }
    }

    #[test]
    fn a_transparent_neighbour_does_not_bleed_into_colour() {
        let (width, height) = (11, 11);
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; width * height];
        for row in 0..height {
            for column in 0..=3 {
                pixels[row * width + column] = [1.0, 0.5, 0.25, 1.0];
            }
        }
        // Horizontal distortion of 100 doubles the gain at the centre row:
        // pixel (4, 5) samples x = 5 + (4 - 5) * 1.5 = 3.5, halfway between the
        // opaque column 3 and the transparent column 4.
        let applied = grade(0.0, 100.0, 0.0, WarpStyle::Arc, WarpAxis::Horizontal);
        assert!(warp_frame_f32(&mut pixels, width, height, applied));
        let sampled = pixels[5 * width + 4];
        assert!((sampled[0] - 1.0).abs() <= 1.0e-6, "red {}", sampled[0]);
        assert!((sampled[1] - 0.5).abs() <= 1.0e-6, "green {}", sampled[1]);
        assert!((sampled[2] - 0.25).abs() <= 1.0e-6, "blue {}", sampled[2]);
        assert!(
            (sampled[3] - 0.5).abs() <= 1.0e-6,
            "alpha {} must be half coverage",
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
    fn hdr_colour_is_not_clamped() {
        let (width, height) = (7, 5);
        let mut pixels = vec![[1.0e6_f32, 1.0e6, 1.0e6, 1.0]; width * height];
        let applied = grade(40.0, 30.0, -20.0, WarpStyle::Inflate, WarpAxis::Horizontal);
        assert!(warp_frame_f32(&mut pixels, width, height, applied));
        for (index, pixel) in pixels.iter().enumerate() {
            for (channel, value) in pixel.iter().enumerate().take(3) {
                let difference = (value - 1.0e6).abs();
                assert!(
                    difference <= 1.0e6 * f32::EPSILON,
                    "pixel {index} channel {channel} was clamped: {value}",
                );
            }
        }
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let (width, height) = (13, 11);
        let original = frame(width, height);
        let applied = grade(73.0, -41.0, 17.0, WarpStyle::ShellUpper, WarpAxis::Vertical);
        let mut first = original.clone();
        let mut second = original;
        assert!(warp_frame_f32(&mut first, width, height, applied));
        assert!(warp_frame_f32(&mut second, width, height, applied));
        for (index, (left, right)) in first.iter().zip(second.iter()).enumerate() {
            assert_eq!(bits(*left), bits(*right), "pixel {index} was not stable");
        }
    }

    #[test]
    fn gpu_controls_packs_the_documented_cells() {
        let applied = grade(-25.0, 40.0, -60.0, WarpStyle::Fisheye, WarpAxis::Vertical);
        let (params, extra) = applied.gpu_controls();
        assert_eq!(
            params,
            [-25.0, 40.0, -60.0, WarpStyle::Fisheye.as_u32() as f32]
        );
        assert_eq!(extra, [WarpAxis::Vertical.as_u32() as f32, 0.0, 0.0, 0.0]);
        assert_eq!(WarpStyle::Fisheye.as_u32(), 11);
        assert_eq!(WarpAxis::Vertical.as_u32(), 1);
    }

    #[test]
    fn the_grade_rejects_non_finite_and_out_of_range_controls() {
        let ok = WarpStyle::Arc;
        let axis = WarpAxis::Horizontal;
        assert_eq!(
            WarpGrade::new(f32::NAN, 0.0, 0.0, ok, axis),
            Err(WarpError::NonFiniteBend)
        );
        assert_eq!(
            WarpGrade::new(f32::INFINITY, 0.0, 0.0, ok, axis),
            Err(WarpError::NonFiniteBend)
        );
        assert_eq!(
            WarpGrade::new(100.1, 0.0, 0.0, ok, axis),
            Err(WarpError::BendOutOfRange)
        );
        assert_eq!(
            WarpGrade::new(-100.1, 0.0, 0.0, ok, axis),
            Err(WarpError::BendOutOfRange)
        );
        assert_eq!(
            WarpGrade::new(0.0, f32::NAN, 0.0, ok, axis),
            Err(WarpError::NonFiniteHorizontalDistortion)
        );
        assert_eq!(
            WarpGrade::new(0.0, -100.1, 0.0, ok, axis),
            Err(WarpError::HorizontalDistortionOutOfRange)
        );
        assert_eq!(
            WarpGrade::new(0.0, 0.0, f32::NEG_INFINITY, ok, axis),
            Err(WarpError::NonFiniteVerticalDistortion)
        );
        assert_eq!(
            WarpGrade::new(0.0, 0.0, 100.1, ok, axis),
            Err(WarpError::VerticalDistortionOutOfRange)
        );
        assert!(WarpGrade::new(
            WARP_MIN_BEND,
            WARP_MIN_DISTORTION,
            WARP_MIN_DISTORTION,
            ok,
            axis
        )
        .is_ok());
        assert!(WarpGrade::new(
            WARP_MAX_BEND,
            WARP_MAX_DISTORTION,
            WARP_MAX_DISTORTION,
            ok,
            axis
        )
        .is_ok());
    }

    #[test]
    fn menu_and_key_round_trips() {
        for (index, style) in WarpStyle::ALL.iter().copied().enumerate() {
            assert_eq!(style.as_u32(), index as u32, "{}", style.as_key());
            assert_eq!(style.as_menu(), style.as_u32() + 1);
            assert_eq!(WarpStyle::from_menu(style.as_menu()), Some(style));
            assert_eq!(WarpStyle::from_key(style.as_key()), Some(style));
        }
        assert_eq!(WarpStyle::ALL.len(), 15);
        assert_eq!(WarpStyle::from_menu(0), None);
        assert_eq!(WarpStyle::from_menu(16), None);
        assert_eq!(WarpStyle::from_key("arc-lower"), None);
        for (index, axis) in WarpAxis::ALL.iter().copied().enumerate() {
            assert_eq!(axis.as_u32(), index as u32);
            assert_eq!(axis.as_menu(), axis.as_u32() + 1);
            assert_eq!(WarpAxis::from_menu(axis.as_menu()), Some(axis));
            assert_eq!(WarpAxis::from_key(axis.as_key()), Some(axis));
        }
        assert_eq!(WarpAxis::ALL.len(), 2);
        assert_eq!(WarpAxis::from_menu(3), None);
        assert_eq!(WarpAxis::from_key("diagonal"), None);
    }

    #[test]
    fn mismatched_frames_are_reported_as_unsupported() {
        let applied = grade(50.0, 0.0, 0.0, WarpStyle::Arc, WarpAxis::Horizontal);
        let mut short = frame(5, 5);
        short.truncate(24);
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert_eq!(
            apply_warp_f32_controlled(&mut short, 5, 5, applied, &mut checkpoint),
            Ok(false)
        );
        assert!(!warp_frame_f32(&mut short, 5, 5, applied));
        assert_eq!(short.len(), 24, "the short buffer was rewritten");
    }

    #[test]
    fn the_style_profiles_are_normalised_and_vanish_at_the_rim() {
        for style in WarpStyle::ALL {
            let [n0, n1, n2] = style.coefficients();
            let mut peak = 0.0_f32;
            for step in 0..=4096 {
                let v = -1.0 + 2.0 * step as f32 / 4096.0;
                let profile = (1.0 - v * v) * (n0 + v * (n1 + v * n2));
                peak = peak.max(profile.abs());
            }
            assert!(
                (0.9999..=1.0001).contains(&peak),
                "{} peak {peak}",
                style.as_key()
            );
            for v in [-1.0_f32, 1.0] {
                let profile = (1.0 - v * v) * (n0 + v * (n1 + v * n2));
                assert!(profile.abs() <= 1.0e-6, "{} rim {profile}", style.as_key());
            }
        }
    }

    #[test]
    fn the_mapping_matches_an_independent_evaluation() {
        let (width, height) = (23, 17);
        let style = WarpStyle::Rise;
        let applied = grade(72.0, -38.0, 61.0, style, WarpAxis::Horizontal);
        for index in 0..20 {
            let column = (index * 7 + 3) % width;
            let row = (index * 5 + 1) % height;
            let [sample_x, sample_y] = applied.sample_position(column, row, width, height);
            let [reference_x, reference_y] = reference_position(
                72.0,
                -38.0,
                61.0,
                style,
                WarpAxis::Horizontal,
                column as f64,
                row as f64,
                width,
                height,
            );
            assert!(
                (f64::from(sample_x) - reference_x).abs() <= 1.0e-5,
                "pixel ({column}, {row}) x {} != {reference_x}",
                sample_x
            );
            assert!(
                (f64::from(sample_y) - reference_y).abs() <= 1.0e-5,
                "pixel ({column}, {row}) y {} != {reference_y}",
                sample_y
            );
        }
    }

    #[test]
    fn the_jacobian_never_folds_at_the_extremes() {
        let (width, height) = (17, 13);
        let step = 0.25;
        for bend in [-100.0_f32, 100.0] {
            for horizontal_distortion in [-100.0_f32, 100.0] {
                for vertical_distortion in [-100.0_f32, 100.0] {
                    for style in [
                        WarpStyle::Arc,
                        WarpStyle::Twist,
                        WarpStyle::Flag,
                        WarpStyle::Fisheye,
                    ] {
                        let mut minimum = f64::INFINITY;
                        for row in 2..height - 2 {
                            for column in 2..width - 2 {
                                let x = column as f64;
                                let y = row as f64;
                                let right = reference_position(
                                    bend,
                                    horizontal_distortion,
                                    vertical_distortion,
                                    style,
                                    WarpAxis::Horizontal,
                                    x + step,
                                    y,
                                    width,
                                    height,
                                );
                                let left = reference_position(
                                    bend,
                                    horizontal_distortion,
                                    vertical_distortion,
                                    style,
                                    WarpAxis::Horizontal,
                                    x - step,
                                    y,
                                    width,
                                    height,
                                );
                                let up = reference_position(
                                    bend,
                                    horizontal_distortion,
                                    vertical_distortion,
                                    style,
                                    WarpAxis::Horizontal,
                                    x,
                                    y + step,
                                    width,
                                    height,
                                );
                                let down = reference_position(
                                    bend,
                                    horizontal_distortion,
                                    vertical_distortion,
                                    style,
                                    WarpAxis::Horizontal,
                                    x,
                                    y - step,
                                    width,
                                    height,
                                );
                                let d_x_d_x = (right[0] - left[0]) / (2.0 * step);
                                let d_y_d_x = (right[1] - left[1]) / (2.0 * step);
                                let d_x_d_y = (up[0] - down[0]) / (2.0 * step);
                                let d_y_d_y = (up[1] - down[1]) / (2.0 * step);
                                let determinant = d_x_d_x * d_y_d_y - d_x_d_y * d_y_d_x;
                                minimum = minimum.min(determinant);
                            }
                        }
                        assert!(
                            minimum >= 0.1875 - 1.0e-3,
                            "bend {bend} hd {horizontal_distortion} vd {vertical_distortion} \
                             style {} folded: determinant {minimum}",
                            style.as_key()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn styles_report_their_documented_coefficients() {
        let expected: [(WarpStyle, [f32; 3]); 15] = [
            (WarpStyle::Arc, [1.0, 0.0, 0.0]),
            (WarpStyle::ArcLower, [0.84375, -0.84375, 0.0]),
            (WarpStyle::ArcUpper, [0.84375, 0.84375, 0.0]),
            (WarpStyle::Arch, [1.0, 0.0, 1.0]),
            (WarpStyle::Bulge, [1.0, 0.0, 0.5]),
            (WarpStyle::ShellLower, [0.8969201, -0.8969201, -0.8969201]),
            (WarpStyle::ShellUpper, [0.8969201, 0.8969201, -0.8969201]),
            (WarpStyle::Flag, [0.0, 2.598_076, 0.0]),
            (WarpStyle::Wave, [1.0, 0.0, -3.0]),
            (WarpStyle::Fish, [0.8802741, -0.6602056, 0.2200685]),
            (WarpStyle::Rise, [0.8802741, 0.6602056, 0.2200685]),
            (WarpStyle::Fisheye, [0.8888889, 0.0, 1.7777778]),
            (WarpStyle::Inflate, [0.9807181, 0.2451795, 0.2451795]),
            (WarpStyle::Squeeze, [1.0, 0.0, -2.0]),
            (WarpStyle::Twist, [0.4825708, 1.9302833, -0.4825708]),
        ];
        for (style, coefficients) in expected {
            assert_eq!(style.coefficients(), coefficients, "{}", style.as_key());
        }
    }

    #[test]
    fn the_defaults_match_the_effect_list() {
        assert_eq!(WARP_MIN_BEND, -100.0);
        assert_eq!(WARP_MAX_BEND, 100.0);
        assert_eq!(WARP_DEFAULT_BEND, 50.0);
        assert_eq!(WARP_MIN_DISTORTION, -100.0);
        assert_eq!(WARP_MAX_DISTORTION, 100.0);
        assert_eq!(WARP_DEFAULT_DISTORTION, 0.0);
        assert_eq!(WARP_DEFAULT_STYLE, WarpStyle::Arc);
        assert_eq!(WARP_DEFAULT_AXIS, WarpAxis::Horizontal);
        assert_eq!(WARP_DEFAULT_STYLE.label(), "弧形");
        assert_eq!(WARP_DEFAULT_AXIS.label(), "水平");
        let default = grade(
            WARP_DEFAULT_BEND,
            WARP_DEFAULT_DISTORTION,
            WARP_DEFAULT_DISTORTION,
            WARP_DEFAULT_STYLE,
            WARP_DEFAULT_AXIS,
        );
        assert!(!default.is_identity());
        assert_eq!(default.bend(), WARP_DEFAULT_BEND);
        assert_eq!(default.style(), WarpStyle::Arc);
        assert_eq!(default.axis(), WarpAxis::Horizontal);
    }

    #[test]
    fn the_distortion_gain_is_symmetric_about_the_centre() {
        let (width, height) = (9, 9);
        let centre = 4_usize;
        let applied = grade(0.0, 100.0, 0.0, WarpStyle::Arc, WarpAxis::Horizontal);
        for row in 0..height {
            for column in 0..=centre {
                let mirrored = width - 1 - column;
                let left = applied.sample_position(column, row, width, height);
                let right = applied.sample_position(mirrored, row, width, height);
                assert_eq!(
                    left[1].to_bits(),
                    right[1].to_bits(),
                    "column {column} row {row} was not mirrored"
                );
                assert_eq!(
                    (left[0] + right[0]).to_bits(),
                    (2.0 * centre as f32).to_bits(),
                    "column {column} row {row} was not symmetric"
                );
            }
        }
        let middle = applied.sample_position(0, centre, width, height);
        assert_eq!(middle[0], -2.0, "the centre row must peak at 1.5 gain");
    }

    #[test]
    fn the_identity_epsilon_covers_only_sub_pixel_controls() {
        for bend in [-1.0e-7_f32, 1.0e-7] {
            assert!(grade(bend, 0.0, 0.0, WarpStyle::Arc, WarpAxis::Horizontal).is_identity());
        }
        assert!(!grade(1.0e-5, 0.0, 0.0, WarpStyle::Arc, WarpAxis::Horizontal).is_identity());
        assert!(grade(
            0.0,
            -WARP_IDENTITY_EPSILON,
            0.0,
            WarpStyle::Arc,
            WarpAxis::Horizontal
        )
        .is_identity());
        assert!(!grade(
            0.0,
            2.0 * WARP_IDENTITY_EPSILON,
            0.0,
            WarpStyle::Arc,
            WarpAxis::Horizontal
        )
        .is_identity());
        assert!(!grade(
            0.0,
            0.0,
            -2.0 * WARP_IDENTITY_EPSILON,
            WarpStyle::Arc,
            WarpAxis::Horizontal
        )
        .is_identity());
    }

    #[test]
    fn a_zero_extent_frame_is_unsupported() {
        let applied = grade(50.0, 0.0, 0.0, WarpStyle::Arc, WarpAxis::Horizontal);
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!warp_frame_f32(&mut empty, 0, 0, applied));
        assert!(!warp_frame_f32(&mut empty, 4, 0, applied));
        assert!(!warp_frame_f32(&mut empty, 0, 4, applied));
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert_eq!(
            apply_warp_f32_controlled(&mut empty, 4, 0, applied, &mut checkpoint),
            Ok(false)
        );
        assert!(
            apply_warp_f32_controlled(&mut empty, usize::MAX, 2, applied, &mut checkpoint).is_ok(),
            "an unrepresentable extent must not panic"
        );
    }

    #[test]
    fn the_vertical_axis_keeps_the_rim_rows_fixed() {
        let (width, height) = (9, 15);
        let applied = grade(50.0, 0.0, 0.0, WarpStyle::Twist, WarpAxis::Vertical);
        for column in 0..width {
            for row in [0, height - 1] {
                let [sample_x, sample_y] = applied.sample_position(column, row, width, height);
                assert_eq!(
                    (sample_x.to_bits(), sample_y.to_bits()),
                    ((column as f32).to_bits(), (row as f32).to_bits()),
                    "column {column} row {row} moved"
                );
            }
        }
    }

    #[test]
    fn the_sample_stays_finite_at_the_extremes() {
        let (width, height) = (13, 9);
        for style in WarpStyle::ALL {
            for axis in WarpAxis::ALL {
                for bend in [-100.0_f32, 100.0] {
                    for horizontal_distortion in [-100.0_f32, 100.0] {
                        for vertical_distortion in [-100.0_f32, 100.0] {
                            let applied = grade(
                                bend,
                                horizontal_distortion,
                                vertical_distortion,
                                style,
                                axis,
                            );
                            for row in 0..height {
                                for column in 0..width {
                                    let [sample_x, sample_y] =
                                        applied.sample_position(column, row, width, height);
                                    assert!(
                                        sample_x.is_finite() && sample_y.is_finite(),
                                        "{} {:?} at ({column}, {row}) sampled {sample_x}, {sample_y}",
                                        style.as_key(),
                                        axis.as_key()
                                    );
                                }
                            }
                            let mut pixels = frame(width, height);
                            assert!(warp_frame_f32(&mut pixels, width, height, applied));
                            for (index, pixel) in pixels.iter().enumerate() {
                                assert!(
                                    pixel.iter().all(|channel| channel.is_finite()),
                                    "{} {:?} pixel {index} left the reals: {pixel:?}",
                                    style.as_key(),
                                    axis.as_key()
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
