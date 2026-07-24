//! 序列（时间线）

use crate::{clip::ActiveClip, track::Track};
pub use mondrian_core::AudioChannelLayout;
use mondrian_core::{
    types::*, DisplayToneMapPolicy, SmpteCountingMode, TimelineDisplayContract,
    TimelineDisplayFormat, TimelineDisplaySettings, TimelineTime, VideoContentLightMetadata,
    VideoMasteringDisplayMetadata,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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

// Re-exported from mondrian_core::timeline_data.
use mondrian_core::timeline_data::{AssetMediaInterpretation, MediaColorInterpretation};
pub use mondrian_core::timeline_data::{FieldOrder, PixelAspectRatio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioDisplayFormat {
    #[default]
    AudioSamples,
    Milliseconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewRenderFormat {
    #[default]
    IFrameOnly,
    ProResProxy,
    DnxHrLb,
    LosslessRgba,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum MissingColorMetadataPolicy {
    /// 无标签素材视为 Rec.709（行业默认）。
    #[default]
    AssumeRec709,
    /// 无标签素材拒绝导入 / 跳过渲染。
    RejectMedia,
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
    /// Explicitly detected media metadata color space.
    pub detected_color_space: Option<ColorSpace>,
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
    /// 根据策略和检测到的色彩空间，解析有效的输入色彩空间。
    ///
    /// `detected` 来自媒体探测（FFmpeg 标签），`working` 是序列工作空间。
    /// 当素材无色彩标签时（`detected == None`），按策略行事。
    /// Resolve the effective input color space and retain the decision branch for diagnostics.
    pub fn resolve_input_decision(
        self,
        override_color_space: Option<ColorSpace>,
        detected: Option<ColorSpace>,
        working: WorkingColorSpace,
    ) -> InputColorResolution {
        if let Some(color_space) = override_color_space {
            return InputColorResolution {
                resolved: ResolvedInputColor::Color(color_space),
                source: InputColorResolutionSource::Override,
                override_color_space,
                detected_color_space: detected,
                missing_metadata_policy: self,
                working_color_space: working,
            };
        }
        if let Some(color_space) = detected {
            return InputColorResolution {
                resolved: ResolvedInputColor::Color(color_space),
                source: InputColorResolutionSource::DetectedMetadata,
                override_color_space,
                detected_color_space: detected,
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
            detected_color_space: detected,
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
        detected: Option<ColorSpace>,
        working: WorkingColorSpace,
    ) -> InputColorResolution {
        if asset_interpretation.payload.is_non_color_data() {
            return InputColorResolution {
                resolved: ResolvedInputColor::Data,
                source: InputColorResolutionSource::DataTexture,
                override_color_space: None,
                detected_color_space: detected,
                missing_metadata_policy: self,
                working_color_space: working,
            };
        }
        if clip_override_color_space.is_some() {
            return self.resolve_input_decision(clip_override_color_space, detected, working);
        }
        match asset_interpretation.color {
            MediaColorInterpretation::Auto => self.resolve_input_decision(None, detected, working),
            MediaColorInterpretation::Override { color_space } => {
                self.resolve_input_decision(Some(color_space), detected, working)
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

/// Default media-input policy for Timeline contributions in one Sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SequenceInputColorSettings {
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    #[serde(default)]
    pub auto_tone_map_media: bool,
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
    pub working_color_space: WorkingColorSpace,
    pub output_color_space: OcioColorSpaceIdentity,
    /// Whether the working-to-Program-Output boundary applies tone mapping.
    pub output_tone_map: bool,
    pub workflow: ColorWorkflow,
    pub engine: ColorEngine,
    /// Sequence input interpretation retained only to derive per-media input
    /// contexts while traversing nested Timelines; it never changes Program
    /// Output pixels by itself.
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    /// Product-level final output transform selected for this context.
    pub output_transform: mondrian_core::OutputTransformIntent,
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
    /// Derive the source-to-working contract for one media contribution.
    pub fn media_input(&self, input_tone_map: bool) -> MediaInputColorContext {
        MediaInputColorContext {
            working_color_space: self.working_color_space,
            input_tone_map,
            engine: self.engine.clone(),
            missing_metadata_policy: self.missing_metadata_policy,
        }
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
            .engine
            .validate_working_space(self.color.working_color_space)
            .map_err(|reason| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_color_settings_validate".to_owned(),
                reason,
            })?;

        let output_context = self.root_color_context_for_output(
            color_environment,
            self.color.program_output.color_space,
        );
        output_context
            .output_transform
            .resolve_display_view(
                self.color.program_output.color_space,
                &output_context.engine,
            )
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
    ) -> ProgramColorContext {
        self.root_color_context_for_output(color_environment, self.color.program_output.color_space)
    }

    fn root_color_context_for_output(
        &self,
        color_environment: &mondrian_core::ProjectColorEnvironment,
        output_color_space: ColorSpace,
    ) -> ProgramColorContext {
        let engine = color_environment.engine.clone();

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
        let output_transform = match (&engine, tone_map) {
            (ColorEngine::MondrianStandard { package }, true) => {
                mondrian_core::OutputTransformIntent::mondrian_standard_package(*package)
            }
            (ColorEngine::Aces { preset }, true) => {
                mondrian_core::OutputTransformIntent::aces_preset(*preset)
            }
            (ColorEngine::CustomOcio { .. }, true) => {
                mondrian_core::OutputTransformIntent::CustomOcio { output_color_space }
            }
            _ => mondrian_core::OutputTransformIntent::Colorimetric,
        };

        ProgramColorContext {
            working_color_space: self.color.working_color_space,
            output_color_space: OcioColorSpaceIdentity::Color(output_color_space),
            output_tone_map: tone_map,
            engine,
            missing_metadata_policy: self.color.input.missing_metadata_policy,
            output_transform,
            workflow: self.color.program_output.workflow,
        }
    }

    pub fn nested_render_color_context(
        &self,
        parent: ProgramColorContext,
        processing: NestedColorProcessing,
    ) -> ProgramColorContext {
        match processing {
            NestedColorProcessing::PreserveChildWorkingSpace => ProgramColorContext {
                working_color_space: self.color.working_color_space,
                output_color_space: OcioColorSpaceIdentity::Working(parent.working_color_space),
                output_tone_map: false,
                engine: parent.engine.clone(),
                missing_metadata_policy: self.color.input.missing_metadata_policy,
                output_transform: mondrian_core::OutputTransformIntent::Colorimetric,
                workflow: self.color.program_output.workflow,
            },
            NestedColorProcessing::ForceParentWorkingSpace => ProgramColorContext {
                working_color_space: parent.working_color_space,
                output_color_space: OcioColorSpaceIdentity::Working(parent.working_color_space),
                output_tone_map: false,
                engine: parent.engine.clone(),
                missing_metadata_policy: self.color.input.missing_metadata_policy,
                output_transform: mondrian_core::OutputTransformIntent::Colorimetric,
                workflow: parent.workflow,
            },
        }
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    pub id: SequenceId,
    /// Monotonic authoring transaction revision for this stable Sequence ID.
    pub revision: SequenceRevision,
    pub name: String,
    pub role: SequenceRole,
    pub settings: SequenceSettings,
    pub video_tracks: Vec<Track>,
    /// Explicit two-input visual Transitions. Endpoint Track membership is
    /// derived from their strong Clip references.
    pub video_transitions: Vec<crate::video_transition::VideoTransition>,
    pub audio_tracks: Vec<Track>,
    /// Sequence semantic catalog for audio classification and output projection.
    pub audio_roles: Vec<crate::audio::AudioRole>,
    /// Sequence-owned audio processing, routing, transitions, and public outputs.
    pub audio_program: crate::audio::AudioProgram,
    pub playhead: TimelineTime,
    pub in_point: Option<TimelineTime>,
    pub out_point: Option<TimelineTime>,
}

impl Sequence {
    pub fn new(name: impl Into<String>) -> Self {
        let settings = SequenceSettings::default();
        let audio_tracks = vec![
            Track::new_audio("A1"),
            Track::new_audio("A2"),
            Track::new_audio("A3"),
        ];
        let audio_program =
            crate::audio::AudioProgram::for_tracks(audio_tracks.iter().map(|track| track.id));
        Self {
            id: SequenceId::new(),
            revision: SequenceRevision::INITIAL,
            name: name.into(),
            role: SequenceRole::Editorial,
            video_tracks: vec![
                Track::new_video("V1"),
                Track::new_video("V2"),
                Track::new_video("V3"),
            ],
            video_transitions: Vec::new(),
            audio_tracks,
            audio_roles: Vec::new(),
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
                let source_time = clip.timeline_to_source_time(time)?;
                let transform_mat = clip.transform.evaluate_matrix(clip_time);
                let opacity =
                    (clip.transform.evaluate_opacity(clip_time) * track_opacity).clamp(0.0, 1.0);
                result.push(ActiveClip {
                    clip: clip.clone(),
                    track_index: i,
                    clip_time,
                    source_time,
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
        if clip.is_nested_sequence() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "add_media_audio_clip".to_owned(),
                reason: "nested Sequence audio requires an explicit output binding".to_owned(),
            });
        }
        if !clip.audio_components.is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "add_media_audio_clip".to_owned(),
                reason: "audio Clip already contains audio component authoring".to_owned(),
            });
        }
        let scope = crate::audio::AudioProcessingScope::identity();
        clip.audio_components.push(crate::audio::AudioComponentEdit::media(
            component_id,
            scope.id,
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
        for route in &mut self.audio_program.routes {
            route.id = AudioRouteId::new();
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

        for transition in &mut self.video_transitions {
            transition.left = clip_ids[&transition.left];
            transition.right = clip_ids[&transition.right];
            transition.fork_author_identities();
        }

        self.fork_audio_identities_for_sequence_duplicate();
        self.fork_clip_link_groups_for_sequence_duplicate();
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
        for clip in self
            .video_tracks
            .iter_mut()
            .chain(&mut self.audio_tracks)
            .flat_map(|track| &mut track.clips)
        {
            if clip.link_group.is_some_and(|group| counts.get(&group) == Some(&1)) {
                clip.link_group = None;
            }
        }
    }

    /// Remove visual Transitions whose strong endpoints or edit geometry no
    /// longer exist after a structural edit.
    pub fn compact_video_transitions(&mut self) {
        let video_tracks = &self.video_tracks;
        self.video_transitions
            .retain(|transition| validate_video_transition(video_tracks, transition).is_ok());
    }

    /// Restore every derived author-graph invariant after a structural edit.
    pub fn compact_structural_references(&mut self) {
        self.compact_clip_link_groups();
        self.compact_video_transitions();
        self.compact_audio_program();
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

    pub fn move_video_track(&mut self, id: TrackId, new_index: usize) -> mondrian_core::Result<()> {
        move_track_in_list(&mut self.video_tracks, id, new_index)?;
        self.normalize_track_names();
        Ok(())
    }

    pub fn move_audio_track(&mut self, id: TrackId, new_index: usize) -> mondrian_core::Result<()> {
        move_track_in_list(&mut self.audio_tracks, id, new_index)?;
        self.normalize_track_names();
        Ok(())
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

fn validate_video_transition(
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
        use mondrian_core::timeline_data::{
            FlatTransitionProgress, FlatVideoTransition, FlatVideoTransitionDefinition,
            FlatVisualItem,
        };

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
                let definition = match &transition.transition_type {
                    crate::video_transition::VideoTransitionType::CrossDissolve => {
                        FlatVideoTransitionDefinition::CrossDissolve
                    }
                    crate::video_transition::VideoTransitionType::Plugin { definition_id } => {
                        FlatVideoTransitionDefinition::Plugin {
                            definition_id: definition_id.clone(),
                        }
                    }
                };
                items.push(FlatVisualItem::Transition(Box::new(FlatVideoTransition {
                    transition_id: transition.id,
                    definition,
                    left: flatten_visual_clip(left, track, track_index, track_opacity, time)?,
                    right: flatten_visual_clip(right, track, track_index, track_opacity, time)?,
                    progress: FlatTransitionProgress {
                        elapsed: time.checked_sub(transition.sequence_range.start)?,
                        duration: transition.sequence_range.duration,
                    },
                    properties: transition.properties.clone(),
                    params: transition.params.clone(),
                })));
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

    fn source_time_base(&self) -> mondrian_core::types::Rational {
        Sequence::time_base(self)
    }

    fn auto_tone_map_media(&self) -> bool {
        self.settings.color.input.auto_tone_map_media
    }
}

fn flatten_visual_clip(
    clip: &crate::clip::Clip,
    track: &Track,
    track_index: usize,
    track_opacity: f32,
    time: TimelineTime,
) -> mondrian_core::Result<mondrian_core::timeline_data::FlatActiveClip> {
    let clip_time = clip.timeline_to_clip_time(time)?;
    let matrix = clip.transform.evaluate_matrix(clip_time);
    Ok(mondrian_core::timeline_data::FlatActiveClip {
        clip_id: clip.id,
        content: clip.content.clone(),
        is_disabled: clip.is_disabled,
        effects: clip.effects.clone(),
        masks: clip.masks.clone(),
        clip_time,
        source_time: clip.timeline_to_source_time(time)?,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceCollection {
    pub sequences: Vec<Sequence>,
    pub default_sequence_id: SequenceId,
    pub active_sequence_id: SequenceId,
}

impl SequenceCollection {
    pub fn new(default_sequence: Sequence) -> Self {
        let id = default_sequence.id;
        Self {
            sequences: vec![default_sequence],
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

    pub fn validate_nested_sequences(&self) -> mondrian_core::Result<()> {
        let sequence_ids: HashSet<SequenceId> = self.sequences.iter().map(|seq| seq.id).collect();
        if sequence_ids.len() != self.sequences.len() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "validate_author_identities".to_owned(),
                reason: "duplicate Sequence identity in project document".to_owned(),
            });
        }
        for sequence in &self.sequences {
            sequence.validate_author_identities()?;
            sequence
                .audio_program
                .validate(
                    &sequence.audio_tracks,
                    &sequence.audio_roles,
                    sequence.settings.audio_channel_layout,
                )
                .map_err(|error| mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "validate_audio_program".to_owned(),
                    reason: format!(
                        "Sequence {} has invalid audio authoring: {error}",
                        sequence.id
                    ),
                })?;
            for clip in sequence.audio_tracks.iter().flat_map(|track| &track.clips) {
                for edit in &clip.audio_components {
                    let crate::audio::AudioComponentSource::NestedOutput { output_id } =
                        edit.source
                    else {
                        continue;
                    };
                    let Some(sequence_id) = clip.nested_sequence_id() else {
                        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                            step_id: "validate_audio_program".to_owned(),
                            reason: format!("nested audio edit {} has no owning Sequence", edit.id),
                        });
                    };
                    let Some(child) = self.sequence(sequence_id) else {
                        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                            step_id: "validate_audio_program".to_owned(),
                            reason: format!("nested audio Sequence does not exist: {sequence_id}"),
                        });
                    };
                    if !child.audio_program.outputs.iter().any(|output| output.id == output_id) {
                        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                            step_id: "validate_audio_program".to_owned(),
                            reason: format!("nested audio output does not exist: {output_id}"),
                        });
                    }
                    if let crate::audio::AudioComponentChannelMapping::Explicit(matrix) =
                        &edit.channel_mapping
                    {
                        if matrix.source_layout() != child.settings.audio_channel_layout {
                            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                                step_id: "validate_audio_program".to_owned(),
                                reason: format!(
                                    "nested audio edit {} matrix source layout does not match child Sequence {}",
                                    edit.id, sequence_id
                                ),
                            });
                        }
                    }
                }
            }
        }
        let graph = self.nested_sequence_graph();
        for nested_id in graph.values().flatten() {
            if !sequence_ids.contains(nested_id) {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "validate_nested_sequences".to_string(),
                    reason: format!("嵌套序列不存在: {nested_id}"),
                });
            }
        }
        for root in &sequence_ids {
            let mut visiting = HashSet::new();
            let mut visited = HashSet::new();
            if has_cycle(*root, &graph, &mut visiting, &mut visited) {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "validate_nested_sequences".to_string(),
                    reason: format!("检测到序列嵌套循环: {root}"),
                });
            }
        }
        Ok(())
    }

    fn nested_sequence_graph(&self) -> HashMap<SequenceId, Vec<SequenceId>> {
        self.sequences
            .iter()
            .map(|seq| {
                let nested = seq
                    .video_tracks
                    .iter()
                    .chain(seq.audio_tracks.iter())
                    .flat_map(|track| track.clips.iter())
                    .filter_map(|clip| clip.nested_sequence_id())
                    .collect();
                (seq.id, nested)
            })
            .collect()
    }
}

fn has_cycle(
    node: SequenceId,
    graph: &HashMap<SequenceId, Vec<SequenceId>>,
    visiting: &mut HashSet<SequenceId>,
    visited: &mut HashSet<SequenceId>,
) -> bool {
    if visited.contains(&node) {
        return false;
    }
    if !visiting.insert(node) {
        return true;
    }
    for child in graph.get(&node).into_iter().flatten() {
        if has_cycle(*child, graph, visiting, visited) {
            return true;
        }
    }
    visiting.remove(&node);
    visited.insert(node);
    false
}

fn move_track_in_list(
    tracks: &mut Vec<Track>,
    id: TrackId,
    new_index: usize,
) -> mondrian_core::Result<()> {
    let current_index = tracks
        .iter()
        .position(|track| track.id == id)
        .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })?;

    let clamped_index = new_index.min(tracks.len().saturating_sub(1));
    if current_index == clamped_index {
        return Ok(());
    }

    let track = tracks.remove(current_index);
    tracks.insert(clamped_index, track);
    Ok(())
}

fn renumber_tracks(tracks: &mut [Track], prefix: &str) {
    for (index, track) in tracks.iter_mut().enumerate() {
        track.name = format!("{prefix}{}", index + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::Clip;
    use mondrian_core::automation::{Keyframe, PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::effect_data::EffectType;
    use mondrian_core::DisplayToneMapPolicy;

    fn pinned_custom_engine(source: OcioConfigSource) -> ColorEngine {
        ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    source,
                    "0".repeat(64),
                    "test-resolved-config".to_owned(),
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
        mondrian_core::ProjectColorEnvironment { engine }
    }

    fn standard_environment() -> mondrian_core::ProjectColorEnvironment {
        mondrian_core::ProjectColorEnvironment::default()
    }

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("valid test time")
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
        left.add_effect(EffectType::GaussianBlur);
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
            .validate_nested_sequences()
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
            override_resolution.detected_color_space,
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
        clip.source_in = tt(100, tb);
        clip.source_out = tt(120, tb);
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
        assert_eq!(active[0].source_time, tt(110, tb));
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

        assert!(collection.validate_nested_sequences().is_err());
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
        let moved_id = seq.video_tracks[2].id;
        let clip = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
        let clip_id = clip.id;
        seq.video_tracks[2].add_clip(clip).expect("add clip to track");

        seq.move_video_track(moved_id, 0).expect("move track to top");

        assert_eq!(seq.video_tracks[0].id, moved_id);
        assert_eq!(seq.video_tracks[0].name, "V1");
        assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
        assert_eq!(seq.video_tracks[1].name, "V2");
        assert_eq!(seq.video_tracks[2].name, "V3");
    }

    // ── ColorEngine / ProgramColorContext tests ───────────────────────────────

    #[test]
    fn nested_edge_policy_changes_working_semantics_without_changing_project_engine() {
        let environment = color_environment(ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        });
        let parent = SequenceSettings::default();
        let parent_context = parent.root_program_color_context(&environment);
        let mut child = SequenceSettings::default();
        child.color.working_color_space = WorkingColorSpace::AcesCg;
        child.color.input.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;

        let preserved = child.nested_render_color_context(
            parent_context.clone(),
            NestedColorProcessing::PreserveChildWorkingSpace,
        );
        assert_eq!(preserved.engine, environment.engine);
        assert_eq!(preserved.working_color_space, WorkingColorSpace::AcesCg);

        let forced = child.nested_render_color_context(
            parent_context.clone(),
            NestedColorProcessing::ForceParentWorkingSpace,
        );
        assert_eq!(forced.engine, environment.engine);
        assert_eq!(
            forced.working_color_space,
            parent_context.working_color_space
        );
        assert_eq!(
            forced.missing_metadata_policy,
            MissingColorMetadataPolicy::RejectMedia,
            "forcing the parent working space must not replace child media interpretation"
        );
    }

    #[test]
    fn resolved_context_uses_project_engine_without_persisting_it_in_sequence() {
        let settings = SequenceSettings::default();
        let environment = color_environment(pinned_custom_engine(OcioConfigSource::Environment));

        assert_eq!(
            settings.root_program_color_context(&environment).engine,
            environment.engine
        );
    }

    #[test]
    fn output_tone_map_policy_is_sequence_author_semantics() {
        let environment = standard_environment();
        let mut settings = SequenceSettings::default();
        settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
        settings.color.input.auto_tone_map_media = false;

        assert!(
            settings.root_program_color_context(&environment).output_tone_map,
            "automatic policy maps scene-referred content to a display output"
        );
        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Never;
        assert!(!settings.root_program_color_context(&environment).output_tone_map);
        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Always;
        assert!(settings.root_program_color_context(&environment).output_tone_map);
    }

    #[test]
    fn media_input_tone_map_is_independent_from_program_output() {
        let environment = standard_environment();
        let mut settings = SequenceSettings::default();
        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Never;
        let program = settings.root_program_color_context(&environment);

        assert!(!program.output_tone_map);
        assert!(program.media_input(true).input_tone_map);
        assert!(!program.media_input(false).input_tone_map);

        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Always;
        let program = settings.root_program_color_context(&environment);
        assert!(program.output_tone_map);
        assert!(!program.media_input(false).input_tone_map);
    }

    #[test]
    fn display_referred_workflow_can_remain_colorimetric() {
        let mut settings = SequenceSettings::default();
        settings.color.input.auto_tone_map_media = false;
        settings.color.program_output.workflow = ColorWorkflow::DisplayReferred;
        settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Automatic;

        let context = settings.root_program_color_context(&standard_environment());
        assert!(!context.output_tone_map);
        assert_eq!(
            context.output_transform,
            mondrian_core::OutputTransformIntent::Colorimetric
        );
    }

    #[test]
    fn default_standard_video_workflow_resolves_standard_rec709_view() {
        let settings = SequenceSettings::default();
        let context = settings.root_program_color_context(&standard_environment());

        assert_eq!(context.workflow, ColorWorkflow::SceneReferred);
        assert_eq!(
            context.output_color_space,
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709)
        );
        assert!(context.output_tone_map);
        assert_eq!(
            context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
        assert_eq!(context.engine, ColorEngine::mondrian_standard());
    }

    #[test]
    fn custom_ocio_scene_output_uses_its_pinned_binding() {
        let settings = SequenceSettings::default();
        let environment = color_environment(pinned_custom_engine(OcioConfigSource::Environment));

        let context = settings.root_program_color_context(&environment);

        assert!(context.output_tone_map);
        assert_eq!(
            context.output_transform,
            mondrian_core::OutputTransformIntent::CustomOcio {
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

        let pq = settings.root_program_color_context(&environment);
        assert_eq!(
            pq.output_transform
                .resolve_display_view(ColorSpace::Rec2100Pq, &pq.engine)
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
        let context = settings.root_program_color_context(&environment);

        assert_eq!(context.engine, environment.engine);
        assert_eq!(
            context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard_package(legacy_package)
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
                    .resolve_display_view(target, &environment.engine)
                    .expect("supported Standard target"),
                Some((expected_display.to_owned(), expected_view.to_owned()))
            );
        }
    }
}
