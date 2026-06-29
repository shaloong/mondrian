//! App UI Preferences dialog.
//!
//! This dialog owns the product settings surface for the custom UI shell. It
//! deliberately depends on app-shell actions instead of legacy egui preference
//! state so each preference can be migrated into a clean, typed boundary.

use mondrian_editor_state::state::WorkspacePreset;
use mondrian_export::preset::TimelineExportRange;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_theme::{ThemePreference, ThemePreset};
use mondrian_ui_widgets::{Button, DialogSurface, Label, WaveformDisplay};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_preferences_shortcut_disabled_action,
    app_shell_preferences_shortcut_rebound_action, app_shell_preferences_shortcut_reset_action,
    app_shell_preferences_tab_changed_action, app_shell_preferences_theme_changed_action,
    app_shell_preferences_waveform_display_changed_action, PreferencesShortcutReboundPayload,
    PreferencesTabPayload,
};
use crate::app::AppState;
use crate::app_ui::shortcuts::{
    active_shortcuts, default_shortcuts, AppUiShortcutKey, AppUiShortcutOverride,
};
use crate::app_ui::window::{APP_UI_BACKGROUND_WORKERS, DEFAULT_APP_UI_LOG_FILTER};

const CARD_MIN_WIDTH: f32 = 480.0;
const CARD_WIDTH: f32 = 680.0;
const CARD_MIN_HEIGHT: f32 = 360.0;
const CARD_HEIGHT: f32 = 480.0;
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
const THEME_BUTTON_WIDTH: f32 = 76.0;
const THEME_BUTTON_HEIGHT: f32 = 26.0;
const THEME_BUTTON_GAP: f32 = 6.0;
const SHORTCUT_BUTTON_WIDTH: f32 = 68.0;
const SHORTCUT_BUTTON_GAP: f32 = 6.0;
const SHORTCUT_BUTTON_HEIGHT: f32 = 24.0;
const SHORTCUT_HEADER_ROW_COUNT: usize = 2;

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
    pub export_range: String,
    pub export_output: String,
    pub runtime_diagnostics: String,
    pub log_filter: String,
    pub background_workers: String,
    pub shortcut_rows: Vec<ShortcutPreferenceRow>,
    pub waveform_display: WaveformDisplay,
    pub waveform_display_label: String,
}

/// One shortcut row shown in the app UI preferences UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutPreferenceRow {
    pub id: String,
    pub label: String,
    pub action: String,
    pub default_label: String,
    pub overridden: bool,
    pub disabled: bool,
    pub conflict_owner: Option<String>,
}

impl AppUiPreferencesModel {
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
    ) -> Self {
        let project_status = state
            .current_project_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未打开项目".to_owned());
        let sequence_summary = state
            .sequence
            .as_ref()
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
        Self {
            theme_preference,
            resolved_theme_preset,
            theme_label: theme_preference.display_name().to_owned(),
            project_status,
            workspace: workspace.display_name().to_owned(),
            sequence_summary,
            proxy_mode: enabled_label(state.auto_proxy_enabled),
            audio_clock: format!("{:?}", state.audio_sync.role),
            audio_sample_rate: format!("{} Hz", state.audio_sample_rate),
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
            audio_clock: "AudioMaster".to_owned(),
            audio_sample_rate: "48000 Hz".to_owned(),
            export_range: export_range_label(TimelineExportRange::SequenceInOut).to_owned(),
            export_output: "未选择".to_owned(),
            runtime_diagnostics: "跟踪已启用".to_owned(),
            log_filter: format!("RUST_LOG / {DEFAULT_APP_UI_LOG_FILTER}"),
            background_workers: APP_UI_BACKGROUND_WORKERS.to_string(),
            shortcut_rows: shortcut_preference_rows(&[]),
            waveform_display: WaveformDisplay::BottomAligned,
            waveform_display_label: "整流".to_owned(),
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
    capturing_shortcut: Option<String>,
    nav_buttons: Vec<Button>,
    theme_buttons: Vec<Button>,
    waveform_buttons: Vec<Button>,
    content_labels: Vec<Label>,
    shortcut_buttons: Vec<ShortcutPreferenceButtons>,
    close_button: Button,
}

struct ShortcutPreferenceButtons {
    id: String,
    rebind: Button,
    rebind_bounds: Rect,
    disable: Button,
    reset: Button,
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
        let theme_buttons = ThemePreference::ALL
            .into_iter()
            .map(|preference| {
                Button::new(preference.display_name())
                    .on_click(app_shell_preferences_theme_changed_action(preference))
            })
            .collect();
        let waveform_buttons = vec![
            Button::new("整流")
                .active(model.waveform_display == WaveformDisplay::BottomAligned)
                .on_click(app_shell_preferences_waveform_display_changed_action(
                    WaveformDisplay::BottomAligned,
                )),
            Button::new("完整")
                .active(model.waveform_display == WaveformDisplay::Centered)
                .on_click(app_shell_preferences_waveform_display_changed_action(
                    WaveformDisplay::Centered,
                )),
        ];
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
            capturing_shortcut: None,
            nav_buttons,
            theme_buttons,
            waveform_buttons,
            content_labels: Vec::new(),
            shortcut_buttons: Vec::new(),
            close_button: Button::new("完成").on_click(app_shell_close_modal_action()),
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
        self.waveform_buttons[0]
            .set_active(self.model.waveform_display == WaveformDisplay::BottomAligned);
        self.waveform_buttons[1]
            .set_active(self.model.waveform_display == WaveformDisplay::Centered);
        if self
            .capturing_shortcut
            .as_deref()
            .is_some_and(|id| !self.model.shortcut_rows.iter().any(|row| row.id == id))
        {
            self.capturing_shortcut = None;
        }
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
            self.capturing_shortcut = None;
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
        self.shortcut_buttons = if self.active_tab == PreferencesDialogTab::Shortcuts {
            self.model
                .shortcut_rows
                .iter()
                .map(|row| ShortcutPreferenceButtons {
                    id: row.id.clone(),
                    rebind: Button::new(if self.capturing_shortcut.as_deref() == Some(&row.id) {
                        "按下按键"
                    } else {
                        "重设"
                    }),
                    rebind_bounds: Rect::ZERO,
                    disable: Button::new("禁用")
                        .on_click(app_shell_preferences_shortcut_disabled_action(
                            row.id.clone(),
                        ))
                        .enabled(!row.disabled),
                    reset: Button::new("默认")
                        .on_click(app_shell_preferences_shortcut_reset_action(row.id.clone()))
                        .enabled(row.overridden),
                })
                .collect()
        } else {
            Vec::new()
        };
    }

    fn shortcut_content_height(&self) -> f32 {
        self.model.shortcut_rows.len() as f32 * ROW_HEIGHT
    }

    fn max_shortcut_scroll(&self) -> f32 {
        (self.shortcut_content_height() - self.shortcut_viewport.height).max(0.0)
    }

    fn clamp_shortcut_scroll(&mut self) {
        self.shortcut_scroll_offset =
            self.shortcut_scroll_offset.clamp(0.0, self.max_shortcut_scroll());
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

    fn begin_shortcut_capture(&mut self, id: String, ctx: &mut EventContext) {
        self.capturing_shortcut = Some(id);
        self.rebuild_content();
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
        ctx.request_repaint();
    }

    fn cancel_shortcut_capture(&mut self, ctx: &mut EventContext) {
        if self.capturing_shortcut.take().is_some() {
            self.rebuild_content();
            if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                self.layout(self.bounds);
            }
            ctx.request_repaint();
        }
    }

    fn capture_shortcut_key(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> EventResult {
        let Some(id) = self.capturing_shortcut.clone() else {
            return EventResult::Ignored;
        };
        if key == KeyCode::Escape {
            self.cancel_shortcut_capture(ctx);
            return EventResult::Handled;
        }
        let Some(key) = AppUiShortcutKey::from_key_code(key) else {
            return EventResult::Handled;
        };
        (ctx.dispatch)(app_shell_preferences_shortcut_rebound_action(
            PreferencesShortcutReboundPayload {
                id,
                key: key.preference_name().to_owned(),
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
                meta: modifiers.meta,
            },
        ));
        self.cancel_shortcut_capture(ctx);
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
                body_top + SHORTCUT_HEADER_ROW_COUNT as f32 * ROW_HEIGHT,
                (content.x + content.width - content_x).max(0.0),
                (list_bottom - (body_top + SHORTCUT_HEADER_ROW_COUNT as f32 * ROW_HEIGHT)).max(0.0),
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
        for (index, button) in self.theme_buttons.iter_mut().enumerate() {
            if self.active_tab == PreferencesDialogTab::General {
                button.layout(Rect::new(
                    content_x + 116.0 + index as f32 * (THEME_BUTTON_WIDTH + THEME_BUTTON_GAP),
                    body_top + ROW_HEIGHT + (ROW_HEIGHT - THEME_BUTTON_HEIGHT) * 0.5,
                    THEME_BUTTON_WIDTH,
                    THEME_BUTTON_HEIGHT,
                ));
            } else {
                button.layout(Rect::ZERO);
            }
        }
        for (index, button) in self.waveform_buttons.iter_mut().enumerate() {
            if self.active_tab == PreferencesDialogTab::General {
                button.layout(Rect::new(
                    content_x + 116.0 + index as f32 * (THEME_BUTTON_WIDTH + THEME_BUTTON_GAP),
                    body_top + 2.0 * ROW_HEIGHT + (ROW_HEIGHT - THEME_BUTTON_HEIGHT) * 0.5,
                    THEME_BUTTON_WIDTH,
                    THEME_BUTTON_HEIGHT,
                ));
            } else {
                button.layout(Rect::ZERO);
            }
        }
        let mut content_width = content.x + content.width - content_x;
        if self.active_tab == PreferencesDialogTab::Shortcuts {
            content_width =
                (content_width - SHORTCUT_BUTTON_WIDTH * 3.0 - SHORTCUT_BUTTON_GAP * 3.0).max(0.0);
        }
        for (index, label) in self.content_labels.iter_mut().enumerate() {
            let y = if self.active_tab == PreferencesDialogTab::Shortcuts
                && index >= SHORTCUT_HEADER_ROW_COUNT
            {
                self.shortcut_viewport.y + (index - SHORTCUT_HEADER_ROW_COUNT) as f32 * ROW_HEIGHT
                    - self.shortcut_scroll_offset
            } else {
                body_top + index as f32 * ROW_HEIGHT
            };
            label.layout(Rect::new(content_x, y, content_width, ROW_HEIGHT));
        }
        if self.active_tab == PreferencesDialogTab::Shortcuts {
            let buttons_left = content.x + content.width
                - SHORTCUT_BUTTON_WIDTH * 3.0
                - SHORTCUT_BUTTON_GAP * 2.0
                - 2.0;
            for (index, buttons) in self.shortcut_buttons.iter_mut().enumerate() {
                let y = self.shortcut_viewport.y + index as f32 * ROW_HEIGHT
                    - self.shortcut_scroll_offset
                    + (ROW_HEIGHT - SHORTCUT_BUTTON_HEIGHT) * 0.5;
                buttons.rebind_bounds = Rect::new(
                    buttons_left,
                    y,
                    SHORTCUT_BUTTON_WIDTH,
                    SHORTCUT_BUTTON_HEIGHT,
                );
                buttons.rebind.layout(buttons.rebind_bounds);
                buttons.disable.layout(Rect::new(
                    buttons_left + SHORTCUT_BUTTON_WIDTH + SHORTCUT_BUTTON_GAP,
                    y,
                    SHORTCUT_BUTTON_WIDTH,
                    SHORTCUT_BUTTON_HEIGHT,
                ));
                buttons.reset.layout(Rect::new(
                    buttons_left + (SHORTCUT_BUTTON_WIDTH + SHORTCUT_BUTTON_GAP) * 2.0,
                    y,
                    SHORTCUT_BUTTON_WIDTH,
                    SHORTCUT_BUTTON_HEIGHT,
                ));
            }
        }

        self.close_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH,
            self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let UiEvent::KeyDown { key, modifiers } = event {
            if self.capturing_shortcut.is_some() {
                return self.capture_shortcut_key(*key, *modifiers, ctx);
            }
        }
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape | KeyCode::Enter, .. } => {
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

        if let UiEvent::MouseWheel { delta, position, .. } = event {
            if self.active_tab == PreferencesDialogTab::Shortcuts
                && self.shortcut_viewport.contains(*position)
            {
                return self.set_shortcut_scroll(self.shortcut_scroll_offset + *delta, ctx);
            }
        }

        if self.close_button.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        for button in &mut self.nav_buttons {
            if button.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        if self.active_tab == PreferencesDialogTab::General {
            for button in &mut self.theme_buttons {
                if button.event(event, ctx) == EventResult::Handled {
                    return EventResult::Handled;
                }
            }
            for button in &mut self.waveform_buttons {
                if button.event(event, ctx) == EventResult::Handled {
                    return EventResult::Handled;
                }
            }
        }
        if self.active_tab == PreferencesDialogTab::Shortcuts {
            let pointer_position = pointer_position(event);
            let inside_viewport =
                pointer_position.is_none_or(|position| self.shortcut_viewport.contains(position));
            if inside_viewport {
                let mut capture_id = None;
                for buttons in &mut self.shortcut_buttons {
                    if buttons.rebind.event(event, ctx) == EventResult::Handled {
                        if let UiEvent::MouseUp { position, button: MouseButton::Left, .. } = event
                        {
                            if buttons.rebind_bounds.contains(*position) {
                                capture_id = Some(buttons.id.clone());
                            }
                        }
                        if capture_id.is_none() {
                            return EventResult::Handled;
                        }
                        break;
                    }
                    if buttons.disable.event(event, ctx) == EventResult::Handled {
                        return EventResult::Handled;
                    }
                    if buttons.reset.event(event, ctx) == EventResult::Handled {
                        return EventResult::Handled;
                    }
                }
                if let Some(id) = capture_id {
                    self.begin_shortcut_capture(id, ctx);
                    return EventResult::Handled;
                }
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

        let content = self.surface.content_rect(self.card);
        let body_top = content.y;

        for button in &self.nav_buttons {
            button.paint(ctx);
        }
        if self.active_tab == PreferencesDialogTab::General {
            for button in &self.theme_buttons {
                button.paint(ctx);
            }
            for (index, preference) in ThemePreference::ALL.into_iter().enumerate() {
                if preference == self.model.theme_preference {
                    paint_theme_button_outline(
                        ctx,
                        Rect::new(
                            content.x
                                + NAV_WIDTH
                                + CONTENT_GAP
                                + 116.0
                                + index as f32 * (THEME_BUTTON_WIDTH + THEME_BUTTON_GAP),
                            body_top + ROW_HEIGHT + (ROW_HEIGHT - THEME_BUTTON_HEIGHT) * 0.5,
                            THEME_BUTTON_WIDTH,
                            THEME_BUTTON_HEIGHT,
                        ),
                    );
                }
            }
            for button in &self.waveform_buttons {
                button.paint(ctx);
            }
            for (index, mode) in [WaveformDisplay::BottomAligned, WaveformDisplay::Centered]
                .into_iter()
                .enumerate()
            {
                if mode == self.model.waveform_display {
                    paint_theme_button_outline(
                        ctx,
                        Rect::new(
                            content.x
                                + NAV_WIDTH
                                + CONTENT_GAP
                                + 116.0
                                + index as f32 * (THEME_BUTTON_WIDTH + THEME_BUTTON_GAP),
                            body_top + 2.0 * ROW_HEIGHT + (ROW_HEIGHT - THEME_BUTTON_HEIGHT) * 0.5,
                            THEME_BUTTON_WIDTH,
                            THEME_BUTTON_HEIGHT,
                        ),
                    );
                }
            }
        }
        if self.active_tab == PreferencesDialogTab::Shortcuts {
            for label in self.content_labels.iter().take(SHORTCUT_HEADER_ROW_COUNT) {
                label.paint(ctx);
            }
            ctx.push_clip(self.shortcut_viewport);
            for label in self.content_labels.iter().skip(SHORTCUT_HEADER_ROW_COUNT) {
                label.paint(ctx);
            }
            for buttons in &self.shortcut_buttons {
                buttons.rebind.paint(ctx);
                buttons.disable.paint(ctx);
                buttons.reset.paint(ctx);
            }
            ctx.pop_clip();
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

    fn child_count(&self) -> usize {
        1 + self.nav_buttons.len()
            + self.theme_buttons.len()
            + self.waveform_buttons.len()
            + self.content_labels.len()
            + self.shortcut_buttons.len() * 3
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        let nav_start = 0;
        let nav_end = nav_start + self.nav_buttons.len();
        if (nav_start..nav_end).contains(&index) {
            return self.nav_buttons.get(index - nav_start).map(|button| button as &dyn Widget);
        }
        let theme_start = nav_end;
        let theme_end = theme_start + self.theme_buttons.len();
        if (theme_start..theme_end).contains(&index) {
            return self.theme_buttons.get(index - theme_start).map(|button| button as &dyn Widget);
        }
        let waveform_start = theme_end;
        let waveform_end = waveform_start + self.waveform_buttons.len();
        if (waveform_start..waveform_end).contains(&index) {
            return self
                .waveform_buttons
                .get(index - waveform_start)
                .map(|button| button as &dyn Widget);
        }
        let content_start = waveform_end;
        let content_end = content_start + self.content_labels.len();
        if (content_start..content_end).contains(&index) {
            return self
                .content_labels
                .get(index - content_start)
                .map(|label| label as &dyn Widget);
        }
        let shortcut_start = content_end;
        let shortcut_end = shortcut_start + self.shortcut_buttons.len() * 3;
        if (shortcut_start..shortcut_end).contains(&index) {
            let button_index = index - shortcut_start;
            let row = button_index / 3;
            return self.shortcut_buttons.get(row).map(|buttons| match button_index % 3 {
                0 => &buttons.rebind as &dyn Widget,
                1 => &buttons.disable as &dyn Widget,
                _ => &buttons.reset as &dyn Widget,
            });
        }
        (index == shortcut_end).then_some(&self.close_button as &dyn Widget)
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
        let theme_start = nav_end;
        let theme_end = theme_start + self.theme_buttons.len();
        if (theme_start..theme_end).contains(&index) {
            return self
                .theme_buttons
                .get_mut(index - theme_start)
                .map(|button| button as &mut dyn Widget);
        }
        let waveform_start = theme_end;
        let waveform_end = waveform_start + self.waveform_buttons.len();
        if (waveform_start..waveform_end).contains(&index) {
            return self
                .waveform_buttons
                .get_mut(index - waveform_start)
                .map(|button| button as &mut dyn Widget);
        }
        let content_start = waveform_end;
        let content_end = content_start + self.content_labels.len();
        if (content_start..content_end).contains(&index) {
            return self
                .content_labels
                .get_mut(index - content_start)
                .map(|label| label as &mut dyn Widget);
        }
        let shortcut_start = content_end;
        let shortcut_end = shortcut_start + self.shortcut_buttons.len() * 3;
        if (shortcut_start..shortcut_end).contains(&index) {
            let button_index = index - shortcut_start;
            let row = button_index / 3;
            return self.shortcut_buttons.get_mut(row).map(|buttons| match button_index % 3 {
                0 => &mut buttons.rebind as &mut dyn Widget,
                1 => &mut buttons.disable as &mut dyn Widget,
                _ => &mut buttons.reset as &mut dyn Widget,
            });
        }
        (index == shortcut_end).then_some(&mut self.close_button as &mut dyn Widget)
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

fn pointer_position(event: &UiEvent) -> Option<Point> {
    match event {
        UiEvent::MouseDown { position, .. }
        | UiEvent::MouseUp { position, .. }
        | UiEvent::MouseMove { position, .. }
        | UiEvent::MouseWheel { position, .. } => Some(*position),
        _ => None,
    }
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
            ShortcutPreferenceRow {
                id: default.id.to_owned(),
                label,
                action: format!("{:?}", default.action),
                default_label: default.label,
                overridden: override_entry.is_some(),
                disabled,
                conflict_owner,
            }
        })
        .collect()
}

fn paint_theme_button_outline(ctx: &mut PaintContext, bounds: Rect) {
    let color = ctx.theme.colors.ring;
    let left = bounds.x + 1.0;
    let right = bounds.x + bounds.width - 1.0;
    let top = bounds.y + 1.0;
    let bottom = bounds.y + bounds.height - 1.0;
    ctx.encoder.draw_line(Point::new(left, top), Point::new(right, top), 1.5, color);
    ctx.encoder.draw_line(
        Point::new(right, top),
        Point::new(right, bottom),
        1.5,
        color,
    );
    ctx.encoder.draw_line(
        Point::new(right, bottom),
        Point::new(left, bottom),
        1.5,
        color,
    );
    ctx.encoder
        .draw_line(Point::new(left, bottom), Point::new(left, top), 1.5, color);
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
            detail(format!("时钟：{}", model.audio_clock)),
            detail(format!("采样率：{}", model.audio_sample_rate)),
            heading("导出草稿"),
            detail(format!("范围：{}", model.export_range)),
            detail(format!("输出：{}", model.export_output)),
        ],
        PreferencesDialogTab::Shortcuts => {
            let mut rows = vec![heading("自研 UI 快捷键"), detail("当前命令绑定")];
            rows.extend(model.shortcut_rows.iter().map(|row| {
                let suffix = if row.disabled || row.overridden {
                    format!("默认 {}", row.default_label)
                } else {
                    "默认".to_owned()
                };
                detail(format!("{}  ·  {}  ·  {}", row.label, row.action, suffix))
            }));
            rows
        }
        PreferencesDialogTab::Developer => vec![
            heading("诊断"),
            detail(format!("运行时诊断：{}", model.runtime_diagnostics)),
            detail(format!("日志过滤：{}", model.log_filter)),
            heading("运行时"),
            detail(format!("后台工作线程：{}", model.background_workers)),
        ],
    }
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
        PreferencesShortcutPayload, PreferencesShortcutReboundPayload, APP_SHELL_CLOSE_MODAL,
        APP_SHELL_NAMESPACE, APP_SHELL_PREFERENCES_SHORTCUT_DISABLED,
        APP_SHELL_PREFERENCES_SHORTCUT_REBOUND, APP_SHELL_PREFERENCES_SHORTCUT_RESET,
        APP_SHELL_PREFERENCES_TAB_CHANGED, APP_SHELL_PREFERENCES_THEME_CHANGED,
    };
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};

    fn click(dialog: &mut PreferencesDialog, ctx: &mut EventContext<'_>, position: Point) {
        assert_eq!(
            dialog.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            dialog.event(
                &UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                ctx,
            ),
            EventResult::Handled
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
    fn preferences_dialog_rebuilds_shortcut_rows_from_registry() {
        let dialog = PreferencesDialog::with_model_and_tab(
            AppUiPreferencesModel::default(),
            PreferencesDialogTab::Shortcuts,
        );

        assert_eq!(dialog.active_tab(), PreferencesDialogTab::Shortcuts);
        assert!(dialog.content_labels.len() > crate::app_ui::shortcuts::default_shortcuts().len());
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
        );

        let inspector = model
            .shortcut_rows
            .iter()
            .find(|row| row.id == "panel.inspector")
            .expect("inspector shortcut row");
        assert_eq!(inspector.label, "已禁用");
        assert!(inspector.disabled);
        assert_eq!(inspector.conflict_owner, None);
        assert!(inspector.overridden);
        assert!(model
            .shortcut_rows
            .iter()
            .any(|row| row.id == "file.save_project" && row.label == "Ctrl+S"));
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

        assert_eq!(save.label, "Ctrl+O");
        assert!(!save.disabled);
        assert!(save.overridden);
        assert_eq!(open.label, "与 file.save_project 冲突");
        assert!(open.disabled);
        assert!(!open.overridden);
        assert_eq!(open.conflict_owner.as_deref(), Some("file.save_project"));
    }

    #[test]
    fn preferences_shortcut_buttons_dispatch_disable_and_default_actions() {
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

        let save_index = dialog
            .model
            .shortcut_rows
            .iter()
            .position(|row| row.id == "file.save_project")
            .expect("save row");
        let content = dialog.surface.content_rect(dialog.card);
        let buttons_left = content.x + content.width
            - SHORTCUT_BUTTON_WIDTH * 3.0
            - SHORTCUT_BUTTON_GAP * 2.0
            - 2.0;
        let shortcut_button_center = |row: usize, column: usize| {
            let x = buttons_left
                + column as f32 * (SHORTCUT_BUTTON_WIDTH + SHORTCUT_BUTTON_GAP)
                + SHORTCUT_BUTTON_WIDTH * 0.5;
            let y = content.y
                + (row + SHORTCUT_HEADER_ROW_COUNT) as f32 * ROW_HEIGHT
                + (ROW_HEIGHT - SHORTCUT_BUTTON_HEIGHT) * 0.5;
            Point::new(x, y)
        };
        let save_disable = shortcut_button_center(save_index, 1);
        let save_reset = shortcut_button_center(save_index, 2);

        click(&mut dialog, &mut ctx, save_disable);
        click(&mut dialog, &mut ctx, save_reset);

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

        let save_index = dialog
            .model
            .shortcut_rows
            .iter()
            .position(|row| row.id == "file.save_project")
            .expect("save row");
        let content = dialog.surface.content_rect(dialog.card);
        let buttons_left = content.x + content.width
            - SHORTCUT_BUTTON_WIDTH * 3.0
            - SHORTCUT_BUTTON_GAP * 2.0
            - 2.0;
        let rebind = Point::new(
            buttons_left + SHORTCUT_BUTTON_WIDTH * 0.5,
            content.y
                + (save_index + SHORTCUT_HEADER_ROW_COUNT) as f32 * ROW_HEIGHT
                + (ROW_HEIGHT - SHORTCUT_BUTTON_HEIGHT) * 0.5,
        );

        click(&mut dialog, &mut ctx, rebind);
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
    fn preferences_shortcuts_scroll_to_late_rows_before_dispatch() {
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

        assert!(dialog.max_shortcut_scroll() > 0.0);
        assert_eq!(
            dialog.event(
                &UiEvent::MouseWheel {
                    delta: 10_000.0,
                    position: dialog.shortcut_viewport.center(),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!((dialog.shortcut_scroll_offset - dialog.max_shortcut_scroll()).abs() < 0.01);

        let export_index = dialog
            .model
            .shortcut_rows
            .iter()
            .position(|row| row.id == "panel.export")
            .expect("export panel shortcut row");
        let content = dialog.surface.content_rect(dialog.card);
        let buttons_left = content.x + content.width
            - SHORTCUT_BUTTON_WIDTH * 3.0
            - SHORTCUT_BUTTON_GAP * 2.0
            - 2.0;
        let export_disable = Point::new(
            buttons_left
                + SHORTCUT_BUTTON_WIDTH
                + SHORTCUT_BUTTON_GAP
                + SHORTCUT_BUTTON_WIDTH * 0.5,
            dialog.shortcut_viewport.y + export_index as f32 * ROW_HEIGHT
                - dialog.shortcut_scroll_offset
                + ROW_HEIGHT * 0.5,
        );
        assert!(dialog.shortcut_viewport.contains(export_disable));

        click(&mut dialog, &mut ctx, export_disable);

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
        state.current_project_path = Some("E:/projects/edit.mdp".into());
        state.sequence = Some(mondrian_timeline::sequence::Sequence::new("Cut"));
        state.auto_proxy_enabled = true;
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
        let content = dialog.surface.content_rect(dialog.card);
        let body_top = content.y;
        let content_x = dialog.card.x + SIDEBAR_WIDTH + CONTENT_GAP;
        let light_bounds = Rect::new(
            content_x + 116.0 + 2.0 * (THEME_BUTTON_WIDTH + THEME_BUTTON_GAP),
            body_top + ROW_HEIGHT + (ROW_HEIGHT - THEME_BUTTON_HEIGHT) * 0.5,
            THEME_BUTTON_WIDTH,
            THEME_BUTTON_HEIGHT,
        );

        dialog.event(
            &UiEvent::MouseDown {
                position: light_bounds.center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        dialog.event(
            &UiEvent::MouseUp {
                position: light_bounds.center(),
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
