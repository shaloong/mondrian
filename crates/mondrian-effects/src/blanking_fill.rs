//! Blanking Fill (TRF-11): extend the content of a crop within the source raster.

use std::hash::{Hash, Hasher};

use glam::{Vec2, Vec4};

use crate::coverage::straight_rgba_from_premultiplied;
use crate::parallel::for_each_row_band;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::premultiply_rgba_controlled;

/// Largest crop or inward warp-band control as a fraction of its raster axis.
pub const BLANKING_FILL_MAX_EDGE: f32 = 0.49;
/// Inclusive Source Zoom range.
pub const BLANKING_FILL_SOURCE_ZOOM_RANGE: [f32; 2] = [1.0, 4.0];
/// Inclusive range of Manual mode's expansion and aspect.
pub const BLANKING_FILL_MANUAL_RANGE: [f32; 2] = [0.25, 4.0];

/// Normalized binomial weights shared with the two GPU blur axes.
pub const BLANKING_FILL_BLUR_WEIGHTS: [f32; 9] = [
    1.0 / 256.0,
    8.0 / 256.0,
    28.0 / 256.0,
    56.0 / 256.0,
    70.0 / 256.0,
    56.0 / 256.0,
    28.0 / 256.0,
    8.0 / 256.0,
    1.0 / 256.0,
];

/// How the image is extended into the blank area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum BlankingFillMode {
    /// Scale each axis independently to the frame.
    #[default]
    Stretch = 1,
    /// Use a single scale that covers the frame.
    Zoom = 2,
    /// Use the authored expansion and aspect.
    Manual = 3,
    /// Stretch the foreground edge strips and preserve the interior.
    Warp = 4,
    /// Fold positions across the crop edges.
    Mirror = 5,
}

impl BlankingFillMode {
    /// The menu's persisted values in display order.
    pub const ALL: [Self; 5] = [
        Self::Stretch,
        Self::Zoom,
        Self::Manual,
        Self::Warp,
        Self::Mirror,
    ];

    /// Stable menu key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Stretch => "stretch",
            Self::Zoom => "zoom",
            Self::Manual => "manual",
            Self::Warp => "warp",
            Self::Mirror => "mirror",
        }
    }

    /// Decode a persisted menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }
}

/// Raw author controls, before resolving linked sides or the actual raster size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlankingFillParameters {
    /// Source zoom within the source crop.
    pub source_zoom: f32,
    /// Left/right and top/bottom independence, respectively.
    pub crop_split: [bool; 2],
    /// Crop fractions in left, top, right, bottom order.
    pub crop: [f32; 4],
    /// Move the cropped foreground to the centre of the raster.
    pub center: bool,
    /// Background or foreground stretch mode.
    pub mode: BlankingFillMode,
    /// Manual background expansion.
    pub expand: f32,
    /// Manual horizontal aspect multiplier.
    pub aspect: f32,
    /// Foreground edge feather strength.
    pub blend_edges: f32,
    /// Separable background blur strength.
    pub background_blur: f32,
    /// Mix of the generated background with the fade swatch.
    pub fade: f32,
    /// Straight linear RGBA fade swatch.
    pub fade_color: [f32; 4],
    /// Shadow opacity before applying swatch alpha.
    pub shadow_strength: f32,
    /// Shadow direction in clockwise degrees from the positive x axis.
    pub shadow_angle: f32,
    /// Shadow offset as a fraction of the short raster axis.
    pub shadow_distance: f32,
    /// Shadow edge softness.
    pub shadow_blur: f32,
    /// Straight linear RGBA shadow swatch.
    pub shadow_color: [f32; 4],
    /// Left/right and top/bottom warp-strip independence.
    pub stretch_split: [bool; 2],
    /// Inward warp bands as nonpositive fractions, in left, top, right, bottom order.
    pub stretch: [f32; 4],
}

impl Default for BlankingFillParameters {
    fn default() -> Self {
        Self {
            source_zoom: 1.0,
            crop_split: [false; 2],
            crop: [0.05, 0.0, 0.05, 0.0],
            center: false,
            mode: BlankingFillMode::Stretch,
            expand: 1.5,
            aspect: 1.0,
            blend_edges: 0.0,
            background_blur: 0.5,
            fade: 0.0,
            fade_color: [1.0; 4],
            shadow_strength: 0.0,
            shadow_angle: 37.5,
            shadow_distance: 0.0,
            shadow_blur: 0.55,
            shadow_color: [0.0, 0.0, 0.0, 1.0],
            stretch_split: [false; 2],
            stretch: [-0.05, 0.0, -0.05, 0.0],
        }
    }
}

/// A malformed Blanking Fill authoring value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlankingFillError {
    /// A numeric parameter is NaN or infinite.
    #[error("Blanking Fill parameter `{field}` must be finite")]
    NonFinite {
        /// Stable authoring key.
        field: &'static str,
    },
    /// A parameter exceeds the authored range.
    #[error("Blanking Fill parameter `{field}` is out of range")]
    OutOfRange {
        /// Stable authoring key.
        field: &'static str,
    },
}

/// Validated controls used by the graph and raster executors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlankingFillGrade(BlankingFillParameters);

impl BlankingFillGrade {
    /// Validate all controls, including the stored values of linked sides.
    pub fn new(parameters: BlankingFillParameters) -> Result<Self, BlankingFillError> {
        let p = parameters;
        for (field, value, [min, max]) in [
            (
                "source_zoom",
                p.source_zoom,
                BLANKING_FILL_SOURCE_ZOOM_RANGE,
            ),
            ("crop_left", p.crop[0], [0.0, BLANKING_FILL_MAX_EDGE]),
            ("crop_top", p.crop[1], [0.0, BLANKING_FILL_MAX_EDGE]),
            ("crop_right", p.crop[2], [0.0, BLANKING_FILL_MAX_EDGE]),
            ("crop_bottom", p.crop[3], [0.0, BLANKING_FILL_MAX_EDGE]),
            ("expand", p.expand, BLANKING_FILL_MANUAL_RANGE),
            ("aspect", p.aspect, BLANKING_FILL_MANUAL_RANGE),
            ("blend_edges", p.blend_edges, [0.0, 1.0]),
            ("background_blur", p.background_blur, [0.0, 1.0]),
            ("fade", p.fade, [0.0, 1.0]),
            ("shadow_strength", p.shadow_strength, [0.0, 1.0]),
            ("shadow_angle", p.shadow_angle, [-180.0, 180.0]),
            ("shadow_distance", p.shadow_distance, [0.0, 1.0]),
            ("shadow_blur", p.shadow_blur, [0.0, 1.0]),
            ("stretch_left", p.stretch[0], [-BLANKING_FILL_MAX_EDGE, 0.0]),
            ("stretch_top", p.stretch[1], [-BLANKING_FILL_MAX_EDGE, 0.0]),
            (
                "stretch_right",
                p.stretch[2],
                [-BLANKING_FILL_MAX_EDGE, 0.0],
            ),
            (
                "stretch_bottom",
                p.stretch[3],
                [-BLANKING_FILL_MAX_EDGE, 0.0],
            ),
        ] {
            validate_value(field, value, min, max)?;
        }
        for (field, color) in [
            ("fade_color", p.fade_color),
            ("shadow_color", p.shadow_color),
        ] {
            for component in color {
                validate_value(field, component, 0.0, 1.0)?;
            }
        }
        Ok(Self(parameters))
    }

    /// Original, unresolved author controls.
    pub const fn parameters(self) -> BlankingFillParameters {
        self.0
    }

    /// Whether this mode warps the foreground directly in one pass.
    pub const fn warps_top_layer(self) -> bool {
        matches!(self.0.mode, BlankingFillMode::Warp)
    }

    /// Whether the entire raster is unchanged and the fill is never exposed.
    pub fn is_identity(self) -> bool {
        self.0.source_zoom == 1.0
            && resolve_sides(self.0.crop, self.0.crop_split) == [0.0; 4]
            && (self.warps_top_layer() || self.0.blend_edges == 0.0)
    }

    /// Hash persisted controls by their original bits and enum values.
    pub(crate) fn hash_controls(self, state: &mut impl Hasher) {
        let p = self.0;
        p.crop_split.hash(state);
        p.center.hash(state);
        (p.mode as u8).hash(state);
        p.stretch_split.hash(state);
        [
            p.source_zoom,
            p.expand,
            p.aspect,
            p.blend_edges,
            p.background_blur,
            p.fade,
            p.shadow_strength,
            p.shadow_angle,
            p.shadow_distance,
            p.shadow_blur,
        ]
        .map(f32::to_bits)
        .hash(state);
        p.crop.map(f32::to_bits).hash(state);
        p.stretch.map(f32::to_bits).hash(state);
        p.fade_color.map(f32::to_bits).hash(state);
        p.shadow_color.map(f32::to_bits).hash(state);
    }

    /// Pack nine vec4 cells from the same raster-dependent controls as the CPU path.
    pub fn gpu_controls(self, width: u32, height: u32) -> [[f32; 4]; 9] {
        let prepared = self.prepare(width as f32, height as f32);
        [
            [
                prepared.extent.x,
                prepared.extent.y,
                prepared.inverse_zoom,
                prepared.blur_step,
            ],
            prepared.source.to_array(),
            prepared.foreground.to_array(),
            prepared.source_texels.to_array(),
            [
                prepared.background_inverse_scale.x,
                prepared.background_inverse_scale.y,
                prepared.feather,
                prepared.mode as u8 as f32,
            ],
            prepared.fade.to_array(),
            prepared.shadow.to_array(),
            [
                prepared.shadow_offset.x,
                prepared.shadow_offset.y,
                prepared.shadow_softness,
                0.0,
            ],
            prepared.stretch.to_array(),
        ]
    }

    fn prepare(self, width: f32, height: f32) -> BlankingFillProjection {
        let p = self.0;
        let extent = Vec2::new(width, height);
        let short = width.min(height);
        let [left, top, right, bottom] = resolve_sides(p.crop, p.crop_split);
        let source = PixelRect {
            min: Vec2::new(left, top) * extent,
            max: Vec2::new(1.0 - right, 1.0 - bottom) * extent,
        };
        let size = source.max - source.min;
        let foreground = if p.center {
            let min = (extent - size) * 0.5;
            PixelRect { min, max: min + size }
        } else {
            source
        };
        let source_texels = source.interior_texels();
        let coverage = size / extent;
        let background_inverse_scale = match p.mode {
            BlankingFillMode::Stretch => coverage,
            BlankingFillMode::Zoom => Vec2::splat(coverage.min_element()),
            BlankingFillMode::Manual => Vec2::new(1.0 / (p.expand * p.aspect), p.expand.recip()),
            BlankingFillMode::Warp | BlankingFillMode::Mirror => Vec2::ONE,
        };
        let [left, top, right, bottom] = resolve_sides(p.stretch, p.stretch_split);
        let stretch = (Vec4::new(-left, -top, -right, -bottom)
            * Vec4::new(width, height, width, height))
        .min(Vec4::new(size.x, size.y, size.x, size.y) * 0.49);
        let (sin, cos) = p.shadow_angle.to_radians().sin_cos();
        let mut fade = Vec4::from_array(p.fade_color);
        fade.w *= p.fade;
        let mut shadow = Vec4::from_array(p.shadow_color);
        shadow.w *= p.shadow_strength;
        BlankingFillProjection {
            extent,
            source,
            foreground,
            source_texels,
            inverse_zoom: p.source_zoom.recip(),
            background_inverse_scale,
            blur_step: (0.04 * short * p.background_blur).min(64.0) * 0.25,
            feather: p.blend_edges * short * 0.08,
            mode: p.mode,
            fade,
            shadow,
            shadow_offset: Vec2::new(cos, sin) * (p.shadow_distance * short),
            shadow_softness: p.shadow_blur * short * 0.08,
            stretch,
        }
    }
}

fn validate_value(
    field: &'static str,
    value: f32,
    min: f32,
    max: f32,
) -> Result<(), BlankingFillError> {
    if !value.is_finite() {
        Err(BlankingFillError::NonFinite { field })
    } else if !(min..=max).contains(&value) {
        Err(BlankingFillError::OutOfRange { field })
    } else {
        Ok(())
    }
}

fn resolve_sides([left, top, right, bottom]: [f32; 4], [lr, tb]: [bool; 2]) -> [f32; 4] {
    [
        left,
        top,
        if lr { right } else { left },
        if tb { bottom } else { top },
    ]
}

#[derive(Clone, Copy)]
struct PixelRect {
    min: Vec2,
    max: Vec2,
}

impl PixelRect {
    fn to_array(self) -> [f32; 4] {
        [self.min.x, self.min.y, self.max.x, self.max.y]
    }

    fn center(self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    fn interior_texels(self) -> Self {
        let min = (self.min - 0.5).ceil();
        let max = (self.max - 0.5).floor();
        // If a subpixel crop contains no centre, take the nearest texel to its midpoint.
        let narrow = min.cmpgt(max);
        let nearest = self.center().floor();
        Self {
            min: Vec2::select(narrow, nearest, min),
            max: Vec2::select(narrow, nearest, max),
        }
    }

    fn contains(self, position: Vec2) -> bool {
        position.cmpge(self.min).all() && position.cmple(self.max).all()
    }
}

struct BlankingFillProjection {
    extent: Vec2,
    source: PixelRect,
    foreground: PixelRect,
    source_texels: PixelRect,
    inverse_zoom: f32,
    background_inverse_scale: Vec2,
    blur_step: f32,
    feather: f32,
    mode: BlankingFillMode,
    fade: Vec4,
    shadow: Vec4,
    shadow_offset: Vec2,
    shadow_softness: f32,
    stretch: Vec4,
}

impl BlankingFillProjection {
    fn foreground_position(&self, position: Vec2) -> Vec2 {
        let source_center = self.source.center();
        let foreground_center = self.foreground.center();
        if self.inverse_zoom == 1.0 && source_center == foreground_center {
            return position;
        }
        source_center + (position - foreground_center) * self.inverse_zoom
    }

    fn background_position(&self, position: Vec2) -> Vec2 {
        if self.mode == BlankingFillMode::Mirror {
            let size = self.foreground.max - self.foreground.min;
            let cycle = (position - self.foreground.min) / size;
            let cycle = cycle - (cycle * 0.5).floor() * 2.0;
            let folded = Vec2::ONE - (cycle - 1.0).abs();
            return self.foreground_position(self.foreground.min + size * folded);
        }
        self.source.center()
            + (position - self.extent * 0.5) * self.background_inverse_scale * self.inverse_zoom
    }

    fn warp_position(&self, position: Vec2) -> Vec2 {
        let inner_min = self.foreground.min + Vec2::new(self.stretch.x, self.stretch.y);
        let inner_max = self.foreground.max - Vec2::new(self.stretch.z, self.stretch.w);
        let low = self.foreground.min
            + (position / inner_min.max(Vec2::splat(1.0e-8))) * (inner_min - self.foreground.min);
        let high = self.foreground.max
            - ((self.extent - position) / (self.extent - inner_max).max(Vec2::splat(1.0e-8)))
                * (self.foreground.max - inner_max);
        let warped = Vec2::select(position.cmplt(inner_min), low, position);
        self.foreground_position(Vec2::select(position.cmpgt(inner_max), high, warped))
    }

    fn foreground_weight(&self, position: Vec2) -> f32 {
        if !self.foreground.contains(position) {
            return 0.0;
        }
        if self.feather == 0.0 {
            return 1.0;
        }
        let inside = (position - self.foreground.min).min(self.foreground.max - position);
        cubic((inside.min_element() / self.feather).clamp(0.0, 1.0))
    }

    fn composite_background(&self, background: Vec4, position: Vec2) -> Vec4 {
        let faded = mix(background, self.fade.truncate().extend(1.0), self.fade.w);
        if self.shadow.w == 0.0 {
            return faded;
        }
        let half_size = (self.foreground.max - self.foreground.min) * 0.5;
        let delta = (position - (self.foreground.center() + self.shadow_offset)).abs() - half_size;
        let distance = delta.max(Vec2::ZERO).length() + delta.max_element().min(0.0);
        let coverage = if self.shadow_softness == 0.0 {
            f32::from(u8::from(distance <= 0.0))
        } else {
            1.0 - cubic((distance / self.shadow_softness).clamp(0.0, 1.0))
        };
        let alpha = coverage * self.shadow.w;
        self.shadow.truncate().extend(1.0) * alpha + faded * (1.0 - alpha)
    }

    fn sample(&self, source: &[[f32; 4]], width: usize, height: usize, position: Vec2) -> Vec4 {
        let texel = (position - 0.5).clamp(self.source_texels.min, self.source_texels.max);
        load_bilinear(source, width, height, texel)
    }
}

fn load_bilinear(source: &[[f32; 4]], width: usize, height: usize, texel: Vec2) -> Vec4 {
    Vec4::from_array(sample_bilinear_clamped(
        source,
        width,
        height,
        f64::from(texel.x),
        f64::from(texel.y),
    ))
}

fn cubic(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}

fn mix(left: Vec4, right: Vec4, weight: f32) -> Vec4 {
    left * (1.0 - weight) + right * weight
}

fn blur_1d(step: f32, sample: impl Fn(f32) -> Vec4) -> Vec4 {
    if step == 0.0 {
        return sample(0.0);
    }
    let mut result = Vec4::ZERO;
    for (index, weight) in BLANKING_FILL_BLUR_WEIGHTS.into_iter().enumerate() {
        result += sample((index as f32 - 4.0) * step) * weight;
    }
    result
}

/// Fill blanking on a straight-alpha Float32 frame without changing its grid.
/// Returns `false` for invalid frame storage.
pub fn blanking_fill_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: BlankingFillGrade,
) -> bool {
    apply_blanking_fill_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

pub(crate) fn apply_blanking_fill_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: BlankingFillGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(pixels.len()) {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    checkpoint()?;
    let projection = grade.prepare(width as f32, height as f32);
    let mut source = pixels.to_vec();
    premultiply_rgba_controlled(&mut source, checkpoint)?;
    if grade.warps_top_layer() {
        render_pixels(pixels, width, height, |position, pixel| {
            let warped = projection.warp_position(position);
            if warped != position || !projection.source_texels.contains(position - 0.5) {
                *pixel = straight_rgba_from_premultiplied(
                    projection.sample(&source, width, height, warped).to_array(),
                );
            }
        });
        checkpoint()?;
        return Ok(true);
    }

    let mut horizontal = vec![[0.0; 4]; pixels.len()];
    render_pixels(&mut horizontal, width, height, |position, pixel| {
        *pixel = blur_1d(projection.blur_step, |offset| {
            let x = (position.x + offset).clamp(0.5, projection.extent.x - 0.5);
            let mapped = projection.background_position(Vec2::new(x, position.y));
            projection.sample(&source, width, height, mapped)
        })
        .to_array();
    });
    checkpoint()?;
    render_pixels(pixels, width, height, |position, pixel| {
        let weight = projection.foreground_weight(position);
        let foreground_position = projection.foreground_position(position);
        if weight == 1.0
            && foreground_position == position
            && projection.source_texels.contains(position - 0.5)
        {
            return;
        }
        let foreground = projection.sample(&source, width, height, foreground_position);
        let texel = position - 0.5;
        let result = if weight == 1.0 {
            foreground
        } else {
            let background = blur_1d(projection.blur_step, |offset| {
                load_bilinear(
                    &horizontal,
                    width,
                    height,
                    Vec2::new(texel.x, texel.y + offset),
                )
            });
            let background = projection.composite_background(background, position);
            mix(background, foreground, weight)
        };
        *pixel = straight_rgba_from_premultiplied(result.to_array());
    });
    checkpoint()?;
    Ok(true)
}

fn render_pixels(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    render: impl Fn(Vec2, &mut [f32; 4]) + Sync,
) {
    for_each_row_band(pixels, width, 0..height, |start, band| {
        for (row, scanline) in band.chunks_mut(width).enumerate() {
            for (column, pixel) in scanline.iter_mut().enumerate() {
                render(
                    Vec2::new(column as f32 + 0.5, (start + row) as f32 + 0.5),
                    pixel,
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(parameters: BlankingFillParameters) -> BlankingFillGrade {
        BlankingFillGrade::new(parameters).unwrap()
    }

    fn gradient(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| [(index % width) as f32, (index / width) as f32, -0.5, 1.0])
            .collect()
    }

    #[test]
    fn blanking_fill_modes_round_trip_stable_keys() {
        for (index, mode) in BlankingFillMode::ALL.into_iter().enumerate() {
            assert_eq!(mode as usize, index + 1);
            assert_eq!(BlankingFillMode::from_key(mode.key()), Some(mode));
        }
        assert_eq!(BlankingFillMode::from_key("unrecognized"), None);
    }

    #[test]
    fn blanking_fill_validates_inactive_controls_and_both_swatches() {
        let defaults = BlankingFillParameters::default();
        for (parameters, error) in [
            (
                BlankingFillParameters { crop: [0.05, 0.0, 0.5, 0.0], ..defaults },
                BlankingFillError::OutOfRange { field: "crop_right" },
            ),
            (
                BlankingFillParameters { stretch: [0.0, 0.0, 0.0, f32::NAN], ..defaults },
                BlankingFillError::NonFinite { field: "stretch_bottom" },
            ),
            (
                BlankingFillParameters { fade_color: [1.0, 1.0, 1.0, 1.1], ..defaults },
                BlankingFillError::OutOfRange { field: "fade_color" },
            ),
            (
                BlankingFillParameters {
                    shadow_color: [f32::INFINITY, 0.0, 0.0, 1.0],
                    ..defaults
                },
                BlankingFillError::NonFinite { field: "shadow_color" },
            ),
        ] {
            assert_eq!(BlankingFillGrade::new(parameters), Err(error));
        }
    }

    #[test]
    fn blanking_fill_defaults_expose_a_fill_and_zero_crop_can_be_identity() {
        let mut frame = [[1.25, -0.0, -0.5, 0.0]; 15];
        let original = frame.map(|pixel| pixel.map(f32::to_bits));
        assert!(!grade(BlankingFillParameters::default()).is_identity());
        let settings = BlankingFillParameters {
            crop: [0.0, 0.0, 0.3, 0.4],
            fade: 1.0,
            shadow_strength: 1.0,
            ..BlankingFillParameters::default()
        };
        let grade = grade(settings);
        assert!(grade.is_identity());
        assert!(blanking_fill_frame_f32(&mut frame, 5, 3, grade));
        assert_eq!(frame.map(|pixel| pixel.map(f32::to_bits)), original);
        assert!(!blanking_fill_frame_f32(&mut frame, 3, 2, grade));
        assert!(!blanking_fill_frame_f32(&mut [], 0, 0, grade));
    }

    #[test]
    fn blanking_fill_resolves_linked_sides_and_recenters_without_changing_the_crop() {
        let p = BlankingFillParameters {
            crop: [0.25, 0.125, 0.125, 0.25],
            crop_split: [true, false],
            center: true,
            source_zoom: 2.0,
            ..BlankingFillParameters::default()
        };
        let projection = grade(p).prepare(16.0, 8.0);
        assert_eq!(projection.source.to_array(), [4.0, 1.0, 14.0, 7.0]);
        assert_eq!(projection.foreground.to_array(), [3.0, 1.0, 13.0, 7.0]);
        assert_eq!(
            projection.foreground_position(Vec2::new(12.0, 6.0)),
            Vec2::new(11.0, 5.0)
        );
        let split =
            grade(BlankingFillParameters { crop_split: [true, true], ..p }).prepare(16.0, 8.0);
        assert_eq!(split.source.to_array(), [4.0, 1.0, 14.0, 6.0]);
        assert_eq!(split.foreground.to_array(), [3.0, 1.5, 13.0, 6.5]);
    }

    #[test]
    fn blanking_fill_background_modes_map_their_own_coordinates() {
        let defaults = BlankingFillParameters {
            crop: [0.25, 0.0, 0.25, 0.0],
            background_blur: 0.0,
            aspect: 2.0,
            ..BlankingFillParameters::default()
        };
        let position = Vec2::new(1.5, 0.5);
        for (mode, expected) in [
            (BlankingFillMode::Stretch, Vec2::new(3.75, 0.5)),
            (BlankingFillMode::Zoom, Vec2::new(3.75, 2.25)),
            (BlankingFillMode::Manual, Vec2::new(4.5, 4.0 - 3.5 / 1.5)),
            (BlankingFillMode::Mirror, Vec2::new(4.5, 0.5)),
        ] {
            let grade = grade(BlankingFillParameters { mode, ..defaults });
            let projection = grade.prepare(12.0, 8.0);
            assert!((projection.background_position(position) - expected).length() < 1.0e-6);
            let mut frame = gradient(12, 8);
            assert!(blanking_fill_frame_f32(&mut frame, 12, 8, grade));
            assert!((frame[1][0] - (expected.x - 0.5)).abs() < 1.0e-6);
            assert!((frame[1][1] - (expected.y - 0.5)).abs() < 1.0e-6);
            assert_eq!(frame[4 * 12 + 6], [6.0, 4.0, -0.5, 1.0]);
        }
    }

    #[test]
    fn blanking_fill_warp_stretches_the_edges_and_preserves_the_interior() {
        let grade = grade(BlankingFillParameters {
            crop: [0.25, 0.0, 0.25, 0.0],
            stretch: [-0.125, 0.0, -0.25, 0.0],
            stretch_split: [true, false],
            mode: BlankingFillMode::Warp,
            fade: 1.0,
            shadow_strength: 1.0,
            ..BlankingFillParameters::default()
        });
        let projection = grade.prepare(16.0, 8.0);
        for x in [6.0, 8.0, 8.08] {
            let point = Vec2::new(x, 4.5);
            assert_eq!(projection.warp_position(point), point);
        }
        let low = projection.warp_position(Vec2::new(3.0, 4.5));
        let high = projection.warp_position(Vec2::new(14.0, 4.5));
        assert_eq!(low, Vec2::new(5.0, 4.5));
        assert!(high.x > 10.0 && high.x < 12.0);
        let mut frame = gradient(16, 8);
        assert!(blanking_fill_frame_f32(&mut frame, 16, 8, grade));
        assert_eq!(frame[4 * 16 + 6], [6.0, 4.0, -0.5, 1.0]);
        assert_eq!(frame[0], [4.0, 0.0, -0.5, 1.0]);
    }

    #[test]
    fn blanking_fill_clamps_to_retained_texels_even_for_a_subpixel_crop() {
        let grade = grade(BlankingFillParameters {
            crop: [0.49; 4],
            ..BlankingFillParameters::default()
        });
        let projection = grade.prepare(8.0, 6.0);
        assert_eq!(projection.source_texels.to_array(), [4.0, 3.0, 4.0, 3.0]);
        let mut frame = gradient(8, 6);
        assert!(blanking_fill_frame_f32(&mut frame, 8, 6, grade));
        assert!(frame.iter().all(|pixel| *pixel == [4.0, 3.0, -0.5, 1.0]));
        let mut single_axis = gradient(1, 7);
        assert!(blanking_fill_frame_f32(&mut single_axis, 1, 7, grade));
        assert!(single_axis.iter().all(|pixel| *pixel == [0.0, 3.0, -0.5, 1.0]));
    }

    #[test]
    fn blanking_fill_feathers_into_the_fade_without_filling_foreground_holes() {
        let grade = grade(BlankingFillParameters {
            crop: [0.25, 0.0, 0.25, 0.0],
            fade: 0.8,
            fade_color: [0.25, 0.5, 0.75, 0.5],
            blend_edges: 1.0,
            ..BlankingFillParameters::default()
        });
        let hidden = [0.25, 1.5, -0.75, 0.0];
        let mut frame = vec![hidden; 20 * 20];
        assert!(blanking_fill_frame_f32(&mut frame, 20, 20, grade));
        assert_eq!(frame[10 * 20], [0.25, 0.5, 0.75, 0.4]);
        let feathered = frame[10 * 20 + 5];
        assert!((0.0..0.4).contains(&feathered[3]) && feathered[3] > 0.0);
        assert_eq!(
            frame[10 * 20 + 10].map(f32::to_bits),
            hidden.map(f32::to_bits)
        );
    }

    #[test]
    fn blanking_fill_composites_the_offset_shadow_only_behind_the_foreground() {
        let grade = grade(BlankingFillParameters {
            crop: [0.25, 0.0, 0.25, 0.0],
            fade: 1.0,
            shadow_strength: 0.5,
            shadow_angle: 0.0,
            shadow_distance: 0.25,
            shadow_blur: 0.0,
            ..BlankingFillParameters::default()
        });
        let mut frame = gradient(12, 8);
        assert!(blanking_fill_frame_f32(&mut frame, 12, 8, grade));
        assert_eq!(frame[4 * 12 + 9], [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(frame[4 * 12 + 11], [1.0; 4]);
        assert_eq!(frame[4 * 12 + 6], [6.0, 4.0, -0.5, 1.0]);
    }

    #[test]
    fn blanking_fill_blurs_both_axes_in_premultiplied_space() {
        let (width, height) = (100, 100);
        for stripe_is_vertical in [true, false] {
            let source: Vec<_> = (0..width * height)
                .map(|index| {
                    let x = index % width;
                    let y = index / width;
                    let covered = if stripe_is_vertical {
                        x % 2 == 0
                    } else {
                        y % 2 == 0
                    };
                    if covered {
                        [2.0, -0.5, 0.25, 0.75]
                    } else {
                        [99.0, 99.0, 99.0, 0.0]
                    }
                })
                .collect();
            let p = BlankingFillParameters {
                crop: [0.2, 0.0, 0.2, 0.0],
                background_blur: 0.0,
                ..BlankingFillParameters::default()
            };
            let mut clear = source.clone();
            let mut blurred = source.clone();
            assert!(blanking_fill_frame_f32(&mut clear, width, height, grade(p)));
            assert!(blanking_fill_frame_f32(
                &mut blurred,
                width,
                height,
                grade(BlankingFillParameters { background_blur: 1.0, ..p })
            ));
            let index = 50 * width + 10;
            assert!((blurred[index][3] - clear[index][3]).abs() > 0.1);
            assert!(blurred[index][3] > 0.0);
            assert!((blurred[index][0] - 2.0).abs() < 1.0e-6);
            assert!((blurred[index][1] + 0.5).abs() < 1.0e-6);
        }
    }
}
