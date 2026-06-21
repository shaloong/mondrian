//! Persistent preferences for the self-hosted product shell.
//!
//! This store belongs to the application adapter layer. Reusable widgets receive
//! typed models/actions only; they never read or write disk directly.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mondrian_editor_state::state::WorkspacePreset;
use mondrian_ui_theme::ThemePreset;
use serde::{Deserialize, Serialize};

use crate::app::app_data_dir;
use crate::self_hosted::shortcuts::{is_known_shortcut_id, SelfHostedShortcutOverride};
use crate::self_hosted::workspace_layout::SelfHostedWorkspaceLayout;

const SELF_HOSTED_PREFERENCES_FILE: &str = "self_hosted_preferences.json";
/// Maximum number of recent projects kept by the self-hosted startup surface.
pub const MAX_RECENT_PROJECTS: usize = 12;

/// Versioned user preferences owned by the self-hosted shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelfHostedPreferences {
    /// Schema version for future non-compatible alpha migrations.
    pub version: u32,
    /// Active product theme preset.
    pub theme_preset: ThemePreset,
    /// Built-in workspace preset restored when the self-hosted shell opens.
    pub workspace_preset: WorkspacePreset,
    /// Most recently opened project files for the startup surface.
    pub recent_projects: Vec<PathBuf>,
    /// User overrides for self-hosted shell shortcut descriptors.
    #[serde(default)]
    pub shortcut_overrides: Vec<SelfHostedShortcutOverride>,
    /// Persisted custom dock layout for the self-hosted workspace.
    #[serde(default)]
    pub custom_workspace_layout: Option<SelfHostedWorkspaceLayout>,
}

impl Default for SelfHostedPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            theme_preset: ThemePreset::Dark,
            workspace_preset: WorkspacePreset::Editing,
            recent_projects: Vec::new(),
            shortcut_overrides: Vec::new(),
            custom_workspace_layout: None,
        }
    }
}

impl SelfHostedPreferences {
    /// Record a project path at the front of the recent list.
    pub fn record_recent_project(&mut self, project_file: PathBuf) {
        self.recent_projects.retain(|existing| existing != &project_file);
        self.recent_projects.insert(0, project_file);
        self.recent_projects.truncate(MAX_RECENT_PROJECTS);
    }

    fn sanitize_loaded(mut self) -> Self {
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
                    .any(|existing: &SelfHostedShortcutOverride| existing.id == entry.id)
            {
                continue;
            }
            shortcut_overrides.push(entry);
        }
        self.shortcut_overrides = shortcut_overrides;

        self.custom_workspace_layout =
            self.custom_workspace_layout.and_then(SelfHostedWorkspaceLayout::sanitized);
        self
    }
}

/// Default self-hosted preferences path under Mondrian's app data directory.
pub fn self_hosted_preferences_path() -> PathBuf {
    app_data_dir().join(SELF_HOSTED_PREFERENCES_FILE)
}

/// Load self-hosted preferences from the default product path.
pub fn load_self_hosted_preferences() -> SelfHostedPreferences {
    load_self_hosted_preferences_from(&self_hosted_preferences_path())
}

/// Load self-hosted preferences from an explicit path, returning defaults when
/// the file is absent or malformed.
pub fn load_self_hosted_preferences_from(path: &Path) -> SelfHostedPreferences {
    let Ok(bytes) = fs::read(path) else {
        return SelfHostedPreferences::default();
    };
    serde_json::from_slice::<SelfHostedPreferences>(&bytes)
        .map(SelfHostedPreferences::sanitize_loaded)
        .unwrap_or_default()
}

/// Persist self-hosted preferences to the default product path.
pub fn persist_self_hosted_preferences(preferences: &SelfHostedPreferences) -> io::Result<()> {
    persist_self_hosted_preferences_to(&self_hosted_preferences_path(), preferences)
}

/// Persist self-hosted preferences to an explicit path.
pub fn persist_self_hosted_preferences_to(
    path: &Path,
    preferences: &SelfHostedPreferences,
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
    use crate::self_hosted::workspace_layout::SelfHostedWorkspaceLayout;
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

        let preferences = load_self_hosted_preferences_from(&path);

        assert_eq!(preferences, SelfHostedPreferences::default());
    }

    #[test]
    fn malformed_preferences_file_uses_defaults() {
        let path = temp_preferences_path("malformed-preferences");
        fs::write(&path, b"{not json").expect("write malformed fixture");

        let preferences = load_self_hosted_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(preferences, SelfHostedPreferences::default());
    }

    #[test]
    fn preferences_round_trip_to_disk() {
        let path = temp_preferences_path("round-trip-preferences");
        let project_path = temp_preferences_path("round-trip-project").with_extension("mdp");
        let preferences = SelfHostedPreferences {
            version: 1,
            theme_preset: ThemePreset::Light,
            workspace_preset: WorkspacePreset::Compositing,
            recent_projects: vec![project_path.clone()],
            shortcut_overrides: vec![SelfHostedShortcutOverride {
                id: "panel.inspector".to_owned(),
                binding: None,
            }],
            custom_workspace_layout: Some(SelfHostedWorkspaceLayout::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.37,
                first: Box::new(SelfHostedWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Assets,
                    active_index: 1,
                    hidden_tabs: Vec::new(),
                }),
                second: Box::new(SelfHostedWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Viewer,
                    active_index: 0,
                    hidden_tabs: Vec::new(),
                }),
            }),
        };

        fs::write(&project_path, b"project").expect("write recent project fixture");
        persist_self_hosted_preferences_to(&path, &preferences).expect("persist preferences");
        let loaded = load_self_hosted_preferences_from(&path);
        fs::remove_file(path).ok();
        fs::remove_file(project_path).ok();

        assert_eq!(loaded, preferences);
    }

    #[test]
    fn loading_preferences_sanitizes_custom_workspace_layout() {
        let path = temp_preferences_path("custom-layout-filter");
        fs::write(
            &path,
            serde_json::to_vec(&SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Custom,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: Some(SelfHostedWorkspaceLayout::Split {
                    direction: SplitDirection::Vertical,
                    ratio: 12.0,
                    first: Box::new(SelfHostedWorkspaceLayout::Panel {
                        kind: mondrian_editor_state::state::PanelKind::Assets,
                        active_index: 99,
                        hidden_tabs: Vec::new(),
                    }),
                    second: Box::new(SelfHostedWorkspaceLayout::Panel {
                        kind: mondrian_editor_state::state::PanelKind::Timeline,
                        active_index: 2,
                        hidden_tabs: Vec::new(),
                    }),
                }),
            })
            .expect("serialize preferences"),
        )
        .expect("write preferences");

        let preferences = load_self_hosted_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(
            preferences.custom_workspace_layout,
            Some(SelfHostedWorkspaceLayout::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.9,
                first: Box::new(SelfHostedWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Assets,
                    active_index: 1,
                    hidden_tabs: Vec::new(),
                }),
                second: Box::new(SelfHostedWorkspaceLayout::Panel {
                    kind: mondrian_editor_state::state::PanelKind::Timeline,
                    active_index: 0,
                    hidden_tabs: Vec::new(),
                }),
            })
        );
    }

    #[test]
    fn partial_preferences_file_uses_clean_defaults() {
        let path = temp_preferences_path("partial-preferences");
        fs::write(&path, br#"{"version":1,"theme_preset":"Light"}"#)
            .expect("write partial fixture");

        let preferences = load_self_hosted_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(preferences, SelfHostedPreferences::default());
    }

    #[test]
    fn recording_recent_projects_deduplicates_and_truncates() {
        let mut preferences = SelfHostedPreferences::default();

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
            serde_json::to_vec(&SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: vec![missing, existing.clone(), existing.clone()],
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
            })
            .expect("serialize preferences"),
        )
        .expect("write preferences");

        let preferences = load_self_hosted_preferences_from(&path);
        fs::remove_file(path).ok();
        fs::remove_file(&existing).ok();

        assert_eq!(preferences.recent_projects, vec![existing]);
    }

    #[test]
    fn loading_preferences_sanitizes_shortcut_overrides() {
        let path = temp_preferences_path("shortcut-filter");
        fs::write(
            &path,
            serde_json::to_vec(&SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: Vec::new(),
                shortcut_overrides: vec![
                    SelfHostedShortcutOverride { id: "panel.inspector".to_owned(), binding: None },
                    SelfHostedShortcutOverride { id: "unknown.shortcut".to_owned(), binding: None },
                    SelfHostedShortcutOverride { id: "panel.inspector".to_owned(), binding: None },
                ],
                custom_workspace_layout: None,
            })
            .expect("serialize preferences"),
        )
        .expect("write preferences");

        let preferences = load_self_hosted_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(
            preferences.shortcut_overrides,
            vec![SelfHostedShortcutOverride { id: "panel.inspector".to_owned(), binding: None }]
        );
    }
}
