//! 序列（时间线）

use crate::{
    clip::{ActiveClip, Clip},
    track::{Track, TrackType},
};
pub use mondrian_core::AudioChannelLayout;
use mondrian_core::{
    types::*, AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError,
    AuthoringList, AuthoringSnapshot, DisplayToneMapPolicy, ProjectColorEnvironment,
    SmpteCountingMode, TimelineDisplayContract, TimelineDisplayFormat, TimelineDisplaySettings,
    TimelineTime, VideoContentLightMetadata, VideoMasteringDisplayMetadata,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Maximum supported nested-sequence recursion depth for preview, export, and
/// diagnostics.
///
/// The root sequence is depth 0. Nested renderers should reject or stop
/// recursing only when `depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH`, so preview and
/// export share the same practical nesting contract.
pub const MAX_NESTED_SEQUENCE_RENDER_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EditingMode {
    #[default]
    Custom,
    Dslr1080p,
    Dslr720p,
    Avchd1080p,
    DigitalCinema4k,
    SocialVertical1080p,
}

impl AuthoringFootprint for EditingMode {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Custom
            | Self::Dslr1080p
            | Self::Dslr720p
            | Self::Avchd1080p
            | Self::DigitalCinema4k
            | Self::SocialVertical1080p => Ok(()),
        }
    }
}

// Re-exported from mondrian_core::timeline_data.
use mondrian_core::timeline_data::{AssetMediaInterpretation, MediaColorInterpretation};
pub use mondrian_core::timeline_data::{FieldOrder, PixelAspectRatio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioDisplayFormat {
    #[default]
    AudioSamples,
    Milliseconds,
}

impl AuthoringFootprint for AudioDisplayFormat {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::AudioSamples | Self::Milliseconds => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewRenderFormat {
    #[default]
    IFrameOnly,
    ProResProxy,
    DnxHrLb,
    LosslessRgba,
}

impl AuthoringFootprint for PreviewRenderFormat {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::IFrameOnly | Self::ProResProxy | Self::DnxHrLb | Self::LosslessRgba => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SequencePreviewSettings {
    #[serde(default)]
    pub format: PreviewRenderFormat,
    #[serde(default = "default_preview_resolution_scale")]
    pub resolution_scale: f32,
    #[serde(default = "default_preview_cache_enabled")]
    pub cache_enabled: bool,
}

impl AuthoringFootprint for SequencePreviewSettings {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { format, resolution_scale: _, cache_enabled: _ } = self;
        collector.collect(format)
    }
}

impl Default for SequencePreviewSettings {
    fn default() -> Self {
        Self {
            format: PreviewRenderFormat::IFrameOnly,
            resolution_scale: default_preview_resolution_scale(),
            cache_enabled: default_preview_cache_enabled(),
        }
    }
}

const fn default_preview_resolution_scale() -> f32 {
    0.5
}

const fn default_preview_cache_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SequenceRole {
    #[default]
    Editorial,
    NestedComposition,
}

impl AuthoringFootprint for SequenceRole {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Editorial | Self::NestedComposition => Ok(()),
        }
    }
}

/// Program rendering domain independently of the project OCIO engine.
///
/// The project `ColorEngine` is the sole Mondrian Standard / ACES / Custom OCIO
/// mode selector. This enum only decides whether the selected engine's product
/// View participates at the output boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorWorkflow {
    /// Conventional video workflow using a direct colorimetric output processor.
    DisplayReferred,
    /// Scene-linear program rendering followed by the selected engine's rendering View.
    #[default]
    SceneReferred,
}

impl AuthoringFootprint for ColorWorkflow {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::DisplayReferred | Self::SceneReferred => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum MissingColorMetadataPolicy {
    /// 无标签素材视为 Rec.709（行业默认）。
    #[default]
    AssumeRec709,
    /// 无标签素材拒绝导入 / 跳过渲染。
    RejectMedia,
}

impl AuthoringFootprint for MissingColorMetadataPolicy {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::AssumeRec709 | Self::RejectMedia => Ok(()),
        }
    }
}

/// Source that decided a media input color space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputColorResolutionSource {
    /// Clip/media interpretation override won.
    Override,
    /// Asset is explicitly marked as data/non-color content.
    DataTexture,
    /// Explicit media metadata identified the input color space.
    DetectedMetadata,
    /// Missing metadata policy assumed Rec.709.
    MissingPolicyAssumeRec709,
    /// Missing metadata policy rejected the media.
    MissingPolicyRejectMedia,
}

impl InputColorResolutionSource {
    /// Whether this branch used explicit user/project metadata instead of policy inference.
    pub fn is_explicit_metadata_or_override(self) -> bool {
        matches!(self, Self::Override | Self::DetectedMetadata)
    }

    /// Whether this branch kept rendering moving by assuming a color space from policy.
    pub fn is_policy_assumption(self) -> bool {
        matches!(self, Self::MissingPolicyAssumeRec709)
    }

    /// Whether this branch rejected media due to missing or unsupported metadata.
    pub fn is_policy_rejection(self) -> bool {
        matches!(self, Self::MissingPolicyRejectMedia)
    }

    /// Whether this branch treated the asset as non-color data.
    pub fn is_data_texture(self) -> bool {
        matches!(self, Self::DataTexture)
    }
}

/// Counts of media input color-resolution branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InputColorResolutionSourceCounts {
    /// Clip/media interpretation overrides.
    pub override_count: u64,
    /// Asset inputs classified as non-color data.
    pub data_texture: u64,
    /// Inputs resolved by explicit media metadata.
    pub detected_metadata: u64,
    /// Inputs assumed as Rec.709 by missing-metadata policy.
    pub missing_assume_rec709: u64,
    /// Inputs rejected by missing-metadata policy.
    pub missing_rejected: u64,
}

impl InputColorResolutionSourceCounts {
    /// Record one input color-resolution branch.
    pub fn record(&mut self, source: InputColorResolutionSource) {
        match source {
            InputColorResolutionSource::Override => {
                self.override_count = self.override_count.saturating_add(1);
            }
            InputColorResolutionSource::DataTexture => {
                self.data_texture = self.data_texture.saturating_add(1);
            }
            InputColorResolutionSource::DetectedMetadata => {
                self.detected_metadata = self.detected_metadata.saturating_add(1);
            }
            InputColorResolutionSource::MissingPolicyAssumeRec709 => {
                self.missing_assume_rec709 = self.missing_assume_rec709.saturating_add(1);
            }
            InputColorResolutionSource::MissingPolicyRejectMedia => {
                self.missing_rejected = self.missing_rejected.saturating_add(1);
            }
        }
    }

    /// Add counts from another input color-resolution counter.
    pub fn accumulate(&mut self, other: Self) {
        self.override_count = self.override_count.saturating_add(other.override_count);
        self.data_texture = self.data_texture.saturating_add(other.data_texture);
        self.detected_metadata = self.detected_metadata.saturating_add(other.detected_metadata);
        self.missing_assume_rec709 =
            self.missing_assume_rec709.saturating_add(other.missing_assume_rec709);
        self.missing_rejected = self.missing_rejected.saturating_add(other.missing_rejected);
    }

    /// Count for one exact branch.
    pub fn count(self, source: InputColorResolutionSource) -> u64 {
        match source {
            InputColorResolutionSource::Override => self.override_count,
            InputColorResolutionSource::DataTexture => self.data_texture,
            InputColorResolutionSource::DetectedMetadata => self.detected_metadata,
            InputColorResolutionSource::MissingPolicyAssumeRec709 => self.missing_assume_rec709,
            InputColorResolutionSource::MissingPolicyRejectMedia => self.missing_rejected,
        }
    }

    /// Count of branches resolved from explicit metadata or user override.
    pub fn explicit_metadata_or_override(self) -> u64 {
        self.sum_by_source(InputColorResolutionSource::is_explicit_metadata_or_override)
    }

    /// Count of branches that assumed a color space from missing-metadata policy.
    pub fn policy_assumptions(self) -> u64 {
        self.sum_by_source(InputColorResolutionSource::is_policy_assumption)
    }

    /// Count of branches rejected by missing-metadata policy.
    pub fn policy_rejections(self) -> u64 {
        self.sum_by_source(InputColorResolutionSource::is_policy_rejection)
    }

    /// Count of branches treated as non-color data.
    pub fn data_textures(self) -> u64 {
        self.sum_by_source(InputColorResolutionSource::is_data_texture)
    }

    /// Total counted branches.
    pub fn total(self) -> u64 {
        self.override_count
            .saturating_add(self.data_texture)
            .saturating_add(self.detected_metadata)
            .saturating_add(self.missing_assume_rec709)
            .saturating_add(self.missing_rejected)
    }

    fn sum_by_source(self, predicate: impl Fn(InputColorResolutionSource) -> bool) -> u64 {
        INPUT_COLOR_RESOLUTION_SOURCES
            .iter()
            .copied()
            .filter_map(|source| predicate(source).then_some(self.count(source)))
            .fold(0, u64::saturating_add)
    }
}

const INPUT_COLOR_RESOLUTION_SOURCES: [InputColorResolutionSource; 5] = [
    InputColorResolutionSource::Override,
    InputColorResolutionSource::DataTexture,
    InputColorResolutionSource::DetectedMetadata,
    InputColorResolutionSource::MissingPolicyAssumeRec709,
    InputColorResolutionSource::MissingPolicyRejectMedia,
];

/// Result of resolving clip override, detected media metadata, and missing-metadata policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputColorResolution {
    /// Typed result of resolving the input color contract.
    pub resolved: ResolvedInputColor,
    /// Decision branch that produced the result.
    pub source: InputColorResolutionSource,
    /// Clip/media color-space override supplied by the user.
    pub override_color_space: Option<ColorSpace>,
    /// Validated metadata identity that was eligible to drive pixels.
    pub executable_color_space: Option<ColorSpace>,
    /// Missing metadata policy active during the decision.
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    /// Sequence working color space active during the decision.
    pub working_color_space: WorkingColorSpace,
}

/// Effective interpretation of decoded source samples before the working transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedInputColor {
    /// Encoded color samples that require an input transform.
    Color(ColorSpace),
    /// Non-color data samples; color transforms must be bypassed deliberately.
    Data,
    /// Missing metadata policy rejected the source.
    Rejected,
}

impl MissingColorMetadataPolicy {
    /// 根据策略和经过验证的可执行元数据，解析有效的输入色彩空间。
    ///
    /// `executable_metadata` 来自媒体探测的封闭证据，`working` 是序列工作空间。
    /// 当素材无可执行色彩身份时，按显式策略行事。
    /// Resolve the effective input color space and retain the decision branch for diagnostics.
    pub fn resolve_input_decision(
        self,
        override_color_space: Option<ColorSpace>,
        executable_metadata: Option<ColorSpace>,
        working: WorkingColorSpace,
    ) -> InputColorResolution {
        if let Some(color_space) = override_color_space {
            return InputColorResolution {
                resolved: ResolvedInputColor::Color(color_space),
                source: InputColorResolutionSource::Override,
                override_color_space,
                executable_color_space: executable_metadata,
                missing_metadata_policy: self,
                working_color_space: working,
            };
        }
        if let Some(color_space) = executable_metadata {
            return InputColorResolution {
                resolved: ResolvedInputColor::Color(color_space),
                source: InputColorResolutionSource::DetectedMetadata,
                override_color_space,
                executable_color_space: executable_metadata,
                missing_metadata_policy: self,
                working_color_space: working,
            };
        }
        let (resolved, source) = match self {
            Self::AssumeRec709 => (
                ResolvedInputColor::Color(ColorSpace::Rec709),
                InputColorResolutionSource::MissingPolicyAssumeRec709,
            ),
            Self::RejectMedia => (
                ResolvedInputColor::Rejected,
                InputColorResolutionSource::MissingPolicyRejectMedia,
            ),
        };
        InputColorResolution {
            resolved,
            source,
            override_color_space,
            executable_color_space: executable_metadata,
            missing_metadata_policy: self,
            working_color_space: working,
        }
    }

    /// Resolve media input color using clip overrides, asset interpretation,
    /// detected metadata, and missing-metadata policy in product precedence.
    pub fn resolve_asset_input_decision(
        self,
        clip_override_color_space: Option<ColorSpace>,
        asset_interpretation: AssetMediaInterpretation,
        executable_metadata: Option<ColorSpace>,
        working: WorkingColorSpace,
    ) -> InputColorResolution {
        if asset_interpretation.payload.is_non_color_data() {
            return InputColorResolution {
                resolved: ResolvedInputColor::Data,
                source: InputColorResolutionSource::DataTexture,
                override_color_space: None,
                executable_color_space: executable_metadata,
                missing_metadata_policy: self,
                working_color_space: working,
            };
        }
        if clip_override_color_space.is_some() {
            return self.resolve_input_decision(
                clip_override_color_space,
                executable_metadata,
                working,
            );
        }
        match asset_interpretation.color {
            MediaColorInterpretation::Auto => {
                self.resolve_input_decision(None, executable_metadata, working)
            }
            MediaColorInterpretation::Override { color_space } => {
                self.resolve_input_decision(Some(color_space), executable_metadata, working)
            }
        }
    }
}

// Re-exported from mondrian_core::timeline_data.
pub use mondrian_core::timeline_data::NestedColorProcessing;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VideoRange {
    #[default]
    Full,
    Legal,
}

impl AuthoringFootprint for VideoRange {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Full | Self::Legal => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
/// Encoded sample depth requested from the delivery codec.
///
/// Renderer working precision and the raw FFmpeg pipe representation are
/// internal contracts and are deliberately not represented here.
pub enum DeliveryBitDepth {
    /// 8-bit encoded delivery samples.
    Eight,
    /// 10-bit encoded delivery samples.
    #[default]
    Ten,
    /// 12-bit encoded delivery samples for codecs such as ProRes 4444.
    Twelve,
}

impl AuthoringFootprint for DeliveryBitDepth {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Eight | Self::Ten | Self::Twelve => Ok(()),
        }
    }
}

/// Sequence-owned color authoring.
///
/// This contains working-domain, media-input, and Program Output semantics.
/// The color engine itself is Project-owned and deliberately absent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SequenceColorSettings {
    pub working_color_space: WorkingColorSpace,
    pub input: SequenceInputColorSettings,
    pub program_output: ProgramOutputColorSettings,
}

impl AuthoringFootprint for SequenceColorSettings {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { working_color_space: _, input, program_output } = self;
        collector.collect(input)?;
        collector.collect(program_output)
    }
}

/// Default media-input policy for Timeline contributions in one Sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SequenceInputColorSettings {
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    #[serde(default)]
    pub auto_tone_map_media: bool,
}

impl AuthoringFootprint for SequenceInputColorSettings {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { missing_metadata_policy, auto_tone_map_media: _ } = self;
        collector.collect(missing_metadata_policy)
    }
}

/// Program Output semantics authored by one Sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProgramOutputColorSettings {
    pub workflow: ColorWorkflow,
    /// Program-output tone-map policy authored for this Sequence.
    pub tone_map_policy: DisplayToneMapPolicy,
    pub color_space: ColorSpace,
}

impl AuthoringFootprint for ProgramOutputColorSettings {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { workflow, tone_map_policy: _, color_space: _ } = self;
        collector.collect(workflow)
    }
}

/// Sequence defaults copied into an export target before per-export overrides.
///
/// Encoded delivery properties are intentionally separate from creative color
/// processing. They do not alter Timeline evaluation or Program Output pixels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SequenceDeliveryDefaults {
    pub video_range: VideoRange,
    /// Actual encoded sample depth of the deliverable.
    pub bit_depth: DeliveryBitDepth,
    /// Whether export omits static HDR metadata or writes the explicitly
    /// authored delivery values below.
    pub static_hdr_metadata_policy: StaticHdrMetadataPolicy,
    /// HDR mastering-display color volume (SMPTE ST 2086).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_mastering_display: Option<VideoMasteringDisplayMetadata>,
    /// HDR content light level metadata (MaxCLL / MaxFALL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_content_light: Option<VideoContentLightMetadata>,
}

impl AuthoringFootprint for SequenceDeliveryDefaults {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            video_range,
            bit_depth,
            static_hdr_metadata_policy,
            hdr_mastering_display: _,
            hdr_content_light: _,
        } = self;
        collector.collect(video_range)?;
        collector.collect(bit_depth)?;
        collector.collect(static_hdr_metadata_policy)
    }
}

/// Sequence-owned default policy for static HDR delivery metadata.
///
/// This policy never means source passthrough. Rendered output may only write
/// metadata explicitly authored for the finished sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StaticHdrMetadataPolicy {
    /// Do not write SMPTE ST 2086 or MaxCLL/MaxFALL metadata.
    #[default]
    Omit,
    /// Write the sequence's explicitly authored static HDR values.
    WriteAuthored,
}

impl AuthoringFootprint for StaticHdrMetadataPolicy {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Omit | Self::WriteAuthored => Ok(()),
        }
    }
}

impl StaticHdrMetadataPolicy {
    /// Whether the finished delivery must contain authored static HDR metadata.
    pub const fn writes_authored_metadata(self) -> bool {
        matches!(self, Self::WriteAuthored)
    }
}

/// Resolved Program Output color context for one Sequence evaluation.
///
/// This combines the Project-owned engine with Sequence-owned program
/// semantics. It deliberately excludes machine-local monitor adaptation and
/// per-media input tone mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramColorContext {
    working_color_space: WorkingColorSpace,
    output: ProgramColorOutput,
    workflow: ColorWorkflow,
    engine: ColorEngine,
    /// Sequence input interpretation retained only to derive per-media input
    /// contexts while traversing nested Timelines; it never changes Program
    /// Output pixels by itself.
    missing_metadata_policy: MissingColorMetadataPolicy,
    origin: ProgramColorContextOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProgramColorOutput {
    Encoded {
        color_space: ColorSpace,
        transform: mondrian_core::OutputTransformIntent,
        rendering_view: bool,
    },
    Working(WorkingColorSpace),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgramColorContextOrigin {
    Root,
    Nested,
}

/// Failure to construct a closed Program color context.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProgramColorContextError {
    /// The selected Project engine cannot execute the requested working space.
    #[error("working color space {working_color_space:?} is invalid for {engine}: {reason}")]
    InvalidWorkingSpace {
        /// Rejected working space.
        working_color_space: WorkingColorSpace,
        /// Selected engine name.
        engine: String,
        /// Engine validation diagnostic.
        reason: String,
    },
    /// A root Program Output must name an encoded display/delivery space.
    #[error("root Program Output {output_color_space:?} is not display-referred")]
    InvalidRootOutput {
        /// Rejected root target.
        output_color_space: ColorSpace,
    },
    /// The selected engine/intent cannot resolve the encoded target.
    #[error("cannot resolve Program Output {output_color_space:?}: {reason}")]
    InvalidOutputTransform {
        /// Rejected encoded target.
        output_color_space: ColorSpace,
        /// Intent-resolution diagnostic.
        reason: String,
    },
    /// Tone-map policy and output-transform kind disagreed.
    #[error(
        "Program Output {output_color_space:?} tone_map={tone_map} disagrees with resolved rendering_view={rendering_view}"
    )]
    OutputTransformModeMismatch {
        /// Rejected encoded target.
        output_color_space: ColorSpace,
        /// Requested tone-map behavior.
        tone_map: bool,
        /// Whether the intent resolved a rendering View.
        rendering_view: bool,
    },
    /// Delivery/display output cannot be derived from a nested working-space context.
    #[error("encoded output contexts may only be derived from a root Program context")]
    EncodedOutputFromNestedContext,
}

/// Resolved media-input color context for one Timeline media contribution.
///
/// Input tone mapping is authored on the Sequence/Timeline source plan and is
/// independent from Program Output tone mapping and monitor adaptation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaInputColorContext {
    pub working_color_space: WorkingColorSpace,
    pub input_tone_map: bool,
    pub engine: ColorEngine,
    pub missing_metadata_policy: MissingColorMetadataPolicy,
}

impl ProgramColorContext {
    fn validate_working_space(
        working_color_space: WorkingColorSpace,
        engine: &ColorEngine,
    ) -> Result<(), ProgramColorContextError> {
        engine.validate_working_space(working_color_space).map_err(|reason| {
            ProgramColorContextError::InvalidWorkingSpace {
                working_color_space,
                engine: engine.name().to_owned(),
                reason,
            }
        })
    }

    fn encoded_output(
        output_color_space: ColorSpace,
        tone_map: bool,
        output_transform: mondrian_core::OutputTransformIntent,
        engine: &ColorEngine,
    ) -> Result<ProgramColorOutput, ProgramColorContextError> {
        let rendering_view = output_transform
            .resolve_display_view(output_color_space, engine)
            .map_err(|error| ProgramColorContextError::InvalidOutputTransform {
                output_color_space,
                reason: error.to_string(),
            })?
            .is_some();
        if tone_map != rendering_view {
            return Err(ProgramColorContextError::OutputTransformModeMismatch {
                output_color_space,
                tone_map,
                rendering_view,
            });
        }
        Ok(ProgramColorOutput::Encoded {
            color_space: output_color_space,
            transform: output_transform,
            rendering_view,
        })
    }

    fn rendering_view_intent(
        output_color_space: ColorSpace,
        engine: &ColorEngine,
    ) -> mondrian_core::OutputTransformIntent {
        match engine {
            ColorEngine::MondrianStandard { package } => {
                mondrian_core::OutputTransformIntent::mondrian_standard_package(*package)
            }
            ColorEngine::Aces { preset } => {
                mondrian_core::OutputTransformIntent::aces_preset(*preset)
            }
            ColorEngine::CustomOcio { .. } => {
                mondrian_core::OutputTransformIntent::CustomOcio { output_color_space }
            }
        }
    }

    /// Working space in which Timeline pixels are composited.
    pub const fn working_color_space(&self) -> WorkingColorSpace {
        self.working_color_space
    }

    /// Output endpoint identity. Nested contexts end in their parent working space.
    pub const fn output_color_space(&self) -> OcioColorSpaceIdentity {
        match self.output {
            ProgramColorOutput::Encoded { color_space, .. } => {
                OcioColorSpaceIdentity::Color(color_space)
            }
            ProgramColorOutput::Working(color_space) => {
                OcioColorSpaceIdentity::Working(color_space)
            }
        }
    }

    /// Whether the final encoded boundary resolves a rendering View.
    pub const fn output_tone_map(&self) -> bool {
        match self.output {
            ProgramColorOutput::Encoded { rendering_view, .. } => rendering_view,
            ProgramColorOutput::Working(_) => false,
        }
    }

    /// Product-level final output transform selected for this context.
    pub fn output_transform(&self) -> &mondrian_core::OutputTransformIntent {
        match &self.output {
            ProgramColorOutput::Encoded { transform, .. } => transform,
            ProgramColorOutput::Working(_) => &mondrian_core::OutputTransformIntent::Colorimetric,
        }
    }

    /// Sequence workflow that authored this evaluation context.
    pub const fn workflow(&self) -> ColorWorkflow {
        self.workflow
    }

    /// Project-owned color engine pinned into this execution context.
    pub const fn engine(&self) -> &ColorEngine {
        &self.engine
    }

    /// Missing-metadata policy used to derive media-input contexts.
    pub const fn missing_metadata_policy(&self) -> MissingColorMetadataPolicy {
        self.missing_metadata_policy
    }

    /// Derive the source-to-working contract for one media contribution.
    pub fn media_input(&self, input_tone_map: bool) -> MediaInputColorContext {
        MediaInputColorContext {
            working_color_space: self.working_color_space,
            input_tone_map,
            engine: self.engine.clone(),
            missing_metadata_policy: self.missing_metadata_policy,
        }
    }

    /// Derive a validated delivery context from a root Program context.
    pub fn for_export_output(
        &self,
        output_color_space: ColorSpace,
        tone_map: bool,
        output_transform: mondrian_core::OutputTransformIntent,
    ) -> Result<Self, ProgramColorContextError> {
        if self.origin != ProgramColorContextOrigin::Root {
            return Err(ProgramColorContextError::EncodedOutputFromNestedContext);
        }
        let output =
            Self::encoded_output(output_color_space, tone_map, output_transform, &self.engine)?;
        Ok(Self { output, ..self.clone() })
    }

    /// Derive a validated rendering-View display context from a root context.
    pub fn for_rendering_view_output(
        &self,
        output_color_space: ColorSpace,
    ) -> Result<Self, ProgramColorContextError> {
        let intent = Self::rendering_view_intent(output_color_space, &self.engine);
        self.for_export_output(output_color_space, true, intent)
    }
}

impl Default for SequenceColorSettings {
    fn default() -> Self {
        Self {
            working_color_space: WorkingColorSpace::LinearRec2020,
            input: SequenceInputColorSettings::default(),
            program_output: ProgramOutputColorSettings::default(),
        }
    }
}

impl Default for SequenceInputColorSettings {
    fn default() -> Self {
        Self {
            missing_metadata_policy: MissingColorMetadataPolicy::AssumeRec709,
            auto_tone_map_media: true,
        }
    }
}

impl Default for ProgramOutputColorSettings {
    fn default() -> Self {
        Self {
            workflow: ColorWorkflow::SceneReferred,
            tone_map_policy: DisplayToneMapPolicy::default(),
            color_space: ColorSpace::Rec709,
        }
    }
}

impl Default for SequenceDeliveryDefaults {
    fn default() -> Self {
        Self {
            video_range: VideoRange::Full,
            bit_depth: DeliveryBitDepth::Ten,
            static_hdr_metadata_policy: StaticHdrMetadataPolicy::Omit,
            hdr_mastering_display: None,
            hdr_content_light: None,
        }
    }
}

/// 序列设置（帧率/分辨率/音频配置）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SequenceSettings {
    #[serde(default)]
    pub editing_mode: EditingMode,
    pub resolution: Resolution,
    pub frame_rate: Rational,
    #[serde(default)]
    pub pixel_aspect_ratio: PixelAspectRatio,
    #[serde(default)]
    pub field_order: FieldOrder,
    /// Sequence position presentation; never an author-time coordinate.
    pub timeline_display: TimelineDisplaySettings,
    pub audio_sample_rate: u32,
    #[serde(default)]
    pub audio_display_format: AudioDisplayFormat,
    #[serde(default)]
    pub audio_channel_layout: AudioChannelLayout,
    #[serde(default)]
    pub preview: SequencePreviewSettings,
    pub color: SequenceColorSettings,
    pub delivery: SequenceDeliveryDefaults,
    /// Action-safe margin as fraction of frame (0.10 = 10% total, 5% per side).
    #[serde(default = "default_action_safe_margin")]
    pub action_safe_margin: f32,
    /// Title-safe margin as fraction of frame (0.20 = 20% total, 10% per side).
    #[serde(default = "default_title_safe_margin")]
    pub title_safe_margin: f32,
}

impl AuthoringFootprint for SequenceSettings {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            editing_mode,
            resolution: _,
            frame_rate: _,
            pixel_aspect_ratio: _,
            field_order: _,
            timeline_display: _,
            audio_sample_rate: _,
            audio_display_format,
            audio_channel_layout: _,
            preview,
            color,
            delivery,
            action_safe_margin: _,
            title_safe_margin: _,
        } = self;
        collector.collect(editing_mode)?;
        collector.collect(audio_display_format)?;
        collector.collect(preview)?;
        collector.collect(color)?;
        collector.collect(delivery)
    }
}

fn default_action_safe_margin() -> f32 {
    0.10
}
fn default_title_safe_margin() -> f32 {
    0.20
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            editing_mode: EditingMode::Custom,
            resolution: Resolution::FHD,
            frame_rate: Rational::FPS_25,
            pixel_aspect_ratio: PixelAspectRatio::Square,
            field_order: FieldOrder::Progressive,
            timeline_display: TimelineDisplaySettings::default(),
            audio_sample_rate: 48000,
            audio_display_format: AudioDisplayFormat::AudioSamples,
            audio_channel_layout: AudioChannelLayout::Stereo,
            preview: SequencePreviewSettings::default(),
            color: SequenceColorSettings::default(),
            delivery: SequenceDeliveryDefaults::default(),
            action_safe_margin: default_action_safe_margin(),
            title_safe_margin: default_title_safe_margin(),
        }
    }
}

impl SequenceSettings {
    pub const AUDIO_SAMPLE_RATES: [u32; 5] = [32_000, 44_100, 48_000, 88_200, 96_000];
    pub const MIN_WIDTH: u32 = 16;
    pub const MIN_HEIGHT: u32 = 16;
    pub const MAX_WIDTH: u32 = 16_384;
    pub const MAX_HEIGHT: u32 = 16_384;

    /// Validate invariants that do not depend on the Project color engine.
    pub fn validate(&self) -> mondrian_core::Result<()> {
        if !Rational::SEQUENCE_FRAME_RATES.contains(&self.frame_rate) {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: format!("不支持的序列时基: {} fps", self.frame_rate),
            });
        }
        if !(Self::MIN_WIDTH..=Self::MAX_WIDTH).contains(&self.resolution.width)
            || !(Self::MIN_HEIGHT..=Self::MAX_HEIGHT).contains(&self.resolution.height)
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: format!(
                    "不支持的帧大小: {}x{}",
                    self.resolution.width, self.resolution.height
                ),
            });
        }
        if self.pixel_aspect_ratio.exact_ratio().is_none() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_owned(),
                reason: "序列像素宽高比不能是 Unknown；请选择一个可执行的精确比例".to_owned(),
            });
        }
        if self.field_order != FieldOrder::Progressive {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_owned(),
                reason: "当前版本只允许逐行 Sequence；隔行交付需要真实的场采样与编码路径"
                    .to_owned(),
            });
        }
        if !Self::AUDIO_SAMPLE_RATES.contains(&self.audio_sample_rate) {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: format!("不支持的音频采样率: {} Hz", self.audio_sample_rate),
            });
        }
        self.timeline_display.resolve(self.frame_rate).map_err(|error| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: format!("序列时间显示合同无效: {error}"),
            }
        })?;
        if !(0.125..=1.0).contains(&self.preview.resolution_scale) {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: format!("预览分辨率比例无效: {}", self.preview.resolution_scale),
            });
        }
        for (name, margin) in [
            ("action_safe_margin", self.action_safe_margin),
            ("title_safe_margin", self.title_safe_margin),
        ] {
            if !margin.is_finite() || !(0.0..1.0).contains(&margin) {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "sequence_settings_validate".to_owned(),
                    reason: format!("{name} 必须是 [0, 1) 内的有限总边距比例，当前值为 {margin}"),
                });
            }
        }
        if !self.color.program_output.color_space.is_display_referred() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "序列输出必须是显示或交付色彩空间，不能使用场景线性或 Log 输入空间"
                    .to_string(),
            });
        }
        if self.delivery.static_hdr_metadata_policy.writes_authored_metadata()
            && !self.color.program_output.color_space.is_hdr()
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "只有 HDR 输出色彩空间可以写入静态 HDR metadata".to_string(),
            });
        }
        if self.delivery.static_hdr_metadata_policy.writes_authored_metadata() {
            let mastering = self.delivery.hdr_mastering_display.as_ref().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "sequence_settings_validate".to_string(),
                    reason: "写入静态 HDR metadata 需要 SMPTE ST 2086 母版显示元数据".to_string(),
                }
            })?;
            mastering.validate().map_err(|error| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "sequence_settings_validate".to_string(),
                    reason: format!("HDR mastering metadata 无效: {error}"),
                }
            })?;
            let content_light = self.delivery.hdr_content_light.ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "sequence_settings_validate".to_string(),
                    reason: "写入静态 HDR metadata 需要 MaxCLL/MaxFALL 内容光级别元数据"
                        .to_string(),
                }
            })?;
            content_light.validate().map_err(|error| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "sequence_settings_validate".to_string(),
                    reason: format!("HDR content-light metadata 无效: {error}"),
                }
            })?;
        }
        Ok(())
    }

    /// Validate the complete Sequence contract in one Project color environment.
    pub fn validate_with_color_environment(
        &self,
        color_environment: &mondrian_core::ProjectColorEnvironment,
    ) -> mondrian_core::Result<()> {
        self.validate()?;
        color_environment
            .engine()
            .validate_working_space(self.color.working_color_space)
            .map_err(|reason| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_color_settings_validate".to_owned(),
                reason,
            })?;

        self.root_program_color_context(color_environment)
            .map(|_| ())
            .map_err(|reason| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_color_output_validate".to_owned(),
                reason: reason.to_string(),
            })
    }

    /// Build the Program Output color context shared by preview, scopes, and export.
    ///
    /// Program Output uses the sequence output color space because these pixels
    /// define the program before any local monitor adaptation.
    ///
    /// The Project engine and Sequence program semantics resolve one typed
    /// `output_transform` intent; no second display/view policy may replace it.
    pub fn root_program_color_context(
        &self,
        color_environment: &mondrian_core::ProjectColorEnvironment,
    ) -> Result<ProgramColorContext, ProgramColorContextError> {
        self.root_color_context_for_output(color_environment, self.color.program_output.color_space)
    }

    fn root_color_context_for_output(
        &self,
        color_environment: &mondrian_core::ProjectColorEnvironment,
        output_color_space: ColorSpace,
    ) -> Result<ProgramColorContext, ProgramColorContextError> {
        let engine = color_environment.engine().clone();
        ProgramColorContext::validate_working_space(self.color.working_color_space, &engine)?;
        if !output_color_space.is_display_referred() {
            return Err(ProgramColorContextError::InvalidRootOutput { output_color_space });
        }

        // Scene-referred workflows need the Project-owned engine's view
        // transform at a display-referred output. The engine alone decides
        // whether that view is Mondrian Standard, ACES, or Custom OCIO.
        let tone_map = self.color.program_output.tone_map_policy.resolve(
            self.color.program_output.workflow == ColorWorkflow::SceneReferred,
            self.color.working_color_space,
            output_color_space,
        );

        // Select one typed output intent. Standard retains its immutable package
        // identity without consulting OCIO process-global state; ACES resolves
        // its target-aware preset and Custom OCIO carries the target used to
        // resolve exactly one project-pinned output binding.
        let output_transform = if tone_map {
            ProgramColorContext::rendering_view_intent(output_color_space, &engine)
        } else {
            mondrian_core::OutputTransformIntent::Colorimetric
        };
        let output = ProgramColorContext::encoded_output(
            output_color_space,
            tone_map,
            output_transform,
            &engine,
        )?;

        Ok(ProgramColorContext {
            working_color_space: self.color.working_color_space,
            output,
            engine,
            missing_metadata_policy: self.color.input.missing_metadata_policy,
            workflow: self.color.program_output.workflow,
            origin: ProgramColorContextOrigin::Root,
        })
    }

    pub fn nested_render_color_context(
        &self,
        parent: &ProgramColorContext,
        processing: NestedColorProcessing,
    ) -> Result<ProgramColorContext, ProgramColorContextError> {
        let (working_color_space, workflow) = match processing {
            NestedColorProcessing::PreserveChildWorkingSpace => (
                self.color.working_color_space,
                self.color.program_output.workflow,
            ),
            NestedColorProcessing::ForceParentWorkingSpace => {
                (parent.working_color_space, parent.workflow)
            }
        };
        ProgramColorContext::validate_working_space(working_color_space, &parent.engine)?;
        Ok(ProgramColorContext {
            working_color_space,
            output: ProgramColorOutput::Working(parent.working_color_space),
            engine: parent.engine.clone(),
            missing_metadata_policy: self.color.input.missing_metadata_policy,
            workflow,
            origin: ProgramColorContextOrigin::Nested,
        })
    }

    pub fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.resolution = Resolution { width, height };
        self
    }

    /// Resolve the single Viewer/Timeline position-display contract.
    pub fn timeline_display_contract(&self) -> mondrian_core::Result<TimelineDisplayContract> {
        self.timeline_display.resolve(self.frame_rate).map_err(|error| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_timeline_display_resolve".to_owned(),
                reason: error.to_string(),
            }
        })
    }

    pub fn from_editing_mode(mode: EditingMode) -> Self {
        let mut settings = Self { editing_mode: mode, ..Default::default() };
        match mode {
            EditingMode::Custom => settings,
            EditingMode::Dslr1080p => {
                settings.resolution = Resolution::FHD;
                settings.frame_rate = Rational::FPS_23976;
                settings.timeline_display.format = TimelineDisplayFormat::Frames;
                settings
            }
            EditingMode::Dslr720p => {
                settings.resolution = Resolution::HD;
                settings.frame_rate = Rational::FPS_5994;
                settings.timeline_display.format = TimelineDisplayFormat::Frames;
                settings
            }
            EditingMode::Avchd1080p => {
                settings.resolution = Resolution::FHD;
                settings.frame_rate = Rational::FPS_2997;
                settings.timeline_display.format =
                    TimelineDisplayFormat::Timecode(SmpteCountingMode::DropFrame);
                settings
            }
            EditingMode::DigitalCinema4k => {
                settings.resolution = Resolution::DCI4K;
                settings.frame_rate = Rational::FPS_24;
                settings
            }
            EditingMode::SocialVertical1080p => {
                settings.resolution = Resolution { width: 1080, height: 1920 };
                settings.frame_rate = Rational::FPS_30;
                settings.timeline_display.format = TimelineDisplayFormat::Frames;
                settings
            }
        }
    }

    pub fn apply_editing_mode_preset(&mut self, mode: EditingMode) {
        let audio_sample_rate = self.audio_sample_rate;
        let audio_display_format = self.audio_display_format;
        let audio_channel_layout = self.audio_channel_layout;
        let start_timecode_frame = self.timeline_display.timecode_start_frame;
        let preview = self.preview.clone();
        let color = self.color.clone();
        let delivery = self.delivery.clone();
        *self = Self::from_editing_mode(mode);
        self.audio_sample_rate = audio_sample_rate;
        self.audio_display_format = audio_display_format;
        self.audio_channel_layout = audio_channel_layout;
        self.timeline_display.timecode_start_frame = start_timecode_frame;
        self.preview = preview;
        self.color = color;
        self.delivery = delivery;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SequencePreset {
    pub name: String,
    pub settings: SequenceSettings,
}

impl AuthoringFootprint for SequencePreset {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { name, settings } = self;
        collector.collect(name)?;
        collector.collect(settings)
    }
}

impl SequencePreset {
    pub fn new(name: impl Into<String>, settings: SequenceSettings) -> mondrian_core::Result<Self> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_preset_new".to_string(),
                reason: "序列预设名称不能为空".to_string(),
            });
        }
        settings.validate()?;
        Ok(Self { name, settings })
    }
}

/// Mondrian 时间线序列
/// Typed Track placement facts for one located Clip identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipTrackLocation {
    /// Owning Track identity.
    pub track_id: TrackId,
    /// Whether the owning Track is a video Track.
    pub is_video_track: bool,
    /// Owning Track index within its kind.
    pub track_index: usize,
    /// Whether the owning Track is locked.
    pub is_locked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    pub id: SequenceId,
    /// Monotonic authoring transaction revision for this stable Sequence ID.
    pub revision: SequenceRevision,
    pub name: String,
    pub role: SequenceRole,
    pub settings: SequenceSettings,
    pub video_tracks: AuthoringList<Track>,
    /// Explicit two-input visual Transitions. Endpoint Track membership is
    /// derived from their strong Clip references.
    pub video_transitions: AuthoringList<crate::video_transition::VideoTransition>,
    /// Sequence-owned shared grades. Clip, group, and timeline scopes only
    /// retain strong typed references into this catalog.
    #[serde(default)]
    pub grade_definitions: AuthoringList<mondrian_core::GradeDefinition>,
    /// Ordered group catalog. Each Clip may reference at most one group.
    #[serde(default)]
    pub grade_groups: AuthoringList<crate::grade::GradeGroup>,
    /// Optional full-composite grade evaluated once after track compositing.
    #[serde(default)]
    pub timeline_grade: Option<mondrian_core::GradeDefinitionId>,
    pub audio_tracks: AuthoringList<Track>,
    /// Sequence semantic catalog for audio classification and output projection.
    pub audio_roles: AuthoringList<crate::audio::AudioRole>,
    /// Sequence-owned audio processing, routing, transitions, and public outputs.
    pub audio_program: crate::audio::AudioProgram,
    pub playhead: TimelineTime,
    pub in_point: Option<TimelineTime>,
    pub out_point: Option<TimelineTime>,
}

impl AuthoringFootprint for Sequence {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            revision: _,
            name,
            role,
            settings,
            video_tracks,
            video_transitions,
            grade_definitions,
            grade_groups,
            timeline_grade: _,
            audio_tracks,
            audio_roles,
            audio_program,
            playhead: _,
            in_point: _,
            out_point: _,
        } = self;
        collector.collect(name)?;
        collector.collect(role)?;
        collector.collect(settings)?;
        collector.collect(video_tracks)?;
        collector.collect(video_transitions)?;
        collector.collect(grade_definitions)?;
        collector.collect(grade_groups)?;
        collector.collect(audio_tracks)?;
        collector.collect(audio_roles)?;
        collector.collect(audio_program)
    }
}

const SEQUENCE_AUTHOR_CONTRACT_CERTIFICATE_VERSION: u32 = 1;

/// Opaque process-local evidence that one exact Sequence snapshot satisfied
/// the complete author contract in one exact Project color environment.
///
/// This certificate is neither persisted nor an alternate author model. It
/// retains the validated immutable baseline solely so a later replacement can
/// prove its current-state and context preconditions before reusing unchanged
/// local-validation evidence.
#[derive(Debug)]
pub struct SequenceAuthorContractCertificate {
    version: u32,
    baseline: AuthoringSnapshot<Sequence>,
    color_environment: ProjectColorEnvironment,
}

impl SequenceAuthorContractCertificate {
    /// Prove that `current` is the exact validated baseline owned by this
    /// certificate in the supplied Project color environment.
    pub fn validate_baseline(
        &self,
        current: &Sequence,
        color_environment: &ProjectColorEnvironment,
    ) -> mondrian_core::Result<()> {
        if self.version != SEQUENCE_AUTHOR_CONTRACT_CERTIFICATE_VERSION {
            return Err(sequence_certificate_error(
                "Sequence author-contract certificate version is unsupported",
            ));
        }
        if self.color_environment != *color_environment {
            return Err(sequence_certificate_error(
                "Sequence author-contract certificate belongs to a different Project color environment",
            ));
        }
        if self.baseline.revision != current.revision {
            return Err(sequence_certificate_error(format!(
                "Sequence {} identity/revision no longer matches the certified baseline",
                current.id
            )));
        }
        if self.baseline.value() != current {
            return Err(sequence_certificate_error(format!(
                "Sequence {} author state no longer matches the certified baseline",
                current.id
            )));
        }
        Ok(())
    }

    /// Prepare validation evidence for one monotonic replacement.
    ///
    /// Identity/link, Transition, and audio-program invariants are always
    /// revalidated across the complete candidate. Only local Track, Clip,
    /// Effect, and Mask validators whose exact subtrees remain unchanged may
    /// reuse the baseline evidence.
    pub fn prepare_replacement(
        &self,
        current: &Sequence,
        candidate: &Sequence,
        color_environment: &ProjectColorEnvironment,
    ) -> mondrian_core::Result<Self> {
        self.validate_baseline(current, color_environment)?;
        if candidate.id != current.id {
            return Err(sequence_certificate_error(format!(
                "Sequence replacement identity changed from {} to {}",
                current.id, candidate.id
            )));
        }
        if candidate.revision <= current.revision {
            return Err(sequence_certificate_error(format!(
                "Sequence {} replacement revision {} must be newer than certified revision {}",
                candidate.id,
                candidate.revision.get(),
                current.revision.get()
            )));
        }

        candidate.validate_author_contract_global(color_environment)?;
        candidate.validate_changed_local_author_contracts(current)?;
        Ok(Self::new(candidate, color_environment))
    }

    fn new(sequence: &Sequence, color_environment: &ProjectColorEnvironment) -> Self {
        Self {
            version: SEQUENCE_AUTHOR_CONTRACT_CERTIFICATE_VERSION,
            baseline: AuthoringSnapshot::new(sequence.clone()),
            color_environment: color_environment.clone(),
        }
    }
}

impl Sequence {
    /// Validate one Sequence's complete local author contract.
    ///
    /// Collection-wide nested-reference closure is intentionally separate.
    /// Project open/edit validation and selected-range execution admission use
    /// this same local seam so neither duplicates Track/Clip schema rules.
    pub fn validate_author_contract(
        &self,
        color_environment: &ProjectColorEnvironment,
    ) -> mondrian_core::Result<()> {
        self.validate_author_contract_global(color_environment)?;
        self.validate_all_local_author_contracts()
    }

    /// Fully validate this Sequence and retain opaque process-local evidence
    /// for later copy-on-write replacement validation.
    pub fn prepare_author_contract_certificate(
        &self,
        color_environment: &ProjectColorEnvironment,
    ) -> mondrian_core::Result<SequenceAuthorContractCertificate> {
        self.validate_author_contract(color_environment)?;
        Ok(SequenceAuthorContractCertificate::new(
            self,
            color_environment,
        ))
    }

    fn validate_author_contract_global(
        &self,
        color_environment: &ProjectColorEnvironment,
    ) -> mondrian_core::Result<()> {
        if self.revision.get() == 0 {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "validate_sequence_author_contract".to_owned(),
                reason: format!("Sequence '{}' has invalid author revision zero", self.name),
            });
        }
        self.settings.validate_with_color_environment(color_environment)?;
        self.validate_grade_hierarchy()?;
        for (tracks, expected_type, role) in [
            (&self.video_tracks, TrackType::Video, "video"),
            (&self.audio_tracks, TrackType::Audio, "audio"),
        ] {
            if let Some(track) = tracks.iter().find(|track| track.track_type != expected_type) {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "validate_sequence_author_contract".to_owned(),
                    reason: format!(
                        "Sequence {} {role} Track {} declares incompatible type {:?}",
                        self.id, track.id, track.track_type
                    ),
                });
            }
        }
        record_sequence_identity_validation();
        self.validate_author_identities()?;
        record_sequence_audio_validation();
        self.audio_program
            .validate(
                &self.audio_tracks,
                &self.audio_roles,
                self.settings.audio_channel_layout,
            )
            .map_err(|error| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "validate_audio_program".to_owned(),
                reason: format!("Sequence {} has invalid audio authoring: {error}", self.id),
            })?;
        Ok(())
    }

    fn validate_all_local_author_contracts(&self) -> mondrian_core::Result<()> {
        for track in self.video_tracks.iter().chain(&self.audio_tracks) {
            validate_track_local_author_contract(track, None)?;
        }
        Ok(())
    }

    fn validate_changed_local_author_contracts(&self, current: &Self) -> mondrian_core::Result<()> {
        validate_changed_track_collection(&current.video_tracks, &self.video_tracks)?;
        validate_changed_track_collection(&current.audio_tracks, &self.audio_tracks)
    }

    /// Compare complete authored state while deliberately ignoring only the
    /// monotonic transaction revision.
    ///
    /// Allocation identities are execution evidence, so structurally equal
    /// COW containers compare equal even when they no longer share storage.
    pub fn author_state_eq_ignoring_revision(&self, other: &Self) -> bool {
        let Self {
            id,
            revision: _,
            name,
            role,
            settings,
            video_tracks,
            video_transitions,
            grade_definitions,
            grade_groups,
            timeline_grade,
            audio_tracks,
            audio_roles,
            audio_program,
            playhead,
            in_point,
            out_point,
        } = self;
        let Self {
            id: other_id,
            revision: _,
            name: other_name,
            role: other_role,
            settings: other_settings,
            video_tracks: other_video_tracks,
            video_transitions: other_video_transitions,
            grade_definitions: other_grade_definitions,
            grade_groups: other_grade_groups,
            timeline_grade: other_timeline_grade,
            audio_tracks: other_audio_tracks,
            audio_roles: other_audio_roles,
            audio_program: other_audio_program,
            playhead: other_playhead,
            in_point: other_in_point,
            out_point: other_out_point,
        } = other;

        id == other_id
            && name == other_name
            && role == other_role
            && settings == other_settings
            && video_tracks == other_video_tracks
            && video_transitions == other_video_transitions
            && grade_definitions == other_grade_definitions
            && grade_groups == other_grade_groups
            && timeline_grade == other_timeline_grade
            && audio_tracks == other_audio_tracks
            && audio_roles == other_audio_roles
            && audio_program == other_audio_program
            && playhead == other_playhead
            && in_point == other_in_point
            && out_point == other_out_point
    }

    pub fn new(name: impl Into<String>) -> Self {
        let settings = SequenceSettings::default();
        let audio_tracks = AuthoringList::from(vec![
            Track::new_audio("A1"),
            Track::new_audio("A2"),
            Track::new_audio("A3"),
        ]);
        let audio_program =
            crate::audio::AudioProgram::for_tracks(audio_tracks.iter().map(|track| track.id));
        Self {
            id: SequenceId::new(),
            revision: SequenceRevision::INITIAL,
            name: name.into(),
            role: SequenceRole::Editorial,
            video_tracks: AuthoringList::from(vec![
                Track::new_video("V1"),
                Track::new_video("V2"),
                Track::new_video("V3"),
            ]),
            video_transitions: AuthoringList::new(),
            grade_definitions: AuthoringList::new(),
            grade_groups: AuthoringList::new(),
            timeline_grade: None,
            audio_tracks,
            audio_roles: AuthoringList::new(),
            audio_program,
            playhead: TimelineTime::ZERO,
            in_point: None,
            out_point: None,
            settings,
        }
    }

    pub fn time_base(&self) -> Rational {
        Rational::new(self.settings.frame_rate.den, self.settings.frame_rate.num)
    }

    pub fn with_settings(
        name: impl Into<String>,
        settings: SequenceSettings,
    ) -> mondrian_core::Result<Self> {
        settings.validate()?;
        let mut sequence = Self::new(name);
        sequence.settings = settings;
        Ok(sequence)
    }

    pub fn apply_settings(&mut self, settings: SequenceSettings) -> mondrian_core::Result<()> {
        settings.validate()?;
        self.settings = settings;
        Ok(())
    }

    pub fn in_point(&self) -> TimelineTime {
        self.in_point.unwrap_or(TimelineTime::ZERO).max(TimelineTime::ZERO)
    }

    pub fn out_point(&self) -> Option<TimelineTime> {
        self.out_point
            .map(|time| time.max(TimelineTime::ZERO))
            .filter(|time| *time >= self.in_point())
    }

    pub fn mark_in(&mut self, time: TimelineTime) {
        let time = time.max(TimelineTime::ZERO);
        self.in_point = Some(time);
        if self.out_point.is_some_and(|out| out < time) {
            self.out_point = Some(time);
        }
    }

    pub fn mark_out(&mut self, time: TimelineTime) {
        self.out_point = Some(time.max(self.in_point()));
    }

    pub fn clear_in_out(&mut self) {
        self.in_point = None;
        self.out_point = None;
    }

    pub fn total_duration(&self) -> mondrian_core::Result<TimelineTime> {
        let mut end = TimelineTime::ZERO;

        for track in self.video_tracks.iter().chain(self.audio_tracks.iter()) {
            if let Some(last) = track.clips.last() {
                end = end.max(last.end_position()?);
            }
        }
        Ok(end)
    }

    pub fn active_clips_at(&self, time: TimelineTime) -> mondrian_core::Result<Vec<ActiveClip>> {
        let mut result = Vec::new();

        for (i, track) in self.video_tracks.iter().enumerate() {
            if !track.is_visible || track.is_muted {
                continue;
            }

            let track_opacity = track.evaluate_opacity(time).clamp(0.0, 1.0);
            for clip in track.active_clips_at(time)? {
                let clip_time = clip.timeline_to_clip_time(time)?;
                let source_sample = clip.timeline_to_source_sample(time)?;
                let transform_mat = clip.transform.evaluate_matrix(clip_time);
                let opacity =
                    (clip.transform.evaluate_opacity(clip_time) * track_opacity).clamp(0.0, 1.0);
                result.push(ActiveClip {
                    clip: clip.clone(),
                    track_index: i,
                    clip_time,
                    source_sample,
                    transform_matrix: transform_mat,
                    opacity,
                    blend_mode: clip.blend_mode.unwrap_or(track.blend_mode),
                });
            }
        }
        Ok(result)
    }

    pub fn snap_points(&self) -> mondrian_core::Result<Vec<TimelineTime>> {
        let mut pts = Vec::new();
        for track in self.video_tracks.iter().chain(self.audio_tracks.iter()) {
            pts.extend(track.snap_points()?);
        }
        pts.push(self.playhead);
        pts.push(TimelineTime::ZERO);
        pts.sort_unstable();
        pts.dedup();
        Ok(pts)
    }

    pub fn video_track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.video_tracks.iter_mut().find(|track| track.id == id)
    }

    /// Locate one Clip by identity across every video Track first, then every
    /// audio Track, in canonical Track order.
    pub fn find_clip(&self, clip_id: ClipId) -> Option<&crate::clip::Clip> {
        self.video_tracks
            .iter()
            .chain(&self.audio_tracks)
            .find_map(|track| track.clips.iter().find(|clip| clip.id == clip_id))
    }

    /// Mutable counterpart of [`Self::find_clip`].
    pub fn find_clip_mut(&mut self, clip_id: ClipId) -> Option<&mut crate::clip::Clip> {
        self.video_tracks
            .iter_mut()
            .chain(&mut self.audio_tracks)
            .find_map(|track| track.clips.iter_mut().find(|clip| clip.id == clip_id))
    }

    /// Typed Track placement facts for one Clip identity.
    pub fn clip_track_location(&self, clip_id: ClipId) -> Option<ClipTrackLocation> {
        for (index, track) in self.video_tracks.iter().enumerate() {
            if track.clips.iter().any(|clip| clip.id == clip_id) {
                return Some(ClipTrackLocation {
                    track_id: track.id,
                    is_video_track: true,
                    track_index: index,
                    is_locked: track.is_locked,
                });
            }
        }
        for (index, track) in self.audio_tracks.iter().enumerate() {
            if track.clips.iter().any(|clip| clip.id == clip_id) {
                return Some(ClipTrackLocation {
                    track_id: track.id,
                    is_video_track: false,
                    track_index: index,
                    is_locked: track.is_locked,
                });
            }
        }
        None
    }

    /// Remove one Clip from whichever Track owns it.
    pub fn remove_clip_anywhere(&mut self, clip_id: ClipId) -> Option<crate::clip::Clip> {
        for track in &mut self.video_tracks {
            if let Some(clip) = track.remove_clip(clip_id) {
                return Some(clip);
            }
        }
        for track in &mut self.audio_tracks {
            if let Some(clip) = track.remove_clip(clip_id) {
                return Some(clip);
            }
        }
        None
    }

    /// Set one Clip's disabled presentation flag. Returns whether state changed.
    pub fn set_clip_disabled(&mut self, clip_id: ClipId, disabled: bool) -> bool {
        let Some(clip) = self.find_clip_mut(clip_id) else {
            return false;
        };
        if clip.is_disabled == disabled {
            return false;
        }
        clip.is_disabled = disabled;
        true
    }

    /// Set one Clip's exact placement position. Returns whether state changed.
    ///
    /// The caller owns any evaluation-grid lowering; this method stores the
    /// exact author time it is given.
    pub fn set_clip_position(&mut self, clip_id: ClipId, position: TimelineTime) -> bool {
        let Some(clip) = self.find_clip_mut(clip_id) else {
            return false;
        };
        if clip.position == position {
            return false;
        }
        clip.position = position;
        true
    }

    /// Move one Clip to a target Track index at an exact position.
    ///
    /// Same-Track moves update the position in place. Cross-Track moves detach
    /// the Clip and push it onto the target Track; the caller owns subsequent
    /// conflict resolution and structural compaction.
    pub fn move_clip_to_track_at_time(
        &mut self,
        is_video_track: bool,
        clip_id: ClipId,
        target_track_index: usize,
        target_position: TimelineTime,
    ) -> bool {
        let Some(location) = self.clip_track_location(clip_id) else {
            return false;
        };
        if location.is_video_track != is_video_track {
            return false;
        }
        let tracks = if is_video_track {
            &mut self.video_tracks
        } else {
            &mut self.audio_tracks
        };
        if target_track_index >= tracks.len() {
            return false;
        }
        let source_track_index = location.track_index;
        if source_track_index == target_track_index {
            let Some(clip) =
                tracks[source_track_index].clips.iter_mut().find(|clip| clip.id == clip_id)
            else {
                return false;
            };
            clip.position = target_position;
            return true;
        }
        let Some(clip_index) =
            tracks[source_track_index].clips.iter().position(|clip| clip.id == clip_id)
        else {
            return false;
        };
        let mut clip = tracks[source_track_index].clips.remove(clip_index);
        clip.position = target_position;
        tracks[target_track_index].clips.push(clip);
        true
    }

    /// Grow audio Tracks until `index` is a valid audio Track index.
    pub fn ensure_audio_track_index(&mut self, index: usize) {
        while self.audio_tracks.len() <= index {
            self.add_audio_track();
        }
    }

    pub fn audio_track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.audio_tracks.iter_mut().find(|track| track.id == id)
    }

    pub fn add_video_track(&mut self) -> TrackId {
        let track = Track::new_video("");
        let id = track.id;
        self.video_tracks.push(track);
        self.normalize_track_names();
        id
    }

    pub fn add_audio_track(&mut self) -> TrackId {
        let track = Track::new_audio("");
        let id = track.id;
        self.audio_tracks.push(track);
        self.audio_program.add_track(id);
        self.normalize_track_names();
        id
    }

    /// Add one media Clip to an audio Track with explicit default audio authoring.
    ///
    /// This is the canonical mutation seam for new audio placements. Callers
    /// cannot create a playable audio Clip without also registering its
    /// non-placement processing scope.
    pub fn add_media_audio_clip(
        &mut self,
        track_id: TrackId,
        mut clip: crate::clip::Clip,
        component_id: AudioSourceComponentId,
    ) -> mondrian_core::Result<ClipId> {
        self.attach_default_media_audio_component(&mut clip, component_id)?;
        let clip_id = clip.id;
        self.audio_track_mut(track_id)
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?
            .add_clip(clip)?;
        Ok(clip_id)
    }

    /// Attach one default media Component and register its processing Scope.
    ///
    /// This prepares a fully authored audio Clip for structural operations,
    /// such as Insert Edit, that must place it only after validating a larger
    /// atomic Track scope. It deliberately does not choose or mutate a Track.
    pub fn attach_default_media_audio_component(
        &mut self,
        clip: &mut crate::clip::Clip,
        component_id: AudioSourceComponentId,
    ) -> mondrian_core::Result<()> {
        if clip.is_nested_sequence() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "attach_default_media_audio_component".to_owned(),
                reason: "nested Sequence audio requires an explicit output binding".to_owned(),
            });
        }
        if !clip.audio_components.is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "attach_default_media_audio_component".to_owned(),
                reason: "audio Clip already contains audio component authoring".to_owned(),
            });
        }
        let scope = crate::audio::AudioProcessingScope::identity();
        clip.audio_components.push(crate::audio::AudioComponentEdit::media(
            component_id,
            scope.id,
        ));
        self.audio_program.add_processing_scope(scope);
        Ok(())
    }

    /// Add one nested-Sequence public output as an audio placement.
    pub fn add_nested_audio_clip(
        &mut self,
        track_id: TrackId,
        mut clip: crate::clip::Clip,
        output_id: ProgramOutputId,
    ) -> mondrian_core::Result<ClipId> {
        if !clip.is_nested_sequence() || !clip.audio_components.is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "add_nested_audio_clip".to_owned(),
                reason: "nested audio placement requires a clean nested Sequence Clip".to_owned(),
            });
        }
        let scope = crate::audio::AudioProcessingScope::identity();
        clip.audio_components.push(crate::audio::AudioComponentEdit::nested(
            output_id, scope.id,
        ));
        let clip_id = clip.id;
        self.audio_program.add_processing_scope(scope);
        self.audio_track_mut(track_id)
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?
            .add_clip(clip)?;
        Ok(clip_id)
    }

    /// Fork copied/moved audio authoring into this Sequence aggregate.
    ///
    /// Scope sharing within the Clip is preserved, but all aggregate-local
    /// entity identities are fresh. The returned mapping allows callers to
    /// recreate Transitions only when both strong endpoints were imported.
    pub fn fork_audio_clip_authoring(
        &mut self,
        clip: &mut crate::clip::Clip,
        source_scopes: &[crate::audio::AudioProcessingScope],
    ) -> mondrian_core::Result<HashMap<AudioComponentEditId, AudioComponentEditId>> {
        let mut scope_ids = HashMap::<AudioProcessingScopeId, AudioProcessingScopeId>::new();
        let mut edit_ids = HashMap::new();
        for edit in &mut clip.audio_components {
            let new_scope_id = if let Some(id) = scope_ids.get(&edit.processing.scope_id) {
                *id
            } else {
                let mut scope = source_scopes
                    .iter()
                    .find(|scope| scope.id == edit.processing.scope_id)
                    .cloned()
                    .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                        step_id: "fork_audio_clip_authoring".to_owned(),
                        reason: format!(
                            "audio processing scope {} is unavailable",
                            edit.processing.scope_id
                        ),
                    })?;
                let old_id = scope.id;
                scope.id = AudioProcessingScopeId::new();
                rekey_audio_scope(&mut scope);
                let new_id = scope.id;
                self.audio_program.add_processing_scope(scope);
                scope_ids.insert(old_id, new_id);
                new_id
            };
            let old_edit_id = edit.id;
            edit.id = AudioComponentEditId::new();
            edit.processing.scope_id = new_scope_id;
            rekey_optional_exact_curve(&mut edit.volume_automation);
            rekey_optional_exact_curve(&mut edit.pan_automation);
            edit_ids.insert(old_edit_id, edit.id);
        }
        Ok(edit_ids)
    }

    /// Fork every Sequence-local audio identity after duplicating a Sequence.
    ///
    /// A duplicated Sequence is an independent author aggregate: later edits,
    /// processor state, automation, routing, and public-output bindings must not
    /// alias the source Sequence. References to a child Sequence's public output
    /// are deliberately not rewritten because they cross this aggregate boundary.
    fn fork_audio_identities_for_sequence_duplicate(&mut self) {
        use crate::audio::{AudioRouteDestination, AudioRouteSource, ProgramOutputMainSource};

        let old_processor_ids = audio_processor_ids(&self.audio_program);

        let role_ids = self
            .audio_roles
            .iter()
            .map(|role| (role.id, AudioRoleId::new()))
            .collect::<HashMap<_, _>>();
        for role in &mut self.audio_roles {
            role.id = role_ids[&role.id];
            role.parent_id = role.parent_id.map(|id| role_ids[&id]);
        }

        let scope_ids = self
            .audio_program
            .processing_scopes
            .iter()
            .map(|scope| (scope.id, AudioProcessingScopeId::new()))
            .collect::<HashMap<_, _>>();
        for scope in &mut self.audio_program.processing_scopes {
            scope.id = scope_ids[&scope.id];
            rekey_audio_scope(scope);
        }

        let mut edit_ids = HashMap::new();
        for clip in self.audio_tracks.iter_mut().flat_map(|track| &mut track.clips) {
            for edit in &mut clip.audio_components {
                let old_id = edit.id;
                edit.id = AudioComponentEditId::new();
                edit.processing.scope_id = scope_ids[&edit.processing.scope_id];
                edit.role_id = edit.role_id.map(|id| role_ids[&id]);
                rekey_optional_exact_curve(&mut edit.volume_automation);
                rekey_optional_exact_curve(&mut edit.pan_automation);
                edit_ids.insert(old_id, edit.id);
            }
        }

        let bus_ids = self
            .audio_program
            .buses
            .iter()
            .map(|bus| (bus.id, MixBusId::new()))
            .collect::<HashMap<_, _>>();
        for bus in &mut self.audio_program.buses {
            bus.id = bus_ids[&bus.id];
            rekey_audio_channel_strip(&mut bus.strip);
        }

        let output_ids = self
            .audio_program
            .outputs
            .iter()
            .map(|output| (output.id, ProgramOutputId::new()))
            .collect::<HashMap<_, _>>();
        for output in &mut self.audio_program.outputs {
            output.id = output_ids[&output.id];
            if let ProgramOutputMainSource::SemanticProjection { role_id } = &mut output.main_source
            {
                *role_id = role_ids[role_id];
            }
            rekey_audio_channel_strip(&mut output.strip);
        }

        for channel in self.audio_program.track_channels.values_mut() {
            rekey_audio_channel_strip(&mut channel.strip);
        }
        let processor_ids = old_processor_ids
            .into_iter()
            .zip(audio_processor_ids(&self.audio_program))
            .collect::<HashMap<_, _>>();
        for route in &mut self.audio_program.routes {
            route.id = AudioRouteId::new();
            rekey_optional_exact_curve(&mut route.gain_automation);
            if let AudioRouteSource::Bus { bus_id, .. } = &mut route.source {
                *bus_id = bus_ids[bus_id];
            }
            match &mut route.destination {
                AudioRouteDestination::Bus(bus_id) => *bus_id = bus_ids[bus_id],
                AudioRouteDestination::Output(output_id) => {
                    *output_id = output_ids[output_id];
                }
            }
        }
        for route in &mut self.audio_program.sidechain_routes {
            route.id = AudioRouteId::new();
            route.processor_id = processor_ids[&route.processor_id];
            rekey_optional_exact_curve(&mut route.gain_automation);
            if let AudioRouteSource::Bus { bus_id, .. } = &mut route.source {
                *bus_id = bus_ids[bus_id];
            }
        }
        for transition in &mut self.audio_program.transitions {
            transition.id = AudioTransitionId::new();
            transition.left = edit_ids[&transition.left];
            transition.right = edit_ids[&transition.right];
        }
    }

    /// Rekey Sequence-local Clip link groups after duplicating a Sequence.
    pub fn fork_clip_link_groups_for_sequence_duplicate(&mut self) {
        let mut groups = HashMap::<ClipLinkGroupId, ClipLinkGroupId>::new();
        for clip in self
            .video_tracks
            .iter_mut()
            .chain(&mut self.audio_tracks)
            .flat_map(|track| &mut track.clips)
        {
            if let Some(group) = clip.link_group {
                clip.link_group = Some(*groups.entry(group).or_default());
            }
        }
    }

    /// Fork every Sequence-owned grade identity while preserving internal
    /// shared-definition and hierarchy references.
    fn fork_grade_hierarchy_for_sequence_duplicate(&mut self) {
        let definition_ids = self
            .grade_definitions
            .iter()
            .map(|definition| (definition.id, mondrian_core::GradeDefinitionId::new()))
            .collect::<HashMap<_, _>>();
        let group_ids = self
            .grade_groups
            .iter()
            .map(|group| (group.id, mondrian_core::GradeGroupId::new()))
            .collect::<HashMap<_, _>>();

        for definition in &mut self.grade_definitions {
            definition.id = definition_ids[&definition.id];
            let old_active_version = definition.active_version;
            let version_ids = definition
                .versions
                .iter()
                .map(|version| (version.id, mondrian_core::GradeVersionId::new()))
                .collect::<HashMap<_, _>>();
            for version in &mut definition.versions {
                version.id = version_ids[&version.id];
                let node_ids = version
                    .graph
                    .nodes
                    .iter()
                    .map(|node| (node.id, mondrian_core::GradeGraphNodeId::new()))
                    .collect::<HashMap<_, _>>();
                version.graph.output = node_ids[&version.graph.output];
                for node in &mut version.graph.nodes {
                    node.id = node_ids[&node.id];
                    match &mut node.kind {
                        mondrian_core::GradeGraphNodeKind::Input => {}
                        mondrian_core::GradeGraphNodeKind::Effect { input, effect } => {
                            *input = node_ids[input];
                            effect.id = EffectId::new();
                            effect.properties.fork_author_identities();
                        }
                        mondrian_core::GradeGraphNodeKind::Parallel { inputs, .. } => {
                            for input in inputs {
                                *input = node_ids[input];
                            }
                        }
                        mondrian_core::GradeGraphNodeKind::Layer { base, overlay, .. } => {
                            *base = node_ids[base];
                            *overlay = node_ids[overlay];
                        }
                    }
                }
            }
            definition.active_version = version_ids[&old_active_version];
        }
        for group in &mut self.grade_groups {
            group.id = group_ids[&group.id];
            group.pre_clip_grade = group.pre_clip_grade.map(|id| definition_ids[&id]);
            group.post_clip_grade = group.post_clip_grade.map(|id| definition_ids[&id]);
        }
        self.timeline_grade = self.timeline_grade.map(|id| definition_ids[&id]);
        for clip in self
            .video_tracks
            .iter_mut()
            .chain(&mut self.audio_tracks)
            .flat_map(|track| &mut track.clips)
        {
            clip.grade = clip.grade.map(|id| definition_ids[&id]);
            clip.grade_group = clip.grade_group.map(|id| group_ids[&id]);
        }
    }

    /// Fork the identity graph of a cloned Sequence into an independent author
    /// aggregate while preserving its authored values and external references.
    pub fn fork_author_identities_for_sequence_duplicate(&mut self) {
        use crate::audio::AudioRouteSource;

        self.id = SequenceId::new();
        self.revision = SequenceRevision::INITIAL;

        let mut track_ids = HashMap::<TrackId, TrackId>::new();
        for track in self.video_tracks.iter_mut().chain(&mut self.audio_tracks) {
            let old_id = track.id;
            track.id = TrackId::new();
            track.opacity.fork_author_identities();
            track_ids.insert(old_id, track.id);
        }

        let mut clip_ids = HashMap::<ClipId, ClipId>::new();
        for clip in self
            .video_tracks
            .iter_mut()
            .chain(&mut self.audio_tracks)
            .flat_map(|track| &mut track.clips)
        {
            let old_id = clip.id;
            clip.fork_visual_placement_identities();
            clip_ids.insert(old_id, clip.id);
        }

        let old_channels = std::mem::take(&mut self.audio_program.track_channels);
        self.audio_program.track_channels = old_channels
            .into_iter()
            .map(|(track_id, channel)| (track_ids[&track_id], channel))
            .collect();
        for route in &mut self.audio_program.routes {
            if let AudioRouteSource::Track { track_id, .. } = &mut route.source {
                *track_id = track_ids[track_id];
            }
        }
        for route in &mut self.audio_program.sidechain_routes {
            if let AudioRouteSource::Track { track_id, .. } = &mut route.source {
                *track_id = track_ids[track_id];
            }
        }

        for transition in &mut self.video_transitions {
            transition.left = clip_ids[&transition.left];
            transition.right = clip_ids[&transition.right];
            transition.fork_author_identities();
        }

        self.fork_audio_identities_for_sequence_duplicate();
        self.fork_clip_link_groups_for_sequence_duplicate();
        self.fork_grade_hierarchy_for_sequence_duplicate();
    }

    /// Remove meaningless singleton link groups after structural edits.
    pub fn compact_clip_link_groups(&mut self) {
        let mut counts = HashMap::<ClipLinkGroupId, usize>::new();
        for clip in self
            .video_tracks
            .iter()
            .chain(&self.audio_tracks)
            .flat_map(|track| &track.clips)
        {
            if let Some(group) = clip.link_group {
                *counts.entry(group).or_default() += 1;
            }
        }
        let singleton_groups = counts
            .into_iter()
            .filter_map(|(group, count)| (count == 1).then_some(group))
            .collect::<HashSet<_>>();
        if singleton_groups.is_empty() {
            return;
        }

        let video_track_indices = self
            .video_tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| {
                track
                    .clips
                    .iter()
                    .any(|clip| {
                        clip.link_group.is_some_and(|group| singleton_groups.contains(&group))
                    })
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let audio_track_indices = self
            .audio_tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| {
                track
                    .clips
                    .iter()
                    .any(|clip| {
                        clip.link_group.is_some_and(|group| singleton_groups.contains(&group))
                    })
                    .then_some(index)
            })
            .collect::<Vec<_>>();

        for track_index in video_track_indices {
            for clip in &mut self.video_tracks[track_index].clips {
                if clip.link_group.is_some_and(|group| singleton_groups.contains(&group)) {
                    clip.link_group = None;
                }
            }
        }
        for track_index in audio_track_indices {
            for clip in &mut self.audio_tracks[track_index].clips {
                if clip.link_group.is_some_and(|group| singleton_groups.contains(&group)) {
                    clip.link_group = None;
                }
            }
        }
    }

    /// Remove visual Transitions whose strong endpoints or edit geometry no
    /// longer exist after a structural edit.
    pub fn compact_video_transitions(&mut self) {
        let video_tracks = &self.video_tracks;
        if self
            .video_transitions
            .iter()
            .all(|transition| validate_video_transition(video_tracks, transition).is_ok())
        {
            return;
        }
        self.video_transitions
            .retain(|transition| validate_video_transition(video_tracks, transition).is_ok());
    }

    /// Restore every derived author-graph invariant after a structural edit.
    pub fn compact_structural_references(&mut self) {
        self.compact_clip_link_groups();
        self.compact_video_transitions();
        self.compact_audio_program();
    }

    /// Replace clips by identity and restore the author-graph invariants.
    ///
    /// This is the canonical apply step for a bulk trim prepared by the
    /// timeline edit algorithms: callers hand over the validated replacement
    /// clips and the Sequence owns re-sorted placement plus structural
    /// compaction, so no caller may sort `Track::clips` or compact references
    /// itself.
    pub fn apply_bulk_trim(
        &mut self,
        updates: impl IntoIterator<Item = Clip>,
    ) -> mondrian_core::Result<()> {
        for updated in updates {
            let clip_id = updated.id;
            let slot = self
                .video_tracks
                .iter_mut()
                .chain(&mut self.audio_tracks)
                .find_map(|track| track.clips.iter_mut().find(|clip| clip.id == clip_id))
                .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                    clip_id: clip_id.to_string(),
                })?;
            *slot = updated;
        }
        for track in self.video_tracks.iter_mut().chain(&mut self.audio_tracks) {
            track.clips.sort_by_key(|clip| clip.position);
        }
        self.compact_structural_references();
        Ok(())
    }

    /// Drop unreferenced processing definitions and invalidated Transition references.
    pub fn compact_audio_program(&mut self) {
        self.audio_program.compact_for_tracks(&self.audio_tracks);
    }

    pub fn remove_video_track(&mut self, id: TrackId) -> mondrian_core::Result<()> {
        if self.video_tracks.len() <= 1 {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_video_track".to_string(),
                reason: "至少保留 1 条视频轨道".to_string(),
            });
        }

        if let Some(index) = self.video_tracks.iter().position(|track| track.id == id) {
            self.video_tracks.remove(index);
            self.compact_structural_references();
            self.normalize_track_names();
            Ok(())
        } else {
            Err(mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })
        }
    }

    pub fn remove_audio_track(&mut self, id: TrackId) -> mondrian_core::Result<()> {
        if self.audio_tracks.len() <= 1 {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_audio_track".to_string(),
                reason: "至少保留 1 条音频轨道".to_string(),
            });
        }

        if let Some(index) = self.audio_tracks.iter().position(|track| track.id == id) {
            self.audio_tracks.remove(index);
            self.audio_program.remove_track(id);
            self.compact_structural_references();
            self.normalize_track_names();
            Ok(())
        } else {
            Err(mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })
        }
    }

    /// Return whether a stable-identity relative Track move would change author order.
    pub fn track_relative_placement_would_change(
        &self,
        id: TrackId,
        placement: crate::TrackRelativePlacement,
    ) -> mondrian_core::Result<bool> {
        let anchor_id = track_placement_anchor(placement);
        if anchor_id == id {
            return Err(track_order_error(
                "a Track cannot be ordered relative to itself",
            ));
        }

        if self.video_tracks.iter().any(|track| track.id == id) {
            ensure_track_anchor_kind(&self.video_tracks, &self.audio_tracks, anchor_id)?;
            return track_relative_placement_would_change_in_list(
                &self.video_tracks,
                id,
                placement,
            );
        }
        if self.audio_tracks.iter().any(|track| track.id == id) {
            ensure_track_anchor_kind(&self.audio_tracks, &self.video_tracks, anchor_id)?;
            return track_relative_placement_would_change_in_list(
                &self.audio_tracks,
                id,
                placement,
            );
        }
        Err(mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })
    }

    /// Move one Track immediately before or after another same-kind Track.
    ///
    /// Returns `false` without mutation when the requested relation already
    /// holds. Missing, cross-kind, or self-referential identities fail closed.
    pub fn reorder_track_relative(
        &mut self,
        id: TrackId,
        placement: crate::TrackRelativePlacement,
    ) -> mondrian_core::Result<bool> {
        if !self.track_relative_placement_would_change(id, placement)? {
            return Ok(false);
        }
        let changed = if self.video_tracks.iter().any(|track| track.id == id) {
            reorder_track_relative_in_list(&mut self.video_tracks, id, placement)?
        } else if self.audio_tracks.iter().any(|track| track.id == id) {
            reorder_track_relative_in_list(&mut self.audio_tracks, id, placement)?
        } else {
            return Err(mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() });
        };
        if changed {
            self.normalize_track_names();
        }
        Ok(changed)
    }

    pub fn normalize_track_names(&mut self) {
        renumber_tracks(&mut self.video_tracks, "V");
        renumber_tracks(&mut self.audio_tracks, "A");
    }

    /// Validate stable author identities and strong Clip links inside this Sequence.
    pub fn validate_author_identities(&self) -> mondrian_core::Result<()> {
        let mut track_ids = HashSet::new();
        let mut clip_ids = HashSet::new();
        let mut effect_ids = HashSet::new();
        let mut mask_ids = HashSet::new();

        for track in self.video_tracks.iter().chain(&self.audio_tracks) {
            if !track_ids.insert(track.id) {
                return Err(duplicate_author_identity("Track", track.id));
            }
            for clip in &track.clips {
                if !clip_ids.insert(clip.id) {
                    return Err(duplicate_author_identity("Clip", clip.id));
                }
                for effect in &clip.effects {
                    if !effect_ids.insert(effect.id) {
                        return Err(duplicate_author_identity("Effect", effect.id));
                    }
                }
                for mask in &clip.masks {
                    if !mask_ids.insert(mask.id) {
                        return Err(duplicate_author_identity("Mask", mask.id));
                    }
                }
            }
        }

        for definition in &self.grade_definitions {
            for version in &definition.versions {
                for node in &version.graph.nodes {
                    if let mondrian_core::GradeGraphNodeKind::Effect { effect, .. } = &node.kind
                        && !effect_ids.insert(effect.id)
                    {
                        return Err(duplicate_author_identity("Effect", effect.id));
                    }
                }
            }
        }

        let mut link_group_counts = HashMap::<ClipLinkGroupId, usize>::new();
        for clip in self
            .video_tracks
            .iter()
            .chain(&self.audio_tracks)
            .flat_map(|track| &track.clips)
        {
            if let Some(group) = clip.link_group {
                *link_group_counts.entry(group).or_default() += 1;
            }
        }
        if let Some((group, _)) = link_group_counts.iter().find(|(_, count)| **count < 2) {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "validate_author_identities".to_owned(),
                reason: format!("Clip link group {group} has fewer than two members"),
            });
        }

        let mut transition_ids = HashSet::new();
        let mut transition_endpoints = HashSet::new();
        let mut transition_ranges = Vec::with_capacity(self.video_transitions.len());
        for transition in &self.video_transitions {
            if !transition_ids.insert(transition.id) {
                return Err(duplicate_author_identity("VideoTransition", transition.id));
            }
            if !transition_endpoints.insert((transition.left, transition.right)) {
                return Err(crate::video_transition::invalid_transition(
                    transition.id,
                    "the edit already owns a visual Transition",
                ));
            }
            validate_video_transition(&self.video_tracks, transition)?;
            let track_index = self
                .video_tracks
                .iter()
                .position(|track| track.clips.iter().any(|clip| clip.id == transition.left))
                .ok_or_else(|| {
                    crate::video_transition::invalid_transition(
                        transition.id,
                        "left endpoint Track disappeared during validation",
                    )
                })?;
            transition_ranges.push((
                track_index,
                transition.sequence_range.start,
                transition.sequence_range.end()?,
                transition.id,
            ));
        }
        transition_ranges.sort_unstable_by_key(|(track, start, _, _)| (*track, *start));
        for pair in transition_ranges.windows(2) {
            let (left_track, _, left_end, _) = pair[0];
            let (right_track, right_start, _, right_id) = pair[1];
            if left_track == right_track && right_start < left_end {
                return Err(crate::video_transition::invalid_transition(
                    right_id,
                    "Transition ranges on one Track must not overlap",
                ));
            }
        }
        Ok(())
    }
}

fn validate_changed_track_collection(
    current: &AuthoringList<Track>,
    candidate: &AuthoringList<Track>,
) -> mondrian_core::Result<()> {
    if current.shares_allocation_with(candidate) {
        return Ok(());
    }
    for track in candidate {
        let previous = current.iter().find(|current| current.id == track.id);
        if previous == Some(track) {
            continue;
        }
        validate_track_local_author_contract(track, previous)?;
    }
    Ok(())
}

fn validate_track_local_author_contract(
    track: &Track,
    previous: Option<&Track>,
) -> mondrian_core::Result<()> {
    record_track_local_validation();
    track.validate_author_state()?;
    if previous.is_none_or(|previous| previous.opacity != track.opacity) {
        track.opacity.validate().map_err(|error| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "validate_sequence_author_contract".to_owned(),
                reason: format!("Track {} opacity is invalid: {error}", track.id),
            }
        })?;
    }
    if previous.is_some_and(|previous| previous.clips.shares_allocation_with(&track.clips)) {
        return Ok(());
    }
    for clip in &track.clips {
        let previous_clip =
            previous.and_then(|previous| previous.clips.iter().find(|item| item.id == clip.id));
        if previous_clip == Some(clip) {
            continue;
        }
        validate_clip_local_author_contract(clip, previous_clip)?;
    }
    Ok(())
}

fn validate_clip_local_author_contract(
    clip: &crate::clip::Clip,
    previous: Option<&crate::clip::Clip>,
) -> mondrian_core::Result<()> {
    record_clip_local_validation();
    clip.validate_time_state()?;
    if let Some(title) = clip.content.basic_title() {
        title.validate_author_state()?;
    }
    if let Some(interpretation) = clip.content.media_interpretation() {
        if let Some(editorial_source) = &interpretation.editorial_source {
            editorial_source.validate().map_err(|error| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "validate_sequence_author_contract".to_owned(),
                    reason: format!(
                        "Clip {} editorial source identity is invalid: {error}",
                        clip.id
                    ),
                }
            })?;
        }
        if interpretation
            .pixel_aspect_ratio_override
            .is_some_and(|ratio| ratio.exact_ratio().is_none())
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "validate_sequence_author_contract".to_owned(),
                reason: format!(
                    "Clip {} has an Unknown pixel-aspect override; use Auto or an exact ratio",
                    clip.id
                ),
            });
        }
        if interpretation
            .field_order_override
            .is_some_and(|order| order != FieldOrder::Progressive)
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "validate_sequence_author_contract".to_owned(),
                reason: format!(
                    "Clip {} requests interlaced interpretation without an admitted deinterlacing path",
                    clip.id
                ),
            });
        }
    }
    clip.transform.to_property_bag().validate().map_err(|error| {
        mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "validate_sequence_author_contract".to_owned(),
            reason: format!("Clip {} transform schema is invalid: {error}", clip.id),
        }
    })?;

    if !previous.is_some_and(|previous| previous.effects.shares_allocation_with(&clip.effects)) {
        for effect in &clip.effects {
            let previous_effect = previous
                .and_then(|previous| previous.effects.iter().find(|item| item.id == effect.id));
            if previous_effect != Some(effect) {
                record_effect_local_validation();
                effect.validate_author_state()?;
            }
        }
    }
    if !previous.is_some_and(|previous| previous.masks.shares_allocation_with(&clip.masks)) {
        for mask in &clip.masks {
            let previous_mask =
                previous.and_then(|previous| previous.masks.iter().find(|item| item.id == mask.id));
            if previous_mask != Some(mask) {
                record_mask_local_validation();
                mask.validate_author_state()?;
            }
        }
    }
    Ok(())
}

fn sequence_certificate_error(reason: impl Into<String>) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "validate_sequence_author_contract_certificate".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SequenceAuthorContractValidationVisits {
    identity_passes: usize,
    audio_passes: usize,
    tracks: usize,
    clips: usize,
    effects: usize,
    masks: usize,
}

#[cfg(test)]
thread_local! {
    static SEQUENCE_AUTHOR_CONTRACT_VALIDATION_VISITS:
        std::cell::Cell<SequenceAuthorContractValidationVisits> =
        const { std::cell::Cell::new(SequenceAuthorContractValidationVisits {
            identity_passes: 0,
            audio_passes: 0,
            tracks: 0,
            clips: 0,
            effects: 0,
            masks: 0,
        }) };
}

#[cfg(test)]
fn update_sequence_author_contract_validation_visits(
    update: impl FnOnce(&mut SequenceAuthorContractValidationVisits),
) {
    SEQUENCE_AUTHOR_CONTRACT_VALIDATION_VISITS.with(|visits| {
        let mut value = visits.get();
        update(&mut value);
        visits.set(value);
    });
}

#[cfg(test)]
fn reset_sequence_author_contract_validation_visits() {
    SEQUENCE_AUTHOR_CONTRACT_VALIDATION_VISITS.with(|visits| {
        visits.set(SequenceAuthorContractValidationVisits::default());
    });
}

#[cfg(test)]
fn sequence_author_contract_validation_visits() -> SequenceAuthorContractValidationVisits {
    SEQUENCE_AUTHOR_CONTRACT_VALIDATION_VISITS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn record_sequence_identity_validation() {
    update_sequence_author_contract_validation_visits(|visits| visits.identity_passes += 1);
}

#[cfg(not(test))]
fn record_sequence_identity_validation() {}

#[cfg(test)]
fn record_sequence_audio_validation() {
    update_sequence_author_contract_validation_visits(|visits| visits.audio_passes += 1);
}

#[cfg(not(test))]
fn record_sequence_audio_validation() {}

#[cfg(test)]
fn record_track_local_validation() {
    update_sequence_author_contract_validation_visits(|visits| visits.tracks += 1);
}

#[cfg(not(test))]
fn record_track_local_validation() {}

#[cfg(test)]
fn record_clip_local_validation() {
    update_sequence_author_contract_validation_visits(|visits| visits.clips += 1);
}

#[cfg(not(test))]
fn record_clip_local_validation() {}

#[cfg(test)]
fn record_effect_local_validation() {
    update_sequence_author_contract_validation_visits(|visits| visits.effects += 1);
}

#[cfg(not(test))]
fn record_effect_local_validation() {}

#[cfg(test)]
fn record_mask_local_validation() {
    update_sequence_author_contract_validation_visits(|visits| visits.masks += 1);
}

#[cfg(not(test))]
fn record_mask_local_validation() {}

pub(crate) fn validate_video_transition(
    video_tracks: &[Track],
    transition: &crate::video_transition::VideoTransition,
) -> mondrian_core::Result<()> {
    transition.validate_definition_state()?;
    let Some((track, left_index, right_index)) = video_tracks.iter().find_map(|track| {
        let left = track.clips.iter().position(|clip| clip.id == transition.left)?;
        let right = track.clips.iter().position(|clip| clip.id == transition.right)?;
        Some((track, left, right))
    }) else {
        return Err(crate::video_transition::invalid_transition(
            transition.id,
            "endpoints must exist on the same video Track",
        ));
    };
    if left_index.checked_add(1) != Some(right_index) {
        return Err(crate::video_transition::invalid_transition(
            transition.id,
            "endpoints must be an ordered adjacent edit",
        ));
    }
    let left = &track.clips[left_index];
    let right = &track.clips[right_index];
    if left.is_adjustment_layer() || right.is_adjustment_layer() {
        return Err(crate::video_transition::invalid_transition(
            transition.id,
            "adjustment layers cannot be Transition endpoints",
        ));
    }
    let cut = left.end_position()?;
    if cut != right.position {
        return Err(crate::video_transition::invalid_transition(
            transition.id,
            "endpoints must share one exact editorial cut",
        ));
    }
    let range_end = transition.sequence_range.end()?;
    if transition.sequence_range.start < left.position
        || range_end > right.end_position()?
        || transition.sequence_range.start > cut
        || range_end < cut
    {
        return Err(crate::video_transition::invalid_transition(
            transition.id,
            "range must cover the cut and remain inside the endpoint placements",
        ));
    }
    Ok(())
}

fn duplicate_author_identity(
    kind: &str,
    identity: impl std::fmt::Display,
) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "validate_author_identities".to_owned(),
        reason: format!("duplicate {kind} identity {identity}"),
    }
}

fn rekey_audio_scope(scope: &mut crate::audio::AudioProcessingScope) {
    rekey_optional_exact_curve(&mut scope.input_gain_automation);
    for processor in &mut scope.processors.processors {
        processor.id = AudioProcessorInstanceId::new();
        for parameter in processor.parameters.values_mut() {
            rekey_exact_curve(&mut parameter.automation);
        }
    }
}

fn rekey_audio_channel_strip(strip: &mut crate::audio::AudioChannelStrip) {
    rekey_optional_exact_curve(&mut strip.fader_automation);
    for rack in [&mut strip.pre_fader, &mut strip.post_fader] {
        for processor in &mut rack.processors {
            processor.id = AudioProcessorInstanceId::new();
            for parameter in processor.parameters.values_mut() {
                rekey_exact_curve(&mut parameter.automation);
            }
        }
    }
}

fn audio_processor_ids(program: &crate::audio::AudioProgram) -> Vec<AudioProcessorInstanceId> {
    program
        .processing_scopes
        .iter()
        .map(|scope| &scope.processors)
        .chain(
            program
                .track_channels
                .values()
                .flat_map(|channel| [&channel.strip.pre_fader, &channel.strip.post_fader]),
        )
        .chain(
            program
                .buses
                .iter()
                .flat_map(|bus| [&bus.strip.pre_fader, &bus.strip.post_fader]),
        )
        .chain(
            program
                .outputs
                .iter()
                .flat_map(|output| [&output.strip.pre_fader, &output.strip.post_fader]),
        )
        .flat_map(|rack| rack.processors.iter().map(|processor| processor.id))
        .collect()
}

fn rekey_optional_exact_curve(curve: &mut Option<mondrian_core::ExactAutomationCurve>) {
    if let Some(curve) = curve {
        rekey_exact_curve(curve);
    }
}

fn rekey_exact_curve(curve: &mut mondrian_core::ExactAutomationCurve) {
    for keyframe in &mut curve.keyframes {
        keyframe.id = KeyframeId::new();
    }
}

impl mondrian_core::timeline_data::RenderPlanSource for Sequence {
    fn flat_visual_items_at(
        &self,
        time: TimelineTime,
    ) -> mondrian_core::Result<Vec<mondrian_core::timeline_data::FlatVisualItem>> {
        use mondrian_core::timeline_data::FlatVisualItem;

        let mut items = Vec::new();
        for (track_index, track) in self.video_tracks.iter().enumerate() {
            if !track.is_visible || track.is_muted {
                continue;
            }
            let track_opacity = track.evaluate_opacity(time).clamp(0.0, 1.0);
            let mut active_transitions = self.video_transitions.iter().filter(|transition| {
                transition.is_enabled
                    && time >= transition.sequence_range.start
                    && transition.sequence_range.end().is_ok_and(|end| time < end)
                    && track.clips.iter().any(|clip| clip.id == transition.left)
            });
            let active_transition = active_transitions.next();
            if let Some(conflict) = active_transitions.next() {
                return Err(crate::video_transition::invalid_transition(
                    conflict.id,
                    "multiple visual Transitions are active on one Track",
                ));
            }
            let mut replaced_endpoints = None;
            if let Some(transition) = active_transition {
                let left = track.clips.iter().find(|clip| clip.id == transition.left).ok_or_else(
                    || {
                        crate::video_transition::invalid_transition(
                            transition.id,
                            "left endpoint disappeared during evaluation",
                        )
                    },
                )?;
                let right =
                    track.clips.iter().find(|clip| clip.id == transition.right).ok_or_else(
                        || {
                            crate::video_transition::invalid_transition(
                                transition.id,
                                "right endpoint disappeared during evaluation",
                            )
                        },
                    )?;
                items.push(flatten_visual_transition(
                    transition,
                    left,
                    right,
                    track,
                    track_index,
                    track_opacity,
                    time,
                )?);
                replaced_endpoints = Some((transition.left, transition.right));
            }

            for clip in track.active_clips_at(time)? {
                if replaced_endpoints
                    .is_some_and(|(left, right)| clip.id == left || clip.id == right)
                {
                    continue;
                }
                items.push(FlatVisualItem::Clip(flatten_visual_clip(
                    clip,
                    track,
                    track_index,
                    track_opacity,
                    time,
                )?));
            }
        }
        Ok(items)
    }

    fn source_sequence_id(&self) -> SequenceId {
        self.id
    }

    fn source_sequence_revision(&self) -> SequenceRevision {
        self.revision
    }

    fn source_time_base(&self) -> mondrian_core::types::Rational {
        Sequence::time_base(self)
    }

    fn source_working_color_space(&self) -> mondrian_core::WorkingColorSpace {
        self.settings.color.working_color_space
    }

    fn auto_tone_map_media(&self) -> bool {
        self.settings.color.input.auto_tone_map_media
    }
}

pub(crate) fn flatten_visual_clip(
    clip: &crate::clip::Clip,
    track: &Track,
    track_index: usize,
    track_opacity: f32,
    time: TimelineTime,
) -> mondrian_core::Result<mondrian_core::timeline_data::FlatActiveClip> {
    flatten_visual_clip_with_effect_snapshots(
        clip,
        track,
        track_index,
        track_opacity,
        time,
        Arc::from(clip.effects.as_slice()),
        Arc::from(clip.masks.as_slice()),
    )
}

pub(crate) fn flatten_visual_clip_with_effect_snapshots(
    clip: &crate::clip::Clip,
    track: &Track,
    track_index: usize,
    track_opacity: f32,
    time: TimelineTime,
    effects: Arc<[mondrian_core::effect_data::EffectNode]>,
    masks: Arc<[mondrian_core::mask_data::MaskComponent]>,
) -> mondrian_core::Result<mondrian_core::timeline_data::FlatActiveClip> {
    let clip_time = clip.timeline_to_clip_time(time)?;
    let matrix = clip.transform.evaluate_matrix(clip_time);
    Ok(mondrian_core::timeline_data::FlatActiveClip {
        clip_id: clip.id,
        content: clip.content.clone(),
        is_disabled: clip.is_disabled,
        effects,
        masks,
        clip_time,
        source_sample: clip.timeline_to_source_sample(time)?,
        transform_matrix: [
            matrix.x_axis.x,
            matrix.x_axis.y,
            matrix.z_axis.x,
            matrix.y_axis.x,
            matrix.y_axis.y,
            matrix.z_axis.y,
        ],
        opacity: (clip.transform.evaluate_opacity(clip_time) * track_opacity).clamp(0.0, 1.0),
        blend_mode: clip.blend_mode.unwrap_or(track.blend_mode),
        track_index,
    })
}

pub(crate) fn flatten_visual_transition(
    transition: &crate::video_transition::VideoTransition,
    left: &crate::clip::Clip,
    right: &crate::clip::Clip,
    track: &Track,
    track_index: usize,
    track_opacity: f32,
    time: TimelineTime,
) -> mondrian_core::Result<mondrian_core::timeline_data::FlatVisualItem> {
    flatten_visual_transition_with_effect_snapshots(
        transition.id,
        transition.sequence_range,
        flat_video_transition_definition_snapshot(transition),
        left,
        right,
        track,
        track_index,
        track_opacity,
        time,
        Arc::from(left.effects.as_slice()),
        Arc::from(left.masks.as_slice()),
        Arc::from(right.effects.as_slice()),
        Arc::from(right.masks.as_slice()),
    )
}

pub(crate) fn flat_video_transition_definition_snapshot(
    transition: &crate::video_transition::VideoTransition,
) -> Arc<mondrian_core::timeline_data::FlatVideoTransitionDefinitionSnapshot> {
    use mondrian_core::timeline_data::{
        FlatVideoTransitionDefinition, FlatVideoTransitionDefinitionSnapshot,
    };

    let definition = match &transition.transition_type {
        crate::video_transition::VideoTransitionType::CrossDissolve => {
            FlatVideoTransitionDefinition::CrossDissolve
        }
        crate::video_transition::VideoTransitionType::Plugin { definition_id } => {
            FlatVideoTransitionDefinition::Plugin { definition_id: definition_id.clone() }
        }
    };
    Arc::new(FlatVideoTransitionDefinitionSnapshot {
        definition,
        properties: transition.properties.clone(),
        params: transition.params.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn flatten_visual_transition_with_effect_snapshots(
    transition_id: mondrian_core::VideoTransitionId,
    sequence_range: mondrian_core::TimelineTimeRange,
    definition: Arc<mondrian_core::timeline_data::FlatVideoTransitionDefinitionSnapshot>,
    left: &crate::clip::Clip,
    right: &crate::clip::Clip,
    track: &Track,
    track_index: usize,
    track_opacity: f32,
    time: TimelineTime,
    left_effects: Arc<[mondrian_core::effect_data::EffectNode]>,
    left_masks: Arc<[mondrian_core::mask_data::MaskComponent]>,
    right_effects: Arc<[mondrian_core::effect_data::EffectNode]>,
    right_masks: Arc<[mondrian_core::mask_data::MaskComponent]>,
) -> mondrian_core::Result<mondrian_core::timeline_data::FlatVisualItem> {
    use mondrian_core::timeline_data::{
        FlatTransitionProgress, FlatVideoTransition, FlatVisualItem,
    };

    Ok(FlatVisualItem::Transition(Box::new(FlatVideoTransition {
        transition_id,
        definition,
        left: flatten_visual_clip_with_effect_snapshots(
            left,
            track,
            track_index,
            track_opacity,
            time,
            left_effects,
            left_masks,
        )?,
        right: flatten_visual_clip_with_effect_snapshots(
            right,
            track,
            track_index,
            track_opacity,
            time,
            right_effects,
            right_masks,
        )?,
        progress: FlatTransitionProgress {
            elapsed: time.checked_sub(sequence_range.start)?,
            duration: sequence_range.duration,
        },
    })))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceCollection {
    pub sequences: AuthoringList<Sequence>,
    pub default_sequence_id: SequenceId,
    pub active_sequence_id: SequenceId,
}

impl AuthoringFootprint for SequenceCollection {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            sequences,
            default_sequence_id: _,
            active_sequence_id: _,
        } = self;
        collector.collect(sequences)
    }
}

impl SequenceCollection {
    pub fn new(default_sequence: Sequence) -> Self {
        let id = default_sequence.id;
        Self {
            sequences: AuthoringList::from(vec![default_sequence]),
            default_sequence_id: id,
            active_sequence_id: id,
        }
    }

    pub fn active(&self) -> Option<&Sequence> {
        self.sequence(self.active_sequence_id)
    }

    pub fn active_mut(&mut self) -> Option<&mut Sequence> {
        self.sequence_mut(self.active_sequence_id)
    }

    pub fn sequence(&self, id: SequenceId) -> Option<&Sequence> {
        self.sequences.iter().find(|sequence| sequence.id == id)
    }

    pub fn sequence_mut(&mut self, id: SequenceId) -> Option<&mut Sequence> {
        self.sequences.iter_mut().find(|sequence| sequence.id == id)
    }

    pub fn add_sequence(&mut self, sequence: Sequence) -> mondrian_core::Result<SequenceId> {
        if self.sequence(sequence.id).is_some() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "add_sequence".to_string(),
                reason: format!("序列 ID 已存在: {}", sequence.id),
            });
        }
        let id = sequence.id;
        self.sequences.push(sequence);
        Ok(id)
    }

    pub fn set_active(&mut self, id: SequenceId) -> mondrian_core::Result<()> {
        if self.sequence(id).is_none() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_active_sequence".to_string(),
                reason: format!("序列不存在: {id}"),
            });
        }
        self.active_sequence_id = id;
        Ok(())
    }

    /// Validate collection identities and the cross-Sequence dependency closure.
    ///
    /// Each Sequence's local author contract is owned by the caller so a
    /// complete Project validation does not traverse the same body twice.
    pub fn validate_dependency_closure(&self) -> mondrian_core::Result<()> {
        crate::SequenceDependencyCertificate::build(self).map(|_| ())
    }

    /// Validate a prospective replacement's collection-wide dependency closure.
    ///
    /// The collection must already be canonical and the caller must validate
    /// the replacement's local author contract exactly once. Collection
    /// This general stateless Interface first derives the canonical collection
    /// index, then evaluates the replacement through the same incremental
    /// contract used by an `AuthoringSession`. Session hot paths retain that
    /// index and therefore extract only the replacement body.
    pub fn validate_dependency_closure_with_replacement(
        &self,
        replacement: &Sequence,
    ) -> mondrian_core::Result<()> {
        let certificate = crate::SequenceDependencyCertificate::build(self)?;
        certificate.prepare_replacement(self, replacement).map(|_| ())
    }
}

fn track_placement_anchor(placement: crate::TrackRelativePlacement) -> TrackId {
    match placement {
        crate::TrackRelativePlacement::Before(anchor_id)
        | crate::TrackRelativePlacement::After(anchor_id) => anchor_id,
    }
}

fn ensure_track_anchor_kind(
    source_kind: &[Track],
    other_kind: &[Track],
    anchor_id: TrackId,
) -> mondrian_core::Result<()> {
    if source_kind.iter().any(|track| track.id == anchor_id) {
        return Ok(());
    }
    if other_kind.iter().any(|track| track.id == anchor_id) {
        return Err(track_order_error(
            "moving Track and anchor must have the same media kind",
        ));
    }
    Err(mondrian_core::MondrianError::TrackNotFound { track_id: anchor_id.to_string() })
}

fn track_relative_placement_would_change_in_list(
    tracks: &[Track],
    id: TrackId,
    placement: crate::TrackRelativePlacement,
) -> mondrian_core::Result<bool> {
    let source = track_index(tracks, id)?;
    let anchor = track_index(tracks, track_placement_anchor(placement))?;
    Ok(match placement {
        crate::TrackRelativePlacement::Before(_) => source.checked_add(1) != Some(anchor),
        crate::TrackRelativePlacement::After(_) => anchor.checked_add(1) != Some(source),
    })
}

fn reorder_track_relative_in_list(
    tracks: &mut Vec<Track>,
    id: TrackId,
    placement: crate::TrackRelativePlacement,
) -> mondrian_core::Result<bool> {
    if !track_relative_placement_would_change_in_list(tracks, id, placement)? {
        return Ok(false);
    }
    let source = track_index(tracks, id)?;
    let track = tracks.remove(source);
    let anchor = track_index(tracks, track_placement_anchor(placement))?;
    let target = match placement {
        crate::TrackRelativePlacement::Before(_) => anchor,
        crate::TrackRelativePlacement::After(_) => anchor
            .checked_add(1)
            .ok_or_else(|| track_order_error("Track insertion index overflowed author order"))?,
    };
    tracks.insert(target, track);
    Ok(true)
}

fn track_index(tracks: &[Track], id: TrackId) -> mondrian_core::Result<usize> {
    tracks
        .iter()
        .position(|track| track.id == id)
        .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })
}

fn track_order_error(reason: impl Into<String>) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "reorder_track_relative".to_owned(),
        reason: reason.into(),
    }
}

fn renumber_tracks(tracks: &mut [Track], prefix: &str) {
    for (index, track) in tracks.iter_mut().enumerate() {
        track.name = format!("{prefix}{}", index + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{
        AudioChannelStrip, AudioComponentChannelMapping, AudioComponentEdit, AudioMixBus,
        AudioProcessingScope, AudioProcessorDefinitionRef, AudioProcessorInstance,
        AudioProcessorSidechainRoute, AudioRole, AudioRouteSource, AudioTransition,
        AudioTransitionCurve, BUILTIN_GAIN_DEFINITION_ID,
    };
    use crate::clip::Clip;
    use mondrian_core::automation::{Keyframe, PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::effect_data::{EffectNode, EffectType};
    use mondrian_core::mask_data::{BezierPoint, MaskComponent, MaskEvaluation, MaskShape};
    use mondrian_core::{
        AudioChannelMixEntry, AudioChannelMixMatrix, AudioSourceComponentId,
        AuthoringFootprintCollector, AuthoringSnapshot, DisplayToneMapPolicy, MixBusId,
        TimelineTimeRange,
    };
    use mondrian_effects::EffectNodeExt;

    fn pinned_custom_engine(source: OcioConfigSource) -> ColorEngine {
        ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    source,
                    "0".repeat(64),
                    "0".repeat(64),
                    "Linear Rec.2020".to_owned(),
                    vec![mondrian_core::CustomOcioOutputIdentity::from_pinned_parts(
                        ColorSpace::Rec709,
                        "Test Display".to_owned(),
                        "Test View".to_owned(),
                        "Test Display Color Space".to_owned(),
                        mondrian_core::CustomOcioLookIdentity::None,
                    )
                    .expect("valid Custom OCIO output binding")],
                    Vec::new(),
                    Vec::new(),
                )
                .expect("structurally valid Custom OCIO test identity"),
            ),
        }
    }

    fn color_environment(engine: ColorEngine) -> mondrian_core::ProjectColorEnvironment {
        mondrian_core::ProjectColorEnvironment::new(engine)
    }

    fn standard_environment() -> mondrian_core::ProjectColorEnvironment {
        mondrian_core::ProjectColorEnvironment::default()
    }

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("valid test time")
    }

    fn footprint<T: AuthoringFootprint>(
        root: &AuthoringSnapshot<T>,
    ) -> mondrian_core::AuthoringFootprintManifest {
        let mut collector = AuthoringFootprintCollector::new();
        collector.collect(root).expect("collect authoring footprint");
        collector.finish()
    }

    fn populated_authoring_collection() -> SequenceCollection {
        let mut sequence = Sequence::new("shared authoring");
        let time_base = sequence.time_base();
        let scope = AudioProcessingScope::identity();
        let scope_id = scope.id;
        sequence.audio_program.add_processing_scope(scope);
        sequence.audio_program.buses.push(AudioMixBus {
            id: MixBusId::new(),
            name: "Stem".to_owned(),
            strip: AudioChannelStrip::default(),
        });
        sequence.audio_roles.push(AudioRole {
            id: AudioRoleId::new(),
            parent_id: None,
            name: "Dialogue".to_owned(),
            standard_semantic_key: Some("dialogue".to_owned()),
        });

        let mut left =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("left Clip");
        left.effects.push(EffectNode::new(EffectType::GaussianBlur));
        left.masks.push(MaskComponent::new(
            "Isolation Mask".to_owned(),
            MaskEvaluation::default(),
        ));
        left.audio_components.push(AudioComponentEdit::media(
            AudioSourceComponentId::primary(),
            scope_id,
        ));
        let left_id = left.id;
        let left_audio_id = left.audio_components[0].id;

        let mut right =
            Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right Clip");
        right.audio_components.push(AudioComponentEdit::media(
            AudioSourceComponentId::primary(),
            scope_id,
        ));
        let right_id = right.id;
        let right_audio_id = right.audio_components[0].id;
        sequence.video_tracks[0].add_clip(left).expect("place left Clip");
        sequence.video_tracks[0].add_clip(right).expect("place right Clip");
        sequence
            .video_transitions
            .push(crate::video_transition::VideoTransition::cross_dissolve(
                left_id,
                right_id,
                TimelineTimeRange::new(tt(8, time_base), tt(4, time_base))
                    .expect("valid visual Transition range"),
            ));
        sequence.audio_program.transitions.push(AudioTransition {
            id: AudioTransitionId::new(),
            left: left_audio_id,
            right: right_audio_id,
            sequence_range: TimelineTimeRange::new(tt(8, time_base), tt(4, time_base))
                .expect("valid audio Transition range"),
            curve: AudioTransitionCurve::EqualPower,
        });
        SequenceCollection::new(sequence)
    }

    fn sequence_with_local_validation_children() -> Sequence {
        let mut sequence = Sequence::new("certificate");
        let time_base = sequence.time_base();
        let mut clip =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("valid Clip");
        let mut effect = EffectNode::new(EffectType::GaussianBlur);
        effect
            .apply_property_mutation(PropertyMutation::DefineProperty(
                mondrian_core::automation::PropertyDescriptor::new(
                    "effect.gaussian_blur.radius",
                    "Radius",
                    PropertyValue::Float(8.0),
                ),
            ))
            .expect("define effect property");
        clip.effects.push(effect);
        clip.masks.push(MaskComponent::new(
            "Mask".to_owned(),
            MaskEvaluation::default(),
        ));
        sequence.video_tracks[0].add_clip(clip).expect("place Clip");
        sequence
    }

    #[test]
    fn cloned_authoring_collection_detaches_every_mutated_nested_list() {
        let original = populated_authoring_collection();
        let original_json = serde_json::to_vec(&original).expect("serialize original aggregate");
        let mut candidate = original.clone();

        candidate.sequences.push(Sequence::new("detached sibling"));
        let sequence = &mut candidate.sequences[0];
        sequence.video_tracks.push(Track::new_video("V4"));
        sequence.audio_tracks.pop();
        sequence.video_transitions.clear();
        sequence.audio_roles.clear();
        sequence.audio_program.processing_scopes.clear();
        sequence.audio_program.transitions.clear();
        sequence.audio_program.buses.clear();
        sequence.audio_program.outputs.clear();
        sequence.audio_program.routes.clear();
        let clip = &mut sequence.video_tracks[0].clips[0];
        clip.effects.clear();
        clip.masks.clear();
        clip.audio_components.clear();
        clip.label = Some("candidate only".to_owned());

        assert_eq!(
            serde_json::to_vec(&original).expect("serialize unchanged original"),
            original_json
        );
        assert_eq!(original.sequences.len(), 1);
        assert_eq!(original.sequences[0].video_tracks.len(), 3);
        assert_eq!(original.sequences[0].audio_tracks.len(), 3);
        assert_eq!(original.sequences[0].video_transitions.len(), 1);
        assert_eq!(original.sequences[0].audio_roles.len(), 1);
        assert_eq!(
            original.sequences[0].video_tracks[0].clips[0].effects.len(),
            1
        );
        assert_eq!(
            original.sequences[0].video_tracks[0].clips[0].masks.len(),
            1
        );
        assert_eq!(
            original.sequences[0].video_tracks[0].clips[0].audio_components.len(),
            1
        );
        assert_eq!(
            original.sequences[0].audio_program.processing_scopes.len(),
            1
        );
        assert_eq!(original.sequences[0].audio_program.transitions.len(), 1);
        assert_eq!(original.sequences[0].audio_program.buses.len(), 1);
        assert_eq!(original.sequences[0].audio_program.outputs.len(), 1);
        assert_eq!(original.sequences[0].audio_program.routes.len(), 3);
    }

    #[test]
    fn authoring_lists_preserve_vec_json_bytes_and_aggregate_roundtrip() {
        fn assert_vec_bytes<T: Serialize>(list: &AuthoringList<T>) {
            assert_eq!(
                serde_json::to_vec(list).expect("serialize AuthoringList"),
                serde_json::to_vec(list.as_slice()).expect("serialize equivalent slice")
            );
        }

        let collection = populated_authoring_collection();
        let sequence = &collection.sequences[0];
        let track = &sequence.video_tracks[0];
        let clip = &track.clips[0];
        assert_vec_bytes(&collection.sequences);
        assert_vec_bytes(&sequence.video_tracks);
        assert_vec_bytes(&sequence.video_transitions);
        assert_vec_bytes(&sequence.audio_tracks);
        assert_vec_bytes(&sequence.audio_roles);
        assert_vec_bytes(&track.clips);
        assert_vec_bytes(&clip.effects);
        assert_vec_bytes(&clip.masks);
        assert_vec_bytes(&clip.audio_components);
        assert_vec_bytes(&sequence.audio_program.processing_scopes);
        assert_vec_bytes(&sequence.audio_program.transitions);
        assert_vec_bytes(&sequence.audio_program.buses);
        assert_vec_bytes(&sequence.audio_program.outputs);
        assert_vec_bytes(&sequence.audio_program.routes);

        let json = serde_json::to_vec(&collection).expect("serialize aggregate");
        let restored: SequenceCollection =
            serde_json::from_slice(&json).expect("deserialize aggregate");
        assert_eq!(restored, collection);
        assert_eq!(
            serde_json::to_vec(&restored).expect("serialize restored aggregate"),
            json
        );
    }

    #[test]
    fn rich_author_snapshot_footprint_covers_visual_audio_and_opaque_payloads() {
        let baseline = AuthoringSnapshot::new(SequenceCollection::new(Sequence::new("baseline")));
        let mut rich = populated_authoring_collection();
        let sequence = &mut rich.sequences[0];
        let time_base = sequence.time_base();

        let transition = &mut sequence.video_transitions[0];
        transition.transition_type = crate::VideoTransitionType::Plugin {
            definition_id: "com.example.page-curl".to_owned(),
        };
        transition.params = serde_json::json!({
            "nested": {
                "labels": ["front", "back"],
                "shader": "page-curl-v4"
            }
        });

        let clip = &mut sequence.video_tracks[0].clips[0];
        clip.effects[0].effect_type = EffectType::Plugin("com.example.glow".to_owned());
        clip.effects[0].params = serde_json::json!({
            "kernel": [1.0, 0.5, 0.25],
            "resource": "project://luts/glow.cube"
        });
        clip.masks[0].shape_keyframes[0].shape = MaskShape::Path {
            points: vec![
                BezierPoint::new(glam::Vec2::new(0.1, 0.1)),
                BezierPoint::new(glam::Vec2::new(0.9, 0.9)),
            ],
            closed: true,
        };
        clip.audio_components[0].channel_mapping = AudioComponentChannelMapping::Explicit(
            AudioChannelMixMatrix::new(
                AudioChannelLayout::Stereo,
                AudioChannelLayout::Stereo,
                [
                    AudioChannelMixEntry::new(0, 0, 1.0).expect("left matrix edge"),
                    AudioChannelMixEntry::new(1, 1, 1.0).expect("right matrix edge"),
                ],
            )
            .expect("explicit matrix"),
        );

        sequence.video_tracks[1]
            .add_clip(
                Clip::new_basic_title(
                    "A deliberately retained title payload",
                    "Test Font Family",
                    tt(30, time_base),
                    tt(10, time_base),
                )
                .expect("Basic Title"),
            )
            .expect("place Basic Title");

        let mut vst3 = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        vst3.definition = AudioProcessorDefinitionRef::Vst3 {
            class_id: "00112233445566778899aabbccddeeff".to_owned(),
            vendor: Some("Example Audio".to_owned()),
            schema_version: 4,
        };
        vst3.opaque_state = Some(AuthoringList::from(vec![0x5a; 4096]));
        let audio_track_id = sequence.audio_tracks[0].id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&audio_track_id)
            .expect("audio Track mixer channel")
            .strip
            .pre_fader
            .processors
            .push(vst3);

        let mut clap = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        clap.definition = AudioProcessorDefinitionRef::Clap {
            plugin_id: "com.example.clap.saturator".to_owned(),
            schema_version: 2,
        };
        sequence
            .audio_program
            .track_channels
            .get_mut(&audio_track_id)
            .expect("audio Track mixer channel")
            .strip
            .post_fader
            .processors
            .push(clap);

        let rich = AuthoringSnapshot::new(rich);
        let baseline_manifest = footprint(&baseline);
        let rich_manifest = footprint(&rich);
        assert!(rich_manifest.total_bytes() > baseline_manifest.total_bytes() + 4096);
        assert!(rich_manifest.allocation_count() > baseline_manifest.allocation_count());
    }

    #[test]
    fn footprint_deduplicates_shared_roots_and_only_detached_branches_diverge() {
        let original = populated_authoring_collection();
        let mut candidate = original.clone();
        assert!(candidate.sequences.shares_allocation_with(&original.sequences));
        assert!(candidate.sequences[0]
            .audio_program
            .track_channels
            .shares_allocation_with(&original.sequences[0].audio_program.track_channels));

        candidate.sequences[0].video_tracks[0].clips[0].effects.push(EffectNode::new(
            EffectType::Plugin("com.example.detached".to_owned()),
        ));

        assert!(!candidate.sequences.shares_allocation_with(&original.sequences));
        assert!(!candidate.sequences[0]
            .video_tracks
            .shares_allocation_with(&original.sequences[0].video_tracks));
        assert!(!candidate.sequences[0].video_tracks[0]
            .clips
            .shares_allocation_with(&original.sequences[0].video_tracks[0].clips));
        assert!(!candidate.sequences[0].video_tracks[0].clips[0]
            .effects
            .shares_allocation_with(&original.sequences[0].video_tracks[0].clips[0].effects));
        assert!(candidate.sequences[0]
            .audio_program
            .track_channels
            .shares_allocation_with(&original.sequences[0].audio_program.track_channels));

        let original = AuthoringSnapshot::new(original);
        let shared = original.clone();
        let single_manifest = footprint(&original);
        let mut shared_collector = AuthoringFootprintCollector::new();
        shared_collector.collect(&original).expect("collect original");
        shared_collector.collect(&shared).expect("collect shared root");
        assert_eq!(shared_collector.finish(), single_manifest);

        let detached = AuthoringSnapshot::new(candidate);
        let mut detached_collector = AuthoringFootprintCollector::new();
        detached_collector.collect(&original).expect("collect original");
        detached_collector.collect(&detached).expect("collect detached root");
        let detached_manifest = detached_collector.finish();
        assert!(detached_manifest.total_bytes() > single_manifest.total_bytes());
        assert!(detached_manifest.allocation_count() > single_manifest.allocation_count());
    }

    #[test]
    fn structured_sequence_equality_ignores_only_revision() {
        let original = populated_authoring_collection().sequences[0].clone();
        let json = serde_json::to_vec(&original).expect("serialize Sequence");
        let restored: Sequence = serde_json::from_slice(&json).expect("deserialize Sequence");
        assert_eq!(restored, original);
        assert!(restored.author_state_eq_ignoring_revision(&original));

        let mut revision_only = original.clone();
        revision_only.revision = revision_only.revision.checked_next().expect("next revision");
        assert_ne!(revision_only, original);
        assert!(revision_only.author_state_eq_ignoring_revision(&original));

        let mut changed = original.clone();
        changed.name.push_str(" changed");
        assert!(!changed.author_state_eq_ignoring_revision(&original));

        let mut changed = original.clone();
        changed.settings.action_safe_margin += 0.01;
        assert!(!changed.author_state_eq_ignoring_revision(&original));

        let mut changed = original.clone();
        changed.video_tracks.push(Track::new_video("new video track"));
        assert!(!changed.author_state_eq_ignoring_revision(&original));

        let mut changed = original.clone();
        changed.audio_program.outputs[0].name.push_str(" changed");
        assert!(!changed.author_state_eq_ignoring_revision(&original));

        let mut changed = original;
        changed.playhead = TimelineTime::ONE;
        assert!(!changed.author_state_eq_ignoring_revision(&restored));
    }

    #[test]
    fn author_identity_validation_rejects_duplicate_clip_identity_across_tracks() {
        let mut sequence = Sequence::new("identity");
        let time_base = sequence.time_base();
        let clip =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("valid clip");
        sequence.video_tracks[0].add_clip(clip.clone()).expect("first placement");
        sequence.video_tracks[1].add_clip(clip).expect("second placement");

        let error =
            sequence.validate_author_identities().expect_err("duplicate identity must fail");
        assert!(error.to_string().contains("duplicate Clip identity"));
    }

    #[test]
    fn author_identity_validation_rejects_singleton_link_group() {
        let mut sequence = Sequence::new("identity");
        let time_base = sequence.time_base();
        let mut clip =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("valid clip");
        clip.link_group = Some(ClipLinkGroupId::new());
        sequence.video_tracks[0].add_clip(clip).expect("placement");

        let error = sequence.validate_author_identities().expect_err("singleton group must fail");
        assert!(error.to_string().contains("fewer than two members"));
    }

    #[test]
    fn compact_clip_link_groups_detaches_only_the_affected_track() {
        let mut original = Sequence::new("link compaction locality");
        let time_base = original.time_base();
        let mut singleton =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("singleton");
        singleton.link_group = Some(ClipLinkGroupId::new());
        original.video_tracks[0].add_clip(singleton).expect("singleton placement");
        original.video_tracks[1]
            .add_clip(
                Clip::new(AssetId::new(), tt(20, time_base), tt(10, time_base))
                    .expect("unaffected video"),
            )
            .expect("unaffected video placement");
        original.audio_tracks[0]
            .add_clip(
                Clip::new(AssetId::new(), tt(40, time_base), tt(10, time_base))
                    .expect("unaffected audio"),
            )
            .expect("unaffected audio placement");

        let mut compacted = original.clone();
        compacted.compact_clip_link_groups();

        assert!(!compacted.video_tracks.shares_allocation_with(&original.video_tracks));
        assert!(!compacted.video_tracks[0]
            .clips
            .shares_allocation_with(&original.video_tracks[0].clips));
        assert!(compacted.video_tracks[1]
            .clips
            .shares_allocation_with(&original.video_tracks[1].clips));
        assert!(compacted.audio_tracks.shares_allocation_with(&original.audio_tracks));
        assert!(compacted.audio_tracks[0]
            .clips
            .shares_allocation_with(&original.audio_tracks[0].clips));
        assert_eq!(compacted.video_tracks[0].clips[0].link_group, None);
    }

    #[test]
    fn no_op_structural_compaction_preserves_clip_and_transition_allocations() {
        let mut original = Sequence::new("structural compaction locality");
        let time_base = original.time_base();
        let left =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("left Clip");
        let right =
            Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right Clip");
        let (left_id, right_id) = (left.id, right.id);
        original.video_tracks[0].add_clip(left).expect("left placement");
        original.video_tracks[0].add_clip(right).expect("right placement");
        original.video_transitions.push(crate::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8, time_base), tt(4, time_base)).expect("Transition range"),
        ));
        original.validate_author_identities().expect("valid source Sequence");

        let mut compacted = original.clone();
        compacted.compact_structural_references();

        assert!(compacted.video_tracks.shares_allocation_with(&original.video_tracks));
        assert!(compacted.audio_tracks.shares_allocation_with(&original.audio_tracks));
        for (compacted_track, original_track) in
            compacted.video_tracks.iter().zip(&original.video_tracks)
        {
            assert!(compacted_track.clips.shares_allocation_with(&original_track.clips));
        }
        for (compacted_track, original_track) in
            compacted.audio_tracks.iter().zip(&original.audio_tracks)
        {
            assert!(compacted_track.clips.shares_allocation_with(&original_track.clips));
        }
        assert!(compacted.video_transitions.shares_allocation_with(&original.video_transitions));
        assert!(compacted
            .audio_program
            .transitions
            .shares_allocation_with(&original.audio_program.transitions));
        assert!(compacted
            .audio_program
            .processing_scopes
            .shares_allocation_with(&original.audio_program.processing_scopes));
    }

    #[test]
    fn video_transition_requires_one_adjacent_exact_cut() {
        let mut sequence = Sequence::new("transition");
        let time_base = sequence.time_base();
        let left = Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("left");
        let right = Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right");
        let left_id = left.id;
        let right_id = right.id;
        sequence.video_tracks[0].add_clip(left).expect("left placement");
        sequence.video_tracks[0].add_clip(right).expect("right placement");
        sequence.video_transitions.push(crate::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(tt(8, time_base), tt(4, time_base))
                .expect("range"),
        ));
        sequence.validate_author_identities().expect("valid transition");

        sequence.video_tracks[0].clips[1].position = tt(11, time_base);
        let error = sequence
            .validate_author_identities()
            .expect_err("gap must invalidate transition");
        assert!(error.to_string().contains("exact editorial cut"));
        sequence.compact_video_transitions();
        assert!(sequence.video_transitions.is_empty());
    }

    #[test]
    fn video_transition_ranges_on_one_track_cannot_overlap() {
        let mut sequence = Sequence::new("transition overlap");
        let time_base = sequence.time_base();
        let first = Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("first");
        let middle =
            Clip::new(AssetId::new(), tt(10, time_base), tt(4, time_base)).expect("middle");
        let last = Clip::new(AssetId::new(), tt(14, time_base), tt(10, time_base)).expect("last");
        let (first_id, middle_id, last_id) = (first.id, middle.id, last.id);
        sequence.video_tracks[0].add_clip(first).expect("first placement");
        sequence.video_tracks[0].add_clip(middle).expect("middle placement");
        sequence.video_tracks[0].add_clip(last).expect("last placement");
        sequence.video_transitions.push(crate::VideoTransition::cross_dissolve(
            first_id,
            middle_id,
            mondrian_core::TimelineTimeRange::new(tt(8, time_base), tt(4, time_base))
                .expect("first transition"),
        ));
        sequence.video_transitions.push(crate::VideoTransition::cross_dissolve(
            middle_id,
            last_id,
            mondrian_core::TimelineTimeRange::new(tt(11, time_base), tt(6, time_base))
                .expect("second transition"),
        ));

        let error = sequence
            .validate_author_identities()
            .expect_err("ambiguous simultaneous Transitions must fail");
        assert!(error.to_string().contains("must not overlap"));
    }

    #[test]
    fn sequence_duplicate_forks_complete_identity_graph_and_strong_references() {
        let mut sequence = Sequence::new("duplicate");
        let original_sequence_id = sequence.id;
        let original_track_ids = sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .map(|track| track.id)
            .collect::<HashSet<_>>();
        let time_base = sequence.time_base();
        let mut left =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("left");
        left.add_effect_node(EffectNode::with_defaults(EffectType::GaussianBlur));
        let right = Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right");
        let left_id = left.id;
        let right_id = right.id;
        sequence.video_tracks[0].add_clip(left).expect("left placement");
        sequence.video_tracks[0].add_clip(right).expect("right placement");
        let original_effect_id = sequence.video_tracks[0].clips[0].effects[0].id;
        let transition = crate::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(tt(8, time_base), tt(4, time_base))
                .expect("range"),
        );
        let original_transition_id = transition.id;
        sequence.video_transitions.push(transition);
        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let original_processor_id = processor.id;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
        let sidechain = AudioProcessorSidechainRoute::new(
            AudioRouteSource::Track {
                track_id: sequence.audio_tracks[0].id,
                port: crate::audio::AudioChannelStripOutputPort::PostMute,
            },
            original_processor_id,
            "detector",
        );
        let original_sidechain_id = sidechain.id;
        sequence.audio_program.sidechain_routes.push(sidechain);

        sequence.fork_author_identities_for_sequence_duplicate();

        assert_ne!(sequence.id, original_sequence_id);
        assert_eq!(sequence.revision, SequenceRevision::INITIAL);
        assert!(sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .all(|track| !original_track_ids.contains(&track.id)));
        let duplicated_left = &sequence.video_tracks[0].clips[0];
        let duplicated_right = &sequence.video_tracks[0].clips[1];
        assert_ne!(duplicated_left.id, left_id);
        assert_ne!(duplicated_right.id, right_id);
        assert_ne!(duplicated_left.effects[0].id, original_effect_id);
        assert_ne!(sequence.video_transitions[0].id, original_transition_id);
        assert_eq!(sequence.video_transitions[0].left, duplicated_left.id);
        assert_eq!(sequence.video_transitions[0].right, duplicated_right.id);
        let duplicated_sidechain = &sequence.audio_program.sidechain_routes[0];
        assert_ne!(duplicated_sidechain.id, original_sidechain_id);
        assert_ne!(duplicated_sidechain.processor_id, original_processor_id);
        assert_eq!(
            duplicated_sidechain.processor_id,
            sequence.audio_program.outputs[0].strip.pre_fader.processors[0].id
        );
        assert_eq!(
            duplicated_sidechain.source,
            AudioRouteSource::Track {
                track_id: sequence.audio_tracks[0].id,
                port: crate::audio::AudioChannelStripOutputPort::PostMute,
            }
        );
        sequence.validate_author_identities().expect("forked author graph");
        sequence
            .audio_program
            .validate(
                &sequence.audio_tracks,
                &sequence.audio_roles,
                sequence.settings.audio_channel_layout,
            )
            .expect("forked audio graph");
    }

    #[test]
    fn collection_validation_rejects_duplicate_sequence_identity() {
        let sequence = Sequence::new("identity");
        let mut collection = SequenceCollection::new(sequence.clone());
        collection.sequences.push(sequence);

        let error = collection
            .validate_dependency_closure()
            .expect_err("duplicate Sequence identity must fail");
        assert!(error.to_string().contains("duplicate Sequence identity"));
    }

    #[test]
    fn delivery_bit_depth_defaults_to_ten_bit_and_serializes_explicitly() {
        let delivery = SequenceDeliveryDefaults::default();
        assert_eq!(delivery.bit_depth, DeliveryBitDepth::Ten);

        let json = serde_json::to_value(delivery).expect("serialize Sequence delivery defaults");
        assert_eq!(json["bit_depth"], "Ten");
        assert!(json.get("export_bit_depth").is_none());
    }

    #[test]
    fn missing_color_metadata_policy_reports_input_resolution_source() {
        let working = WorkingColorSpace::LinearRec2020;

        let override_resolution = MissingColorMetadataPolicy::RejectMedia.resolve_input_decision(
            Some(ColorSpace::SonySLog3SGamut3Cine),
            Some(ColorSpace::Srgb),
            working,
        );
        assert_eq!(
            override_resolution.resolved,
            ResolvedInputColor::Color(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            override_resolution.source,
            InputColorResolutionSource::Override
        );
        assert_eq!(
            override_resolution.executable_color_space,
            Some(ColorSpace::Srgb)
        );

        let detected_resolution = MissingColorMetadataPolicy::AssumeRec709.resolve_input_decision(
            None,
            Some(ColorSpace::DisplayP3),
            working,
        );
        assert_eq!(
            detected_resolution.resolved,
            ResolvedInputColor::Color(ColorSpace::DisplayP3)
        );
        assert_eq!(
            detected_resolution.source,
            InputColorResolutionSource::DetectedMetadata
        );

        let rejected_resolution =
            MissingColorMetadataPolicy::RejectMedia.resolve_input_decision(None, None, working);
        assert_eq!(rejected_resolution.resolved, ResolvedInputColor::Rejected);
        assert_eq!(
            rejected_resolution.source,
            InputColorResolutionSource::MissingPolicyRejectMedia
        );
    }

    #[test]
    fn asset_media_interpretation_participates_in_input_resolution() {
        let working = WorkingColorSpace::LinearRec2020;

        let asset_override = MissingColorMetadataPolicy::RejectMedia.resolve_asset_input_decision(
            None,
            AssetMediaInterpretation {
                color: MediaColorInterpretation::Override {
                    color_space: ColorSpace::SonySLog3SGamut3Cine,
                },
                ..AssetMediaInterpretation::default()
            },
            Some(ColorSpace::Rec709),
            working,
        );
        assert_eq!(
            asset_override.resolved,
            ResolvedInputColor::Color(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(asset_override.source, InputColorResolutionSource::Override);

        let data = MissingColorMetadataPolicy::RejectMedia.resolve_asset_input_decision(
            None,
            AssetMediaInterpretation {
                payload: mondrian_core::timeline_data::AssetColorPayload::NonColorData,
                ..AssetMediaInterpretation::default()
            },
            None,
            working,
        );
        assert_eq!(data.resolved, ResolvedInputColor::Data);
        assert_eq!(data.source, InputColorResolutionSource::DataTexture);

        let clip_override = MissingColorMetadataPolicy::RejectMedia.resolve_asset_input_decision(
            Some(ColorSpace::AppleLogBt2020),
            AssetMediaInterpretation {
                color: MediaColorInterpretation::Override {
                    color_space: ColorSpace::SonySLog3SGamut3Cine,
                },
                ..AssetMediaInterpretation::default()
            },
            Some(ColorSpace::Rec709),
            working,
        );
        assert_eq!(
            clip_override.resolved,
            ResolvedInputColor::Color(ColorSpace::AppleLogBt2020)
        );
        assert_eq!(clip_override.source, InputColorResolutionSource::Override);
    }

    #[test]
    fn input_color_resolution_source_reports_diagnostic_categories() {
        assert!(InputColorResolutionSource::Override.is_explicit_metadata_or_override());
        assert!(InputColorResolutionSource::DetectedMetadata.is_explicit_metadata_or_override());
        assert!(!InputColorResolutionSource::DataTexture.is_explicit_metadata_or_override());
        assert!(!InputColorResolutionSource::MissingPolicyAssumeRec709
            .is_explicit_metadata_or_override());

        assert!(InputColorResolutionSource::MissingPolicyAssumeRec709.is_policy_assumption());
        assert!(!InputColorResolutionSource::MissingPolicyRejectMedia.is_policy_assumption());
        assert!(!InputColorResolutionSource::DataTexture.is_policy_assumption());

        assert!(InputColorResolutionSource::MissingPolicyRejectMedia.is_policy_rejection());
        assert!(!InputColorResolutionSource::Override.is_policy_rejection());

        assert!(InputColorResolutionSource::DataTexture.is_data_texture());
        assert!(!InputColorResolutionSource::Override.is_data_texture());
    }

    #[test]
    fn input_color_resolution_source_counts_derive_diagnostic_totals() {
        let mut counts = InputColorResolutionSourceCounts::default();
        counts.record(InputColorResolutionSource::Override);
        counts.record(InputColorResolutionSource::DataTexture);
        counts.record(InputColorResolutionSource::DetectedMetadata);
        counts.record(InputColorResolutionSource::DetectedMetadata);
        counts.record(InputColorResolutionSource::MissingPolicyAssumeRec709);
        counts.record(InputColorResolutionSource::MissingPolicyRejectMedia);

        assert_eq!(counts.count(InputColorResolutionSource::Override), 1);
        assert_eq!(counts.count(InputColorResolutionSource::DataTexture), 1);
        assert_eq!(
            counts.count(InputColorResolutionSource::DetectedMetadata),
            2
        );
        assert_eq!(counts.explicit_metadata_or_override(), 3);
        assert_eq!(counts.policy_assumptions(), 1);
        assert_eq!(counts.policy_rejections(), 1);
        assert_eq!(counts.data_textures(), 1);
        assert_eq!(counts.total(), 6);

        let mut accumulated = InputColorResolutionSourceCounts::default();
        accumulated.record(InputColorResolutionSource::Override);
        accumulated.accumulate(counts);
        assert_eq!(accumulated.count(InputColorResolutionSource::Override), 2);
        assert_eq!(
            accumulated.count(InputColorResolutionSource::DetectedMetadata),
            2
        );
        assert_eq!(accumulated.total(), 7);
    }

    #[test]
    fn sequence_active_clips() {
        let mut seq = Sequence::new("Test");
        let asset_id = AssetId::new();
        let tb = seq.time_base();

        let clip = Clip::new(asset_id, tt(0, tb), tt(50, tb)).expect("valid clip");
        seq.video_tracks[0].add_clip(clip).unwrap();

        let active = seq.active_clips_at(tt(25, tb)).expect("evaluate timeline");
        assert_eq!(active.len(), 1);

        let outside = seq.active_clips_at(tt(100, tb)).expect("evaluate timeline");
        assert_eq!(outside.len(), 0);
    }

    #[test]
    fn clip_visual_automation_evaluates_in_clip_local_time() {
        let mut seq = Sequence::new("Clip-local animation");
        let tb = seq.time_base();
        let mut clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        clip.set_source_origin(tt(100, tb)).expect("set source origin");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: crate::clip::Transform2D::OPACITY_PATH.to_owned(),
            keyframe: Keyframe::linear(tt(0, tb), PropertyValue::Float(0.0)),
        })
        .expect("start key");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: crate::clip::Transform2D::OPACITY_PATH.to_owned(),
            keyframe: Keyframe::linear(tt(20, tb), PropertyValue::Float(1.0)),
        })
        .expect("end key");
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let active = seq.active_clips_at(tt(20, tb)).expect("evaluate timeline");

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].clip_time, tt(10, tb));
        assert_eq!(active[0].source_sample.time(), tt(110, tb));
        assert!((active[0].opacity - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn track_opacity_automation_affects_active_clip_opacity() {
        let mut seq = Sequence::new("Opacity Test");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.video_tracks[0]
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Track::OPACITY_PATH.to_string(),
                keyframe: Keyframe::linear(tt(0, tb), PropertyValue::Float(1.0)),
            })
            .expect("set start opacity");
        seq.video_tracks[0]
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Track::OPACITY_PATH.to_string(),
                keyframe: Keyframe::linear(tt(20, tb), PropertyValue::Float(0.4)),
            })
            .expect("set end opacity");

        let active = seq.active_clips_at(tt(10, tb)).expect("evaluate timeline");
        assert_eq!(active.len(), 1);
        assert!((active[0].opacity - 0.7).abs() < 0.01);
    }

    #[test]
    fn active_clips_follow_bottom_to_top_track_order() {
        let mut seq = Sequence::new("Track Order");
        let tb = seq.time_base();
        let bottom = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        let top = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        seq.video_tracks[0].add_clip(bottom).expect("add bottom clip");
        seq.video_tracks[2].add_clip(top).expect("add top clip");

        let active = seq.active_clips_at(tt(5, tb)).expect("evaluate timeline");
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].track_index, 0);
        assert_eq!(active[1].track_index, 2);
    }

    #[test]
    fn active_clips_inherit_track_blend_mode_when_clip_uses_default() {
        let mut seq = Sequence::new("Track Blend Inheritance");
        let tb = seq.time_base();
        seq.video_tracks[0].blend_mode = BlendMode::Screen;
        let clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let active = seq.active_clips_at(tt(5, tb)).expect("evaluate timeline");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].blend_mode, BlendMode::Screen);
    }

    #[test]
    fn active_clips_prefer_clip_blend_mode_over_track_blend_mode() {
        let mut seq = Sequence::new("Clip Blend Override");
        let tb = seq.time_base();
        seq.video_tracks[0].blend_mode = BlendMode::Screen;
        let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
        clip.blend_mode = Some(BlendMode::Multiply);
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let active = seq.active_clips_at(tt(5, tb)).expect("evaluate timeline");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].blend_mode, BlendMode::Multiply);
    }

    #[test]
    fn sequence_settings_validate_supported_presets() {
        let settings = SequenceSettings {
            frame_rate: Rational::FPS_23976,
            pixel_aspect_ratio: PixelAspectRatio::D1DvNtscWidescreen,
            field_order: FieldOrder::Progressive,
            timeline_display: TimelineDisplaySettings::timecode(
                SmpteCountingMode::NonDropFrame,
                24 * 60 * 60,
            ),
            color: SequenceColorSettings {
                working_color_space: WorkingColorSpace::LinearRec2020,
                ..SequenceColorSettings::default()
            },
            audio_sample_rate: 96_000,
            audio_display_format: AudioDisplayFormat::Milliseconds,
            audio_channel_layout: AudioChannelLayout::Surround51Side,
            preview: SequencePreviewSettings {
                format: PreviewRenderFormat::ProResProxy,
                resolution_scale: 0.5,
                cache_enabled: true,
            },
            ..Default::default()
        };

        settings.validate().expect("professional sequence preset should validate");
        assert!((settings.pixel_aspect_ratio.ratio().unwrap() - 1.2121).abs() < 0.0001);
    }

    #[test]
    fn editing_mode_preset_materializes_sequence_settings() {
        let vertical = SequenceSettings::from_editing_mode(EditingMode::SocialVertical1080p);
        assert_eq!(
            vertical.resolution,
            Resolution { width: 1080, height: 1920 }
        );
        assert_eq!(vertical.frame_rate, Rational::FPS_30);

        let cinema = SequenceSettings::from_editing_mode(EditingMode::DigitalCinema4k);
        assert_eq!(cinema.resolution, Resolution::DCI4K);
        assert_eq!(cinema.frame_rate, Rational::FPS_24);
        assert_eq!(
            cinema.color.working_color_space,
            WorkingColorSpace::LinearRec2020
        );
    }

    #[test]
    fn sequence_settings_reject_unsupported_fps_and_sample_rate() {
        let unsupported_fps = SequenceSettings {
            frame_rate: Rational::new(48, 1),
            ..Default::default()
        };
        assert!(unsupported_fps.validate().is_err());

        let unsupported_audio =
            SequenceSettings { audio_sample_rate: 22_050, ..Default::default() };
        assert!(unsupported_audio.validate().is_err());

        let bad_preview = SequenceSettings {
            preview: SequencePreviewSettings { resolution_scale: 2.0, ..Default::default() },
            ..Default::default()
        };
        assert!(bad_preview.validate().is_err());
    }

    #[test]
    fn sequence_settings_reject_inert_scan_and_unknown_geometry_contracts() {
        let interlaced = SequenceSettings {
            field_order: FieldOrder::UpperFirst,
            ..Default::default()
        };
        let error = interlaced.validate().expect_err("interlaced output must fail closed");
        assert!(error.to_string().contains("逐行"));

        let unknown_par = SequenceSettings {
            pixel_aspect_ratio: PixelAspectRatio::Unknown,
            ..Default::default()
        };
        let error = unknown_par.validate().expect_err("unknown authored PAR must fail closed");
        assert!(error.to_string().contains("Unknown"));
    }

    #[test]
    fn sequence_settings_reject_non_finite_or_canvas_consuming_safe_margins() {
        for title_safe_margin in [f32::NAN, f32::INFINITY, -0.01, 1.0] {
            let settings = SequenceSettings { title_safe_margin, ..SequenceSettings::default() };
            assert!(settings.validate().is_err());
        }
        for action_safe_margin in [f32::NAN, f32::NEG_INFINITY, -0.01, 1.0] {
            let settings = SequenceSettings { action_safe_margin, ..SequenceSettings::default() };
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn sequence_preset_validates_name_and_settings() {
        assert!(SequencePreset::new("", SequenceSettings::default()).is_err());
        let preset = SequencePreset::new("Editorial 25p", SequenceSettings::default())
            .expect("valid preset");
        assert_eq!(preset.name, "Editorial 25p");
    }

    #[test]
    fn sequence_color_management_rejects_invalid_hdr_metadata_policy() {
        let mut settings = SequenceSettings::default();
        settings.delivery.static_hdr_metadata_policy = StaticHdrMetadataPolicy::WriteAuthored;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn sequence_color_management_rejects_source_only_output_space() {
        let mut settings = SequenceSettings::default();
        settings.color.program_output.color_space = ColorSpace::AcesCg;

        assert!(settings.validate().is_err());
    }

    #[test]
    fn sequence_color_management_accepts_hdr_output_metadata_policy() {
        let mut settings = SequenceSettings::default();
        settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        settings.delivery.static_hdr_metadata_policy = StaticHdrMetadataPolicy::WriteAuthored;
        settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn static_hdr_metadata_policy_serialization_is_explicit_and_breaking() {
        let delivery = SequenceDeliveryDefaults {
            static_hdr_metadata_policy: StaticHdrMetadataPolicy::WriteAuthored,
            ..SequenceDeliveryDefaults::default()
        };
        let json = serde_json::to_value(&delivery).expect("serialize delivery defaults");
        assert_eq!(
            json.get("static_hdr_metadata_policy"),
            Some(&serde_json::Value::String("WriteAuthored".to_owned()))
        );

        let old_shape = serde_json::json!({ "preserve_hdr_metadata": true });
        let error = serde_json::from_value::<SequenceDeliveryDefaults>(old_shape)
            .expect_err("removed preservation flag must not silently become Omit");
        assert!(error.to_string().contains("preserve_hdr_metadata"));
    }

    #[test]
    fn sequence_color_management_rejects_numerically_invalid_hdr_metadata() {
        let mut mastering = VideoMasteringDisplayMetadata::rec2100_1000_nit_reference();
        mastering.luminance.as_mut().expect("reference luminance").max =
            mondrian_core::VideoHdrRational::new(1000, 0);
        let mut invalid_mastering = SequenceSettings::default();
        invalid_mastering.color.program_output.color_space = ColorSpace::Rec2100Pq;
        invalid_mastering.delivery.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        invalid_mastering.delivery.hdr_mastering_display = Some(mastering);
        invalid_mastering.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        assert!(invalid_mastering.validate().is_err());

        let mut invalid_content_light = SequenceSettings::default();
        invalid_content_light.color.program_output.color_space = ColorSpace::Rec2100Pq;
        invalid_content_light.delivery.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        invalid_content_light.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        invalid_content_light.delivery.hdr_content_light = Some(VideoContentLightMetadata {
            max_content_light_level: 400,
            max_frame_average_light_level: 500,
        });
        assert!(invalid_content_light.validate().is_err());
    }

    #[test]
    fn editing_mode_preserves_color_management_policy() {
        let mut settings = SequenceSettings::default();
        settings.color.working_color_space = WorkingColorSpace::AcesCg;
        settings.color.program_output.workflow = ColorWorkflow::SceneReferred;
        settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        settings.color.input.auto_tone_map_media = false;
        settings.delivery.static_hdr_metadata_policy = StaticHdrMetadataPolicy::WriteAuthored;
        settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());

        settings.apply_editing_mode_preset(EditingMode::Custom);

        assert_eq!(
            settings.color.working_color_space,
            WorkingColorSpace::AcesCg
        );
        assert_eq!(
            settings.color.program_output.workflow,
            ColorWorkflow::SceneReferred
        );
        assert_eq!(
            settings.color.program_output.color_space,
            ColorSpace::Rec2100Pq
        );
        assert_eq!(
            settings.delivery.static_hdr_metadata_policy,
            StaticHdrMetadataPolicy::WriteAuthored
        );
        assert!(settings.delivery.hdr_mastering_display.is_some());
        assert!(settings.delivery.hdr_content_light.is_some());
        assert!(!settings.color.input.auto_tone_map_media);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn applying_sequence_settings_preserves_exact_author_time() {
        let mut seq = Sequence::new("Settings");
        let old_tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(
                Clip::new(AssetId::new(), tt(10, old_tb), tt(20, old_tb)).expect("valid clip"),
            )
            .expect("add clip");

        let settings = SequenceSettings {
            frame_rate: Rational::FPS_2997,
            ..seq.settings.clone()
        };
        seq.apply_settings(settings).expect("apply settings");

        let new_tb = seq.time_base();
        assert_eq!(new_tb, Rational::new(1001, 30000));
        assert_eq!(seq.video_tracks[0].clips[0].position, tt(10, old_tb));
        assert_eq!(seq.video_tracks[0].clips[0].duration, tt(20, old_tb));
    }

    #[test]
    fn sequence_author_contract_rejects_track_container_type_mismatch() {
        let mut sequence = Sequence::new("Type mismatch");
        sequence.video_tracks[0].track_type = TrackType::Audio;
        let error = sequence
            .validate_author_contract(&standard_environment())
            .expect_err("video container may not hold an Audio Track");
        assert!(error.to_string().contains("declares incompatible type"));

        let mut sequence = Sequence::new("Type mismatch");
        sequence.audio_tracks[0].track_type = TrackType::Video;
        let error = sequence
            .validate_author_contract(&standard_environment())
            .expect_err("audio container may not hold a Video Track");
        assert!(error.to_string().contains("declares incompatible type"));
    }

    #[test]
    fn incremental_author_contract_visits_only_the_changed_local_spine() {
        let environment = standard_environment();
        let sequence = sequence_with_local_validation_children();
        let certificate = sequence
            .prepare_author_contract_certificate(&environment)
            .expect("valid baseline");

        let mut effect_edit = sequence.clone();
        effect_edit.revision = effect_edit.revision.checked_next().expect("next revision");
        effect_edit.video_tracks[0].clips[0].effects[0]
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: "effect.gaussian_blur.radius".to_owned(),
                keyframe: Keyframe::linear(TimelineTime::ZERO, PropertyValue::Float(12.0)),
            })
            .expect("set effect keyframe");
        reset_sequence_author_contract_validation_visits();
        let effect_certificate = certificate
            .prepare_replacement(&sequence, &effect_edit, &environment)
            .expect("valid effect edit");
        assert_eq!(
            sequence_author_contract_validation_visits(),
            SequenceAuthorContractValidationVisits {
                identity_passes: 1,
                audio_passes: 1,
                tracks: 1,
                clips: 1,
                effects: 1,
                masks: 0,
            }
        );

        let mut placement_edit = effect_edit.clone();
        placement_edit.revision =
            placement_edit.revision.checked_next().expect("next placement revision");
        placement_edit.video_tracks[0].clips[0].position = TimelineTime::ONE;
        reset_sequence_author_contract_validation_visits();
        effect_certificate
            .prepare_replacement(&effect_edit, &placement_edit, &environment)
            .expect("valid placement edit");
        assert_eq!(
            sequence_author_contract_validation_visits(),
            SequenceAuthorContractValidationVisits {
                identity_passes: 1,
                audio_passes: 1,
                tracks: 1,
                clips: 1,
                effects: 0,
                masks: 0,
            }
        );
    }

    #[test]
    fn detached_equal_author_state_reuses_local_validation_without_allocation_identity() {
        let environment = standard_environment();
        let sequence = sequence_with_local_validation_children();
        let certificate = sequence
            .prepare_author_contract_certificate(&environment)
            .expect("valid baseline");
        let json = serde_json::to_vec(&sequence).expect("serialize baseline");
        let restored: Sequence = serde_json::from_slice(&json).expect("deserialize baseline");
        assert!(!restored.video_tracks.shares_allocation_with(&sequence.video_tracks));
        let mut candidate = restored.clone();
        candidate.revision = candidate.revision.checked_next().expect("next revision");

        reset_sequence_author_contract_validation_visits();
        let next = certificate
            .prepare_replacement(&restored, &candidate, &environment)
            .expect("structurally exact deserialized baseline remains valid");
        assert_eq!(
            sequence_author_contract_validation_visits(),
            SequenceAuthorContractValidationVisits {
                identity_passes: 1,
                audio_passes: 1,
                ..SequenceAuthorContractValidationVisits::default()
            }
        );
        assert!(next.baseline.video_tracks.shares_allocation_with(&candidate.video_tracks));
        assert!(next.baseline.audio_tracks.shares_allocation_with(&candidate.audio_tracks));
    }

    #[test]
    fn sequence_certificate_rejects_stale_state_revision_and_color_context() {
        let environment = standard_environment();
        let sequence = sequence_with_local_validation_children();
        let certificate = sequence
            .prepare_author_contract_certificate(&environment)
            .expect("valid baseline");

        let mut mutated_current = sequence.clone();
        let clips_allocation = mutated_current.video_tracks[0].clips.allocation_id();
        mutated_current.name = "same nested allocation, different state".to_owned();
        assert_eq!(
            mutated_current.video_tracks[0].clips.allocation_id(),
            clips_allocation
        );
        let mut candidate = mutated_current.clone();
        candidate.revision = candidate.revision.checked_next().expect("next revision");
        assert!(certificate
            .prepare_replacement(&mutated_current, &candidate, &environment)
            .expect_err("same-ID unique mutation must stale the exact baseline")
            .to_string()
            .contains("author state"));

        let mut forged_revision = sequence.clone();
        forged_revision.name = "forged revision".to_owned();
        assert!(certificate
            .prepare_replacement(&sequence, &forged_revision, &environment)
            .expect_err("non-monotonic revision must fail")
            .to_string()
            .contains("must be newer"));

        let other_environment = ProjectColorEnvironment::new(ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::default(),
        });
        let mut valid_revision = sequence.clone();
        valid_revision.revision =
            valid_revision.revision.checked_next().expect("next valid revision");
        assert!(certificate
            .prepare_replacement(&sequence, &valid_revision, &other_environment)
            .expect_err("certificate may not cross color environments")
            .to_string()
            .contains("different Project color environment"));
    }

    #[test]
    fn incremental_and_full_author_contracts_reject_the_same_global_failures() {
        fn assert_both_reject(base: &Sequence, mut candidate: Sequence) {
            let environment = standard_environment();
            let certificate =
                base.prepare_author_contract_certificate(&environment).expect("valid baseline");
            candidate.revision = base.revision.checked_next().expect("next revision");
            assert!(candidate.validate_author_contract(&environment).is_err());
            assert!(certificate.prepare_replacement(base, &candidate, &environment).is_err());
        }

        let base = sequence_with_local_validation_children();
        let mut duplicate_clip = base.clone();
        let duplicate = duplicate_clip.video_tracks[0].clips[0].clone();
        duplicate_clip.video_tracks[1]
            .add_clip(duplicate)
            .expect("place duplicate Clip identity");
        assert_both_reject(&base, duplicate_clip);

        let mut singleton_link = base.clone();
        singleton_link.video_tracks[0].clips[0].link_group = Some(ClipLinkGroupId::new());
        assert_both_reject(&base, singleton_link);

        let mut duplicate_effect = base.clone();
        let effect = duplicate_effect.video_tracks[0].clips[0].effects[0].clone();
        duplicate_effect.video_tracks[0].clips[0].effects.push(effect);
        assert_both_reject(&base, duplicate_effect);

        let mut duplicate_mask = base.clone();
        let mask = duplicate_mask.video_tracks[0].clips[0].masks[0].clone();
        duplicate_mask.video_tracks[0].clips[0].masks.push(mask);
        assert_both_reject(&base, duplicate_mask);

        let mut invalid_audio = base.clone();
        invalid_audio.audio_program.outputs[0].id = ProgramOutputId::new();
        assert_both_reject(&base, invalid_audio);

        let mut transition_base = Sequence::new("Transition baseline");
        let time_base = transition_base.time_base();
        let left =
            Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("left Clip");
        let right =
            Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right Clip");
        let (left_id, right_id) = (left.id, right.id);
        transition_base.video_tracks[0].add_clip(left).expect("left placement");
        transition_base.video_tracks[0].add_clip(right).expect("right placement");
        transition_base.video_transitions.push(crate::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8, time_base), tt(4, time_base)).expect("Transition range"),
        ));
        let mut invalid_transition = transition_base.clone();
        invalid_transition.video_tracks[0].clips[1].position = tt(11, time_base);
        assert_both_reject(&transition_base, invalid_transition);
    }

    #[test]
    fn sequence_collection_detects_nested_sequence_cycles() {
        let mut parent = Sequence::new("Parent");
        let mut child = Sequence::new("Child");
        let parent_id = parent.id;
        let child_id = child.id;
        let tb = parent.time_base();

        parent.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child_id,
                    tt(0, tb),
                    tt(20, tb),
                    Some("Child".to_string()),
                )
                .expect("valid nested clip"),
            )
            .expect("add child nest");
        child.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    parent_id,
                    tt(0, tb),
                    tt(20, tb),
                    Some("Parent".to_string()),
                )
                .expect("valid nested clip"),
            )
            .expect("add parent nest");

        let mut collection = SequenceCollection::new(parent);
        collection.add_sequence(child).expect("add child sequence");

        assert!(collection.validate_dependency_closure().is_err());
    }

    fn deep_nested_collection(length: usize, close_cycle: bool) -> SequenceCollection {
        assert!(length >= 2);
        let mut sequences = (0..length)
            .map(|index| Sequence::new(format!("Nested {index}")))
            .collect::<Vec<_>>();
        let sequence_ids = sequences.iter().map(|sequence| sequence.id).collect::<Vec<_>>();
        for index in 0..length - 1 {
            let time_base = sequences[index].time_base();
            sequences[index].video_tracks[0]
                .add_clip(
                    Clip::new_nested_sequence(
                        sequence_ids[index + 1],
                        TimelineTime::ZERO,
                        tt(1, time_base),
                        None,
                    )
                    .expect("valid nested placement"),
                )
                .expect("place nested Sequence");
        }
        if close_cycle {
            let last = length - 1;
            let time_base = sequences[last].time_base();
            sequences[last].video_tracks[0]
                .add_clip(
                    Clip::new_nested_sequence(
                        sequence_ids[0],
                        TimelineTime::ZERO,
                        tt(1, time_base),
                        None,
                    )
                    .expect("valid cycle placement"),
                )
                .expect("place cycle edge");
        }

        let first = sequences.remove(0);
        let mut collection = SequenceCollection::new(first);
        for sequence in sequences {
            collection.add_sequence(sequence).expect("unique Sequence");
        }
        collection
    }

    #[test]
    fn dependency_validation_handles_a_deep_acyclic_chain_without_recursion() {
        deep_nested_collection(1_024, false)
            .validate_dependency_closure()
            .expect("deep acyclic author graph remains valid");
    }

    #[test]
    fn dependency_validation_rejects_a_deep_cycle_without_recursion() {
        let error = deep_nested_collection(1_024, true)
            .validate_dependency_closure()
            .expect_err("deep cycle must be rejected");
        assert!(error.to_string().contains("检测到序列嵌套循环"));
    }

    #[test]
    fn replacement_overlay_detects_a_cycle_without_mutating_the_collection() {
        let mut parent = Sequence::new("Parent");
        let child = Sequence::new("Child");
        let parent_id = parent.id;
        let child_id = child.id;
        let parent_time_base = parent.time_base();
        parent.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child_id,
                    TimelineTime::ZERO,
                    tt(20, parent_time_base),
                    Some("Child".to_owned()),
                )
                .expect("valid child placement"),
            )
            .expect("add child placement");
        let mut collection = SequenceCollection::new(parent);
        collection.add_sequence(child.clone()).expect("add child Sequence");
        collection.validate_dependency_closure().expect("stored collection is acyclic");

        let mut replacement = child;
        let replacement_time_base = replacement.time_base();
        replacement.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    parent_id,
                    TimelineTime::ZERO,
                    tt(20, replacement_time_base),
                    Some("Parent".to_owned()),
                )
                .expect("valid parent placement"),
            )
            .expect("add parent placement");

        let error = collection
            .validate_dependency_closure_with_replacement(&replacement)
            .expect_err("overlay cycle must be rejected");

        assert!(error.to_string().contains("检测到序列嵌套循环"));
        collection
            .validate_dependency_closure()
            .expect("failed overlay must not mutate stored Sequences");
    }

    #[test]
    fn replacement_overlay_revalidates_existing_nested_audio_bindings() {
        let child = Sequence::new("Child");
        let child_output = child.audio_program.outputs[0].id;
        let child_id = child.id;
        let mut parent = Sequence::new("Parent");
        let parent_audio_track = parent.audio_tracks[0].id;
        let nested = Clip::new_nested_sequence(
            child_id,
            TimelineTime::ZERO,
            tt(20, parent.time_base()),
            Some("Child".to_owned()),
        )
        .expect("valid nested audio placement");
        parent
            .add_nested_audio_clip(parent_audio_track, nested, child_output)
            .expect("bind child output");
        parent.audio_tracks[0].clips[0].audio_components[0].channel_mapping =
            crate::audio::AudioComponentChannelMapping::Explicit(AudioChannelMixMatrix::identity(
                AudioChannelLayout::Stereo,
            ));
        let mut collection = SequenceCollection::new(parent);
        collection.add_sequence(child.clone()).expect("add child Sequence");
        collection
            .validate_dependency_closure()
            .expect("initial nested binding is valid");

        let mut output_replacement = child.clone();
        let replacement_output = ProgramOutputId::new();
        output_replacement.audio_program.outputs[0].id = replacement_output;
        for route in &mut output_replacement.audio_program.routes {
            if route.destination == crate::audio::AudioRouteDestination::Output(child_output) {
                route.destination = crate::audio::AudioRouteDestination::Output(replacement_output);
            }
        }
        let output_error = collection
            .validate_dependency_closure_with_replacement(&output_replacement)
            .expect_err("parent binding to removed public output must fail");
        assert!(output_error.to_string().contains("nested audio output does not exist"));

        let mut layout_replacement = child;
        layout_replacement.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let layout_error = collection
            .validate_dependency_closure_with_replacement(&layout_replacement)
            .expect_err("parent matrix authored for the old child layout must fail");
        assert!(layout_error
            .to_string()
            .contains("matrix source layout does not match child Sequence"));

        collection
            .validate_dependency_closure()
            .expect("failed overlays must not mutate the canonical binding");
    }

    #[test]
    fn replacement_overlay_rejects_an_unknown_sequence_identity() {
        let collection = SequenceCollection::new(Sequence::new("Stored"));
        let replacement = Sequence::new("Not in collection");

        let error = collection
            .validate_dependency_closure_with_replacement(&replacement)
            .expect_err("unknown replacement identity must fail");

        assert!(error
            .to_string()
            .contains("replacement Sequence does not exist in the collection"));
        collection
            .validate_dependency_closure()
            .expect("failed lookup must not mutate the collection");
    }

    #[test]
    fn adjustment_layer_is_active_only_within_its_time_range() {
        let mut seq = Sequence::new("Adjustment Range");
        let tb = seq.time_base();
        let media = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
        let adjustment = Clip::new_adjustment_layer(AssetId::new(), tt(10, tb), tt(5, tb))
            .expect("valid adjustment clip");
        seq.video_tracks[0].add_clip(media).expect("add media clip");
        seq.video_tracks[1].add_clip(adjustment).expect("add adjustment clip");

        let before = seq.active_clips_at(tt(9, tb)).expect("evaluate timeline");
        assert_eq!(before.len(), 1);
        assert!(!before.iter().any(|clip| clip.clip.is_adjustment_layer()));

        let overlapping = seq.active_clips_at(tt(12, tb)).expect("evaluate timeline");
        assert_eq!(overlapping.len(), 2);
        assert!(overlapping.iter().any(|clip| clip.clip.is_adjustment_layer()));

        let after = seq.active_clips_at(tt(15, tb)).expect("evaluate timeline");
        assert_eq!(after.len(), 1);
        assert!(!after.iter().any(|clip| clip.clip.is_adjustment_layer()));
    }

    #[test]
    fn removing_track_renumbers_remaining_tracks() {
        let mut seq = Sequence::new("Track Names");
        let removed_id = seq.video_tracks[1].id;
        let last_id = seq.video_tracks[2].id;

        seq.remove_video_track(removed_id).expect("remove middle video track");

        assert_eq!(seq.video_tracks.len(), 2);
        assert_eq!(seq.video_tracks[0].name, "V1");
        assert_eq!(seq.video_tracks[1].name, "V2");
        assert_eq!(seq.video_tracks[1].id, last_id);
    }

    #[test]
    fn moving_track_preserves_track_identity_and_clips() {
        let mut seq = Sequence::new("Track Move");
        let tb = seq.time_base();
        let anchor_id = seq.video_tracks[0].id;
        let moved_id = seq.video_tracks[2].id;
        let clip = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
        let clip_id = clip.id;
        seq.video_tracks[2].add_clip(clip).expect("add clip to track");

        assert!(seq
            .reorder_track_relative(moved_id, crate::TrackRelativePlacement::Before(anchor_id))
            .expect("move track before stable anchor"));

        assert_eq!(seq.video_tracks[0].id, moved_id);
        assert_eq!(seq.video_tracks[0].name, "V1");
        assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
        assert_eq!(seq.video_tracks[1].name, "V2");
        assert_eq!(seq.video_tracks[2].name, "V3");
    }

    #[test]
    fn track_relative_order_rejects_cross_kind_and_elides_satisfied_relations() {
        let mut seq = Sequence::new("Track Move Contract");
        let first_video = seq.video_tracks[0].id;
        let second_video = seq.video_tracks[1].id;
        let first_audio = seq.audio_tracks[0].id;

        assert!(!seq
            .track_relative_placement_would_change(
                first_video,
                crate::TrackRelativePlacement::Before(second_video),
            )
            .expect("existing relation is valid"));
        assert!(!seq
            .reorder_track_relative(
                first_video,
                crate::TrackRelativePlacement::Before(second_video),
            )
            .expect("existing relation is a no-op"));
        assert!(seq
            .reorder_track_relative(
                first_video,
                crate::TrackRelativePlacement::After(second_video),
            )
            .expect("reverse the relation"));
        assert_eq!(seq.video_tracks[1].id, first_video);

        assert!(seq
            .reorder_track_relative(
                first_video,
                crate::TrackRelativePlacement::Before(first_audio),
            )
            .is_err());
        assert!(seq
            .reorder_track_relative(
                first_video,
                crate::TrackRelativePlacement::Before(first_video),
            )
            .is_err());
    }

    // ── ColorEngine / ProgramColorContext tests ───────────────────────────────

    #[test]
    fn nested_edge_policy_changes_working_semantics_without_changing_project_engine() {
        let environment = color_environment(ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        });
        let parent = SequenceSettings::default();
        let parent_context = parent
            .root_program_color_context(&environment)
            .expect("valid parent color context");
        let mut child = SequenceSettings::default();
        child.color.working_color_space = WorkingColorSpace::AcesCg;
        child.color.input.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;

        let preserved = child
            .nested_render_color_context(
                &parent_context,
                NestedColorProcessing::PreserveChildWorkingSpace,
            )
            .expect("valid preserved child context");
        assert_eq!(preserved.engine(), environment.engine());
        assert_eq!(preserved.working_color_space(), WorkingColorSpace::AcesCg);

        let forced = child
            .nested_render_color_context(
                &parent_context,
                NestedColorProcessing::ForceParentWorkingSpace,
            )
            .expect("valid forced child context");
        assert_eq!(forced.engine(), environment.engine());
        assert_eq!(
            forced.working_color_space(),
            parent_context.working_color_space()
        );
        assert_eq!(
            forced.missing_metadata_policy(),
            MissingColorMetadataPolicy::RejectMedia,
            "forcing the parent working space must not replace child media interpretation"
        );
    }

    #[test]
    fn root_context_rejects_invalid_working_space_without_prior_settings_validation() {
        let mut settings = SequenceSettings::default();
        settings.color.working_color_space = WorkingColorSpace::AcesCg;

        let error = settings
            .root_program_color_context(&standard_environment())
            .expect_err("root construction must validate the engine/working-space pair");

        assert!(
            matches!(error, ProgramColorContextError::InvalidWorkingSpace { .. }),
            "{error:#}"
        );
    }

    #[test]
    fn export_context_rejects_tone_map_and_intent_disagreement() {
        let root = SequenceSettings::default()
            .root_program_color_context(&standard_environment())
            .expect("valid root context");

        let view_without_tone_map = root
            .for_export_output(
                ColorSpace::Rec709,
                false,
                mondrian_core::OutputTransformIntent::mondrian_standard(),
            )
            .expect_err("a rendering View cannot execute with tone mapping disabled");
        assert!(matches!(
            view_without_tone_map,
            ProgramColorContextError::OutputTransformModeMismatch {
                tone_map: false,
                rendering_view: true,
                ..
            }
        ));

        let tone_map_without_view = root
            .for_export_output(
                ColorSpace::Rec709,
                true,
                mondrian_core::OutputTransformIntent::Colorimetric,
            )
            .expect_err("tone mapping requires a rendering View");
        assert!(matches!(
            tone_map_without_view,
            ProgramColorContextError::OutputTransformModeMismatch {
                tone_map: true,
                rendering_view: false,
                ..
            }
        ));
    }

    #[test]
    fn export_context_rejects_engine_intent_identity_drift() {
        let root = SequenceSettings::default()
            .root_program_color_context(&standard_environment())
            .expect("valid root context");

        let error = root
            .for_export_output(
                ColorSpace::Rec709,
                true,
                mondrian_core::OutputTransformIntent::aces_preset(
                    mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
                ),
            )
            .expect_err("an ACES intent cannot drift from the Standard engine");

        assert!(matches!(
            error,
            ProgramColorContextError::InvalidOutputTransform { .. }
        ));
    }

    #[test]
    fn nested_context_is_working_only_and_cannot_become_delivery_output() {
        let parent = SequenceSettings::default()
            .root_program_color_context(&standard_environment())
            .expect("valid parent context");
        let nested = SequenceSettings::default()
            .nested_render_color_context(&parent, NestedColorProcessing::ForceParentWorkingSpace)
            .expect("valid nested context");

        assert_eq!(
            nested.output_color_space(),
            OcioColorSpaceIdentity::Working(parent.working_color_space())
        );
        assert!(!nested.output_tone_map());
        assert_eq!(
            nested.output_transform(),
            &mondrian_core::OutputTransformIntent::Colorimetric
        );
        assert_eq!(
            nested
                .for_export_output(
                    ColorSpace::Rec709,
                    false,
                    mondrian_core::OutputTransformIntent::Colorimetric,
                )
                .expect_err("nested contexts cannot cross a delivery boundary"),
            ProgramColorContextError::EncodedOutputFromNestedContext
        );
    }

    #[test]
    fn nested_context_rejects_child_working_space_unsupported_by_parent_engine() {
        let parent = SequenceSettings::default()
            .root_program_color_context(&standard_environment())
            .expect("valid parent context");
        let mut child = SequenceSettings::default();
        child.color.working_color_space = WorkingColorSpace::AcesCg;

        let error = child
            .nested_render_color_context(&parent, NestedColorProcessing::PreserveChildWorkingSpace)
            .expect_err("nested construction must validate the child working space");

        assert!(matches!(
            error,
            ProgramColorContextError::InvalidWorkingSpace { .. }
        ));
    }

    #[test]
    fn resolved_context_uses_project_engine_without_persisting_it_in_sequence() {
        let settings = SequenceSettings::default();
        let environment = color_environment(pinned_custom_engine(OcioConfigSource::Environment));

        assert_eq!(
            settings
                .root_program_color_context(&environment)
                .expect("valid context")
                .engine(),
            environment.engine()
        );
    }

    #[test]
    fn output_tone_map_policy_is_sequence_author_semantics() {
        let environment = standard_environment();
        let mut settings = SequenceSettings::default();
        settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
        settings.color.input.auto_tone_map_media = false;

        assert!(
            settings
                .root_program_color_context(&environment)
                .expect("valid context")
                .output_tone_map(),
            "automatic policy maps scene-referred content to a display output"
        );
        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Never;
        assert!(!settings
            .root_program_color_context(&environment)
            .expect("valid context")
            .output_tone_map());
        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Always;
        assert!(settings
            .root_program_color_context(&environment)
            .expect("valid context")
            .output_tone_map());
    }

    #[test]
    fn media_input_tone_map_is_independent_from_program_output() {
        let environment = standard_environment();
        let mut settings = SequenceSettings::default();
        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Never;
        let program = settings.root_program_color_context(&environment).expect("valid context");

        assert!(!program.output_tone_map());
        assert!(program.media_input(true).input_tone_map);
        assert!(!program.media_input(false).input_tone_map);

        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Always;
        let program = settings.root_program_color_context(&environment).expect("valid context");
        assert!(program.output_tone_map());
        assert!(!program.media_input(false).input_tone_map);
    }

    #[test]
    fn display_referred_workflow_can_remain_colorimetric() {
        let mut settings = SequenceSettings::default();
        settings.color.input.auto_tone_map_media = false;
        settings.color.program_output.workflow = ColorWorkflow::DisplayReferred;
        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Automatic;

        let context = settings
            .root_program_color_context(&standard_environment())
            .expect("valid colorimetric context");
        assert!(!context.output_tone_map());
        assert_eq!(
            context.output_transform(),
            &mondrian_core::OutputTransformIntent::Colorimetric
        );
    }

    #[test]
    fn default_standard_video_workflow_resolves_standard_rec709_view() {
        let settings = SequenceSettings::default();
        let context = settings
            .root_program_color_context(&standard_environment())
            .expect("valid Standard context");

        assert_eq!(context.workflow(), ColorWorkflow::SceneReferred);
        assert_eq!(
            context.output_color_space(),
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709)
        );
        assert!(context.output_tone_map());
        assert_eq!(
            context.output_transform(),
            &mondrian_core::OutputTransformIntent::mondrian_standard()
        );
        assert_eq!(context.engine(), &ColorEngine::mondrian_standard());
    }

    #[test]
    fn custom_ocio_scene_output_uses_its_pinned_binding() {
        let settings = SequenceSettings::default();
        let environment = color_environment(pinned_custom_engine(OcioConfigSource::Environment));

        let context = settings
            .root_program_color_context(&environment)
            .expect("valid Custom OCIO context");

        assert!(context.output_tone_map());
        assert_eq!(
            context.output_transform(),
            &mondrian_core::OutputTransformIntent::CustomOcio {
                output_color_space: ColorSpace::Rec709,
            }
        );
    }

    #[test]
    fn custom_ocio_rejects_unpinned_output_and_working_space() {
        let mut settings = SequenceSettings::default();
        let environment = color_environment(pinned_custom_engine(OcioConfigSource::Environment));
        settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        let output_error = settings
            .validate_with_color_environment(&environment)
            .expect_err("Custom OCIO must reject an unpinned output binding");
        assert!(output_error.to_string().contains("output binding"));

        settings.color.program_output.color_space = ColorSpace::Rec709;
        settings.color.working_color_space = WorkingColorSpace::AcesCg;
        let working_error = settings
            .validate_with_color_environment(&environment)
            .expect_err("Custom OCIO must reject an unpinned working space");
        assert!(working_error.to_string().contains("pins working space 'Linear Rec.2020'"));
    }

    #[test]
    fn standard_rejects_unversioned_working_space_and_scene_view() {
        let environment = standard_environment();
        let mut settings = SequenceSettings::default();
        settings.color.working_color_space = WorkingColorSpace::LinearP3D65;
        let working_error = settings
            .validate_with_color_environment(&environment)
            .expect_err("Standard must reject a non-versioned working space");
        assert!(working_error.to_string().contains("Mondrian Standard"));

        settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
        settings.color.program_output.color_space = ColorSpace::Rec601Pal;
        let output_error = settings
            .validate_with_color_environment(&environment)
            .expect_err("scene-referred Standard must require a product View");
        assert!(
            output_error.to_string().contains("no rendering View"),
            "{output_error:#}"
        );

        settings.color.program_output.workflow = ColorWorkflow::DisplayReferred;
        settings
            .validate_with_color_environment(&environment)
            .expect("display-referred colorimetric output remains valid");
    }

    #[test]
    fn aces_resolves_target_specific_view_or_fails_closed() {
        let preset = mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25;
        let mut settings = SequenceSettings::default();
        let environment = color_environment(ColorEngine::Aces { preset });
        settings.color.program_output.color_space = ColorSpace::Rec2100Pq;

        let pq = settings
            .root_program_color_context(&environment)
            .expect("valid ACES PQ context");
        assert_eq!(
            pq.output_transform()
                .resolve_display_view(ColorSpace::Rec2100Pq, pq.engine())
                .expect("target-specific ACES PQ View")
                .expect("display/view"),
            (
                "Rec.2100-PQ - Display".to_owned(),
                "ACES 2.0 - HDR 1000 nits (Rec.2020)".to_owned(),
            )
        );

        settings.color.program_output.color_space = ColorSpace::Rec2020;
        let error = settings
            .validate_with_color_environment(&environment)
            .expect_err("ACES must not relabel its default Rec.709 View as Rec.2020 SDR");
        assert!(error.to_string().contains("no rendering View"), "{error:#}");
    }

    #[test]
    fn project_environment_persists_exact_standard_package_identity() {
        let legacy_package = mondrian_core::MondrianStandardPackageIdentity::V2;
        let settings = SequenceSettings::default();
        let environment =
            color_environment(ColorEngine::MondrianStandard { package: legacy_package });
        let context = settings
            .root_program_color_context(&environment)
            .expect("valid legacy Standard context");

        assert_eq!(context.engine(), environment.engine());
        assert_eq!(
            context.output_transform(),
            &mondrian_core::OutputTransformIntent::mondrian_standard_package(legacy_package)
        );
    }

    #[test]
    fn mondrian_standard_output_intent_resolves_each_supported_delivery_target() {
        let environment = standard_environment();
        for (target, expected_display, expected_view) in [
            (
                ColorSpace::DisplayP3,
                "Display P3 - Display",
                "Mondrian Standard SDR v2",
            ),
            (
                ColorSpace::Rec2020,
                "Rec.2020 SDR - Display",
                "Mondrian Standard SDR v2",
            ),
            (
                ColorSpace::Rec2100Pq,
                "Rec.2100-PQ - Display",
                "Mondrian Standard HDR 1000 nits v1",
            ),
            (
                ColorSpace::Rec2100Hlg,
                "Rec.2100-HLG - Display",
                "Mondrian Standard HDR 1000 nits v1",
            ),
        ] {
            let intent = mondrian_core::OutputTransformIntent::mondrian_standard();
            assert_eq!(
                intent
                    .resolve_display_view(target, environment.engine())
                    .expect("supported Standard target"),
                Some((expected_display.to_owned(), expected_view.to_owned()))
            );
        }
    }
}

#[cfg(test)]
mod clip_mutation_tests {
    use super::*;
    use crate::clip::Clip;
    use mondrian_core::{AssetId, FramePosition, Rational};

    fn at(tb: Rational, frame: i64) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, tb)).expect("test time")
    }

    fn sequence_with_two_video_clips() -> (Sequence, ClipId, ClipId) {
        let mut sequence = Sequence::new("mutation-tests");
        let tb = sequence.time_base();
        let first = Clip::new(AssetId::new(), at(tb, 0), at(tb, 10)).expect("Clip");
        let second = Clip::new(AssetId::new(), at(tb, 10), at(tb, 10)).expect("Clip");
        let first_id = first.id;
        let second_id = second.id;
        sequence.video_tracks[0].clips.push(first);
        sequence.video_tracks[0].clips.push(second);
        (sequence, first_id, second_id)
    }

    #[test]
    fn find_and_locate_clips_across_track_kinds() {
        let (mut sequence, first_id, second_id) = sequence_with_two_video_clips();
        let tb = sequence.time_base();
        let audio_clip = Clip::new(AssetId::new(), at(tb, 0), at(tb, 5)).expect("Clip");
        let audio_id = audio_clip.id;
        sequence.audio_tracks[0].clips.push(audio_clip);

        assert_eq!(
            sequence.find_clip(first_id).map(|clip| clip.id),
            Some(first_id)
        );
        assert_eq!(
            sequence.find_clip(audio_id).map(|clip| clip.id),
            Some(audio_id)
        );
        assert!(sequence.find_clip(ClipId::new()).is_none());

        let video_location = sequence.clip_track_location(second_id).expect("video location");
        assert!(video_location.is_video_track);
        assert_eq!(video_location.track_index, 0);
        assert!(!video_location.is_locked);
        let audio_location = sequence.clip_track_location(audio_id).expect("audio location");
        assert!(!audio_location.is_video_track);

        sequence.find_clip_mut(first_id).expect("mut").is_disabled = true;
        assert!(sequence.find_clip(first_id).expect("readback").is_disabled);
    }

    #[test]
    fn remove_disable_and_reposition_clips() {
        let (mut sequence, first_id, second_id) = sequence_with_two_video_clips();
        let tb = sequence.time_base();

        assert!(sequence.set_clip_disabled(first_id, true));
        assert!(
            !sequence.set_clip_disabled(first_id, true),
            "no-op reports unchanged"
        );
        assert!(sequence.find_clip(first_id).expect("Clip").is_disabled);

        assert!(sequence.set_clip_position(second_id, at(tb, 20)));
        assert!(!sequence.set_clip_position(second_id, at(tb, 20)));
        assert_eq!(
            sequence.find_clip(second_id).expect("Clip").position,
            at(tb, 20)
        );

        let removed = sequence.remove_clip_anywhere(first_id).expect("removed");
        assert_eq!(removed.id, first_id);
        assert!(sequence.find_clip(first_id).is_none());
        assert!(sequence.remove_clip_anywhere(first_id).is_none());
    }

    #[test]
    fn move_clip_between_tracks_at_time() {
        let (mut sequence, first_id, _second_id) = sequence_with_two_video_clips();
        let tb = sequence.time_base();
        sequence.add_video_track();

        assert!(sequence.move_clip_to_track_at_time(true, first_id, 1, at(tb, 40)));
        let location = sequence.clip_track_location(first_id).expect("new location");
        assert_eq!(location.track_index, 1);
        assert_eq!(
            sequence.find_clip(first_id).expect("Clip").position,
            at(tb, 40)
        );
        assert!(sequence.video_tracks[0].clips.len() == 1);

        // Same-Track move updates the position in place.
        assert!(sequence.move_clip_to_track_at_time(true, first_id, 1, at(tb, 45)));
        assert_eq!(
            sequence.find_clip(first_id).expect("Clip").position,
            at(tb, 45)
        );

        // Out-of-range targets and kind mismatches are rejected without mutation.
        assert!(!sequence.move_clip_to_track_at_time(true, first_id, 9, at(tb, 0)));
        assert!(!sequence.move_clip_to_track_at_time(false, first_id, 0, at(tb, 0)));
    }

    #[test]
    fn ensure_audio_track_index_grows_only_as_needed() {
        let mut sequence = Sequence::new("audio-track-tests");
        let existing = sequence.audio_tracks.len();
        sequence.ensure_audio_track_index(existing + 1);
        assert_eq!(sequence.audio_tracks.len(), existing + 2);
        sequence.ensure_audio_track_index(0);
        assert_eq!(sequence.audio_tracks.len(), existing + 2);
    }
}
