//! Timeline-agnostic data types shared between `mondrian-timeline` and
//! `mondrian-renderer`. These types decouple the renderer from the timeline
//! crate so the renderer only sees flat data, never `Sequence` internals.
//!
//! ## Architecture (P-ARCH2)
//! - `mondrian-timeline` defines `Sequence`, `Track`, `Clip` and implements
//!   `RenderPlanSource` to project them into `FlatActiveClip` slices.
//! - `mondrian-renderer` consumes `&dyn RenderPlanSource` only, with zero
//!   knowledge of `Sequence`/`Track`/`Clip`.

use crate::effect_data::EffectNode;
use crate::mask_data::MaskComponent;
use crate::types::{AssetId, BlendMode, ClipId, Color, ColorSpace, Rational, SequenceId, TimeCode};
use serde::{Deserialize, Serialize};

// ── Pure data enums (moved from mondrian-timeline) ────────────────────

/// The semantic kind of a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClipKind {
    #[default]
    Media,
    AdjustmentLayer,
    NestedSequence,
    SolidColor,
}

/// How to interpret alpha channel in media assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AlphaInterpretation {
    #[default]
    Straight,
    Premultiplied,
    Ignore,
}

/// Overrides for media asset metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MediaInterpretation {
    #[serde(default)]
    pub color_space_override: Option<ColorSpace>,
    #[serde(default)]
    pub frame_rate_override: Option<Rational>,
    #[serde(default)]
    pub pixel_aspect_ratio_override: Option<PixelAspectRatio>,
    #[serde(default)]
    pub field_order_override: Option<FieldOrder>,
    #[serde(default)]
    pub alpha: AlphaInterpretation,
}

/// Pixel aspect ratio presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PixelAspectRatio {
    #[default]
    Square,
    D1DvNtsc,
    D1DvNtscWidescreen,
    D1DvPal,
    D1DvPalWidescreen,
    Anamorphic2x,
    HdAnamorphic1080,
    DvcproHd,
    Unknown,
}

impl PixelAspectRatio {
    pub fn ratio(self) -> Option<f32> {
        match self {
            Self::Square => Some(1.0),
            Self::D1DvNtsc => Some(0.9091),
            Self::D1DvNtscWidescreen => Some(1.2121),
            Self::D1DvPal => Some(1.0940),
            Self::D1DvPalWidescreen => Some(1.4587),
            Self::Anamorphic2x => Some(2.0),
            Self::HdAnamorphic1080 => Some(1.333),
            Self::DvcproHd => Some(1.5),
            Self::Unknown => None,
        }
    }
}

/// Field order for interlaced media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FieldOrder {
    #[default]
    Progressive,
    UpperFirst,
    LowerFirst,
}

/// How nested sequence color processing interacts with the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
pub enum NestedColorProcessing {
    #[default]
    PreserveChildWorkingSpace,
    ForceParentWorkingSpace,
    BakeChildOutputTransform,
}

// ── Flat clip representation (no timeline internals) ──────────────────

/// A flattened view of an active clip for render plan construction.
///
/// This carries every field the render plan builder needs without
/// exposing `Clip`, `Track`, or `Sequence` internals.
#[derive(Debug, Clone)]
pub struct FlatActiveClip {
    pub asset_id: AssetId,
    pub clip_id: ClipId,
    pub kind: ClipKind,
    pub nested_sequence_id: Option<SequenceId>,
    pub is_disabled: bool,
    pub effects: Vec<EffectNode>,
    pub masks: Vec<MaskComponent>,
    pub solid_color: Option<Color>,
    pub interpretation: MediaInterpretation,
    pub source_time: TimeCode,
    /// Affine transform matrix as 6-element array [a, c, tx, b, d, ty].
    pub transform_matrix: [f32; 6],
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub track_index: usize,
}

// ── Trait for render plan sources ─────────────────────────────────────

/// Source of timeline data for building render plans.
///
/// Implemented by `Sequence` in `mondrian-timeline`. The renderer only
/// knows about this trait, never about `Sequence` itself.
pub trait RenderPlanSource {
    /// Return all active clips at a given time, flattened.
    fn flat_active_clips_at(&self, time: TimeCode) -> Vec<FlatActiveClip>;

    /// Time base of the sequence.
    fn source_time_base(&self) -> Rational;

    /// Nested color processing mode for nested sequences.
    fn nested_color_processing(&self) -> NestedColorProcessing;

    /// Whether to auto tone-map media to the working color space.
    fn auto_tone_map_media(&self) -> bool;
}
