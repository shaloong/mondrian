//! Deterministic Basic Title font resolution, shaping, and CPU rasterization.
//!
//! This Module owns one lazy system-font session and bounded raster cache.
//! Preview and Export use the same Interface; scheduling and publication remain
//! the responsibility of their respective Adapters.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Arc;

use cosmic_text::{
    Align, Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Stretch, Style, SwashCache, Weight,
    Wrap,
};
use fontdb::{Query, Source as FontSource, ID as FontFaceId};
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

/// Immutable Basic Title font-selection query.
///
/// All three author properties are non-animatable. A selected-range execution
/// closure may therefore deduplicate this value without sampling frames.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BasicTitleFontQuery {
    /// Exact author-requested family.
    pub family: String,
    /// OpenType/CSS numeric weight.
    pub weight: u16,
    /// Requested font posture.
    pub style: BasicTitleFontStyle,
}

impl BasicTitleFontQuery {
    /// Derive the exact query from an evaluated Basic Title.
    pub fn from_title(title: &EvaluatedBasicTitle) -> Self {
        Self {
            family: title.font_family.clone(),
            weight: title.font_weight,
            style: title.font_style,
        }
    }
}

impl PartialOrd for BasicTitleFontQuery {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BasicTitleFontQuery {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (
            self.family.as_str(),
            self.weight,
            basic_title_font_style_order(self.style),
        )
            .cmp(&(
                other.family.as_str(),
                other.weight,
                basic_title_font_style_order(other.style),
            ))
    }
}

/// One exact font face frozen for offline Basic Title execution.
#[derive(Debug, Clone)]
pub struct PreparedBasicTitleFontFace {
    bytes: Arc<Vec<u8>>,
    face_index: u32,
    fingerprint: [u8; 32],
    postscript_name: String,
    resolved_weight: u16,
}

impl PreparedBasicTitleFontFace {
    /// Exact font-file bytes retained by this face.
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// Face index inside a font collection.
    pub const fn face_index(&self) -> u32 {
        self.face_index
    }

    /// SHA-256 of face index and exact font-file bytes.
    pub const fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    /// Resolved PostScript face name.
    pub fn postscript_name(&self) -> &str {
        self.postscript_name.as_str()
    }

    /// Resolved numeric weight.
    pub const fn resolved_weight(&self) -> u16 {
        self.resolved_weight
    }
}

/// Byte-frozen font closure for one selected offline visual execution.
///
/// Font bytes are deduplicated by exact face fingerprint. Worker rasterizers
/// load only these in-memory sources and never query the live system catalog.
#[derive(Debug, Clone, Default)]
pub struct PreparedBasicTitleFontSet {
    bindings: HashMap<BasicTitleFontQuery, [u8; 32]>,
    faces: HashMap<[u8; 32], PreparedBasicTitleFontFace>,
    sources: HashMap<[u8; 32], Arc<Vec<u8>>>,
    retained_bytes: usize,
}

impl PreparedBasicTitleFontSet {
    /// Resolve and byte-freeze every selected query under one aggregate grant.
    pub fn prepare(
        queries: impl IntoIterator<Item = BasicTitleFontQuery>,
        max_bytes: usize,
    ) -> Result<Self, BasicTitleRasterError> {
        let mut queries = queries.into_iter().collect::<Vec<_>>();
        queries.sort();
        queries.dedup();
        if queries.is_empty() {
            return Ok(Self::default());
        }

        let font_system = FontSystem::new();
        let mut prepared = Self::default();
        for query in queries {
            let resolved = resolve_font_face(&font_system, &query)?;
            let (bytes, face_index) = font_system
                .db()
                .with_face_data(resolved.id, |bytes, face_index| {
                    (bytes.to_vec(), face_index)
                })
                .ok_or_else(|| BasicTitleRasterError::FontFaceUnavailable {
                    postscript_name: resolved.postscript_name.clone(),
                })?;
            let fingerprint = fingerprint_font_bytes(bytes.as_slice(), face_index);
            if fingerprint != resolved.fingerprint {
                return Err(BasicTitleRasterError::FontDependencyChanged {
                    postscript_name: resolved.postscript_name,
                    expected: resolved.fingerprint,
                    actual: fingerprint,
                });
            }
            if !prepared.faces.contains_key(&fingerprint) {
                let source_fingerprint = fingerprint_font_source(bytes.as_slice());
                let source = if let Some(source) = prepared.sources.get(&source_fingerprint) {
                    Arc::clone(source)
                } else {
                    let retained_bytes = prepared.retained_bytes.checked_add(bytes.len()).ok_or(
                        BasicTitleRasterError::PreparedFontByteBudgetExceeded {
                            required: usize::MAX,
                            limit: max_bytes,
                        },
                    )?;
                    if retained_bytes > max_bytes {
                        return Err(BasicTitleRasterError::PreparedFontByteBudgetExceeded {
                            required: retained_bytes,
                            limit: max_bytes,
                        });
                    }
                    prepared.retained_bytes = retained_bytes;
                    let source = Arc::new(bytes);
                    prepared.sources.insert(source_fingerprint, Arc::clone(&source));
                    source
                };
                prepared.faces.insert(
                    fingerprint,
                    PreparedBasicTitleFontFace {
                        bytes: source,
                        face_index,
                        fingerprint,
                        postscript_name: resolved.postscript_name.clone(),
                        resolved_weight: resolved.resolved_weight,
                    },
                );
            }
            prepared.bindings.insert(query, fingerprint);
        }
        Ok(prepared)
    }

    /// Number of distinct author queries in the selected closure.
    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }

    /// Whether this frozen closure contains one exact author query.
    pub fn contains_query(&self, query: &BasicTitleFontQuery) -> bool {
        self.bindings.contains_key(query)
    }

    /// Frozen author queries in unspecified iteration order.
    pub fn queries(&self) -> impl ExactSizeIterator<Item = &BasicTitleFontQuery> {
        self.bindings.keys()
    }

    /// Number of distinct byte-frozen faces.
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// Number of distinct retained font files or in-memory source blobs.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Aggregate deduplicated font-source bytes.
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

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
    frame: CpuColorFrame,
    /// Affine mapping from sampled raster pixels to full-resolution title
    /// author coordinates.
    sampled_source_to_author: [f32; 6],
    /// Session-stable semantic and resolved-font identity.
    identity: BasicTitleRasterIdentity,
    /// Font/raster execution evidence.
    diagnostics: BasicTitleRasterDiagnostics,
}

/// Complete strong identity of one font-independent Basic Title raster request.
///
/// The full 32-byte value is cache authority. A compact projection must never
/// be used as equality authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BasicTitleRasterRequestIdentity([u8; 32]);

impl std::fmt::Display for BasicTitleRasterRequestIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_hex_identity(self.0, formatter)
    }
}

/// Complete strong identity of one generated Basic Title frame.
///
/// This extends the complete raster request identity with the exact resolved
/// font bytes and face index used by the raster Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BasicTitleRasterIdentity([u8; 32]);

impl std::fmt::Display for BasicTitleRasterIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_hex_identity(self.0, formatter)
    }
}

impl BasicTitleRasterFrame {
    /// Borrow the generated working-linear frame.
    pub fn frame(&self) -> &CpuColorFrame {
        &self.frame
    }

    /// Mapping from sampled raster pixels to title author coordinates.
    pub const fn sampled_source_to_author(&self) -> [f32; 6] {
        self.sampled_source_to_author
    }

    /// Complete semantic and resolved-font identity.
    pub const fn identity(&self) -> BasicTitleRasterIdentity {
        self.identity
    }

    /// Borrow font and raster execution evidence.
    pub const fn diagnostics(&self) -> &BasicTitleRasterDiagnostics {
        &self.diagnostics
    }

    /// Consume the generated source and return its working-linear frame.
    pub fn into_frame(self) -> CpuColorFrame {
        self.frame
    }

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
    /// Byte-frozen selected faces exceed the offline execution grant.
    #[error(
        "prepared Basic Title font sources require {required} bytes, exceeding the {limit}-byte grant"
    )]
    PreparedFontByteBudgetExceeded {
        /// Deduplicated font-source bytes required by the selected closure.
        required: usize,
        /// Maximum font-source bytes admitted for the offline Session.
        limit: usize,
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
    cache: LruCache<BasicTitleRasterIdentity, BasicTitleRasterFrame>,
    cache_bytes: usize,
    max_cache_bytes: usize,
    max_glyph_cache_entries: usize,
    face_bindings: HashMap<BasicTitleFontQuery, [u8; 32]>,
}

impl std::fmt::Debug for BasicTitleRasterizer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BasicTitleRasterizer")
            .field("font_system_initialized", &self.font_system.is_some())
            .field("cache_entries", &self.cache.len())
            .field("cache_bytes", &self.cache_bytes)
            .field("glyph_cache_bytes", &self.glyph_cache_bytes())
            .field("max_cache_bytes", &self.max_cache_bytes)
            .field("max_glyph_cache_entries", &self.max_glyph_cache_entries)
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
        let entries = NonZeroUsize::new(max_entries).unwrap_or(NonZeroUsize::MIN);
        Self {
            font_system: None,
            swash_cache: SwashCache::new(),
            cache: LruCache::new(entries),
            cache_bytes: 0,
            max_cache_bytes: max_bytes,
            max_glyph_cache_entries: if max_bytes == 0 {
                0
            } else {
                max_entries.saturating_mul(64).max(64)
            },
            face_bindings: HashMap::new(),
        }
    }

    /// Create an offline raster Session from byte-frozen font sources.
    ///
    /// The resulting `FontSystem` contains only the supplied in-memory faces;
    /// it never loads or queries the live system font catalog.
    pub fn with_prepared_font_set(
        max_entries: usize,
        max_bytes: usize,
        prepared: &PreparedBasicTitleFontSet,
    ) -> Result<Self, BasicTitleRasterError> {
        let mut database = fontdb::Database::new();
        for source in prepared.sources.values() {
            let bytes: Arc<dyn AsRef<[u8]> + Send + Sync> = source.clone();
            database.load_font_source(FontSource::Binary(bytes));
        }
        database.set_monospace_family("Noto Sans Mono");
        database.set_sans_serif_family("Open Sans");
        database.set_serif_family("DejaVu Serif");
        let font_system = FontSystem::new_with_locale_and_db("en-US".to_owned(), database);
        for (query, expected) in &prepared.bindings {
            let resolved = resolve_font_face(&font_system, query)?;
            if resolved.fingerprint != *expected {
                return Err(BasicTitleRasterError::FontDependencyChanged {
                    postscript_name: resolved.postscript_name,
                    expected: *expected,
                    actual: resolved.fingerprint,
                });
            }
            let expected_face = prepared.faces.get(expected).ok_or_else(|| {
                BasicTitleRasterError::FontFaceUnavailable {
                    postscript_name: resolved.postscript_name.clone(),
                }
            })?;
            if resolved.postscript_name != expected_face.postscript_name
                || resolved.resolved_weight != expected_face.resolved_weight
            {
                return Err(BasicTitleRasterError::FontDependencyChanged {
                    postscript_name: resolved.postscript_name,
                    expected: *expected,
                    actual: resolved.fingerprint,
                });
            }
        }
        let entries = NonZeroUsize::new(max_entries).unwrap_or(NonZeroUsize::MIN);
        Ok(Self {
            font_system: Some(font_system),
            swash_cache: SwashCache::new(),
            cache: LruCache::new(entries),
            cache_bytes: 0,
            max_cache_bytes: max_bytes,
            max_glyph_cache_entries: if max_bytes == 0 {
                0
            } else {
                max_entries.saturating_mul(64).max(64)
            },
            face_bindings: prepared.bindings.clone(),
        })
    }

    /// Aggregate retained frame/glyph cache bytes owned by this Session.
    pub fn retained_cache_bytes(&self) -> usize {
        self.cache_bytes.saturating_add(self.glyph_cache_bytes())
    }

    /// Aggregate retained frame/glyph cache byte limit.
    pub const fn cache_byte_budget(&self) -> usize {
        self.max_cache_bytes
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
        let face_query = BasicTitleFontQuery::from_title(title);
        let resolved_face = resolve_font_face(font_system, &face_query)?;
        let primary_id = resolved_face.id;
        let resolved_postscript_name = resolved_face.postscript_name;
        let resolved_weight = resolved_face.resolved_weight;
        let face_fingerprint = resolved_face.fingerprint;
        bind_face_fingerprint(
            &mut self.face_bindings,
            face_query,
            &resolved_postscript_name,
            face_fingerprint,
        )?;
        let request_identity = basic_title_raster_request_identity(
            title,
            author_resolution,
            title_safe_margin,
            sampled_resolution,
            working_color_space,
        );
        let identity = title_raster_identity(request_identity, face_fingerprint);
        if let Some(cached) = self.cache.get(&identity) {
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
        trim_swash_cache_to_aggregate_budget(
            &mut self.swash_cache,
            self.cache_bytes,
            self.max_cache_bytes,
            self.max_glyph_cache_entries,
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
        trim_swash_cache_to_aggregate_budget(
            &mut self.swash_cache,
            self.cache_bytes,
            self.max_cache_bytes,
            self.max_glyph_cache_entries,
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
            identity,
            diagnostics: BasicTitleRasterDiagnostics {
                requested_family: title.font_family.clone(),
                resolved_postscript_name,
                resolved_weight,
                face_fingerprint,
                glyphs,
                cache_hit: false,
            },
        };
        self.insert_cache(identity, frame.clone());
        Ok(frame)
    }

    fn insert_cache(&mut self, key: BasicTitleRasterIdentity, frame: BasicTitleRasterFrame) {
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
        self.trim_aggregate_cache();
    }

    fn glyph_cache_bytes(&self) -> usize {
        swash_cache_bytes(&self.swash_cache)
    }

    fn trim_aggregate_cache(&mut self) {
        trim_swash_cache_to_aggregate_budget(
            &mut self.swash_cache,
            self.cache_bytes,
            self.max_cache_bytes,
            self.max_glyph_cache_entries,
        );
    }
}

fn swash_cache_bytes(swash_cache: &SwashCache) -> usize {
    let images = swash_cache
        .image_cache
        .values()
        .filter_map(Option::as_ref)
        .fold(0_usize, |bytes, image| {
            bytes.saturating_add(image.data.len())
        });
    swash_cache
        .outline_command_cache
        .values()
        .filter_map(Option::as_ref)
        .fold(images, |bytes, commands| {
            bytes.saturating_add(std::mem::size_of_val(commands.as_ref()))
        })
}

fn trim_swash_cache_to_aggregate_budget(
    swash_cache: &mut SwashCache,
    frame_cache_bytes: usize,
    max_cache_bytes: usize,
    max_glyph_cache_entries: usize,
) {
    let glyph_entries = swash_cache
        .image_cache
        .len()
        .saturating_add(swash_cache.outline_command_cache.len());
    if frame_cache_bytes.saturating_add(swash_cache_bytes(swash_cache)) <= max_cache_bytes
        && glyph_entries <= max_glyph_cache_entries
    {
        return;
    }
    // Swash exposes no bounded eviction Interface. Clearing this transient
    // acceleration cache preserves face bindings and frame-cache identity
    // while keeping persistent title residency inside the caller budget.
    swash_cache.image_cache.clear();
    swash_cache.outline_command_cache.clear();
}

fn bind_face_fingerprint(
    bindings: &mut HashMap<BasicTitleFontQuery, [u8; 32]>,
    query: BasicTitleFontQuery,
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
/// completed frame identity additionally contains the resolved font bytes and
/// face index, so downstream output identity names the exact face used. This
/// request key is Session-scoped; a font-catalog refresh must rotate the owning
/// raster Session instead of reusing it.
pub fn basic_title_raster_request_identity(
    title: &EvaluatedBasicTitle,
    author_resolution: Resolution,
    title_safe_margin: f32,
    sampled_resolution: Resolution,
    working_color_space: WorkingColorSpace,
) -> BasicTitleRasterRequestIdentity {
    let mut hasher =
        BasicTitleIdentityHasher::new(b"mondrian.renderer.basic-title-raster-request.v1");
    hash_title_raster_request(
        &mut hasher,
        title,
        author_resolution,
        title_safe_margin,
        sampled_resolution,
        working_color_space,
    );
    BasicTitleRasterRequestIdentity(hasher.finish_identity())
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

const fn basic_title_font_style_order(style: BasicTitleFontStyle) -> u8 {
    match style {
        BasicTitleFontStyle::Normal => 0,
        BasicTitleFontStyle::Italic => 1,
        BasicTitleFontStyle::Oblique => 2,
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
        fingerprint_font_bytes(bytes, face_index)
    })
}

fn fingerprint_font_bytes(bytes: &[u8], face_index: u32) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(face_index.to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

fn fingerprint_font_source(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

struct ResolvedBasicTitleFontFace {
    id: FontFaceId,
    postscript_name: String,
    resolved_weight: u16,
    fingerprint: [u8; 32],
}

fn resolve_font_face(
    font_system: &FontSystem,
    query: &BasicTitleFontQuery,
) -> Result<ResolvedBasicTitleFontFace, BasicTitleRasterError> {
    let families = [Family::Name(query.family.as_str())];
    let id = font_system
        .db()
        .query(&Query {
            families: &families,
            weight: Weight(query.weight),
            stretch: Stretch::Normal,
            style: cosmic_style(query.style),
        })
        .ok_or_else(|| BasicTitleRasterError::MissingFontFamily { family: query.family.clone() })?;
    let face = font_system
        .db()
        .face(id)
        .ok_or_else(|| BasicTitleRasterError::MissingFontFamily { family: query.family.clone() })?;
    let postscript_name = face.post_script_name.clone();
    let fingerprint = fingerprint_face(font_system, id).ok_or_else(|| {
        BasicTitleRasterError::FontFaceUnavailable { postscript_name: postscript_name.clone() }
    })?;
    Ok(ResolvedBasicTitleFontFace {
        id,
        postscript_name,
        resolved_weight: face.weight.0,
        fingerprint,
    })
}

fn title_raster_identity(
    request_identity: BasicTitleRasterRequestIdentity,
    face_fingerprint: [u8; 32],
) -> BasicTitleRasterIdentity {
    let mut hasher =
        BasicTitleIdentityHasher::new(b"mondrian.renderer.basic-title-raster-frame.v1");
    request_identity.hash(&mut hasher);
    face_fingerprint.hash(&mut hasher);
    BasicTitleRasterIdentity(hasher.finish_identity())
}

struct BasicTitleIdentityHasher {
    hasher: Sha256,
}

impl BasicTitleIdentityHasher {
    fn new(domain: &'static [u8]) -> Self {
        let mut identity = Self { hasher: Sha256::new() };
        identity.write(domain);
        identity
    }

    fn finish_identity(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }
}

impl Hasher for BasicTitleIdentityHasher {
    fn finish(&self) -> u64 {
        let identity: [u8; 32] = self.hasher.clone().finalize().into();
        u64::from_le_bytes([
            identity[0],
            identity[1],
            identity[2],
            identity[3],
            identity[4],
            identity[5],
            identity[6],
            identity[7],
        ])
    }

    fn write(&mut self, bytes: &[u8]) {
        self.hasher.update((bytes.len() as u64).to_le_bytes());
        self.hasher.update(bytes);
    }
}

fn write_hex_identity(
    identity: [u8; 32],
    formatter: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    for byte in identity {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
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

        assert_eq!(first.identity, second.identity);
        assert!(!first.diagnostics.cache_hit);
        assert!(second.diagnostics.cache_hit);
    }

    #[test]
    fn prepared_font_set_deduplicates_bytes_and_rasterizes_without_system_catalog() {
        let title = evaluated("Frozen face");
        let query = BasicTitleFontQuery::from_title(&title);
        let prepared =
            PreparedBasicTitleFontSet::prepare([query.clone(), query], 128 * 1024 * 1024)
                .expect("freeze selected face");
        assert_eq!(prepared.binding_count(), 1);
        assert_eq!(prepared.face_count(), 1);
        assert_eq!(prepared.source_count(), 1);
        assert!(prepared.retained_bytes() > 0);

        let mut rasterizer =
            BasicTitleRasterizer::with_prepared_font_set(4, 16 * 1024 * 1024, &prepared)
                .expect("construct frozen-source rasterizer");
        let frame = rasterizer
            .rasterize(
                &title,
                Resolution::FHD,
                0.20,
                Resolution::HD,
                WorkingColorSpace::LinearRec709,
            )
            .expect("rasterize from frozen face bytes");
        assert_eq!(
            frame.diagnostics().face_fingerprint,
            prepared.faces.values().next().expect("frozen face").fingerprint()
        );
    }

    #[test]
    fn prepared_font_set_rejects_face_bytes_beyond_grant() {
        let query = BasicTitleFontQuery::from_title(&evaluated("Budget"));
        let error = PreparedBasicTitleFontSet::prepare([query], 0)
            .expect_err("font bytes must be admitted before offline execution");
        assert!(matches!(
            error,
            BasicTitleRasterError::PreparedFontByteBudgetExceeded { limit: 0, .. }
        ));
    }

    #[test]
    fn aggregate_budget_includes_swash_glyph_residency() {
        let mut rasterizer = BasicTitleRasterizer::with_cache_budget(1, 1);
        rasterizer
            .rasterize(
                &evaluated("Bounded glyph cache"),
                Resolution::FHD,
                0.20,
                Resolution::HD,
                WorkingColorSpace::LinearRec709,
            )
            .expect("raster");

        assert!(rasterizer.retained_cache_bytes() <= rasterizer.cache_byte_budget());
        assert!(rasterizer.cache.is_empty());
        assert!(rasterizer.swash_cache.image_cache.is_empty());
        assert!(rasterizer.swash_cache.outline_command_cache.is_empty());
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
        let query = BasicTitleFontQuery {
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
        let narrow = basic_title_raster_request_identity(
            &title,
            Resolution::FHD,
            0.10,
            Resolution::HD,
            WorkingColorSpace::LinearRec709,
        );
        let wide = basic_title_raster_request_identity(
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
    fn title_raster_request_identity_covers_every_rendering_input() {
        let title = evaluated("Identity");
        let base = basic_title_raster_request_identity(
            &title,
            Resolution::FHD,
            0.20,
            Resolution::HD,
            WorkingColorSpace::LinearRec709,
        );
        let mut variants = Vec::new();

        let mut changed = title.clone();
        changed.text.push('!');
        variants.push(("text", changed));
        let mut changed = title.clone();
        changed.font_family.push_str(" Alternate");
        variants.push(("font_family", changed));
        let mut changed = title.clone();
        changed.font_weight = changed.font_weight.saturating_add(1);
        variants.push(("font_weight", changed));
        let mut changed = title.clone();
        changed.font_style = BasicTitleFontStyle::Italic;
        variants.push(("font_style", changed));
        let mut changed = title.clone();
        changed.font_size += 1.0;
        variants.push(("font_size", changed));
        let mut changed = title.clone();
        changed.fill.r = if changed.fill.r.to_bits() == 0.25f32.to_bits() {
            0.5
        } else {
            0.25
        };
        variants.push(("fill", changed));
        let mut changed = title.clone();
        changed.tracking_em += 0.01;
        variants.push(("tracking", changed));
        let mut changed = title.clone();
        changed.line_height += 0.1;
        variants.push(("line_height", changed));
        let mut changed = title.clone();
        changed.horizontal_align = BasicTitleHorizontalAlign::Left;
        variants.push(("horizontal_align", changed));
        let mut changed = title.clone();
        changed.vertical_align = BasicTitleVerticalAlign::Top;
        variants.push(("vertical_align", changed));

        for (field, changed) in variants {
            assert_ne!(
                base,
                basic_title_raster_request_identity(
                    &changed,
                    Resolution::FHD,
                    0.20,
                    Resolution::HD,
                    WorkingColorSpace::LinearRec709,
                ),
                "{field} must participate in Basic Title request identity"
            );
        }

        assert_ne!(
            base,
            basic_title_raster_request_identity(
                &title,
                Resolution { width: 1919, height: 1080 },
                0.20,
                Resolution::HD,
                WorkingColorSpace::LinearRec709,
            ),
            "author extent must participate in Basic Title request identity"
        );
        assert_ne!(
            base,
            basic_title_raster_request_identity(
                &title,
                Resolution::FHD,
                0.21,
                Resolution::HD,
                WorkingColorSpace::LinearRec709,
            ),
            "title-safe margin must participate in Basic Title request identity"
        );
        assert_ne!(
            base,
            basic_title_raster_request_identity(
                &title,
                Resolution::FHD,
                0.20,
                Resolution { width: 1279, height: 720 },
                WorkingColorSpace::LinearRec709,
            ),
            "sampled extent must participate in Basic Title request identity"
        );
        assert_ne!(
            base,
            basic_title_raster_request_identity(
                &title,
                Resolution::FHD,
                0.20,
                Resolution::HD,
                WorkingColorSpace::LinearRec2020,
            ),
            "working color space must participate in Basic Title request identity"
        );
    }

    #[test]
    fn title_raster_identity_binds_font_and_uses_all_strong_identity_bytes() {
        let request = basic_title_raster_request_identity(
            &evaluated("Font Identity"),
            Resolution::FHD,
            0.20,
            Resolution::HD,
            WorkingColorSpace::LinearRec709,
        );
        assert_eq!(
            title_raster_identity(request, [0x11; 32]),
            title_raster_identity(request, [0x11; 32])
        );
        assert_ne!(
            title_raster_identity(request, [0x11; 32]),
            title_raster_identity(request, [0x22; 32]),
            "resolved font bytes and face index must participate in frame identity"
        );

        let mut first = [0x44; 32];
        let mut second = first;
        first[31] = 0x55;
        second[31] = 0xaa;
        assert_eq!(&first[..8], &second[..8]);
        let first = BasicTitleRasterIdentity(first);
        let second = BasicTitleRasterIdentity(second);
        let mut cache = HashMap::new();
        cache.insert(first, "first");
        cache.insert(second, "second");
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&first), Some(&"first"));
        assert_eq!(cache.get(&second), Some(&"second"));
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
