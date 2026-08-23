//! Shared project color-management controls for app UI workflows.
//!
//! This module owns the product-facing color-engine catalog and the native
//! Custom OCIO selection boundary. Creation and settings dialogs provide only
//! their draft-specific actions.

use mondrian_core::{
    AcesConfigPreset, ColorEngine, ColorSpace, OcioConfigSource, ProjectColorEnvironment,
    WorkingColorSpace,
};
use mondrian_editor_state::Action;
use mondrian_platform::{FileFilter, PlatformService};
use mondrian_ui_widgets::menu::MenuItem;

use crate::app::ui_actions::app_shell_select_custom_ocio_config_action;

/// Choose and fully pin a Custom OCIO config.
///
/// Canceling returns `Ok(None)`. Invalid or incompatible configs fail before
/// any partial Custom mode can enter a dialog draft.
pub(crate) fn choose_custom_ocio_config(
    platform: &dyn PlatformService,
    sequence_color_contracts: &[(WorkingColorSpace, ColorSpace)],
) -> Result<Option<ColorEngine>, String> {
    let Some(path) = platform
        .open_file_dialog(
            "选择 OpenColorIO 配置",
            &[FileFilter::new("OpenColorIO 配置", vec!["ocio"])],
        )
        .map_err(|error| error.to_string())?
        .into_selection()
        .and_then(|paths| paths.into_iter().next())
    else {
        return Ok(None);
    };
    ProjectColorEnvironment::custom_ocio(OcioConfigSource::Path { path }, sequence_color_contracts)
        .map(ProjectColorEnvironment::into_engine)
        .map(Some)
}

/// Return the stable product label for a project color engine.
pub(crate) fn color_engine_label(engine: &ColorEngine) -> &'static str {
    match engine {
        ColorEngine::MondrianStandard { .. } => "Mondrian Standard",
        ColorEngine::Aces { preset: AcesConfigPreset::StudioV4Aces2Ocio25 } => "ACES 2.0 Studio",
        ColorEngine::Aces { preset: AcesConfigPreset::CgV4Aces2Ocio25 } => "ACES 2.0 CG",
        ColorEngine::CustomOcio { .. } => "自定义 OpenColorIO",
    }
}

/// Build the shared project color-engine catalog with workflow-local actions.
pub(crate) fn color_engine_menu_items(
    color_engine_action: impl Fn(ColorEngine) -> Action,
) -> Vec<MenuItem> {
    let item = |label, engine| MenuItem::new(label, color_engine_action(engine));
    vec![
        item("Mondrian Standard", ColorEngine::mondrian_standard()),
        item(
            "ACES 2.0 Studio",
            ColorEngine::Aces { preset: AcesConfigPreset::StudioV4Aces2Ocio25 },
        ),
        item(
            "ACES 2.0 CG",
            ColorEngine::Aces { preset: AcesConfigPreset::CgV4Aces2Ocio25 },
        ),
        MenuItem::new(
            "选择自定义 OpenColorIO…",
            app_shell_select_custom_ocio_config_action(),
        ),
    ]
}
