//! App UI sequence settings dialog.
//!
//! The dialog owns shell-local draft state. It emits a typed sequence update
//! only on Apply, keeping editor mutations in `AppState`.

use mondrian_core::display_labels::color_space_label;
use mondrian_core::{
    ColorSpace, Rational, Resolution, SmpteCountingMode, TimelineDisplayFormat, WorkingColorSpace,
};
use mondrian_timeline::{
    sequence::{
        ColorWorkflow, DeliveryBitDepth, MissingColorMetadataPolicy, NestedColorProcessing,
        StaticHdrMetadataPolicy, VideoRange,
    },
    AudioChannelLayout, AudioDisplayFormat, EditingMode, FieldOrder, PixelAspectRatio,
    PreviewRenderFormat, Sequence, SequenceSettings,
};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use mondrian_ui_widgets::{Button, Checkbox, DialogSurface, Label, NumberInput, Slider, TextInput};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_sequence_settings_action,
    app_shell_sequence_settings_draft_changed_action,
    app_shell_sequence_settings_tab_changed_action, SequenceSettingsDraftUpdatePayload,
    SequenceSettingsTabPayload, SequenceUpdateSettingsPayload,
};
use crate::app_ui::icons::AppIcon;
use crate::app_ui::preview_scale::preview_scale_percent_label;

/// Shell-local sequence settings form state.
#[derive(Debug, Clone)]
pub struct AppUiSequenceSettingsDraft {
    /// Sequence targeted by this modal.
    pub sequence_id: mondrian_core::types::SequenceId,
    /// Editable user-facing sequence name.
    pub name: String,
    /// Editable timeline format and preview settings.
    pub settings: SequenceSettings,
}

impl AppUiSequenceSettingsDraft {
    /// Build a draft from the active sequence snapshot.
    pub fn from_sequence(sequence: &Sequence) -> Self {
        Self {
            sequence_id: sequence.id,
            name: sequence.name.clone(),
            settings: sequence.settings.clone(),
        }
    }

    /// Validate the draft before applying it to editor state.
    pub fn validate(&self) -> mondrian_core::Result<()> {
        if self.name.trim().is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_draft_validate".to_owned(),
                reason: "序列名称不能为空".to_owned(),
            });
        }
        self.settings.validate()
    }

    fn can_edit_working_color_space(&self) -> bool {
        !self.settings.color_management.inherit
            && self.settings.color_management.engine.pinned_working_space().is_none()
    }

    /// Apply one shell-local form update to the real sequence settings.
    pub fn apply_update(&mut self, update: SequenceSettingsDraftUpdatePayload) {
        match update {
            SequenceSettingsDraftUpdatePayload::Name(name) => {
                self.name = name;
            }
            SequenceSettingsDraftUpdatePayload::EditingMode(mode) => {
                self.settings.apply_editing_mode_preset(mode);
            }
            SequenceSettingsDraftUpdatePayload::Resolution(resolution) => {
                self.settings.resolution = resolution;
            }
            SequenceSettingsDraftUpdatePayload::ResolutionWidth(width) => {
                self.settings.resolution.width = width;
            }
            SequenceSettingsDraftUpdatePayload::ResolutionHeight(height) => {
                self.settings.resolution.height = height;
            }
            SequenceSettingsDraftUpdatePayload::FrameRate(frame_rate) => {
                if Rational::SEQUENCE_FRAME_RATES.contains(&frame_rate) {
                    self.settings.frame_rate = frame_rate;
                    if self.settings.timeline_display.resolve(frame_rate).is_err() {
                        self.settings.timeline_display.format =
                            TimelineDisplayFormat::Timecode(SmpteCountingMode::NonDropFrame);
                    }
                }
            }
            SequenceSettingsDraftUpdatePayload::PixelAspectRatio(pixel_aspect_ratio) => {
                self.settings.pixel_aspect_ratio = pixel_aspect_ratio;
            }
            SequenceSettingsDraftUpdatePayload::FieldOrder(field_order) => {
                self.settings.field_order = field_order;
            }
            SequenceSettingsDraftUpdatePayload::TimelineDisplayFormat(display_format) => {
                self.settings.timeline_display.format = display_format;
            }
            SequenceSettingsDraftUpdatePayload::StartTimecodeFrame(frame) => {
                self.settings.timeline_display.timecode_start_frame = frame;
            }
            SequenceSettingsDraftUpdatePayload::WorkingColorSpace(color_space) => {
                if self.can_edit_working_color_space() {
                    self.settings.working_color_space = color_space;
                }
            }
            SequenceSettingsDraftUpdatePayload::AutoToneMapMedia(enabled) => {
                self.settings.auto_tone_map_media = enabled;
            }
            SequenceSettingsDraftUpdatePayload::ColorManagementInherit(inherit) => {
                self.settings.color_management.inherit = inherit;
            }
            SequenceSettingsDraftUpdatePayload::ColorWorkflow(workflow) => {
                self.settings.color_management.workflow = workflow;
            }
            SequenceSettingsDraftUpdatePayload::MissingColorMetadataPolicy(policy) => {
                self.settings.color_management.missing_metadata_policy = policy;
            }
            SequenceSettingsDraftUpdatePayload::NestedColorProcessing(processing) => {
                self.settings.color_management.nested_processing = processing;
            }
            SequenceSettingsDraftUpdatePayload::OutputColorSpace(color_space) => {
                self.settings.color_management.output_color_space = color_space;
            }
            SequenceSettingsDraftUpdatePayload::VideoRange(range) => {
                self.settings.color_management.video_range = range;
            }
            SequenceSettingsDraftUpdatePayload::DeliveryBitDepth(bit_depth) => {
                self.settings.color_management.delivery_bit_depth = bit_depth;
            }
            SequenceSettingsDraftUpdatePayload::WriteStaticHdrMetadata(enabled) => {
                self.settings.color_management.static_hdr_metadata_policy = if enabled {
                    StaticHdrMetadataPolicy::WriteAuthored
                } else {
                    StaticHdrMetadataPolicy::Omit
                };
            }
            SequenceSettingsDraftUpdatePayload::AudioSampleRate(sample_rate) => {
                if SequenceSettings::AUDIO_SAMPLE_RATES.contains(&sample_rate) {
                    self.settings.audio_sample_rate = sample_rate;
                }
            }
            SequenceSettingsDraftUpdatePayload::AudioChannelLayout(layout) => {
                self.settings.audio_channel_layout = layout;
            }
            SequenceSettingsDraftUpdatePayload::AudioDisplayFormat(display_format) => {
                self.settings.audio_display_format = display_format;
            }
            SequenceSettingsDraftUpdatePayload::PreviewRenderFormat(format) => {
                self.settings.preview.format = format;
            }
            SequenceSettingsDraftUpdatePayload::PreviewResolutionScale(scale) => {
                if scale.is_finite() {
                    self.settings.preview.resolution_scale = scale.clamp(0.125, 1.0);
                }
            }
            SequenceSettingsDraftUpdatePayload::PreviewCacheEnabled(enabled) => {
                self.settings.preview.cache_enabled = enabled;
            }
        }
        self.settings.audio_channels = self.settings.audio_channel_layout.channels();
    }

    /// Convert the current draft into the editor action payload.
    pub fn into_payload(self) -> SequenceUpdateSettingsPayload {
        SequenceUpdateSettingsPayload {
            sequence_id: self.sequence_id,
            name: self.name.trim().to_owned(),
            settings: self.settings,
        }
    }
}

const RESOLUTION_PRESETS: [(&str, Resolution); 4] = [
    ("高清 720p", Resolution::HD),
    ("全高清 1080p", Resolution::FHD),
    ("UHD 4K", Resolution::UHD4K),
    ("DCI 4K", Resolution::DCI4K),
];

const FRAME_RATE_PRESETS: [(&str, Rational); 7] = [
    ("23.976 fps", Rational::FPS_23976),
    ("24 fps", Rational::FPS_24),
    ("25 fps", Rational::FPS_25),
    ("29.97 fps", Rational::FPS_2997),
    ("30 fps", Rational::FPS_30),
    ("50 fps", Rational::FPS_50),
    ("59.94 fps", Rational::FPS_5994),
];

const AUDIO_SAMPLE_RATE_PRESETS: [(&str, u32); 5] = [
    ("32 kHz", 32_000),
    ("44.1 kHz", 44_100),
    ("48 kHz", 48_000),
    ("88.2 kHz", 88_200),
    ("96 kHz", 96_000),
];

const EDITING_MODE_OPTIONS: [EditingMode; 6] = [
    EditingMode::Custom,
    EditingMode::Dslr1080p,
    EditingMode::Dslr720p,
    EditingMode::Avchd1080p,
    EditingMode::DigitalCinema4k,
    EditingMode::SocialVertical1080p,
];

const PIXEL_ASPECT_RATIO_OPTIONS: [PixelAspectRatio; 9] = [
    PixelAspectRatio::Square,
    PixelAspectRatio::D1DvNtsc,
    PixelAspectRatio::D1DvNtscWidescreen,
    PixelAspectRatio::D1DvPal,
    PixelAspectRatio::D1DvPalWidescreen,
    PixelAspectRatio::Anamorphic2x,
    PixelAspectRatio::HdAnamorphic1080,
    PixelAspectRatio::DvcproHd,
    PixelAspectRatio::Unknown,
];

const FIELD_ORDER_OPTIONS: [FieldOrder; 3] = [
    FieldOrder::Progressive,
    FieldOrder::UpperFirst,
    FieldOrder::LowerFirst,
];

const TIMELINE_DISPLAY_FORMAT_OPTIONS: [TimelineDisplayFormat; 3] = [
    TimelineDisplayFormat::Timecode(SmpteCountingMode::NonDropFrame),
    TimelineDisplayFormat::Timecode(SmpteCountingMode::DropFrame),
    TimelineDisplayFormat::Frames,
];

const AUDIO_CHANNEL_LAYOUT_OPTIONS: [AudioChannelLayout; 3] = [
    AudioChannelLayout::Mono,
    AudioChannelLayout::Stereo,
    AudioChannelLayout::Surround51,
];

const AUDIO_DISPLAY_FORMAT_OPTIONS: [AudioDisplayFormat; 2] = [
    AudioDisplayFormat::AudioSamples,
    AudioDisplayFormat::Milliseconds,
];

const PREVIEW_RENDER_FORMAT_OPTIONS: [PreviewRenderFormat; 4] = [
    PreviewRenderFormat::IFrameOnly,
    PreviewRenderFormat::ProResProxy,
    PreviewRenderFormat::DnxHrLb,
    PreviewRenderFormat::LosslessRgba,
];

const PROGRAM_OUTPUT_COLOR_SPACE_OPTIONS: [ColorSpace; 6] = [
    ColorSpace::Rec709,
    ColorSpace::Rec2100Hlg,
    ColorSpace::Rec2100Pq,
    ColorSpace::Srgb,
    ColorSpace::Rec2020,
    ColorSpace::DisplayP3,
];

const WORKING_COLOR_SPACE_OPTIONS: [WorkingColorSpace; 4] = [
    WorkingColorSpace::LinearRec709,
    WorkingColorSpace::LinearRec2020,
    WorkingColorSpace::LinearP3D65,
    WorkingColorSpace::AcesCg,
];

const COLOR_WORKFLOW_OPTIONS: [ColorWorkflow; 2] =
    [ColorWorkflow::SceneReferred, ColorWorkflow::DisplayReferred];

const MISSING_COLOR_METADATA_OPTIONS: [MissingColorMetadataPolicy; 2] = [
    MissingColorMetadataPolicy::AssumeRec709,
    MissingColorMetadataPolicy::RejectMedia,
];

const NESTED_COLOR_PROCESSING_OPTIONS: [NestedColorProcessing; 3] = [
    NestedColorProcessing::PreserveChildWorkingSpace,
    NestedColorProcessing::ForceParentWorkingSpace,
    NestedColorProcessing::BakeChildOutputTransform,
];

const VIDEO_RANGE_OPTIONS: [VideoRange; 2] = [VideoRange::Full, VideoRange::Legal];

const DELIVERY_BIT_DEPTH_OPTIONS: [DeliveryBitDepth; 3] = [
    DeliveryBitDepth::Eight,
    DeliveryBitDepth::Ten,
    DeliveryBitDepth::Twelve,
];

impl SequenceSettingsTabPayload {
    const ALL: [Self; 3] = [Self::Format, Self::Color, Self::Preview];

    fn label(self) -> &'static str {
        match self {
            Self::Format => "格式",
            Self::Color => "色彩",
            Self::Preview => "预览",
        }
    }
}

fn editing_mode_label(value: EditingMode) -> &'static str {
    match value {
        EditingMode::Custom => "自定义",
        EditingMode::Dslr1080p => "DSLR 1080p",
        EditingMode::Dslr720p => "DSLR 720p",
        EditingMode::Avchd1080p => "AVCHD 1080p",
        EditingMode::DigitalCinema4k => "Digital Cinema 4K",
        EditingMode::SocialVertical1080p => "Social Vertical 1080p",
    }
}

fn resolution_label(resolution: Resolution) -> String {
    RESOLUTION_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == resolution).then_some((*label).to_owned()))
        .unwrap_or_else(|| resolution.to_string())
}

fn frame_rate_label(frame_rate: Rational) -> String {
    FRAME_RATE_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == frame_rate).then_some((*label).to_owned()))
        .unwrap_or_else(|| format!("{frame_rate} fps"))
}

fn audio_sample_rate_label(sample_rate: u32) -> String {
    AUDIO_SAMPLE_RATE_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == sample_rate).then_some((*label).to_owned()))
        .unwrap_or_else(|| format!("{sample_rate} Hz"))
}

fn pixel_aspect_ratio_label(value: PixelAspectRatio) -> &'static str {
    match value {
        PixelAspectRatio::Square => "方形像素 (1.0)",
        PixelAspectRatio::D1DvNtsc => "D1/DV NTSC",
        PixelAspectRatio::D1DvNtscWidescreen => "D1/DV NTSC 16:9",
        PixelAspectRatio::D1DvPal => "D1/DV PAL",
        PixelAspectRatio::D1DvPalWidescreen => "D1/DV PAL 16:9",
        PixelAspectRatio::Anamorphic2x => "Anamorphic 2:1",
        PixelAspectRatio::HdAnamorphic1080 => "HD Anamorphic 1080",
        PixelAspectRatio::DvcproHd => "DVCPRO HD",
        PixelAspectRatio::Unknown => "未知像素长宽比",
    }
}

fn field_order_label(value: FieldOrder) -> &'static str {
    match value {
        FieldOrder::Progressive => "逐行",
        FieldOrder::UpperFirst => "上场优先",
        FieldOrder::LowerFirst => "下场优先",
    }
}

fn timeline_display_format_label(value: TimelineDisplayFormat) -> &'static str {
    match value {
        TimelineDisplayFormat::Timecode(SmpteCountingMode::DropFrame) => "SMPTE drop-frame",
        TimelineDisplayFormat::Timecode(SmpteCountingMode::NonDropFrame) => "SMPTE non-drop-frame",
        TimelineDisplayFormat::Frames => "序列帧",
    }
}

fn audio_channel_layout_label(value: AudioChannelLayout) -> &'static str {
    match value {
        AudioChannelLayout::Mono => "单声道",
        AudioChannelLayout::Stereo => "立体声",
        AudioChannelLayout::Surround51 => "5.1 Surround",
    }
}

fn audio_display_format_label(value: AudioDisplayFormat) -> &'static str {
    match value {
        AudioDisplayFormat::AudioSamples => "音频采样",
        AudioDisplayFormat::Milliseconds => "毫秒",
    }
}

fn preview_render_format_label(value: PreviewRenderFormat) -> &'static str {
    match value {
        PreviewRenderFormat::IFrameOnly => "I-frame Only",
        PreviewRenderFormat::ProResProxy => "ProRes Proxy",
        PreviewRenderFormat::DnxHrLb => "DNxHR LB",
        PreviewRenderFormat::LosslessRgba => "Lossless RGBA",
    }
}

fn working_color_space_label(value: WorkingColorSpace) -> &'static str {
    match value {
        WorkingColorSpace::LinearRec709 => "Linear Rec. 709",
        WorkingColorSpace::LinearRec2020 => "Linear Rec. 2020",
        WorkingColorSpace::LinearP3D65 => "Linear P3-D65",
        WorkingColorSpace::AcesCg => "ACEScg",
    }
}

fn color_workflow_label(value: ColorWorkflow) -> &'static str {
    match value {
        ColorWorkflow::DisplayReferred => "显示参考",
        ColorWorkflow::SceneReferred => "场景参考",
    }
}

fn missing_color_metadata_policy_label(value: MissingColorMetadataPolicy) -> &'static str {
    match value {
        MissingColorMetadataPolicy::AssumeRec709 => "假定 Rec. 709",
        MissingColorMetadataPolicy::RejectMedia => "拒绝媒体",
    }
}

fn nested_color_processing_label(value: NestedColorProcessing) -> &'static str {
    match value {
        NestedColorProcessing::PreserveChildWorkingSpace => "保留子序列工作空间",
        NestedColorProcessing::ForceParentWorkingSpace => "强制使用父级工作空间",
        NestedColorProcessing::BakeChildOutputTransform => "烘焙子序列输出变换",
    }
}

fn video_range_label(value: VideoRange) -> &'static str {
    match value {
        VideoRange::Full => "全范围",
        VideoRange::Legal => "合法范围",
    }
}

fn delivery_bit_depth_label(value: DeliveryBitDepth) -> &'static str {
    match value {
        DeliveryBitDepth::Eight => "8-bit",
        DeliveryBitDepth::Ten => "10-bit",
        DeliveryBitDepth::Twelve => "12-bit",
    }
}

fn preview_scale_label(scale: f32) -> String {
    preview_scale_percent_label(scale)
}

fn resolution_items() -> Vec<MenuItem> {
    RESOLUTION_PRESETS
        .into_iter()
        .map(|(label, resolution)| {
            MenuItem::new(
                label,
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::Resolution(resolution),
                ),
            )
        })
        .collect()
}

fn editing_mode_items() -> Vec<MenuItem> {
    EDITING_MODE_OPTIONS
        .into_iter()
        .map(|mode| {
            MenuItem::new(
                editing_mode_label(mode),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::EditingMode(mode),
                ),
            )
        })
        .collect()
}

fn frame_rate_items() -> Vec<MenuItem> {
    FRAME_RATE_PRESETS
        .into_iter()
        .map(|(label, frame_rate)| {
            MenuItem::new(
                label,
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::FrameRate(frame_rate),
                ),
            )
        })
        .collect()
}

fn pixel_aspect_ratio_items() -> Vec<MenuItem> {
    PIXEL_ASPECT_RATIO_OPTIONS
        .into_iter()
        .map(|pixel_aspect_ratio| {
            MenuItem::new(
                pixel_aspect_ratio_label(pixel_aspect_ratio),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::PixelAspectRatio(pixel_aspect_ratio),
                ),
            )
        })
        .collect()
}

fn field_order_items() -> Vec<MenuItem> {
    FIELD_ORDER_OPTIONS
        .into_iter()
        .map(|field_order| {
            MenuItem::new(
                field_order_label(field_order),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::FieldOrder(field_order),
                ),
            )
        })
        .collect()
}

fn timeline_display_format_items(frame_rate: Rational) -> Vec<MenuItem> {
    TIMELINE_DISPLAY_FORMAT_OPTIONS
        .into_iter()
        .filter(|format| {
            !matches!(
                format,
                TimelineDisplayFormat::Timecode(SmpteCountingMode::DropFrame)
            ) || matches!(frame_rate, Rational::FPS_2997 | Rational::FPS_5994)
        })
        .map(|display_format| {
            MenuItem::new(
                timeline_display_format_label(display_format),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::TimelineDisplayFormat(display_format),
                ),
            )
        })
        .collect()
}

fn audio_sample_rate_items() -> Vec<MenuItem> {
    AUDIO_SAMPLE_RATE_PRESETS
        .into_iter()
        .map(|(label, sample_rate)| {
            MenuItem::new(
                label,
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::AudioSampleRate(sample_rate),
                ),
            )
        })
        .collect()
}

fn audio_channel_layout_items() -> Vec<MenuItem> {
    AUDIO_CHANNEL_LAYOUT_OPTIONS
        .into_iter()
        .map(|layout| {
            MenuItem::new(
                audio_channel_layout_label(layout),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::AudioChannelLayout(layout),
                ),
            )
        })
        .collect()
}

fn audio_display_format_items() -> Vec<MenuItem> {
    AUDIO_DISPLAY_FORMAT_OPTIONS
        .into_iter()
        .map(|display_format| {
            MenuItem::new(
                audio_display_format_label(display_format),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::AudioDisplayFormat(display_format),
                ),
            )
        })
        .collect()
}

fn preview_render_format_items() -> Vec<MenuItem> {
    PREVIEW_RENDER_FORMAT_OPTIONS
        .into_iter()
        .map(|format| {
            MenuItem::new(
                preview_render_format_label(format),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::PreviewRenderFormat(format),
                ),
            )
        })
        .collect()
}

fn color_space_items(
    update: fn(ColorSpace) -> SequenceSettingsDraftUpdatePayload,
) -> Vec<MenuItem> {
    PROGRAM_OUTPUT_COLOR_SPACE_OPTIONS
        .into_iter()
        .map(|color_space| {
            MenuItem::new(
                color_space_label(color_space),
                app_shell_sequence_settings_draft_changed_action(update(color_space)),
            )
        })
        .collect()
}

fn working_color_space_items(
    update: fn(WorkingColorSpace) -> SequenceSettingsDraftUpdatePayload,
) -> Vec<MenuItem> {
    WORKING_COLOR_SPACE_OPTIONS
        .into_iter()
        .map(|color_space| {
            MenuItem::new(
                working_color_space_label(color_space),
                app_shell_sequence_settings_draft_changed_action(update(color_space)),
            )
        })
        .collect()
}

fn color_workflow_items() -> Vec<MenuItem> {
    COLOR_WORKFLOW_OPTIONS
        .into_iter()
        .map(|workflow| {
            MenuItem::new(
                color_workflow_label(workflow),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::ColorWorkflow(workflow),
                ),
            )
        })
        .collect()
}

fn missing_color_metadata_items() -> Vec<MenuItem> {
    MISSING_COLOR_METADATA_OPTIONS
        .into_iter()
        .map(|policy| {
            MenuItem::new(
                missing_color_metadata_policy_label(policy),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::MissingColorMetadataPolicy(policy),
                ),
            )
        })
        .collect()
}

fn nested_color_processing_items() -> Vec<MenuItem> {
    NESTED_COLOR_PROCESSING_OPTIONS
        .into_iter()
        .map(|processing| {
            MenuItem::new(
                nested_color_processing_label(processing),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::NestedColorProcessing(processing),
                ),
            )
        })
        .collect()
}

fn video_range_items() -> Vec<MenuItem> {
    VIDEO_RANGE_OPTIONS
        .into_iter()
        .map(|range| {
            MenuItem::new(
                video_range_label(range),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::VideoRange(range),
                ),
            )
        })
        .collect()
}

fn delivery_bit_depth_items() -> Vec<MenuItem> {
    DELIVERY_BIT_DEPTH_OPTIONS
        .into_iter()
        .map(|bit_depth| {
            MenuItem::new(
                delivery_bit_depth_label(bit_depth),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::DeliveryBitDepth(bit_depth),
                ),
            )
        })
        .collect()
}

fn color_management_inherit_label(inherit: bool) -> &'static str {
    if inherit {
        "继承项目色彩设置"
    } else {
        "使用序列色彩设置"
    }
}

fn color_management_inherit_items() -> Vec<MenuItem> {
    [true, false]
        .into_iter()
        .map(|inherit| {
            MenuItem::new(
                color_management_inherit_label(inherit),
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::ColorManagementInherit(inherit),
                ),
            )
        })
        .collect()
}

fn editing_mode_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        editing_mode_label(draft.settings.editing_mode),
        editing_mode_items(),
    )
    .with_max_visible_items(6)
}

fn resolution_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        resolution_label(draft.settings.resolution),
        resolution_items(),
    )
    .with_max_visible_items(4)
}

fn resolution_width_input_for(draft: &AppUiSequenceSettingsDraft) -> NumberInput {
    NumberInput::new(
        draft.settings.resolution.width as f64,
        SequenceSettings::MIN_WIDTH as f64,
        SequenceSettings::MAX_WIDTH as f64,
    )
    .with_placeholder("宽度")
    .with_step(1.0)
    .on_change(|value| {
        app_shell_sequence_settings_draft_changed_action(
            SequenceSettingsDraftUpdatePayload::ResolutionWidth(value.round() as u32),
        )
    })
}

fn resolution_height_input_for(draft: &AppUiSequenceSettingsDraft) -> NumberInput {
    NumberInput::new(
        draft.settings.resolution.height as f64,
        SequenceSettings::MIN_HEIGHT as f64,
        SequenceSettings::MAX_HEIGHT as f64,
    )
    .with_placeholder("高度")
    .with_step(1.0)
    .on_change(|value| {
        app_shell_sequence_settings_draft_changed_action(
            SequenceSettingsDraftUpdatePayload::ResolutionHeight(value.round() as u32),
        )
    })
}

fn frame_rate_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        frame_rate_label(draft.settings.frame_rate),
        frame_rate_items(),
    )
    .with_max_visible_items(7)
}

fn start_timecode_input_for(draft: &AppUiSequenceSettingsDraft) -> NumberInput {
    NumberInput::new(
        draft.settings.timeline_display.timecode_start_frame as f64,
        -(24 * 60 * 60 * 240) as f64,
        (24 * 60 * 60 * 240) as f64,
    )
    .with_placeholder("起始帧")
    .with_step(1.0)
    .on_change(|value| {
        app_shell_sequence_settings_draft_changed_action(
            SequenceSettingsDraftUpdatePayload::StartTimecodeFrame(value.round() as i64),
        )
    })
}

fn pixel_aspect_ratio_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        pixel_aspect_ratio_label(draft.settings.pixel_aspect_ratio),
        pixel_aspect_ratio_items(),
    )
    .with_max_visible_items(6)
}

fn field_order_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        field_order_label(draft.settings.field_order),
        field_order_items(),
    )
    .with_max_visible_items(3)
}

fn timeline_display_format_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        timeline_display_format_label(draft.settings.timeline_display.format),
        timeline_display_format_items(draft.settings.frame_rate),
    )
    .with_max_visible_items(5)
}

fn color_space_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    maybe_disable_dropdown(
        Dropdown::new(
            working_color_space_label(draft.settings.working_color_space),
            working_color_space_items(SequenceSettingsDraftUpdatePayload::WorkingColorSpace),
        )
        .with_max_visible_items(6),
        !draft.can_edit_working_color_space(),
    )
}

fn maybe_disable_dropdown(dropdown: Dropdown, disabled: bool) -> Dropdown {
    if disabled {
        dropdown.disabled()
    } else {
        dropdown
    }
}

fn maybe_disable_checkbox(checkbox: Checkbox, disabled: bool) -> Checkbox {
    if disabled {
        checkbox.disabled()
    } else {
        checkbox
    }
}

fn color_management_inherit_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        color_management_inherit_label(draft.settings.color_management.inherit),
        color_management_inherit_items(),
    )
    .with_max_visible_items(2)
}

fn output_color_space_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    maybe_disable_dropdown(
        Dropdown::new(
            color_space_label(draft.settings.color_management.output_color_space),
            color_space_items(SequenceSettingsDraftUpdatePayload::OutputColorSpace),
        )
        .with_max_visible_items(6),
        draft.settings.color_management.inherit,
    )
}

fn color_workflow_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    maybe_disable_dropdown(
        Dropdown::new(
            color_workflow_label(draft.settings.color_management.workflow),
            color_workflow_items(),
        )
        .with_max_visible_items(3),
        draft.settings.color_management.inherit,
    )
}

fn missing_color_metadata_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    maybe_disable_dropdown(
        Dropdown::new(
            missing_color_metadata_policy_label(
                draft.settings.color_management.missing_metadata_policy,
            ),
            missing_color_metadata_items(),
        )
        .with_max_visible_items(3),
        draft.settings.color_management.inherit,
    )
}

fn nested_color_processing_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    maybe_disable_dropdown(
        Dropdown::new(
            nested_color_processing_label(draft.settings.color_management.nested_processing),
            nested_color_processing_items(),
        )
        .with_max_visible_items(3),
        draft.settings.color_management.inherit,
    )
}

fn video_range_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    maybe_disable_dropdown(
        Dropdown::new(
            video_range_label(draft.settings.color_management.video_range),
            video_range_items(),
        )
        .with_max_visible_items(2),
        draft.settings.color_management.inherit,
    )
}

fn delivery_bit_depth_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    maybe_disable_dropdown(
        Dropdown::new(
            delivery_bit_depth_label(draft.settings.color_management.delivery_bit_depth),
            delivery_bit_depth_items(),
        )
        .with_max_visible_items(3),
        draft.settings.color_management.inherit,
    )
}

fn audio_sample_rate_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        audio_sample_rate_label(draft.settings.audio_sample_rate),
        audio_sample_rate_items(),
    )
    .with_max_visible_items(5)
}

fn audio_channel_layout_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        audio_channel_layout_label(draft.settings.audio_channel_layout),
        audio_channel_layout_items(),
    )
    .with_max_visible_items(3)
}

fn audio_display_format_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        audio_display_format_label(draft.settings.audio_display_format),
        audio_display_format_items(),
    )
    .with_max_visible_items(2)
}

fn preview_render_format_dropdown_for(draft: &AppUiSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        preview_render_format_label(draft.settings.preview.format),
        preview_render_format_items(),
    )
    .with_max_visible_items(4)
}

fn preview_scale_slider_for(draft: &AppUiSequenceSettingsDraft) -> Slider {
    Slider::new(draft.settings.preview.resolution_scale, 0.125, 1.0)
        .with_step(0.125)
        .on_change(|scale| {
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::PreviewResolutionScale(scale),
            )
        })
}

fn preview_cache_checkbox_for(draft: &AppUiSequenceSettingsDraft) -> Checkbox {
    Checkbox::new("预览缓存", draft.settings.preview.cache_enabled).on_change(|enabled| {
        app_shell_sequence_settings_draft_changed_action(
            SequenceSettingsDraftUpdatePayload::PreviewCacheEnabled(enabled),
        )
    })
}

fn auto_tone_map_checkbox_for(draft: &AppUiSequenceSettingsDraft) -> Checkbox {
    maybe_disable_checkbox(
        Checkbox::new("自动色调映射媒体", draft.settings.auto_tone_map_media).on_change(
            |enabled| {
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::AutoToneMapMedia(enabled),
                )
            },
        ),
        draft.settings.color_management.inherit,
    )
}

fn static_hdr_metadata_policy_checkbox_for(draft: &AppUiSequenceSettingsDraft) -> Checkbox {
    maybe_disable_checkbox(
        Checkbox::new(
            "写入静态 HDR 元数据",
            draft
                .settings
                .color_management
                .static_hdr_metadata_policy
                .writes_authored_metadata(),
        )
        .on_change(|enabled| {
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::WriteStaticHdrMetadata(enabled),
            )
        }),
        draft.settings.color_management.inherit,
    )
}

const CARD_MIN_WIDTH: f32 = 420.0;
const CARD_WIDTH: f32 = 680.0;
const CARD_MIN_HEIGHT: f32 = 500.0;
const CARD_HEIGHT: f32 = 660.0;
const CONTENT_PADDING: f32 = 20.0;
const FIELD_HEIGHT: f32 = 34.0;
const DROPDOWN_HEIGHT: f32 = 28.0;
const ROW_GAP: f32 = 12.0;
const TAB_Y: f32 = 78.0;
const TAB_BUTTON_WIDTH: f32 = 88.0;
const TAB_BUTTON_HEIGHT: f32 = 28.0;
const NAME_Y: f32 = 138.0;
const FORMAT_LABEL_BASELINE_Y: f32 = 188.0;
const FORMAT_ROW_1_Y: f32 = 204.0;
const FORMAT_ROW_2_Y: f32 = 252.0;
const FORMAT_ROW_3_Y: f32 = 300.0;
const FORMAT_ROW_4_Y: f32 = 348.0;
const FORMAT_ROW_5_Y: f32 = 396.0;
const AUDIO_LABEL_BASELINE_Y: f32 = 456.0;
const AUDIO_ROW_Y: f32 = 472.0;
const PREVIEW_LABEL_BASELINE_Y: f32 = 126.0;
const PREVIEW_ROW_Y: f32 = 142.0;
const PREVIEW_SCALE_LABEL_Y: f32 = 188.0;
const PREVIEW_SCALE_ROW_Y: f32 = 202.0;
const BUTTON_WIDTH: f32 = 88.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_GAP: f32 = 8.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const LABEL_FONT_SIZE: f32 = 12.0;
const TITLE_BASELINE_Y: f32 = 22.0;
const DESCRIPTION_BASELINE_Y: f32 = 46.0;
const NAME_LABEL_BASELINE_Y: f32 = 132.0;

/// Sequence settings modal for the app UI product shell.
pub struct SequenceSettingsDialog {
    id: WidgetId,
    draft: AppUiSequenceSettingsDraft,
    active_tab: SequenceSettingsTabPayload,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    description_label: Label,
    tab_buttons: Vec<Button>,
    name_label: Label,
    format_label: Label,
    frame_size_label: Label,
    start_timecode_label: Label,
    audio_label: Label,
    preview_label: Label,
    color_label: Label,
    preview_scale_label: Label,
    name_input: TextInput,
    editing_mode_dropdown: Dropdown,
    resolution_dropdown: Dropdown,
    resolution_width_input: NumberInput,
    resolution_height_input: NumberInput,
    frame_rate_dropdown: Dropdown,
    pixel_aspect_ratio_dropdown: Dropdown,
    field_order_dropdown: Dropdown,
    timeline_display_format_dropdown: Dropdown,
    start_timecode_input: NumberInput,
    color_space_dropdown: Dropdown,
    output_color_space_dropdown: Dropdown,
    color_management_inherit_dropdown: Dropdown,
    color_workflow_dropdown: Dropdown,
    missing_color_metadata_dropdown: Dropdown,
    nested_color_processing_dropdown: Dropdown,
    video_range_dropdown: Dropdown,
    delivery_bit_depth_dropdown: Dropdown,
    auto_tone_map_checkbox: Checkbox,
    static_hdr_metadata_policy_checkbox: Checkbox,
    audio_sample_rate_dropdown: Dropdown,
    audio_channel_layout_dropdown: Dropdown,
    audio_display_format_dropdown: Dropdown,
    preview_render_format_dropdown: Dropdown,
    preview_scale_slider: Slider,
    preview_cache_checkbox: Checkbox,
    cancel_button: Button,
    apply_button: Button,
}

impl SequenceSettingsDialog {
    /// Build the sequence settings dialog from an explicit draft.
    pub fn new(draft: AppUiSequenceSettingsDraft) -> Self {
        let title_label = Label::new("序列设置")
            .popover_foreground()
            .with_font_size(TITLE_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let description_label = Label::new("调整活动序列的时间线格式和预览设置。")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0)
            .wrapped();
        let tab_buttons = SequenceSettingsTabPayload::ALL
            .into_iter()
            .map(|tab| {
                Button::new(tab.label())
                    .on_click(app_shell_sequence_settings_tab_changed_action(tab))
            })
            .collect();
        let name_label = Label::new("名称")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let format_label = Label::new("格式")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let frame_size_label = Label::new("自定义画面尺寸")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let start_timecode_label = Label::new("时间码起点（实际帧）")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let audio_label = Label::new("音频")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let preview_label = Label::new("预览")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let color_label = Label::new("色彩管理")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let preview_scale_label = Label::new(format!(
            "预览分辨率 {}",
            preview_scale_label(draft.settings.preview.resolution_scale)
        ))
        .muted()
        .with_font_size(LABEL_FONT_SIZE)
        .with_padding(0.0, 0.0);
        let name_input = TextInput::new("序列名称").with_text(&draft.name).on_change(|name| {
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::Name(name.into()),
            )
        });
        let editing_mode_dropdown = editing_mode_dropdown_for(&draft);
        let resolution_dropdown = resolution_dropdown_for(&draft);
        let resolution_width_input = resolution_width_input_for(&draft);
        let resolution_height_input = resolution_height_input_for(&draft);
        let frame_rate_dropdown = frame_rate_dropdown_for(&draft);
        let pixel_aspect_ratio_dropdown = pixel_aspect_ratio_dropdown_for(&draft);
        let field_order_dropdown = field_order_dropdown_for(&draft);
        let timeline_display_format_dropdown = timeline_display_format_dropdown_for(&draft);
        let start_timecode_input = start_timecode_input_for(&draft);
        let color_space_dropdown = color_space_dropdown_for(&draft);
        let output_color_space_dropdown = output_color_space_dropdown_for(&draft);
        let color_management_inherit_dropdown = color_management_inherit_dropdown_for(&draft);
        let color_workflow_dropdown = color_workflow_dropdown_for(&draft);
        let missing_color_metadata_dropdown = missing_color_metadata_dropdown_for(&draft);
        let nested_color_processing_dropdown = nested_color_processing_dropdown_for(&draft);
        let video_range_dropdown = video_range_dropdown_for(&draft);
        let delivery_bit_depth_dropdown = delivery_bit_depth_dropdown_for(&draft);
        let auto_tone_map_checkbox = auto_tone_map_checkbox_for(&draft);
        let static_hdr_metadata_policy_checkbox = static_hdr_metadata_policy_checkbox_for(&draft);
        let audio_sample_rate_dropdown = audio_sample_rate_dropdown_for(&draft);
        let audio_channel_layout_dropdown = audio_channel_layout_dropdown_for(&draft);
        let audio_display_format_dropdown = audio_display_format_dropdown_for(&draft);
        let preview_render_format_dropdown = preview_render_format_dropdown_for(&draft);
        let preview_scale_slider = preview_scale_slider_for(&draft);
        let preview_cache_checkbox = preview_cache_checkbox_for(&draft);
        Self {
            id: WidgetId::new(),
            draft,
            active_tab: SequenceSettingsTabPayload::Format,
            surface: DialogSurface::new(
                Size::new(CARD_MIN_WIDTH, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title_label,
            description_label,
            tab_buttons,
            name_label,
            format_label,
            frame_size_label,
            start_timecode_label,
            audio_label,
            preview_label,
            color_label,
            preview_scale_label,
            name_input,
            editing_mode_dropdown,
            resolution_dropdown,
            resolution_width_input,
            resolution_height_input,
            frame_rate_dropdown,
            pixel_aspect_ratio_dropdown,
            field_order_dropdown,
            timeline_display_format_dropdown,
            start_timecode_input,
            color_space_dropdown,
            output_color_space_dropdown,
            color_management_inherit_dropdown,
            color_workflow_dropdown,
            missing_color_metadata_dropdown,
            nested_color_processing_dropdown,
            video_range_dropdown,
            delivery_bit_depth_dropdown,
            auto_tone_map_checkbox,
            static_hdr_metadata_policy_checkbox,
            audio_sample_rate_dropdown,
            audio_channel_layout_dropdown,
            audio_display_format_dropdown,
            preview_render_format_dropdown,
            preview_scale_slider,
            preview_cache_checkbox,
            cancel_button: Button::new("取消").on_click(app_shell_close_modal_action()),
            apply_button: AppIcon::Save
                .text_button_or_label("应用")
                .on_click(app_shell_confirm_sequence_settings_action()),
        }
    }

    /// Apply one shell-local update and rebuild controls whose labels changed.
    pub fn apply_update(&mut self, update: SequenceSettingsDraftUpdatePayload) {
        let rebuild_controls = !matches!(update, SequenceSettingsDraftUpdatePayload::Name(_));
        self.draft.apply_update(update);
        if rebuild_controls {
            self.editing_mode_dropdown = editing_mode_dropdown_for(&self.draft);
            self.resolution_dropdown = resolution_dropdown_for(&self.draft);
            self.resolution_width_input = resolution_width_input_for(&self.draft);
            self.resolution_height_input = resolution_height_input_for(&self.draft);
            self.frame_rate_dropdown = frame_rate_dropdown_for(&self.draft);
            self.pixel_aspect_ratio_dropdown = pixel_aspect_ratio_dropdown_for(&self.draft);
            self.field_order_dropdown = field_order_dropdown_for(&self.draft);
            self.timeline_display_format_dropdown =
                timeline_display_format_dropdown_for(&self.draft);
            self.start_timecode_input = start_timecode_input_for(&self.draft);
            self.color_space_dropdown = color_space_dropdown_for(&self.draft);
            self.output_color_space_dropdown = output_color_space_dropdown_for(&self.draft);
            self.color_management_inherit_dropdown =
                color_management_inherit_dropdown_for(&self.draft);
            self.color_workflow_dropdown = color_workflow_dropdown_for(&self.draft);
            self.missing_color_metadata_dropdown = missing_color_metadata_dropdown_for(&self.draft);
            self.nested_color_processing_dropdown =
                nested_color_processing_dropdown_for(&self.draft);
            self.video_range_dropdown = video_range_dropdown_for(&self.draft);
            self.delivery_bit_depth_dropdown = delivery_bit_depth_dropdown_for(&self.draft);
            self.auto_tone_map_checkbox = auto_tone_map_checkbox_for(&self.draft);
            self.static_hdr_metadata_policy_checkbox =
                static_hdr_metadata_policy_checkbox_for(&self.draft);
            self.audio_sample_rate_dropdown = audio_sample_rate_dropdown_for(&self.draft);
            self.audio_channel_layout_dropdown = audio_channel_layout_dropdown_for(&self.draft);
            self.audio_display_format_dropdown = audio_display_format_dropdown_for(&self.draft);
            self.preview_render_format_dropdown = preview_render_format_dropdown_for(&self.draft);
            self.preview_scale_slider = preview_scale_slider_for(&self.draft);
            self.preview_scale_label = Label::new(format!(
                "预览分辨率 {}",
                preview_scale_label(self.draft.settings.preview.resolution_scale)
            ))
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
            self.preview_cache_checkbox = preview_cache_checkbox_for(&self.draft);
            if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                self.layout(self.bounds);
            }
        }
    }

    /// Select the visible settings section.
    pub fn set_active_tab(&mut self, tab: SequenceSettingsTabPayload) {
        self.active_tab = tab;
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
    }

    /// Current shell-local draft.
    pub fn draft(&self) -> &AppUiSequenceSettingsDraft {
        &self.draft
    }
}

impl Widget for SequenceSettingsDialog {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        self.surface.preferred_size()
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.card = self.surface.card_rect(bounds);
        let content = self.surface.content_rect(self.card);
        self.title_label.layout(Rect::new(
            content.x,
            content.y + TITLE_BASELINE_Y,
            content.width,
            LABEL_FONT_SIZE * 2.0,
        ));
        self.description_label.layout(Rect::new(
            content.x,
            content.y + DESCRIPTION_BASELINE_Y,
            content.width,
            LABEL_FONT_SIZE * 3.0,
        ));
        for (index, button) in self.tab_buttons.iter_mut().enumerate() {
            button.layout(Rect::new(
                content.x + index as f32 * (TAB_BUTTON_WIDTH + ROW_GAP),
                content.y + TAB_Y,
                TAB_BUTTON_WIDTH,
                TAB_BUTTON_HEIGHT,
            ));
        }

        let half = (content.width - ROW_GAP) * 0.5;
        let right_x = content.x + half + ROW_GAP;
        match self.active_tab {
            SequenceSettingsTabPayload::Format => {
                self.name_label.layout(Rect::new(
                    content.x,
                    content.y + NAME_LABEL_BASELINE_Y,
                    content.width,
                    LABEL_FONT_SIZE * 1.5,
                ));
                self.name_input.layout(Rect::new(
                    content.x,
                    content.y + NAME_Y,
                    content.width,
                    FIELD_HEIGHT,
                ));
                self.format_label.layout(Rect::new(
                    content.x,
                    content.y + FORMAT_LABEL_BASELINE_Y,
                    content.width,
                    LABEL_FONT_SIZE * 1.5,
                ));
                self.editing_mode_dropdown.layout(Rect::new(
                    content.x,
                    content.y + FORMAT_ROW_1_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.resolution_dropdown.layout(Rect::new(
                    right_x,
                    content.y + FORMAT_ROW_1_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.frame_size_label.layout(Rect::new(
                    content.x,
                    content.y + FORMAT_ROW_2_Y - 16.0,
                    content.width,
                    LABEL_FONT_SIZE * 1.5,
                ));
                self.resolution_width_input.layout(Rect::new(
                    content.x,
                    content.y + FORMAT_ROW_2_Y,
                    half,
                    FIELD_HEIGHT,
                ));
                self.resolution_height_input.layout(Rect::new(
                    right_x,
                    content.y + FORMAT_ROW_2_Y,
                    half,
                    FIELD_HEIGHT,
                ));
                self.frame_rate_dropdown.layout(Rect::new(
                    content.x,
                    content.y + FORMAT_ROW_3_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.pixel_aspect_ratio_dropdown.layout(Rect::new(
                    right_x,
                    content.y + FORMAT_ROW_3_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.field_order_dropdown.layout(Rect::new(
                    content.x,
                    content.y + FORMAT_ROW_4_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.timeline_display_format_dropdown.layout(Rect::new(
                    right_x,
                    content.y + FORMAT_ROW_4_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.start_timecode_label.layout(Rect::new(
                    content.x,
                    content.y + FORMAT_ROW_5_Y - 16.0,
                    half,
                    LABEL_FONT_SIZE * 1.5,
                ));
                self.start_timecode_input.layout(Rect::new(
                    content.x,
                    content.y + FORMAT_ROW_5_Y,
                    half,
                    FIELD_HEIGHT,
                ));
                self.audio_label.layout(Rect::new(
                    content.x,
                    content.y + AUDIO_LABEL_BASELINE_Y,
                    content.width,
                    LABEL_FONT_SIZE * 1.5,
                ));
                self.audio_sample_rate_dropdown.layout(Rect::new(
                    content.x,
                    content.y + AUDIO_ROW_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.audio_channel_layout_dropdown.layout(Rect::new(
                    right_x,
                    content.y + AUDIO_ROW_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.audio_display_format_dropdown.layout(Rect::new(
                    content.x,
                    content.y + AUDIO_ROW_Y + 38.0,
                    half,
                    DROPDOWN_HEIGHT,
                ));
            }
            SequenceSettingsTabPayload::Color => {
                self.color_label.layout(Rect::new(
                    content.x,
                    content.y + NAME_LABEL_BASELINE_Y,
                    content.width,
                    LABEL_FONT_SIZE * 1.5,
                ));
                let mut row_y = content.y + NAME_Y;
                self.color_management_inherit_dropdown.layout(Rect::new(
                    content.x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                row_y += 48.0;
                self.color_space_dropdown.layout(Rect::new(
                    content.x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.output_color_space_dropdown.layout(Rect::new(
                    right_x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                row_y += 48.0;
                self.color_workflow_dropdown.layout(Rect::new(
                    content.x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.missing_color_metadata_dropdown.layout(Rect::new(
                    right_x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                row_y += 48.0;
                self.nested_color_processing_dropdown.layout(Rect::new(
                    content.x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.video_range_dropdown
                    .layout(Rect::new(right_x, row_y, half, DROPDOWN_HEIGHT));
                row_y += 48.0;
                self.delivery_bit_depth_dropdown.layout(Rect::new(
                    content.x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.auto_tone_map_checkbox.layout(Rect::new(
                    right_x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                row_y += 48.0;
                self.static_hdr_metadata_policy_checkbox.layout(Rect::new(
                    content.x,
                    row_y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
            }
            SequenceSettingsTabPayload::Preview => {
                self.preview_label.layout(Rect::new(
                    content.x,
                    content.y + PREVIEW_LABEL_BASELINE_Y,
                    content.width,
                    LABEL_FONT_SIZE * 1.5,
                ));
                self.preview_render_format_dropdown.layout(Rect::new(
                    content.x,
                    content.y + PREVIEW_ROW_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.preview_cache_checkbox.layout(Rect::new(
                    right_x,
                    content.y + PREVIEW_ROW_Y,
                    half,
                    DROPDOWN_HEIGHT,
                ));
                self.preview_scale_label.layout(Rect::new(
                    content.x,
                    content.y + PREVIEW_SCALE_LABEL_Y,
                    content.width,
                    LABEL_FONT_SIZE * 1.5,
                ));
                self.preview_scale_slider.layout(Rect::new(
                    content.x,
                    content.y + PREVIEW_SCALE_ROW_Y,
                    content.width,
                    DROPDOWN_HEIGHT,
                ));
            }
        }

        let button_y = self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT;
        self.cancel_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH * 2.0 - BUTTON_GAP,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
        self.apply_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_close_modal_action());
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_confirm_sequence_settings_action());
                return EventResult::Handled;
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                return EventResult::Handled;
            }
            _ => {}
        }

        if self.apply_button.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.cancel_button.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        for button in &mut self.tab_buttons {
            if button.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        match self.active_tab {
            SequenceSettingsTabPayload::Format => {
                if self.name_input.event(event, ctx) == EventResult::Handled
                    || self.editing_mode_dropdown.event(event, ctx) == EventResult::Handled
                    || self.resolution_dropdown.event(event, ctx) == EventResult::Handled
                    || self.resolution_width_input.event(event, ctx) == EventResult::Handled
                    || self.resolution_height_input.event(event, ctx) == EventResult::Handled
                    || self.frame_rate_dropdown.event(event, ctx) == EventResult::Handled
                    || self.pixel_aspect_ratio_dropdown.event(event, ctx) == EventResult::Handled
                    || self.field_order_dropdown.event(event, ctx) == EventResult::Handled
                    || self.timeline_display_format_dropdown.event(event, ctx)
                        == EventResult::Handled
                    || self.start_timecode_input.event(event, ctx) == EventResult::Handled
                    || self.audio_sample_rate_dropdown.event(event, ctx) == EventResult::Handled
                    || self.audio_channel_layout_dropdown.event(event, ctx) == EventResult::Handled
                    || self.audio_display_format_dropdown.event(event, ctx) == EventResult::Handled
                {
                    return EventResult::Handled;
                }
            }
            SequenceSettingsTabPayload::Color => {
                if self.color_management_inherit_dropdown.event(event, ctx) == EventResult::Handled
                    || self.color_space_dropdown.event(event, ctx) == EventResult::Handled
                    || self.output_color_space_dropdown.event(event, ctx) == EventResult::Handled
                    || self.color_workflow_dropdown.event(event, ctx) == EventResult::Handled
                    || self.missing_color_metadata_dropdown.event(event, ctx)
                        == EventResult::Handled
                    || self.nested_color_processing_dropdown.event(event, ctx)
                        == EventResult::Handled
                    || self.video_range_dropdown.event(event, ctx) == EventResult::Handled
                    || self.delivery_bit_depth_dropdown.event(event, ctx) == EventResult::Handled
                    || self.auto_tone_map_checkbox.event(event, ctx) == EventResult::Handled
                    || self.static_hdr_metadata_policy_checkbox.event(event, ctx)
                        == EventResult::Handled
                {
                    return EventResult::Handled;
                }
            }
            SequenceSettingsTabPayload::Preview => {
                if self.preview_render_format_dropdown.event(event, ctx) == EventResult::Handled
                    || self.preview_scale_slider.event(event, ctx) == EventResult::Handled
                    || self.preview_cache_checkbox.event(event, ctx) == EventResult::Handled
                {
                    return EventResult::Handled;
                }
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title_label.paint(ctx);
        self.description_label.paint(ctx);
        for button in &self.tab_buttons {
            button.paint(ctx);
        }
        match self.active_tab {
            SequenceSettingsTabPayload::Format => {
                self.name_label.paint(ctx);
                self.name_input.paint(ctx);
                self.format_label.paint(ctx);
                self.editing_mode_dropdown.paint(ctx);
                self.resolution_dropdown.paint(ctx);
                self.frame_size_label.paint(ctx);
                self.resolution_width_input.paint(ctx);
                self.resolution_height_input.paint(ctx);
                self.frame_rate_dropdown.paint(ctx);
                self.pixel_aspect_ratio_dropdown.paint(ctx);
                self.field_order_dropdown.paint(ctx);
                self.timeline_display_format_dropdown.paint(ctx);
                self.start_timecode_label.paint(ctx);
                self.start_timecode_input.paint(ctx);
                self.audio_label.paint(ctx);
                self.audio_sample_rate_dropdown.paint(ctx);
                self.audio_channel_layout_dropdown.paint(ctx);
                self.audio_display_format_dropdown.paint(ctx);
            }
            SequenceSettingsTabPayload::Color => {
                self.color_label.paint(ctx);
                self.color_management_inherit_dropdown.paint(ctx);
                self.color_space_dropdown.paint(ctx);
                self.output_color_space_dropdown.paint(ctx);
                self.color_workflow_dropdown.paint(ctx);
                self.missing_color_metadata_dropdown.paint(ctx);
                self.nested_color_processing_dropdown.paint(ctx);
                self.video_range_dropdown.paint(ctx);
                self.delivery_bit_depth_dropdown.paint(ctx);
                self.auto_tone_map_checkbox.paint(ctx);
                self.static_hdr_metadata_policy_checkbox.paint(ctx);
            }
            SequenceSettingsTabPayload::Preview => {
                self.preview_label.paint(ctx);
                self.preview_render_format_dropdown.paint(ctx);
                self.preview_cache_checkbox.paint(ctx);
                self.preview_scale_label.paint(ctx);
                self.preview_scale_slider.paint(ctx);
            }
        }
        self.cancel_button.paint(ctx);
        self.apply_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        41
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title_label),
            1 => Some(&self.description_label),
            2..=4 => self.tab_buttons.get(index - 2).map(|button| button as &dyn Widget),
            5 => Some(&self.name_label),
            6 => Some(&self.format_label),
            7 => Some(&self.frame_size_label),
            8 => Some(&self.start_timecode_label),
            9 => Some(&self.audio_label),
            10 => Some(&self.preview_label),
            11 => Some(&self.color_label),
            12 => Some(&self.preview_scale_label),
            13 => Some(&self.name_input),
            14 => Some(&self.editing_mode_dropdown),
            15 => Some(&self.resolution_dropdown),
            16 => Some(&self.resolution_width_input),
            17 => Some(&self.resolution_height_input),
            18 => Some(&self.frame_rate_dropdown),
            19 => Some(&self.pixel_aspect_ratio_dropdown),
            20 => Some(&self.field_order_dropdown),
            21 => Some(&self.timeline_display_format_dropdown),
            22 => Some(&self.start_timecode_input),
            23 => Some(&self.color_space_dropdown),
            24 => Some(&self.output_color_space_dropdown),
            25 => Some(&self.color_management_inherit_dropdown),
            26 => Some(&self.color_workflow_dropdown),
            27 => Some(&self.missing_color_metadata_dropdown),
            28 => Some(&self.nested_color_processing_dropdown),
            29 => Some(&self.video_range_dropdown),
            30 => Some(&self.delivery_bit_depth_dropdown),
            31 => Some(&self.auto_tone_map_checkbox),
            32 => Some(&self.static_hdr_metadata_policy_checkbox),
            33 => Some(&self.audio_sample_rate_dropdown),
            34 => Some(&self.audio_channel_layout_dropdown),
            35 => Some(&self.audio_display_format_dropdown),
            36 => Some(&self.preview_render_format_dropdown),
            37 => Some(&self.preview_cache_checkbox),
            38 => Some(&self.preview_scale_slider),
            39 => Some(&self.cancel_button),
            40 => Some(&self.apply_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title_label),
            1 => Some(&mut self.description_label),
            2..=4 => self.tab_buttons.get_mut(index - 2).map(|button| button as &mut dyn Widget),
            5 => Some(&mut self.name_label),
            6 => Some(&mut self.format_label),
            7 => Some(&mut self.frame_size_label),
            8 => Some(&mut self.start_timecode_label),
            9 => Some(&mut self.audio_label),
            10 => Some(&mut self.preview_label),
            11 => Some(&mut self.color_label),
            12 => Some(&mut self.preview_scale_label),
            13 => Some(&mut self.name_input),
            14 => Some(&mut self.editing_mode_dropdown),
            15 => Some(&mut self.resolution_dropdown),
            16 => Some(&mut self.resolution_width_input),
            17 => Some(&mut self.resolution_height_input),
            18 => Some(&mut self.frame_rate_dropdown),
            19 => Some(&mut self.pixel_aspect_ratio_dropdown),
            20 => Some(&mut self.field_order_dropdown),
            21 => Some(&mut self.timeline_display_format_dropdown),
            22 => Some(&mut self.start_timecode_input),
            23 => Some(&mut self.color_space_dropdown),
            24 => Some(&mut self.output_color_space_dropdown),
            25 => Some(&mut self.color_management_inherit_dropdown),
            26 => Some(&mut self.color_workflow_dropdown),
            27 => Some(&mut self.missing_color_metadata_dropdown),
            28 => Some(&mut self.nested_color_processing_dropdown),
            29 => Some(&mut self.video_range_dropdown),
            30 => Some(&mut self.delivery_bit_depth_dropdown),
            31 => Some(&mut self.auto_tone_map_checkbox),
            32 => Some(&mut self.static_hdr_metadata_policy_checkbox),
            33 => Some(&mut self.audio_sample_rate_dropdown),
            34 => Some(&mut self.audio_channel_layout_dropdown),
            35 => Some(&mut self.audio_display_format_dropdown),
            36 => Some(&mut self.preview_render_format_dropdown),
            37 => Some(&mut self.preview_cache_checkbox),
            38 => Some(&mut self.preview_scale_slider),
            39 => Some(&mut self.cancel_button),
            40 => Some(&mut self.apply_button),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_picker_exposes_rendering_domain_without_duplicating_project_color_modes() {
        assert_eq!(ColorWorkflow::default(), ColorWorkflow::SceneReferred);
        assert_eq!(
            COLOR_WORKFLOW_OPTIONS,
            [ColorWorkflow::SceneReferred, ColorWorkflow::DisplayReferred]
        );
        assert!(COLOR_WORKFLOW_OPTIONS
            .iter()
            .map(|workflow| color_workflow_label(*workflow))
            .all(|label| !label.contains("ACES") && !label.contains("OpenColorIO")));
    }

    #[test]
    fn program_output_picker_only_offers_versioned_standard_targets() {
        assert_eq!(
            PROGRAM_OUTPUT_COLOR_SPACE_OPTIONS,
            [
                ColorSpace::Rec709,
                ColorSpace::Rec2100Hlg,
                ColorSpace::Rec2100Pq,
                ColorSpace::Srgb,
                ColorSpace::Rec2020,
                ColorSpace::DisplayP3,
            ]
        );
    }

    #[test]
    fn working_space_control_respects_the_local_engine_contract() {
        let mut standard_sequence = Sequence::new("Standard");
        standard_sequence.settings.color_management.inherit = false;
        let mut standard = AppUiSequenceSettingsDraft::from_sequence(&standard_sequence);

        assert!(!color_space_dropdown_for(&standard).is_enabled());
        standard.apply_update(SequenceSettingsDraftUpdatePayload::WorkingColorSpace(
            WorkingColorSpace::LinearP3D65,
        ));
        assert_eq!(
            standard.settings.working_color_space,
            WorkingColorSpace::LinearRec2020
        );

        let mut aces_sequence = Sequence::new("ACES");
        aces_sequence.settings.color_management.inherit = false;
        aces_sequence.settings.color_management.engine = mondrian_core::ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        };
        let mut aces = AppUiSequenceSettingsDraft::from_sequence(&aces_sequence);

        assert!(color_space_dropdown_for(&aces).is_enabled());
        aces.apply_update(SequenceSettingsDraftUpdatePayload::WorkingColorSpace(
            WorkingColorSpace::AcesCg,
        ));
        assert_eq!(aces.settings.working_color_space, WorkingColorSpace::AcesCg);
    }

    #[test]
    fn timeline_display_draft_keeps_a_signed_origin_and_repairs_invalid_drop_frame_rate() {
        let mut sequence = Sequence::new("Drop frame");
        sequence.settings.frame_rate = Rational::FPS_2997;
        sequence.settings.timeline_display.format =
            TimelineDisplayFormat::Timecode(SmpteCountingMode::DropFrame);
        let mut draft = AppUiSequenceSettingsDraft::from_sequence(&sequence);

        draft.apply_update(SequenceSettingsDraftUpdatePayload::StartTimecodeFrame(
            -1_800,
        ));
        assert_eq!(draft.settings.timeline_display.timecode_start_frame, -1_800);

        draft.apply_update(SequenceSettingsDraftUpdatePayload::FrameRate(
            Rational::FPS_25,
        ));
        assert_eq!(draft.settings.frame_rate, Rational::FPS_25);
        assert_eq!(
            draft.settings.timeline_display.format,
            TimelineDisplayFormat::Timecode(SmpteCountingMode::NonDropFrame)
        );
        draft.settings.validate().expect("repaired draft must remain valid");
    }

    #[test]
    fn drop_frame_menu_is_available_only_for_exact_supported_rates() {
        let has_drop_frame = |rate| {
            timeline_display_format_items(rate)
                .iter()
                .any(|item| item.label == "SMPTE drop-frame")
        };

        assert!(has_drop_frame(Rational::FPS_2997));
        assert!(has_drop_frame(Rational::FPS_5994));
        assert!(!has_drop_frame(Rational::FPS_25));
        assert!(!has_drop_frame(Rational::FPS_30));
    }
}
