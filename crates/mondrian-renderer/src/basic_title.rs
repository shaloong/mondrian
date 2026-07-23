//! Deterministic Basic Title font resolution, shaping, and CPU rasterization.
//!
//! This Module owns one lazy system-font session and bounded raster cache.
//! Preview and Export use the same Interface; scheduling and publication remain
//! the responsibility of their respective Adapters.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;

use cosmic_text::{
    Align, Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Stretch, Style, SwashCache, Weight,
    Wrap,
};
use fontdb::{Query, ID as FontFaceId};
use lru::LruCache;
use mondrian_core::{
    BasicTitleFontStyle, BasicTitleHorizontalAlign, BasicTitleVerticalAlign, EvaluatedBasicTitle,
    Resolution, WorkingColorSpace, WorkingRgbaF32Frame,
};
use sha2::{Digest, Sha256};

use crate::CpuColorFrame;

const DEFAULT_CACHE_ENTRIES: usize = 64;
const DEFAULT_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_RASTER_PIXELS: u64 = 16 * 1024 * 1024;

/// Font and raster facts bound to one generated title frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicTitleRasterDiagnostics {
    /// Exact family requested by author state.
    pub requested_family: String,
    /// Resolved PostScript face name.
    pub resolved_postscript_name: String,
    /// Resolved face weight.
    pub resolved_weight: u16,
    /// Stable hash of the font bytes and face index used by this Session.
    pub face_fingerprint: [u8; 32],
    /// Shaped glyph count.
    pub glyphs: u32,
    /// Whether the bounded raster cache supplied this frame.
    pub cache_hit: bool,
}

/// A tightly cropped working-linear title frame and its source geometry.
#[derive(Debug, Clone)]
pub struct BasicTitleRasterFrame {
    /// Straight-alpha working-linear pixels.
    pub frame: CpuColorFrame,
    /// Affine mapping from sampled raster pixels to full-resolution title
    /// author coordinates.
    pub sampled_source_to_author: [f32; 6],
    /// Session-stable semantic and font identity.
    pub signature: u64,
    /// Font/raster execution evidence.
    pub diagnostics: BasicTitleRasterDiagnostics,
}

impl BasicTitleRasterFrame {
    /// Host pixel bytes retained by this generated frame.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of_val(self.frame.rgba_f32().data.as_slice())
    }
}

/// Fail-closed Basic Title generation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BasicTitleRasterError {
    /// Author or sampled canvas has no usable pixels.
    #[error(
        "Basic Title canvas is invalid: author {author_width}x{author_height}, sampled {sampled_width}x{sampled_height}"
    )]
    InvalidCanvas {
        author_width: u32,
        author_height: u32,
        sampled_width: u32,
        sampled_height: u32,
    },
    /// The Sequence title-safe total margin leaves no finite layout area.
    #[error("Basic Title safe-area total margin `{value}` must be finite and in [0, 1)")]
    InvalidTitleSafeMargin { value: String },
    /// The exact named font dependency is unavailable.
    #[error("Basic Title font family `{family}` is unavailable")]
    MissingFontFamily { family: String },
    /// Shaping needed an undeclared fallback face.
    #[error(
        "Basic Title font `{requested_family}` cannot render all text without undeclared fallback `{fallback_postscript_name}`"
    )]
    UndeclaredFontFallback {
        requested_family: String,
        fallback_postscript_name: String,
    },
    /// The font database could not expose bytes for the chosen face.
    #[error("Basic Title font face `{postscript_name}` could not be fingerprinted")]
    FontFaceUnavailable { postscript_name: String },
    /// A font file changed after this generation Session bound its face.
    #[error(
        "Basic Title font face `{postscript_name}` changed during one generation Session (expected {expected:?}, observed {actual:?})"
    )]
    FontDependencyChanged {
        postscript_name: String,
        expected: [u8; 32],
        actual: [u8; 32],
    },
    /// Shaped pixels exceed the bounded title-frame admission.
    #[error(
        "Basic Title raster {width}x{height} exceeds the {max_pixels}-pixel generation budget"
    )]
    RasterBudgetExceeded {
        width: u32,
        height: u32,
        max_pixels: u64,
    },
    /// Pixel allocation arithmetic overflowed.
    #[error("Basic Title raster dimensions overflowed addressable memory")]
    RasterSizeOverflow,
}

/// Session-owned Basic Title generator with a bounded pixel cache.
pub struct BasicTitleRasterizer {
    font_system: Option<FontSystem>,
    swash_cache: SwashCache,
    cache: LruCache<u64, BasicTitleRasterFrame>,
    cache_bytes: usize,
    max_cache_bytes: usize,
    face_bindings: HashMap<BasicTitleFontQueryIdentity, [u8; 32]>,
}

impl std::fmt::Debug for BasicTitleRasterizer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BasicTitleRasterizer")
            .field("font_system_initialized", &self.font_system.is_some())
            .field("cache_entries", &self.cache.len())
            .field("cache_bytes", &self.cache_bytes)
            .field("max_cache_bytes", &self.max_cache_bytes)
            .field("face_bindings", &self.face_bindings.len())
            .finish()
    }
}

impl Default for BasicTitleRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl BasicTitleRasterizer {
    /// Create a lazy system-font Session with production cache budgets.
    pub fn new() -> Self {
        Self::with_cache_budget(DEFAULT_CACHE_ENTRIES, DEFAULT_CACHE_BYTES)
    }

    /// Create a rasterizer with explicit bounded cache budgets.
    pub fn with_cache_budget(max_entries: usize, max_bytes: usize) -> Self {
        let entries = NonZeroUsize::new(max_entries.max(1))
            .expect("a positive Basic Title cache capacity is always constructed");
        Self {
            font_system: None,
            swash_cache: SwashCache::new(),
            cache: LruCache::new(entries),
            cache_bytes: 0,
            max_cache_bytes: max_bytes,
            face_bindings: HashMap::new(),
        }
    }

    /// Resolve, shape, and rasterize one evaluated title into working space.
    pub fn rasterize(
        &mut self,
        title: &EvaluatedBasicTitle,
        author_resolution: Resolution,
        title_safe_margin: f32,
        sampled_resolution: Resolution,
        working_color_space: WorkingColorSpace,
    ) -> Result<BasicTitleRasterFrame, BasicTitleRasterError> {
        validate_canvas(author_resolution, sampled_resolution)?;
        validate_title_safe_margin(title_safe_margin)?;
        let font_system = self.font_system.get_or_insert_with(FontSystem::new);
        let requested_style = cosmic_style(title.font_style);
        let requested_weight = Weight(title.font_weight);
        let requested_family = [Family::Name(title.font_family.as_str())];
        let primary_id = font_system
            .db()
            .query(&Query {
                families: &requested_family,
                weight: requested_weight,
                stretch: Stretch::Normal,
                style: requested_style,
            })
            .ok_or_else(|| BasicTitleRasterError::MissingFontFamily {
                family: title.font_family.clone(),
            })?;
        let primary_face = font_system.db().face(primary_id).ok_or_else(|| {
            BasicTitleRasterError::MissingFontFamily { family: title.font_family.clone() }
        })?;
        let resolved_postscript_name = primary_face.post_script_name.clone();
        let resolved_weight = primary_face.weight.0;
        let face_fingerprint = fingerprint_face(font_system, primary_id).ok_or_else(|| {
            BasicTitleRasterError::FontFaceUnavailable {
                postscript_name: resolved_postscript_name.clone(),
            }
        })?;
        let face_query = BasicTitleFontQueryIdentity {
            family: title.font_family.clone(),
            weight: title.font_weight,
            style: title.font_style,
        };
        bind_face_fingerprint(
            &mut self.face_bindings,
            face_query,
            &resolved_postscript_name,
            face_fingerprint,
        )?;
        let signature = title_signature(
            title,
            author_resolution,
            title_safe_margin,
            sampled_resolution,
            working_color_space,
            face_fingerprint,
        );
        if let Some(cached) = self.cache.get(&signature) {
            let mut cached = cached.clone();
            cached.diagnostics.cache_hit = true;
            return Ok(cached);
        }

        let uniform_scale = (sampled_resolution.width as f32 / author_resolution.width as f32)
            .min(sampled_resolution.height as f32 / author_resolution.height as f32);
        let sampled_font_size = title.font_size * uniform_scale;
        let sampled_line_height = sampled_font_size * title.line_height;
        let safe_margin_per_side = title_safe_margin * 0.5;
        let safe_width = author_resolution.width as f32 * (1.0 - title_safe_margin) * uniform_scale;
        let safe_height =
            author_resolution.height as f32 * (1.0 - title_safe_margin) * uniform_scale;
        let safe_left = author_resolution.width as f32 * safe_margin_per_side * uniform_scale;
        let safe_top = author_resolution.height as f32 * safe_margin_per_side * uniform_scale;

        let mut buffer = Buffer::new(
            font_system,
            Metrics::new(sampled_font_size, sampled_line_height),
        );
        buffer.set_size(Some(safe_width.max(1.0)), None);
        buffer.set_wrap(Wrap::None);
        let attrs = Attrs::new()
            .family(Family::Name(title.font_family.as_str()))
            .weight(requested_weight)
            .style(requested_style)
            .letter_spacing(title.tracking_em);
        buffer.set_text(
            title.text.as_str(),
            &attrs,
            Shaping::Advanced,
            Some(cosmic_align(title.horizontal_align)),
        );
        buffer.shape_until_scroll(font_system, true);

        let mut glyphs = 0u32;
        let mut block_height = 0.0f32;
        for run in buffer.layout_runs() {
            block_height = block_height.max(run.line_top + run.line_height);
            for glyph in run.glyphs {
                glyphs = glyphs.saturating_add(1);
                if glyph.font_id != primary_id {
                    let fallback = font_system
                        .db()
                        .face(glyph.font_id)
                        .map(|face| face.post_script_name.clone())
                        .unwrap_or_else(|| format!("{:?}", glyph.font_id));
                    return Err(BasicTitleRasterError::UndeclaredFontFallback {
                        requested_family: title.font_family.clone(),
                        fallback_postscript_name: fallback,
                    });
                }
            }
        }
        let vertical_offset = match title.vertical_align {
            BasicTitleVerticalAlign::Top => safe_top,
            BasicTitleVerticalAlign::Center => {
                safe_top + ((safe_height - block_height).max(0.0) * 0.5)
            }
            BasicTitleVerticalAlign::Bottom => safe_top + (safe_height - block_height).max(0.0),
        };
        let offset_x = safe_left.round() as i32;
        let offset_y = vertical_offset.round() as i32;

        let mut bounds = PixelBounds::default();
        buffer.draw(
            font_system,
            &mut self.swash_cache,
            cosmic_text::Color::rgb(255, 255, 255),
            |x, y, width, height, color| {
                if color.a() > 0 {
                    bounds.include(
                        x.saturating_add(offset_x),
                        y.saturating_add(offset_y),
                        width,
                        height,
                    );
                }
            },
        );

        let (crop_x, crop_y, width, height) = bounds.extent()?.unwrap_or((0, 0, 1, 1));
        admit_raster(width, height)?;
        let pixel_count = (width as usize)
            .checked_mul(height as usize)
            .ok_or(BasicTitleRasterError::RasterSizeOverflow)?;
        let mut pixels = vec![[0.0f32; 4]; pixel_count];
        buffer.draw(
            font_system,
            &mut self.swash_cache,
            cosmic_text::Color::rgb(255, 255, 255),
            |x, y, draw_width, draw_height, color| {
                if color.a() == 0 {
                    return;
                }
                paint_coverage(
                    &mut pixels,
                    width,
                    height,
                    x.saturating_add(offset_x).saturating_sub(crop_x),
                    y.saturating_add(offset_y).saturating_sub(crop_y),
                    draw_width,
                    draw_height,
                    color.a() as f32 / 255.0,
                    title.fill,
                );
            },
        );
        let completed_fingerprint = fingerprint_face(font_system, primary_id).ok_or_else(|| {
            BasicTitleRasterError::FontFaceUnavailable {
                postscript_name: resolved_postscript_name.clone(),
            }
        })?;
        if completed_fingerprint != face_fingerprint {
            return Err(BasicTitleRasterError::FontDependencyChanged {
                postscript_name: resolved_postscript_name.clone(),
                expected: face_fingerprint,
                actual: completed_fingerprint,
            });
        }

        let frame = BasicTitleRasterFrame {
            frame: CpuColorFrame::working(WorkingRgbaF32Frame {
                width,
                height,
                data: pixels,
                color_space: working_color_space,
            }),
            sampled_source_to_author: [
                1.0 / uniform_scale,
                0.0,
                crop_x as f32 / uniform_scale,
                0.0,
                1.0 / uniform_scale,
                crop_y as f32 / uniform_scale,
            ],
            signature,
            diagnostics: BasicTitleRasterDiagnostics {
                requested_family: title.font_family.clone(),
                resolved_postscript_name,
                resolved_weight,
                face_fingerprint,
                glyphs,
                cache_hit: false,
            },
        };
        self.insert_cache(signature, frame.clone());
        Ok(frame)
    }

    fn insert_cache(&mut self, key: u64, frame: BasicTitleRasterFrame) {
        let frame_bytes = frame.retained_bytes();
        if frame_bytes > self.max_cache_bytes {
            return;
        }
        if let Some(replaced) = self.cache.put(key, frame) {
            self.cache_bytes = self.cache_bytes.saturating_sub(replaced.retained_bytes());
        }
        self.cache_bytes = self.cache_bytes.saturating_add(frame_bytes);
        while self.cache_bytes > self.max_cache_bytes {
            let Some((_, retired)) = self.cache.pop_lru() else {
                self.cache_bytes = 0;
                break;
            };
            self.cache_bytes = self.cache_bytes.saturating_sub(retired.retained_bytes());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BasicTitleFontQueryIdentity {
    family: String,
    weight: u16,
    style: BasicTitleFontStyle,
}

fn bind_face_fingerprint(
    bindings: &mut HashMap<BasicTitleFontQueryIdentity, [u8; 32]>,
    query: BasicTitleFontQueryIdentity,
    postscript_name: &str,
    actual: [u8; 32],
) -> Result<(), BasicTitleRasterError> {
    if let Some(expected) = bindings.get(&query) {
        if *expected != actual {
            return Err(BasicTitleRasterError::FontDependencyChanged {
                postscript_name: postscript_name.to_owned(),
                expected: *expected,
                actual,
            });
        }
    } else {
        bindings.insert(query, actual);
    }
    Ok(())
}

/// Return the font-independent identity of one Basic Title raster request.
///
/// Preview uses this key only to coalesce identical background work. The
/// completed frame signature additionally contains the resolved font bytes and
/// face index, so downstream output identity names the exact face used. This
/// request key is Session-scoped; a font-catalog refresh must rotate the owning
/// raster Session instead of reusing it.
pub fn basic_title_raster_request_key(
    title: &EvaluatedBasicTitle,
    author_resolution: Resolution,
    title_safe_margin: f32,
    sampled_resolution: Resolution,
    working_color_space: WorkingColorSpace,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_title_raster_request(
        &mut hasher,
        title,
        author_resolution,
        title_safe_margin,
        sampled_resolution,
        working_color_space,
    );
    hasher.finish()
}

/// Project a sampled title raster through its Clip transform into one sampled
/// output canvas.
pub fn project_basic_title_transform(
    clip_transform: [f32; 6],
    sampled_source_to_author: [f32; 6],
    output_author_resolution: Resolution,
    output_sampled_resolution: Resolution,
) -> Option<[f32; 6]> {
    if output_author_resolution.width == 0
        || output_author_resolution.height == 0
        || output_sampled_resolution.width == 0
        || output_sampled_resolution.height == 0
        || clip_transform
            .iter()
            .chain(sampled_source_to_author.iter())
            .any(|v| !v.is_finite())
    {
        return None;
    }
    let source_to_transformed_author = multiply_affine(clip_transform, sampled_source_to_author);
    let author_to_output = [
        output_sampled_resolution.width as f32 / output_author_resolution.width as f32,
        0.0,
        0.0,
        0.0,
        output_sampled_resolution.height as f32 / output_author_resolution.height as f32,
        0.0,
    ];
    let projected = multiply_affine(author_to_output, source_to_transformed_author);
    projected.iter().all(|value| value.is_finite()).then_some(projected)
}

fn multiply_affine(left: [f32; 6], right: [f32; 6]) -> [f32; 6] {
    [
        left[0] * right[0] + left[1] * right[3],
        left[0] * right[1] + left[1] * right[4],
        left[0] * right[2] + left[1] * right[5] + left[2],
        left[3] * right[0] + left[4] * right[3],
        left[3] * right[1] + left[4] * right[4],
        left[3] * right[2] + left[4] * right[5] + left[5],
    ]
}

fn validate_canvas(author: Resolution, sampled: Resolution) -> Result<(), BasicTitleRasterError> {
    if author.width == 0 || author.height == 0 || sampled.width == 0 || sampled.height == 0 {
        Err(BasicTitleRasterError::InvalidCanvas {
            author_width: author.width,
            author_height: author.height,
            sampled_width: sampled.width,
            sampled_height: sampled.height,
        })
    } else {
        Ok(())
    }
}

fn validate_title_safe_margin(value: f32) -> Result<(), BasicTitleRasterError> {
    if value.is_finite() && (0.0..1.0).contains(&value) {
        Ok(())
    } else {
        Err(BasicTitleRasterError::InvalidTitleSafeMargin { value: value.to_string() })
    }
}

fn admit_raster(width: u32, height: u32) -> Result<(), BasicTitleRasterError> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(BasicTitleRasterError::RasterSizeOverflow)?;
    if pixels > MAX_RASTER_PIXELS {
        Err(BasicTitleRasterError::RasterBudgetExceeded {
            width,
            height,
            max_pixels: MAX_RASTER_PIXELS,
        })
    } else {
        Ok(())
    }
}

fn cosmic_style(style: BasicTitleFontStyle) -> Style {
    match style {
        BasicTitleFontStyle::Normal => Style::Normal,
        BasicTitleFontStyle::Italic => Style::Italic,
        BasicTitleFontStyle::Oblique => Style::Oblique,
    }
}

fn cosmic_align(align: BasicTitleHorizontalAlign) -> Align {
    match align {
        BasicTitleHorizontalAlign::Left => Align::Left,
        BasicTitleHorizontalAlign::Center => Align::Center,
        BasicTitleHorizontalAlign::Right => Align::Right,
    }
}

fn fingerprint_face(font_system: &FontSystem, id: FontFaceId) -> Option<[u8; 32]> {
    font_system.db().with_face_data(id, |bytes, face_index| {
        let mut hasher = Sha256::new();
        hasher.update(face_index.to_le_bytes());
        hasher.update(bytes);
        hasher.finalize().into()
    })
}

fn title_signature(
    title: &EvaluatedBasicTitle,
    author: Resolution,
    title_safe_margin: f32,
    sampled: Resolution,
    working: WorkingColorSpace,
    face_fingerprint: [u8; 32],
) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_title_raster_request(
        &mut hasher,
        title,
        author,
        title_safe_margin,
        sampled,
        working,
    );
    face_fingerprint.hash(&mut hasher);
    hasher.finish()
}

fn hash_title_raster_request(
    hasher: &mut impl Hasher,
    title: &EvaluatedBasicTitle,
    author: Resolution,
    title_safe_margin: f32,
    sampled: Resolution,
    working: WorkingColorSpace,
) {
    // Revision tag prevents an implementation contract change from reusing
    // an earlier session key.
    1u8.hash(hasher);
    title.text.hash(hasher);
    title.font_family.hash(hasher);
    title.font_weight.hash(hasher);
    title.font_style.hash(hasher);
    title.font_size.to_bits().hash(hasher);
    title.fill.r.to_bits().hash(hasher);
    title.fill.g.to_bits().hash(hasher);
    title.fill.b.to_bits().hash(hasher);
    title.fill.a.to_bits().hash(hasher);
    title.tracking_em.to_bits().hash(hasher);
    title.line_height.to_bits().hash(hasher);
    title.horizontal_align.hash(hasher);
    title.vertical_align.hash(hasher);
    author.width.hash(hasher);
    author.height.hash(hasher);
    title_safe_margin.to_bits().hash(hasher);
    sampled.width.hash(hasher);
    sampled.height.hash(hasher);
    working.hash(hasher);
}

#[derive(Debug, Default)]
struct PixelBounds {
    min_x: i64,
    min_y: i64,
    max_x: i64,
    max_y: i64,
    present: bool,
}

impl PixelBounds {
    fn include(&mut self, x: i32, y: i32, width: u32, height: u32) {
        let x = i64::from(x);
        let y = i64::from(y);
        let max_x = x.saturating_add(i64::from(width));
        let max_y = y.saturating_add(i64::from(height));
        if !self.present {
            self.min_x = x;
            self.min_y = y;
            self.max_x = max_x;
            self.max_y = max_y;
            self.present = true;
        } else {
            self.min_x = self.min_x.min(x);
            self.min_y = self.min_y.min(y);
            self.max_x = self.max_x.max(max_x);
            self.max_y = self.max_y.max(max_y);
        }
    }

    fn extent(&self) -> Result<Option<(i32, i32, u32, u32)>, BasicTitleRasterError> {
        if !self.present {
            return Ok(None);
        }
        let min_x =
            i32::try_from(self.min_x).map_err(|_| BasicTitleRasterError::RasterSizeOverflow)?;
        let min_y =
            i32::try_from(self.min_y).map_err(|_| BasicTitleRasterError::RasterSizeOverflow)?;
        let width = self
            .max_x
            .checked_sub(self.min_x)
            .and_then(|width| u32::try_from(width).ok())
            .ok_or(BasicTitleRasterError::RasterSizeOverflow)?;
        let height = self
            .max_y
            .checked_sub(self.min_y)
            .and_then(|height| u32::try_from(height).ok())
            .ok_or(BasicTitleRasterError::RasterSizeOverflow)?;
        Ok(Some((min_x, min_y, width.max(1), height.max(1))))
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_coverage(
    pixels: &mut [[f32; 4]],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    draw_width: u32,
    draw_height: u32,
    coverage: f32,
    fill: mondrian_core::Color,
) {
    let source_alpha = (coverage * fill.a).clamp(0.0, 1.0);
    if source_alpha <= 0.0 {
        return;
    }
    for row in 0..draw_height {
        let target_y = i64::from(y) + i64::from(row);
        if !(0..i64::from(height)).contains(&target_y) {
            continue;
        }
        for column in 0..draw_width {
            let target_x = i64::from(x) + i64::from(column);
            if !(0..i64::from(width)).contains(&target_x) {
                continue;
            }
            let index = target_y as usize * width as usize + target_x as usize;
            if let Some(pixel) = pixels.get_mut(index) {
                pixel[0] = fill.r;
                pixel[1] = fill.g;
                pixel[2] = fill.b;
                pixel[3] = source_alpha + pixel[3] * (1.0 - source_alpha);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{default_basic_title_font_family, BasicTitle, TimelineTime};

    fn evaluated(text: &str) -> EvaluatedBasicTitle {
        BasicTitle::new(text, default_basic_title_font_family())
            .expect("title")
            .evaluate(TimelineTime::ZERO)
            .expect("evaluate")
    }

    #[test]
    fn title_raster_is_tightly_cropped_straight_alpha_working_color() {
        let mut rasterizer = BasicTitleRasterizer::new();
        let output = rasterizer
            .rasterize(
                &evaluated("Mondrian"),
                Resolution::FHD,
                0.20,
                Resolution::FHD,
                WorkingColorSpace::LinearRec709,
            )
            .expect("raster");

        assert!(output.frame.descriptor().width < Resolution::FHD.width);
        assert!(output.frame.descriptor().height < Resolution::FHD.height);
        assert_eq!(
            output.frame.descriptor().alpha,
            crate::ColorFrameAlpha::StraightCoverage
        );
        assert!(output.frame.rgba_f32().data.iter().any(|pixel| pixel[3] > 0.0));
        assert!(!output.diagnostics.cache_hit);
    }

    #[test]
    fn same_title_reuses_bounded_session_raster() {
        let mut rasterizer = BasicTitleRasterizer::new();
        let title = evaluated("Cached");
        let first = rasterizer
            .rasterize(
                &title,
                Resolution::FHD,
                0.20,
                Resolution::HD,
                WorkingColorSpace::LinearRec709,
            )
            .expect("first");
        let second = rasterizer
            .rasterize(
                &title,
                Resolution::FHD,
                0.20,
                Resolution::HD,
                WorkingColorSpace::LinearRec709,
            )
            .expect("second");

        assert_eq!(first.signature, second.signature);
        assert!(!first.diagnostics.cache_hit);
        assert!(second.diagnostics.cache_hit);
    }

    #[test]
    fn named_missing_font_fails_closed() {
        let mut title = evaluated("Missing");
        title.font_family = "Mondrian Font That Does Not Exist 9F0E".to_owned();
        let error = BasicTitleRasterizer::new()
            .rasterize(
                &title,
                Resolution::FHD,
                0.20,
                Resolution::FHD,
                WorkingColorSpace::LinearRec709,
            )
            .expect_err("missing font must fail");

        assert!(matches!(
            error,
            BasicTitleRasterError::MissingFontFamily { .. }
        ));
    }

    #[test]
    fn generation_session_binds_one_exact_face_per_font_query() {
        let mut bindings = HashMap::new();
        let query = BasicTitleFontQueryIdentity {
            family: "Mondrian Test Sans".to_owned(),
            weight: 400,
            style: BasicTitleFontStyle::Normal,
        };
        let original = [0x11; 32];

        bind_face_fingerprint(
            &mut bindings,
            query.clone(),
            "MondrianTest-Regular",
            original,
        )
        .expect("first observation binds the face");
        bind_face_fingerprint(
            &mut bindings,
            query.clone(),
            "MondrianTest-Regular",
            original,
        )
        .expect("the bound face remains valid");

        let error = bind_face_fingerprint(&mut bindings, query, "MondrianTest-Regular", [0x22; 32])
            .expect_err("a font dependency cannot drift inside one generation session");
        assert!(matches!(
            error,
            BasicTitleRasterError::FontDependencyChanged {
                expected,
                actual,
                ..
            } if expected == original && actual == [0x22; 32]
        ));
    }

    #[test]
    fn title_safe_margin_is_part_of_layout_identity_and_fails_closed_when_invalid() {
        let title = evaluated("Safe");
        let narrow = basic_title_raster_request_key(
            &title,
            Resolution::FHD,
            0.10,
            Resolution::HD,
            WorkingColorSpace::LinearRec709,
        );
        let wide = basic_title_raster_request_key(
            &title,
            Resolution::FHD,
            0.20,
            Resolution::HD,
            WorkingColorSpace::LinearRec709,
        );
        assert_ne!(narrow, wide);

        let error = BasicTitleRasterizer::new()
            .rasterize(
                &title,
                Resolution::FHD,
                f32::NAN,
                Resolution::HD,
                WorkingColorSpace::LinearRec709,
            )
            .expect_err("invalid safe area must fail");
        assert!(matches!(
            error,
            BasicTitleRasterError::InvalidTitleSafeMargin { .. }
        ));
    }

    #[test]
    fn sampled_title_transform_preserves_author_position() {
        let projected = project_basic_title_transform(
            [1.0, 0.0, 20.0, 0.0, 1.0, 10.0],
            [2.0, 0.0, 100.0, 0.0, 2.0, 50.0],
            Resolution::FHD,
            Resolution::HD,
        )
        .expect("projection");

        assert_eq!(projected, [4.0 / 3.0, 0.0, 80.0, 0.0, 4.0 / 3.0, 40.0]);
    }
}
