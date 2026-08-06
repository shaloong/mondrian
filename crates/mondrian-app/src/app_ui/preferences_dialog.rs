//! App UI Preferences dialog.
//!
//! This dialog owns the product settings surface for the custom UI shell. It
//! deliberately depends on app-shell actions instead of legacy egui preference
//! state so each preference can be migrated into a clean, typed boundary.

use mondrian_core::Color;
use mondrian_editor_state::state::WorkspacePreset;
use mondrian_export::preset::TimelineExportRange;
use mondrian_media::{RealtimeAudioOutputDeviceCatalog, RealtimeAudioOutputDeviceSelection};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_theme::{ThemePreference, ThemePreset};
use mondrian_ui_widgets::{
    Button, ContextMenu, DialogSurface, Dropdown, Label, MenuItem, SegmentedButtonGroup,
    SegmentedButtonItem, TextInput, VectorIcon, ViewerCanvasBackground, WaveformDisplay,
};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_preferences_audio_output_device_changed_action,
    app_shell_preferences_refresh_audio_output_devices_action,
    app_shell_preferences_shortcut_disabled_action, app_shell_preferences_shortcut_rebound_action,
    app_shell_preferences_shortcut_reset_action, app_shell_preferences_tab_changed_action,
    app_shell_preferences_theme_changed_action,
    app_shell_preferences_viewer_background_changed_action,
    app_shell_preferences_waveform_display_changed_action, PreferencesShortcutReboundPayload,
    PreferencesTabPayload,
};
use crate::app::AppState;
use crate::app_ui::audio_device_catalog::AudioOutputDeviceCatalogState;
use crate::app_ui::commands::{command_by_id, AppUiCommandCategory};
use crate::app_ui::shortcuts::{
    active_shortcuts, default_shortcuts, AppUiShortcutBinding, AppUiShortcutKey,
    AppUiShortcutOverride,
};
use crate::app_ui::window::{APP_UI_BACKGROUND_WORKERS, DEFAULT_APP_UI_LOG_FILTER};

const CARD_MIN_WIDTH: f32 = 680.0;
const CARD_WIDTH: f32 = 860.0;
const CARD_MIN_HEIGHT: f32 = 360.0;
const CARD_HEIGHT: f32 = 560.0;
const CONTENT_PADDING: f32 = 22.0;
const BODY_FONT_SIZE: f32 = 13.0;
const NAV_WIDTH: f32 = 144.0;
const NAV_BUTTON_HEIGHT: f32 = 32.0;
const NAV_BUTTON_GAP: f32 = 4.0;
const SIDEBAR_WIDTH: f32 = 172.0;
const SIDEBAR_PADDING: f32 = 14.0;
const CONTENT_GAP: f32 = 16.0;
const ROW_HEIGHT: f32 = 28.0;
const BUTTON_WIDTH: f32 = 84.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;
const SEGMENTED_GROUP_HEIGHT: f32 = 30.0;
const SHORTCUT_SEARCH_HEIGHT: f32 = 32.0;
const SHORTCUT_SECTION_HEIGHT: f32 = 30.0;
const SHORTCUT_ROW_HEIGHT: f32 = 38.0;
const SHORTCUT_ACTION_MENU_WIDTH: f32 = 116.0;
const SHORTCUT_SCROLLBAR_GAP: f32 = 10.0;
const SHORTCUT_KEYCAP_HEIGHT: f32 = 24.0;
const SHORTCUT_KEYCAP_GAP: f32 = 5.0;
const SHORTCUT_KEYCAP_PADDING_X: f32 = 8.0;
const SHORTCUT_KEYCAP_MIN_WIDTH: f32 = 24.0;
const SHORTCUT_KEYCAP_ACTION_GAP: f32 = 14.0;
const SHORTCUT_CHEVRON_SIZE: f32 = 12.0;
const PREFERENCES_INTERACTIVE_CHILD_COUNT: usize = PreferencesDialogTab::ALL.len() + 6;

const CHEVRON_RIGHT_SVG: &str = r#"<svg viewBox="0 0 16 16"><path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>"#;
const CHEVRON_DOWN_SVG: &str = r#"<svg viewBox="0 0 16 16"><path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>"#;

/// Read-only settings/status snapshot shown by the app UI preferences UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiPreferencesModel {
    pub theme_preference: ThemePreference,
    pub resolved_theme_preset: ThemePreset,
    pub theme_label: String,
    pub project_status: String,
    pub workspace: String,
    pub sequence_summary: String,
    pub proxy_mode: String,
    pub audio_clock: String,
    pub audio_sample_rate: String,
    pub audio_output_device_selection: RealtimeAudioOutputDeviceSelection,
    pub audio_output_device_label: String,
    pub audio_output_device_catalog: AudioOutputDeviceCatalogState,
    pub export_range: String,
    pub export_output: String,
    pub runtime_diagnostics: String,
    pub log_filter: String,
    pub background_workers: String,
    pub shortcut_rows: Vec<ShortcutPreferenceRow>,
    pub waveform_display: WaveformDisplay,
    pub waveform_display_label: String,
    pub viewer_canvas_background: ViewerCanvasBackground,
    pub viewer_canvas_background_label: String,
}

/// One shortcut row shown in the app UI preferences UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutPreferenceRow {
    pub id: String,
    pub command_title: String,
    pub category: AppUiCommandCategory,
    pub binding_label: String,
    pub default_binding_label: String,
    pub overridden: bool,
    pub disabled: bool,
    pub conflict_owner: Option<String>,
    pub search_text: String,
}

impl AppUiPreferencesModel {
    /// Replace only the low-frequency physical-device observation.
    pub fn set_audio_output_device_catalog(&mut self, catalog: AudioOutputDeviceCatalogState) {
        self.audio_output_device_label =
            audio_output_device_label(&self.audio_output_device_selection, &catalog);
        self.audio_output_device_catalog = catalog;
    }

    /// Build the preferences model from the state actually owned by the
    /// app UI product shell.
    pub fn from_app_state(
        state: &AppState,
        workspace: WorkspacePreset,
        theme_preference: ThemePreference,
        resolved_theme_preset: ThemePreset,
    ) -> Self {
        Self::from_app_state_with_shortcut_overrides(
            state,
            workspace,
            theme_preference,
            resolved_theme_preset,
            &[],
            WaveformDisplay::BottomAligned,
            ViewerCanvasBackground::Checkerboard,
        )
    }

    /// Build the preferences model using the active app UI shortcut table.
    pub fn from_app_state_with_shortcut_overrides(
        state: &AppState,
        workspace: WorkspacePreset,
        theme_preference: ThemePreference,
        resolved_theme_preset: ThemePreset,
        shortcut_overrides: &[AppUiShortcutOverride],
        waveform_display: WaveformDisplay,
        viewer_canvas_background: ViewerCanvasBackground,
    ) -> Self {
        Self::from_app_state_with_shortcut_overrides_and_audio_output(
            state,
            workspace,
            theme_preference,
            resolved_theme_preset,
            shortcut_overrides,
            waveform_display,
            viewer_canvas_background,
            RealtimeAudioOutputDeviceSelection::SystemDefault,
            AudioOutputDeviceCatalogState::Loading,
        )
    }

    /// Build the preferences model with one runtime audio-device observation.
    #[allow(clippy::too_many_arguments)]
    pub fn from_app_state_with_shortcut_overrides_and_audio_output(
        state: &AppState,
        workspace: WorkspacePreset,
        theme_preference: ThemePreference,
        resolved_theme_preset: ThemePreset,
        shortcut_overrides: &[AppUiShortcutOverride],
        waveform_display: WaveformDisplay,
        viewer_canvas_background: ViewerCanvasBackground,
        audio_output_device_selection: RealtimeAudioOutputDeviceSelection,
        audio_output_device_catalog: AudioOutputDeviceCatalogState,
    ) -> Self {
        let project_status = state
            .current_project_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未打开项目".to_owned());
        let sequence_summary = state
            .active_sequence()
            .map(|sequence| {
                format!(
                    "{} · {}x{} · {} fps",
                    sequence.name,
                    sequence.settings.resolution.width,
                    sequence.settings.resolution.height,
                    sequence.settings.frame_rate
                )
            })
            .unwrap_or_else(|| "没有活动序列".to_owned());
        let audio_output_device_label =
            audio_output_device_label(&audio_output_device_selection, &audio_output_device_catalog);
        Self {
            theme_preference,
            resolved_theme_preset,
            theme_label: theme_preference.display_name().to_owned(),
            project_status,
            workspace: workspace.display_name().to_owned(),
            sequence_summary,
            proxy_mode: enabled_label(state.should_auto_generate_proxy_for_import()),
            audio_clock: state
                .playback_clock_master()
                .map(|master| format!("{master:?}"))
                .unwrap_or_else(|| "Inactive".to_owned()),
            audio_sample_rate: format!("{} Hz", state.audio_sample_rate),
            audio_output_device_selection,
            audio_output_device_label,
            audio_output_device_catalog,
            export_range: export_range_label(state.export_draft.range).to_owned(),
            export_output: if state.export_draft.output_path.trim().is_empty() {
                "未选择".to_owned()
            } else {
                state.export_draft.output_path.clone()
            },
            runtime_diagnostics: "跟踪已启用".to_owned(),
            log_filter: format!("RUST_LOG / {DEFAULT_APP_UI_LOG_FILTER}"),
            background_workers: APP_UI_BACKGROUND_WORKERS.to_string(),
            shortcut_rows: shortcut_preference_rows(shortcut_overrides),
            waveform_display,
            waveform_display_label: waveform_display_label(waveform_display).to_owned(),
            viewer_canvas_background,
            viewer_canvas_background_label: viewer_canvas_background_label(
                viewer_canvas_background,
            )
            .to_owned(),
        }
    }
}

impl Default for AppUiPreferencesModel {
    fn default() -> Self {
        Self {
            theme_preference: ThemePreference::System,
            resolved_theme_preset: ThemePreset::Dark,
            theme_label: ThemePreference::System.display_name().to_owned(),
            project_status: "未打开项目".to_owned(),
            workspace: WorkspacePreset::Editing.display_name().to_owned(),
            sequence_summary: "没有活动序列".to_owned(),
            proxy_mode: enabled_label(false),
            audio_clock: "Inactive".to_owned(),
            audio_sample_rate: "48000 Hz".to_owned(),
            audio_output_device_selection: RealtimeAudioOutputDeviceSelection::SystemDefault,
            audio_output_device_label: "系统默认".to_owned(),
            audio_output_device_catalog: AudioOutputDeviceCatalogState::Loading,
            export_range: export_range_label(TimelineExportRange::SequenceInOut).to_owned(),
            export_output: "未选择".to_owned(),
            runtime_diagnostics: "跟踪已启用".to_owned(),
            log_filter: format!("RUST_LOG / {DEFAULT_APP_UI_LOG_FILTER}"),
            background_workers: APP_UI_BACKGROUND_WORKERS.to_string(),
            shortcut_rows: shortcut_preference_rows(&[]),
            waveform_display: WaveformDisplay::BottomAligned,
            waveform_display_label: "整流".to_owned(),
            viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
            viewer_canvas_background_label: "棋盘格".to_owned(),
        }
    }
}

/// Product preferences section shown by the app UI shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferencesDialogTab {
    General,
    Media,
    Shortcuts,
    Developer,
}

impl PreferencesDialogTab {
    const ALL: [Self; 4] = [Self::General, Self::Media, Self::Shortcuts, Self::Developer];

    fn label(self) -> &'static str {
        match self {
            Self::General => "常规",
            Self::Media => "媒体",
            Self::Shortcuts => "快捷键",
            Self::Developer => "开发者",
        }
    }

    fn payload(self) -> PreferencesTabPayload {
        match self {
            Self::General => PreferencesTabPayload::General,
            Self::Media => PreferencesTabPayload::Media,
            Self::Shortcuts => PreferencesTabPayload::Shortcuts,
            Self::Developer => PreferencesTabPayload::Developer,
        }
    }
}

impl From<PreferencesTabPayload> for PreferencesDialogTab {
    fn from(value: PreferencesTabPayload) -> Self {
        match value {
            PreferencesTabPayload::General => Self::General,
            PreferencesTabPayload::Media => Self::Media,
            PreferencesTabPayload::Shortcuts => Self::Shortcuts,
            PreferencesTabPayload::Developer => Self::Developer,
        }
    }
}

/// Preferences modal for the app UI product shell.
pub struct PreferencesDialog {
    id: WidgetId,
    active_tab: PreferencesDialogTab,
    model: AppUiPreferencesModel,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    shortcut_viewport: Rect,
    shortcut_scroll_offset: f32,
    shortcut_search: TextInput,
    shortcut_search_query: String,
    shortcut_sections: Vec<ShortcutSectionState>,
    shortcut_layout_rows: Vec<ShortcutLayoutRow>,
    hovered_shortcut_id: Option<String>,
    shortcut_actions_menu: Option<ShortcutActionMenuState>,
    shortcut_capture: Option<ShortcutCaptureState>,
    collapsed_chevron: Option<VectorIcon>,
    expanded_chevron: Option<VectorIcon>,
    nav_buttons: Vec<Button>,
    theme_group: SegmentedButtonGroup,
    waveform_group: SegmentedButtonGroup,
    viewer_background_group: SegmentedButtonGroup,
    audio_output_device_dropdown: Dropdown,
    content_labels: Vec<Label>,
    close_button: Button,
}

#[derive(Debug, Clone)]
struct ShortcutSectionState {
    category: AppUiCommandCategory,
    collapsed: bool,
}

#[derive(Debug, Clone)]
struct ShortcutLayoutRow {
    kind: ShortcutLayoutRowKind,
    rect: Rect,
}

#[derive(Debug, Clone)]
enum ShortcutLayoutRowKind {
    Section {
        category: AppUiCommandCategory,
        title: &'static str,
        collapsed: bool,
    },
    Command {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShortcutCaptureState {
    id: String,
    pending: Option<AppUiShortcutBinding>,
    conflict_owner: Option<String>,
}

struct ShortcutActionMenuState {
    id: String,
    menu: ContextMenu,
}

impl PreferencesDialog {
    /// Build the preferences dialog with the default General tab.
    pub fn new() -> Self {
        Self::with_model(AppUiPreferencesModel::default())
    }

    /// Build the preferences dialog from an explicit model.
    pub fn with_model(model: AppUiPreferencesModel) -> Self {
        Self::with_model_and_tab(model, PreferencesDialogTab::General)
    }

    /// Build the preferences dialog from an explicit model and active tab.
    pub fn with_model_and_tab(
        model: AppUiPreferencesModel,
        active_tab: PreferencesDialogTab,
    ) -> Self {
        let nav_buttons = PreferencesDialogTab::ALL
            .into_iter()
            .map(|tab| {
                Button::new(tab.label())
                    .minimal()
                    .text_left()
                    .active(tab == active_tab)
                    .on_click(app_shell_preferences_tab_changed_action(tab.payload()))
            })
            .collect();
        let theme_selected = ThemePreference::ALL
            .iter()
            .position(|preference| *preference == model.theme_preference)
            .unwrap_or(0);
        let theme_group = SegmentedButtonGroup::new(
            ThemePreference::ALL
                .into_iter()
                .map(|preference| {
                    SegmentedButtonItem::new(
                        preference.display_name(),
                        app_shell_preferences_theme_changed_action(preference),
                    )
                })
                .collect(),
            theme_selected,
        );
        let waveform_selected = match model.waveform_display {
            WaveformDisplay::BottomAligned => 0,
            WaveformDisplay::Centered => 1,
        };
        let waveform_group = SegmentedButtonGroup::new(
            vec![
                SegmentedButtonItem::new(
                    "整流",
                    app_shell_preferences_waveform_display_changed_action(
                        WaveformDisplay::BottomAligned,
                    ),
                ),
                SegmentedButtonItem::new(
                    "完整",
                    app_shell_preferences_waveform_display_changed_action(
                        WaveformDisplay::Centered,
                    ),
                ),
            ],
            waveform_selected,
        );
        let viewer_background_selected = match model.viewer_canvas_background {
            ViewerCanvasBackground::Checkerboard => 0,
            ViewerCanvasBackground::Black => 1,
        };
        let viewer_background_group = SegmentedButtonGroup::new(
            vec![
                SegmentedButtonItem::new(
                    "棋盘格",
                    app_shell_preferences_viewer_background_changed_action(
                        ViewerCanvasBackground::Checkerboard,
                    ),
                ),
                SegmentedButtonItem::new(
                    "黑色",
                    app_shell_preferences_viewer_background_changed_action(
                        ViewerCanvasBackground::Black,
                    ),
                ),
            ],
            viewer_background_selected,
        );
        let audio_output_device_dropdown = audio_output_device_dropdown(&model);
        let mut dialog = Self {
            id: WidgetId::new(),
            active_tab,
            model,
            surface: DialogSurface::new(
                Size::new(CARD_MIN_WIDTH, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            shortcut_viewport: Rect::ZERO,
            shortcut_scroll_offset: 0.0,
            shortcut_search: TextInput::new("搜索命令或快捷键..."),
            shortcut_search_query: String::new(),
            shortcut_sections: default_shortcut_sections(),
            shortcut_layout_rows: Vec::new(),
            hovered_shortcut_id: None,
            shortcut_actions_menu: None,
            shortcut_capture: None,
            collapsed_chevron: VectorIcon::from_svg_str(CHEVRON_RIGHT_SVG).ok(),
            expanded_chevron: VectorIcon::from_svg_str(CHEVRON_DOWN_SVG).ok(),
            nav_buttons,
            theme_group,
            waveform_group,
            viewer_background_group,
            audio_output_device_dropdown,
            content_labels: Vec::new(),
            close_button: Button::new("关闭").on_click(app_shell_close_modal_action()),
        };
        dialog.rebuild_content();
        dialog
    }

    /// Current selected preferences section.
    pub fn active_tab(&self) -> PreferencesDialogTab {
        self.active_tab
    }

    /// Current settings/status snapshot backing the dialog.
    pub fn model(&self) -> &AppUiPreferencesModel {
        &self.model
    }

    /// Update the backing settings/status snapshot without replacing widget ids.
    pub fn set_model(&mut self, model: AppUiPreferencesModel) {
        if self.model == model {
            return;
        }
        self.model = model;
        let theme_selected = ThemePreference::ALL
            .iter()
            .position(|preference| *preference == self.model.theme_preference)
            .unwrap_or(0);
        self.theme_group.set_selected_index(theme_selected);
        self.waveform_group.set_selected_index(
            if self.model.waveform_display == WaveformDisplay::BottomAligned {
                0
            } else {
                1
            },
        );
        self.viewer_background_group.set_selected_index(
            if self.model.viewer_canvas_background == ViewerCanvasBackground::Checkerboard {
                0
            } else {
                1
            },
        );
        self.audio_output_device_dropdown.set_model(
            self.model.audio_output_device_label.clone(),
            audio_output_device_items(&self.model),
        );
        if self
            .shortcut_capture
            .as_ref()
            .map(|capture| capture.id.as_str())
            .is_some_and(|id| !self.model.shortcut_rows.iter().any(|row| row.id == id))
        {
            self.shortcut_capture = None;
        }
        self.shortcut_actions_menu = self
            .shortcut_actions_menu
            .take()
            .filter(|state| self.model.shortcut_rows.iter().any(|row| row.id == state.id));
        self.clamp_shortcut_scroll();
        self.rebuild_content();
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
    }

    /// Select one preferences section and rebuild its view model.
    pub fn set_active_tab(&mut self, tab: PreferencesDialogTab) {
        if self.active_tab == tab {
            return;
        }
        self.active_tab = tab;
        for (i, button) in self.nav_buttons.iter_mut().enumerate() {
            button.set_active(PreferencesDialogTab::ALL[i] == tab);
        }
        if self.active_tab != PreferencesDialogTab::Shortcuts {
            self.shortcut_capture = None;
            self.shortcut_actions_menu = None;
            self.hovered_shortcut_id = None;
        }
        self.clamp_shortcut_scroll();
        self.rebuild_content();
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
    }

    fn rebuild_content(&mut self) {
        self.content_labels = content_rows_for_tab(self.active_tab, &self.model)
            .into_iter()
            .map(|row| {
                if row.heading {
                    Label::new(row.text)
                        .popover_foreground()
                        .with_font_size(BODY_FONT_SIZE)
                        .with_padding(0.0, 0.0)
                } else {
                    Label::new(row.text)
                        .muted()
                        .with_font_size(BODY_FONT_SIZE)
                        .with_padding(0.0, 0.0)
                        .wrapped()
                }
            })
            .collect();
        self.rebuild_shortcut_layout_rows();
    }

    fn shortcut_content_height(&self) -> f32 {
        self.shortcut_layout_rows
            .iter()
            .map(|row| match row.kind {
                ShortcutLayoutRowKind::Section { .. } => SHORTCUT_SECTION_HEIGHT,
                ShortcutLayoutRowKind::Command { .. } => SHORTCUT_ROW_HEIGHT,
            })
            .sum()
    }

    fn max_shortcut_scroll(&self) -> f32 {
        (self.shortcut_content_height() - self.shortcut_viewport.height).max(0.0)
    }

    fn clamp_shortcut_scroll(&mut self) {
        self.shortcut_scroll_offset =
            self.shortcut_scroll_offset.clamp(0.0, self.max_shortcut_scroll());
    }

    fn rebuild_shortcut_layout_rows(&mut self) {
        self.shortcut_layout_rows.clear();
        if self.active_tab != PreferencesDialogTab::Shortcuts {
            return;
        }
        let query = normalized_search_query(&self.shortcut_search_query);
        for section in &self.shortcut_sections {
            let rows: Vec<&ShortcutPreferenceRow> = self
                .model
                .shortcut_rows
                .iter()
                .filter(|row| row.category == section.category)
                .filter(|row| query.is_empty() || row.search_text.contains(&query))
                .collect();
            if rows.is_empty() {
                continue;
            }
            let collapsed = section.collapsed && query.is_empty();
            self.shortcut_layout_rows.push(ShortcutLayoutRow {
                kind: ShortcutLayoutRowKind::Section {
                    category: section.category,
                    title: shortcut_category_label(section.category),
                    collapsed,
                },
                rect: Rect::ZERO,
            });
            if !collapsed {
                self.shortcut_layout_rows.extend(rows.into_iter().map(|row| ShortcutLayoutRow {
                    kind: ShortcutLayoutRowKind::Command { id: row.id.clone() },
                    rect: Rect::ZERO,
                }));
            }
        }
        if let Some(id) = self.hovered_shortcut_id.clone()
            && !self
                .shortcut_layout_rows
                .iter()
                .any(|row| matches!(&row.kind, ShortcutLayoutRowKind::Command { id: row_id } if row_id == &id))
            {
                self.hovered_shortcut_id = None;
            }
    }

    fn find_shortcut_row(&self, id: &str) -> Option<&ShortcutPreferenceRow> {
        self.model.shortcut_rows.iter().find(|row| row.id == id)
    }

    fn toggle_shortcut_section(&mut self, category: AppUiCommandCategory, ctx: &mut EventContext) {
        if let Some(section) =
            self.shortcut_sections.iter_mut().find(|section| section.category == category)
        {
            section.collapsed = !section.collapsed;
            self.rebuild_shortcut_layout_rows();
            self.layout(self.bounds);
            ctx.request_repaint();
        }
    }

    fn row_keycap_rect(&self, rect: Rect, label: &str, disabled: bool) -> Rect {
        let action = self.row_action_rect(rect);
        let right = action.x - SHORTCUT_KEYCAP_ACTION_GAP;
        let min_left = rect.x + 240.0;
        let width = keycap_group_width(label, disabled).min((right - min_left).max(0.0));
        Rect::new(
            right - width,
            rect.y + (rect.height - SHORTCUT_KEYCAP_HEIGHT) * 0.5,
            width,
            SHORTCUT_KEYCAP_HEIGHT,
        )
    }

    fn row_action_rect(&self, rect: Rect) -> Rect {
        Rect::new(rect.x + rect.width - 42.0, rect.y + 7.0, 28.0, 24.0)
    }

    fn action_menu_anchor_for(&self, id: &str) -> Option<Point> {
        let row = self.shortcut_layout_rows.iter().find(|row| {
            matches!(&row.kind, ShortcutLayoutRowKind::Command { id: row_id } if row_id == id)
        })?;
        let action = self.row_action_rect(row.rect);
        Some(Point::new(
            action.x + action.width - SHORTCUT_ACTION_MENU_WIDTH,
            action.y,
        ))
    }

    fn shortcut_action_menu_for(&self, id: &str, anchor: Point) -> Option<ShortcutActionMenuState> {
        let row = self.find_shortcut_row(id)?;
        let reset = MenuItem::new(
            "重置",
            app_shell_preferences_shortcut_reset_action(row.id.clone()),
        );
        let reset = if row.overridden {
            reset
        } else {
            reset.disabled()
        };
        let disable = MenuItem::new(
            "禁用",
            app_shell_preferences_shortcut_disabled_action(row.id.clone()),
        );
        let disable = if row.disabled {
            disable.disabled()
        } else {
            disable
        };
        Some(ShortcutActionMenuState {
            id: row.id.clone(),
            menu: ContextMenu::new(anchor, vec![reset, disable]),
        })
    }

    fn shortcut_row_at(&self, position: Point) -> Option<&ShortcutLayoutRow> {
        self.shortcut_layout_rows.iter().find(|row| row.rect.contains(position))
    }

    fn shortcut_conflict_owner(&self, id: &str, binding: AppUiShortcutBinding) -> Option<String> {
        let label = binding.label();
        self.model
            .shortcut_rows
            .iter()
            .find(|row| row.id != id && !row.disabled && row.binding_label == label)
            .map(|row| row.id.clone())
    }

    fn begin_shortcut_capture(&mut self, id: String, ctx: &mut EventContext) {
        self.shortcut_capture =
            Some(ShortcutCaptureState { id, pending: None, conflict_owner: None });
        self.shortcut_actions_menu = None;
        ctx.request_repaint();
    }

    fn cancel_shortcut_capture(&mut self, ctx: &mut EventContext) {
        if self.shortcut_capture.take().is_some() {
            ctx.request_repaint();
        }
    }

    fn commit_shortcut_capture(&mut self, ctx: &mut EventContext) -> EventResult {
        let Some(capture) = self.shortcut_capture.clone() else {
            return EventResult::Ignored;
        };
        let Some(binding) = capture.pending else {
            return EventResult::Handled;
        };
        (ctx.dispatch)(app_shell_preferences_shortcut_rebound_action(
            PreferencesShortcutReboundPayload {
                id: capture.id,
                key: binding.key.preference_name().to_owned(),
                ctrl: binding.ctrl,
                alt: binding.alt,
                shift: binding.shift,
                meta: binding.meta,
            },
        ));
        self.shortcut_capture = None;
        ctx.request_repaint();
        EventResult::Handled
    }

    fn set_shortcut_scroll(&mut self, offset: f32, ctx: &mut EventContext) -> EventResult {
        let old = self.shortcut_scroll_offset;
        self.shortcut_scroll_offset = offset.clamp(0.0, self.max_shortcut_scroll());
        if (self.shortcut_scroll_offset - old).abs() > 0.01 {
            self.layout(self.bounds);
            ctx.request_repaint();
            EventResult::Handled
        } else {
            EventResult::Ignored
        }
    }

    fn capture_shortcut_key(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> EventResult {
        let Some(id) = self.shortcut_capture.as_ref().map(|capture| capture.id.clone()) else {
            return EventResult::Ignored;
        };
        if key == KeyCode::Escape {
            self.cancel_shortcut_capture(ctx);
            return EventResult::Handled;
        }
        if key == KeyCode::Enter {
            return self.commit_shortcut_capture(ctx);
        }
        let Some(key) = AppUiShortcutKey::from_key_code(key) else {
            return EventResult::Handled;
        };
        let binding = AppUiShortcutBinding {
            key,
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            shift: modifiers.shift,
            meta: modifiers.meta,
        };
        let conflict_owner = self.shortcut_conflict_owner(&id, binding);
        self.shortcut_capture =
            Some(ShortcutCaptureState { id, pending: Some(binding), conflict_owner });
        ctx.request_repaint();
        EventResult::Handled
    }
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for PreferencesDialog {
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

        let body_top = content.y;
        let nav_x = self.card.x + SIDEBAR_PADDING;
        let content_x = self.card.x + SIDEBAR_WIDTH + CONTENT_GAP;
        let list_bottom =
            self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT - 18.0;
        self.shortcut_viewport = if self.active_tab == PreferencesDialogTab::Shortcuts {
            Rect::new(
                content_x,
                body_top + SHORTCUT_SEARCH_HEIGHT + 14.0,
                (content.x + content.width - content_x).max(0.0),
                (list_bottom - (body_top + SHORTCUT_SEARCH_HEIGHT + 14.0)).max(0.0),
            )
        } else {
            Rect::ZERO
        };
        self.clamp_shortcut_scroll();
        for (index, button) in self.nav_buttons.iter_mut().enumerate() {
            button.layout(Rect::new(
                nav_x,
                body_top + index as f32 * (NAV_BUTTON_HEIGHT + NAV_BUTTON_GAP),
                NAV_WIDTH,
                NAV_BUTTON_HEIGHT,
            ));
        }
        if self.active_tab == PreferencesDialogTab::General {
            self.theme_group.layout(Rect::new(
                content_x + 126.0,
                body_top + ROW_HEIGHT + (ROW_HEIGHT - SEGMENTED_GROUP_HEIGHT) * 0.5,
                238.0,
                SEGMENTED_GROUP_HEIGHT,
            ));
            self.waveform_group.layout(Rect::new(
                content_x + 126.0,
                body_top + 2.0 * ROW_HEIGHT + (ROW_HEIGHT - SEGMENTED_GROUP_HEIGHT) * 0.5,
                160.0,
                SEGMENTED_GROUP_HEIGHT,
            ));
            self.viewer_background_group.layout(Rect::new(
                content_x + 126.0,
                body_top + 3.0 * ROW_HEIGHT + (ROW_HEIGHT - SEGMENTED_GROUP_HEIGHT) * 0.5,
                160.0,
                SEGMENTED_GROUP_HEIGHT,
            ));
            self.audio_output_device_dropdown.layout(Rect::ZERO);
        } else if self.active_tab == PreferencesDialogTab::Media {
            self.theme_group.layout(Rect::ZERO);
            self.waveform_group.layout(Rect::ZERO);
            self.viewer_background_group.layout(Rect::ZERO);
            self.audio_output_device_dropdown.layout(Rect::new(
                content_x + 126.0,
                body_top + 3.0 * ROW_HEIGHT + (ROW_HEIGHT - SEGMENTED_GROUP_HEIGHT) * 0.5,
                320.0,
                SEGMENTED_GROUP_HEIGHT,
            ));
        } else {
            self.theme_group.layout(Rect::ZERO);
            self.waveform_group.layout(Rect::ZERO);
            self.viewer_background_group.layout(Rect::ZERO);
            self.audio_output_device_dropdown.layout(Rect::ZERO);
        }
        if self.active_tab == PreferencesDialogTab::Shortcuts {
            self.shortcut_search.layout(Rect::new(
                content_x,
                body_top,
                (content.x + content.width - content_x).max(0.0),
                SHORTCUT_SEARCH_HEIGHT,
            ));
            let mut y = self.shortcut_viewport.y - self.shortcut_scroll_offset;
            for row in &mut self.shortcut_layout_rows {
                let height = match row.kind {
                    ShortcutLayoutRowKind::Section { .. } => SHORTCUT_SECTION_HEIGHT,
                    ShortcutLayoutRowKind::Command { .. } => SHORTCUT_ROW_HEIGHT,
                };
                row.rect = Rect::new(
                    self.shortcut_viewport.x,
                    y,
                    (self.shortcut_viewport.width - SHORTCUT_SCROLLBAR_GAP).max(0.0),
                    height,
                );
                y += height;
            }
        } else {
            self.shortcut_search.layout(Rect::ZERO);
            for row in &mut self.shortcut_layout_rows {
                row.rect = Rect::ZERO;
            }
        }
        let content_width = content.x + content.width - content_x;
        for (index, label) in self.content_labels.iter_mut().enumerate() {
            let y = body_top + index as f32 * ROW_HEIGHT;
            label.layout(Rect::new(content_x, y, content_width, ROW_HEIGHT));
        }

        self.close_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH,
            self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let UiEvent::KeyDown { key, modifiers } = event
            && self.shortcut_capture.is_some()
        {
            return self.capture_shortcut_key(*key, *modifiers, ctx);
        }
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape | KeyCode::Enter, .. }
                if !self.accepts_text_input() =>
            {
                (ctx.dispatch)(app_shell_close_modal_action());
                return EventResult::Handled;
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                return EventResult::Handled;
            }
            _ => {}
        }

        if let UiEvent::MouseWheel { delta, position, .. } = event
            && self.active_tab == PreferencesDialogTab::Shortcuts
            && self.shortcut_viewport.contains(*position)
        {
            return self.set_shortcut_scroll(self.shortcut_scroll_offset + *delta, ctx);
        }

        if self.close_button.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        for button in &mut self.nav_buttons {
            if let UiEvent::MouseDown { position, button: MouseButton::Left, .. } = event
                && button.hit_test(*position)
                && button.can_focus()
            {
                ctx.focus.request_focus(button.id());
            }
            if button.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        if self.active_tab == PreferencesDialogTab::General {
            if self.theme_group.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
            if self.waveform_group.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
            if self.viewer_background_group.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        if self.active_tab == PreferencesDialogTab::Media
            && self.audio_output_device_dropdown.event(event, ctx) == EventResult::Handled
        {
            return EventResult::Handled;
        }
        if self.active_tab == PreferencesDialogTab::Shortcuts {
            if let Some(menu_state) = &mut self.shortcut_actions_menu {
                let result = menu_state.menu.event(event, ctx);
                if !menu_state.menu.is_visible() {
                    self.shortcut_actions_menu = None;
                }
                if result == EventResult::Handled {
                    return EventResult::Handled;
                }
            }

            let before_query = self.shortcut_search.text().to_owned();
            if self.shortcut_search.event(event, ctx) == EventResult::Handled {
                let next_query = self.shortcut_search.text().to_owned();
                if next_query != before_query {
                    self.shortcut_search_query = next_query;
                    self.shortcut_scroll_offset = 0.0;
                    self.rebuild_shortcut_layout_rows();
                    self.layout(self.bounds);
                }
                return EventResult::Handled;
            }

            match event {
                UiEvent::MouseMove { position, .. } => {
                    let next_hover = if self.shortcut_viewport.contains(*position) {
                        self.shortcut_row_at(*position).and_then(|row| match &row.kind {
                            ShortcutLayoutRowKind::Command { id } => Some(id.clone()),
                            ShortcutLayoutRowKind::Section { .. } => None,
                        })
                    } else {
                        None
                    };
                    if next_hover != self.hovered_shortcut_id {
                        self.hovered_shortcut_id = next_hover;
                        ctx.request_repaint();
                    }
                }
                UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                    if let Some(capture) = &self.shortcut_capture {
                        let confirm = shortcut_capture_confirm_rect(self.shortcut_viewport);
                        let replace = Rect::new(
                            confirm.x + confirm.width - 148.0,
                            confirm.y + 8.0,
                            66.0,
                            24.0,
                        );
                        let cancel = Rect::new(
                            confirm.x + confirm.width - 74.0,
                            confirm.y + 8.0,
                            58.0,
                            24.0,
                        );
                        if capture.pending.is_some() && replace.contains(*position) {
                            return self.commit_shortcut_capture(ctx);
                        }
                        if cancel.contains(*position) {
                            self.cancel_shortcut_capture(ctx);
                            return EventResult::Handled;
                        }
                    }
                    let Some(row) = self.shortcut_row_at(*position) else {
                        self.shortcut_actions_menu = None;
                        return EventResult::Ignored;
                    };
                    match &row.kind {
                        ShortcutLayoutRowKind::Section { category, .. } => {
                            self.toggle_shortcut_section(*category, ctx);
                            return EventResult::Handled;
                        }
                        ShortcutLayoutRowKind::Command { id } => {
                            let Some(shortcut) = self.find_shortcut_row(id) else {
                                return EventResult::Ignored;
                            };
                            if self
                                .row_keycap_rect(
                                    row.rect,
                                    &shortcut.binding_label,
                                    shortcut.disabled,
                                )
                                .contains(*position)
                            {
                                self.begin_shortcut_capture(id.clone(), ctx);
                                return EventResult::Handled;
                            }
                            if self.row_action_rect(row.rect).contains(*position)
                                && self.hovered_shortcut_id.as_deref() == Some(id)
                                && let Some(anchor) = self.action_menu_anchor_for(id)
                            {
                                self.shortcut_actions_menu =
                                    self.shortcut_action_menu_for(id, anchor);
                                ctx.request_repaint();
                                return EventResult::Handled;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        // Step 1: full card — scrim + shadow + rounded-corner popover fill
        self.surface.paint(self.bounds, self.card, ctx);

        let tokens = &ctx.theme.colors;

        // Step 2: sidebar overlay — left corners rounded to match the card,
        // right corners square where they meet the content area.
        let sidebar = Rect::new(self.card.x, self.card.y, SIDEBAR_WIDTH, self.card.height);
        let mut sidebar_bg = tokens.foreground;
        sidebar_bg.a = 0.012;
        let radius = ctx.theme.spacing.radius_lg;
        ctx.encoder.draw_rect_radii(
            sidebar,
            sidebar_bg,
            CornerRadii {
                top_left: radius,
                top_right: 0.0,
                bottom_right: 0.0,
                bottom_left: radius,
            },
        );

        let _content = self.surface.content_rect(self.card);

        for button in &self.nav_buttons {
            button.paint(ctx);
        }
        if self.active_tab == PreferencesDialogTab::General {
            self.theme_group.paint(ctx);
            self.waveform_group.paint(ctx);
            self.viewer_background_group.paint(ctx);
        }
        if self.active_tab == PreferencesDialogTab::Media {
            self.audio_output_device_dropdown.paint(ctx);
        }
        if self.active_tab == PreferencesDialogTab::Shortcuts {
            self.shortcut_search.paint(ctx);
            ctx.push_clip(self.shortcut_viewport);
            for row in &self.shortcut_layout_rows {
                if row.rect.y + row.rect.height < self.shortcut_viewport.y
                    || row.rect.y > self.shortcut_viewport.y + self.shortcut_viewport.height
                {
                    continue;
                }
                match &row.kind {
                    ShortcutLayoutRowKind::Section { title, collapsed, .. } => {
                        let icon = if *collapsed {
                            self.collapsed_chevron.as_ref()
                        } else {
                            self.expanded_chevron.as_ref()
                        };
                        paint_shortcut_section(ctx, row.rect, title, icon);
                    }
                    ShortcutLayoutRowKind::Command { id } => {
                        if let Some(command) = self.find_shortcut_row(id) {
                            let hovered = self.hovered_shortcut_id.as_deref() == Some(id);
                            let capture =
                                self.shortcut_capture.as_ref().filter(|capture| capture.id == *id);
                            paint_shortcut_command_row(
                                ctx,
                                row.rect,
                                command,
                                self.row_keycap_rect(
                                    row.rect,
                                    &capture
                                        .and_then(|capture| {
                                            capture.pending.map(AppUiShortcutBinding::label)
                                        })
                                        .unwrap_or_else(|| command.binding_label.clone()),
                                    command.disabled,
                                ),
                                self.row_action_rect(row.rect),
                                hovered,
                                capture,
                            );
                        }
                    }
                }
            }
            ctx.pop_clip();
            if let Some(capture) = &self.shortcut_capture {
                paint_shortcut_capture_prompt(ctx, self.shortcut_viewport, capture, &self.model);
            }
            if let Some(menu_state) = &self.shortcut_actions_menu {
                menu_state.menu.paint_overlay(ctx);
            }
            paint_shortcut_scrollbar(
                ctx,
                self.shortcut_viewport,
                self.shortcut_scroll_offset,
                self.max_shortcut_scroll(),
            );
        } else {
            for label in &self.content_labels {
                label.paint(ctx);
            }
        }
        self.close_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        true
    }

    fn accepts_text_input(&self) -> bool {
        self.active_tab == PreferencesDialogTab::Shortcuts && self.shortcut_search.is_focused()
    }

    fn child_count(&self) -> usize {
        PREFERENCES_INTERACTIVE_CHILD_COUNT
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        let nav_start = 0;
        let nav_end = nav_start + self.nav_buttons.len();
        if (nav_start..nav_end).contains(&index) {
            return self.nav_buttons.get(index - nav_start).map(|button| button as &dyn Widget);
        }
        let theme_index = nav_end;
        if index == theme_index {
            return (self.active_tab == PreferencesDialogTab::General)
                .then_some(&self.theme_group as &dyn Widget);
        }
        let waveform_index = theme_index + 1;
        if index == waveform_index {
            return (self.active_tab == PreferencesDialogTab::General)
                .then_some(&self.waveform_group as &dyn Widget);
        }
        let viewer_background_index = waveform_index + 1;
        if index == viewer_background_index {
            return (self.active_tab == PreferencesDialogTab::General)
                .then_some(&self.viewer_background_group as &dyn Widget);
        }
        let audio_output_device_index = viewer_background_index + 1;
        if index == audio_output_device_index {
            return (self.active_tab == PreferencesDialogTab::Media)
                .then_some(&self.audio_output_device_dropdown as &dyn Widget);
        }
        let search_index = audio_output_device_index + 1;
        if index == search_index {
            return (self.active_tab == PreferencesDialogTab::Shortcuts)
                .then_some(&self.shortcut_search as &dyn Widget);
        }
        (index == search_index + 1).then_some(&self.close_button as &dyn Widget)
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        let nav_start = 0;
        let nav_end = nav_start + self.nav_buttons.len();
        if (nav_start..nav_end).contains(&index) {
            return self
                .nav_buttons
                .get_mut(index - nav_start)
                .map(|button| button as &mut dyn Widget);
        }
        let theme_index = nav_end;
        if index == theme_index {
            return (self.active_tab == PreferencesDialogTab::General)
                .then_some(&mut self.theme_group as &mut dyn Widget);
        }
        let waveform_index = theme_index + 1;
        if index == waveform_index {
            return (self.active_tab == PreferencesDialogTab::General)
                .then_some(&mut self.waveform_group as &mut dyn Widget);
        }
        let viewer_background_index = waveform_index + 1;
        if index == viewer_background_index {
            return (self.active_tab == PreferencesDialogTab::General)
                .then_some(&mut self.viewer_background_group as &mut dyn Widget);
        }
        let audio_output_device_index = viewer_background_index + 1;
        if index == audio_output_device_index {
            return (self.active_tab == PreferencesDialogTab::Media)
                .then_some(&mut self.audio_output_device_dropdown as &mut dyn Widget);
        }
        let search_index = audio_output_device_index + 1;
        if index == search_index {
            return (self.active_tab == PreferencesDialogTab::Shortcuts)
                .then_some(&mut self.shortcut_search as &mut dyn Widget);
        }
        (index == search_index + 1).then_some(&mut self.close_button as &mut dyn Widget)
    }
}

struct ContentRow {
    text: String,
    heading: bool,
}

fn heading(text: impl Into<String>) -> ContentRow {
    ContentRow { text: text.into(), heading: true }
}

fn detail(text: impl Into<String>) -> ContentRow {
    ContentRow { text: text.into(), heading: false }
}

fn default_shortcut_sections() -> Vec<ShortcutSectionState> {
    shortcut_category_order()
        .into_iter()
        .map(|category| ShortcutSectionState {
            category,
            collapsed: !matches!(
                category,
                AppUiCommandCategory::File | AppUiCommandCategory::Edit
            ),
        })
        .collect()
}

fn shortcut_category_order() -> Vec<AppUiCommandCategory> {
    vec![
        AppUiCommandCategory::File,
        AppUiCommandCategory::Edit,
        AppUiCommandCategory::Timeline,
        AppUiCommandCategory::Transport,
        AppUiCommandCategory::View,
        AppUiCommandCategory::Window,
        AppUiCommandCategory::Workspace,
        AppUiCommandCategory::Help,
    ]
}

fn shortcut_category_label(category: AppUiCommandCategory) -> &'static str {
    match category {
        AppUiCommandCategory::File => "File",
        AppUiCommandCategory::Edit => "Edit",
        AppUiCommandCategory::View => "View",
        AppUiCommandCategory::Window => "Window",
        AppUiCommandCategory::Workspace => "Workspace",
        AppUiCommandCategory::Timeline => "Timeline",
        AppUiCommandCategory::Transport => "Playback",
        AppUiCommandCategory::Help => "Help",
    }
}

fn normalized_search_query(query: &str) -> String {
    query.trim().to_lowercase()
}

fn shortcut_preference_rows(overrides: &[AppUiShortcutOverride]) -> Vec<ShortcutPreferenceRow> {
    let active = active_shortcuts(overrides);
    default_shortcuts()
        .into_iter()
        .map(|default| {
            let override_entry = overrides.iter().find(|entry| entry.id == default.id);
            let active_entry = active.iter().find(|shortcut| shortcut.id == default.id);
            let conflict_owner = active_entry
                .is_none()
                .then(|| {
                    active
                        .iter()
                        .find(|shortcut| {
                            shortcut.id != default.id && shortcut.binding == default.binding
                        })
                        .map(|shortcut| shortcut.id.to_owned())
                })
                .flatten();
            let disabled = override_entry.is_some_and(|entry| entry.binding.is_none())
                || active_entry.is_none();
            let label = active_entry.map(|shortcut| shortcut.label.clone()).unwrap_or_else(|| {
                conflict_owner
                    .as_ref()
                    .map_or_else(|| "已禁用".to_owned(), |owner| format!("与 {owner} 冲突"))
            });
            let command = command_by_id(default.id);
            let command_title = command
                .as_ref()
                .map(|command| command.title.to_owned())
                .unwrap_or_else(|| default.id.to_owned());
            let category = command
                .as_ref()
                .map(|command| command.category)
                .unwrap_or(AppUiCommandCategory::Help);
            let default_label = default.label;
            let search_text = format!(
                "{} {} {} {} {}",
                default.id,
                command_title,
                label,
                default_label,
                shortcut_category_label(category)
            )
            .to_lowercase();
            ShortcutPreferenceRow {
                id: default.id.to_owned(),
                command_title,
                category,
                binding_label: label,
                default_binding_label: default_label,
                overridden: override_entry.is_some(),
                disabled,
                conflict_owner,
                search_text,
            }
        })
        .collect()
}

fn with_alpha(mut color: Color, alpha: f32) -> Color {
    color.a *= alpha;
    color
}

fn paint_shortcut_section(
    ctx: &mut PaintContext,
    rect: Rect,
    title: &str,
    icon: Option<&VectorIcon>,
) {
    let colors = &ctx.theme.colors;
    let font_size = ctx.theme.typography.metadata.font_size;
    if let Some(icon) = icon {
        icon.paint(
            ctx,
            Rect::new(
                rect.x + 1.0,
                rect.y + (rect.height - SHORTCUT_CHEVRON_SIZE) * 0.5,
                SHORTCUT_CHEVRON_SIZE,
                SHORTCUT_CHEVRON_SIZE,
            ),
            colors.text_tertiary,
        );
    }
    ctx.encoder.draw_text(
        title,
        font_size,
        Point::new(rect.x + 20.0, rect.y + 9.0),
        colors.text_secondary,
    );
}

fn paint_shortcut_command_row(
    ctx: &mut PaintContext,
    rect: Rect,
    row: &ShortcutPreferenceRow,
    keycap_rect: Rect,
    action_rect: Rect,
    hovered: bool,
    capture: Option<&ShortcutCaptureState>,
) {
    let colors = &ctx.theme.colors;
    let radius = ctx.theme.spacing.radius_sm;
    if hovered || capture.is_some() {
        ctx.encoder.draw_rect(
            rect.inset(0.0, 2.0),
            with_alpha(colors.foreground, 0.055),
            radius,
        );
    }
    let title_color = if row.disabled {
        colors.text_tertiary
    } else {
        colors.foreground
    };
    ctx.encoder.draw_text(
        &row.command_title,
        ctx.theme.typography.body.font_size,
        Point::new(rect.x + 10.0, rect.y + 11.0),
        title_color,
    );
    if row.overridden || row.disabled || row.conflict_owner.is_some() {
        let status = if row.disabled {
            "Disabled"
        } else if row.conflict_owner.is_some() {
            "Conflict"
        } else {
            "Custom"
        };
        ctx.encoder.draw_text(
            status,
            ctx.theme.typography.metadata.font_size,
            Point::new(rect.x + 208.0, rect.y + 12.0),
            colors.text_tertiary,
        );
    }

    let label = capture
        .and_then(|capture| capture.pending.map(AppUiShortcutBinding::label))
        .unwrap_or_else(|| row.binding_label.clone());
    let capturing = capture.is_some();
    paint_keycap_label(ctx, keycap_rect, &label, row.disabled, capturing);

    if hovered || capture.is_some() {
        ctx.encoder.draw_rect(action_rect, with_alpha(colors.foreground, 0.055), radius);
        ctx.encoder.draw_text(
            "...",
            ctx.theme.typography.button.font_size,
            Point::new(action_rect.x + 8.0, action_rect.y + 5.0),
            colors.text_secondary,
        );
    }
}

fn paint_keycap_label(
    ctx: &mut PaintContext,
    rect: Rect,
    label: &str,
    disabled: bool,
    capturing: bool,
) {
    let colors = &ctx.theme.colors;
    let radius = ctx.theme.spacing.radius_sm;
    if capturing {
        ctx.encoder.draw_rect(rect, with_alpha(colors.primary, 0.18), radius);
        ctx.encoder.draw_rect(
            rect.inset(0.75, 0.75),
            with_alpha(colors.ring, 0.55),
            radius,
        );
        let text = if label.is_empty() {
            "按下组合"
        } else {
            label
        };
        ctx.encoder.draw_text(
            text,
            ctx.theme.typography.metadata.font_size,
            Point::new(rect.x + SHORTCUT_KEYCAP_PADDING_X, keycap_text_y(ctx, rect)),
            colors.foreground,
        );
        return;
    }
    if disabled || label == "已禁用" {
        ctx.encoder.draw_text(
            "未绑定",
            ctx.theme.typography.metadata.font_size,
            Point::new(rect.x + SHORTCUT_KEYCAP_PADDING_X, keycap_text_y(ctx, rect)),
            colors.text_tertiary,
        );
        return;
    }
    let mut x = rect.x;
    for (index, part) in label.split('+').enumerate() {
        if index > 0 {
            ctx.encoder.draw_text(
                "+",
                ctx.theme.typography.metadata.font_size,
                Point::new(x + 2.0, keycap_text_y(ctx, rect)),
                colors.text_tertiary,
            );
            x += SHORTCUT_KEYCAP_GAP + 8.0;
        }
        let width = keycap_part_width(part);
        let key = Rect::new(x, rect.y + 1.0, width, rect.height - 2.0);
        ctx.encoder.draw_rect(key, with_alpha(colors.foreground, 0.06), radius);
        ctx.encoder
            .draw_rect(key.inset(0.5, 0.5), with_alpha(colors.border, 0.55), radius);
        ctx.encoder.draw_text(
            part,
            ctx.theme.typography.metadata.font_size,
            Point::new(key.x + SHORTCUT_KEYCAP_PADDING_X, keycap_text_y(ctx, key)),
            colors.text_secondary,
        );
        x += width + SHORTCUT_KEYCAP_GAP;
    }
}

fn keycap_text_y(ctx: &PaintContext, rect: Rect) -> f32 {
    let font_size = ctx.theme.typography.metadata.font_size;
    rect.y + ((rect.height - font_size * 1.3) * 0.5).max(0.0)
}

fn keycap_part_width(part: &str) -> f32 {
    (part.chars().count() as f32 * 7.0 + SHORTCUT_KEYCAP_PADDING_X * 2.0)
        .max(SHORTCUT_KEYCAP_MIN_WIDTH)
}

fn keycap_group_width(label: &str, disabled: bool) -> f32 {
    if disabled || label == "已禁用" || label.is_empty() {
        return keycap_part_width("未绑定");
    }
    let parts: Vec<&str> = label.split('+').collect();
    let keys_width: f32 = parts.iter().map(|part| keycap_part_width(part)).sum();
    let separators = parts.len().saturating_sub(1) as f32 * (SHORTCUT_KEYCAP_GAP + 8.0);
    keys_width + separators
}

fn shortcut_capture_confirm_rect(viewport: Rect) -> Rect {
    Rect::new(
        viewport.x,
        viewport.y,
        viewport.width - SHORTCUT_SCROLLBAR_GAP,
        40.0,
    )
}

fn paint_shortcut_capture_prompt(
    ctx: &mut PaintContext,
    viewport: Rect,
    capture: &ShortcutCaptureState,
    model: &AppUiPreferencesModel,
) {
    let Some(pending) = capture.pending else {
        return;
    };
    let colors = &ctx.theme.colors;
    let rect = shortcut_capture_confirm_rect(viewport);
    ctx.encoder.draw_rect(
        rect,
        with_alpha(colors.popover, 0.96),
        ctx.theme.spacing.radius_md,
    );
    ctx.encoder.draw_rect(
        rect.inset(0.75, 0.75),
        with_alpha(colors.ring, 0.35),
        ctx.theme.spacing.radius_md,
    );
    let owner = capture
        .conflict_owner
        .as_ref()
        .and_then(|id| model.shortcut_rows.iter().find(|row| row.id == *id))
        .map(|row| row.command_title.as_str());
    let message = owner.map_or_else(
        || format!("确认绑定：{}", pending.label()),
        |owner| format!("已被占用：{owner}（{}）", pending.label()),
    );
    ctx.encoder.draw_text(
        &message,
        ctx.theme.typography.body.font_size,
        Point::new(rect.x + 12.0, rect.y + 13.0),
        colors.foreground,
    );
    let replace = Rect::new(rect.x + rect.width - 148.0, rect.y + 8.0, 66.0, 24.0);
    let cancel = Rect::new(rect.x + rect.width - 74.0, rect.y + 8.0, 58.0, 24.0);
    for (button, label) in [(replace, "替换"), (cancel, "取消")] {
        ctx.encoder.draw_rect(
            button,
            with_alpha(colors.foreground, 0.07),
            ctx.theme.spacing.radius_sm,
        );
        ctx.encoder.draw_text(
            label,
            ctx.theme.typography.button.font_size,
            Point::new(button.x + 16.0, button.y + 6.0),
            colors.foreground,
        );
    }
}

fn paint_shortcut_scrollbar(
    ctx: &mut PaintContext,
    viewport: Rect,
    scroll_offset: f32,
    max_scroll: f32,
) {
    if max_scroll <= 0.0 || viewport.height <= 0.0 {
        return;
    }
    let track = Rect::new(
        viewport.x + viewport.width - 3.0,
        viewport.y,
        3.0,
        viewport.height,
    );
    let content_height = viewport.height + max_scroll;
    let thumb_height =
        (viewport.height / content_height * viewport.height).clamp(24.0, viewport.height);
    let thumb_range = (viewport.height - thumb_height).max(0.0);
    let thumb_y = viewport.y + (scroll_offset / max_scroll) * thumb_range;
    ctx.encoder.draw_rect(track, ctx.theme.colors.muted, 1.5);
    ctx.encoder.draw_rect(
        Rect::new(track.x, thumb_y, track.width, thumb_height),
        ctx.theme.colors.muted_foreground,
        1.5,
    );
}

fn content_rows_for_tab(
    tab: PreferencesDialogTab,
    model: &AppUiPreferencesModel,
) -> Vec<ContentRow> {
    match tab {
        PreferencesDialogTab::General => vec![
            heading("外观"),
            detail(format!("主题：{}", model.theme_label)),
            detail(format!("波形显示：{}", model.waveform_display_label)),
            detail(format!(
                "透明画布：{}",
                model.viewer_canvas_background_label
            )),
            heading("工作区"),
            detail(format!("当前工作区：{}", model.workspace)),
            heading("项目"),
            detail(format!("项目：{}", model.project_status)),
            detail(format!("序列：{}", model.sequence_summary)),
        ],
        PreferencesDialogTab::Media => vec![
            heading("预览"),
            detail(format!("自动代理：{}", model.proxy_mode)),
            heading("音频"),
            detail("输出设备："),
            detail(format!("时钟：{}", model.audio_clock)),
            detail(format!("采样率：{}", model.audio_sample_rate)),
            heading("导出草稿"),
            detail(format!("范围：{}", model.export_range)),
            detail(format!("输出：{}", model.export_output)),
        ],
        PreferencesDialogTab::Shortcuts => Vec::new(),
        PreferencesDialogTab::Developer => vec![
            heading("诊断"),
            detail(format!("运行时诊断：{}", model.runtime_diagnostics)),
            detail(format!("日志过滤：{}", model.log_filter)),
            heading("运行时"),
            detail(format!("后台工作线程：{}", model.background_workers)),
        ],
    }
}

fn audio_output_device_label(
    selection: &RealtimeAudioOutputDeviceSelection,
    catalog: &AudioOutputDeviceCatalogState,
) -> String {
    match selection {
        RealtimeAudioOutputDeviceSelection::SystemDefault => {
            let default_name = match catalog {
                AudioOutputDeviceCatalogState::Ready(catalog) => catalog
                    .devices
                    .iter()
                    .find(|device| device.is_system_default)
                    .map(|device| device.display_name.as_str()),
                AudioOutputDeviceCatalogState::Loading
                | AudioOutputDeviceCatalogState::Failed(_) => None,
            };
            default_name.map_or_else(
                || "系统默认".to_owned(),
                |name| format!("系统默认 — {name}"),
            )
        }
        RealtimeAudioOutputDeviceSelection::Specific { device_id } => match catalog {
            AudioOutputDeviceCatalogState::Ready(catalog) => catalog
                .devices
                .iter()
                .find(|device| device.device_id.as_ref() == Some(device_id))
                .map(|device| device.display_name.clone())
                .unwrap_or_else(|| "已选设备当前不可用".to_owned()),
            AudioOutputDeviceCatalogState::Loading => "正在确认已选设备…".to_owned(),
            AudioOutputDeviceCatalogState::Failed(_) => "无法确认已选设备".to_owned(),
        },
    }
}

fn audio_output_device_dropdown(model: &AppUiPreferencesModel) -> Dropdown {
    Dropdown::new(
        model.audio_output_device_label.clone(),
        audio_output_device_items(model),
    )
    .with_max_visible_items(10)
}

fn audio_output_device_items(model: &AppUiPreferencesModel) -> Vec<MenuItem> {
    let mut items = vec![MenuItem::new(
        "跟随系统默认设备",
        app_shell_preferences_audio_output_device_changed_action(
            RealtimeAudioOutputDeviceSelection::SystemDefault,
        ),
    )
    .checked(matches!(
        model.audio_output_device_selection,
        RealtimeAudioOutputDeviceSelection::SystemDefault
    ))];
    match &model.audio_output_device_catalog {
        AudioOutputDeviceCatalogState::Loading => {
            items.push(MenuItem::inert("正在发现输出设备…"));
        }
        AudioOutputDeviceCatalogState::Failed(detail) => {
            items.push(MenuItem::inert(format!("设备发现失败：{detail}")));
        }
        AudioOutputDeviceCatalogState::Ready(catalog) => {
            items.extend(selectable_audio_output_device_items(
                catalog,
                &model.audio_output_device_selection,
            ));
        }
    }
    items.push(MenuItem::separator());
    items.push(MenuItem::new(
        "刷新设备列表",
        app_shell_preferences_refresh_audio_output_devices_action(),
    ));
    items
}

fn selectable_audio_output_device_items(
    catalog: &RealtimeAudioOutputDeviceCatalog,
    selection: &RealtimeAudioOutputDeviceSelection,
) -> Vec<MenuItem> {
    catalog
        .devices
        .iter()
        .map(|device| {
            let Some(device_id) = device.device_id.clone() else {
                return MenuItem::inert(format!("{}（身份不可用）", device.display_name));
            };
            let checked = matches!(
                selection,
                RealtimeAudioOutputDeviceSelection::Specific { device_id: selected }
                    if selected == &device_id
            );
            MenuItem::new(
                device.display_name.clone(),
                app_shell_preferences_audio_output_device_changed_action(
                    RealtimeAudioOutputDeviceSelection::Specific { device_id },
                ),
            )
            .checked(checked)
        })
        .collect()
}

fn enabled_label(enabled: bool) -> String {
    if enabled { "已启用" } else { "已禁用" }.to_owned()
}

fn waveform_display_label(mode: WaveformDisplay) -> &'static str {
    match mode {
        WaveformDisplay::BottomAligned => "整流",
        WaveformDisplay::Centered => "完整",
    }
}

fn viewer_canvas_background_label(background: ViewerCanvasBackground) -> &'static str {
    match background {
        ViewerCanvasBackground::Checkerboard => "棋盘格",
        ViewerCanvasBackground::Black => "黑色",
    }
}

fn export_range_label(range: TimelineExportRange) -> &'static str {
    match range {
        TimelineExportRange::EntireSequence => "整个序列",
        TimelineExportRange::SequenceInOut => "序列入点/出点",
        TimelineExportRange::WorkArea { .. } => "工作区域",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use mondrian_editor_state::Action;
    use mondrian_ui_core::widget::EventRequests;

    use crate::app::ui_actions::{
        PreferencesAudioOutputDevicePayload, PreferencesShortcutPayload,
        PreferencesShortcutReboundPayload, APP_SHELL_CLOSE_MODAL, APP_SHELL_NAMESPACE,
        APP_SHELL_PREFERENCES_AUDIO_OUTPUT_DEVICE_CHANGED, APP_SHELL_PREFERENCES_SHORTCUT_DISABLED,
        APP_SHELL_PREFERENCES_SHORTCUT_REBOUND, APP_SHELL_PREFERENCES_SHORTCUT_RESET,
        APP_SHELL_PREFERENCES_TAB_CHANGED, APP_SHELL_PREFERENCES_THEME_CHANGED,
    };
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};

    fn click(dialog: &mut PreferencesDialog, ctx: &mut EventContext<'_>, position: Point) {
        let down = dialog.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            ctx,
        );
        let up = dialog.event(
            &UiEvent::MouseUp {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            ctx,
        );
        assert!(
            down == EventResult::Handled || up == EventResult::Handled,
            "click at {position:?} was ignored"
        );
    }

    fn command_row_rect(dialog: &PreferencesDialog, id: &str) -> Rect {
        dialog
            .shortcut_layout_rows
            .iter()
            .find_map(|row| match &row.kind {
                ShortcutLayoutRowKind::Command { id: row_id } if row_id == id => Some(row.rect),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing visible shortcut command row {id}"))
    }

    fn shortcut_keycap_center(dialog: &PreferencesDialog, id: &str) -> Point {
        let row = dialog.find_shortcut_row(id).expect("shortcut row");
        dialog
            .row_keycap_rect(
                command_row_rect(dialog, id),
                &row.binding_label,
                row.disabled,
            )
            .center()
    }

    fn shortcut_action_center(dialog: &PreferencesDialog, id: &str) -> Point {
        dialog.row_action_rect(command_row_rect(dialog, id)).center()
    }

    fn shortcut_keycap_rect(dialog: &PreferencesDialog, id: &str) -> Rect {
        let row = dialog.find_shortcut_row(id).expect("shortcut row");
        dialog.row_keycap_rect(
            command_row_rect(dialog, id),
            &row.binding_label,
            row.disabled,
        )
    }

    fn shortcut_action_menu_item_point(
        dialog: &PreferencesDialog,
        id: &str,
        index: usize,
    ) -> Point {
        let anchor = dialog.action_menu_anchor_for(id).expect("action menu anchor");
        Point::new(anchor.x + 20.0, anchor.y + 10.0 + index as f32 * 28.0)
    }

    fn hover(dialog: &mut PreferencesDialog, ctx: &mut EventContext<'_>, position: Point) {
        dialog.event(
            &UiEvent::MouseMove { position, modifiers: Modifiers::none() },
            ctx,
        );
    }

    #[test]
    fn preferences_dialog_defaults_to_general() {
        let mut dialog = PreferencesDialog::new();
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));

        assert_eq!(dialog.active_tab(), PreferencesDialogTab::General);
        assert!(dialog.child_count() > PreferencesDialogTab::ALL.len());
        assert_eq!(
            dialog.measure(LayoutConstraint::LOOSE),
            Size::new(CARD_WIDTH, CARD_HEIGHT)
        );
    }

    #[test]
    fn preferences_interactive_child_slots_stay_stable_across_tabs() {
        let mut dialog = PreferencesDialog::new();
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));

        let theme_index = PreferencesDialogTab::ALL.len();
        let waveform_index = theme_index + 1;
        let viewer_background_index = waveform_index + 1;
        let audio_output_device_index = viewer_background_index + 1;
        let search_index = audio_output_device_index + 1;
        let close_index = search_index + 1;
        let close_id = dialog.close_button.id();

        assert_eq!(dialog.child_count(), PREFERENCES_INTERACTIVE_CHILD_COUNT);
        assert!(dialog.child(theme_index).is_some());
        assert!(dialog.child(waveform_index).is_some());
        assert!(dialog.child(viewer_background_index).is_some());
        assert!(dialog.child(audio_output_device_index).is_none());
        assert!(dialog.child(search_index).is_none());
        assert_eq!(dialog.child(close_index).map(Widget::id), Some(close_id));
        let close = dialog.child(close_index).expect("close child");
        assert!(close.can_focus());
        assert_eq!(
            close.accessibility().and_then(|node| node.name),
            Some("关闭".to_owned())
        );

        dialog.set_active_tab(PreferencesDialogTab::Media);
        assert_eq!(dialog.child_count(), PREFERENCES_INTERACTIVE_CHILD_COUNT);
        assert!(dialog.child(theme_index).is_none());
        assert!(dialog.child(waveform_index).is_none());
        assert!(dialog.child(viewer_background_index).is_none());
        assert!(dialog.child(audio_output_device_index).is_some());
        assert!(dialog.child(search_index).is_none());
        assert_eq!(dialog.child(close_index).map(Widget::id), Some(close_id));
        assert!(dialog.child(close_index).expect("close child").can_focus());

        dialog.set_active_tab(PreferencesDialogTab::Shortcuts);
        assert_eq!(dialog.child_count(), PREFERENCES_INTERACTIVE_CHILD_COUNT);
        assert!(dialog.child(theme_index).is_none());
        assert!(dialog.child(waveform_index).is_none());
        assert!(dialog.child(viewer_background_index).is_none());
        assert!(dialog.child(audio_output_device_index).is_none());
        assert!(dialog.child(search_index).is_some());
        assert_eq!(dialog.child(close_index).map(Widget::id), Some(close_id));
        assert!(dialog.child(close_index).expect("close child").can_focus());
    }

    #[test]
    fn preferences_tab_click_moves_focus_to_clicked_tab_button() {
        struct RecordingFocus {
            focused: Option<WidgetId>,
        }

        impl mondrian_ui_core::FocusManager for RecordingFocus {
            fn focused_widget(&self) -> Option<WidgetId> {
                self.focused
            }

            fn focused_panel(&self) -> Option<mondrian_editor_state::state::PanelKind> {
                None
            }

            fn request_focus(&mut self, widget: WidgetId) {
                self.focused = Some(widget);
            }

            fn release_focus(&mut self, widget: WidgetId) {
                if self.focused == Some(widget) {
                    self.focused = None;
                }
            }

            fn clear_focus(&mut self) {
                self.focused = None;
            }
        }

        let mut dialog = PreferencesDialog::new();
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        let media_button_id = dialog.nav_buttons[1].id();
        let close_button_id = dialog.close_button.id();
        let content = dialog.surface.content_rect(dialog.card);
        let media_point = Rect::new(
            dialog.card.x + SIDEBAR_PADDING,
            content.y + NAV_BUTTON_HEIGHT + NAV_BUTTON_GAP,
            NAV_WIDTH,
            NAV_BUTTON_HEIGHT,
        )
        .center();
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = RecordingFocus { focused: Some(dialog.theme_group.id()) };
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcut,
            tooltip: &mut tooltip,
            dispatch: &dispatch,
            platform: &mondrian_platform::NoopPlatformService,
            requests: &mut requests,
        };

        assert_eq!(
            dialog.event(
                &UiEvent::MouseDown {
                    position: media_point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(ctx.focus.focused_widget(), Some(media_button_id));
        assert_ne!(ctx.focus.focused_widget(), Some(close_button_id));
    }

    #[test]
    fn preferences_dialog_rebuilds_shortcut_rows_from_registry() {
        let dialog = PreferencesDialog::with_model_and_tab(
            AppUiPreferencesModel::default(),
            PreferencesDialogTab::Shortcuts,
        );

        assert_eq!(dialog.active_tab(), PreferencesDialogTab::Shortcuts);
        assert!(dialog.content_labels.is_empty());
        assert!(dialog.shortcut_layout_rows.iter().any(|row| matches!(
            row.kind,
            ShortcutLayoutRowKind::Section { category: AppUiCommandCategory::File, .. }
        )));
        assert!(dialog.shortcut_layout_rows.iter().any(
            |row| matches!(&row.kind, ShortcutLayoutRowKind::Command { id } if id == "file.save_project")
        ));
    }

    #[test]
    fn preferences_shortcut_rows_show_command_titles_not_debug_actions() {
        let model = AppUiPreferencesModel::default();

        assert!(model
            .shortcut_rows
            .iter()
            .any(|row| row.id == "file.new_project" && row.command_title == "新建项目"));
        assert!(
            model.shortcut_rows.iter().all(|row| !row.command_title.contains("Custom")
                && !row.command_title.contains("namespace")),
            "shortcut preference rows should not expose internal Action debug formatting"
        );
    }

    #[test]
    fn preferences_shortcut_rows_use_active_overrides() {
        let overrides =
            vec![AppUiShortcutOverride { id: "panel.inspector".to_owned(), binding: None }];
        let model = AppUiPreferencesModel::from_app_state_with_shortcut_overrides(
            &AppState::new(),
            WorkspacePreset::Editing,
            ThemePreference::Dark,
            ThemePreset::Dark,
            &overrides,
            WaveformDisplay::BottomAligned,
            ViewerCanvasBackground::Checkerboard,
        );

        let inspector = model
            .shortcut_rows
            .iter()
            .find(|row| row.id == "panel.inspector")
            .expect("inspector shortcut row");
        assert_eq!(inspector.binding_label, "已禁用");
        assert!(inspector.disabled);
        assert_eq!(inspector.conflict_owner, None);
        assert!(inspector.overridden);
        assert!(model
            .shortcut_rows
            .iter()
            .any(|row| row.id == "file.save_project" && row.binding_label == "Ctrl+S"));
    }

    #[test]
    fn preferences_shortcut_rows_hide_bindings_taken_by_overrides() {
        let overrides = vec![AppUiShortcutOverride {
            id: "file.save_project".to_owned(),
            binding: Some(crate::app_ui::shortcuts::AppUiShortcutBinding {
                key: AppUiShortcutKey::O,
                ctrl: true,
                alt: false,
                shift: false,
                meta: false,
            }),
        }];
        let model = AppUiPreferencesModel::from_app_state_with_shortcut_overrides(
            &AppState::new(),
            WorkspacePreset::Editing,
            ThemePreference::Dark,
            ThemePreset::Dark,
            &overrides,
            WaveformDisplay::BottomAligned,
            ViewerCanvasBackground::Checkerboard,
        );

        let save = model
            .shortcut_rows
            .iter()
            .find(|row| row.id == "file.save_project")
            .expect("save row");
        let open = model
            .shortcut_rows
            .iter()
            .find(|row| row.id == "file.open_project")
            .expect("open row");

        assert_eq!(save.binding_label, "Ctrl+O");
        assert!(!save.disabled);
        assert!(save.overridden);
        assert_eq!(open.binding_label, "与 file.save_project 冲突");
        assert!(open.disabled);
        assert!(!open.overridden);
        assert_eq!(open.conflict_owner.as_deref(), Some("file.save_project"));
    }

    #[test]
    fn preferences_shortcut_action_menu_dispatches_disable_and_reset_actions() {
        let overrides = vec![AppUiShortcutOverride {
            id: "file.save_project".to_owned(),
            binding: Some(crate::app_ui::shortcuts::AppUiShortcutBinding {
                key: crate::app_ui::shortcuts::AppUiShortcutKey::S,
                ctrl: true,
                alt: true,
                shift: false,
                meta: false,
            }),
        }];
        let model = AppUiPreferencesModel::from_app_state_with_shortcut_overrides(
            &AppState::new(),
            WorkspacePreset::Editing,
            ThemePreference::Dark,
            ThemePreset::Dark,
            &overrides,
            WaveformDisplay::BottomAligned,
            ViewerCanvasBackground::Checkerboard,
        );
        let mut dialog =
            PreferencesDialog::with_model_and_tab(model, PreferencesDialogTab::Shortcuts);
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        let actions = RefCell::new(Vec::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let action = shortcut_action_center(&dialog, "file.save_project");
        hover(&mut dialog, &mut ctx, action);
        click(&mut dialog, &mut ctx, action);
        let disable_point = shortcut_action_menu_item_point(&dialog, "file.save_project", 1);
        click(&mut dialog, &mut ctx, disable_point);

        hover(&mut dialog, &mut ctx, action);
        click(&mut dialog, &mut ctx, action);
        let reset_point = shortcut_action_menu_item_point(&dialog, "file.save_project", 0);
        click(&mut dialog, &mut ctx, reset_point);

        let recorded = actions.borrow();
        assert_eq!(recorded.len(), 2);
        match &recorded[0] {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_PREFERENCES_SHORTCUT_DISABLED);
                let payload: PreferencesShortcutPayload =
                    serde_json::from_value(payload.clone()).unwrap();
                assert_eq!(payload.id, "file.save_project");
            }
            other => panic!("expected shortcut disabled action, got {other:?}"),
        }
        match &recorded[1] {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_PREFERENCES_SHORTCUT_RESET);
                let payload: PreferencesShortcutPayload =
                    serde_json::from_value(payload.clone()).unwrap();
                assert_eq!(payload.id, "file.save_project");
            }
            other => panic!("expected shortcut reset action, got {other:?}"),
        }
    }

    #[test]
    fn preferences_shortcut_rebind_captures_next_keydown() {
        let mut dialog = PreferencesDialog::with_model_and_tab(
            AppUiPreferencesModel::default(),
            PreferencesDialogTab::Shortcuts,
        );
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        let actions = RefCell::new(Vec::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let keycap = shortcut_keycap_center(&dialog, "file.save_project");
        click(&mut dialog, &mut ctx, keycap);
        assert_eq!(
            dialog.event(
                &UiEvent::KeyDown {
                    key: KeyCode::I,
                    modifiers: Modifiers { ctrl: true, alt: true, shift: false, meta: false },
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(
            actions.borrow().is_empty(),
            "shortcut capture should wait for Enter confirmation"
        );
        assert_eq!(
            dialog.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        let recorded = actions.borrow();
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_PREFERENCES_SHORTCUT_REBOUND);
                let payload: PreferencesShortcutReboundPayload =
                    serde_json::from_value(payload.clone()).unwrap();
                assert_eq!(payload.id, "file.save_project");
                assert_eq!(payload.key, "I");
                assert!(payload.ctrl);
                assert!(payload.alt);
                assert!(!payload.shift);
                assert!(!payload.meta);
            }
            other => panic!("expected shortcut rebound action, got {other:?}"),
        }
    }

    #[test]
    fn preferences_shortcut_search_accepts_text_after_click() {
        let mut dialog = PreferencesDialog::with_model_and_tab(
            AppUiPreferencesModel::default(),
            PreferencesDialogTab::Shortcuts,
        );
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let search_rect = Rect::new(
            dialog.shortcut_viewport.x,
            dialog.shortcut_viewport.y - SHORTCUT_SEARCH_HEIGHT - 14.0,
            dialog.shortcut_viewport.width,
            SHORTCUT_SEARCH_HEIGHT,
        );
        click(&mut dialog, &mut ctx, search_rect.center());
        assert!(dialog.shortcut_search.is_focused());
        assert!(dialog.accepts_text_input());
        assert_eq!(
            dialog.event(&UiEvent::TextInput("save".into()), &mut ctx),
            EventResult::Handled
        );

        assert_eq!(dialog.shortcut_search_query, "save");
        assert!(dialog.shortcut_layout_rows.iter().any(
            |row| matches!(&row.kind, ShortcutLayoutRowKind::Command { id } if id == "file.save_project")
        ));
    }

    #[test]
    fn preferences_shortcut_keycaps_share_right_edge() {
        let model = AppUiPreferencesModel {
            shortcut_rows: vec![
                ShortcutPreferenceRow {
                    id: "test.one".into(),
                    command_title: "One".into(),
                    category: AppUiCommandCategory::File,
                    binding_label: "S".into(),
                    default_binding_label: "S".into(),
                    disabled: false,
                    overridden: false,
                    conflict_owner: None,
                    search_text: "one s".into(),
                },
                ShortcutPreferenceRow {
                    id: "test.two".into(),
                    command_title: "Two".into(),
                    category: AppUiCommandCategory::File,
                    binding_label: "Ctrl+S".into(),
                    default_binding_label: "Ctrl+S".into(),
                    disabled: false,
                    overridden: false,
                    conflict_owner: None,
                    search_text: "two ctrl s".into(),
                },
                ShortcutPreferenceRow {
                    id: "test.three".into(),
                    command_title: "Three".into(),
                    category: AppUiCommandCategory::File,
                    binding_label: "Ctrl+Shift+S".into(),
                    default_binding_label: "Ctrl+Shift+S".into(),
                    disabled: false,
                    overridden: false,
                    conflict_owner: None,
                    search_text: "three ctrl shift s".into(),
                },
            ],
            ..AppUiPreferencesModel::default()
        };
        let mut dialog =
            PreferencesDialog::with_model_and_tab(model, PreferencesDialogTab::Shortcuts);
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));

        let one = shortcut_keycap_rect(&dialog, "test.one");
        let two = shortcut_keycap_rect(&dialog, "test.two");
        let three = shortcut_keycap_rect(&dialog, "test.three");
        let right_one = one.x + one.width;
        let right_two = two.x + two.width;
        let right_three = three.x + three.width;

        assert!((right_one - right_two).abs() < 0.001);
        assert!((right_two - right_three).abs() < 0.001);
    }

    #[test]
    fn preferences_shortcut_search_reveals_collapsed_category_commands_before_dispatch() {
        let mut dialog = PreferencesDialog::with_model_and_tab(
            AppUiPreferencesModel::default(),
            PreferencesDialogTab::Shortcuts,
        );
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        let actions = RefCell::new(Vec::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert!(
            !dialog.shortcut_layout_rows.iter().any(
                |row| matches!(&row.kind, ShortcutLayoutRowKind::Command { id } if id == "panel.export")
            ),
            "Window category starts collapsed"
        );

        dialog.shortcut_search.set_text("export".to_owned());
        dialog.shortcut_search_query = "export".to_owned();
        dialog.rebuild_shortcut_layout_rows();
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));

        let export_action = shortcut_action_center(&dialog, "panel.export");
        assert!(dialog.shortcut_viewport.contains(export_action));
        hover(&mut dialog, &mut ctx, export_action);
        click(&mut dialog, &mut ctx, export_action);
        let disable_point = shortcut_action_menu_item_point(&dialog, "panel.export", 1);
        click(&mut dialog, &mut ctx, disable_point);

        let recorded = actions.borrow();
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_PREFERENCES_SHORTCUT_DISABLED);
                let payload: PreferencesShortcutPayload =
                    serde_json::from_value(payload.clone()).unwrap();
                assert_eq!(payload.id, "panel.export");
            }
            other => panic!("expected shortcut disabled action, got {other:?}"),
        }
    }

    #[test]
    fn preferences_model_reads_real_app_state_values() {
        let mut state = AppState::new();
        state.test_set_project_path("E:/projects/edit.mdp".into());
        state.test_set_sequence(Some(mondrian_timeline::sequence::Sequence::new("Cut")));
        state.test_project_settings_mut().proxy_enabled = true;
        state.export_draft.range = TimelineExportRange::EntireSequence;
        state.export_draft.output_path = "E:/renders/cut.mp4".to_owned();

        let model = AppUiPreferencesModel::from_app_state(
            &state,
            WorkspacePreset::Color,
            ThemePreference::Light,
            ThemePreset::Light,
        );

        assert_eq!(model.workspace, WorkspacePreset::Color.display_name());
        assert_eq!(model.theme_preference, ThemePreference::Light);
        assert_eq!(model.resolved_theme_preset, ThemePreset::Light);
        assert_eq!(model.theme_label, ThemePreference::Light.display_name());
        assert!(model.project_status.contains("edit.mdp"));
        assert!(model.sequence_summary.contains("Cut"));
        assert_eq!(model.proxy_mode, "已启用");
        assert_eq!(model.export_range, "整个序列");
        assert_eq!(model.export_output, "E:/renders/cut.mp4");
    }

    #[test]
    fn preferences_model_reports_effective_project_proxy_policy() {
        let mut state = AppState::new();
        state.auto_proxy_enabled = true;
        state.test_project_settings_mut().proxy_enabled = false;

        let model = AppUiPreferencesModel::from_app_state(
            &state,
            WorkspacePreset::Editing,
            ThemePreference::System,
            ThemePreset::Dark,
        );

        assert_eq!(model.proxy_mode, "已禁用");
    }

    #[test]
    fn audio_device_model_preserves_specific_identity_and_marks_current_row() {
        let device_id = mondrian_media::RealtimeAudioOutputDeviceId::new("wasapi:studio-out")
            .expect("fixture identity");
        let selection =
            RealtimeAudioOutputDeviceSelection::Specific { device_id: device_id.clone() };
        let model = AppUiPreferencesModel::from_app_state_with_shortcut_overrides_and_audio_output(
            &AppState::new(),
            WorkspacePreset::Editing,
            ThemePreference::System,
            ThemePreset::Dark,
            &[],
            WaveformDisplay::BottomAligned,
            ViewerCanvasBackground::Checkerboard,
            selection.clone(),
            AudioOutputDeviceCatalogState::Ready(RealtimeAudioOutputDeviceCatalog {
                host_name: "WASAPI".to_owned(),
                devices: vec![mondrian_media::RealtimeAudioOutputDeviceDescriptor {
                    device_id: Some(device_id),
                    display_name: "Studio Output".to_owned(),
                    is_system_default: false,
                    device_id_error: None,
                    description_error: None,
                }],
            }),
        );

        assert_eq!(model.audio_output_device_label, "Studio Output");
        let items = audio_output_device_items(&model);
        let selected = items.iter().find(|item| item.checked).expect("checked device row");
        let action = selected.action().expect("device selection action");
        let Action::Custom { name, payload, .. } = action else {
            panic!("expected custom device selection action");
        };
        assert_eq!(name, APP_SHELL_PREFERENCES_AUDIO_OUTPUT_DEVICE_CHANGED);
        assert_eq!(
            serde_json::from_value::<PreferencesAudioOutputDevicePayload>(payload.clone())
                .expect("device payload")
                .selection,
            selection
        );
    }

    #[test]
    fn preferences_theme_button_dispatches_theme_action() {
        let mut dialog = PreferencesDialog::new();
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        let actions = RefCell::new(Vec::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let light_center = dialog.theme_group.segment_rect(2).center();

        dialog.event(
            &UiEvent::MouseDown {
                position: light_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        dialog.event(
            &UiEvent::MouseUp {
                position: light_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        let recorded = actions.borrow();
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_PREFERENCES_THEME_CHANGED);
                assert_eq!(
                    serde_json::from_value::<crate::app::ui_actions::PreferencesThemePayload>(
                        payload.clone()
                    )
                    .unwrap()
                    .preference,
                    ThemePreference::Light
                );
            }
            other => panic!("expected preferences theme action, got {other:?}"),
        }
    }

    #[test]
    fn preferences_nav_dispatches_tab_action() {
        let mut dialog = PreferencesDialog::new();
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        let actions = RefCell::new(Vec::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let media_button = dialog.nav_buttons.get(1).expect("media nav button should exist").id();
        let content = dialog.surface.content_rect(dialog.card);
        let body_top = content.y;
        let nav_x = dialog.card.x + SIDEBAR_PADDING;
        let media_bounds = Rect::new(
            nav_x,
            body_top + NAV_BUTTON_HEIGHT + NAV_BUTTON_GAP,
            NAV_WIDTH,
            NAV_BUTTON_HEIGHT,
        );

        assert_ne!(media_button, WidgetId::new());
        dialog.event(
            &UiEvent::MouseDown {
                position: media_bounds.center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        dialog.event(
            &UiEvent::MouseUp {
                position: media_bounds.center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        let recorded = actions.borrow();
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_PREFERENCES_TAB_CHANGED);
                assert_eq!(
                    serde_json::from_value::<PreferencesTabPayload>(payload.clone()).unwrap(),
                    PreferencesTabPayload::Media
                );
            }
            other => panic!("expected preferences tab action, got {other:?}"),
        }
    }

    #[test]
    fn preferences_escape_closes_modal() {
        let mut dialog = PreferencesDialog::new();
        let actions = RefCell::new(Vec::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            dialog.event(
                &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        let action = actions.borrow()[0].clone();
        match &action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_CLOSE_MODAL);
                assert!(payload.is_null());
            }
            other => panic!("expected close modal action, got {other:?}"),
        }
    }
}
