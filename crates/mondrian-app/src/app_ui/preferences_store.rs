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
use crate::app::native_audio_plugin::{NativeAudioPluginFormat, NativeAudioPluginSelection};
use crate::app::openfx_catalog::OpenFxBundleSelection;
use crate::app_ui::localization::AppUiLocalePreference;
use crate::app_ui::shortcuts::{is_known_shortcut_id, AppUiShortcutOverride};
use crate::app_ui::workspace_layout::AppUiWorkspaceLayout;

const APP_UI_PREFERENCES_FILE: &str = "app_ui_preferences.json";
const APP_UI_PREFERENCES_VERSION: u32 = 4;
/// Maximum number of recent projects kept by the app UI startup surface.
pub const MAX_RECENT_PROJECTS: usize = 12;
/// Bound the number of machine-local native libraries retried on startup.
pub const MAX_NATIVE_AUDIO_PLUGINS: usize = 64;
/// Bound the number of selected OpenFX bundles restored on startup.
pub const MAX_NATIVE_OPENFX_BUNDLES: usize = 64;

/// Versioned user preferences owned by the app UI shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppUiPreferences {
    /// Current machine schema; v3 Filter paths are migrated, other versions rejected.
    pub version: u32,
    /// Active product theme preference. `System` resolves to a concrete preset
    /// at runtime and is not persisted as a third theme token set.
    pub theme_preference: ThemePreference,
    /// Machine-local UI language; never persisted in a Project.
    pub locale_preference: AppUiLocalePreference,
    /// Built-in workspace preset restored when the app UI shell opens.
    pub workspace_preset: WorkspacePreset,
    /// Most recently opened project files for the startup surface.
    pub recent_projects: Vec<PathBuf>,
    /// Explicitly selected native audio binaries; project files store plugin identities only.
    pub installed_audio_plugins: Vec<NativeAudioPluginSelection>,
    /// Explicitly selected native OpenFX bundles; project files retain effect identities.
    pub installed_openfx_bundles: Vec<OpenFxBundleSelection>,
    /// User overrides for app UI shell shortcut descriptors.
    pub shortcut_overrides: Vec<AppUiShortcutOverride>,
    /// Persisted custom dock layout for the app UI workspace.
    pub custom_workspace_layout: Option<AppUiWorkspaceLayout>,
    /// Waveform display mode for audio clips on the timeline.
    pub waveform_display: WaveformDisplay,
    /// Presentation-only background visible through transparent Viewer pixels.
    pub viewer_canvas_background: ViewerCanvasBackground,
    /// Machine-local professional Scopes analysis and presentation controls.
    pub video_scopes: VideoScopesSettings,
    /// Runtime output-device intent. This is a user preference, never Project state.
    pub audio_output_device: mondrian_media::RealtimeAudioOutputDeviceSelection,
    /// Machine-local professional output routing intent. Loading preferences
    /// never auto-acquires a DeckLink/AJA device.
    pub reference_output: mondrian_reference_output::ReferenceOutputRoutingPreferences,
    /// Machine-local Viewer monitor target, ICC calibration, and HDR policy.
    /// This value never enters `.mdp` Project authoring state.
    pub display_management: mondrian_core::DisplayManagementPolicy,
}

impl Default for AppUiPreferences {
    fn default() -> Self {
        Self {
            version: APP_UI_PREFERENCES_VERSION,
            theme_preference: ThemePreference::System,
            locale_preference: AppUiLocalePreference::System,
            workspace_preset: WorkspacePreset::Editing,
            recent_projects: Vec::new(),
            installed_audio_plugins: Vec::new(),
            installed_openfx_bundles: Vec::new(),
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

    /// Remember one successfully scanned native library without adding duplicates.
    pub fn record_native_audio_plugin(&mut self, format: NativeAudioPluginFormat, path: PathBuf) {
        self.installed_audio_plugins
            .retain(|existing| existing.format != format || existing.path != path);
        self.installed_audio_plugins.push(NativeAudioPluginSelection { format, path });
        if self.installed_audio_plugins.len() > MAX_NATIVE_AUDIO_PLUGINS {
            self.installed_audio_plugins.remove(0);
        }
    }

    /// Remember one successfully admitted OpenFX bundle without duplicates.
    pub fn record_openfx_bundle(&mut self, selection: OpenFxBundleSelection) {
        self.installed_openfx_bundles.retain(|existing| existing != &selection);
        self.installed_openfx_bundles.push(selection);
        if self.installed_openfx_bundles.len() > MAX_NATIVE_OPENFX_BUNDLES {
            self.installed_openfx_bundles.remove(0);
        }
    }

    fn sanitize_loaded(mut self) -> Self {
        if self.version == 3 {
            self.version = APP_UI_PREFERENCES_VERSION;
        }
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

        let mut plugins = Vec::new();
        for selection in self.installed_audio_plugins {
            if !selection.path.is_absolute() || plugins.contains(&selection) {
                continue;
            }
            plugins.push(selection);
            if plugins.len() == MAX_NATIVE_AUDIO_PLUGINS {
                break;
            }
        }
        self.installed_audio_plugins = plugins;

        let mut bundles = Vec::new();
        for selection in self.installed_openfx_bundles {
            if !selection.path.is_absolute() || bundles.contains(&selection) {
                continue;
            }
            bundles.push(selection);
            if bundles.len() == MAX_NATIVE_OPENFX_BUNDLES {
                break;
            }
        }
        self.installed_openfx_bundles = bundles;

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
                locale_preference: Default::default(),
                workspace_preset: WorkspacePreset::Export,
                recent_projects: Vec::new(),
                installed_audio_plugins: Vec::new(),
                installed_openfx_bundles: Vec::new(),
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
            version: AppUiPreferences::default().version,
            theme_preference: ThemePreference::Light,
            locale_preference: AppUiLocalePreference::EnUs,
            workspace_preset: WorkspacePreset::Compositing,
            recent_projects: vec![project_path.clone()],
            installed_audio_plugins: Vec::new(),
            installed_openfx_bundles: Vec::new(),
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
    fn native_plugin_selections_are_bounded_deduplicated_and_missing_files_remain_recoverable() {
        let path = temp_preferences_path("clap-library-preferences");
        let missing = temp_preferences_path("missing-plugin").with_extension("clap");
        let mut preferences = AppUiPreferences::default();
        preferences.record_native_audio_plugin(NativeAudioPluginFormat::Clap, missing.clone());
        preferences.record_native_audio_plugin(NativeAudioPluginFormat::Clap, missing.clone());
        let expected = NativeAudioPluginSelection {
            format: NativeAudioPluginFormat::Clap,
            path: missing.clone(),
        };
        assert_eq!(preferences.installed_audio_plugins, vec![expected.clone()]);
        persist_app_ui_preferences_to(&path, &preferences).expect("persist selections");
        assert_eq!(
            load_app_ui_preferences_from(&path).installed_audio_plugins,
            vec![expected.clone()]
        );
        for index in 0..=MAX_NATIVE_AUDIO_PLUGINS {
            preferences.record_native_audio_plugin(
                NativeAudioPluginFormat::Clap,
                std::env::temp_dir().join(format!("mondrian-clap-library-{index}.dll")),
            );
        }
        assert_eq!(
            preferences.installed_audio_plugins.len(),
            MAX_NATIVE_AUDIO_PLUGINS
        );
        assert!(!preferences.installed_audio_plugins.contains(&expected));

        let missing_bundle = temp_preferences_path("missing-openfx").with_extension("ofx.bundle");
        preferences.record_openfx_bundle(missing_bundle.clone().into());
        preferences.record_openfx_bundle(missing_bundle.clone().into());
        assert_eq!(
            preferences.installed_openfx_bundles,
            vec![missing_bundle.clone().into()]
        );
        persist_app_ui_preferences_to(&path, &preferences).expect("persist bundle selection");
        assert_eq!(
            load_app_ui_preferences_from(&path).installed_openfx_bundles,
            vec![missing_bundle.clone().into()]
        );
        for index in 0..=MAX_NATIVE_OPENFX_BUNDLES {
            preferences.record_openfx_bundle(
                std::env::temp_dir().join(format!("mondrian-filter-{index}.ofx.bundle")).into(),
            );
        }
        assert_eq!(
            preferences.installed_openfx_bundles.len(),
            MAX_NATIVE_OPENFX_BUNDLES
        );
        assert!(!preferences.installed_openfx_bundles.contains(&missing_bundle.into()));
        fs::remove_file(path).ok();
    }

    fn assert_rejected_preferences_preserve_file(value: serde_json::Value) {
        assert!(serde_json::from_value::<AppUiPreferences>(value.clone()).is_err());
        let path = temp_preferences_path("rejected-current-schema");
        let bytes = serde_json::to_vec(&value).expect("serialize rejected preferences");
        fs::write(&path, &bytes).expect("write preferences fixture");
        assert_eq!(
            load_app_ui_preferences_from(&path),
            AppUiPreferences::default()
        );
        assert_eq!(fs::read(&path).expect("read untouched preferences"), bytes);
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn legacy_filter_paths_migrate_without_losing_preferences_or_generator_selections() {
        use crate::openfx_host::OpenFxContext;
        let path = temp_preferences_path("openfx-context-migration");
        let bundle = path.with_extension("missing.ofx.bundle");
        let filter = OpenFxBundleSelection::from(bundle.clone());
        let generator = OpenFxBundleSelection {
            path: bundle.clone(),
            context: OpenFxContext::Generator,
        };
        let mut expected = AppUiPreferences::default();
        expected.recent_projects = vec![path.clone()];
        expected.installed_openfx_bundles = vec![filter.clone()];
        let mut old = serde_json::to_value(&expected).unwrap();
        old["version"] = serde_json::json!(3);
        old["installed_openfx_bundles"] = serde_json::json!([bundle]);
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let mut loaded = load_app_ui_preferences_from(&path);
        assert_eq!(
            loaded, expected,
            "migration keeps the complete unrelated preference state"
        );
        loaded.record_openfx_bundle(generator.clone());
        loaded.record_openfx_bundle(filter.clone());
        assert_eq!(loaded.installed_openfx_bundles, vec![generator, filter]);
        persist_app_ui_preferences_to(&path, &loaded).unwrap();
        assert_eq!(load_app_ui_preferences_from(&path), loaded);
        let canonical: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(canonical["version"], 4);
        assert_eq!(
            canonical["installed_openfx_bundles"][0]["context"],
            "generator"
        );
        assert_eq!(
            canonical["installed_openfx_bundles"][1]["context"],
            "filter"
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_openfx_contexts_never_fall_back_to_filter() {
        let mut value = serde_json::to_value(AppUiPreferences::default()).unwrap();
        for selection in [
            serde_json::json!({"path": std::env::temp_dir().join("plugin.ofx.bundle"), "context":"retimer"}),
            serde_json::json!({"path": std::env::temp_dir().join("plugin.ofx.bundle"), "context":"generator", "unknown":true}),
            serde_json::json!({"path": std::env::temp_dir().join("plugin.ofx.bundle")}),
        ] {
            value["installed_openfx_bundles"] = serde_json::json!([selection]);
            assert_rejected_preferences_preserve_file(value.clone());
        }
    }

    #[test]
    fn current_preferences_schema_rejects_unknown_language_and_fields() {
        let fixture = AppUiPreferences {
            theme_preference: ThemePreference::Light,
            locale_preference: AppUiLocalePreference::EnUs,
            ..AppUiPreferences::default()
        };
        let mut unknown_locale = serde_json::to_value(&fixture).expect("serialize fixture");
        unknown_locale["locale_preference"] = "FutureLocale".into();
        assert_rejected_preferences_preserve_file(unknown_locale);
        let mut unknown_field = serde_json::to_value(&fixture).expect("serialize fixture");
        unknown_field["unsupported_preference"] = true.into();
        assert_rejected_preferences_preserve_file(unknown_field);
    }

    #[test]
    fn current_preferences_schema_requires_machine_fields() {
        let fixture = AppUiPreferences {
            theme_preference: ThemePreference::Light,
            locale_preference: AppUiLocalePreference::EnUs,
            ..AppUiPreferences::default()
        };
        for field in [
            "version",
            "theme_preference",
            "locale_preference",
            "workspace_preset",
            "recent_projects",
            "installed_audio_plugins",
            "installed_openfx_bundles",
            "shortcut_overrides",
            "waveform_display",
            "viewer_canvas_background",
            "video_scopes",
            "audio_output_device",
            "reference_output",
            "display_management",
        ] {
            let mut missing = serde_json::to_value(&fixture).expect("serialize fixture");
            missing.as_object_mut().expect("preferences object").remove(field);
            assert_rejected_preferences_preserve_file(missing);
        }
    }

    #[test]
    fn current_preferences_languages_round_trip_with_optional_layout_absent() {
        for locale in [
            AppUiLocalePreference::System,
            AppUiLocalePreference::ZhCn,
            AppUiLocalePreference::EnUs,
            AppUiLocalePreference::Pseudo,
        ] {
            let fixture = AppUiPreferences {
                theme_preference: ThemePreference::Light,
                locale_preference: locale,
                ..AppUiPreferences::default()
            };
            let mut value = serde_json::to_value(&fixture).expect("serialize fixture");
            value
                .as_object_mut()
                .expect("preferences object")
                .remove("custom_workspace_layout");
            assert_eq!(
                serde_json::from_value::<AppUiPreferences>(value.clone()).expect("current schema"),
                fixture
            );
            let path = temp_preferences_path("supported-language");
            fs::write(
                &path,
                serde_json::to_vec(&value).expect("serialize fixture"),
            )
            .expect("write fixture");
            assert_eq!(load_app_ui_preferences_from(&path), fixture);
            fs::remove_file(path).expect("remove fixture");
        }
    }

    #[test]
    fn loading_preferences_sanitizes_custom_workspace_layout() {
        let path = temp_preferences_path("custom-layout-filter");
        fs::write(
            &path,
            serde_json::to_vec(&AppUiPreferences {
                version: AppUiPreferences::default().version,
                theme_preference: ThemePreference::Dark,
                locale_preference: Default::default(),
                workspace_preset: WorkspacePreset::Custom,
                recent_projects: Vec::new(),
                installed_audio_plugins: Vec::new(),
                installed_openfx_bundles: Vec::new(),
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
                locale_preference: Default::default(),
                workspace_preset: WorkspacePreset::Custom,
                recent_projects: Vec::new(),
                installed_audio_plugins: Vec::new(),
                installed_openfx_bundles: Vec::new(),
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
                version: AppUiPreferences::default().version,
                theme_preference: ThemePreference::Dark,
                locale_preference: Default::default(),
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: vec![missing, existing.clone(), existing.clone()],
                installed_audio_plugins: Vec::new(),
                installed_openfx_bundles: Vec::new(),
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
                version: AppUiPreferences::default().version,
                theme_preference: ThemePreference::Dark,
                locale_preference: Default::default(),
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: Vec::new(),
                installed_audio_plugins: Vec::new(),
                installed_openfx_bundles: Vec::new(),
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
