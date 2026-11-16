//! Cartoon: flatten a frame into a few tone steps and draw soft edges.
//!
//! The effect runs as three spatial passes: a separable tent blur, then a
//! combine pass that quantises the blurred frame and overlays edges
//! reconstructed from the same blurred frame. The CPU kernel below mirrors the
//! GPU passes step for step so the two stay bit-comparable.

use std::hash::{Hash, Hasher};

use crate::coverage;

/// Smallest accepted detail radius in pixels.
pub const CARTOON_MIN_DETAIL_RADIUS: f32 = 0.001;
/// Largest accepted detail radius in pixels.
pub const CARTOON_MAX_DETAIL_RADIUS: f32 = 40.0;
/// Documented default detail radius.
pub const CARTOON_DEFAULT_DETAIL_RADIUS: f32 = 8.0;
/// Smallest accepted detail threshold, in percent of the luma range.
pub const CARTOON_MIN_DETAIL_THRESHOLD: f32 = 0.001;
/// Largest accepted detail threshold, in percent of the luma range.
pub const CARTOON_MAX_DETAIL_THRESHOLD: f32 = 100.0;
/// Documented default detail threshold.
pub const CARTOON_DEFAULT_DETAIL_THRESHOLD: f32 = 10.0;
/// Smallest accepted tone step count.
pub const CARTOON_MIN_SHADOW_STEPS: u32 = 2;
/// Largest accepted tone step count.
pub const CARTOON_MAX_SHADOW_STEPS: u32 = 30;
/// Documented default tone step count.
pub const CARTOON_DEFAULT_SHADOW_STEPS: u32 = 8;
/// Smallest accepted shadow smoothness, in percent.
pub const CARTOON_MIN_SHADOW_SMOOTHNESS: f32 = 0.001;
/// Largest accepted shadow smoothness, in percent.
pub const CARTOON_MAX_SHADOW_SMOOTHNESS: f32 = 100.0;
/// Documented default shadow smoothness.
pub const CARTOON_DEFAULT_SHADOW_SMOOTHNESS: f32 = 70.0;
/// Smallest accepted edge threshold, in percent.
pub const CARTOON_MIN_THRESHOLD: f32 = 1.0;
/// Largest accepted edge threshold, in percent.
pub const CARTOON_MAX_THRESHOLD: f32 = 10.0;
/// Documented default edge threshold.
pub const CARTOON_DEFAULT_THRESHOLD: f32 = 1.6;
/// Smallest accepted edge width in pixels.
pub const CARTOON_MIN_WIDTH: f32 = 0.001;
/// Largest accepted edge width in pixels.
pub const CARTOON_MAX_WIDTH: f32 = 20.0;
/// Documented default edge width.
pub const CARTOON_DEFAULT_WIDTH: f32 = 1.5;
/// Smallest accepted edge softness, in percent.
pub const CARTOON_MIN_SOFTNESS: f32 = 0.0;
/// Largest accepted edge softness, in percent.
pub const CARTOON_MAX_SOFTNESS: f32 = 100.0;
/// Documented default edge softness.
pub const CARTOON_DEFAULT_SOFTNESS: f32 = 60.0;
/// Smallest accepted edge opacity, in percent.
pub const CARTOON_MIN_OPACITY: f32 = 0.0;
/// Largest accepted edge opacity, in percent.
pub const CARTOON_MAX_OPACITY: f32 = 100.0;
/// Documented default edge opacity.
pub const CARTOON_DEFAULT_OPACITY: f32 = 100.0;
/// Smallest accepted edge gain, in percent.
pub const CARTOON_MIN_EDGE_ENHANCE: f32 = -100.0;
/// Largest accepted edge gain, in percent.
pub const CARTOON_MAX_EDGE_ENHANCE: f32 = 100.0;
/// Documented default edge gain.
pub const CARTOON_DEFAULT_EDGE_ENHANCE: f32 = 1.0;
/// Smallest accepted edge black level.
pub const CARTOON_MIN_EDGE_BLACK_LEVEL: f32 = -10.0;
/// Largest accepted edge black level.
pub const CARTOON_MAX_EDGE_BLACK_LEVEL: f32 = 10.0;
/// Documented default edge black level.
pub const CARTOON_DEFAULT_EDGE_BLACK_LEVEL: f32 = 0.0;
/// Smallest accepted edge contrast.
pub const CARTOON_MIN_EDGE_CONTRAST: f32 = 0.0;
/// Largest accepted edge contrast.
pub const CARTOON_MAX_EDGE_CONTRAST: f32 = 5.0;
/// Documented default edge contrast.
pub const CARTOON_DEFAULT_EDGE_CONTRAST: f32 = 0.5;

/// What the combine pass writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CartoonRender {
    /// Quantised fill only.
    Fill,
    /// Original colours with edges drawn over them.
    Edges,
    /// Quantised fill with edges drawn over it.
    FillAndEdges,
}

impl CartoonRender {
    /// Every variant, in menu order.
    pub const ALL: [Self; 3] = [Self::Fill, Self::Edges, Self::FillAndEdges];
    /// The documented default.
    pub const DEFAULT: Self = Self::FillAndEdges;

    /// The stable author key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Edges => "edges",
            Self::FillAndEdges => "fill_and_edges",
        }
    }

    /// The Chinese menu caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fill => "填充",
            Self::Edges => "边缘",
            Self::FillAndEdges => "填充与边缘",
        }
    }

    /// Resolves an author key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|variant| variant.as_key() == key)
    }

    /// The stable tag stored in the fused uniform.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Fill => 1,
            Self::Edges => 2,
            Self::FillAndEdges => 3,
        }
    }
}

/// The authored cartoon values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CartoonParameters {
    /// What the combine pass writes.
    pub render: CartoonRender,
    /// Pre-blur radius in pixels.
    pub detail_radius: f32,
    /// Luma difference treated as detail, in percent.
    pub detail_threshold: f32,
    /// Number of tone steps.
    pub shadow_steps: f32,
    /// How hard the tone steps are, in percent.
    pub shadow_smoothness: f32,
    /// Edge threshold, in percent.
    pub threshold: f32,
    /// Edge sampling distance in pixels.
    pub width: f32,
    /// Edge softness, in percent.
    pub softness: f32,
    /// Edge opacity, in percent.
    pub opacity: f32,
    /// Signed edge gain, in percent.
    pub edge_enhance: f32,
    /// Signed edge black level.
    pub edge_black_level: f32,
    /// Edge contrast.
    pub edge_contrast: f32,
}

impl Default for CartoonParameters {
    fn default() -> Self {
        Self {
            render: CartoonRender::DEFAULT,
            detail_radius: CARTOON_DEFAULT_DETAIL_RADIUS,
            detail_threshold: CARTOON_DEFAULT_DETAIL_THRESHOLD,
            shadow_steps: CARTOON_DEFAULT_SHADOW_STEPS as f32,
            shadow_smoothness: CARTOON_DEFAULT_SHADOW_SMOOTHNESS,
            threshold: CARTOON_DEFAULT_THRESHOLD,
            width: CARTOON_DEFAULT_WIDTH,
            softness: CARTOON_DEFAULT_SOFTNESS,
            opacity: CARTOON_DEFAULT_OPACITY,
            edge_enhance: CARTOON_DEFAULT_EDGE_ENHANCE,
            edge_black_level: CARTOON_DEFAULT_EDGE_BLACK_LEVEL,
            edge_contrast: CARTOON_DEFAULT_EDGE_CONTRAST,
        }
    }
}

/// The author parameter that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CartoonError {
    parameter: &'static str,
}

impl std::fmt::Display for CartoonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "cartoon.{} must be finite and within its documented range",
            self.parameter
        )
    }
}

impl std::error::Error for CartoonError {}

impl CartoonError {
    /// The author parameter that was rejected.
    pub const fn parameter(&self) -> &'static str {
        self.parameter
    }
}

/// A validated cartoon grade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CartoonGrade {
    parameters: CartoonParameters,
}

impl CartoonGrade {
    /// Validates every author value and records it.
    pub fn new(parameters: CartoonParameters) -> Result<Self, CartoonError> {
        let range = |parameter: &'static str, value: f32, low: f32, high: f32| {
            if !value.is_finite() || value < low || value > high {
                Err(CartoonError { parameter })
            } else {
                Ok(())
            }
        };
        range(
            "detail_radius",
            parameters.detail_radius,
            CARTOON_MIN_DETAIL_RADIUS,
            CARTOON_MAX_DETAIL_RADIUS,
        )?;
        range(
            "detail_threshold",
            parameters.detail_threshold,
            CARTOON_MIN_DETAIL_THRESHOLD,
            CARTOON_MAX_DETAIL_THRESHOLD,
        )?;
        range(
            "shadow_steps",
            parameters.shadow_steps,
            CARTOON_MIN_SHADOW_STEPS as f32,
            CARTOON_MAX_SHADOW_STEPS as f32,
        )?;
        range(
            "shadow_smoothness",
            parameters.shadow_smoothness,
            CARTOON_MIN_SHADOW_SMOOTHNESS,
            CARTOON_MAX_SHADOW_SMOOTHNESS,
        )?;
        range(
            "threshold",
            parameters.threshold,
            CARTOON_MIN_THRESHOLD,
            CARTOON_MAX_THRESHOLD,
        )?;
        range(
            "width",
            parameters.width,
            CARTOON_MIN_WIDTH,
            CARTOON_MAX_WIDTH,
        )?;
        range(
            "softness",
            parameters.softness,
            CARTOON_MIN_SOFTNESS,
            CARTOON_MAX_SOFTNESS,
        )?;
        range(
            "opacity",
            parameters.opacity,
            CARTOON_MIN_OPACITY,
            CARTOON_MAX_OPACITY,
        )?;
        range(
            "edge_enhance",
            parameters.edge_enhance,
            CARTOON_MIN_EDGE_ENHANCE,
            CARTOON_MAX_EDGE_ENHANCE,
        )?;
        range(
            "edge_black_level",
            parameters.edge_black_level,
            CARTOON_MIN_EDGE_BLACK_LEVEL,
            CARTOON_MAX_EDGE_BLACK_LEVEL,
        )?;
        range(
            "edge_contrast",
            parameters.edge_contrast,
            CARTOON_MIN_EDGE_CONTRAST,
            CARTOON_MAX_EDGE_CONTRAST,
        )?;
        Ok(Self { parameters })
    }

    /// The documented defaults, which always validate.
    pub fn documented_defaults() -> Self {
        Self { parameters: CartoonParameters::default() }
    }

    /// The recorded author values.
    pub const fn parameters(&self) -> &CartoonParameters {
        &self.parameters
    }

    /// Pre-blur radius in pixels, at least one.
    pub fn radius(&self) -> u32 {
        (self.parameters.detail_radius.ceil() as i64).clamp(1, CARTOON_MAX_DETAIL_RADIUS as i64)
            as u32
    }

    /// Quantisation step count.
    pub fn steps(&self) -> u32 {
        (self.parameters.shadow_steps.round() as i64).clamp(
            CARTOON_MIN_SHADOW_STEPS as i64,
            CARTOON_MAX_SHADOW_STEPS as i64,
        ) as u32
    }

    /// Edge sampling distance in pixels, at least one.
    pub fn edge_width(&self) -> u32 {
        (self.parameters.width.ceil() as i64).clamp(1, CARTOON_MAX_WIDTH as i64) as u32
    }

    /// Normalised detail threshold.
    pub fn detail_threshold(&self) -> f32 {
        self.parameters.detail_threshold.clamp(0.001, 100.0) / 100.0
    }

    /// Normalised edge threshold.
    pub fn edge_threshold(&self) -> f32 {
        self.parameters.threshold.clamp(1.0, 10.0) / 100.0
    }

    /// Whether the grade is an exact identity.
    ///
    /// Only the edge-only rendering can be an identity, and only when it draws
    /// nothing at all.
    pub fn is_identity(&self) -> bool {
        self.parameters.render == CartoonRender::Edges
            && self.parameters.opacity <= 0.0
            && self.parameters.edge_black_level == 0.0
    }

    /// The halo the spatial chain must reserve around the frame.
    pub fn input_halo(&self) -> (u32, u32) {
        let halo = self.radius() + self.edge_width() + 1;
        (halo, halo)
    }

    /// The fused uniform cells for the three spatial passes.
    pub fn gpu_controls(&self) -> CartoonGpuControls {
        (
            [
                self.radius() as f32,
                self.steps() as f32,
                self.parameters.shadow_smoothness.clamp(0.001, 100.0) / 100.0,
                self.detail_threshold(),
            ],
            [
                self.edge_threshold(),
                self.edge_width() as f32,
                self.parameters.softness.clamp(0.0, 100.0) / 100.0,
                self.parameters.opacity.clamp(0.0, 100.0) / 100.0,
            ],
            [
                self.parameters.render.as_u8() as f32,
                self.parameters.edge_enhance.clamp(-100.0, 100.0) / 100.0,
                self.parameters.edge_black_level.clamp(-10.0, 10.0) / 10.0,
                self.parameters.edge_contrast.clamp(0.0, 5.0),
            ],
        )
    }

    /// Relative cost of the three spatial passes.
    pub fn cost(&self) -> u8 {
        3
    }

    /// Mixes the grade into an operation hash.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        let parameters = &self.parameters;
        parameters.render.as_u8().hash(state);
        for value in [
            parameters.detail_radius,
            parameters.detail_threshold,
            parameters.shadow_steps,
            parameters.shadow_smoothness,
            parameters.threshold,
            parameters.width,
            parameters.softness,
            parameters.opacity,
            parameters.edge_enhance,
            parameters.edge_black_level,
            parameters.edge_contrast,
        ] {
            value.to_bits().hash(state);
        }
    }
}

/// The six fused uniform cells of the three cartoon passes.
pub type CartoonGpuControls = ([f32; 4], [f32; 4], [f32; 4]);

/// Applies the cartoon effect to a straight RGBA frame.
///
/// Returns `false` when the frame is empty or shorter than its declared size,
/// and `true` otherwise. A grade that is an exact identity still returns
/// `true`.
pub fn cartoon_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CartoonGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_cartoon_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Checkpointed kernel shared by the scalar executor and the frame harness.
pub(crate) fn apply_cartoon_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CartoonGrade,
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
    checkpoint()?;

    let radius = grade.radius() as usize;
    let mut horizontal = vec![[0.0f32; 4]; pixel_count];
    let mut vertical = vec![[0.0f32; 4]; pixel_count];
    tent_axis(
        working,
        &mut horizontal,
        width,
        height,
        radius,
        Axis::Horizontal,
    );
    tent_axis(
        &horizontal,
        &mut vertical,
        width,
        height,
        radius,
        Axis::Vertical,
    );

    let parameters = grade.parameters();
    let steps = grade.steps() as f32;
    let smoothness = parameters.shadow_smoothness.clamp(0.001, 100.0) / 100.0;
    let detail_threshold = grade.detail_threshold();
    let edge_threshold = grade.edge_threshold();
    let reach = grade.edge_width() as i64;
    let softness = parameters.softness.clamp(0.0, 100.0) / 100.0;
    let opacity = parameters.opacity.clamp(0.0, 100.0) / 100.0;
    let enhance = 1.0 + parameters.edge_enhance.clamp(-100.0, 100.0) / 100.0;
    let black_level = parameters.edge_black_level.clamp(-10.0, 10.0) / 10.0;
    let contrast = parameters.edge_contrast.clamp(0.0, 5.0);
    let render = parameters.render;
    let draws_edges = render != CartoonRender::Fill;
    let last_column = width as i64 - 1;
    let last_row = height as i64 - 1;

    crate::parallel::for_each_row_band(working, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let source = *pixel;
            let base = vertical[row * width + column];
            if !coverage::has_positive_coverage(source[3].clamp(0.0, 1.0)) {
                *pixel = [0.0; 4];
                continue;
            }
            let fill = {
                let detail = (luma(source) - luma(base)).abs();
                let keep = smoothstep(detail_threshold, detail_threshold + 0.001, detail);
                let mut quantised = [0.0f32; 3];
                for channel in 0..3 {
                    let snapped = quantize_channel(base[channel], steps);
                    quantised[channel] = base[channel] + (snapped - base[channel]) * smoothness;
                }
                let mut mixed = [0.0f32; 3];
                for channel in 0..3 {
                    mixed[channel] =
                        quantised[channel] + (source[channel] - quantised[channel]) * keep;
                }
                mixed
            };
            let darken = if draws_edges {
                let left =
                    vertical[row * width + (column as i64 - reach).clamp(0, last_column) as usize];
                let right =
                    vertical[row * width + (column as i64 + reach).clamp(0, last_column) as usize];
                let above =
                    vertical[(row as i64 - reach).clamp(0, last_row) as usize * width + column];
                let below =
                    vertical[(row as i64 + reach).clamp(0, last_row) as usize * width + column];
                let gradient = (luma(right) - luma(left)).abs() + (luma(below) - luma(above)).abs();
                let low = edge_threshold * (1.0 - softness);
                let high = edge_threshold * (1.0 + softness) + 0.0001;
                let mask = smoothstep(low, high, gradient * enhance.max(0.0));
                ((mask * contrast).clamp(0.0, 1.0) * opacity + black_level).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut result = match render {
                CartoonRender::Fill => fill,
                CartoonRender::Edges => [source[0], source[1], source[2]],
                CartoonRender::FillAndEdges => fill,
            };
            if draws_edges {
                for channel in result.iter_mut() {
                    *channel *= 1.0 - darken;
                }
            }
            pixel[0] = result[0];
            pixel[1] = result[1];
            pixel[2] = result[2];
            pixel[3] = source[3];
        }
    });

    checkpoint()?;
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// One separable tent pass: `w(d) = 1 / (1 + d²)` over `-radius..=radius`.
fn tent_axis(
    source: &[[f32; 4]],
    destination: &mut [[f32; 4]],
    width: usize,
    height: usize,
    radius: usize,
    axis: Axis,
) {
    if radius == 0 {
        destination[..width * height].copy_from_slice(&source[..width * height]);
        return;
    }
    let last_column = width as i64 - 1;
    let last_row = height as i64 - 1;
    let reach = radius as i64;
    crate::parallel::for_each_row_band(destination, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let mut sum = [0.0f32; 4];
            let mut weight_sum = 0.0f32;
            for offset in -reach..=reach {
                let weight = 1.0 / (1.0 + (offset * offset) as f32);
                let (sample_row, sample_column) = match axis {
                    Axis::Horizontal => {
                        (row as i64, (column as i64 + offset).clamp(0, last_column))
                    }
                    Axis::Vertical => ((row as i64 + offset).clamp(0, last_row), column as i64),
                };
                let sample =
                    premultiply(source[sample_row as usize * width + sample_column as usize]);
                for channel in 0..4 {
                    sum[channel] += sample[channel] * weight;
                }
                weight_sum += weight;
            }
            *pixel = if weight_sum > 0.0 {
                unpremultiply([
                    sum[0] / weight_sum,
                    sum[1] / weight_sum,
                    sum[2] / weight_sum,
                    sum[3] / weight_sum,
                ])
            } else {
                [0.0; 4]
            };
        }
    });
}

fn quantize_channel(value: f32, steps: f32) -> f32 {
    ((value.clamp(0.0, 1.0) * steps + 0.5).floor() / steps).clamp(0.0, 1.0)
}

fn premultiply(sample: [f32; 4]) -> [f32; 4] {
    let alpha = sample[3].clamp(0.0, 1.0);
    [
        sample[0] * alpha,
        sample[1] * alpha,
        sample[2] * alpha,
        alpha,
    ]
}

fn unpremultiply(sample: [f32; 4]) -> [f32; 4] {
    let alpha = sample[3].clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    [
        sample[0] / alpha,
        sample[1] / alpha,
        sample[2] / alpha,
        alpha,
    ]
}

fn luma(sample: [f32; 4]) -> f32 {
    if sample[3].clamp(0.0, 1.0) <= 0.0 {
        return 0.0;
    }
    0.2126 * sample[0] + 0.7152 * sample[1] + 0.0722 * sample[2]
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(parameters: CartoonParameters) -> CartoonGrade {
        CartoonGrade::new(parameters).expect("the fixture grade must validate")
    }

    fn flat(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    fn fill_and_edges() -> CartoonParameters {
        CartoonParameters {
            render: CartoonRender::FillAndEdges,
            ..CartoonParameters::default()
        }
    }

    fn assert_close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        for mutate in [
            |p: &mut CartoonParameters| p.detail_radius = 0.0,
            |p: &mut CartoonParameters| p.detail_radius = 40.5,
            |p: &mut CartoonParameters| p.detail_radius = f32::NAN,
            |p: &mut CartoonParameters| p.detail_threshold = 0.0,
            |p: &mut CartoonParameters| p.detail_threshold = 101.0,
            |p: &mut CartoonParameters| p.shadow_steps = 1.9,
            |p: &mut CartoonParameters| p.shadow_steps = 30.1,
            |p: &mut CartoonParameters| p.shadow_smoothness = 0.0,
            |p: &mut CartoonParameters| p.shadow_smoothness = 101.0,
            |p: &mut CartoonParameters| p.threshold = 0.9,
            |p: &mut CartoonParameters| p.threshold = 10.1,
            |p: &mut CartoonParameters| p.width = 0.0,
            |p: &mut CartoonParameters| p.width = 20.5,
            |p: &mut CartoonParameters| p.softness = -1.0,
            |p: &mut CartoonParameters| p.opacity = 101.0,
            |p: &mut CartoonParameters| p.edge_enhance = -101.0,
            |p: &mut CartoonParameters| p.edge_black_level = 10.5,
            |p: &mut CartoonParameters| p.edge_contrast = 5.5,
            |p: &mut CartoonParameters| p.edge_contrast = f32::INFINITY,
        ] {
            let mut parameters = CartoonParameters::default();
            mutate(&mut parameters);
            assert!(CartoonGrade::new(parameters).is_err());
        }
    }

    #[test]
    fn records_the_error_parameter() {
        let parameters = CartoonParameters { width: -1.0, ..CartoonParameters::default() };
        let error = CartoonGrade::new(parameters).expect_err("width is invalid");
        assert_eq!(error.parameter(), "width");
        assert!(error.to_string().contains("cartoon.width"));
    }

    #[test]
    fn documented_defaults_validate_and_pack_the_controls() {
        let grade = CartoonGrade::documented_defaults();
        assert_eq!(grade.radius(), 8);
        assert_eq!(grade.steps(), 8);
        assert_eq!(grade.edge_width(), 2);
        let (params, extra, aces) = grade.gpu_controls();
        assert_eq!(params[0], 8.0);
        assert_eq!(params[1], 8.0);
        assert_close(params[2], 0.7, 1e-6);
        assert_close(params[3], 0.1, 1e-6);
        assert_close(extra[0], 0.016, 1e-6);
        assert_eq!(extra[1], 2.0);
        assert_close(extra[2], 0.6, 1e-6);
        assert_close(extra[3], 1.0, 1e-6);
        assert_eq!(aces[0], 3.0);
        assert_close(aces[1], 0.01, 1e-6);
        assert_eq!(aces[2], 0.0);
        assert_close(aces[3], 0.5, 1e-6);
    }

    #[test]
    fn edge_only_render_with_zero_opacity_is_an_identity() {
        let parameters = CartoonParameters {
            render: CartoonRender::Edges,
            opacity: 0.0,
            ..CartoonParameters::default()
        };
        let grade = grade(parameters);
        assert!(grade.is_identity());
        let mut frame = flat(5, 4, [0.4, 0.3, 0.2, 1.0]);
        let before = frame.clone();
        assert!(cartoon_frame_f32(&mut frame, 5, 4, &grade));
        assert_eq!(frame, before);
        let default = CartoonGrade::documented_defaults();
        assert!(!default.is_identity());
    }

    #[test]
    fn halo_covers_the_blur_and_the_edge_reach() {
        let grade = grade(CartoonParameters {
            detail_radius: 10.0,
            width: 3.4,
            ..CartoonParameters::default()
        });
        assert_eq!(grade.input_halo(), (15, 15));
    }

    #[test]
    fn flat_frames_quantise_their_tone() {
        let parameters = CartoonParameters {
            shadow_steps: 8.0,
            shadow_smoothness: 100.0,
            ..fill_and_edges()
        };
        let grade = grade(parameters);
        let mut frame = flat(6, 5, [0.4, 0.4, 0.4, 1.0]);
        assert!(cartoon_frame_f32(&mut frame, 6, 5, &grade));
        for pixel in &frame {
            assert_close(pixel[0], 0.375, 1e-6);
            assert_close(pixel[1], 0.375, 1e-6);
            assert_close(pixel[2], 0.375, 1e-6);
            assert_close(pixel[3], 1.0, 1e-6);
        }
    }

    #[test]
    fn smoothness_mixes_the_quantised_tone_back_towards_the_source() {
        let parameters = CartoonParameters {
            shadow_steps: 4.0,
            shadow_smoothness: 50.0,
            detail_threshold: 100.0,
            ..fill_and_edges()
        };
        let grade = grade(parameters);
        let mut frame = flat(4, 4, [0.3, 0.3, 0.3, 1.0]);
        assert!(cartoon_frame_f32(&mut frame, 4, 4, &grade));
        // base is a flat 0.3, quantised to 1/4 = 0.25, then half way back.
        assert_close(frame[0][0], 0.275, 1e-5);
    }

    #[test]
    fn detail_above_the_threshold_keeps_the_source_colour() {
        let grade = grade(CartoonParameters {
            detail_radius: 1.0,
            detail_threshold: 30.0,
            shadow_steps: 4.0,
            shadow_smoothness: 100.0,
            render: CartoonRender::Fill,
            ..CartoonParameters::default()
        });
        let mut frame = flat(9, 9, [0.2, 0.2, 0.2, 1.0]);
        frame[4 * 9 + 4] = [1.0, 1.0, 1.0, 1.0];
        assert!(cartoon_frame_f32(&mut frame, 9, 9, &grade));
        let centre = frame[4 * 9 + 4];
        assert_close(centre[0], 1.0, 1e-4);
        let neighbour = frame[4 * 9 + 3];
        assert_close(neighbour[0], 0.25, 1e-4);
    }

    #[test]
    fn edges_darken_the_boundary_between_two_tones() {
        let grade = grade(CartoonParameters {
            detail_radius: 1.0,
            threshold: 1.0,
            width: 1.0,
            softness: 0.0,
            opacity: 100.0,
            edge_contrast: 5.0,
            render: CartoonRender::Edges,
            ..CartoonParameters::default()
        });
        let mut frame = vec![[1.0f32, 1.0, 1.0, 1.0]; 9 * 5];
        for row in 0..5 {
            for column in 5..9 {
                frame[row * 9 + column] = [0.0, 0.0, 0.0, 1.0];
            }
        }
        assert!(cartoon_frame_f32(&mut frame, 9, 5, &grade));
        let boundary = frame[2 * 9 + 5];
        assert!(boundary[0] < 0.5, "boundary stayed bright: {}", boundary[0]);
        let flat_pixel = frame[2 * 9 + 1];
        assert_close(flat_pixel[0], 1.0, 1e-5);
    }

    #[test]
    fn a_wider_edge_samples_further_from_the_pixel() {
        let narrow = grade(CartoonParameters {
            detail_radius: 1.0,
            threshold: 1.0,
            width: 1.0,
            softness: 0.0,
            edge_contrast: 5.0,
            render: CartoonRender::Edges,
            ..CartoonParameters::default()
        });
        let wide = grade(CartoonParameters { width: 5.0, ..*narrow.parameters() });
        let mut frame = vec![[1.0f32, 1.0, 1.0, 1.0]; 16 * 5];
        for row in 0..5 {
            for column in 11..16 {
                frame[row * 16 + column] = [0.0, 0.0, 0.0, 1.0];
            }
        }
        let mut narrow_frame = frame.clone();
        let mut wide_frame = frame.clone();
        assert!(cartoon_frame_f32(&mut narrow_frame, 16, 5, &narrow));
        assert!(cartoon_frame_f32(&mut wide_frame, 16, 5, &wide));
        // Column six is two pixels wide of flat white for the narrow reach of
        // one, so it stays untouched; the wide reach of five sees the boundary
        // at eleven and darkens it.
        assert_close(narrow_frame[2 * 16 + 6][0], 1.0, 1e-5);
        assert!(wide_frame[2 * 16 + 6][0] < 1.0);
    }

    #[test]
    fn softness_turns_the_edge_cut_into_a_ramp() {
        let hard = grade(CartoonParameters {
            detail_radius: 1.0,
            threshold: 10.0,
            width: 1.0,
            softness: 0.0,
            edge_contrast: 5.0,
            render: CartoonRender::Edges,
            ..CartoonParameters::default()
        });
        let soft = grade(CartoonParameters { softness: 100.0, ..*hard.parameters() });
        let mut frame = vec![[1.0f32, 1.0, 1.0, 1.0]; 21 * 3];
        for row in 0..3 {
            for column in 0..21 {
                let value = 0.5 * (1.0 - column as f32 / 20.0);
                frame[row * 21 + column] = [value, value, value, 1.0];
            }
        }
        let mut hard_frame = frame.clone();
        let mut soft_frame = frame.clone();
        assert!(cartoon_frame_f32(&mut hard_frame, 21, 3, &hard));
        assert!(cartoon_frame_f32(&mut soft_frame, 21, 3, &soft));
        // The gentle ramp never reaches the hard threshold, so only the soft
        // ramp darkens; the hard cut stays untouched.
        let partials = |out: &[[f32; 4]]| {
            out.iter()
                .zip(frame.iter())
                .filter(|(pixel, original)| {
                    if original[0] <= 1e-4 {
                        return false;
                    }
                    let darken = 1.0 - pixel[0].max(0.0) / original[0];
                    darken > 0.01 && darken < 0.99
                })
                .count()
        };
        assert_eq!(partials(&hard_frame), 0);
        assert!(
            partials(&soft_frame) > 0,
            "soft frame kept every tone: {soft_frame:?}"
        );
    }

    #[test]
    fn fill_and_edges_combines_the_quantised_fill_and_the_darkened_edge() {
        let grade = grade(CartoonParameters {
            detail_radius: 1.0,
            shadow_steps: 4.0,
            shadow_smoothness: 100.0,
            detail_threshold: 100.0,
            threshold: 1.0,
            width: 1.0,
            softness: 0.0,
            opacity: 100.0,
            edge_contrast: 5.0,
            render: CartoonRender::FillAndEdges,
            ..CartoonParameters::default()
        });
        let mut frame = flat(9, 5, [0.3, 0.3, 0.3, 1.0]);
        for row in 0..5 {
            for column in 5..9 {
                frame[row * 9 + column] = [0.8, 0.8, 0.8, 1.0];
            }
        }
        assert!(cartoon_frame_f32(&mut frame, 9, 5, &grade));
        let fill_only = frame[8][0];
        assert_close(fill_only, 0.75, 1e-5);
        let boundary = frame[2 * 9 + 5][0];
        assert!(boundary < fill_only);
    }

    #[test]
    fn fill_rendering_ignores_every_edge_parameter() {
        let plain = grade(CartoonParameters {
            render: CartoonRender::Fill,
            edge_contrast: 0.0,
            ..fill_and_edges()
        });
        let armed = grade(CartoonParameters {
            render: CartoonRender::Fill,
            edge_contrast: 5.0,
            opacity: 100.0,
            edge_black_level: 5.0,
            ..fill_and_edges()
        });
        let mut left = flat(7, 4, [0.4, 0.4, 0.4, 1.0]);
        let mut right = left.clone();
        assert!(cartoon_frame_f32(&mut left, 7, 4, &plain));
        assert!(cartoon_frame_f32(&mut right, 7, 4, &armed));
        assert_eq!(left, right);
    }

    #[test]
    fn transparent_pixels_stay_clear_and_alpha_is_preserved() {
        let grade = CartoonGrade::documented_defaults();
        let mut frame = flat(4, 3, [0.5, 0.5, 0.5, 0.75]);
        frame[6] = [0.5, 0.5, 0.5, 0.0];
        assert!(cartoon_frame_f32(&mut frame, 4, 3, &grade));
        assert_eq!(frame[6], [0.0, 0.0, 0.0, 0.0]);
        for (index, pixel) in frame.iter().enumerate() {
            if index != 6 {
                assert_close(pixel[3], 0.75, 1e-5);
            }
        }
    }

    #[test]
    fn empty_or_short_frames_are_rejected() {
        let grade = CartoonGrade::documented_defaults();
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!cartoon_frame_f32(&mut empty, 0, 0, &grade));
        let mut short = flat(4, 4, [0.5, 0.5, 0.5, 1.0]);
        short.truncate(3);
        assert!(!cartoon_frame_f32(&mut short, 4, 4, &grade));
        let mut zero = flat(4, 4, [0.5, 0.5, 0.5, 1.0]);
        assert!(!cartoon_frame_f32(&mut zero, 0, 4, &grade));
    }

    #[test]
    fn the_checkpoint_is_observed_between_row_bands() {
        let grade = CartoonGrade::documented_defaults();
        let mut frame = flat(8, 6, [0.4, 0.4, 0.4, 1.0]);
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), &'static str>(())
        };
        assert!(apply_cartoon_f32_controlled(&mut frame, 8, 6, &grade, &mut checkpoint).unwrap());
        assert!(calls >= 2);
        let mut frame = flat(8, 6, [0.4, 0.4, 0.4, 1.0]);
        let mut failing = || Err::<(), &'static str>("cancelled");
        assert_eq!(
            apply_cartoon_f32_controlled(&mut frame, 8, 6, &grade, &mut failing),
            Err("cancelled")
        );
    }

    #[test]
    fn the_hash_tracks_every_authored_value() {
        use std::collections::hash_map::DefaultHasher;

        let digest = |parameters: CartoonParameters| {
            let mut hasher = DefaultHasher::new();
            grade(parameters).hash_state(&mut hasher);
            hasher.finish()
        };
        let baseline = digest(CartoonParameters::default());
        for mutate in [
            |p: &mut CartoonParameters| p.render = CartoonRender::Fill,
            |p: &mut CartoonParameters| p.detail_radius = 9.0,
            |p: &mut CartoonParameters| p.detail_threshold = 11.0,
            |p: &mut CartoonParameters| p.shadow_steps = 9.0,
            |p: &mut CartoonParameters| p.shadow_smoothness = 71.0,
            |p: &mut CartoonParameters| p.threshold = 2.0,
            |p: &mut CartoonParameters| p.width = 2.0,
            |p: &mut CartoonParameters| p.softness = 61.0,
            |p: &mut CartoonParameters| p.opacity = 99.0,
            |p: &mut CartoonParameters| p.edge_enhance = 2.0,
            |p: &mut CartoonParameters| p.edge_black_level = 1.0,
            |p: &mut CartoonParameters| p.edge_contrast = 1.5,
        ] {
            let mut parameters = CartoonParameters::default();
            mutate(&mut parameters);
            assert_ne!(digest(parameters), baseline);
        }
    }

    #[test]
    fn render_keys_round_trip() {
        for variant in CartoonRender::ALL {
            assert_eq!(CartoonRender::from_key(variant.as_key()), Some(variant));
            assert!(!variant.label().is_empty());
        }
        assert_eq!(CartoonRender::from_key("missing"), None);
        assert_eq!(CartoonRender::DEFAULT, CartoonRender::FillAndEdges);
        assert_eq!([1, 2, 3], CartoonRender::ALL.map(|variant| variant.as_u8()));
    }
}
