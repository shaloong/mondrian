//! Texturize (STY-08): light the frame with a second layer used as a height map.
//!
//! The authored controls are the auxiliary layer slot that carries the height
//! map, the light direction in degrees, how strongly the height differences
//! shade the frame, and how a differently sized map is placed over the primary
//! frame.
//!
//! The map contributes one number per pixel: the Rec. 709 luminance of its
//! straight sample. The kernel takes a centre difference on the map's own pixel
//! grid, `du = t(u + 1 texel) - t(u - 1 texel)` and the same along `v`, projects
//! the gradient onto the light direction, and scales it by the authored contrast:
//! `relief = (du * cos(theta) + dv * sin(theta)) * contrast`. The shade is
//! `clamp(1 + relief, 0, 2)`, so a flat map or a zero contrast leaves the frame
//! untouched, and the light direction decides which slope brightens the frame.
//!
//! The primary frame is multiplied channel by channel by that single scalar and
//! keeps its coverage; no colour value is clamped, so a high contrast may push
//! the result above one for later tone mapping. A pixel whose coverage is zero
//! is normalized to transparent black. Tiled differences are taken inside the
//! `fract` domain, so they stay continuous across the tile borders; centred
//! samples outside the map window are transparent and contribute no relief.

use std::hash::{Hash, Hasher};

use crate::auxiliary::{sample_auxiliary_f32, AuxiliaryFloatFrame, PIXEL_CENTRE};
use crate::graph::{AuxiliaryCoordinateMapping, AuxiliarySampling};

/// Lowest accepted auxiliary layer slot; `0` means no height map.
pub const TEXTURIZE_MIN_LAYER: u32 = 0;
/// Highest accepted auxiliary layer slot.
pub const TEXTURIZE_MAX_LAYER: u32 = 19;
/// Documented default auxiliary layer slot.
pub const TEXTURIZE_DEFAULT_LAYER: u32 = 0;
/// Documented default light direction, in degrees.
pub const TEXTURIZE_DEFAULT_LIGHT_DIRECTION: f32 = 135.0;
/// Lowest accepted light direction, in degrees.
pub const TEXTURIZE_MIN_LIGHT_DIRECTION: f32 = 0.0;
/// Highest accepted light direction, in degrees.
pub const TEXTURIZE_MAX_LIGHT_DIRECTION: f32 = 360.0;
/// Lowest accepted texture contrast.
pub const TEXTURIZE_MIN_CONTRAST: f32 = 0.0;
/// Highest accepted texture contrast.
pub const TEXTURIZE_MAX_CONTRAST: f32 = 2.0;
/// Documented default texture contrast.
pub const TEXTURIZE_DEFAULT_CONTRAST: f32 = 1.0;
/// Reach of the kernel in source pixels, in both directions.
pub const TEXTURIZE_HALO_PIXELS: u32 = 2;

/// Authored placement of a differently sized height map over the primary frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexturizePlacement {
    /// Repeat the map at its own pixel scale.
    Tile,
    /// Centre the map at its own pixel size, with transparent surroundings.
    Center,
    /// Stretch the map onto the primary frame.
    Stretch,
}

impl TexturizePlacement {
    /// Every authored placement, in menu order.
    pub const ALL: [Self; 3] = [Self::Tile, Self::Center, Self::Stretch];

    /// Stable code used by the graph identity byte stream.
    pub const fn code(self) -> u8 {
        match self {
            Self::Tile => 1,
            Self::Center => 2,
            Self::Stretch => 3,
        }
    }

    /// The placement for one identity code.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Tile),
            2 => Some(Self::Center),
            3 => Some(Self::Stretch),
            _ => None,
        }
    }

    /// Stable key used by the authored properties.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Tile => "tile",
            Self::Center => "center",
            Self::Stretch => "stretch",
        }
    }

    /// The placement for one authored key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "tile" => Some(Self::Tile),
            "center" => Some(Self::Center),
            "stretch" => Some(Self::Stretch),
            _ => None,
        }
    }

    /// Localized menu label, matching the authored properties.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tile => "拼贴纹理",
            Self::Center => "居中纹理",
            Self::Stretch => "拉伸纹理以适合",
        }
    }
}

/// One authored texturize control outside its documented range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexturizeError {
    /// The auxiliary layer slot is outside `0..=19`.
    TextureLayerOutOfRange,
    /// The light direction is not finite or outside `0..=360`.
    LightDirectionOutOfRange,
    /// The texture contrast is not finite or outside `0..=2`.
    TextureContrastOutOfRange,
}

impl TexturizeError {
    /// The authored property that was rejected.
    pub const fn parameter(self) -> &'static str {
        match self {
            Self::TextureLayerOutOfRange => "texture_layer",
            Self::LightDirectionOutOfRange => "light_direction",
            Self::TextureContrastOutOfRange => "texture_contrast",
        }
    }
}

impl std::fmt::Display for TexturizeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "texturize.{} must be finite and within its documented range",
            self.parameter()
        )
    }
}

impl std::error::Error for TexturizeError {}

/// Validated texturize controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexturizeGrade {
    texture_layer: u32,
    light_direction: f32,
    texture_contrast: f32,
    texture_placement: TexturizePlacement,
}

impl TexturizeGrade {
    /// Validate one authored control set.
    pub fn new(
        texture_layer: u32,
        light_direction: f32,
        texture_contrast: f32,
        texture_placement: TexturizePlacement,
    ) -> Result<Self, TexturizeError> {
        if !(TEXTURIZE_MIN_LAYER..=TEXTURIZE_MAX_LAYER).contains(&texture_layer) {
            return Err(TexturizeError::TextureLayerOutOfRange);
        }
        if !light_direction.is_finite()
            || !(TEXTURIZE_MIN_LIGHT_DIRECTION..=TEXTURIZE_MAX_LIGHT_DIRECTION)
                .contains(&light_direction)
        {
            return Err(TexturizeError::LightDirectionOutOfRange);
        }
        if !texture_contrast.is_finite()
            || !(TEXTURIZE_MIN_CONTRAST..=TEXTURIZE_MAX_CONTRAST).contains(&texture_contrast)
        {
            return Err(TexturizeError::TextureContrastOutOfRange);
        }
        Ok(Self {
            texture_layer,
            light_direction,
            texture_contrast,
            texture_placement,
        })
    }

    /// The documented defaults: no layer, 135 degrees, unit contrast, tiled.
    pub const fn documented_defaults() -> Self {
        Self {
            texture_layer: TEXTURIZE_DEFAULT_LAYER,
            light_direction: TEXTURIZE_DEFAULT_LIGHT_DIRECTION,
            texture_contrast: TEXTURIZE_DEFAULT_CONTRAST,
            texture_placement: TexturizePlacement::Tile,
        }
    }

    /// Auxiliary layer slot that carries the height map.
    pub const fn texture_layer(self) -> u32 {
        self.texture_layer
    }

    /// Light direction, in degrees.
    pub const fn light_direction(self) -> f32 {
        self.light_direction
    }

    /// How strongly the height differences shade the frame.
    pub const fn texture_contrast(self) -> f32 {
        self.texture_contrast
    }

    /// Authored placement of a differently sized height map.
    pub const fn texture_placement(self) -> TexturizePlacement {
        self.texture_placement
    }

    /// Whether the authored controls leave the frame untouched.
    pub const fn is_identity(self) -> bool {
        self.texture_layer == 0 || self.texture_contrast <= 0.0
    }

    /// Packed controls shared with the GPU spatial pass.
    ///
    /// `params = [texture_contrast, cos(theta), sin(theta), placement.code()]` —
    /// the placement code sits in the third lane because the renderer overwrites
    /// the fourth lane with the linear filter flag. The trigonometry is done here
    /// so both backends shade with the very same cosine and sine.
    ///
    /// `extra = [W / Wt, H / Ht, 0, 0]`: the two pixel-scale ratios depend on
    /// both rasters' extents, which the grade does not know, so they are
    /// placeholders the renderer fills from the bound textures. The kernel
    /// derives the same ratios from the frame extents.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        let radians = self.light_direction.to_radians();
        (
            [
                self.texture_contrast,
                radians.cos(),
                radians.sin(),
                self.texture_placement.code() as f32,
            ],
            [0.0; 4],
        )
    }

    /// Hash every authored control into the render operation's signature.
    pub fn hash_state<H: Hasher>(&self, state: &mut H) {
        self.texture_layer.hash(state);
        self.light_direction.to_bits().hash(state);
        self.texture_contrast.to_bits().hash(state);
        self.texture_placement.hash(state);
    }
}

impl Hash for TexturizeGrade {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash_state(state);
    }
}

/// Rows between two cooperative checkpoints.
const CHECKPOINT_STRIDE: usize = 4096;

/// Applies the texturize shade to one Float32 frame with a cooperative checkpoint.
///
/// Returns `Ok(false)` when the buffers are inconsistent, and `Ok(true)` after
/// writing the whole frame.
pub fn apply_texturize_controlled<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    texture: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &TexturizeGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    checkpoint()?;
    let Some(pixel_count) = (width as usize).checked_mul(height as usize) else {
        return Ok(false);
    };
    if pixel_count == 0 || primary.len() != pixel_count {
        return Ok(false);
    }
    let extent = texture.frame.extent();
    if extent.is_empty()
        || (extent.width() as usize).checked_mul(extent.height() as usize)
            != Some(texture.frame.pixels().len())
    {
        return Ok(false);
    }

    // The placement transform is the effect's own, so the sampler must read the
    // plain primary grid: any delivery affine would be applied a second time.
    let grid_sampling = AuxiliarySampling::new(
        AuxiliaryCoordinateMapping::PrimaryGrid,
        sampling.filter,
        sampling.out_of_bounds,
    );

    // Taking the cosine and the sine from the packed controls keeps the CPU and
    // the GPU pass on the same numbers.
    let (params, _) = grade.gpu_controls();
    let contrast = params[0];
    let cos_theta = params[1];
    let sin_theta = params[2];
    let placement = grade.texture_placement();

    let frame_width = width as usize;
    let frame_height = height as usize;
    let texture_width = extent.width() as f32;
    let texture_height = extent.height() as f32;
    let scale = [
        frame_width as f32 / texture_width,
        frame_height as f32 / texture_height,
    ];
    let step = [1.0 / texture_width, 1.0 / texture_height];

    // The rows are split into checkpoint-sized chunks so the kernel reports
    // progress about every `CHECKPOINT_STRIDE` rows without threading the
    // checkpoint into the parallel worker closure.
    let mut row_start = 0;
    while row_start < frame_height {
        let rows_end = (row_start + CHECKPOINT_STRIDE).min(frame_height);
        crate::parallel::for_each_row_band(
            primary,
            frame_width,
            row_start..rows_end,
            |start, band| {
                for (row_offset, row_pixels) in band.chunks_mut(frame_width).enumerate() {
                    let y = start + row_offset;
                    let v = (y as f32 + PIXEL_CENTRE) / frame_height as f32;
                    for (column, pixel) in row_pixels.iter_mut().enumerate() {
                        let u = (column as f32 + PIXEL_CENTRE) / frame_width as f32;
                        let uv = texturize_uv([u, v], placement, scale);
                        // The centre difference lives on the map's own texel grid;
                        // a tiled map shifts inside the `fract` domain so the
                        // difference crosses a tile border without a seam.
                        let du = texturize_luma(
                            texture,
                            texturize_shift(uv, placement, [step[0], 0.0]),
                            grid_sampling,
                            placement,
                        ) - texturize_luma(
                            texture,
                            texturize_shift(uv, placement, [-step[0], 0.0]),
                            grid_sampling,
                            placement,
                        );
                        let dv = texturize_luma(
                            texture,
                            texturize_shift(uv, placement, [0.0, step[1]]),
                            grid_sampling,
                            placement,
                        ) - texturize_luma(
                            texture,
                            texturize_shift(uv, placement, [0.0, -step[1]]),
                            grid_sampling,
                            placement,
                        );
                        let relief = (du * cos_theta + dv * sin_theta) * contrast;
                        let shade = (1.0 + relief).clamp(0.0, 2.0);
                        let alpha = pixel[3];
                        // A single scalar multiplies every colour channel, so the
                        // premultiplied coverage stays exactly the authored one. A
                        // transparent pixel carries no colour at all.
                        *pixel = if alpha <= 0.0 {
                            [0.0; 4]
                        } else {
                            [pixel[0] * shade, pixel[1] * shade, pixel[2] * shade, alpha]
                        };
                    }
                }
            },
        );
        checkpoint()?;
        row_start = rows_end;
    }
    Ok(true)
}

/// Applies the texturize shade to one Float32 frame.
///
/// Returns `false` when the buffers are inconsistent.
pub fn apply_texturize_f32(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    texture: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &TexturizeGrade,
) -> bool {
    apply_texturize_controlled(
        primary,
        width,
        height,
        texture,
        sampling,
        grade,
        &mut || Ok::<(), std::convert::Infallible>(()),
    )
    .unwrap_or(false)
}

/// Map one primary uv onto the height map's own uv under the authored placement.
///
/// `scale` is `[W / Wt, H / Ht]`: the number of map repeats along each axis.
pub(crate) fn texturize_uv(
    uv: [f32; 2],
    placement: TexturizePlacement,
    scale: [f32; 2],
) -> [f32; 2] {
    match placement {
        TexturizePlacement::Tile => [
            (uv[0] * scale[0]).rem_euclid(1.0),
            (uv[1] * scale[1]).rem_euclid(1.0),
        ],
        TexturizePlacement::Center => [
            PIXEL_CENTRE + (uv[0] - PIXEL_CENTRE) * scale[0],
            PIXEL_CENTRE + (uv[1] - PIXEL_CENTRE) * scale[1],
        ],
        TexturizePlacement::Stretch => uv,
    }
}

/// Offset one already placed map uv by a part of a texel.
///
/// Tiled maps wrap, so the difference stays continuous across the tile border;
/// centred and stretched maps step off the window and are handled by the
/// transparent-window rule in [`texturize_luma`].
fn texturize_shift(uv: [f32; 2], placement: TexturizePlacement, delta: [f32; 2]) -> [f32; 2] {
    match placement {
        TexturizePlacement::Tile => [
            (uv[0] + delta[0]).rem_euclid(1.0),
            (uv[1] + delta[1]).rem_euclid(1.0),
        ],
        TexturizePlacement::Center | TexturizePlacement::Stretch => {
            [uv[0] + delta[0], uv[1] + delta[1]]
        }
    }
}

/// Rec. 709 luminance of one straight height sample.
///
/// A centred map only illuminates its own window: a sample outside `0..1` is
/// transparent, so it contributes no luminance and therefore no relief.
fn texturize_luma(
    texture: AuxiliaryFloatFrame<'_>,
    uv: [f32; 2],
    sampling: AuxiliarySampling,
    placement: TexturizePlacement,
) -> f32 {
    if placement == TexturizePlacement::Center
        && (uv[0] < 0.0 || uv[0] > 1.0 || uv[1] < 0.0 || uv[1] > 1.0)
    {
        return 0.0;
    }
    let sample = sample_auxiliary_f32(texture, uv, sampling);
    0.2126 * sample[0] + 0.7152 * sample[1] + 0.0722 * sample[2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AuxiliaryPolicy, AuxiliaryResolution, EffectColorDomain, EffectFloatFrameView,
        EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
    };
    use std::collections::hash_map::DefaultHasher;

    fn format() -> EffectValueFormat {
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        )
    }

    fn reference(pixels: &[[f32; 4]], width: u32, height: u32) -> AuxiliaryFloatFrame<'_> {
        assert_eq!(pixels.len(), (width as usize) * (height as usize));
        AuxiliaryFloatFrame::new(
            EffectFloatFrameView::new(
                EffectFrameExtent::new(width, height),
                format(),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                pixels,
            ),
            AuxiliaryPolicy::new(AuxiliaryResolution::Authored),
        )
    }

    fn sampling() -> AuxiliarySampling {
        AuxiliarySampling::PRIMARY_GRID_LINEAR_CLAMP
    }

    /// A grey height map whose texels carry the given luminance.
    fn grey_ramp(values: &[f32]) -> Vec<[f32; 4]> {
        values.iter().map(|value| [*value, *value, *value, 1.0]).collect()
    }

    fn grade(
        light_direction: f32,
        texture_contrast: f32,
        texture_placement: TexturizePlacement,
    ) -> TexturizeGrade {
        TexturizeGrade::new(1, light_direction, texture_contrast, texture_placement).unwrap()
    }

    fn run(
        primary: &mut [[f32; 4]],
        width: u32,
        height: u32,
        map: &[[f32; 4]],
        map_width: u32,
        map_height: u32,
        grade: TexturizeGrade,
    ) -> bool {
        apply_texturize_f32(
            primary,
            width,
            height,
            reference(map, map_width, map_height),
            sampling(),
            &grade,
        )
    }

    fn assert_close(actual: f32, expected: f32, context: &str) {
        assert!(
            (actual - expected).abs() <= 1.0e-5,
            "{context}: expected {expected}, got {actual}"
        );
    }

    /// The expected shade of a lit pixel: one scalar times the source colour.
    fn assert_shaded(pixel: [f32; 4], shade: f32, context: &str) {
        assert_close(pixel[0], shade, &format!("{context} red"));
        assert_close(pixel[1], shade, &format!("{context} green"));
        assert_close(pixel[2], shade, &format!("{context} blue"));
        assert_eq!(pixel[3], 1.0, "{context} keeps its coverage");
    }

    #[test]
    fn documented_defaults_are_the_authored_defaults() {
        let defaults = TexturizeGrade::documented_defaults();
        assert_eq!(defaults.texture_layer(), 0);
        assert_eq!(defaults.light_direction(), 135.0);
        assert_eq!(defaults.texture_contrast(), 1.0);
        assert_eq!(defaults.texture_placement(), TexturizePlacement::Tile);
        assert!(defaults.is_identity(), "no layer means no texture at all");
        assert_eq!(TEXTURIZE_MIN_LAYER, 0);
        assert_eq!(TEXTURIZE_MAX_LAYER, 19);
        assert_eq!(TEXTURIZE_DEFAULT_LAYER, 0);
        assert_eq!(TEXTURIZE_DEFAULT_LIGHT_DIRECTION, 135.0);
        assert_eq!(TEXTURIZE_MIN_CONTRAST, 0.0);
        assert_eq!(TEXTURIZE_MAX_CONTRAST, 2.0);
        assert_eq!(TEXTURIZE_DEFAULT_CONTRAST, 1.0);
        assert!(grade(135.0, 0.0, TexturizePlacement::Tile).is_identity());
        assert!(!grade(135.0, 0.5, TexturizePlacement::Tile).is_identity());
    }

    #[test]
    fn placements_round_trip_through_their_codes_and_keys() {
        assert_eq!(TexturizePlacement::ALL.len(), 3);
        for placement in TexturizePlacement::ALL {
            assert_eq!(
                TexturizePlacement::from_code(placement.code()),
                Some(placement)
            );
            assert_eq!(
                TexturizePlacement::from_key(placement.as_key()),
                Some(placement)
            );
            assert!(!placement.label().is_empty());
        }
        assert_eq!(TexturizePlacement::Tile.code(), 1);
        assert_eq!(TexturizePlacement::Center.code(), 2);
        assert_eq!(TexturizePlacement::Stretch.code(), 3);
        assert_eq!(TexturizePlacement::Tile.as_key(), "tile");
        assert_eq!(TexturizePlacement::Center.as_key(), "center");
        assert_eq!(TexturizePlacement::Stretch.as_key(), "stretch");
        assert_eq!(TexturizePlacement::Tile.label(), "拼贴纹理");
        assert_eq!(TexturizePlacement::Center.label(), "居中纹理");
        assert_eq!(TexturizePlacement::Stretch.label(), "拉伸纹理以适合");
        assert_eq!(TexturizePlacement::from_code(0), None);
        assert_eq!(TexturizePlacement::from_code(4), None);
        assert_eq!(TexturizePlacement::from_key("fit"), None);
    }

    #[test]
    fn params_outside_their_documented_ranges_error() {
        for layer in [20_u32, 1000] {
            let error =
                TexturizeGrade::new(layer, 135.0, 1.0, TexturizePlacement::Tile).unwrap_err();
            assert_eq!(error, TexturizeError::TextureLayerOutOfRange);
            assert_eq!(error.parameter(), "texture_layer");
            assert!(error.to_string().contains("texture_layer"));
        }
        for direction in [-1.0, 360.5, f32::NAN, f32::INFINITY] {
            let error =
                TexturizeGrade::new(1, direction, 1.0, TexturizePlacement::Tile).unwrap_err();
            assert_eq!(error, TexturizeError::LightDirectionOutOfRange);
            assert_eq!(error.parameter(), "light_direction");
        }
        for contrast in [-0.5, 2.5, f32::NAN, f32::NEG_INFINITY] {
            let error =
                TexturizeGrade::new(1, 135.0, contrast, TexturizePlacement::Tile).unwrap_err();
            assert_eq!(error, TexturizeError::TextureContrastOutOfRange);
            assert_eq!(error.parameter(), "texture_contrast");
        }
        assert!(TexturizeGrade::new(19, 360.0, 2.0, TexturizePlacement::Stretch).is_ok());
        assert!(TexturizeGrade::new(0, 0.0, 0.0, TexturizePlacement::Center).is_ok());
    }

    #[test]
    fn gpu_controls_pack_the_documented_layout() {
        let grade = TexturizeGrade::new(7, 135.0, 1.5, TexturizePlacement::Center).unwrap();
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params[0], 1.5, "the contrast owns the first lane");
        assert_close(
            params[1],
            -std::f32::consts::FRAC_1_SQRT_2,
            "cos 135 degrees",
        );
        assert_close(
            params[2],
            std::f32::consts::FRAC_1_SQRT_2,
            "sin 135 degrees",
        );
        assert_eq!(params[3], 2.0, "the placement code sits in the third lane");
        assert_eq!(
            extra, [0.0; 4],
            "the renderer fills the two pixel-scale ratios"
        );
        let (defaults, _) = TexturizeGrade::documented_defaults().gpu_controls();
        assert_eq!(defaults[0], 1.0);
        assert_eq!(defaults[3], 1.0);
        let (zero, _) = TexturizeGrade::new(1, 0.0, 1.0, TexturizePlacement::Tile)
            .unwrap()
            .gpu_controls();
        assert_close(zero[1], 1.0, "cos 0 degrees");
        assert_close(zero[2], 0.0, "sin 0 degrees");
        let (quarter, _) = TexturizeGrade::new(1, 90.0, 1.0, TexturizePlacement::Tile)
            .unwrap()
            .gpu_controls();
        assert_close(quarter[1], 0.0, "cos 90 degrees");
        assert_close(quarter[2], 1.0, "sin 90 degrees");
    }

    /// One white row of a 4 texel wide map, lit at zero degrees.
    fn lit_row() -> Vec<[f32; 4]> {
        vec![[1.0, 1.0, 1.0, 1.0]; 4]
    }

    #[test]
    fn tile_placement_wraps_the_centre_difference_at_the_tile_border() {
        // A 4 texel grey ramp on a same-sized primary is sampled exactly at the
        // texel centres, so the only interpolation is the placement itself.
        let map = grey_ramp(&[0.0, 0.25, 0.5, 0.75]);
        let mut frame = lit_row();
        assert!(run(
            &mut frame,
            4,
            1,
            &map,
            4,
            1,
            grade(0.0, 1.0, TexturizePlacement::Tile)
        ));
        // The difference wraps: x = 0 reads t1 - t3 and x = 3 reads t0 - t2.
        for (column, shade) in [0.5_f32, 1.5, 1.5, 0.5].iter().enumerate() {
            assert_shaded(frame[column], *shade, &format!("tile column {column}"));
        }
    }

    #[test]
    fn stretch_placement_clamps_at_the_texture_border() {
        let map = grey_ramp(&[0.0, 0.25, 0.5, 0.75]);
        let mut frame = lit_row();
        assert!(run(
            &mut frame,
            4,
            1,
            &map,
            4,
            1,
            grade(0.0, 1.0, TexturizePlacement::Stretch)
        ));
        // Stretch keeps the same uv but clamps the border taps instead of
        // wrapping them, so the two edges only see one neighbour.
        for (column, shade) in [1.25_f32, 1.5, 1.5, 1.25].iter().enumerate() {
            assert_shaded(frame[column], *shade, &format!("stretch column {column}"));
        }
    }

    #[test]
    fn center_placement_treats_the_window_outside_as_transparent() {
        let map = grey_ramp(&[0.0, 0.25, 0.5, 0.75]);
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; 8];
        assert!(run(
            &mut frame,
            8,
            1,
            &map,
            4,
            1,
            grade(0.0, 1.0, TexturizePlacement::Center)
        ));
        // The four texel map sits centred in the eight pixel frame. The taps are
        // transparent outside the plate, so a pixel whose centre has left the map
        // still keeps the relief of the one tap that is still inside it: column 5
        // sees a single neighbour and column 6 keeps a quarter of the ramp step.
        for (column, shade) in [1.0_f32, 1.0, 1.25, 1.5, 1.5, 0.5, 0.25, 1.0].iter().enumerate() {
            assert_shaded(frame[column], *shade, &format!("center column {column}"));
        }
    }

    #[test]
    fn the_light_direction_selects_the_axis_that_shades() {
        // A four texel vertical ramp: the horizontal difference is always zero
        // because the map is one texel wide, so only `dv` can shade.
        let map = grey_ramp(&[0.0, 0.25, 0.5, 0.75]);
        let mut lit_along_v = vec![[1.0, 1.0, 1.0, 1.0]; 4];
        assert!(run(
            &mut lit_along_v,
            1,
            4,
            &map,
            1,
            4,
            grade(90.0, 1.0, TexturizePlacement::Tile)
        ));
        for (row, shade) in [0.5_f32, 1.5, 1.5, 0.5].iter().enumerate() {
            assert_shaded(
                lit_along_v[row],
                *shade,
                &format!("vertical light row {row}"),
            );
        }
        // At zero degrees the same ramp has no horizontal difference at all and
        // the frame is left exactly as it was.
        let mut unlit = vec![[1.0, 1.0, 1.0, 1.0]; 4];
        assert!(run(
            &mut unlit,
            1,
            4,
            &map,
            1,
            4,
            grade(0.0, 1.0, TexturizePlacement::Tile)
        ));
        assert_eq!(unlit, vec![[1.0, 1.0, 1.0, 1.0]; 4]);
    }

    #[test]
    fn luma_weights_follow_rec_709() {
        // Three texels carry the three primaries: the differences between them
        // are exactly the documented luma weights.
        let map = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
        ];
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; 3];
        assert!(run(
            &mut frame,
            3,
            1,
            &map,
            3,
            1,
            grade(0.0, 1.0, TexturizePlacement::Tile)
        ));
        let red = 0.2126_f32;
        let green = 0.7152_f32;
        let blue = 0.0722_f32;
        assert_shaded(frame[0], 1.0 + (green - blue), "red texel");
        assert_shaded(frame[1], 1.0 + (blue - red), "green texel");
        assert_shaded(frame[2], 1.0 + (red - green), "blue texel");
    }

    #[test]
    fn zero_contrast_is_the_identity() {
        let map = grey_ramp(&[0.0, 1.0, 0.0, 1.0]);
        let mut frame: Vec<[f32; 4]> = (0..4).map(|index| [index as f32, 0.5, 0.25, 1.0]).collect();
        let original = frame.clone();
        assert!(run(
            &mut frame,
            4,
            1,
            &map,
            4,
            1,
            grade(135.0, 0.0, TexturizePlacement::Tile)
        ));
        assert_eq!(frame, original, "shade 1 multiplies every channel by one");
    }

    #[test]
    fn alpha_is_preserved_and_zero_alpha_normalises_to_transparent_black() {
        let map = grey_ramp(&[0.0, 1.0]);
        let mut frame = vec![[0.9, 0.8, 0.7, 0.0], [0.9, 0.8, 0.7, 0.5]];
        assert!(run(
            &mut frame,
            2,
            1,
            &map,
            2,
            1,
            grade(0.0, 2.0, TexturizePlacement::Tile)
        ));
        assert_eq!(
            frame[0], [0.0; 4],
            "a transparent pixel keeps no colour, whatever the shading"
        );
        assert_eq!(frame[1][3], 0.5, "the coverage is never renormalized");
        // Two texels give a wrapping difference of zero, so the half covered
        // pixel is only scaled by one.
        assert_close(frame[1][0], 0.9, "covered red");
        assert_close(frame[1][1], 0.8, "covered green");
        assert_close(frame[1][2], 0.7, "covered blue");
    }

    #[test]
    fn shade_is_clamped_to_the_documented_range() {
        // A three texel tiled ramp yields a difference of two texels at every
        // pixel: contrast two doubles it past both ends of the shade range.
        let ramp = grey_ramp(&[0.0, 1.0, 2.0]);
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; 3];
        assert!(run(
            &mut frame,
            3,
            1,
            &ramp,
            3,
            1,
            grade(0.0, 2.0, TexturizePlacement::Tile)
        ));
        // x = 0 reads t1 - t2 = -1, x = 1 reads t2 - t0 = 2, x = 2 reads
        // t0 - t1 = -1, so both clamps are exercised.
        assert_shaded(frame[0], 0.0, "relief below minus one clamps to zero");
        assert_shaded(frame[1], 2.0, "relief above one clamps to two");
        assert_shaded(frame[2], 0.0, "relief below minus one clamps to zero");
    }

    #[test]
    fn checkpoint_every_stride_rows_reports_progress() {
        // A frame taller than one stride spends more than one checkpoint.
        let height = CHECKPOINT_STRIDE as u32 + 2;
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; height as usize];
        let map = grey_ramp(&[0.0, 1.0]);
        let texture = reference(&map, 2, 1);
        let mut calls = 0_u32;
        let result = apply_texturize_controlled(
            &mut frame,
            1,
            height,
            texture,
            sampling(),
            &grade(0.0, 1.0, TexturizePlacement::Tile),
            &mut || {
                calls += 1;
                Ok::<(), std::convert::Infallible>(())
            },
        );
        assert_eq!(result, Ok(true));
        assert!(
            calls >= 3,
            "one checkpoint per stride, plus the final one: {calls}"
        );
    }

    #[test]
    fn a_failing_checkpoint_stops_the_kernel() {
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; 4];
        let map = grey_ramp(&[0.0, 1.0]);
        let mut calls = 0_u32;
        let result = apply_texturize_controlled(
            &mut frame,
            4,
            1,
            reference(&map, 2, 1),
            sampling(),
            &grade(0.0, 1.0, TexturizePlacement::Tile),
            &mut || {
                calls += 1;
                if calls >= 2 {
                    Err("stop")
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result, Err("stop"));
    }

    #[test]
    fn inconsistent_extents_are_rejected() {
        let map = grey_ramp(&[0.0, 1.0]);
        // The primary length disagrees with its declared extent.
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; 3];
        assert!(!run(
            &mut frame,
            4,
            1,
            &map,
            2,
            1,
            grade(0.0, 1.0, TexturizePlacement::Tile)
        ));
        // A zero-sized map cannot carry a height.
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; 4];
        let empty: [[f32; 4]; 0] = [];
        assert!(!run(
            &mut frame,
            4,
            1,
            &empty,
            0,
            0,
            grade(0.0, 1.0, TexturizePlacement::Tile)
        ));
    }

    #[test]
    fn hash_state_is_sensitive_to_every_authored_control() {
        let base = TexturizeGrade::new(1, 135.0, 1.0, TexturizePlacement::Tile).unwrap();
        let hash = |grade: TexturizeGrade| {
            let mut hasher = DefaultHasher::new();
            grade.hash_state(&mut hasher);
            hasher.finish()
        };
        let base_hash = hash(base);
        for changed in [
            TexturizeGrade::new(2, 135.0, 1.0, TexturizePlacement::Tile).unwrap(),
            TexturizeGrade::new(1, 136.0, 1.0, TexturizePlacement::Tile).unwrap(),
            TexturizeGrade::new(1, 135.0, 1.5, TexturizePlacement::Tile).unwrap(),
            TexturizeGrade::new(1, 135.0, 1.0, TexturizePlacement::Center).unwrap(),
            TexturizeGrade::new(1, 135.0, 1.0, TexturizePlacement::Stretch).unwrap(),
        ] {
            assert_ne!(
                hash(changed),
                base_hash,
                "{changed:?} should hash differently"
            );
        }
        assert_eq!(hash(base), base_hash, "the hash is deterministic");
    }

    #[test]
    fn the_packed_placement_code_matches_the_menu_order() {
        for placement in TexturizePlacement::ALL {
            let grade = TexturizeGrade::new(1, 135.0, 1.0, placement).unwrap();
            let (params, _) = grade.gpu_controls();
            assert_eq!(params[3], placement.code() as f32);
        }
    }
}
