//! 序列（时间线）

use crate::{clip::ActiveClip, track::Track};
use mondrian_core::{
    types::*, DisplayManagementPolicy, VideoContentLightMetadata, VideoMasteringDisplayMetadata,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
pub use mondrian_core::timeline_data::{FieldOrder, PixelAspectRatio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VideoDisplayFormat {
    Timecode2997DropFrame,
    Timecode2997NonDropFrame,
    FeetAndFrames16mm,
    FeetAndFrames35mm,
    #[default]
    Frames,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioDisplayFormat {
    #[default]
    AudioSamples,
    Milliseconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioChannelLayout {
    Mono,
    #[default]
    Stereo,
    Surround51,
}

impl AudioChannelLayout {
    pub const fn channels(self) -> u8 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Surround51 => 6,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorWorkflow {
    #[default]
    DisplayReferred,
    SceneReferred,
    Aces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MissingColorMetadataPolicy {
    /// 无标签素材视为 Rec.709（行业默认）。
    #[default]
    AssumeRec709,
    /// 无标签素材直接视为序列工作空间（跳过输入变换）。
    AssumeSequenceWorkingSpace,
    /// 无标签素材拒绝导入 / 跳过渲染。
    RejectMedia,
}

impl MissingColorMetadataPolicy {
    /// 根据策略和检测到的色彩空间，解析有效的输入色彩空间。
    ///
    /// `detected` 来自媒体探测（FFmpeg 标签），`working` 是序列工作空间。
    /// 当素材无色彩标签时（`detected == None`），按策略行事。
    pub fn resolve_input(
        self,
        detected: Option<ColorSpace>,
        working: ColorSpace,
    ) -> Option<ColorSpace> {
        match detected {
            Some(cs) => Some(cs),
            None => match self {
                Self::AssumeRec709 => Some(ColorSpace::Rec709),
                Self::AssumeSequenceWorkingSpace => Some(working),
                Self::RejectMedia => None,
            },
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
pub enum ExportBitDepth {
    Eight,
    Ten,
    #[default]
    SixteenFloat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    pub export_bit_depth: ExportBitDepth,
    #[serde(default = "default_preserve_hdr_metadata")]
    pub preserve_hdr_metadata: bool,
    /// HDR mastering-display color volume (SMPTE ST 2086).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_mastering_display: Option<VideoMasteringDisplayMetadata>,
    /// HDR content light level metadata (MaxCLL / MaxFALL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_content_light: Option<VideoContentLightMetadata>,
}

/// 渲染色彩上下文 —— 单帧渲染所需的全部色彩信息。
///
/// 由序列设置 + 项目设置合并生成，贯穿整个渲染管线。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorContext {
    pub working_color_space: ColorSpace,
    pub output_color_space: ColorSpace,
    pub tone_map: bool,
    pub workflow: ColorWorkflow,
    pub nested_processing: NestedColorProcessing,
    pub engine: ColorEngine,
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    /// Resolved display-management policy for this render context.
    pub display_management: DisplayManagementPolicy,
    /// OCIO 显示设备名（仅在 OCIO 引擎 + 预览路径使用）。
    pub ocio_display: Option<String>,
    /// OCIO 视图名（仅在 OCIO 引擎 + 预览路径使用）。
    pub ocio_view: Option<String>,
}

impl Default for SequenceColorManagement {
    fn default() -> Self {
        Self {
            workflow: ColorWorkflow::DisplayReferred,
            inherit: default_inherit_color_management(),
            engine: ColorEngine::default(),
            missing_metadata_policy: MissingColorMetadataPolicy::AssumeRec709,
            nested_processing: NestedColorProcessing::PreserveChildWorkingSpace,
            display_management: DisplayManagementPolicy::default(),
            output_color_space: ColorSpace::Rec709,
            video_range: VideoRange::Full,
            export_bit_depth: ExportBitDepth::SixteenFloat,
            preserve_hdr_metadata: false,
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

const fn default_preserve_hdr_metadata() -> bool {
    false
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
    #[serde(default)]
    pub video_display_format: VideoDisplayFormat,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    #[serde(default)]
    pub audio_display_format: AudioDisplayFormat,
    #[serde(default)]
    pub audio_channel_layout: AudioChannelLayout,
    #[serde(default)]
    pub start_timecode_frame: i64,
    #[serde(default)]
    pub preview: SequencePreviewSettings,
    pub color_space: ColorSpace,
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
            video_display_format: VideoDisplayFormat::Frames,
            audio_sample_rate: 48000,
            audio_channels: AudioChannelLayout::Stereo.channels(),
            audio_display_format: AudioDisplayFormat::AudioSamples,
            audio_channel_layout: AudioChannelLayout::Stereo,
            start_timecode_frame: 0,
            preview: SequencePreviewSettings::default(),
            color_space: ColorSpace::Rec709,
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
        if self.audio_channels != self.audio_channel_layout.channels() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: format!(
                    "音频声道数 {} 与声道布局 {:?} 不匹配",
                    self.audio_channels, self.audio_channel_layout
                ),
            });
        }
        if self.start_timecode_frame < 0 {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "序列起始时间码不能为负数".to_string(),
            });
        }
        if !(0.125..=1.0).contains(&self.preview.resolution_scale) {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: format!("预览分辨率比例无效: {}", self.preview.resolution_scale),
            });
        }
        if self.color_management.workflow == ColorWorkflow::Aces
            && !matches!(
                self.color_space,
                ColorSpace::Rec2020 | ColorSpace::Rec2100Hlg | ColorSpace::Rec2100Pq
            )
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "ACES 工作流需要宽色域或 HDR 工作色彩空间".to_string(),
            });
        }
        if self.color_management.preserve_hdr_metadata
            && !self.color_management.output_color_space.is_hdr()
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "只有 HDR 输出色彩空间可以保留 HDR metadata".to_string(),
            });
        }
        if self.color_management.preserve_hdr_metadata
            && self.color_management.hdr_mastering_display.is_none()
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "保留 HDR metadata 需要 SMPTE ST 2086 母版显示元数据".to_string(),
            });
        }
        if self.color_management.preserve_hdr_metadata
            && self.color_management.hdr_content_light.is_none()
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_validate".to_string(),
                reason: "保留 HDR metadata 需要 MaxCLL/MaxFALL 内容光级别元数据".to_string(),
            });
        }
        Ok(())
    }

    /// Build the final-output color context for root sequence export.
    ///
    /// Export uses the sequence output color space because the rendered frames
    /// will be encoded and tagged for delivery. When the sequence inherits
    /// color management from the project (`color_management.inherit == true`),
    /// the `engine` is taken from `project_cm` instead of per-sequence settings.
    pub fn root_export_color_context(
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

        // SceneReferred and Aces workflows always need a view transform
        // (tone map) when output is display-referred (SDR).
        let tone_map = display_management.tone_map_policy.resolve(
            self.auto_tone_map_media,
            matches!(
                self.color_management.workflow,
                ColorWorkflow::SceneReferred | ColorWorkflow::Aces
            ),
            self.color_space,
            output_color_space,
        );

        // Auto-populate OCIO display/view from config defaults.
        let (ocio_display, ocio_view) = if matches!(
            engine,
            ColorEngine::MondrianSmart | ColorEngine::Ocio { .. }
        ) {
            mondrian_core::ocio_default_display_view()
                .map(|(d, v)| (Some(d), Some(v)))
                .unwrap_or((None, None))
        } else {
            (None, None)
        };

        ColorContext {
            working_color_space: self.color_space,
            output_color_space,
            tone_map,
            nested_processing: self.color_management.nested_processing,
            engine,
            missing_metadata_policy: self.color_management.missing_metadata_policy,
            display_management,
            ocio_display,
            ocio_view,
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
                    working_color_space: self.color_space,
                    output_color_space: parent.working_color_space,
                    tone_map: self.auto_tone_map_media,
                    nested_processing: self.color_management.nested_processing,
                    engine,
                    missing_metadata_policy: self.color_management.missing_metadata_policy,
                    display_management: parent.display_management.clone(),
                    ocio_display: parent.ocio_display.clone(),
                    ocio_view: parent.ocio_view.clone(),
                    workflow: self.color_management.workflow,
                }
            }
            NestedColorProcessing::ForceParentWorkingSpace => ColorContext {
                working_color_space: parent.working_color_space,
                output_color_space: parent.working_color_space,
                tone_map: parent.tone_map,
                nested_processing: self.color_management.nested_processing,
                engine: parent.engine.clone(),
                missing_metadata_policy: parent.missing_metadata_policy,
                display_management: parent.display_management.clone(),
                ocio_display: parent.ocio_display.clone(),
                ocio_view: parent.ocio_view.clone(),
                workflow: parent.workflow,
            },
            NestedColorProcessing::BakeChildOutputTransform => {
                let engine = if self.color_management.inherit {
                    parent.engine.clone()
                } else {
                    self.color_management.engine.clone()
                };
                ColorContext {
                    working_color_space: self.color_space,
                    output_color_space: parent.working_color_space,
                    tone_map: self.auto_tone_map_media || parent.tone_map,
                    nested_processing: self.color_management.nested_processing,
                    engine,
                    missing_metadata_policy: self.color_management.missing_metadata_policy,
                    display_management: parent.display_management.clone(),
                    ocio_display: parent.ocio_display.clone(),
                    ocio_view: parent.ocio_view.clone(),
                    workflow: self.color_management.workflow,
                }
            }
        }
    }

    pub fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.resolution = Resolution { width, height };
        self
    }

    pub fn from_editing_mode(mode: EditingMode) -> Self {
        let mut settings = Self { editing_mode: mode, ..Default::default() };
        match mode {
            EditingMode::Custom => settings,
            EditingMode::Dslr1080p => {
                settings.resolution = Resolution::FHD;
                settings.frame_rate = Rational::FPS_23976;
                settings.video_display_format = VideoDisplayFormat::Frames;
                settings
            }
            EditingMode::Dslr720p => {
                settings.resolution = Resolution::HD;
                settings.frame_rate = Rational::FPS_5994;
                settings.video_display_format = VideoDisplayFormat::Frames;
                settings
            }
            EditingMode::Avchd1080p => {
                settings.resolution = Resolution::FHD;
                settings.frame_rate = Rational::FPS_2997;
                settings.video_display_format = VideoDisplayFormat::Timecode2997DropFrame;
                settings
            }
            EditingMode::DigitalCinema4k => {
                settings.resolution = Resolution::DCI4K;
                settings.frame_rate = Rational::FPS_24;
                settings.color_space = ColorSpace::DciP3;
                settings
            }
            EditingMode::SocialVertical1080p => {
                settings.resolution = Resolution { width: 1080, height: 1920 };
                settings.frame_rate = Rational::FPS_30;
                settings.video_display_format = VideoDisplayFormat::Frames;
                settings
            }
        }
    }

    pub fn apply_editing_mode_preset(&mut self, mode: EditingMode) {
        let audio_sample_rate = self.audio_sample_rate;
        let audio_channels = self.audio_channels;
        let audio_display_format = self.audio_display_format;
        let audio_channel_layout = self.audio_channel_layout;
        let start_timecode_frame = self.start_timecode_frame;
        let preview = self.preview.clone();
        *self = Self::from_editing_mode(mode);
        self.audio_sample_rate = audio_sample_rate;
        self.audio_channels = audio_channels;
        self.audio_display_format = audio_display_format;
        self.audio_channel_layout = audio_channel_layout;
        self.start_timecode_frame = start_timecode_frame;
        self.preview = preview;
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
    pub name: String,
    #[serde(default)]
    pub role: SequenceRole,
    pub settings: SequenceSettings,
    pub video_tracks: Vec<Track>,
    pub audio_tracks: Vec<Track>,
    pub playhead: TimeCode,
    #[serde(default)]
    pub in_point_frame: Option<i64>,
    #[serde(default)]
    pub out_point_frame: Option<i64>,
}

impl Sequence {
    pub fn new(name: impl Into<String>) -> Self {
        let settings = SequenceSettings::default();
        let tb = Rational::new(settings.frame_rate.den, settings.frame_rate.num);
        Self {
            id: SequenceId::new(),
            name: name.into(),
            role: SequenceRole::Editorial,
            video_tracks: vec![
                Track::new_video("V1"),
                Track::new_video("V2"),
                Track::new_video("V3"),
            ],
            audio_tracks: vec![
                Track::new_audio("A1"),
                Track::new_audio("A2"),
                Track::new_audio("A3"),
            ],
            playhead: TimeCode::new(0, tb),
            in_point_frame: None,
            out_point_frame: None,
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
        sequence.playhead = TimeCode::new(0, sequence.time_base());
        Ok(sequence)
    }

    pub fn apply_settings_preserve_frames(
        &mut self,
        settings: SequenceSettings,
    ) -> mondrian_core::Result<()> {
        settings.validate()?;
        self.settings = settings;
        let tb = self.time_base();
        self.playhead.time_base = tb;
        self.in_point_frame = self.in_point_frame.map(|frame| frame.max(0));
        self.out_point_frame = self
            .out_point_frame
            .map(|frame| frame.max(0))
            .filter(|frame| *frame >= self.in_point_frame.unwrap_or(0));
        for track in self.video_tracks.iter_mut().chain(self.audio_tracks.iter_mut()) {
            for clip in &mut track.clips {
                clip.position.time_base = tb;
                clip.duration.time_base = tb;
                clip.source_in.time_base = tb;
                clip.source_out.time_base = tb;
            }
        }
        Ok(())
    }

    pub fn in_point_frame(&self) -> i64 {
        self.in_point_frame.unwrap_or(0).max(0)
    }

    pub fn out_point_frame(&self) -> Option<i64> {
        self.out_point_frame
            .map(|frame| frame.max(0))
            .filter(|frame| *frame >= self.in_point_frame())
    }

    pub fn mark_in(&mut self, frame: i64) {
        let frame = frame.max(0);
        self.in_point_frame = Some(frame);
        if self.out_point_frame.is_some_and(|out| out < frame) {
            self.out_point_frame = Some(frame);
        }
    }

    pub fn mark_out(&mut self, frame: i64) {
        self.out_point_frame = Some(frame.max(self.in_point_frame()));
    }

    pub fn clear_in_out(&mut self) {
        self.in_point_frame = None;
        self.out_point_frame = None;
    }

    pub fn total_duration(&self) -> TimeCode {
        let tb = self.time_base();
        let mut max_frame = 0i64;

        for track in self.video_tracks.iter().chain(self.audio_tracks.iter()) {
            if let Some(last) = track.clips.last() {
                max_frame = max_frame.max(last.end_position().frame);
            }
        }
        TimeCode::new(max_frame, tb)
    }

    pub fn active_clips_at(&self, time: TimeCode) -> Vec<ActiveClip> {
        let mut result = Vec::new();

        for (i, track) in self.video_tracks.iter().enumerate() {
            if !track.is_visible || track.is_muted {
                continue;
            }

            let track_opacity = track.evaluate_opacity(time).clamp(0.0, 1.0);
            for clip in track.active_clips_at(time) {
                let source_time = clip.timeline_to_source_time(time);
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
        result
    }

    pub fn snap_points(&self) -> Vec<TimeCode> {
        let mut pts: Vec<TimeCode> = self
            .video_tracks
            .iter()
            .chain(self.audio_tracks.iter())
            .flat_map(|track| track.snap_points())
            .collect();
        pts.push(self.playhead);
        pts.push(TimeCode::new(0, self.time_base()));
        pts.sort_unstable();
        pts.dedup();
        pts
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
        self.normalize_track_names();
        id
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
}

impl mondrian_core::timeline_data::RenderPlanSource for Sequence {
    fn flat_active_clips_at(
        &self,
        time: TimeCode,
    ) -> Vec<mondrian_core::timeline_data::FlatActiveClip> {
        use mondrian_core::timeline_data::FlatActiveClip;
        self.active_clips_at(time)
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
            .collect()
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
    use mondrian_core::automation::{
        timecode_to_ticks, Keyframe, PropertyHost, PropertyMutation, PropertyValue,
    };
    use mondrian_core::{DisplayToneMapPolicy, ProjectColorManagement};

    #[test]
    fn sequence_active_clips() {
        let mut seq = Sequence::new("Test");
        let asset_id = AssetId::new();
        let tb = seq.time_base();

        let clip = Clip::new(asset_id, TimeCode::new(0, tb), TimeCode::new(50, tb));
        seq.video_tracks[0].add_clip(clip).unwrap();

        let active = seq.active_clips_at(TimeCode::new(25, tb));
        assert_eq!(active.len(), 1);

        let outside = seq.active_clips_at(TimeCode::new(100, tb));
        assert_eq!(outside.len(), 0);
    }

    #[test]
    fn track_opacity_automation_affects_active_clip_opacity() {
        let mut seq = Sequence::new("Opacity Test");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.video_tracks[0]
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Track::OPACITY_PATH.to_string(),
                keyframe: Keyframe::linear(
                    timecode_to_ticks(TimeCode::new(0, tb)),
                    PropertyValue::Float(1.0),
                ),
            })
            .expect("set start opacity");
        seq.video_tracks[0]
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Track::OPACITY_PATH.to_string(),
                keyframe: Keyframe::linear(
                    timecode_to_ticks(TimeCode::new(20, tb)),
                    PropertyValue::Float(0.4),
                ),
            })
            .expect("set end opacity");

        let active = seq.active_clips_at(TimeCode::new(10, tb));
        assert_eq!(active.len(), 1);
        assert!((active[0].opacity - 0.7).abs() < 0.01);
    }

    #[test]
    fn active_clips_follow_bottom_to_top_track_order() {
        let mut seq = Sequence::new("Track Order");
        let tb = seq.time_base();
        let bottom = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        let top = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        seq.video_tracks[0].add_clip(bottom).expect("add bottom clip");
        seq.video_tracks[2].add_clip(top).expect("add top clip");

        let active = seq.active_clips_at(TimeCode::new(5, tb));
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].track_index, 0);
        assert_eq!(active[1].track_index, 2);
    }

    #[test]
    fn active_clips_inherit_track_blend_mode_when_clip_uses_default() {
        let mut seq = Sequence::new("Track Blend Inheritance");
        let tb = seq.time_base();
        seq.video_tracks[0].blend_mode = BlendMode::Screen;
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let active = seq.active_clips_at(TimeCode::new(5, tb));
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].blend_mode, BlendMode::Screen);
    }

    #[test]
    fn active_clips_prefer_clip_blend_mode_over_track_blend_mode() {
        let mut seq = Sequence::new("Clip Blend Override");
        let tb = seq.time_base();
        seq.video_tracks[0].blend_mode = BlendMode::Screen;
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        clip.blend_mode = Some(BlendMode::Multiply);
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let active = seq.active_clips_at(TimeCode::new(5, tb));
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].blend_mode, BlendMode::Multiply);
    }

    #[test]
    fn sequence_settings_validate_supported_presets() {
        let settings = SequenceSettings {
            frame_rate: Rational::FPS_23976,
            pixel_aspect_ratio: PixelAspectRatio::D1DvNtscWidescreen,
            field_order: FieldOrder::Progressive,
            video_display_format: VideoDisplayFormat::Timecode2997DropFrame,
            color_space: ColorSpace::Rec2100Pq,
            audio_sample_rate: 96_000,
            audio_display_format: AudioDisplayFormat::Milliseconds,
            audio_channel_layout: AudioChannelLayout::Surround51,
            audio_channels: AudioChannelLayout::Surround51.channels(),
            start_timecode_frame: 24 * 60 * 60,
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
        assert_eq!(cinema.color_space, ColorSpace::DciP3);
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

        let mismatched_channels = SequenceSettings {
            audio_channel_layout: AudioChannelLayout::Surround51,
            audio_channels: 2,
            ..Default::default()
        };
        assert!(mismatched_channels.validate().is_err());

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
                preserve_hdr_metadata: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn sequence_color_management_accepts_hdr_output_metadata_policy() {
        let settings = SequenceSettings {
            color_space: ColorSpace::Rec2100Pq,
            color_management: SequenceColorManagement {
                output_color_space: ColorSpace::Rec2100Pq,
                preserve_hdr_metadata: true,
                hdr_mastering_display: Some(
                    VideoMasteringDisplayMetadata::rec2100_pq_1000_nit_reference(),
                ),
                hdr_content_light: Some(VideoContentLightMetadata::hdr10_1000_nit_reference()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn applying_sequence_settings_updates_existing_time_bases() {
        let mut seq = Sequence::new("Settings");
        let old_tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(Clip::new(
                AssetId::new(),
                TimeCode::new(10, old_tb),
                TimeCode::new(20, old_tb),
            ))
            .expect("add clip");

        let settings = SequenceSettings {
            frame_rate: Rational::FPS_2997,
            ..seq.settings.clone()
        };
        seq.apply_settings_preserve_frames(settings).expect("apply settings");

        let new_tb = seq.time_base();
        assert_eq!(new_tb, Rational::new(1001, 30000));
        assert_eq!(seq.video_tracks[0].clips[0].position.frame, 10);
        assert_eq!(seq.video_tracks[0].clips[0].position.time_base, new_tb);
        assert_eq!(seq.video_tracks[0].clips[0].duration.time_base, new_tb);
    }

    #[test]
    fn sequence_collection_detects_nested_sequence_cycles() {
        let mut parent = Sequence::new("Parent");
        let mut child = Sequence::new("Child");
        let parent_id = parent.id;
        let child_id = child.id;
        let tb = parent.time_base();

        parent.video_tracks[0]
            .add_clip(Clip::new_nested_sequence(
                child_id,
                TimeCode::new(0, tb),
                TimeCode::new(20, tb),
                Some("Child".to_string()),
            ))
            .expect("add child nest");
        child.video_tracks[0]
            .add_clip(Clip::new_nested_sequence(
                parent_id,
                TimeCode::new(0, tb),
                TimeCode::new(20, tb),
                Some("Parent".to_string()),
            ))
            .expect("add parent nest");

        let mut collection = SequenceCollection::new(parent);
        collection.add_sequence(child).expect("add child sequence");

        assert!(collection.validate_nested_sequences().is_err());
    }

    #[test]
    fn adjustment_layer_is_active_only_within_its_time_range() {
        let mut seq = Sequence::new("Adjustment Range");
        let tb = seq.time_base();
        let media = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let adjustment =
            Clip::new_adjustment_layer(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(5, tb));
        seq.video_tracks[0].add_clip(media).expect("add media clip");
        seq.video_tracks[1].add_clip(adjustment).expect("add adjustment clip");

        let before = seq.active_clips_at(TimeCode::new(9, tb));
        assert_eq!(before.len(), 1);
        assert!(!before.iter().any(|clip| clip.clip.is_adjustment_layer()));

        let overlapping = seq.active_clips_at(TimeCode::new(12, tb));
        assert_eq!(overlapping.len(), 2);
        assert!(overlapping.iter().any(|clip| clip.clip.is_adjustment_layer()));

        let after = seq.active_clips_at(TimeCode::new(15, tb));
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
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
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
        parent.color_management.engine =
            ColorEngine::Ocio { source: OcioConfigSource::Environment };
        parent.color_management.inherit = false;

        // Child with inherit=true should get parent's engine
        let mut child = SequenceSettings::default();
        child.color_management.engine = ColorEngine::MondrianSmart;
        child.color_management.inherit = true;
        child.color_management.nested_processing = NestedColorProcessing::ForceParentWorkingSpace;

        let parent_ctx = parent.root_export_color_context(&ProjectColorManagement::default());
        let child_ctx = child.nested_render_color_context(parent_ctx.clone());

        // ForceParentWorkingSpace uses parent's engine
        assert_eq!(
            child_ctx.engine,
            ColorEngine::Ocio { source: OcioConfigSource::Environment }
        );

        // PreserveChildWorkingSpace with inherit should also use parent engine
        child.color_management.nested_processing = NestedColorProcessing::PreserveChildWorkingSpace;
        let child_ctx2 = child.nested_render_color_context(parent_ctx);
        assert_eq!(
            child_ctx2.engine,
            ColorEngine::Ocio { source: OcioConfigSource::Environment }
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

        let ctx = settings.root_export_color_context(&ProjectColorManagement::default());
        assert!(
            ctx.tone_map,
            "SceneReferred should always enable tone mapping"
        );

        let aces_settings = SequenceSettings {
            auto_tone_map_media: false,
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::Aces,
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx2 = aces_settings.root_export_color_context(&ProjectColorManagement::default());
        assert!(ctx2.tone_map, "ACES should always enable tone mapping");

        let display_settings = SequenceSettings {
            auto_tone_map_media: false,
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::DisplayReferred,
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx3 = display_settings.root_export_color_context(&ProjectColorManagement::default());
        assert!(
            !ctx3.tone_map,
            "DisplayReferred with auto_tone_map=false should not tone map"
        );
    }

    #[test]
    fn preview_and_export_root_color_contexts_separate_presentation_from_delivery() {
        let settings = SequenceSettings {
            color_space: ColorSpace::Rec2020,
            color_management: SequenceColorManagement {
                output_color_space: ColorSpace::Rec2100Pq,
                workflow: ColorWorkflow::SceneReferred,
                ..Default::default()
            },
            ..Default::default()
        };
        let project_cm = ProjectColorManagement::default();

        let preview = settings.root_preview_color_context(&project_cm, ColorSpace::Rec709);
        let export = settings.root_export_color_context(&project_cm);

        assert_eq!(preview.working_color_space, ColorSpace::Rec2020);
        assert_eq!(preview.output_color_space, ColorSpace::Rec709);
        assert_eq!(export.working_color_space, ColorSpace::Rec2020);
        assert_eq!(export.output_color_space, ColorSpace::Rec2100Pq);
        assert_eq!(preview.engine, export.engine);
        assert_eq!(preview.workflow, export.workflow);
        assert_eq!(
            preview.missing_metadata_policy,
            export.missing_metadata_policy
        );
        assert!(preview.tone_map);
        assert!(export.tone_map);
    }

    #[test]
    fn display_management_policy_inherits_from_project_color_management() {
        let project_cm = ProjectColorManagement {
            engine: ColorEngine::MondrianSmart,
            display_management: DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(
                    ColorSpace::DciP3,
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
            ctx.display_management.viewer_mode.resolve(ctx.output_color_space),
            mondrian_core::ResolvedViewerDisplayMode::HdrPq
        );
    }

    #[test]
    fn sequence_display_management_override_controls_tone_map_policy() {
        let project_cm = ProjectColorManagement {
            engine: ColorEngine::MondrianSmart,
            display_management: DisplayManagementPolicy {
                tone_map_policy: DisplayToneMapPolicy::Always,
                ..Default::default()
            },
        };
        let mut settings = SequenceSettings {
            color_space: ColorSpace::Rec2100Pq,
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
    fn automatic_display_policy_tone_maps_hdr_working_space_to_sdr_output() {
        let settings = SequenceSettings {
            color_space: ColorSpace::Rec2100Pq,
            auto_tone_map_media: false,
            color_management: SequenceColorManagement {
                workflow: ColorWorkflow::DisplayReferred,
                ..Default::default()
            },
            ..Default::default()
        };

        let ctx = settings
            .root_preview_color_context(&ProjectColorManagement::default(), ColorSpace::Rec709);

        assert!(ctx.tone_map);
        assert_eq!(
            ctx.display_management.viewer_mode.resolve(ctx.output_color_space),
            mondrian_core::ResolvedViewerDisplayMode::Sdr
        );
    }

    #[test]
    fn ocio_engine_populates_display_view_in_context() {
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = false;
        settings.color_management.engine =
            ColorEngine::Ocio { source: OcioConfigSource::Environment };

        let ctx = settings.root_export_color_context(&ProjectColorManagement::default());
        assert!(matches!(ctx.engine, ColorEngine::Ocio { .. }));
    }

    #[test]
    fn mondrian_smart_engine_uses_available_default_display_view() {
        let settings = SequenceSettings::default();
        let ctx = settings.root_export_color_context(&ProjectColorManagement::default());
        assert_eq!(ctx.engine, ColorEngine::MondrianSmart);
        let expected = mondrian_core::ocio_default_display_view();
        assert_eq!(
            ctx.ocio_display,
            expected.as_ref().map(|(display, _)| display.clone())
        );
        assert_eq!(ctx.ocio_view, expected.map(|(_, view)| view));
    }

    #[test]
    fn inherit_flag_controls_engine_source() {
        let project_cm = ProjectColorManagement {
            engine: ColorEngine::Ocio {
                source: OcioConfigSource::Builtin { name: String::from("aces_1.2") },
            },
            display_management: DisplayManagementPolicy::default(),
        };

        // inherit=true → use project engine
        let mut settings = SequenceSettings::default();
        settings.color_management.inherit = true;
        settings.color_management.engine = ColorEngine::MondrianSmart;
        let ctx = settings.root_export_color_context(&project_cm);
        assert_eq!(
            ctx.engine,
            ColorEngine::Ocio {
                source: OcioConfigSource::Builtin { name: String::from("aces_1.2") },
            }
        );

        // inherit=false → use sequence's own engine
        settings.color_management.inherit = false;
        let ctx2 = settings.root_export_color_context(&project_cm);
        assert_eq!(ctx2.engine, ColorEngine::MondrianSmart);
    }
}
