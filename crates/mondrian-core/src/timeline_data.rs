//! Timeline-agnostic data types shared between `mondrian-timeline` and
//! `mondrian-renderer`. They keep frame lowering independent from author-model
//! traversal while allowing a separate renderer-owned compiler Module to bind
//! an immutable `Sequence` revision into a prepared execution program.
//!
//! ## Architecture (P-ARCH2)
//! - `mondrian-timeline` defines `Sequence`, `Track`, `Clip` and implements
//!   `RenderPlanSource` to project them into ordered `FlatVisualItem` values.
//! - The low-level render-plan lowering Interface consumes
//!   `&dyn RenderPlanSource` and never traverses author objects.
//! - The higher-level prepared-program compiler intentionally accepts one
//!   immutable `Sequence` revision, then publishes only the flat schedule and
//!   prepared Effect programs to repeated frame evaluation.

use crate::automation::PropertyBag;
use crate::effect_data::EffectNode;
use crate::mask_data::MaskComponent;
use crate::types::{
    AssetId, BlendMode, ClipId, Color, ColorSpace, Rational, SequenceId, VideoTransitionId,
    WorkingColorSpace,
};
use crate::{BasicTitle, Result, SequenceRevision, SourceSampleTarget, TimelineTime};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Pure data enums (moved from mondrian-timeline) ────────────────────

/// The semantic kind of a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClipKind {
    #[default]
    Media,
    AdjustmentLayer,
    NestedSequence,
    SolidColor,
    BasicTitle,
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

impl MediaInterpretation {
    /// Closed picture-geometry override projection for execution Adapters.
    pub const fn picture_overrides(&self) -> crate::PictureInterpretationOverrides {
        crate::PictureInterpretationOverrides {
            pixel_aspect_ratio: self.pixel_aspect_ratio_override,
            field_order: self.field_order_override,
        }
    }
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
    ///
    /// Color integration belongs to this parent-to-child placement edge, not
    /// to either Sequence node. The same child can therefore be integrated
    /// differently by different parents without mutating the child.
    NestedSequence {
        sequence_id: SequenceId,
        color_processing: NestedColorProcessing,
    },
    /// Deterministic project generator sourced from a reusable palette entry.
    SolidColor { asset_id: AssetId, color: Color },
    /// Sequence-local generated text with a closed parameter definition.
    BasicTitle { title: BasicTitle },
}

impl ClipContent {
    /// Stable discriminator for presentation and dispatch adapters.
    pub const fn kind(&self) -> ClipKind {
        match self {
            Self::Media { .. } => ClipKind::Media,
            Self::AdjustmentLayer { .. } => ClipKind::AdjustmentLayer,
            Self::NestedSequence { .. } => ClipKind::NestedSequence,
            Self::SolidColor { .. } => ClipKind::SolidColor,
            Self::BasicTitle { .. } => ClipKind::BasicTitle,
        }
    }

    /// Project Asset Library identity for asset-backed content.
    ///
    /// This includes generated palette/effect entries and therefore does not
    /// imply that the asset owns a readable external media file. Callers that
    /// need a decode/export file dependency must use [`Self::media_asset_id`].
    pub const fn library_asset_id(&self) -> Option<AssetId> {
        match self {
            Self::Media { asset_id, .. }
            | Self::AdjustmentLayer { asset_id }
            | Self::SolidColor { asset_id, .. } => Some(*asset_id),
            Self::NestedSequence { .. } | Self::BasicTitle { .. } => None,
        }
    }

    /// File-backed media identity that requires a resolved media dependency.
    pub const fn media_asset_id(&self) -> Option<AssetId> {
        match self {
            Self::Media { asset_id, .. } => Some(*asset_id),
            Self::AdjustmentLayer { .. }
            | Self::NestedSequence { .. }
            | Self::SolidColor { .. }
            | Self::BasicTitle { .. } => None,
        }
    }

    /// Nested Sequence identity when this is nested content.
    pub const fn nested_sequence_id(&self) -> Option<SequenceId> {
        match self {
            Self::NestedSequence { sequence_id, .. } => Some(*sequence_id),
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

    /// Basic Title author state when this is generated text content.
    pub const fn basic_title(&self) -> Option<&BasicTitle> {
        match self {
            Self::BasicTitle { title } => Some(title),
            _ => None,
        }
    }

    /// Mutable Basic Title author state for command routing.
    pub fn basic_title_mut(&mut self) -> Option<&mut BasicTitle> {
        match self {
            Self::BasicTitle { title } => Some(title),
            _ => None,
        }
    }

    /// Fork author identities owned by this content occurrence.
    pub fn fork_author_identities(&mut self) {
        if let Self::BasicTitle { title } = self {
            title.fork_author_identities();
        }
    }
}

impl crate::AuthoringFootprint for ClipContent {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::BasicTitle { title } => collector.collect(title),
            Self::Media { asset_id: _, interpretation: _ }
            | Self::AdjustmentLayer { asset_id: _ }
            | Self::NestedSequence { sequence_id: _, color_processing: _ }
            | Self::SolidColor { asset_id: _, color: _ } => Ok(()),
        }
    }
}

/// Pixel aspect ratio presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
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
    /// Exact sample aspect ratio used by execution and delivery.
    pub fn exact_ratio(self) -> Option<crate::SampleAspectRatio> {
        let (numerator, denominator) = match self {
            Self::Square => (1, 1),
            Self::D1DvNtsc => (10, 11),
            Self::D1DvNtscWidescreen => (40, 33),
            Self::D1DvPal => (12, 11),
            Self::D1DvPalWidescreen => (16, 11),
            Self::Anamorphic2x => (2, 1),
            Self::HdAnamorphic1080 => (4, 3),
            Self::DvcproHd => (3, 2),
            Self::Unknown => return None,
        };
        crate::SampleAspectRatio::new(numerator, denominator)
    }

    pub fn ratio(self) -> Option<f32> {
        self.exact_ratio().map(|ratio| ratio.to_f64() as f32)
    }
}

/// Field order for interlaced media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum FieldOrder {
    #[default]
    Progressive,
    UpperFirst,
    LowerFirst,
}

/// How nested sequence color processing interacts with the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
pub enum NestedColorProcessing {
    /// Composite in the child's authored working space, then convert exactly
    /// once into the parent working space at the placement edge.
    #[default]
    PreserveChildWorkingSpace,
    /// Evaluate the child directly in the parent working space.
    ForceParentWorkingSpace,
}

/// Which use of a Clip placement produced one execution request.
///
/// Transition endpoints retain their explicit side because querying "the
/// active Transition" again at a historical Effect time can select a different
/// editorial operation or no operation at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineClipEndpointContext {
    /// Ordinary Track-stack evaluation outside a Transition replacement.
    Ordinary,
    /// The earlier endpoint of one exact authored Transition.
    TransitionLeft {
        /// Transition whose left endpoint owns this request.
        transition_id: VideoTransitionId,
    },
    /// The later endpoint of one exact authored Transition.
    TransitionRight {
        /// Transition whose right endpoint owns this request.
        transition_id: VideoTransitionId,
    },
}

/// Stable placement identity carried from the prepared author snapshot into
/// Preview and Export execution.
///
/// `clip_time` is the current output sample. Historical requests retain this
/// placement reference and provide a different requested Clip time to the
/// prepared schedule's canonical sampling API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineClipExecutionRef {
    /// Sequence that owns the placement.
    pub sequence_id: SequenceId,
    /// Exact conservative author revision compiled for execution.
    pub sequence_revision: SequenceRevision,
    /// Stable Clip occurrence identity.
    pub clip_id: ClipId,
    /// Current Clip-local visual author time.
    pub clip_time: TimelineTime,
    /// Ordinary or exact Transition-endpoint context.
    pub endpoint: TimelineClipEndpointContext,
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
    /// Immutable effect author snapshot for this Sequence revision.
    ///
    /// Prepared visual evaluation shares this payload across frame queries so
    /// animated parameter sampling does not clone every Property Bag.
    pub effects: Arc<[EffectNode]>,
    /// Immutable mask author snapshot for this Sequence revision.
    pub masks: Arc<[MaskComponent]>,
    /// Stable Clip-local visual author time for all Clip-owned processing.
    pub clip_time: TimelineTime,
    /// Exact source coordinate and half-open boundary ownership.
    pub source_sample: SourceSampleTarget,
    /// Affine transform matrix as 6-element array [a, c, tx, b, d, ty].
    pub transform_matrix: [f32; 6],
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub track_index: usize,
}

/// Timeline-agnostic identity of a two-input visual Transition definition.
///
/// The authoring crate owns endpoint geometry and persistence. The renderer
/// receives only this closed execution-facing discriminator, so it does not
/// need to depend on `Sequence` or author-model types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlatVideoTransitionDefinition {
    /// Scene-linear, coverage-correct two-input Cross Dissolve.
    CrossDissolve,
    /// Recoverable author intent for an externally supplied definition.
    Plugin { definition_id: String },
}

/// Immutable execution snapshot of one visual Transition definition instance.
///
/// Prepared visual evaluation shares this value across frame queries. Dynamic
/// progress and endpoint sampling remain frame-local, while definition
/// identity, parameter automation, and opaque payload are copied exactly once
/// for the owning Sequence revision.
#[derive(Debug, Clone)]
pub struct FlatVideoTransitionDefinitionSnapshot {
    /// Built-in or external definition selected by the author.
    pub definition: FlatVideoTransitionDefinition,
    /// Definition-described parameter state.
    pub properties: PropertyBag,
    /// Definition-specific non-parameter payload.
    pub params: serde_json::Value,
}

/// Exact progress coordinates for one Transition evaluation.
///
/// Keeping elapsed and duration exact until render-plan lowering avoids
/// making a floating-point sample the cache or scheduling authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlatTransitionProgress {
    /// Exact elapsed Sequence time since the Transition range start.
    pub elapsed: TimelineTime,
    /// Exact non-zero Transition duration.
    pub duration: TimelineTime,
}

impl FlatTransitionProgress {
    /// Project exact author time to the normalized execution coefficient.
    pub fn normalized(self) -> Option<f32> {
        if self.duration <= TimelineTime::ZERO
            || self.elapsed.is_negative()
            || self.elapsed > self.duration
        {
            return None;
        }
        let normalized = self.elapsed.to_f64() / self.duration.to_f64();
        normalized.is_finite().then_some(normalized as f32)
    }
}

/// Flattened two-input visual Transition at one Sequence time.
#[derive(Debug, Clone)]
pub struct FlatVideoTransition {
    /// Stable author identity for diagnostics and future execution caches.
    pub transition_id: VideoTransitionId,
    /// Shared immutable definition and parameter snapshot.
    pub definition: Arc<FlatVideoTransitionDefinitionSnapshot>,
    /// Earlier edit endpoint evaluated at the requested Sequence time.
    pub left: FlatActiveClip,
    /// Later edit endpoint evaluated at the requested Sequence time.
    pub right: FlatActiveClip,
    /// Exact normalized-progress source coordinates.
    pub progress: FlatTransitionProgress,
}

/// One ordered visual item emitted by timeline semantic evaluation.
///
/// A Transition replaces its two endpoint placements at that track position;
/// consumers must not independently composite those endpoints a second time.
#[derive(Debug, Clone)]
pub enum FlatVisualItem {
    /// One ordinary active Clip.
    Clip(FlatActiveClip),
    /// One explicit two-input visual Transition.
    Transition(Box<FlatVideoTransition>),
}

// ── Trait for render plan sources ─────────────────────────────────────

/// Source of timeline data for building render plans.
///
/// Implemented by `Sequence` in `mondrian-timeline`. The renderer only
/// knows about this trait, never about `Sequence` itself.
pub trait RenderPlanSource {
    /// Return the ordered visual program at a given time, flattened.
    fn flat_visual_items_at(&self, time: TimelineTime) -> Result<Vec<FlatVisualItem>>;

    /// Sequence identity represented by this immutable source.
    fn source_sequence_id(&self) -> SequenceId;

    /// Exact conservative author revision represented by this source.
    fn source_sequence_revision(&self) -> SequenceRevision;

    /// Time base of the sequence.
    fn source_time_base(&self) -> Rational;

    /// Linear-light working color space in which clip effects execute.
    fn source_working_color_space(&self) -> WorkingColorSpace;

    /// Whether to auto tone-map media to the working color space.
    fn auto_tone_map_media(&self) -> bool;
}
