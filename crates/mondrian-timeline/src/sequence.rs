//! 序列（时间线）

use crate::{clip::ActiveClip, track::Track};
pub use mondrian_core::AudioChannelLayout;
use mondrian_core::{
    types::*, DisplayManagementPolicy, SmpteCountingMode, TimelineDisplayContract,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SequenceColorManagement {
    #[serde(default)]
    pub workflow: ColorWorkflow,
    /// `true` 时使用项目级色彩管理设置（引擎 + 策略）。
    /// `false` 时使用此结构体中的独立设置。
    #[serde(default = "default_inherit_color_management")]
    pub inherit: bool,
    #[serde(default)]
    pub engine: ColorEngine,
    #[serde(default)]
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    #[serde(default)]
    pub nested_processing: NestedColorProcessing,
    /// Display/monitor/tone-map policy used when this sequence does not inherit
    /// project-level color management.
    #[serde(default)]
    pub display_management: DisplayManagementPolicy,
    #[serde(default = "default_output_color_space")]
    pub output_color_space: ColorSpace,
    #[serde(default)]
    pub video_range: VideoRange,
    #[serde(default)]
    /// Actual encoded sample depth of the deliverable.
    pub delivery_bit_depth: DeliveryBitDepth,
    /// Whether export omits static HDR metadata or writes the explicitly
    /// authored delivery values below.
    #[serde(default)]
    pub static_hdr_metadata_policy: StaticHdrMetadataPolicy,
    /// HDR mastering-display color volume (SMPTE ST 2086).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_mastering_display: Option<VideoMasteringDisplayMetadata>,
    /// HDR content light level metadata (MaxCLL / MaxFALL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_content_light: Option<VideoContentLightMetadata>,
}

/// Project-level policy for static HDR delivery metadata.
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

/// 渲染色彩上下文 —— 单帧渲染所需的全部色彩信息。
///
/// 由序列设置 + 项目设置合并生成，贯穿整个渲染管线。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorContext {
    pub working_color_space: WorkingColorSpace,
    pub output_color_space: OcioColorSpaceIdentity,
    pub tone_map: bool,
    pub workflow: ColorWorkflow,
    pub nested_processing: NestedColorProcessing,
    pub engine: ColorEngine,
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    /// Resolved display-management policy for this render context.
    pub display_management: DisplayManagementPolicy,
    /// Product-level final output transform selected for this context.
    pub output_transform: mondrian_core::OutputTransformIntent,
}

impl Default for SequenceColorManagement {
    fn default() -> Self {
        Self {
            workflow: ColorWorkflow::SceneReferred,
            inherit: default_inherit_color_management(),
            engine: ColorEngine::default(),
            missing_metadata_policy: MissingColorMetadataPolicy::AssumeRec709,
            nested_processing: NestedColorProcessing::PreserveChildWorkingSpace,
            display_management: DisplayManagementPolicy::default(),
            output_color_space: ColorSpace::Rec709,
            video_range: VideoRange::Full,
            delivery_bit_depth: DeliveryBitDepth::Ten,
            static_hdr_metadata_policy: StaticHdrMetadataPolicy::Omit,
            hdr_mastering_display: None,
            hdr_content_light: None,
        }
    }
}

const fn default_inherit_color_management() -> bool {
    true
}

const fn default_output_color_space() -> ColorSpace {
    ColorSpace::Rec709
}

/// 序列设置（帧率/分辨率/音频配置）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    pub working_color_space: WorkingColorSpace,
    #[serde(default)]
    pub auto_tone_map_media: bool,
    /// Action-safe margin as fraction of frame (0.10 = 10% total, 5% per side).
    #[serde(default = "default_action_safe_margin")]
    pub action_safe_margin: f32,
    /// Title-safe margin as fraction of frame (0.20 = 20% total, 10% per side).
    #[serde(default = "default_title_safe_margin")]
    pub title_safe_margin: f32,
    #[serde(default)]
    pub color_management: SequenceColorManagement,
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
            working_color_space: WorkingColorSpace::LinearRec2020,
            auto_tone_map_media: true,
            action_safe_margin: default_action_safe_margin(),
            title_safe_margin: default_title_safe_margin(),
            color_management: SequenceColorManagement::default(),
        }
    }
}

impl SequenceSettings {
    pub const AUDIO_SAMPLE_RATES: [u32; 5] = [32_000, 44_100, 48_000, 88_200, 96_000];
    pub const MIN_WIDTH: u32 = 16;
    pub const MIN_HEIGHT: u32 = 16;
    pub const MAX_WIDTH: u32 = 16_384;
    pub const MAX_HEIGHT: u32 = 16_384;

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
        if !self.color_management.output_color_space.is_display_referred() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "序列输出必须是显示或交付色彩空间，不能使用场景线性或 Log 输入空间"
                    .to_string(),
            });
        }
        if self.color_management.static_hdr_metadata_policy.writes_authored_metadata()
            && !self.color_management.output_color_space.is_hdr()
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "只有 HDR 输出色彩空间可以写入静态 HDR metadata".to_string(),
            });
        }
        if self.color_management.static_hdr_metadata_policy.writes_authored_metadata() {
            let mastering =
                self.color_management.hdr_mastering_display.as_ref().ok_or_else(|| {
                    mondrian_core::MondrianError::WorkflowStepFailed {
                        step_id: "sequence_settings_validate".to_string(),
                        reason: "写入静态 HDR metadata 需要 SMPTE ST 2086 母版显示元数据"
                            .to_string(),
                    }
                })?;
            mastering.validate().map_err(|error| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "sequence_settings_validate".to_string(),
                    reason: format!("HDR mastering metadata 无效: {error}"),
                }
            })?;
            let content_light = self.color_management.hdr_content_light.ok_or_else(|| {
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

    /// Validate sequence settings together with the effective project color mode.
    ///
    /// This catches working-space mismatches and output intents that the exact
    /// selected engine/package cannot resolve before render planning.
    pub fn validate_with_project_color_management(
        &self,
        project_cm: &mondrian_core::ProjectColorManagement,
    ) -> mondrian_core::Result<()> {
        self.validate()?;
        let engine = if self.color_management.inherit {
            &project_cm.engine
        } else {
            &self.color_management.engine
        };
        engine.validate_working_space(self.working_color_space).map_err(|reason| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_color_management_validate".to_owned(),
                reason,
            }
        })?;

        let output_context = self
            .root_color_context_for_output(project_cm, self.color_management.output_color_space);
        output_context
            .output_transform
            .resolve_display_view(
                self.color_management.output_color_space,
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
    /// define the program before any local monitor adaptation. When the sequence
    /// inherits color management from the project (`color_management.inherit == true`),
    /// the `engine` is taken from `project_cm` instead of per-sequence settings.
    ///
    /// The effective engine owns the single typed `output_transform` intent;
    /// no second display/view policy may replace it.
    pub fn root_program_color_context(
        &self,
        project_cm: &mondrian_core::ProjectColorManagement,
    ) -> ColorContext {
        self.root_color_context_for_output(project_cm, self.color_management.output_color_space)
    }

    /// Build the presentation color context for root sequence preview.
    ///
    /// Preview uses the display/output color space supplied by the caller rather
    /// than the sequence export output, so monitor presentation can evolve
    /// independently from delivery encoding.
    pub fn root_preview_color_context(
        &self,
        project_cm: &mondrian_core::ProjectColorManagement,
        display_color_space: ColorSpace,
    ) -> ColorContext {
        self.root_color_context_for_output(project_cm, display_color_space)
    }

    fn root_color_context_for_output(
        &self,
        project_cm: &mondrian_core::ProjectColorManagement,
        output_color_space: ColorSpace,
    ) -> ColorContext {
        let engine = if self.color_management.inherit {
            project_cm.engine.clone()
        } else {
            self.color_management.engine.clone()
        };
        let display_management = if self.color_management.inherit {
            project_cm.display_management.clone()
        } else {
            self.color_management.display_management.clone()
        };

        // Scene-referred workflows need the selected project engine's view
        // transform at a display-referred output. The engine alone decides
        // whether that view is Mondrian Standard, ACES, or Custom OCIO.
        let tone_map = display_management.tone_map_policy.resolve(
            self.color_management.workflow == ColorWorkflow::SceneReferred,
            self.working_color_space,
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

        ColorContext {
            working_color_space: self.working_color_space,
            output_color_space: OcioColorSpaceIdentity::Color(output_color_space),
            tone_map,
            nested_processing: self.color_management.nested_processing,
            engine,
            missing_metadata_policy: self.color_management.missing_metadata_policy,
            display_management,
            output_transform,
            workflow: self.color_management.workflow,
        }
    }

    pub fn nested_render_color_context(&self, parent: ColorContext) -> ColorContext {
        match self.color_management.nested_processing {
            NestedColorProcessing::PreserveChildWorkingSpace => {
                let engine = if self.color_management.inherit {
                    parent.engine.clone()
                } else {
                    self.color_management.engine.clone()
                };
                ColorContext {
                    working_color_space: self.working_color_space,
                    output_color_space: OcioColorSpaceIdentity::Working(parent.working_color_space),
                    tone_map: self.auto_tone_map_media,
                    nested_processing: self.color_management.nested_processing,
                    engine,
                    missing_metadata_policy: self.color_management.missing_metadata_policy,
                    display_management: parent.display_management.clone(),
                    output_transform: parent.output_transform.clone(),
                    workflow: self.color_management.workflow,
                }
            }
            NestedColorProcessing::ForceParentWorkingSpace => ColorContext {
                working_color_space: parent.working_color_space,
                output_color_space: OcioColorSpaceIdentity::Working(parent.working_color_space),
                tone_map: parent.tone_map,
                nested_processing: self.color_management.nested_processing,
                engine: parent.engine.clone(),
                missing_metadata_policy: parent.missing_metadata_policy,
                display_management: parent.display_management.clone(),
                output_transform: parent.output_transform.clone(),
                workflow: parent.workflow,
            },
            NestedColorProcessing::BakeChildOutputTransform => {
                let engine = if self.color_management.inherit {
                    parent.engine.clone()
                } else {
                    self.color_management.engine.clone()
                };
                ColorContext {
                    working_color_space: self.working_color_space,
                    output_color_space: OcioColorSpaceIdentity::Working(parent.working_color_space),
                    tone_map: self.auto_tone_map_media || parent.tone_map,
                    nested_processing: self.color_management.nested_processing,
                    engine,
                    missing_metadata_policy: self.color_management.missing_metadata_policy,
                    display_management: parent.display_management.clone(),
                    output_transform: parent.output_transform.clone(),
                    workflow: self.color_management.workflow,
                }
            }
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
        let working_color_space = self.working_color_space;
        let color_management = self.color_management.clone();
        let auto_tone_map_media = self.auto_tone_map_media;
        *self = Self::from_editing_mode(mode);
        self.audio_sample_rate = audio_sample_rate;
        self.audio_display_format = audio_display_format;
        self.audio_channel_layout = audio_channel_layout;
        self.timeline_display.timecode_start_frame = start_timecode_frame;
        self.preview = preview;
        self.working_color_space = working_color_space;
        self.color_management = color_management;
        self.auto_tone_map_media = auto_tone_map_media;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
pub struct Sequence {
    pub id: SequenceId,
    /// Monotonic authoring transaction revision for this stable Sequence ID.
    pub revision: SequenceRevision,
    pub name: String,
    pub role: SequenceRole,
    pub settings: SequenceSettings,
    pub video_tracks: Vec<Track>,
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
                let source_time = clip.timeline_to_source_time(time)?;
                let transform_mat = clip.transform.evaluate_matrix(time);
                let opacity =
                    (clip.transform.evaluate_opacity(time) * track_opacity).clamp(0.0, 1.0);
                result.push(ActiveClip {
                    clip: clip.clone(),
                    track_index: i,
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
    pub fn fork_audio_identities_for_sequence_duplicate(&mut self) {
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
            self.compact_audio_program();
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

        for clip in self
            .video_tracks
            .iter()
            .chain(&self.audio_tracks)
            .flat_map(|track| &track.clips)
        {
            if let Some(linked_clip) = clip.linked_clip {
                if linked_clip == clip.id || !clip_ids.contains(&linked_clip) {
                    return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                        step_id: "validate_author_identities".to_owned(),
                        reason: format!(
                            "Clip {} has invalid strong linked-Clip reference {}",
                            clip.id, linked_clip
                        ),
                    });
                }
            }
        }
        Ok(())
    }
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
    fn flat_active_clips_at(
        &self,
        time: TimelineTime,
    ) -> mondrian_core::Result<Vec<mondrian_core::timeline_data::FlatActiveClip>> {
        use mondrian_core::timeline_data::FlatActiveClip;
        Ok(self
            .active_clips_at(time)?
            .into_iter()
            .map(|ac| {
                let matrix = ac.transform_matrix;
                FlatActiveClip {
                    asset_id: ac.clip.asset_id,
                    clip_id: ac.clip.id,
                    kind: ac.clip.kind,
                    nested_sequence_id: ac.clip.nested_sequence_id,
                    is_disabled: ac.clip.is_disabled,
                    effects: ac.clip.effects.clone(),
                    masks: ac.clip.masks.clone(),
                    solid_color: ac.clip.solid_color,
                    interpretation: ac.clip.interpretation.clone(),
                    source_time: ac.source_time,
                    transform_matrix: [
                        matrix.x_axis.x,
                        matrix.x_axis.y,
                        matrix.z_axis.x,
                        matrix.y_axis.x,
                        matrix.y_axis.y,
                        matrix.z_axis.y,
                    ],
                    opacity: ac.opacity,
                    blend_mode: ac.blend_mode,
                    track_index: ac.track_index,
                }
            })
            .collect())
    }

    fn source_time_base(&self) -> mondrian_core::types::Rational {
        Sequence::time_base(self)
    }

    fn nested_color_processing(&self) -> mondrian_core::timeline_data::NestedColorProcessing {
        self.settings.color_management.nested_processing
    }

    fn auto_tone_map_media(&self) -> bool {
        self.settings.auto_tone_map_media
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
                .validate(&sequence.audio_tracks, &sequence.audio_roles)
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
                    let Some(sequence_id) = clip.nested_sequence_id else {
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
                    .filter_map(|clip| clip.nested_sequence_id)
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
    use mondrian_core::{DisplayToneMapPolicy, ProjectColorManagement};

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
        let color = SequenceColorManagement::default();
        assert_eq!(color.delivery_bit_depth, DeliveryBitDepth::Ten);

        let json = serde_json::to_value(color).expect("serialize sequence color management");
        assert_eq!(json["delivery_bit_depth"], "Ten");
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
            working_color_space: WorkingColorSpace::LinearRec2020,
            audio_sample_rate: 96_000,
            audio_display_format: AudioDisplayFormat::Milliseconds,
            audio_channel_layout: AudioChannelLayout::Surround51,
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
        assert_eq!(cinema.working_color_space, WorkingColorSpace::LinearRec2020);
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
    fn sequence_preset_validates_name_and_settings() {
        assert!(SequencePreset::new("", SequenceSettings::default()).is_err());
        let preset = SequencePreset::new("Editorial 25p", SequenceSettings::default())
            .expect("valid preset");
        assert_eq!(preset.name, "Editorial 25p");
    }

    #[test]
    fn sequence_color_management_rejects_invalid_hdr_metadata_policy() {
        let settings = SequenceSettings {
            color_management: SequenceColorManagement {
                output_color_space: ColorSpace::Rec709,
                static_hdr_metadata_policy: StaticHdrMetadataPolicy::WriteAuthored,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn sequence_color_management_rejects_source_only_output_space() {
        let settings = SequenceSettings {
            color_management: SequenceColorManagement {
                output_color_space: ColorSpace::AcesCg,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(settings.validate().is_err());
    }

    #[test]
    fn sequence_color_management_accepts_hdr_output_metadata_policy() {
        let settings = SequenceSettings {
            working_color_space: WorkingColorSpace::LinearRec2020,
            color_management: SequenceColorManagement {
                output_color_space: ColorSpace::Rec2100Pq,
                static_hdr_metadata_policy: StaticHdrMetadataPolicy::WriteAuthored,
                hdr_mastering_display: Some(
                    VideoMasteringDisplayMetadata::rec2100_1000_nit_reference(),
                ),
                hdr_content_light: Some(VideoContentLightMetadata::rec2100_1000_nit_reference()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn static_hdr_metadata_policy_serialization_is_explicit_and_breaking() {
        let color_management = SequenceColorManagement {
            static_hdr_metadata_policy: StaticHdrMetadataPolicy::WriteAuthored,
            ..SequenceColorManagement::default()
        };
        let json = serde_json::to_value(&color_management).expect("serialize color management");
        assert_eq!(
            json.get("static_hdr_metadata_policy"),
            Some(&serde_json::Value::String("WriteAuthored".to_owned()))
        );

        let old_shape = serde_json::json!({ "preserve_hdr_metadata": true });
        let error = serde_json::from_value::<SequenceColorManagement>(old_shape)
            .expect_err("removed preservation flag must not silently become Omit");
        assert!(error.to_string().contains("preserve_hdr_metadata"));
    }

    #[test]
    fn sequence_color_management_rejects_numerically_invalid_hdr_metadata() {
        let mut mastering = VideoMasteringDisplayMetadata::rec2100_1000_nit_reference();
        mastering.luminance.as_mut().expect("reference luminance").max =
            mondrian_core::VideoHdrRational::new(1000, 0);
        let invalid_mastering = SequenceSettings {
            color_management: SequenceColorManagement {
                output_color_space: ColorSpace::Rec2100Pq,
                static_hdr_metadata_policy: StaticHdrMetadataPolicy::WriteAuthored,
                hdr_mastering_display: Some(mastering),
                hdr_content_light: Some(VideoContentLightMetadata::rec2100_1000_nit_reference()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(invalid_mastering.validate().is_err());

        let invalid_content_light = SequenceSettings {
            color_management: SequenceColorManagement {
                output_color_space: ColorSpace::Rec2100Pq,
                static_hdr_metadata_policy: StaticHdrMetadataPolicy::WriteAuthored,
                hdr_mastering_display: Some(
                    VideoMasteringDisplayMetadata::rec2100_1000_nit_reference(),
                ),
                hdr_content_light: Some(VideoContentLightMetadata {
                    max_content_light_level: 400,
                    max_frame_average_light_level: 500,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(invalid_content_light.validate().is_err());
    }

    #[test]
    fn editing_mode_preserves_color_management_policy() {
        let mut settings = SequenceSettings {
            working_color_space: WorkingColorSpace::AcesCg,
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::SceneReferred,
                output_color_space: ColorSpace::Rec2100Pq,
                static_hdr_metadata_policy: StaticHdrMetadataPolicy::WriteAuthored,
                hdr_mastering_display: Some(
                    VideoMasteringDisplayMetadata::rec2100_1000_nit_reference(),
                ),
                hdr_content_light: Some(VideoContentLightMetadata::rec2100_1000_nit_reference()),
                ..Default::default()
            },
            auto_tone_map_media: false,
            ..Default::default()
        };

        settings.apply_editing_mode_preset(EditingMode::Custom);

        assert_eq!(settings.working_color_space, WorkingColorSpace::AcesCg);
        assert_eq!(
            settings.color_management.workflow,
            ColorWorkflow::SceneReferred
        );
        assert_eq!(
            settings.color_management.output_color_space,
            ColorSpace::Rec2100Pq
        );
        assert_eq!(
            settings.color_management.static_hdr_metadata_policy,
            StaticHdrMetadataPolicy::WriteAuthored
        );
        assert!(settings.color_management.hdr_mastering_display.is_some());
        assert!(settings.color_management.hdr_content_light.is_some());
        assert!(!settings.auto_tone_map_media);
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

    // ── ColorEngine / ColorContext tests ──────────────────────────────────────

    #[test]
    fn nested_sequence_respects_engine_inherit() {
        let mut parent = SequenceSettings::default();
        parent.color_management.engine = pinned_custom_engine(OcioConfigSource::Environment);
        parent.color_management.inherit = false;

        // Child with inherit=true should get parent's engine
        let mut child = SequenceSettings::default();
        child.color_management.engine = ColorEngine::mondrian_standard();
        child.color_management.inherit = true;
        child.color_management.nested_processing = NestedColorProcessing::ForceParentWorkingSpace;

        let parent_ctx = parent.root_program_color_context(&ProjectColorManagement::default());
        let child_ctx = child.nested_render_color_context(parent_ctx.clone());

        // ForceParentWorkingSpace uses parent's engine
        assert_eq!(
            child_ctx.engine,
            pinned_custom_engine(OcioConfigSource::Environment)
        );

        // PreserveChildWorkingSpace with inherit should also use parent engine
        child.color_management.nested_processing = NestedColorProcessing::PreserveChildWorkingSpace;
        let child_ctx2 = child.nested_render_color_context(parent_ctx);
        assert_eq!(
            child_ctx2.engine,
            pinned_custom_engine(OcioConfigSource::Environment)
        );
    }

    #[test]
    fn scene_referred_workflow_forces_tone_map() {
        let settings = SequenceSettings {
            auto_tone_map_media: false,
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::SceneReferred,
                ..Default::default()
            },
            ..Default::default()
        };

        let ctx = settings.root_program_color_context(&ProjectColorManagement::default());
        assert!(
            ctx.tone_map,
            "SceneReferred should always enable tone mapping"
        );

        let display_settings = SequenceSettings {
            auto_tone_map_media: false,
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::DisplayReferred,
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx3 = display_settings.root_program_color_context(&ProjectColorManagement::default());
        assert!(
            !ctx3.tone_map,
            "DisplayReferred with auto_tone_map=false should not tone map"
        );
    }

    #[test]
    fn default_standard_video_workflow_resolves_standard_rec709_view() {
        let settings = SequenceSettings::default();
        let context = settings.root_program_color_context(&ProjectColorManagement::default());

        assert_eq!(
            settings.color_management.workflow,
            ColorWorkflow::SceneReferred
        );
        assert_eq!(context.workflow, ColorWorkflow::SceneReferred);
        assert_eq!(
            context.output_color_space,
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709)
        );
        assert!(context.tone_map);
        assert_eq!(
            context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
        assert!(matches!(
            context.engine,
            ColorEngine::MondrianStandard { .. }
        ));
    }

    #[test]
    fn preview_monitor_and_program_output_contexts_are_explicitly_distinct() {
        let settings = SequenceSettings {
            working_color_space: WorkingColorSpace::LinearRec2020,
            color_management: SequenceColorManagement {
                output_color_space: ColorSpace::Rec2100Pq,
                workflow: ColorWorkflow::SceneReferred,
                ..Default::default()
            },
            ..Default::default()
        };
        let project_cm = ProjectColorManagement::default();

        let preview = settings.root_preview_color_context(&project_cm, ColorSpace::Rec709);
        let program = settings.root_program_color_context(&project_cm);

        assert_eq!(
            preview.working_color_space,
            WorkingColorSpace::LinearRec2020
        );
        assert_eq!(
            preview.output_color_space,
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709)
        );
        assert_eq!(
            program.working_color_space,
            WorkingColorSpace::LinearRec2020
        );
        assert_eq!(
            program.output_color_space,
            OcioColorSpaceIdentity::Color(ColorSpace::Rec2100Pq)
        );
        assert_eq!(preview.engine, program.engine);
        assert_eq!(preview.workflow, program.workflow);
        assert_eq!(
            preview.missing_metadata_policy,
            program.missing_metadata_policy
        );
        assert!(preview.tone_map);
        assert!(program.tone_map);
    }

    #[test]
    fn display_management_policy_inherits_from_project_color_management() {
        let project_cm = ProjectColorManagement {
            engine: ColorEngine::mondrian_standard(),
            display_management: DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(
                    ColorSpace::DisplayP3,
                ),
                viewer_mode: mondrian_core::ViewerDisplayMode::HdrPq,
                tone_map_policy: DisplayToneMapPolicy::Always,
            },
        };
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = true;
        settings.color_management.display_management = DisplayManagementPolicy {
            monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(ColorSpace::Srgb),
            viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
            tone_map_policy: DisplayToneMapPolicy::Never,
        };

        let ctx = settings.root_preview_color_context(&project_cm, ColorSpace::Rec2100Pq);

        assert_eq!(ctx.display_management, project_cm.display_management);
        assert!(ctx.tone_map);
        assert_eq!(
            ctx.display_management
                .viewer_mode
                .resolve(ctx.output_color_space.color().expect("root preview output")),
            mondrian_core::ResolvedViewerDisplayMode::HdrPq
        );
    }

    #[test]
    fn sequence_display_management_override_controls_tone_map_policy() {
        let project_cm = ProjectColorManagement {
            engine: ColorEngine::mondrian_standard(),
            display_management: DisplayManagementPolicy {
                tone_map_policy: DisplayToneMapPolicy::Always,
                ..Default::default()
            },
        };
        let mut settings = SequenceSettings {
            working_color_space: WorkingColorSpace::LinearRec2020,
            auto_tone_map_media: true,
            ..Default::default()
        };
        settings.color_management.inherit = false;
        settings.color_management.display_management = DisplayManagementPolicy {
            tone_map_policy: DisplayToneMapPolicy::Never,
            ..Default::default()
        };

        let ctx = settings.root_preview_color_context(&project_cm, ColorSpace::Rec709);

        assert_eq!(
            ctx.display_management.tone_map_policy,
            DisplayToneMapPolicy::Never
        );
        assert!(
            !ctx.tone_map,
            "explicit sequence display policy should be able to bypass tone mapping"
        );
    }

    #[test]
    fn automatic_display_policy_tone_maps_scene_referred_workflow_to_sdr_output() {
        let settings = SequenceSettings {
            working_color_space: WorkingColorSpace::LinearRec2020,
            auto_tone_map_media: false,
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::SceneReferred,
                ..Default::default()
            },
            ..Default::default()
        };

        let ctx = settings
            .root_preview_color_context(&ProjectColorManagement::default(), ColorSpace::Rec709);

        assert!(ctx.tone_map);
        assert_eq!(
            ctx.display_management
                .viewer_mode
                .resolve(ctx.output_color_space.color().expect("root preview output")),
            mondrian_core::ResolvedViewerDisplayMode::Sdr
        );
    }

    #[test]
    fn custom_ocio_engine_is_preserved_in_context() {
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = false;
        settings.color_management.engine = pinned_custom_engine(OcioConfigSource::Environment);

        let ctx = settings.root_program_color_context(&ProjectColorManagement::default());
        assert!(matches!(ctx.engine, ColorEngine::CustomOcio { .. }));
    }

    #[test]
    fn custom_ocio_scene_output_uses_project_pinned_display_view() {
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = false;
        settings.color_management.workflow = ColorWorkflow::SceneReferred;
        settings.color_management.engine = pinned_custom_engine(OcioConfigSource::Environment);

        let ctx = settings
            .root_preview_color_context(&ProjectColorManagement::default(), ColorSpace::Rec709);

        assert!(ctx.tone_map);
        assert_eq!(
            ctx.output_transform,
            mondrian_core::OutputTransformIntent::CustomOcio {
                output_color_space: ColorSpace::Rec709,
            }
        );
    }

    #[test]
    fn custom_ocio_scene_output_rejects_an_unpinned_output_target() {
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = false;
        settings.color_management.workflow = ColorWorkflow::SceneReferred;
        settings.color_management.output_color_space = ColorSpace::Rec2100Pq;
        settings.color_management.engine = pinned_custom_engine(OcioConfigSource::Environment);

        let error = settings
            .validate_with_project_color_management(&ProjectColorManagement::default())
            .expect_err("Custom OCIO must not relabel its pinned SDR View as Rec.2100 PQ");

        assert!(error.to_string().contains("Rec2100Pq"));
        assert!(error.to_string().contains("output binding"));
    }

    #[test]
    fn inherited_custom_ocio_rejects_unpinned_sequence_working_space() {
        let project_cm = ProjectColorManagement {
            engine: pinned_custom_engine(OcioConfigSource::Environment),
            ..ProjectColorManagement::default()
        };
        let settings = SequenceSettings {
            working_color_space: WorkingColorSpace::AcesCg,
            ..SequenceSettings::default()
        };

        let error = settings
            .validate_with_project_color_management(&project_cm)
            .expect_err("Custom OCIO must reject an unpinned sequence working space");

        assert!(error.to_string().contains("pins working space 'Linear Rec.2020'"));
    }

    #[test]
    fn inherited_standard_rejects_non_versioned_working_space() {
        let settings = SequenceSettings {
            working_color_space: WorkingColorSpace::LinearP3D65,
            ..SequenceSettings::default()
        };

        let error = settings
            .validate_with_project_color_management(&ProjectColorManagement::default())
            .expect_err("Standard working-space identity v1 must reject a non-versioned space");

        assert!(error.to_string().contains("Mondrian Standard"));
        assert!(error.to_string().contains("Linear Rec.2020"));
    }

    #[test]
    fn standard_scene_referred_output_requires_a_versioned_rendering_view() {
        let mut settings = SequenceSettings::default();
        settings.color_management.output_color_space = ColorSpace::Rec601Pal;

        let error = settings
            .validate_with_project_color_management(&ProjectColorManagement::default())
            .expect_err("Scene-referred Standard must reject an output without a product View");
        assert!(error.to_string().contains("no rendering View"), "{error:#}");

        settings.color_management.workflow = ColorWorkflow::DisplayReferred;
        settings
            .validate_with_project_color_management(&ProjectColorManagement::default())
            .expect("explicit display-referred colorimetric output remains valid");
    }

    #[test]
    fn aces_scene_output_uses_preset_pinned_display_view() {
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = false;
        settings.color_management.workflow = ColorWorkflow::SceneReferred;
        settings.color_management.engine = ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        };

        let ctx = settings
            .root_preview_color_context(&ProjectColorManagement::default(), ColorSpace::Rec709);

        assert!(ctx.tone_map);
        assert_eq!(
            ctx.output_transform,
            mondrian_core::OutputTransformIntent::aces_preset(
                mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
            )
        );
    }

    #[test]
    fn aces_scene_output_resolves_the_requested_target_or_fails_closed() {
        let preset = mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25;
        let project_cm = ProjectColorManagement::default();
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = false;
        settings.color_management.engine = ColorEngine::Aces { preset };
        settings.color_management.output_color_space = ColorSpace::Rec2100Pq;

        let pq = settings.root_program_color_context(&project_cm);
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

        settings.color_management.output_color_space = ColorSpace::Rec2020;
        let error = settings
            .validate_with_project_color_management(&project_cm)
            .expect_err("ACES must not relabel its default Rec.709 View as Rec.2020 SDR");
        assert!(error.to_string().contains("no rendering View"), "{error:#}");
    }

    #[test]
    fn mondrian_standard_scene_program_context_uses_standard_view() {
        let settings = SequenceSettings {
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::SceneReferred,
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx = settings.root_program_color_context(&ProjectColorManagement::default());
        assert_eq!(ctx.engine, ColorEngine::mondrian_standard());
        assert!(ctx.tone_map);
        assert_eq!(
            ctx.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
    }

    #[test]
    fn legacy_standard_v2_project_context_keeps_its_pinned_view() {
        let legacy_package = mondrian_core::MondrianStandardPackageIdentity::V2;
        let project_cm = ProjectColorManagement {
            engine: ColorEngine::MondrianStandard { package: legacy_package },
            display_management: DisplayManagementPolicy::default(),
        };
        let ctx = SequenceSettings::default().root_program_color_context(&project_cm);

        assert_eq!(ctx.engine, project_cm.engine);
        assert_eq!(
            ctx.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard_package(legacy_package)
        );
        assert_eq!(
            ctx.output_transform
                .resolve_display_view(ColorSpace::Rec709, &ctx.engine)
                .expect("legacy Standard intent"),
            Some((
                "Rec.1886 Rec.709 - Display".to_owned(),
                "Mondrian Standard SDR v1".to_owned(),
            ))
        );
    }

    #[test]
    fn mondrian_standard_scene_preview_context_uses_standard_view() {
        let settings = SequenceSettings {
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::SceneReferred,
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx = settings
            .root_preview_color_context(&ProjectColorManagement::default(), ColorSpace::Rec709);

        assert_eq!(ctx.engine, ColorEngine::mondrian_standard());
        assert!(ctx.tone_map);
        assert_eq!(
            ctx.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
    }

    #[test]
    fn mondrian_standard_tone_map_keeps_one_product_intent_across_targets() {
        let project_cm = ProjectColorManagement {
            engine: ColorEngine::mondrian_standard(),
            display_management: DisplayManagementPolicy {
                tone_map_policy: DisplayToneMapPolicy::Always,
                ..Default::default()
            },
        };
        let settings = SequenceSettings::default();

        let preview = settings.root_preview_color_context(&project_cm, ColorSpace::Rec709);
        let program = settings.root_program_color_context(&project_cm);

        assert!(preview.tone_map);
        assert!(program.tone_map);
        assert_eq!(
            preview.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
        assert_eq!(preview.output_transform, program.output_transform);
    }

    #[test]
    fn mondrian_standard_tone_map_resolves_the_output_targets_display() {
        let project_cm = ProjectColorManagement {
            engine: ColorEngine::mondrian_standard(),
            display_management: DisplayManagementPolicy {
                tone_map_policy: DisplayToneMapPolicy::Always,
                ..Default::default()
            },
        };
        let settings = SequenceSettings::default();

        let p3 = settings.root_preview_color_context(&project_cm, ColorSpace::DisplayP3);
        assert_eq!(
            p3.output_transform
                .resolve_display_view(ColorSpace::DisplayP3, &p3.engine)
                .expect("P3 intent"),
            Some((
                "Display P3 - Display".to_owned(),
                "Mondrian Standard SDR v2".to_owned(),
            ))
        );

        let rec2020 = settings.root_preview_color_context(&project_cm, ColorSpace::Rec2020);
        assert_eq!(
            rec2020
                .output_transform
                .resolve_display_view(ColorSpace::Rec2020, &rec2020.engine)
                .expect("Rec.2020 SDR intent"),
            Some((
                "Rec.2020 SDR - Display".to_owned(),
                "Mondrian Standard SDR v2".to_owned(),
            ))
        );

        let pq = settings.root_preview_color_context(&project_cm, ColorSpace::Rec2100Pq);
        assert_eq!(
            pq.output_transform
                .resolve_display_view(ColorSpace::Rec2100Pq, &pq.engine)
                .expect("PQ intent"),
            Some((
                "Rec.2100-PQ - Display".to_owned(),
                "Mondrian Standard HDR 1000 nits v1".to_owned(),
            ))
        );
        assert_eq!(
            pq.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );

        let hlg = settings.root_preview_color_context(&project_cm, ColorSpace::Rec2100Hlg);
        assert_eq!(
            hlg.output_transform
                .resolve_display_view(ColorSpace::Rec2100Hlg, &hlg.engine)
                .expect("HLG intent"),
            Some((
                "Rec.2100-HLG - Display".to_owned(),
                "Mondrian Standard HDR 1000 nits v1".to_owned(),
            ))
        );
    }

    #[test]
    fn inherit_flag_controls_engine_source() {
        let project_cm = ProjectColorManagement {
            engine: pinned_custom_engine(OcioConfigSource::Builtin {
                name: String::from("aces_1.2"),
            }),
            display_management: DisplayManagementPolicy::default(),
        };

        // inherit=true → use project engine
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = true;
        settings.color_management.engine = ColorEngine::mondrian_standard();
        let ctx = settings.root_program_color_context(&project_cm);
        assert_eq!(
            ctx.engine,
            pinned_custom_engine(OcioConfigSource::Builtin { name: String::from("aces_1.2") })
        );

        // inherit=false → use sequence's own engine
        settings.color_management.inherit = false;
        let ctx2 = settings.root_program_color_context(&project_cm);
        assert_eq!(ctx2.engine, ColorEngine::mondrian_standard());
    }

    #[test]
    fn default_program_context_uses_engine_owned_standard_view() {
        let settings = SequenceSettings::default();
        let project_cm = ProjectColorManagement::default();
        let ctx = settings.root_program_color_context(&project_cm);
        assert!(ctx.tone_map);
        assert_eq!(
            ctx.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
    }
}
