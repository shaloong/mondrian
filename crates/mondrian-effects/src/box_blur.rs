//! Separable box blur kernel for the Box Blur built-in effect.
//!
//! The working-frame seam stays straight-alpha, and the kernel blurs premultiplied
//! color so fully transparent pixels cannot bleed their color into visible ones.
//! Each pass evaluates a normalized box filter from prefix sums, which keeps the
//! per-pixel cost independent of the radius: the authored range reaches 30000
//! pixels, and a sliding-window implementation would make the cost grow with the
//! radius instead of with the image.
//!
//! A fractional radius weights the two samples just outside the integer window,
//! matching the fractional box kernel used by the Gaussian blur, so a radius of
//! `1.5` is a 3-tap window plus two half-weight neighbours rather than a rounded
//! integer window.

use serde::{Deserialize, Serialize};

/// Largest authored blur radius in pixels, taken from the effect list range.
pub const BOX_BLUR_AUTHOR_MAX_RADIUS_PIXELS: f32 = 30_000.0;
/// Smallest authored iteration count.
pub const BOX_BLUR_AUTHOR_MIN_ITERATIONS: i64 = 1;
/// Largest authored iteration count.
pub const BOX_BLUR_AUTHOR_MAX_ITERATIONS: i64 = 50;

/// Largest input halo the planner is asked to materialize as an explicit ROI
/// expansion. Wider kernels are expressed as a full-frame input instead.
const MAX_BOX_BLUR_INPUT_HALO_PIXELS: u64 = 4_096;

/// Axis set a single Box Blur instance repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoxBlurDirection {
    /// Blur rows and columns.
    HorizontalAndVertical,
    /// Blur rows only.
    Horizontal,
    /// Blur columns only.
    Vertical,
}

impl BoxBlurDirection {
    /// Every direction in authoring menu order.
    pub const ALL: [Self; 3] = [
        Self::HorizontalAndVertical,
        Self::Horizontal,
        Self::Vertical,
    ];

    /// Stable persistence key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::HorizontalAndVertical => "horizontal_and_vertical",
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }

    /// Parses a persistence key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "horizontal_and_vertical" => Some(Self::HorizontalAndVertical),
            "horizontal" => Some(Self::Horizontal),
            "vertical" => Some(Self::Vertical),
            _ => None,
        }
    }

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::HorizontalAndVertical => "水平和垂直",
            Self::Horizontal => "水平",
            Self::Vertical => "垂直",
        }
    }

    /// Whether this direction repeats a row pass.
    pub const fn blurs_horizontal(self) -> bool {
        matches!(self, Self::HorizontalAndVertical | Self::Horizontal)
    }

    /// Whether this direction repeats a column pass.
    pub const fn blurs_vertical(self) -> bool {
        matches!(self, Self::HorizontalAndVertical | Self::Vertical)
    }
}

/// Normalizes an authored radius, rejecting non-finite and negative values.
pub(crate) fn valid_box_blur_radius(radius: f32) -> Option<f32> {
    (radius.is_finite() && (0.0..=BOX_BLUR_AUTHOR_MAX_RADIUS_PIXELS).contains(&radius))
        .then_some(radius)
}

/// Input halo of one Box Blur instance, per axis.
///
/// Returns `None` when the exact halo exceeds the explicit-expansion limit; the
/// caller then declares a full-frame input, which is conservative but never
/// smaller than the real read extent.
pub(crate) fn box_blur_input_halo(radius: f32, iterations: u32) -> Option<u32> {
    let radius = valid_box_blur_radius(radius)?;
    let whole_radius = radius.floor() as u64;
    let has_fractional_tail = radius.fract() > f32::EPSILON;
    let per_pass = whole_radius + u64::from(has_fractional_tail);
    let total = per_pass.checked_mul(u64::from(iterations.max(1)))?;
    if total > MAX_BOX_BLUR_INPUT_HALO_PIXELS {
        return None;
    }
    u32::try_from(total).ok()
}

/// Per-axis input halo of one Box Blur instance.
///
/// An axis that is not blurred reads no neighbour, so its halo is zero.
pub(crate) fn box_blur_axis_halo(
    radius: f32,
    iterations: u32,
    direction: BoxBlurDirection,
) -> Option<(u32, u32)> {
    let halo = box_blur_input_halo(radius, iterations)?;
    Some((
        if direction.blurs_horizontal() {
            halo
        } else {
            0
        },
        if direction.blurs_vertical() { halo } else { 0 },
    ))
}

/// Blurs one straight-alpha f32 frame in place.
///
/// Returns `false` when the arguments do not describe a blur, so callers can
/// report the same unsupported-parameter path as other render ops instead of
/// silently returning an unblurred frame.
pub fn box_blur_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    radius: f32,
    iterations: u32,
    direction: BoxBlurDirection,
    repeat_edge_pixels: bool,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_box_blur_f32_controlled(
        pixels,
        width,
        height,
        radius,
        iterations,
        direction,
        repeat_edge_pixels,
        &mut checkpoint,
    )
    .is_ok_and(|applied| applied)
}

/// Blurs one straight-alpha u8 frame, matching the f32 kernel through a single
/// final rounding step.
pub fn box_blur_frame_u8(
    pixels: &[u8],
    width: usize,
    height: usize,
    radius: f32,
    iterations: u32,
    direction: BoxBlurDirection,
    repeat_edge_pixels: bool,
) -> Option<Vec<u8>> {
    let pixel_count = width.checked_mul(height)?;
    if pixels.len() < pixel_count * 4 {
        return None;
    }
    let mut floats = Vec::with_capacity(pixel_count);
    for pixel in pixels[..pixel_count * 4].chunks_exact(4) {
        floats.push([
            f32::from(pixel[0]) / 255.0,
            f32::from(pixel[1]) / 255.0,
            f32::from(pixel[2]) / 255.0,
            f32::from(pixel[3]) / 255.0,
        ]);
    }
    if !box_blur_frame_f32(
        &mut floats,
        width,
        height,
        radius,
        iterations,
        direction,
        repeat_edge_pixels,
    ) {
        return None;
    }
    let mut output = Vec::with_capacity(pixel_count * 4);
    for pixel in floats {
        output.extend(pixel.map(crate::adjustment::unit_to_u8));
    }
    Some(output)
}

/// Applies the blur to one region of a straight-alpha f32 frame.
///
/// The caller owns the region extent and its halo; the kernel only needs the
/// buffer geometry, exactly like the Gaussian blur path.
pub(crate) fn apply_box_blur_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    radius: f32,
    iterations: u32,
    direction: BoxBlurDirection,
    repeat_edge_pixels: bool,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(radius) = valid_box_blur_radius(radius) else {
        return Ok(false);
    };
    let pixel_count = match width.checked_mul(height) {
        Some(pixel_count) if working.len() >= pixel_count => pixel_count,
        _ => return Ok(false),
    };
    let kernel = BoxKernel::new(radius);
    if kernel.is_identity() {
        return Ok(true);
    }
    let iterations = iterations.max(1);

    premultiply_rgba_controlled(&mut working[..pixel_count], checkpoint)?;
    let mut scratch = vec![[0.0_f32; 4]; pixel_count];
    let mut prefix = vec![[0.0_f64; 4]; width.max(height) + 1];
    let mut working_is_source = true;
    for _ in 0..iterations {
        if direction.blurs_horizontal() {
            if working_is_source {
                blur_axis_controlled(
                    &working[..pixel_count],
                    &mut scratch,
                    &mut prefix,
                    Axis::Horizontal,
                    width,
                    height,
                    kernel,
                    repeat_edge_pixels,
                    checkpoint,
                )?;
            } else {
                blur_axis_controlled(
                    &scratch,
                    &mut working[..pixel_count],
                    &mut prefix,
                    Axis::Horizontal,
                    width,
                    height,
                    kernel,
                    repeat_edge_pixels,
                    checkpoint,
                )?;
            }
            working_is_source = !working_is_source;
        }
        if direction.blurs_vertical() {
            if working_is_source {
                blur_axis_controlled(
                    &working[..pixel_count],
                    &mut scratch,
                    &mut prefix,
                    Axis::Vertical,
                    width,
                    height,
                    kernel,
                    repeat_edge_pixels,
                    checkpoint,
                )?;
            } else {
                blur_axis_controlled(
                    &scratch,
                    &mut working[..pixel_count],
                    &mut prefix,
                    Axis::Vertical,
                    width,
                    height,
                    kernel,
                    repeat_edge_pixels,
                    checkpoint,
                )?;
            }
            working_is_source = !working_is_source;
        }
    }
    if !working_is_source {
        working[..pixel_count].copy_from_slice(&scratch);
    }
    unpremultiply_rgba_controlled(&mut working[..pixel_count], checkpoint)?;
    Ok(true)
}

#[derive(Debug, Clone, Copy)]
struct BoxKernel {
    whole_radius: u32,
    fraction: f32,
}

impl BoxKernel {
    fn new(radius: f32) -> Self {
        let whole = radius.floor();
        Self {
            whole_radius: whole as u32,
            fraction: radius - whole,
        }
    }

    fn is_identity(self) -> bool {
        self.whole_radius == 0 && self.fraction <= f32::EPSILON
    }

    fn normalization(self) -> f64 {
        2.0 * f64::from(self.whole_radius) + 1.0 + 2.0 * f64::from(self.fraction)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// One separable pass over one axis.
///
/// Both axes share the sample math through `origin` and `stride`: a row pass
/// walks with stride 1 from `y * width`, a column pass walks with stride `width`
/// from `x`. Prefix sums and neighbours are then addressed exactly like the
/// same filter on a one-dimensional sequence.
fn blur_axis_controlled<E>(
    source: &[[f32; 4]],
    destination: &mut [[f32; 4]],
    prefix: &mut Vec<[f64; 4]>,
    axis: Axis,
    width: usize,
    height: usize,
    kernel: BoxKernel,
    repeat_edge_pixels: bool,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let (lines, extent, origin_step, stride) = match axis {
        Axis::Horizontal => (height, width, width, 1),
        Axis::Vertical => (width, height, 1, width),
    };
    prefix.resize(extent + 1, [0.0; 4]);
    for line in 0..lines {
        checkpoint()?;
        let origin = line * origin_step;
        prefix[0] = [0.0; 4];
        for index in 0..extent {
            let pixel = source[origin + index * stride];
            let previous = prefix[index];
            prefix[index + 1] = [
                previous[0] + f64::from(pixel[0]),
                previous[1] + f64::from(pixel[1]),
                previous[2] + f64::from(pixel[2]),
                previous[3] + f64::from(pixel[3]),
            ];
        }
        for index in 0..extent {
            destination[origin + index * stride] = sample_box(
                source,
                prefix.as_slice(),
                origin,
                stride,
                extent,
                index,
                kernel,
                repeat_edge_pixels,
            );
        }
    }
    checkpoint()
}

/// One normalized box sample.
///
/// Samples inside the integer window come from the prefix sums. Samples the
/// window reaches beyond the extent either replicate the boundary pixel or count
/// as transparent, depending on `repeat_edge_pixels`; either way they stay in the
/// normalization, which is what makes a non-repeating edge fade instead of
/// brightening.
fn sample_box(
    source: &[[f32; 4]],
    prefix: &[[f64; 4]],
    origin: usize,
    stride: usize,
    extent: usize,
    position: usize,
    kernel: BoxKernel,
    repeat_edge_pixels: bool,
) -> [f32; 4] {
    let last = extent as i64 - 1;
    let low = position as i64 - i64::from(kernel.whole_radius);
    let high = position as i64 + i64::from(kernel.whole_radius);
    let low_inside = low.clamp(0, last) as usize;
    let high_inside = high.clamp(0, last) as usize;
    let mut sum = [0.0_f64; 4];
    for channel in 0..4 {
        sum[channel] = prefix[high_inside + 1][channel] - prefix[low_inside][channel];
    }
    if repeat_edge_pixels {
        let left_repeat = (low_inside as i64 - low) as f64;
        let right_repeat = (high - high_inside as i64) as f64;
        if left_repeat > 0.0 {
            let pixel = source[origin];
            for channel in 0..4 {
                sum[channel] += left_repeat * f64::from(pixel[channel]);
            }
        }
        if right_repeat > 0.0 {
            let pixel = source[origin + (extent - 1) * stride];
            for channel in 0..4 {
                sum[channel] += right_repeat * f64::from(pixel[channel]);
            }
        }
    }
    if kernel.fraction > f32::EPSILON {
        let weight = f64::from(kernel.fraction);
        let fractional_low = low - 1;
        let fractional_high = high + 1;
        if repeat_edge_pixels || fractional_low >= 0 {
            let index = fractional_low.clamp(0, last) as usize;
            let pixel = source[origin + index * stride];
            for channel in 0..4 {
                sum[channel] += weight * f64::from(pixel[channel]);
            }
        }
        if repeat_edge_pixels || fractional_high <= last {
            let index = fractional_high.clamp(0, last) as usize;
            let pixel = source[origin + index * stride];
            for channel in 0..4 {
                sum[channel] += weight * f64::from(pixel[channel]);
            }
        }
    }
    let normalization = kernel.normalization();
    [
        (sum[0] / normalization) as f32,
        (sum[1] / normalization) as f32,
        (sum[2] / normalization) as f32,
        (sum[3] / normalization) as f32,
    ]
}

fn premultiply_rgba_controlled<E>(
    pixels: &mut [[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for chunk in pixels.chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            let alpha = pixel[3].clamp(0.0, 1.0);
            pixel[0] *= alpha;
            pixel[1] *= alpha;
            pixel[2] *= alpha;
            pixel[3] = alpha;
        }
    }
    checkpoint()
}

fn unpremultiply_rgba_controlled<E>(
    pixels: &mut [[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for chunk in pixels.chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            *pixel = crate::coverage::straight_rgba_from_premultiplied(*pixel);
        }
    }
    checkpoint()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blur(
        pixels: Vec<[f32; 4]>,
        width: usize,
        radius: f32,
        iterations: u32,
        direction: BoxBlurDirection,
        repeat_edge_pixels: bool,
    ) -> Vec<[f32; 4]> {
        let height = pixels.len() / width;
        let mut pixels = pixels;
        assert!(box_blur_frame_f32(
            &mut pixels,
            width,
            height,
            radius,
            iterations,
            direction,
            repeat_edge_pixels
        ));
        pixels
    }

    fn gray(values: &[f32]) -> Vec<[f32; 4]> {
        values.iter().map(|value| [*value, *value, *value, 1.0]).collect()
    }

    #[test]
    fn direction_keys_are_stable_and_distinct() {
        let mut keys = Vec::new();
        for direction in BoxBlurDirection::ALL {
            keys.push(direction.key());
            assert_eq!(BoxBlurDirection::from_key(direction.key()), Some(direction));
            assert!(!direction.label().is_empty());
        }
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), BoxBlurDirection::ALL.len());
        assert_eq!(BoxBlurDirection::from_key("diagonal"), None);
    }

    #[test]
    fn zero_radius_is_identity() {
        let source = [[0.25, 0.5, 0.75, 1.0], [0.1, 0.2, 0.3, 0.4]];
        let output = blur(
            source.to_vec(),
            2,
            0.0,
            3,
            BoxBlurDirection::HorizontalAndVertical,
            true,
        );
        assert_eq!(output, source);
    }

    #[test]
    fn single_pass_averages_with_replicated_edges() {
        // A 1x3 row: the window of the first pixel reaches one sample before the
        // row, which replicates the first pixel.
        let output = blur(
            gray(&[0.0, 1.0, 0.0]),
            3,
            1.0,
            1,
            BoxBlurDirection::Horizontal,
            true,
        );
        let expected = 1.0 / 3.0;
        for pixel in output {
            assert!((pixel[0] - expected).abs() < 1.0e-6, "{pixel:?}");
        }
    }

    #[test]
    fn non_repeating_edges_fade_instead_of_brightening() {
        // A uniform row stays uniform when the edge pixels repeat, but loses
        // weight when samples outside the row count as transparent.
        let repeated = blur(
            gray(&[10.0, 10.0, 10.0]),
            3,
            2.0,
            1,
            BoxBlurDirection::Horizontal,
            true,
        );
        for pixel in repeated {
            assert!((pixel[0] - 10.0).abs() < 1.0e-6, "{pixel:?}");
        }
        let faded = blur(
            gray(&[10.0, 10.0, 10.0]),
            3,
            2.0,
            1,
            BoxBlurDirection::Horizontal,
            false,
        );
        // Every pixel now sees only the three samples inside the row but keeps
        // the five-sample normalization: coverage drops while the straight-alpha
        // color is restored by the unpremultiply.
        let expected_coverage = 3.0 / 5.0;
        for pixel in faded {
            assert!((pixel[0] - 10.0).abs() < 1.0e-6, "{pixel:?}");
            assert!((pixel[3] - expected_coverage).abs() < 1.0e-6, "{pixel:?}");
        }
    }

    #[test]
    fn iterations_compose_into_a_triangular_kernel() {
        let output = blur(
            gray(&[0.0, 0.0, 1.0, 0.0, 0.0]),
            5,
            1.0,
            2,
            BoxBlurDirection::Horizontal,
            true,
        );
        let expected = [1.0 / 9.0, 2.0 / 9.0, 3.0 / 9.0, 2.0 / 9.0, 1.0 / 9.0];
        for (pixel, expected) in output.iter().zip(expected) {
            assert!((pixel[0] - expected).abs() < 1.0e-6, "{pixel:?}");
        }
    }

    #[test]
    fn fractional_radius_weights_the_outside_samples() {
        // radius 0.5 keeps the center pixel at full weight and adds half of each
        // neighbour; the row [0, 1, 0] therefore spreads 1.0 into a 2-tap window.
        let output = blur(
            gray(&[0.0, 1.0, 0.0]),
            3,
            0.5,
            1,
            BoxBlurDirection::Horizontal,
            true,
        );
        let total: f32 = output.iter().map(|pixel| pixel[0]).sum();
        assert!((total - 1.0).abs() < 1.0e-6, "{output:?}");
        assert!(output[1][0] > output[0][0]);
    }

    #[test]
    fn radius_wider_than_the_frame_stays_normalized() {
        let output = blur(
            gray(&[0.2, 0.4, 0.9]),
            3,
            30_000.0,
            1,
            BoxBlurDirection::HorizontalAndVertical,
            true,
        );
        // Edge replication weights the boundary samples for a kernel wider than
        // the frame, but the result must stay finite and uniform.
        for pixel in &output {
            assert!(pixel[0].is_finite(), "{output:?}");
            assert!((0.2..=0.9).contains(&pixel[0]), "{output:?}");
            assert!((pixel[0] - output[0][0]).abs() < 1.0e-3, "{output:?}");
        }
    }

    #[test]
    fn transparent_pixels_do_not_contaminate_visible_colors() {
        let source = vec![[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.0]];
        let output = blur(source, 2, 1.0, 1, BoxBlurDirection::Horizontal, true);
        // The fully transparent red pixel carries no premultiplied color, so the
        // opaque green pixel never picks up red. Its coverage drops to two of the
        // three samples, which the straight-alpha output reports as alpha 2/3.
        assert!((output[1][0]).abs() < 1.0e-6, "{:?}", output[1]);
        assert!((output[1][1] - 1.0).abs() < 1.0e-6, "{:?}", output[1]);
        assert!((output[1][3] - 2.0 / 3.0).abs() < 1.0e-6, "{:?}", output[1]);
    }

    #[test]
    fn vertical_direction_only_changes_columns() {
        let source = vec![
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
        ];
        let output = blur(source, 2, 1.0, 1, BoxBlurDirection::Vertical, true);
        assert!((output[0][0] - output[1][0]).abs() < f32::EPSILON);
        assert!(output[2][0] > output[1][0], "{output:?}");
    }

    #[test]
    fn hdr_values_are_averaged_without_clamping() {
        let output = blur(
            vec![
                [0.0, 0.0, 0.0, 1.0],
                [8.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            3,
            1.0,
            1,
            BoxBlurDirection::Horizontal,
            true,
        );
        assert!(output[1][0] > 2.6, "{output:?}");
    }

    #[test]
    fn u8_path_matches_the_float_kernel_with_one_rounding() {
        let source: Vec<u8> = [0_u8, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255].to_vec();
        let output = box_blur_frame_u8(&source, 3, 1, 1.0, 1, BoxBlurDirection::Horizontal, true)
            .expect("blur");
        assert_eq!(output.len(), source.len());
        for pixel in output.chunks_exact(4) {
            assert_eq!(pixel[0], 85, "{pixel:?}");
        }
    }

    #[test]
    fn halo_and_axis_halo_describe_the_read_extent() {
        assert_eq!(box_blur_input_halo(1.0, 3), Some(3));
        assert_eq!(box_blur_input_halo(1.5, 2), Some(4));
        assert_eq!(box_blur_input_halo(0.0, 5), Some(0));
        assert_eq!(box_blur_input_halo(30_000.0, 50), None);
        assert_eq!(box_blur_input_halo(f32::NAN, 1), None);
        assert_eq!(box_blur_input_halo(-1.0, 1), None);
        assert_eq!(
            box_blur_axis_halo(2.0, 2, BoxBlurDirection::Horizontal),
            Some((4, 0))
        );
        assert_eq!(
            box_blur_axis_halo(2.0, 2, BoxBlurDirection::Vertical),
            Some((0, 4))
        );
    }
}
