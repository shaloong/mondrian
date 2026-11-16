//! Radial shadow: a layer's alpha cast away from a point light source.
//!
//! Unlike [`crate::DropShadow`], the shadow is not an offset copy: it is
//! projected from a point light onto a plane that sits `projection_distance`
//! behind the layer, so the shadow spreads outwards from the light and grows
//! with the distance.
//!
//! Geometry. Pixels are measured at `(x + 0.5, y + 0.5)`. The light sits at
//! `L = light * (width, height)` and an output pixel `S` samples the source at
//!
//! ```text
//! k = 1 / (1 + projection_distance / 100)
//! A = L + (S - L) * k
//! ```
//!
//! Because `0 < k <= 1`, `A` lies on the segment between the light and the
//! output pixel, so every sample stays inside the frame when the light does.
//! That is why the pass needs no halo. At `projection_distance = 0` the shadow
//! sits exactly under the layer and only shows through semi-transparent pixels;
//! larger distances push the shadow outwards, matching the documented
//! "larger values make the shadow larger".
//!
//! Softness samples a disc of radius `softness / 100 * min(width, height) * 0.1`
//! around `A`: the centre tap counts four times, eight deterministic ring taps
//! count once each, and the sum is normalised by twelve. A softness of zero
//! takes the single centre tap, so the kernel stays a pure resample.
//!
//! Rendering. `regular` paints the shadow with `shadow_color` and `opacity`
//! only. `glass_edge` mixes the layer colour at the sampled point into the
//! shadow colour with `color_influence` (0–2, where 1 is the authored colour
//! mixed at 100%): light passing through semi-transparent pixels tints the
//! shadow. `shadow_only` replaces the frame with the shadow on transparent
//! surroundings; otherwise the source is composited over the shadow.
//!
//! `resize_layer` is accepted for authoring parity. The frame is the canvas in
//! this engine and shadows are never clipped to a layer bounding box, so the
//! switch cannot change the output and is documented as inert.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Documented shadow colour.
pub const RADIAL_SHADOW_DEFAULT_COLOR: [f32; 3] = [0.0, 0.0, 0.0];
/// Documented shadow opacity.
pub const RADIAL_SHADOW_DEFAULT_OPACITY: f32 = 0.5;
/// Documented light position, normalized to the frame.
pub const RADIAL_SHADOW_DEFAULT_LIGHT: [f32; 2] = [0.5, 0.5];
/// Documented projection distance.
pub const RADIAL_SHADOW_DEFAULT_PROJECTION_DISTANCE: f32 = 10.0;
/// Documented softness, in percent.
pub const RADIAL_SHADOW_DEFAULT_SOFTNESS: f32 = 0.0;
/// Documented colour influence.
pub const RADIAL_SHADOW_DEFAULT_COLOR_INFLUENCE: f32 = 1.0;

/// Largest accepted projection distance.
pub const RADIAL_SHADOW_MAX_PROJECTION_DISTANCE: f32 = 100.0;
/// Largest accepted softness, in percent.
pub const RADIAL_SHADOW_MAX_SOFTNESS: f32 = 100.0;
/// Largest accepted colour influence.
pub const RADIAL_SHADOW_MAX_COLOR_INFLUENCE: f32 = 2.0;

/// Softness at 100% blurs over this fraction of the shorter frame side.
const SOFTNESS_FRAME_FRACTION: f32 = 0.1;
/// Eight ring taps around the centre sample. The quarter-turn table is stored
/// as f32 literals so the tap placement is byte-stable across platforms.
#[allow(clippy::approx_constant)]
const RING_TAPS: [f32; 8] = [
    0.0, 0.7853982, 1.5707964, 2.3561945, 3.1415927, 3.926991, 4.712389, 5.4977875,
];
/// Weight of the centre tap relative to one ring tap.
const CENTRE_TAP_WEIGHT: f32 = 4.0;
/// Total tap weight: the centre plus eight ring taps.
const TOTAL_TAP_WEIGHT: f32 = CENTRE_TAP_WEIGHT + RING_TAPS.len() as f32;

/// How the shadow picks up the layer colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadialShadowRender {
    /// Paint the shadow with the authored colour only.
    Regular,
    /// Tint the shadow with the layer colour at the sampled point.
    GlassEdge,
}

impl RadialShadowRender {
    /// Every mode in display order.
    pub const ALL: [Self; 2] = [Self::Regular, Self::GlassEdge];

    /// Default mode.
    pub const DEFAULT: Self = Self::Regular;

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::GlassEdge => "glass_edge",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_key() == key)
    }

    /// Packed tag used by the spatial pass.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Regular => 1,
            Self::GlassEdge => 2,
        }
    }

    /// Chinese menu caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Regular => "常规",
            Self::GlassEdge => "玻璃边缘",
        }
    }
}

/// One authored radial shadow configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialShadowParameters {
    /// Colour of the shadow.
    pub shadow_color: [f32; 3],
    /// Shadow opacity in `0..=1`.
    pub opacity: f32,
    /// Point light position, normalized to the frame.
    pub light: [f32; 2],
    /// Distance between the layer and the shadow plane.
    pub projection_distance: f32,
    /// Shadow edge softness, in percent.
    pub softness: f32,
    /// How the layer colour enters the shadow.
    pub render: RadialShadowRender,
    /// Weight of the layer colour in `glass_edge`, in `0..=2`.
    pub color_influence: f32,
    /// Replace the frame with the shadow alone.
    pub shadow_only: bool,
    /// Accepted for authoring parity; inert in a frame-canvas engine.
    pub resize_layer: bool,
}

impl Default for RadialShadowParameters {
    fn default() -> Self {
        Self {
            shadow_color: RADIAL_SHADOW_DEFAULT_COLOR,
            opacity: RADIAL_SHADOW_DEFAULT_OPACITY,
            light: RADIAL_SHADOW_DEFAULT_LIGHT,
            projection_distance: RADIAL_SHADOW_DEFAULT_PROJECTION_DISTANCE,
            softness: RADIAL_SHADOW_DEFAULT_SOFTNESS,
            render: RadialShadowRender::DEFAULT,
            color_influence: RADIAL_SHADOW_DEFAULT_COLOR_INFLUENCE,
            shadow_only: false,
            resize_layer: false,
        }
    }
}

/// Reports the first authored value that falls outside its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("radial_shadow.{parameter} must be finite and within its documented range")]
pub struct RadialShadowError {
    parameter: &'static str,
}

impl RadialShadowError {
    /// Stable snake_case id of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// The three spatial pass uniform cells produced by
/// [`RadialShadowGrade::gpu_controls`].
pub type RadialShadowGpuControls = ([f32; 4], [f32; 4], [f32; 4]);

/// A validated radial shadow configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialShadowGrade {
    parameters: RadialShadowParameters,
}

impl RadialShadowGrade {
    /// Validates one authored configuration.
    pub fn new(parameters: RadialShadowParameters) -> Result<Self, RadialShadowError> {
        let numeric = [
            ("opacity", parameters.opacity, 0.0, 1.0),
            ("light_x", parameters.light[0], 0.0, 1.0),
            ("light_y", parameters.light[1], 0.0, 1.0),
            (
                "projection_distance",
                parameters.projection_distance,
                0.0,
                RADIAL_SHADOW_MAX_PROJECTION_DISTANCE,
            ),
            (
                "softness",
                parameters.softness,
                0.0,
                RADIAL_SHADOW_MAX_SOFTNESS,
            ),
            (
                "color_influence",
                parameters.color_influence,
                0.0,
                RADIAL_SHADOW_MAX_COLOR_INFLUENCE,
            ),
        ];
        for (parameter, value, minimum, maximum) in numeric {
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(RadialShadowError { parameter });
            }
        }
        for component in parameters.shadow_color {
            if !component.is_finite() || !(0.0..=1.0).contains(&component) {
                return Err(RadialShadowError { parameter: "shadow_color" });
            }
        }
        Ok(Self { parameters })
    }

    /// Documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(RadialShadowParameters::default())
            .expect("the documented radial shadow defaults are in range")
    }

    /// The authored values.
    pub const fn parameters(&self) -> &RadialShadowParameters {
        &self.parameters
    }

    /// Whether the configuration leaves every pixel untouched.
    pub const fn is_identity(&self) -> bool {
        self.parameters.opacity <= 0.0
    }

    /// Relative cost of one spatial pass.
    pub const fn cost(&self) -> u8 {
        4
    }

    /// Input halo. Every sample is clamped to the frame, so no padding is read.
    pub const fn input_halo(&self) -> (u32, u32) {
        (0, 0)
    }

    /// Projection scale applied to the sample point.
    pub fn projection_scale(&self) -> f32 {
        1.0 / (1.0 + self.parameters.projection_distance / 100.0)
    }

    /// Softness radius in pixels for one frame size.
    pub fn softness_radius(&self, width: usize, height: usize) -> f32 {
        let shorter = width.min(height) as f32;
        self.parameters.softness / 100.0 * shorter * SOFTNESS_FRAME_FRACTION
    }

    /// Packs the grade into the spatial pass uniform cells.
    ///
    /// The cells are `[light x, light y, projection scale, softness radius]`,
    /// `[shadow rgb, opacity]`, and `[render tag, colour influence, shadow
    /// only, 0]`. The light stays normalized until the shader multiplies it by
    /// the pass geometry, and the softness radius is resolved by the recorder
    /// because it depends on the frame size.
    pub fn gpu_controls(&self, width: usize, height: usize) -> RadialShadowGpuControls {
        let params = [
            self.parameters.light[0],
            self.parameters.light[1],
            self.projection_scale(),
            self.softness_radius(width, height),
        ];
        let color = [
            self.parameters.shadow_color[0],
            self.parameters.shadow_color[1],
            self.parameters.shadow_color[2],
            self.parameters.opacity,
        ];
        let extra0 = [
            f32::from(self.parameters.render.as_u8()),
            self.parameters.color_influence,
            f32::from(self.parameters.shadow_only),
            0.0,
        ];
        (params, color, extra0)
    }

    /// Feeds the whole configuration into a graph or plan fingerprint.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        for component in self.parameters.shadow_color {
            component.to_bits().hash(state);
        }
        self.parameters.color_influence.to_bits().hash(state);
        self.parameters.opacity.to_bits().hash(state);
        self.parameters.projection_distance.to_bits().hash(state);
        self.parameters.softness.to_bits().hash(state);
        for component in self.parameters.light {
            component.to_bits().hash(state);
        }
        self.parameters.render.as_u8().hash(state);
        self.parameters.shadow_only.hash(state);
        self.parameters.resize_layer.hash(state);
    }
}

/// Applies the radial shadow to one tightly packed frame.
pub fn radial_shadow_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &RadialShadowGrade,
) -> bool {
    apply_radial_shadow_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Cancellation-aware radial shadow kernel over one tightly packed frame.
pub(crate) fn apply_radial_shadow_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &RadialShadowGrade,
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
    let source: Vec<[f32; 4]> = working[..pixel_count].to_vec();
    let light = [
        parameters.light[0] * width as f32,
        parameters.light[1] * height as f32,
    ];
    let scale = grade.projection_scale();
    let soft_radius = grade.softness_radius(width, height);
    let opacity = parameters.opacity;
    let shadow_color = parameters.shadow_color;
    let render = parameters.render;
    let color_influence = parameters.color_influence;
    let shadow_only = parameters.shadow_only;

    crate::parallel::for_each_row_band(working, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let position = [column as f32 + 0.5, row as f32 + 0.5];
            let sample =
                project_sample(&source, width, height, light, position, scale, soft_radius);
            let shadow_alpha = sample[3] * opacity;
            let mut shade = [0.0_f32; 4];
            for channel in 0..3 {
                shade[channel] = match render {
                    RadialShadowRender::Regular => shadow_color[channel],
                    RadialShadowRender::GlassEdge => {
                        shadow_color[channel]
                            + (sample[channel] - shadow_color[channel]) * color_influence
                    }
                };
            }
            shade[3] = shadow_alpha;

            if shadow_only {
                *pixel = if shadow_alpha > 0.0 { shade } else { [0.0; 4] };
                continue;
            }
            let source_alpha = pixel[3].clamp(0.0, 1.0);
            let clamped_shadow = shadow_alpha.clamp(0.0, 1.0);
            let out_alpha = source_alpha + clamped_shadow * (1.0 - source_alpha);
            if out_alpha <= f32::EPSILON {
                *pixel = [0.0; 4];
                continue;
            }
            for channel in 0..3 {
                let source_value = pixel[channel] * source_alpha;
                let shade_value = shade[channel] * clamped_shadow * (1.0 - source_alpha);
                pixel[channel] = (source_value + shade_value) / out_alpha;
            }
            pixel[3] = out_alpha;
        }
    });

    checkpoint()?;
    Ok(true)
}

/// Projects one output position onto the shadow plane and averages the taps.
fn project_sample(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    light: [f32; 2],
    position: [f32; 2],
    scale: f32,
    soft_radius: f32,
) -> [f32; 4] {
    let centre = [
        light[0] + (position[0] - light[0]) * scale,
        light[1] + (position[1] - light[1]) * scale,
    ];
    if soft_radius <= 0.0 {
        return sample_straight(source, width, height, centre[0], centre[1]);
    }
    let mut result = [0.0_f32; 4];
    let centre_tap = sample_straight(source, width, height, centre[0], centre[1]);
    for channel in 0..4 {
        result[channel] = centre_tap[channel] * CENTRE_TAP_WEIGHT;
    }
    for angle in RING_TAPS {
        let tap = [
            centre[0] + soft_radius * angle.cos(),
            centre[1] + soft_radius * angle.sin(),
        ];
        let sampled = sample_straight(source, width, height, tap[0], tap[1]);
        for channel in 0..4 {
            result[channel] += sampled[channel];
        }
    }
    for value in &mut result {
        *value /= TOTAL_TAP_WEIGHT;
    }
    result
}

/// Bilinear straight-RGBA sample with clamped coordinates.
fn sample_straight(source: &[[f32; 4]], width: usize, height: usize, x: f32, y: f32) -> [f32; 4] {
    let max_x = width.saturating_sub(1) as f32;
    let max_y = height.saturating_sub(1) as f32;
    let clamped_x = x.clamp(0.0, max_x);
    let clamped_y = y.clamp(0.0, max_y);
    let x0 = clamped_x.floor();
    let y0 = clamped_y.floor();
    let fx = clamped_x - x0;
    let fy = clamped_y - y0;
    let column = x0 as usize;
    let row = y0 as usize;
    let next_column = (column + 1).min(width - 1);
    let next_row = (row + 1).min(height - 1);
    let top_left = source[row * width + column];
    let top_right = source[row * width + next_column];
    let bottom_left = source[next_row * width + column];
    let bottom_right = source[next_row * width + next_column];
    let mut result = [0.0_f32; 4];
    for channel in 0..4 {
        let top = top_left[channel] * (1.0 - fx) + top_right[channel] * fx;
        let bottom = bottom_left[channel] * (1.0 - fx) + bottom_right[channel] * fx;
        result[channel] = top * (1.0 - fy) + bottom * fy;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(parameters: RadialShadowParameters) -> RadialShadowGrade {
        RadialShadowGrade::new(parameters).expect("valid radial shadow grade")
    }

    fn rejected(parameters: RadialShadowParameters) -> RadialShadowError {
        RadialShadowGrade::new(parameters).expect_err("the parameters must be rejected")
    }

    fn filled(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.2, 0.4, 0.6, 1.0]; width * height]
    }

    fn pixel(frame: &[[f32; 4]], width: usize, column: usize, row: usize) -> [f32; 4] {
        frame[row * width + column]
    }

    fn apply(
        frame: &mut [[f32; 4]],
        width: usize,
        height: usize,
        parameters: RadialShadowParameters,
    ) {
        let grade = grade(parameters);
        assert!(radial_shadow_frame_f32(frame, width, height, &grade));
    }

    #[test]
    fn documented_defaults_are_in_range() {
        let grade = RadialShadowGrade::documented_defaults();
        assert_eq!(grade.parameters().shadow_color, RADIAL_SHADOW_DEFAULT_COLOR);
        assert_eq!(grade.parameters().opacity, RADIAL_SHADOW_DEFAULT_OPACITY);
        assert_eq!(grade.parameters().light, RADIAL_SHADOW_DEFAULT_LIGHT);
        assert_eq!(grade.parameters().render, RadialShadowRender::Regular);
        assert!(!grade.is_identity());
        assert_eq!(grade.cost(), 4);
        assert_eq!(grade.input_halo(), (0, 0));
    }

    #[test]
    fn projection_scale_is_one_without_distance_and_halves_at_the_limit() {
        let mut parameters =
            RadialShadowParameters { projection_distance: 0.0, ..Default::default() };
        assert_eq!(grade(parameters).projection_scale(), 1.0);
        parameters.projection_distance = 50.0;
        assert!((grade(parameters).projection_scale() - 2.0 / 3.0).abs() < 1.0e-6);
        parameters.projection_distance = 100.0;
        assert_eq!(grade(parameters).projection_scale(), 0.5);
    }

    #[test]
    fn zero_distance_reproduces_the_source_alpha() {
        let width = 8;
        let height = 6;
        let mut frame = filled(width, height);
        for (index, pixel) in frame.iter_mut().enumerate() {
            if index % width >= 4 {
                pixel[3] = 0.0;
            }
        }
        let parameters = RadialShadowParameters {
            projection_distance: 0.0,
            opacity: 1.0,
            shadow_only: true,
            ..Default::default()
        };
        apply(&mut frame, width, height, parameters);
        assert!((pixel(&frame, width, 2, 2)[3] - 1.0).abs() < 1.0e-6);
        assert_eq!(pixel(&frame, width, 0, 0)[3], 1.0);
        assert_eq!(pixel(&frame, width, 6, 2)[0], 0.0);
        assert!((pixel(&frame, width, 2, 2)[0]).abs() < 1.0e-6);
    }

    #[test]
    fn distance_pulls_the_sample_towards_the_light() {
        let width = 8;
        let height = 1;
        let mut frame = vec![[0.0_f32, 0.0, 0.0, 0.0]; width];
        for (column, pixel) in frame.iter_mut().enumerate() {
            pixel[3] = column as f32 / 8.0;
        }
        let parameters = RadialShadowParameters {
            light: [0.0, 0.5],
            projection_distance: 100.0,
            opacity: 1.0,
            shadow_only: true,
            ..Default::default()
        };
        apply(&mut frame, width, height, parameters);
        // k = 0.5 and the light sits on x = 0, so output column 4 samples x =
        // 2.25, which is a quarter of the way from column 2 to column 3.
        let expected = 0.75 * (2.0 / 8.0) + 0.25 * (3.0 / 8.0);
        assert!((pixel(&frame, width, 4, 0)[3] - expected).abs() < 1.0e-6);
    }

    #[test]
    fn softness_blurs_the_shadow_edge() {
        let width = 12;
        let height = 12;
        let mut hard = vec![[0.0_f32, 0.0, 0.0, 0.0]; width * height];
        for (index, pixel) in hard.iter_mut().enumerate() {
            if index % width < 6 {
                pixel[3] = 1.0;
            }
        }
        let mut soft = hard.clone();
        let mut parameters = RadialShadowParameters {
            light: [0.0, 0.5],
            projection_distance: 0.0,
            opacity: 1.0,
            shadow_only: true,
            ..Default::default()
        };
        apply(&mut hard, width, height, parameters);
        parameters.softness = 100.0;
        apply(&mut soft, width, height, parameters);
        assert_eq!(pixel(&hard, width, 6, 6)[3], 0.0);
        let blurred = pixel(&soft, width, 6, 6)[3];
        assert!(blurred > 0.0 && blurred < 0.9, "blurred {blurred}");
        // Far from the boundary the shadow still matches the hard version.
        assert!((pixel(&soft, width, 1, 6)[3] - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn zero_softness_takes_a_single_tap() {
        let width = 7;
        let height = 7;
        let mut frame = vec![[0.0_f32, 0.0, 0.0, 0.0]; width * height];
        frame[(3 * width) + 3] = [0.9, 0.9, 0.9, 1.0];
        let source = frame.clone();
        let parameters = RadialShadowParameters {
            light: [0.0, 0.0],
            opacity: 1.0,
            shadow_only: true,
            ..Default::default()
        };
        apply(&mut frame, width, height, parameters);
        for (index, output) in frame.iter().enumerate() {
            let column = index % width;
            let row = index / width;
            let sample_x = (column as f32 + 0.5) * grade(parameters).projection_scale();
            let sample_y = (row as f32 + 0.5) * grade(parameters).projection_scale();
            let expected = sample_straight(&source, width, height, sample_x, sample_y)[3];
            assert!(
                (output[3] - expected).abs() < 1.0e-6,
                "pixel ({column}, {row})"
            );
        }
    }

    #[test]
    fn glass_edge_mixes_the_layer_colour() {
        let mut frame = vec![[0.8_f32, 0.2, 0.1, 1.0]; 4];
        let parameters = RadialShadowParameters {
            projection_distance: 0.0,
            opacity: 1.0,
            shadow_only: true,
            render: RadialShadowRender::GlassEdge,
            color_influence: 2.0,
            ..Default::default()
        };
        apply(&mut frame, 4, 1, parameters);
        assert!((frame[0][0] - 1.6).abs() < 1.0e-6);
        assert!((frame[0][1] - 0.4).abs() < 1.0e-6);
        assert!((frame[0][2] - 0.2).abs() < 1.0e-6);
    }

    #[test]
    fn regular_rendering_ignores_the_colour_influence() {
        let mut frame = vec![[0.8_f32, 0.2, 0.1, 1.0]; 4];
        let parameters = RadialShadowParameters {
            projection_distance: 0.0,
            opacity: 1.0,
            shadow_only: true,
            color_influence: 2.0,
            ..Default::default()
        };
        apply(&mut frame, 4, 1, parameters);
        assert_eq!(frame[0], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn empty_source_leaves_a_transparent_shadow() {
        let mut frame = vec![[0.0_f32; 4]; 16];
        let parameters = RadialShadowParameters { shadow_only: true, ..Default::default() };
        apply(&mut frame, 4, 4, parameters);
        assert!(frame.iter().all(|pixel| *pixel == [0.0; 4]));
    }

    #[test]
    fn opaque_source_hides_the_shadow() {
        let mut frame = filled(4, 4);
        let parameters = RadialShadowParameters {
            shadow_color: [1.0, 1.0, 1.0],
            opacity: 1.0,
            ..Default::default()
        };
        apply(&mut frame, 4, 4, parameters);
        for pixel in &frame {
            assert!((pixel[0] - 0.2).abs() < 1.0e-6);
            assert!((pixel[1] - 0.4).abs() < 1.0e-6);
            assert!((pixel[2] - 0.6).abs() < 1.0e-6);
            assert!((pixel[3] - 1.0).abs() < 1.0e-6);
        }
    }

    #[test]
    fn semi_transparent_source_shows_the_shadow_through_it() {
        let mut frame = vec![[1.0_f32, 1.0, 1.0, 0.5]; 1];
        let parameters = RadialShadowParameters {
            projection_distance: 0.0,
            opacity: 1.0,
            shadow_color: [0.0, 0.0, 0.0],
            ..Default::default()
        };
        apply(&mut frame, 1, 1, parameters);
        assert!((frame[0][3] - 0.75).abs() < 1.0e-6);
        assert!((frame[0][0] - 2.0 / 3.0).abs() < 1.0e-6);
    }

    #[test]
    fn zero_opacity_is_an_identity() {
        let mut frame = filled(5, 3);
        let before = frame.clone();
        let parameters = RadialShadowParameters { opacity: 0.0, ..Default::default() };
        assert!(grade(parameters).is_identity());
        apply(&mut frame, 5, 3, parameters);
        assert_eq!(frame, before);
    }

    #[test]
    fn resize_layer_is_accepted_but_inert() {
        let mut left = filled(5, 5);
        let mut right = left.clone();
        let mut parameters = RadialShadowParameters { resize_layer: false, ..Default::default() };
        apply(&mut left, 5, 5, parameters);
        parameters.resize_layer = true;
        apply(&mut right, 5, 5, parameters);
        assert_eq!(left, right);
    }

    #[test]
    fn degenerate_frames_are_rejected() {
        let grade = RadialShadowGrade::documented_defaults();
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!radial_shadow_frame_f32(&mut empty, 0, 0, &grade));
        let mut short = vec![[0.0_f32; 4]; 3];
        assert!(!radial_shadow_frame_f32(&mut short, 4, 4, &grade));
    }

    #[test]
    fn hash_state_tracks_every_field() {
        fn digest(parameters: RadialShadowParameters) -> u64 {
            use std::collections::hash_map::DefaultHasher;
            let mut hasher = DefaultHasher::new();
            grade(parameters).hash_state(&mut hasher);
            hasher.finish()
        }

        let base = digest(RadialShadowParameters::default());
        let variants = [
            RadialShadowParameters {
                shadow_color: [0.5, 0.0, 0.0],
                ..RadialShadowParameters::default()
            },
            RadialShadowParameters { opacity: 0.9, ..RadialShadowParameters::default() },
            RadialShadowParameters {
                light: [0.1, 0.2],
                ..RadialShadowParameters::default()
            },
            RadialShadowParameters {
                projection_distance: 42.0,
                ..RadialShadowParameters::default()
            },
            RadialShadowParameters {
                softness: 30.0,
                ..RadialShadowParameters::default()
            },
            RadialShadowParameters {
                render: RadialShadowRender::GlassEdge,
                ..RadialShadowParameters::default()
            },
            RadialShadowParameters {
                color_influence: 0.0,
                ..RadialShadowParameters::default()
            },
            RadialShadowParameters {
                shadow_only: true,
                ..RadialShadowParameters::default()
            },
            RadialShadowParameters {
                resize_layer: true,
                ..RadialShadowParameters::default()
            },
        ];
        for variant in variants {
            assert_ne!(digest(variant), base);
        }
    }

    #[test]
    fn errors_name_the_offending_parameter() {
        let mut parameters = RadialShadowParameters { opacity: 2.0, ..Default::default() };
        assert_eq!(rejected(parameters).parameter(), "opacity");
        parameters = RadialShadowParameters::default();
        parameters.light = [1.5, 0.0];
        assert_eq!(rejected(parameters).parameter(), "light_x");
        parameters = RadialShadowParameters::default();
        parameters.projection_distance = 101.0;
        assert_eq!(rejected(parameters).parameter(), "projection_distance");
        parameters = RadialShadowParameters::default();
        parameters.softness = 101.0;
        assert_eq!(rejected(parameters).parameter(), "softness");
        parameters = RadialShadowParameters::default();
        parameters.color_influence = 2.5;
        assert_eq!(rejected(parameters).parameter(), "color_influence");
        parameters = RadialShadowParameters::default();
        parameters.shadow_color = [0.0, 1.5, 0.0];
        assert_eq!(rejected(parameters).parameter(), "shadow_color");
    }

    #[test]
    fn render_menu_round_trips() {
        for render in RadialShadowRender::ALL {
            assert_eq!(RadialShadowRender::from_key(render.as_key()), Some(render));
            assert!(!render.label().is_empty());
        }
        assert_eq!(RadialShadowRender::Regular.as_u8(), 1);
        assert_eq!(RadialShadowRender::GlassEdge.as_u8(), 2);
        assert_eq!(RadialShadowRender::from_key("nope"), None);
    }
}
