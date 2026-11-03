//! Eyedropper fill: floods the frame with a statistic of one sample window.
//!
//! The effect list names this entry `吸管填充` (eyedropper fill) and gives it a
//! sample point, a sample radius, an average mode, an alpha toggle and a mix
//! amount. The kernel reads a square window of its own input frame around the
//! sample point, reduces it to one colour, and paints that colour over the whole
//! frame.
//!
//! # Frame
//!
//! The sample reads the effect's input frame, so the colours it sees already
//! include every upstream effect in the chain; it never reads the layer below.
//! The window is an axis-aligned square with half-side `sample_radius`:
//! `x0 = ceil(cx - radius)` and `x1 = floor(cx + radius)` around the sample point
//! `cx = clamp(sample_x, 0, width - 1)`. The window is then cropped to the frame,
//! so the sample never wraps around an edge and never invents coverage outside
//! the source.
//!
//! # Statistics
//!
//! * `mean` returns `sum(alpha_i * rgb_i) / sum(alpha_i)` over the window.
//! * `median` takes the per-channel median of the samples with positive coverage,
//!   using the lower median `(m - 1) / 2` for an even sample count.
//! * `mode` counts eight-bit quantised colour buckets and returns the straight
//!   mean colour of the largest bucket; the earliest bucket wins a tie.
//! * `luminance` returns the coverage-weighted luma of the working colour space,
//!   replicated over the three channels.
//!
//! A window without positive coverage (`sum(alpha_i) = 0`) leaves the frame
//! untouched: filling black would invent coverage that the source never had.
//!
//! # Dimensions
//!
//! `sample_radius` only ever enters the window bounds as `±radius`; it is never
//! multiplied by a frame dimension. The only product with the frame size is the
//! pixel index `row * width + column`. Both window bounds are asserted inside the
//! frame while debug assertions are enabled.
//!
//! # Alpha
//!
//! The fill is composited in premultiplied space as
//! `out = fill * (1 - blend) + source * blend` with
//! `fill = (colour * fill_alpha, fill_alpha)`. `fill_alpha` is one, or the
//! pixel's own coverage when `preserve_alpha` is on, so the kernel may turn a
//! transparent pixel opaque: that is why the effect declares a coverage-operation
//! alpha contract. The frame is premultiplied and unpremultiplied exactly once.
//!
//! Every output pixel reads the same window, so there is no row-band parallelism
//! to exploit here: the kernel is a plain serial loop that checkpoints every
//! [`CHECKPOINT_STRIDE`] output pixels.

use mondrian_core::WorkingColorSpace;

use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Smallest sample radius the author surface can express, in pixels.
pub const EYEDROPPER_MIN_RADIUS: f32 = 0.5;
/// Largest sample radius the author surface can express, in pixels.
pub const EYEDROPPER_MAX_RADIUS: f32 = 200.0;
/// Largest absolute sample coordinate the author surface accepts, in pixels.
pub const EYEDROPPER_MAX_COORDINATE: f32 = 1_000_000.0;

/// Blends this close to one leave the frame untouched.
const BLEND_EPSILON: f32 = 1.0e-6;
/// Output pixels written per cancellation checkpoint.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Failures raised when validating an eyedropper fill grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EyedropperFillError {
    /// The sample radius is not finite or lies outside the declared range.
    Radius,
    /// A sample coordinate is not finite or lies outside the declared range.
    Point,
    /// The average mode key is not a declared option.
    Mode,
    /// The blend amount is not finite or lies outside the declared range.
    Blend,
}

impl std::fmt::Display for EyedropperFillError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Radius => formatter.write_str("eyedropper sample radius is outside the range"),
            Self::Point => formatter.write_str("eyedropper sample point is outside the range"),
            Self::Mode => formatter.write_str("eyedropper average mode is not a declared option"),
            Self::Blend => formatter.write_str("eyedropper blend amount is outside the range"),
        }
    }
}

impl std::error::Error for EyedropperFillError {}

/// The four window statistics, in authoring menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AverageMode {
    /// Coverage-weighted arithmetic mean.
    Mean,
    /// Per-channel median of the samples with positive coverage.
    Median,
    /// Most frequent eight-bit quantised colour.
    Mode,
    /// Coverage-weighted luma of the working colour space.
    Luminance,
}

impl AverageMode {
    /// Every mode in authoring menu order.
    pub const ALL: [Self; 4] = [Self::Mean, Self::Median, Self::Mode, Self::Luminance];

    /// Stable persistence key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Mean => "mean",
            Self::Median => "median",
            Self::Mode => "mode",
            Self::Luminance => "luminance",
        }
    }

    /// Parses a persistence key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "mean" => Some(Self::Mean),
            "median" => Some(Self::Median),
            "mode" => Some(Self::Mode),
            "luminance" => Some(Self::Luminance),
            _ => None,
        }
    }

    /// Zero-based value used by menus and hashes.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Mean => 0,
            Self::Median => 1,
            Self::Mode => 2,
            Self::Luminance => 3,
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
            1 => Some(Self::Mean),
            2 => Some(Self::Median),
            3 => Some(Self::Mode),
            4 => Some(Self::Luminance),
            _ => None,
        }
    }

    /// Localized menu label used by the schema fallback.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mean => "均值",
            Self::Median => "中位数",
            Self::Mode => "众数",
            Self::Luminance => "亮度均值",
        }
    }
}

/// A validated eyedropper fill grade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyedropperFillGrade {
    sample: [f32; 2],
    radius: f32,
    mode: AverageMode,
    preserve_alpha: bool,
    blend_with_original: f32,
    luminance_coefficients: [f32; 3],
}

impl EyedropperFillGrade {
    /// Validates the authored parameters against the default working space.
    pub fn new(
        sample: [f32; 2],
        radius: f32,
        mode: AverageMode,
        preserve_alpha: bool,
        blend_with_original: f32,
    ) -> Result<Self, EyedropperFillError> {
        Self::for_space(
            sample,
            radius,
            mode,
            preserve_alpha,
            blend_with_original,
            WorkingColorSpace::default(),
        )
    }

    /// Validates the authored parameters against an explicit working space.
    pub fn for_space(
        sample: [f32; 2],
        radius: f32,
        mode: AverageMode,
        preserve_alpha: bool,
        blend_with_original: f32,
        space: WorkingColorSpace,
    ) -> Result<Self, EyedropperFillError> {
        validate_numbers(sample, radius, blend_with_original)?;
        Ok(Self {
            sample,
            radius,
            mode,
            preserve_alpha,
            blend_with_original,
            luminance_coefficients: space.luminance_coefficients(),
        })
    }

    /// Validates an authored mode key alongside the numeric parameters.
    ///
    /// The mode key is checked after the numeric parameters so an out-of-range
    /// radius or point is reported first, like the other builtin grades.
    pub fn from_authored(
        sample: [f32; 2],
        radius: f32,
        mode_key: &str,
        preserve_alpha: bool,
        blend_with_original: f32,
        space: WorkingColorSpace,
    ) -> Result<Self, EyedropperFillError> {
        validate_numbers(sample, radius, blend_with_original)?;
        let mode = AverageMode::from_key(mode_key).ok_or(EyedropperFillError::Mode)?;
        Ok(Self {
            sample,
            radius,
            mode,
            preserve_alpha,
            blend_with_original,
            luminance_coefficients: space.luminance_coefficients(),
        })
    }

    /// Sample point in pixel coordinates.
    #[must_use]
    pub fn sample(&self) -> [f32; 2] {
        self.sample
    }

    /// Sample radius in pixels.
    #[must_use]
    pub fn radius(&self) -> f32 {
        self.radius
    }

    /// Window statistic used for the fill colour.
    #[must_use]
    pub fn mode(&self) -> AverageMode {
        self.mode
    }

    /// Whether the fill keeps each pixel's own coverage.
    #[must_use]
    pub fn preserve_alpha(&self) -> bool {
        self.preserve_alpha
    }

    /// Mix amount towards the original frame, `0` = pure fill, `1` = source.
    #[must_use]
    pub fn blend_with_original(&self) -> f32 {
        self.blend_with_original
    }

    /// Luminance coefficients of the working colour space.
    #[must_use]
    pub fn luminance_coefficients(&self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// Whether the grade leaves the frame untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.blend_with_original >= 1.0 - BLEND_EPSILON
    }

    /// The sample window cropped to the frame.
    #[must_use]
    fn window(&self, width: usize, height: usize) -> Option<Window> {
        if width == 0 || height == 0 {
            return None;
        }
        let max_x = (width - 1) as f64;
        let max_y = (height - 1) as f64;
        // Clamp the sample point into the frame first, then crop the square
        // window: no wrap and no invented coverage outside the source.
        let centre_x = f64::from(self.sample[0]).clamp(0.0, max_x);
        let centre_y = f64::from(self.sample[1]).clamp(0.0, max_y);
        let radius = f64::from(self.radius);
        let x0 = (centre_x - radius).ceil().clamp(0.0, max_x) as usize;
        let x1 = (centre_x + radius).floor().clamp(0.0, max_x) as usize;
        let y0 = (centre_y - radius).ceil().clamp(0.0, max_y) as usize;
        let y1 = (centre_y + radius).floor().clamp(0.0, max_y) as usize;
        debug_assert!(x0 <= x1 && y0 <= y1 && x1 < width && y1 < height);
        Some(Window { x0, x1, y0, y1 })
    }

    /// Reduces the sample window to one fill colour.
    ///
    /// The returned coverage is the window statistic `A / N`; the fill alpha is
    /// derived per output pixel from `preserve_alpha` instead.
    #[must_use]
    fn fill_colour(
        &self,
        source: &[[f32; 4]],
        width: usize,
        height: usize,
    ) -> Option<([f32; 3], f32)> {
        let window = self.window(width, height)?;
        let mut samples = Vec::with_capacity(window.area());
        let mut coverage = 0.0_f64;
        for row in window.y0..=window.y1 {
            let start = row * width;
            for column in window.x0..=window.x1 {
                let pixel = source[start + column];
                let alpha = pixel[3].clamp(0.0, 1.0);
                if alpha > 0.0 {
                    samples.push(([pixel[0], pixel[1], pixel[2]], alpha));
                    coverage += f64::from(alpha);
                }
            }
        }
        let count = window.area();
        if samples.is_empty() || coverage <= 0.0 {
            return None;
        }
        let colour = match self.mode {
            AverageMode::Mean => mean_colour(&samples, coverage),
            AverageMode::Median => median_colour(&samples),
            AverageMode::Mode => mode_colour(&samples),
            AverageMode::Luminance => {
                luminance_colour(&samples, coverage, self.luminance_coefficients)
            }
        };
        Some((colour, (coverage / count as f64) as f32))
    }

    /// Writes the fill over a premultiplied frame.
    fn apply<E>(
        &self,
        frame: &mut [[f32; 4]],
        fill: [f32; 3],
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), E> {
        let weight = self.blend_with_original;
        let keep = 1.0 - weight;
        for (index, pixel) in frame.iter_mut().enumerate() {
            if index % CHECKPOINT_STRIDE == 0 {
                checkpoint()?;
            }
            // Premultiplied alpha is the pixel's clamped straight coverage.
            let fill_alpha = if self.preserve_alpha { pixel[3] } else { 1.0 };
            let fill_red = fill[0] * fill_alpha;
            let fill_green = fill[1] * fill_alpha;
            let fill_blue = fill[2] * fill_alpha;
            *pixel = [
                fill_red * keep + pixel[0] * weight,
                fill_green * keep + pixel[1] * weight,
                fill_blue * keep + pixel[2] * weight,
                fill_alpha * keep + pixel[3] * weight,
            ];
        }
        checkpoint()
    }
}

/// Frame-space sample window, inclusive on both axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

impl Window {
    /// Number of pixels inside the window.
    fn area(&self) -> usize {
        (self.x1 - self.x0 + 1) * (self.y1 - self.y0 + 1)
    }
}

/// Validates the numeric author surface shared by every constructor.
fn validate_numbers(
    sample: [f32; 2],
    radius: f32,
    blend_with_original: f32,
) -> Result<(), EyedropperFillError> {
    for coordinate in sample {
        if !coordinate.is_finite() || coordinate.abs() > EYEDROPPER_MAX_COORDINATE {
            return Err(EyedropperFillError::Point);
        }
    }
    if !radius.is_finite() || !(EYEDROPPER_MIN_RADIUS..=EYEDROPPER_MAX_RADIUS).contains(&radius) {
        return Err(EyedropperFillError::Radius);
    }
    if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
        return Err(EyedropperFillError::Blend);
    }
    Ok(())
}

/// Coverage-weighted arithmetic mean.
fn mean_colour(samples: &[([f32; 3], f32)], coverage: f64) -> [f32; 3] {
    let mut sum = [0.0_f64; 3];
    for (rgb, alpha) in samples {
        let weight = f64::from(*alpha);
        for channel in 0..3 {
            sum[channel] += weight * f64::from(rgb[channel]);
        }
    }
    [
        (sum[0] / coverage) as f32,
        (sum[1] / coverage) as f32,
        (sum[2] / coverage) as f32,
    ]
}

/// Per-channel lower median of the samples with positive coverage.
fn median_colour(samples: &[([f32; 3], f32)]) -> [f32; 3] {
    let mut channels = [
        Vec::with_capacity(samples.len()),
        Vec::with_capacity(samples.len()),
        Vec::with_capacity(samples.len()),
    ];
    for (rgb, _) in samples {
        for channel in 0..3 {
            channels[channel].push(rgb[channel]);
        }
    }
    [
        lower_median(&mut channels[0]),
        lower_median(&mut channels[1]),
        lower_median(&mut channels[2]),
    ]
}

/// Lower median of a non-empty sample list.
fn lower_median(values: &mut [f32]) -> f32 {
    debug_assert!(!values.is_empty());
    let index = (values.len() - 1) / 2;
    let (_, median, _) = values.select_nth_unstable_by(index, f32::total_cmp);
    *median
}

/// Straight mean colour of the most frequent eight-bit bucket.
///
/// Ties keep the bucket whose first sample appears earliest in the window.
fn mode_colour(samples: &[([f32; 3], f32)]) -> [f32; 3] {
    let mut buckets: std::collections::HashMap<u32, ModeBucket> = std::collections::HashMap::new();
    for (index, (rgb, _)) in samples.iter().enumerate() {
        let key = bucket_key(*rgb);
        let bucket =
            buckets
                .entry(key)
                .or_insert(ModeBucket { count: 0, first: index, sum: [0.0; 3] });
        bucket.count += 1;
        bucket.sum[0] += f64::from(rgb[0]);
        bucket.sum[1] += f64::from(rgb[1]);
        bucket.sum[2] += f64::from(rgb[2]);
    }
    let mut best: Option<&ModeBucket> = None;
    for bucket in buckets.values() {
        best = match best {
            None => Some(bucket),
            Some(current)
                if bucket.count > current.count
                    || (bucket.count == current.count && bucket.first < current.first) =>
            {
                Some(bucket)
            }
            Some(current) => Some(current),
        };
    }
    let best = best.expect("a positive sample always fills one bucket");
    [
        (best.sum[0] / best.count as f64) as f32,
        (best.sum[1] / best.count as f64) as f32,
        (best.sum[2] / best.count as f64) as f32,
    ]
}

/// Streaming bucket state for [`mode_colour`].
struct ModeBucket {
    count: usize,
    first: usize,
    sum: [f64; 3],
}

/// Eight-bit quantised bucket key of one straight colour.
fn bucket_key(rgb: [f32; 3]) -> u32 {
    let mut key = 0_u32;
    for channel in rgb {
        let quantised = (channel.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u32;
        key = (key << 8) | quantised;
    }
    key
}

/// Coverage-weighted luma of the working colour space, replicated to rgb.
fn luminance_colour(
    samples: &[([f32; 3], f32)],
    coverage: f64,
    coefficients: [f32; 3],
) -> [f32; 3] {
    let mut luma = 0.0_f64;
    for (rgb, alpha) in samples {
        let weighted = f64::from(*alpha)
            * (f64::from(rgb[0]) * f64::from(coefficients[0])
                + f64::from(rgb[1]) * f64::from(coefficients[1])
                + f64::from(rgb[2]) * f64::from(coefficients[2]));
        luma += weighted;
    }
    let luma = (luma / coverage) as f32;
    [luma, luma, luma]
}

/// Applies an eyedropper fill grade to a whole straight-alpha frame.
pub fn eyedropper_fill_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: EyedropperFillGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_eyedropper_fill_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies an eyedropper fill grade to a frame slice with checkpointing.
pub(crate) fn apply_eyedropper_fill_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: EyedropperFillGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || expected != pixels.len() || grade.is_identity() {
        return Ok(false);
    }
    // Reduce on the straight-alpha input before the frame is premultiplied.
    let Some((fill, _window_coverage)) = grade.fill_colour(pixels, width, height) else {
        // A transparent window has no colour to invent: leave every pixel alone.
        return Ok(true);
    };
    premultiply_rgba_controlled(pixels, checkpoint)?;
    grade.apply(pixels, fill, checkpoint)?;
    unpremultiply_rgba_controlled(pixels, checkpoint)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    fn assert_pixel_close(actual: [f32; 4], expected: [f32; 4], tolerance: f32) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= tolerance,
                "channel {channel}: {actual:?} vs {expected:?}",
            );
        }
    }

    fn grade(sample: [f32; 2], radius: f32, mode: AverageMode) -> EyedropperFillGrade {
        EyedropperFillGrade::new(sample, radius, mode, false, 0.0).expect("valid grade")
    }

    #[test]
    fn a_uniform_frame_fills_with_its_only_colour_in_every_mode() {
        let source = [0.2, 0.4, 0.6, 1.0];
        let luma = f64::from(0.2_f32) * f64::from(0.2126_f32)
            + f64::from(0.4_f32) * f64::from(0.7152_f32)
            + f64::from(0.6_f32) * f64::from(0.0722_f32);
        for mode in AverageMode::ALL {
            let mut pixels = frame(8, 8, source);
            assert!(eyedropper_fill_frame_f32(
                &mut pixels,
                8,
                8,
                grade([4.0, 4.0], 1.5, mode)
            ));
            let expected = if mode == AverageMode::Luminance {
                [luma as f32, luma as f32, luma as f32, 1.0]
            } else {
                source
            };
            for pixel in &pixels {
                assert_pixel_close(*pixel, expected, 1.0e-6);
            }
        }
    }

    #[test]
    fn a_half_pixel_radius_samples_exactly_one_pixel() {
        let mut pixels = frame(5, 5, [0.0, 0.0, 0.0, 1.0]);
        pixels[2 * 5 + 3] = [0.9, 0.1, 0.5, 1.0];
        assert!(eyedropper_fill_frame_f32(
            &mut pixels,
            5,
            5,
            grade([3.0, 2.0], EYEDROPPER_MIN_RADIUS, AverageMode::Mean)
        ));
        for pixel in &pixels {
            assert_pixel_close(*pixel, [0.9, 0.1, 0.5, 1.0], 1.0e-6);
        }
    }

    #[test]
    fn an_outside_sample_point_clamps_into_the_frame() {
        let mut near = frame(6, 6, [0.0, 0.0, 0.0, 1.0]);
        near[0] = [1.0, 0.0, 0.0, 1.0];
        near[1] = [0.0, 1.0, 0.0, 1.0];
        let mut far = near.clone();
        assert!(eyedropper_fill_frame_f32(
            &mut near,
            6,
            6,
            grade([0.0, 0.0], 2.0, AverageMode::Mean)
        ));
        assert!(eyedropper_fill_frame_f32(
            &mut far,
            6,
            6,
            grade([-50.0, -50.0], 2.0, AverageMode::Mean)
        ));
        assert_eq!(near, far);

        let mut bottom = frame(6, 6, [0.0, 0.0, 0.0, 1.0]);
        bottom[5 * 6 + 5] = [0.0, 0.0, 1.0, 1.0];
        let mut beyond = bottom.clone();
        assert!(eyedropper_fill_frame_f32(
            &mut bottom,
            6,
            6,
            grade([5.0, 5.0], 1.0, AverageMode::Mean)
        ));
        assert!(eyedropper_fill_frame_f32(
            &mut beyond,
            6,
            6,
            grade([15.0, 15.0], 1.0, AverageMode::Mean)
        ));
        assert_eq!(bottom, beyond);
    }

    #[test]
    fn a_corner_window_crops_to_the_whole_frame() {
        let mut pixels = frame(4, 3, [0.0, 0.0, 0.0, 1.0]);
        for (index, pixel) in pixels.iter_mut().enumerate() {
            *pixel = [index as f32 / 12.0, 0.5, 0.25, 1.0];
        }
        let mean = pixels.iter().fold(0.0_f32, |sum, pixel| sum + pixel[0]) / 12.0;
        assert!(eyedropper_fill_frame_f32(
            &mut pixels,
            4,
            3,
            grade([0.0, 0.0], EYEDROPPER_MAX_RADIUS, AverageMode::Mean)
        ));
        for pixel in &pixels {
            assert_pixel_close(*pixel, [mean, 0.5, 0.25, 1.0], 1.0e-6);
        }
    }

    #[test]
    fn mean_is_coverage_weighted_and_can_make_the_frame_opaque() {
        let mut pixels = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 0.5],
            [0.0, 1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 1.0],
        ];
        assert!(eyedropper_fill_frame_f32(
            &mut pixels,
            2,
            2,
            grade([0.0, 0.0], 4.0, AverageMode::Mean)
        ));
        // coverage = 1 + 0.5 + 0 + 1 = 2.5
        let expected = [2.0 / 2.5, 1.0 / 2.5, 1.5 / 2.5, 1.0];
        for pixel in &pixels {
            assert_pixel_close(*pixel, expected, 1.0e-6);
        }
    }

    #[test]
    fn median_ignores_transparent_samples_and_takes_the_lower_median() {
        let mut pixels = vec![
            [0.1, 0.0, 0.0, 1.0],
            [0.5, 0.0, 0.0, 1.0],
            [0.9, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 0.0],
        ];
        assert!(eyedropper_fill_frame_f32(
            &mut pixels,
            4,
            1,
            grade([1.0, 0.0], 3.0, AverageMode::Median)
        ));
        for pixel in &pixels {
            assert_pixel_close(*pixel, [0.5, 0.0, 0.0, 1.0], 1.0e-6);
        }

        let mut even = vec![[0.1, 0.0, 0.0, 1.0], [0.9, 0.0, 0.0, 1.0]];
        assert!(eyedropper_fill_frame_f32(
            &mut even,
            1,
            2,
            grade([0.0, 0.0], 1.0, AverageMode::Median)
        ));
        for pixel in &even {
            assert_pixel_close(*pixel, [0.1, 0.0, 0.0, 1.0], 1.0e-6);
        }
    }

    #[test]
    fn mode_picks_the_largest_bucket_and_the_earliest_on_a_tie() {
        let first = [0.2, 0.0, 0.0, 1.0];
        let second = [0.8, 0.8, 0.8, 1.0];
        let mut pixels = vec![first, second, second, first];
        assert!(eyedropper_fill_frame_f32(
            &mut pixels,
            4,
            1,
            grade([0.0, 0.0], 3.0, AverageMode::Mode)
        ));
        for pixel in &pixels {
            assert_pixel_close(*pixel, first, 1.0e-6);
        }
    }

    #[test]
    fn mode_quantises_at_the_eight_bit_rounding_boundary() {
        let above = 0.5 / 255.0;
        let below = 0.49 / 255.0;
        assert_eq!(bucket_key([above, above, above]) & 0xFF, 1);
        assert_eq!(bucket_key([below, below, below]) & 0xFF, 0);
        let mut pixels = vec![
            [above, above, above, 1.0],
            [above, above, above, 1.0],
            [below, below, below, 1.0],
        ];
        assert!(eyedropper_fill_frame_f32(
            &mut pixels,
            3,
            1,
            grade([0.0, 0.0], 2.0, AverageMode::Mode)
        ));
        for pixel in &pixels {
            assert_pixel_close(*pixel, [above, above, above, 1.0], 1.0e-9);
        }
    }

    #[test]
    fn luminance_is_the_coverage_weighted_luma_of_the_working_space() {
        let coefficients =
            EyedropperFillGrade::new([0.0, 0.0], 1.0, AverageMode::Luminance, false, 0.0)
                .expect("valid grade")
                .luminance_coefficients();
        let mut pixels = vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]];
        assert!(eyedropper_fill_frame_f32(
            &mut pixels,
            2,
            1,
            grade([0.0, 0.0], 1.0, AverageMode::Luminance)
        ));
        let luma = (coefficients[0] + coefficients[1]) * 0.5;
        for pixel in &pixels {
            assert_pixel_close(*pixel, [luma, luma, luma, 1.0], 1.0e-6);
        }
    }

    #[test]
    fn a_fully_transparent_window_leaves_the_frame_untouched() {
        let mut pixels = frame(4, 4, [0.4, 0.5, 0.6, 0.0]);
        let original = pixels.clone();
        assert!(eyedropper_fill_frame_f32(
            &mut pixels,
            4,
            4,
            grade([2.0, 2.0], 1.0, AverageMode::Mean)
        ));
        assert_eq!(pixels, original);
    }

    #[test]
    fn preserve_alpha_keeps_each_output_pixels_own_coverage() {
        let mut opaque = vec![[0.5, 0.0, 0.0, 0.25], [0.1, 0.2, 0.3, 1.0]];
        let fill = {
            let mut probe = opaque.clone();
            assert!(eyedropper_fill_frame_f32(
                &mut probe,
                2,
                1,
                grade([0.0, 0.0], 1.0, AverageMode::Mean)
            ));
            probe[0]
        };
        assert!(eyedropper_fill_frame_f32(
            &mut opaque,
            2,
            1,
            grade([0.0, 0.0], 1.0, AverageMode::Mean)
        ));
        assert_pixel_close(opaque[0], [fill[0], fill[1], fill[2], 1.0], 1.0e-6);
        assert_pixel_close(opaque[1], [fill[0], fill[1], fill[2], 1.0], 1.0e-6);

        let mut preserved = vec![[0.5, 0.0, 0.0, 0.25], [0.1, 0.2, 0.3, 1.0]];
        let preserve = EyedropperFillGrade::new([0.0, 0.0], 1.0, AverageMode::Mean, true, 0.0)
            .expect("valid grade");
        assert!(eyedropper_fill_frame_f32(&mut preserved, 2, 1, preserve));
        assert_pixel_close(preserved[0], [fill[0], fill[1], fill[2], 0.25], 1.0e-6);
        assert_pixel_close(preserved[1], [fill[0], fill[1], fill[2], 1.0], 1.0e-6);
    }

    #[test]
    fn blend_with_original_interpolates_in_premultiplied_space() {
        let mut source = vec![
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let fill = [1.0, 0.0, 0.0];
        assert!(eyedropper_fill_frame_f32(
            &mut source,
            4,
            1,
            grade([0.0, 0.0], 1.0, AverageMode::Mean)
        ));
        assert_pixel_close(source[3], [fill[0], fill[1], fill[2], 1.0], 1.0e-6);

        let mut half = vec![
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let blend = EyedropperFillGrade::new([0.0, 0.0], 1.0, AverageMode::Mean, false, 0.5)
            .expect("valid grade");
        assert!(eyedropper_fill_frame_f32(&mut half, 4, 1, blend));
        assert_pixel_close(half[3], [0.5, 0.0, 0.0, 1.0], 1.0e-6);
    }

    #[test]
    fn a_full_blend_is_identity() {
        let source = frame(3, 2, [0.1, 0.2, 0.3, 0.75]);
        let mut pixels = source.clone();
        let identity = EyedropperFillGrade::new([1.0, 1.0], 1.0, AverageMode::Mean, false, 1.0)
            .expect("valid grade");
        assert!(identity.is_identity());
        assert!(!eyedropper_fill_frame_f32(&mut pixels, 3, 2, identity));
        assert_eq!(pixels, source);

        let almost =
            EyedropperFillGrade::new([1.0, 1.0], 1.0, AverageMode::Mean, false, 1.0 - 1.0e-9)
                .expect("valid grade");
        assert!(almost.is_identity());
        assert!(!eyedropper_fill_frame_f32(&mut pixels, 3, 2, almost));
        assert_eq!(pixels, source);
    }

    #[test]
    fn mismatched_dimensions_are_rejected() {
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!eyedropper_fill_frame_f32(
            &mut empty,
            0,
            0,
            grade([0.0, 0.0], 1.0, AverageMode::Mean)
        ));
        let mut short = frame(2, 2, [0.0, 0.0, 0.0, 1.0]);
        assert!(!eyedropper_fill_frame_f32(
            &mut short,
            3,
            3,
            grade([0.0, 0.0], 1.0, AverageMode::Mean)
        ));
    }

    #[test]
    fn rejects_author_values_outside_the_declared_ranges() {
        assert_eq!(
            EyedropperFillGrade::new([0.0, 0.0], 0.4, AverageMode::Mean, false, 0.0),
            Err(EyedropperFillError::Radius)
        );
        assert_eq!(
            EyedropperFillGrade::new([0.0, 0.0], 200.5, AverageMode::Mean, false, 0.0),
            Err(EyedropperFillError::Radius)
        );
        assert_eq!(
            EyedropperFillGrade::new([f32::NAN, 0.0], 1.0, AverageMode::Mean, false, 0.0),
            Err(EyedropperFillError::Point)
        );
        assert_eq!(
            EyedropperFillGrade::new(
                [EYEDROPPER_MAX_COORDINATE * 2.0, 0.0],
                1.0,
                AverageMode::Mean,
                false,
                0.0
            ),
            Err(EyedropperFillError::Point)
        );
        assert_eq!(
            EyedropperFillGrade::new([0.0, 0.0], 1.0, AverageMode::Mean, false, -0.1),
            Err(EyedropperFillError::Blend)
        );
        assert_eq!(
            EyedropperFillGrade::new([0.0, 0.0], 1.0, AverageMode::Mean, false, 1.1),
            Err(EyedropperFillError::Blend)
        );
        assert_eq!(
            EyedropperFillGrade::new([0.0, 0.0], 1.0, AverageMode::Mean, false, f32::NAN),
            Err(EyedropperFillError::Blend)
        );
        assert_eq!(
            EyedropperFillGrade::from_authored(
                [0.0, 0.0],
                1.0,
                "bogus",
                false,
                0.0,
                WorkingColorSpace::default()
            ),
            Err(EyedropperFillError::Mode)
        );
        assert!(EyedropperFillGrade::new(
            [EYEDROPPER_MAX_COORDINATE, 0.0],
            EYEDROPPER_MAX_RADIUS,
            AverageMode::Mode,
            true,
            1.0
        )
        .is_ok());
    }

    #[test]
    fn the_same_grade_is_deterministic() {
        let mut first = frame(6, 5, [0.3, 0.1, 0.7, 1.0]);
        first[0] = [0.9, 0.9, 0.1, 0.5];
        let mut second = first.clone();
        let apply = grade([2.0, 3.0], 2.0, AverageMode::Mode);
        assert!(eyedropper_fill_frame_f32(&mut first, 6, 5, apply));
        assert!(eyedropper_fill_frame_f32(&mut second, 6, 5, apply));
        assert_eq!(first, second);
    }

    #[test]
    fn the_window_is_square_and_cropped_to_the_frame() {
        let grade = grade([2.0, 2.0], 1.0, AverageMode::Mean);
        let window = grade.window(8, 8).expect("window");
        assert_eq!(window, Window { x0: 1, x1: 3, y0: 1, y1: 3 });
        assert_eq!(window.area(), 9);

        let cropped = grade.window(3, 3).expect("cropped window");
        assert_eq!(cropped, Window { x0: 1, x1: 2, y0: 1, y1: 2 });
        assert_eq!(cropped.area(), 4);
    }

    #[test]
    fn the_straight_to_premultiplied_round_trip_is_exact_for_the_fill() {
        // A fully opaque fill must land bit-exactly, without accumulating a
        // premultiply/unpremultiply round-trip error.
        let mut pixels = frame(2, 2, [0.0, 0.0, 0.0, 0.0]);
        let fill = EyedropperFillGrade::new([0.0, 0.0], 1.0, AverageMode::Mean, false, 0.0)
            .expect("valid grade");
        pixels[0] = [0.25, 0.5, 0.75, 1.0];
        let expected = pixels[0];
        assert!(eyedropper_fill_frame_f32(&mut pixels, 2, 2, fill));
        assert_pixel_close(pixels[3], expected, 0.0);
    }
}
