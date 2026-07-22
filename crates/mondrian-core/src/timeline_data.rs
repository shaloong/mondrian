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
use crate::types::{AssetId, BlendMode, ClipId, Color, ColorSpace, Rational, SequenceId};
use crate::{Result, TimelineTime};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum AlphaInterpretation {
    #[default]
    Straight,
    Premultiplied,
    Ignore,
}

/// User-selected color interpretation mode for a media asset.
///
/// `Auto` stores user intent, not a resolved color space. The active color
/// space is resolved at runtime from media metadata, detection policy, and
/// project color management settings. `Override` is the only mode that pins a
/// user-authored interpretation and must not be replaced by later auto-detect
/// improvements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MediaColorInterpretation {
    /// Resolve color interpretation automatically from media metadata and
    /// project color management policy.
    #[default]
    Auto,
    /// Use this explicit user override instead of detected media metadata.
    Override {
        /// The color space selected by the user.
        color_space: ColorSpace,
    },
}

impl MediaColorInterpretation {
    /// Returns the user override color space when this interpretation pins one.
    pub fn override_color_space(self) -> Option<ColorSpace> {
        match self {
            Self::Override { color_space } => Some(color_space),
            Self::Auto => None,
        }
    }
}

/// User-authored quantization range for encoded video samples.
///
/// This core-owned enum deliberately does not depend on FFmpeg or a decoder
/// representation. Media adapters translate it at their decode boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaSignalRange {
    /// Studio/legal-range encoded samples.
    Limited,
    /// Full-range encoded samples.
    Full,
}

/// User-selected range interpretation mode for a media asset.
///
/// `Auto` follows the current probe result. `Override` is authoritative when
/// media carries absent or incorrect range metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MediaRangeInterpretation {
    /// Follow the range resolved by media probing.
    #[default]
    Auto,
    /// Use an explicit quantization range instead of probed metadata.
    Override {
        /// The range selected by the user.
        range: MediaSignalRange,
    },
}

impl MediaRangeInterpretation {
    /// Returns the user override range when this interpretation pins one.
    pub fn override_range(self) -> Option<MediaSignalRange> {
        match self {
            Self::Override { range } => Some(range),
            Self::Auto => None,
        }
    }
}

/// Whether an asset payload represents color-managed picture data.
///
/// This is intentionally separate from `MediaColorInterpretation`: "non-color
/// data" is an asset/workflow property, not a color-space choice in the
/// Interpret Footage dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AssetColorPayload {
    /// Normal picture media that participates in color management.
    #[default]
    ColorManaged,
    /// Data payload such as masks, mattes, height/normal maps, or technical
    /// textures. Color transforms must not be applied to the payload.
    NonColorData,
}

impl AssetColorPayload {
    /// Whether this payload must bypass color-managed interpretation.
    pub fn is_non_color_data(self) -> bool {
        matches!(self, Self::NonColorData)
    }
}

/// Persistent media interpretation stored on an asset library record.
///
/// This is deliberately separated from probe results. Asset records keep the
/// user's color and signal-range modes (`Auto` or `Override`) plus payload
/// kind; resolved diagnostics remain runtime data so detector/config upgrades
/// can improve Auto behavior without rewriting library state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AssetMediaInterpretation {
    /// Color interpretation intent for the asset.
    #[serde(default)]
    pub color: MediaColorInterpretation,
    /// Encoded video quantization-range intent for the asset.
    #[serde(default)]
    pub range: MediaRangeInterpretation,
    /// Payload kind that decides whether color management applies at all.
    #[serde(default)]
    pub payload: AssetColorPayload,
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

/// Closed set of authored Clip payloads.
///
/// Payload-specific data lives inside the matching variant, so a persisted
/// Clip cannot claim one kind while carrying missing or contradictory fields
/// from another kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClipContent {
    /// File-backed media with placement-local interpretation overrides.
    Media {
        asset_id: AssetId,
        #[serde(default)]
        interpretation: MediaInterpretation,
    },
    /// Effect-only layer sourced from a reusable project asset entry.
    AdjustmentLayer { asset_id: AssetId },
    /// Public output of another Sequence in the same Project.
    NestedSequence { sequence_id: SequenceId },
    /// Deterministic project generator sourced from a reusable palette entry.
    SolidColor { asset_id: AssetId, color: Color },
}

impl ClipContent {
    /// Stable discriminator for presentation and dispatch adapters.
    pub const fn kind(&self) -> ClipKind {
        match self {
            Self::Media { .. } => ClipKind::Media,
            Self::AdjustmentLayer { .. } => ClipKind::AdjustmentLayer,
            Self::NestedSequence { .. } => ClipKind::NestedSequence,
            Self::SolidColor { .. } => ClipKind::SolidColor,
        }
    }

    /// Project asset when this content is backed by the asset library.
    pub const fn asset_id(&self) -> Option<AssetId> {
        match self {
            Self::Media { asset_id, .. }
            | Self::AdjustmentLayer { asset_id }
            | Self::SolidColor { asset_id, .. } => Some(*asset_id),
            Self::NestedSequence { .. } => None,
        }
    }

    /// Nested Sequence identity when this is nested content.
    pub const fn nested_sequence_id(&self) -> Option<SequenceId> {
        match self {
            Self::NestedSequence { sequence_id } => Some(*sequence_id),
            _ => None,
        }
    }

    /// Read media interpretation only for file-backed media.
    pub const fn media_interpretation(&self) -> Option<&MediaInterpretation> {
        match self {
            Self::Media { interpretation, .. } => Some(interpretation),
            _ => None,
        }
    }

    /// Mutate media interpretation only for file-backed media.
    pub fn media_interpretation_mut(&mut self) -> Option<&mut MediaInterpretation> {
        match self {
            Self::Media { interpretation, .. } => Some(interpretation),
            _ => None,
        }
    }

    /// Solid generator color when this is a solid-color payload.
    pub const fn solid_color(&self) -> Option<Color> {
        match self {
            Self::SolidColor { color, .. } => Some(*color),
            _ => None,
        }
    }
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
    pub clip_id: ClipId,
    pub content: ClipContent,
    pub is_disabled: bool,
    pub effects: Vec<EffectNode>,
    pub masks: Vec<MaskComponent>,
    pub source_time: TimelineTime,
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
    fn flat_active_clips_at(&self, time: TimelineTime) -> Result<Vec<FlatActiveClip>>;

    /// Time base of the sequence.
    fn source_time_base(&self) -> Rational;

    /// Nested color processing mode for nested sequences.
    fn nested_color_processing(&self) -> NestedColorProcessing;

    /// Whether to auto tone-map media to the working color space.
    fn auto_tone_map_media(&self) -> bool;
}
