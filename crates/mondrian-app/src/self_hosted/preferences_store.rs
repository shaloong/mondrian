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

const SELF_HOSTED_PREFERENCES_FILE: &str = "self_hosted_preferences.json";

/// Versioned user preferences owned by the self-hosted shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfHostedPreferences {
    /// Schema version for future non-compatible alpha migrations.
    pub version: u32,
    /// Active product theme preset.
    pub theme_preset: ThemePreset,
    /// Built-in workspace preset restored when the self-hosted shell opens.
    pub workspace_preset: WorkspacePreset,
}

impl Default for SelfHostedPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            theme_preset: ThemePreset::Dark,
            workspace_preset: WorkspacePreset::Editing,
        }
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
    serde_json::from_slice::<SelfHostedPreferences>(&bytes).unwrap_or_default()
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
        let preferences = SelfHostedPreferences {
            version: 1,
            theme_preset: ThemePreset::Light,
            workspace_preset: WorkspacePreset::Compositing,
        };

        persist_self_hosted_preferences_to(&path, &preferences).expect("persist preferences");
        let loaded = load_self_hosted_preferences_from(&path);
        fs::remove_file(path).ok();

        assert_eq!(loaded, preferences);
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
}
