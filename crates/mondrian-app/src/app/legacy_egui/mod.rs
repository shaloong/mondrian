//! Reference-only legacy eframe/egui editor modules.
//!
//! The product entrypoint is the self-hosted winit/wgpu shell. This module keeps
//! the old eframe app compiling while useful migration reference code is lifted
//! into neutral app/self-hosted modules or deleted.

use super::*;

use crate::app::media_cache::MediaCacheCleanupStats;
use crate::egui_ui::{
    effect_controls_panel::EffectControlsPanel,
    effect_library_panel::EffectLibraryPanel,
    export_panel::ExportPanel,
    library_panel::LibraryPanel,
    startup::{BootstrapAction, BootstrapRecentProjectItem, BootstrapRecoveryItem},
    timeline_panel::TimelinePanel,
    viewer_panel::ViewerPanel,
};
use crate::shortcuts::{ShortcutAction, ShortcutBinding, ShortcutKey, ShortcutPreferences};

#[allow(dead_code)]
pub(in crate::app) mod app;
#[allow(dead_code)]
pub(in crate::app) mod bootstrap;
#[allow(dead_code)]
pub(in crate::app) mod chrome;
#[allow(dead_code)]
pub(in crate::app) mod new_project;
#[allow(dead_code)]
pub(in crate::app) mod preferences;
pub(in crate::app) mod preferences_model;

pub(in crate::app) use app::MondrianApp;
use preferences_model::{
    builtin_sequence_presets, default_app_theme, default_auto_save_enabled,
    default_auto_save_interval_secs, default_auto_save_max_recovery_points,
    default_auto_save_retention_days, default_media_cache_auto_cleanup,
    default_media_cache_max_age_days, default_media_cache_max_size_gb, default_show_audio_metrics,
    default_show_video_metrics, default_timeline_panel_height, AppPreferences, ColorMode,
    NewProjectDraft, NewProjectTab, PendingCloseAction, PreferencesTab,
};
