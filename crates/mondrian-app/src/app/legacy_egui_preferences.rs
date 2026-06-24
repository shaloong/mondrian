use std::path::PathBuf;

use mondrian_core::types::{ColorEngine, ColorSpace, OcioConfigSource};
use mondrian_media::audio::ClockRole;
use mondrian_timeline::sequence::{
    AudioChannelLayout, ColorWorkflow, EditingMode, ExportBitDepth, FieldOrder,
    MissingColorMetadataPolicy, NestedColorProcessing, PixelAspectRatio, PreviewRenderFormat,
    SequencePreset, SequenceSettings, VideoDisplayFormat, VideoRange,
};
use serde::{Deserialize, Serialize};

use crate::app::viewer_preferences::ViewerPreferences;
use crate::shortcuts::ShortcutPreferences;

/// Legacy egui new-project dialog tab state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::app) enum NewProjectTab {
    #[default]
    Basic,
    Timeline,
    Color,
    Audio,
    Advanced,
}

/// Legacy egui colour preset that drives all derived new-project colour settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(in crate::app) enum ColorMode {
    #[default]
    Sdr,
    HdrPq,
    HdrHlg,
    Aces,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(in crate::app) struct NewProjectDraft {
    #[serde(default)]
    pub(in crate::app) name: String,
    #[serde(default = "default_new_project_width")]
    pub(in crate::app) width: u32,
    #[serde(default = "default_new_project_height")]
    pub(in crate::app) height: u32,
    #[serde(default = "default_new_project_fps_num")]
    pub(in crate::app) fps_num: i64,
    #[serde(default = "default_new_project_fps_den")]
    pub(in crate::app) fps_den: i64,
    #[serde(default)]
    pub(in crate::app) color_mode: ColorMode,
    #[serde(default = "default_new_project_audio_sample_rate")]
    pub(in crate::app) audio_sample_rate: u32,
    #[serde(default)]
    pub(in crate::app) audio_channel_layout: AudioChannelLayout,
    #[serde(default)]
    pub(in crate::app) start_timecode_frame: i64,
    #[serde(default)]
    pub(in crate::app) video_display_format: VideoDisplayFormat,
    #[serde(default)]
    pub(in crate::app) pixel_aspect_ratio: PixelAspectRatio,
    #[serde(default)]
    pub(in crate::app) field_order: FieldOrder,
    #[serde(default)]
    pub(in crate::app) color_workflow: ColorWorkflow,
    #[serde(default)]
    pub(in crate::app) engine: ColorEngine,
    #[serde(default)]
    pub(in crate::app) missing_color_metadata_policy: MissingColorMetadataPolicy,
    #[serde(default)]
    pub(in crate::app) nested_color_processing: NestedColorProcessing,
    #[serde(default)]
    pub(in crate::app) preserve_hdr_metadata: bool,
    #[serde(default)]
    pub(in crate::app) video_range: VideoRange,
    #[serde(default)]
    pub(in crate::app) preview_format: PreviewRenderFormat,
    #[serde(default = "default_new_project_preview_resolution_scale")]
    pub(in crate::app) preview_resolution_scale: f32,
    #[serde(default = "default_new_project_preview_cache_enabled")]
    pub(in crate::app) preview_cache_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::app) enum PreferencesTab {
    #[default]
    General,
    Media,
    Shortcuts,
    Developer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum PendingCloseAction {
    CloseProject,
    QuitApp,
}

/// Persisted preferences for the legacy egui reference app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(in crate::app) struct AppPreferences {
    pub(in crate::app) version: u32,
    #[serde(default = "default_app_theme")]
    pub(in crate::app) theme: crate::egui_ui::theme::Theme,
    #[serde(default = "default_show_effect_controls")]
    pub(in crate::app) show_effect_controls: bool,
    #[serde(default = "default_show_effect_library")]
    pub(in crate::app) show_effect_library: bool,
    pub(in crate::app) show_library: bool,
    pub(in crate::app) auto_proxy_enabled: bool,
    pub(in crate::app) show_dev_metrics: bool,
    pub(in crate::app) av_clock_role: ClockRole,
    pub(in crate::app) new_project_draft: NewProjectDraft,
    #[serde(default)]
    pub(in crate::app) sequence_presets: Vec<SequencePreset>,
    #[serde(default)]
    pub(in crate::app) shortcuts: ShortcutPreferences,
    #[serde(default = "default_media_cache_auto_cleanup")]
    pub(in crate::app) media_cache_auto_cleanup: bool,
    #[serde(default = "default_media_cache_max_size_gb")]
    pub(in crate::app) media_cache_max_size_gb: u32,
    #[serde(default = "default_media_cache_max_age_days")]
    pub(in crate::app) media_cache_max_age_days: u32,
    #[serde(default = "default_show_video_metrics")]
    pub(in crate::app) show_video_metrics: bool,
    #[serde(default = "default_show_audio_metrics")]
    pub(in crate::app) show_audio_metrics: bool,
    #[serde(default = "default_timeline_panel_height")]
    pub(in crate::app) timeline_panel_height: f32,
    #[serde(default = "default_auto_save_enabled")]
    pub(in crate::app) auto_save_enabled: bool,
    #[serde(default = "default_auto_save_interval_secs")]
    pub(in crate::app) auto_save_interval_secs: u32,
    #[serde(default = "default_auto_save_max_recovery_points")]
    pub(in crate::app) auto_save_max_recovery_points: u32,
    #[serde(default = "default_auto_save_retention_days")]
    pub(in crate::app) auto_save_retention_days: u32,
    #[serde(default)]
    pub(in crate::app) recent_projects: Vec<PathBuf>,
    pub(in crate::app) viewer: ViewerPreferences,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            theme: default_app_theme(),
            show_effect_controls: default_show_effect_controls(),
            show_effect_library: default_show_effect_library(),
            show_library: true,
            auto_proxy_enabled: false,
            show_dev_metrics: false,
            av_clock_role: ClockRole::AudioMaster,
            new_project_draft: NewProjectDraft::default(),
            sequence_presets: builtin_sequence_presets(),
            shortcuts: ShortcutPreferences::default(),
            media_cache_auto_cleanup: default_media_cache_auto_cleanup(),
            media_cache_max_size_gb: default_media_cache_max_size_gb(),
            media_cache_max_age_days: default_media_cache_max_age_days(),
            show_video_metrics: default_show_video_metrics(),
            show_audio_metrics: default_show_audio_metrics(),
            timeline_panel_height: default_timeline_panel_height(),
            auto_save_enabled: default_auto_save_enabled(),
            auto_save_interval_secs: default_auto_save_interval_secs(),
            auto_save_max_recovery_points: default_auto_save_max_recovery_points(),
            auto_save_retention_days: default_auto_save_retention_days(),
            recent_projects: Vec::new(),
            viewer: ViewerPreferences::default(),
        }
    }
}

impl Default for NewProjectDraft {
    fn default() -> Self {
        Self {
            name: "未命名项目".to_string(),
            width: default_new_project_width(),
            height: default_new_project_height(),
            fps_num: default_new_project_fps_num(),
            fps_den: default_new_project_fps_den(),
            color_mode: ColorMode::default(),
            audio_sample_rate: default_new_project_audio_sample_rate(),
            audio_channel_layout: AudioChannelLayout::Stereo,
            start_timecode_frame: 0,
            video_display_format: VideoDisplayFormat::Frames,
            pixel_aspect_ratio: PixelAspectRatio::Square,
            field_order: FieldOrder::Progressive,
            color_workflow: ColorWorkflow::DisplayReferred,
            engine: ColorEngine::default(),
            missing_color_metadata_policy: MissingColorMetadataPolicy::AssumeRec709,
            nested_color_processing: NestedColorProcessing::PreserveChildWorkingSpace,
            preserve_hdr_metadata: false,
            video_range: VideoRange::Full,
            preview_format: PreviewRenderFormat::IFrameOnly,
            preview_resolution_scale: default_new_project_preview_resolution_scale(),
            preview_cache_enabled: default_new_project_preview_cache_enabled(),
        }
    }
}

impl NewProjectDraft {
    /// Apply `ColorMode`-driven defaults to this draft.
    pub(in crate::app) fn apply_color_mode_defaults(&mut self) {
        match self.color_mode {
            ColorMode::Sdr => {
                self.color_workflow = ColorWorkflow::DisplayReferred;
                self.engine = ColorEngine::MondrianSmart;
                self.preserve_hdr_metadata = false;
            }
            ColorMode::HdrPq => {
                self.color_workflow = ColorWorkflow::SceneReferred;
                self.preserve_hdr_metadata = true;
            }
            ColorMode::HdrHlg => {
                self.color_workflow = ColorWorkflow::SceneReferred;
                self.preserve_hdr_metadata = true;
            }
            ColorMode::Aces => {
                self.color_workflow = ColorWorkflow::Aces;
                self.engine = ColorEngine::Ocio {
                    source: OcioConfigSource::Builtin { name: "aces_1.2".into() },
                };
                self.preserve_hdr_metadata = false;
            }
        }
    }

    /// Resolve the working colour space from the colour mode.
    pub(in crate::app) fn working_color_space(&self) -> ColorSpace {
        match self.color_mode {
            ColorMode::Sdr => ColorSpace::Rec709,
            ColorMode::HdrPq => ColorSpace::Rec2100Pq,
            ColorMode::HdrHlg => ColorSpace::Rec2100Hlg,
            ColorMode::Aces => ColorSpace::Rec2020,
        }
    }

    /// Resolve the output colour space from the colour mode.
    pub(in crate::app) fn output_color_space(&self) -> ColorSpace {
        self.working_color_space()
    }

    /// Resolve the export bit depth from the colour mode.
    pub(in crate::app) fn export_bit_depth(&self) -> ExportBitDepth {
        match self.color_mode {
            ColorMode::Sdr => ExportBitDepth::Eight,
            ColorMode::HdrPq | ColorMode::HdrHlg | ColorMode::Aces => ExportBitDepth::SixteenFloat,
        }
    }
}

pub(in crate::app) const fn default_new_project_width() -> u32 {
    1920
}

pub(in crate::app) const fn default_new_project_height() -> u32 {
    1080
}

pub(in crate::app) const fn default_new_project_fps_num() -> i64 {
    25
}

pub(in crate::app) const fn default_new_project_fps_den() -> i64 {
    1
}

pub(in crate::app) const fn default_new_project_audio_sample_rate() -> u32 {
    48_000
}

pub(in crate::app) const fn default_new_project_preview_resolution_scale() -> f32 {
    0.5
}

pub(in crate::app) const fn default_new_project_preview_cache_enabled() -> bool {
    true
}

pub(in crate::app) fn builtin_sequence_presets() -> Vec<SequencePreset> {
    [
        (EditingMode::Dslr1080p, "DSLR 1080p"),
        (EditingMode::Dslr720p, "DSLR 720p"),
        (EditingMode::Avchd1080p, "AVCHD 1080p"),
        (EditingMode::DigitalCinema4k, "Digital Cinema 4K"),
        (EditingMode::SocialVertical1080p, "社媒竖屏 1080p"),
    ]
    .into_iter()
    .filter_map(|(mode, name)| {
        SequencePreset::new(name, SequenceSettings::from_editing_mode(mode)).ok()
    })
    .collect()
}

pub(in crate::app) const fn default_media_cache_auto_cleanup() -> bool {
    false
}

pub(in crate::app) const fn default_app_theme() -> crate::egui_ui::theme::Theme {
    crate::egui_ui::theme::Theme::System
}

pub(in crate::app) const fn default_show_effect_controls() -> bool {
    true
}

pub(in crate::app) const fn default_show_effect_library() -> bool {
    true
}

pub(in crate::app) const fn default_media_cache_max_size_gb() -> u32 {
    60
}

pub(in crate::app) const fn default_media_cache_max_age_days() -> u32 {
    30
}

pub(in crate::app) const fn default_show_video_metrics() -> bool {
    true
}

pub(in crate::app) const fn default_show_audio_metrics() -> bool {
    true
}

pub(in crate::app) const fn default_timeline_panel_height() -> f32 {
    286.0
}

pub(in crate::app) const fn default_auto_save_enabled() -> bool {
    true
}

pub(in crate::app) const fn default_auto_save_interval_secs() -> u32 {
    60
}

pub(in crate::app) const fn default_auto_save_max_recovery_points() -> u32 {
    10
}

pub(in crate::app) const fn default_auto_save_retention_days() -> u32 {
    7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_legacy_preferences_keep_safe_values() {
        let preferences = AppPreferences::default();

        assert_eq!(preferences.version, 1);
        assert_eq!(preferences.theme, crate::egui_ui::theme::Theme::System);
        assert!(preferences.show_effect_controls);
        assert!(preferences.show_effect_library);
        assert!(preferences.show_library);
        assert_eq!(preferences.media_cache_max_size_gb, 60);
        assert_eq!(preferences.media_cache_max_age_days, 30);
        assert_eq!(preferences.timeline_panel_height, 286.0);
        assert_eq!(preferences.auto_save_interval_secs, 60);
        assert_eq!(preferences.auto_save_max_recovery_points, 10);
        assert_eq!(preferences.auto_save_retention_days, 7);
        assert!(!preferences.sequence_presets.is_empty());
    }

    #[test]
    fn legacy_new_project_color_mode_derives_consistent_color_settings() {
        let mut draft = NewProjectDraft {
            color_mode: ColorMode::Aces,
            ..NewProjectDraft::default()
        };

        draft.apply_color_mode_defaults();

        assert_eq!(draft.color_workflow, ColorWorkflow::Aces);
        assert_eq!(draft.working_color_space(), ColorSpace::Rec2020);
        assert_eq!(draft.output_color_space(), ColorSpace::Rec2020);
        assert_eq!(draft.export_bit_depth(), ExportBitDepth::SixteenFloat);
        assert!(!draft.preserve_hdr_metadata);
    }

    #[test]
    fn missing_legacy_preferences_fields_deserialize_to_defaults() {
        let json = r#"{
            "version": 1,
            "show_library": true,
            "auto_proxy_enabled": false,
            "show_dev_metrics": false,
            "av_clock_role": "AudioMaster",
            "new_project_draft": {},
            "viewer": {}
        }"#;

        let preferences: AppPreferences =
            serde_json::from_str(json).expect("legacy preferences should deserialize");

        assert_eq!(preferences.theme, crate::egui_ui::theme::Theme::System);
        assert!(preferences.show_effect_controls);
        assert!(preferences.show_effect_library);
        assert!(preferences.shortcuts.open_project.is_some());
        assert_eq!(
            preferences.media_cache_max_size_gb,
            default_media_cache_max_size_gb()
        );
        assert_eq!(
            preferences.media_cache_max_age_days,
            default_media_cache_max_age_days()
        );
        assert_eq!(
            preferences.timeline_panel_height,
            default_timeline_panel_height()
        );
        assert!(preferences.auto_save_enabled);
    }
}
