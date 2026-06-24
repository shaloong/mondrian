use mondrian_core::DisplayColorProfile;
use serde::{Deserialize, Serialize};

/// Preview resolution mode persisted with app-level viewer preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum PreviewScaleMode {
    #[default]
    Full,
    Half,
    Quarter,
}

impl PreviewScaleMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Full => "全分辨率",
            Self::Half => "1/2",
            Self::Quarter => "1/4",
        }
    }

    pub(crate) const fn factor(self) -> f32 {
        match self {
            Self::Full => 1.0,
            Self::Half => 0.5,
            Self::Quarter => 0.25,
        }
    }
}

/// Viewer preferences owned by the app layer, independent of any UI toolkit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ViewerPreferences {
    #[serde(default)]
    pub(crate) preview_scale_mode: PreviewScaleMode,
    #[serde(default)]
    pub(crate) proxy_config: mondrian_media::ProxyConfig,
    #[serde(default)]
    pub(crate) decode_backend: mondrian_media::PreviewDecodeBackend,
    #[serde(default = "default_prefetch_enabled")]
    pub(crate) prefetch_enabled: bool,
    #[serde(default = "default_layer_cache_enabled")]
    pub(crate) layer_cache_enabled: bool,
    #[serde(default = "DisplayColorProfile::rec709_reference")]
    pub(crate) display_profile: DisplayColorProfile,
    /// Canvas background color (letterbox/pillarbox), stored as 0xRRGGBB hex.
    #[serde(default = "default_canvas_bg")]
    pub(crate) canvas_bg_hex: u32,
}

impl Default for ViewerPreferences {
    fn default() -> Self {
        Self {
            preview_scale_mode: PreviewScaleMode::default(),
            proxy_config: mondrian_media::ProxyConfig::default(),
            decode_backend: mondrian_media::PreviewDecodeBackend::default(),
            prefetch_enabled: default_prefetch_enabled(),
            layer_cache_enabled: default_layer_cache_enabled(),
            display_profile: DisplayColorProfile::rec709_reference(),
            canvas_bg_hex: default_canvas_bg(),
        }
    }
}

pub(crate) const fn default_canvas_bg() -> u32 {
    0x2a2a2a
}

const fn default_prefetch_enabled() -> bool {
    true
}

const fn default_layer_cache_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preferences_use_stable_preview_defaults() {
        let preferences = ViewerPreferences::default();

        assert_eq!(preferences.preview_scale_mode, PreviewScaleMode::Full);
        assert_eq!(
            preferences.proxy_config,
            mondrian_media::ProxyConfig::default()
        );
        assert_eq!(
            preferences.decode_backend,
            mondrian_media::PreviewDecodeBackend::default()
        );
        assert!(preferences.prefetch_enabled);
        assert!(preferences.layer_cache_enabled);
        assert_eq!(
            preferences.display_profile,
            DisplayColorProfile::rec709_reference()
        );
        assert_eq!(preferences.canvas_bg_hex, default_canvas_bg());
    }

    #[test]
    fn missing_persisted_fields_fall_back_to_current_defaults() {
        let json = r#"{
            "preview_scale_mode": "Half"
        }"#;

        let preferences: ViewerPreferences =
            serde_json::from_str(json).expect("viewer preferences should deserialize");

        assert_eq!(preferences.preview_scale_mode, PreviewScaleMode::Half);
        assert_eq!(
            preferences.proxy_config,
            mondrian_media::ProxyConfig::default()
        );
        assert_eq!(
            preferences.decode_backend,
            mondrian_media::PreviewDecodeBackend::default()
        );
        assert!(preferences.prefetch_enabled);
        assert!(preferences.layer_cache_enabled);
        assert_eq!(
            preferences.display_profile,
            DisplayColorProfile::rec709_reference()
        );
        assert_eq!(preferences.canvas_bg_hex, default_canvas_bg());
    }

    #[test]
    fn empty_persisted_preferences_deserialize_to_defaults() {
        let preferences: ViewerPreferences =
            serde_json::from_str("{}").expect("empty viewer preferences should deserialize");

        assert_eq!(preferences, ViewerPreferences::default());
    }

    #[test]
    fn preview_scale_modes_have_labels_and_factors() {
        assert_eq!(PreviewScaleMode::Full.label(), "全分辨率");
        assert_eq!(PreviewScaleMode::Full.factor(), 1.0);
        assert_eq!(PreviewScaleMode::Half.label(), "1/2");
        assert_eq!(PreviewScaleMode::Half.factor(), 0.5);
        assert_eq!(PreviewScaleMode::Quarter.label(), "1/4");
        assert_eq!(PreviewScaleMode::Quarter.factor(), 0.25);
    }

    #[test]
    fn preferences_round_trip_without_legacy_ui_types() {
        let preferences = ViewerPreferences {
            preview_scale_mode: PreviewScaleMode::Quarter,
            canvas_bg_hex: 0x112233,
            ..ViewerPreferences::default()
        };

        let json =
            serde_json::to_string(&preferences).expect("viewer preferences should serialize");
        let restored: ViewerPreferences =
            serde_json::from_str(&json).expect("viewer preferences should deserialize");

        assert_eq!(restored, preferences);
    }
}
