//! Persistent preferences for the app UI product shell.
//!
//! This store belongs to the application adapter layer. Reusable widgets receive
//! typed models/actions only; they never read or write disk directly.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mondrian_editor_state::state::WorkspacePreset;
use mondrian_ui_theme::ThemePreference;
use mondrian_ui_widgets::{VideoScopesSettings, ViewerCanvasBackground, WaveformDisplay};
use serde::{Deserialize, Serialize};

use crate::app::app_data_dir;
use crate::app_ui::shortcuts::{is_known_shortcut_id, AppUiShortcutOverride};
use crate::app_ui::workspace_layout::AppUiWorkspaceLayout;

const APP_UI_PREFERENCES_FILE: &str = "app_ui_preferences.json";
const APP_UI_PREFERENCES_VERSION: u32 = 1;
/// Maximum number of recent projects kept by the app UI startup surface.
pub const MAX_RECENT_PROJECTS: usize = 12;

/// Versioned user preferences owned by the app UI shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppUiPreferences {
    /// Schema version for future non-compatible alpha migrations.
    pub version: u32,
    /// Active product theme preference. `System` resolves to a concrete preset
    /// at runtime and is not persisted as a third theme token set.
    pub theme_preference: ThemePreference,
    /// Built-in workspace preset restored when the app UI shell opens.
    pub workspace_preset: WorkspacePreset,
    /// Most recently opened project files for the startup surface.
    pub recent_projects: Vec<PathBuf>,
    /// User overrides for app UI shell shortcut descriptors.
    #[serde(default)]
    pub shortcut_overrides: Vec<AppUiShortcutOverride>,
    /// Persisted custom dock layout for the app UI workspace.
    #[serde(default)]
    pub custom_workspace_layout: Option<AppUiWorkspaceLayout>,
    /// Waveform display mode for audio clips on the timeline.
    #[serde(default)]
    pub waveform_display: WaveformDisplay,
    /// Presentation-only background visible through transparent Viewer pixels.
    #[serde(default)]
    pub viewer_canvas_background: ViewerCanvasBackground,
    /// Machine-local professional Scopes analysis and presentation controls.
    #[serde(default)]
    pub video_scopes: VideoScopesSettings,
    /// Runtime output-device intent. This is a user preference, never Project state.
    #[serde(default)]
    pub audio_output_device: mondrian_media::RealtimeAudioOutputDeviceSelection,
    /// Machine-local professional output routing intent. Loading preferences
    /// never auto-acquires a DeckLink/AJA device.
    #[serde(default)]
    pub reference_output: mondrian_reference_output::ReferenceOutputRoutingPreferences,
    /// Machine-local Viewer monitor target, ICC calibration, and HDR policy.
    /// This value never enters `.mdp` Project authoring state.
    #[serde(default)]
    pub display_management: mondrian_core::DisplayManagementPolicy,
}

impl Default for AppUiPreferences {
    fn default() -> Self {
        Self {
            version: APP_UI_PREFERENCES_VERSION,
            theme_preference: ThemePreference::System,
            workspace_preset: WorkspacePreset::Editing,
            recent_projects: Vec::new(),
            shortcut_overrides: Vec::new(),
            custom_workspace_layout: None,
            waveform_display: WaveformDisplay::BottomAligned,
            viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
            video_scopes: VideoScopesSettings::default(),
            audio_output_device: mondrian_media::RealtimeAudioOutputDeviceSelection::SystemDefault,
            reference_output: Default::default(),
            display_management: mondrian_core::DisplayManagementPolicy::default(),
        }
    }
}

impl AppUiPreferences {
    /// Record a project path at the front of the recent list.
    pub fn record_recent_project(&mut self, project_file: PathBuf) {
        self.recent_projects.retain(|existing| existing != &project_file);
        self.recent_projects.insert(0, project_file);
        self.recent_projects.truncate(MAX_RECENT_PROJECTS);
    }

    fn sanitize_loaded(mut self) -> Self {
        if self.version != APP_UI_PREFERENCES_VERSION {
            return Self::default();
        }

        let mut sanitized = Vec::new();
        for path in self.recent_projects {
            if !path.exists() || sanitized.contains(&path) {
                continue;
            }
            sanitized.push(path);
            if sanitized.len() == MAX_RECENT_PROJECTS {
                break;
            }
        }
        self.recent_projects = sanitized;

        let mut shortcut_overrides = Vec::new();
        for entry in self.shortcut_overrides {
            if !is_known_shortcut_id(&entry.id)
                || shortcut_overrides
                    .iter()
                    .any(|existing: &AppUiShortcutOverride| existing.id == entry.id)
            {
                continue;
            }
            shortcut_overrides.push(entry);
        }
        self.shortcut_overrides = shortcut_overrides;

        self.custom_workspace_layout =
            self.custom_workspace_layout.and_then(AppUiWorkspaceLayout::sanitized);
        if self.workspace_preset == WorkspacePreset::Custom
            && !self
                .custom_workspace_layout
                .as_ref()
                .is_some_and(AppUiWorkspaceLayout::is_split_root)
        {
            self.workspace_preset = WorkspacePreset::Editing;
            self.custom_workspace_layout = None;
        }
        self
    }
}

/// Default app UI preferences path under Mondrian's app data directory.
pub fn app_ui_preferences_path() -> PathBuf {
    app_data_dir().join(APP_UI_PREFERENCES_FILE)
}

/// Load app UI preferences from the default product path.
pub fn load_app_ui_preferences() -> AppUiPreferences {
    load_app_ui_preferences_from(&app_ui_preferences_path())
}

/// Load app UI preferences from an explicit path, returning defaults when
/// the file is absent or malformed.
pub fn load_app_ui_preferences_from(path: &Path) -> AppUiPreferences {
    let Ok(bytes) = fs::read(path) else {
        return AppUiPreferences::default();
    };
    serde_json::from_slice::<AppUiPreferences>(&bytes)
        .map(AppUiPreferences::sanitize_loaded)
        .unwrap_or_default()
}

/// Persist app UI preferences to the default product path.
pub fn persist_app_ui_preferences(preferences: &AppUiPreferences) -> io::Result<()> {
    persist_app_ui_preferences_to(&app_ui_preferences_path(), preferences)
}

/// Persist app UI preferences to an explicit path.
pub fn persist_app_ui_preferences_to(
    path: &Path,
    preferences: &AppUiPreferences,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(preferences).map_err(io::Error::other)?;
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_ui::workspace_layout::AppUiWorkspaceLayout;
    use mondrian_ui_core::types::SplitDirection;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_preferences_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("mondrian-{name}-{nanos}.json"))
    }

    #[test]
    fn missing_preferences_file_uses_defaults() {
        let path = temp_preferences_path("missing-preferences");

        let preferences = load_app_ui_preferences_from(&path);

        assert_eq!(preferences, AppUiPreferences::default());
    }

    #[test]
    fn malformed_preferences_file_uses_defaults() {
        let path = temp_preferences_path("malformed-preferences");
        fs::write(&path, b"{not json").expect("write malformed fixture");

        let preferences = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(preferences, AppUiPreferences::default());
    }

    #[test]
    fn incompatible_preferences_version_uses_defaults() {
        let path = temp_preferences_path("future-preferences");
        fs::write(
            &path,
            serde_json::to_vec(&AppUiPreferences {
                version: APP_UI_PREFERENCES_VERSION + 1,
                theme_preference: ThemePreference::Light,
                workspace_preset: WorkspacePreset::Export,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                video_scopes: Default::default(),
                audio_output_device: Default::default(),
                reference_output: Default::default(),
                display_management: Default::default(),
            })
            .expect("serialize preferences"),
        )
        .expect("write future preferences");

        let preferences = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(preferences, AppUiPreferences::default());
    }

    #[test]
    fn preferences_round_trip_to_disk() {
        let path = temp_preferences_path("round-trip-preferences");
        let project_path = temp_preferences_path("round-trip-project").with_extension("mdp");
        let preferences = AppUiPreferences {
            version: 1,
            theme_preference: ThemePreference::Light,
            workspace_preset: WorkspacePreset::Compositing,
            recent_projects: vec![project_path.clone()],
            shortcut_overrides: vec![AppUiShortcutOverride {
                id: "panel.inspector".to_owned(),
                binding: None,
            }],
            waveform_display: WaveformDisplay::BottomAligned,
            viewer_canvas_background: ViewerCanvasBackground::Black,
            video_scopes: VideoScopesSettings {
                waveform_mode: mondrian_core::WaveformMode::RgbParade,
                scale: mondrian_core::ProgramScopeScale::Nits1000,
                tap: mondrian_core::ProgramScopesTap::MonitorOutput,
                layout: mondrian_ui_widgets::VideoScopesLayout::Grid,
                show_skin_tone_line: false,
                show_color_targets: true,
                monitoring: mondrian_core::SignalMonitoringSettings {
                    false_color: true,
                    zebra: true,
                    gamut_alarm: true,
                    zebra_lower_per_mille: 850,
                    zebra_upper_per_mille: 980,
                },
            },
            audio_output_device: mondrian_media::RealtimeAudioOutputDeviceSelection::Specific {
                device_id: mondrian_media::RealtimeAudioOutputDeviceId::new(
                    "wasapi:round-trip-device",
                )
                .expect("fixture device identity"),
            },
            reference_output: mondrian_reference_output::ReferenceOutputRoutingPreferences {
                provider: Some(mondrian_reference_output::ReferenceOutputProvider::DeckLink),
                device_id: Some(
                    mondrian_reference_output::ReferenceOutputDeviceId::new(
                        "decklink:round-trip-device",
                    )
                    .expect("fixture reference-output identity"),
                ),
                pixel_format:
                    mondrian_reference_output::ReferenceOutputPixelFormat::Rgb444TwelveIn16Le,
                reference_policy:
                    mondrian_reference_output::ReferenceOutputReferencePolicy::RequireExternalLock,
            },
            display_management: mondrian_core::DisplayManagementPolicy::default()
                .with_calibration(mondrian_core::DisplayCalibrationPolicy::OsDefault)
                .expect("OS default ICC policy")
                .with_icc_rendering_intent(mondrian_core::IccRenderingIntent::RelativeColorimetric),
            custom_workspace_layout: Some(AppUiWorkspaceLayout::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.37,
                first: Box::new(AppUiWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Assets,
                    active_index: 1,
                    hidden_tabs: Vec::new(),
                    tabs: vec![
                        mondrian_editor_state::state::PanelKind::Assets,
                        mondrian_editor_state::state::PanelKind::Effects,
                    ],
                }),
                second: Box::new(AppUiWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Viewer,
                    active_index: 0,
                    hidden_tabs: Vec::new(),
                    tabs: vec![mondrian_editor_state::state::PanelKind::Viewer],
                }),
            }),
        };

        fs::write(&project_path, b"project").expect("write recent project fixture");
        persist_app_ui_preferences_to(&path, &preferences).expect("persist preferences");
        let loaded = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();
        fs::remove_file(project_path).ok();

        assert_eq!(loaded, preferences);
    }

    #[test]
    fn legacy_preferences_without_display_policy_restore_safe_default() {
        let path = temp_preferences_path("legacy-display-policy");
        let mut value = serde_json::to_value(AppUiPreferences::default())
            .expect("serialize current preferences");
        value.as_object_mut().expect("preferences object").remove("display_management");
        fs::write(
            &path,
            serde_json::to_vec(&value).expect("serialize legacy preferences"),
        )
        .expect("write legacy preferences");

        let loaded = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(
            loaded.display_management,
            mondrian_core::DisplayManagementPolicy::default()
        );
    }

    #[test]
    fn legacy_preferences_without_reference_output_restore_disabled_routing() {
        let path = temp_preferences_path("legacy-reference-output");
        let mut value = serde_json::to_value(AppUiPreferences::default())
            .expect("serialize current preferences");
        value.as_object_mut().expect("preferences object").remove("reference_output");
        fs::write(
            &path,
            serde_json::to_vec(&value).expect("serialize legacy preferences"),
        )
        .expect("write legacy preferences");

        let loaded = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(
            loaded.reference_output,
            mondrian_reference_output::ReferenceOutputRoutingPreferences::default()
        );
    }

    #[test]
    fn legacy_preferences_without_scopes_controls_restore_professional_defaults() {
        let path = temp_preferences_path("legacy-scopes-controls");
        let mut value = serde_json::to_value(AppUiPreferences::default())
            .expect("serialize current preferences");
        value.as_object_mut().expect("preferences object").remove("video_scopes");
        fs::write(
            &path,
            serde_json::to_vec(&value).expect("serialize legacy preferences"),
        )
        .expect("write legacy preferences");

        let loaded = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(loaded.video_scopes, VideoScopesSettings::default());
    }

    #[test]
    fn loading_preferences_sanitizes_custom_workspace_layout() {
        let path = temp_preferences_path("custom-layout-filter");
        fs::write(
            &path,
            serde_json::to_vec(&AppUiPreferences {
                version: 1,
                theme_preference: ThemePreference::Dark,
                workspace_preset: WorkspacePreset::Custom,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                video_scopes: Default::default(),
                audio_output_device: Default::default(),
                reference_output: Default::default(),
                display_management: Default::default(),
                custom_workspace_layout: Some(AppUiWorkspaceLayout::Split {
                    direction: SplitDirection::Vertical,
                    ratio: 12.0,
                    first: Box::new(AppUiWorkspaceLayout::Panel {
                        kind: mondrian_editor_state::state::PanelKind::Assets,
                        active_index: 99,
                        hidden_tabs: Vec::new(),
                        tabs: Vec::new(),
                    }),
                    second: Box::new(AppUiWorkspaceLayout::Panel {
                        kind: mondrian_editor_state::state::PanelKind::Timeline,
                        active_index: 2,
                        hidden_tabs: Vec::new(),
                        tabs: Vec::new(),
                    }),
                }),
            })
            .expect("serialize preferences"),
        )
        .expect("write preferences");

        let preferences = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(
            preferences.custom_workspace_layout,
            Some(AppUiWorkspaceLayout::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.9,
                first: Box::new(AppUiWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Assets,
                    active_index: 1,
                    hidden_tabs: Vec::new(),
                    tabs: vec![
                        mondrian_editor_state::state::PanelKind::Assets,
                        mondrian_editor_state::state::PanelKind::Effects,
                    ],
                }),
                second: Box::new(AppUiWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Timeline,
                    active_index: 0,
                    hidden_tabs: Vec::new(),
                    tabs: vec![mondrian_editor_state::state::PanelKind::Timeline],
                }),
            })
        );
    }

    #[test]
    fn loading_custom_workspace_without_split_root_downgrades_to_editing() {
        let path = temp_preferences_path("custom-panel-root-filter");
        fs::write(
            &path,
            serde_json::to_vec(&AppUiPreferences {
                version: APP_UI_PREFERENCES_VERSION,
                theme_preference: ThemePreference::Dark,
                workspace_preset: WorkspacePreset::Custom,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                video_scopes: Default::default(),
                audio_output_device: Default::default(),
                reference_output: Default::default(),
                display_management: Default::default(),
                custom_workspace_layout: Some(AppUiWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Assets,
                    active_index: 0,
                    hidden_tabs: vec![mondrian_editor_state::state::PanelKind::Effects],
                    tabs: vec![
                        mondrian_editor_state::state::PanelKind::Assets,
                        mondrian_editor_state::state::PanelKind::Effects,
                    ],
                }),
            })
            .expect("serialize preferences"),
        )
        .expect("write preferences");

        let preferences = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(preferences.workspace_preset, WorkspacePreset::Editing);
        assert_eq!(preferences.custom_workspace_layout, None);
    }

    #[test]
    fn partial_preferences_file_uses_clean_defaults() {
        let path = temp_preferences_path("partial-preferences");
        fs::write(&path, br#"{"version":1,"theme_preference":"Light"}"#)
            .expect("write partial fixture");

        let preferences = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(preferences, AppUiPreferences::default());
    }

    #[test]
    fn recording_recent_projects_deduplicates_and_truncates() {
        let mut preferences = AppUiPreferences::default();

        for index in 0..(MAX_RECENT_PROJECTS + 2) {
            preferences.record_recent_project(PathBuf::from(format!("E:/projects/{index}.mdp")));
        }
        let repeated = PathBuf::from("E:/projects/4.mdp");
        preferences.record_recent_project(repeated.clone());

        assert_eq!(preferences.recent_projects.len(), MAX_RECENT_PROJECTS);
        assert_eq!(preferences.recent_projects[0], repeated);
        assert_eq!(
            preferences.recent_projects.iter().filter(|path| **path == repeated).count(),
            1
        );
    }

    #[test]
    fn loading_preferences_filters_missing_recent_projects() {
        let path = temp_preferences_path("recent-filter");
        let existing = temp_preferences_path("recent-existing").with_extension("mdp");
        let missing = temp_preferences_path("recent-missing").with_extension("mdp");
        fs::write(&existing, b"project").expect("write existing recent project");
        fs::write(
            &path,
            serde_json::to_vec(&AppUiPreferences {
                version: 1,
                theme_preference: ThemePreference::Dark,
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: vec![missing, existing.clone(), existing.clone()],
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                video_scopes: Default::default(),
                audio_output_device: Default::default(),
                reference_output: Default::default(),
                display_management: Default::default(),
            })
            .expect("serialize preferences"),
        )
        .expect("write preferences");

        let preferences = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();
        fs::remove_file(&existing).ok();

        assert_eq!(preferences.recent_projects, vec![existing]);
    }

    #[test]
    fn loading_preferences_sanitizes_shortcut_overrides() {
        let path = temp_preferences_path("shortcut-filter");
        fs::write(
            &path,
            serde_json::to_vec(&AppUiPreferences {
                version: 1,
                theme_preference: ThemePreference::Dark,
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: Vec::new(),
                shortcut_overrides: vec![
                    AppUiShortcutOverride { id: "panel.inspector".to_owned(), binding: None },
                    AppUiShortcutOverride { id: "unknown.shortcut".to_owned(), binding: None },
                    AppUiShortcutOverride { id: "panel.inspector".to_owned(), binding: None },
                ],
                custom_workspace_layout: None,
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                video_scopes: Default::default(),
                audio_output_device: Default::default(),
                reference_output: Default::default(),
                display_management: Default::default(),
            })
            .expect("serialize preferences"),
        )
        .expect("write preferences");

        let preferences = load_app_ui_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(
            preferences.shortcut_overrides,
            vec![AppUiShortcutOverride { id: "panel.inspector".to_owned(), binding: None }]
        );
    }
}
