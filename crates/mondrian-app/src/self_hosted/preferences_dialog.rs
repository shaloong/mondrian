//! Self-hosted Preferences dialog.
//!
//! This dialog owns the product settings surface for the custom UI shell. It
//! deliberately depends on app-shell actions instead of legacy egui preference
//! state so each preference can be migrated into a clean, typed boundary.

use mondrian_editor_state::state::WorkspacePreset;
use mondrian_export::preset::TimelineExportRange;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_theme::ThemePreset;
use mondrian_ui_widgets::{Button, DialogSurface, Label};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_preferences_tab_changed_action,
    app_shell_preferences_theme_changed_action, PreferencesTabPayload,
};
use crate::app::AppState;
use crate::self_hosted::shortcuts::{active_shortcuts, SelfHostedShortcutOverride};
use crate::self_hosted::window::{DEFAULT_SELF_HOSTED_LOG_FILTER, SELF_HOSTED_BACKGROUND_WORKERS};

const CARD_MIN_WIDTH: f32 = 480.0;
const CARD_WIDTH: f32 = 680.0;
const CARD_MIN_HEIGHT: f32 = 380.0;
const CARD_HEIGHT: f32 = 520.0;
const CONTENT_PADDING: f32 = 22.0;
const TITLE_FONT_SIZE: f32 = 19.0;
const BODY_FONT_SIZE: f32 = 13.0;
const NAV_WIDTH: f32 = 148.0;
const NAV_BUTTON_HEIGHT: f32 = 30.0;
const NAV_BUTTON_GAP: f32 = 7.0;
const CONTENT_GAP: f32 = 24.0;
const ROW_HEIGHT: f32 = 28.0;
const BUTTON_WIDTH: f32 = 84.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;
const THEME_BUTTON_WIDTH: f32 = 76.0;
const THEME_BUTTON_HEIGHT: f32 = 26.0;
const THEME_BUTTON_GAP: f32 = 6.0;

/// Read-only settings/status snapshot shown by the self-hosted preferences UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHostedPreferencesModel {
    pub theme_preset: ThemePreset,
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
    pub shortcut_rows: Vec<String>,
}

impl SelfHostedPreferencesModel {
    /// Build the preferences model from the state actually owned by the
    /// self-hosted product shell.
    pub fn from_app_state(
        state: &AppState,
        workspace: WorkspacePreset,
        theme_preset: ThemePreset,
    ) -> Self {
        Self::from_app_state_with_shortcut_overrides(state, workspace, theme_preset, &[])
    }

    /// Build the preferences model using the active self-hosted shortcut table.
    pub fn from_app_state_with_shortcut_overrides(
        state: &AppState,
        workspace: WorkspacePreset,
        theme_preset: ThemePreset,
        shortcut_overrides: &[SelfHostedShortcutOverride],
    ) -> Self {
        let project_status = state
            .current_project_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "No project open".to_owned());
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
            .unwrap_or_else(|| "No active sequence".to_owned());
        Self {
            theme_preset,
            theme_label: theme_preset.display_name().to_owned(),
            project_status,
            workspace: workspace.display_name().to_owned(),
            sequence_summary,
            proxy_mode: enabled_label(state.auto_proxy_enabled),
            audio_clock: format!("{:?}", state.audio_sync.role),
            audio_sample_rate: format!("{} Hz", state.audio_sample_rate),
            export_range: export_range_label(state.export_draft.range).to_owned(),
            export_output: if state.export_draft.output_path.trim().is_empty() {
                "Not selected".to_owned()
            } else {
                state.export_draft.output_path.clone()
            },
            runtime_diagnostics: "Tracing enabled".to_owned(),
            log_filter: format!("RUST_LOG / {DEFAULT_SELF_HOSTED_LOG_FILTER}"),
            background_workers: SELF_HOSTED_BACKGROUND_WORKERS.to_string(),
            shortcut_rows: active_shortcuts(shortcut_overrides)
                .into_iter()
                .map(|shortcut| format!("{:<18} {:?}", shortcut.label, shortcut.action))
                .collect(),
        }
    }
}

impl Default for SelfHostedPreferencesModel {
    fn default() -> Self {
        Self {
            theme_preset: ThemePreset::Dark,
            theme_label: ThemePreset::Dark.display_name().to_owned(),
            project_status: "No project open".to_owned(),
            workspace: WorkspacePreset::Editing.display_name().to_owned(),
            sequence_summary: "No active sequence".to_owned(),
            proxy_mode: enabled_label(false),
            audio_clock: "AudioMaster".to_owned(),
            audio_sample_rate: "48000 Hz".to_owned(),
            export_range: export_range_label(TimelineExportRange::SequenceInOut).to_owned(),
            export_output: "Not selected".to_owned(),
            runtime_diagnostics: "Tracing enabled".to_owned(),
            log_filter: format!("RUST_LOG / {DEFAULT_SELF_HOSTED_LOG_FILTER}"),
            background_workers: SELF_HOSTED_BACKGROUND_WORKERS.to_string(),
            shortcut_rows: active_shortcuts(&[])
                .into_iter()
                .map(|shortcut| format!("{:<18} {:?}", shortcut.label, shortcut.action))
                .collect(),
        }
    }
}

/// Product preferences section shown by the self-hosted shell.
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
            Self::General => "General",
            Self::Media => "Media",
            Self::Shortcuts => "Shortcuts",
            Self::Developer => "Developer",
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

/// Preferences modal for the self-hosted product shell.
pub struct PreferencesDialog {
    id: WidgetId,
    active_tab: PreferencesDialogTab,
    model: SelfHostedPreferencesModel,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    description_label: Label,
    nav_buttons: Vec<Button>,
    theme_buttons: Vec<Button>,
    content_labels: Vec<Label>,
    close_button: Button,
}

impl PreferencesDialog {
    /// Build the preferences dialog with the default General tab.
    pub fn new() -> Self {
        Self::with_model(SelfHostedPreferencesModel::default())
    }

    /// Build the preferences dialog from an explicit model.
    pub fn with_model(model: SelfHostedPreferencesModel) -> Self {
        Self::with_model_and_tab(model, PreferencesDialogTab::General)
    }

    /// Build the preferences dialog from an explicit model and active tab.
    pub fn with_model_and_tab(
        model: SelfHostedPreferencesModel,
        active_tab: PreferencesDialogTab,
    ) -> Self {
        let nav_buttons = PreferencesDialogTab::ALL
            .into_iter()
            .map(|tab| {
                Button::new(tab.label())
                    .on_click(app_shell_preferences_tab_changed_action(tab.payload()))
            })
            .collect();
        let theme_buttons = ThemePreset::ALL
            .into_iter()
            .map(|preset| {
                Button::new(preset.display_name())
                    .on_click(app_shell_preferences_theme_changed_action(preset))
            })
            .collect();
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
            title_label: Label::new("Preferences")
                .popover_foreground()
                .with_font_size(TITLE_FONT_SIZE)
                .with_padding(0.0, 0.0),
            description_label: Label::new(
                "Custom UI settings are grouped by editing surface and runtime behavior.",
            )
            .muted()
            .with_font_size(BODY_FONT_SIZE)
            .with_padding(0.0, 0.0)
            .wrapped(),
            nav_buttons,
            theme_buttons,
            content_labels: Vec::new(),
            close_button: Button::new("Done").on_click(app_shell_close_modal_action()),
        };
        dialog.rebuild_content();
        dialog
    }

    /// Current selected preferences section.
    pub fn active_tab(&self) -> PreferencesDialogTab {
        self.active_tab
    }

    /// Current settings/status snapshot backing the dialog.
    pub fn model(&self) -> &SelfHostedPreferencesModel {
        &self.model
    }

    /// Update the backing settings/status snapshot without replacing widget ids.
    pub fn set_model(&mut self, model: SelfHostedPreferencesModel) {
        if self.model == model {
            return;
        }
        self.model = model;
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
        self.title_label.layout(Rect::new(
            content.x,
            content.y + 2.0,
            content.width,
            TITLE_FONT_SIZE * 1.4,
        ));
        self.description_label.layout(Rect::new(
            content.x,
            content.y + 34.0,
            content.width,
            BODY_FONT_SIZE * 2.2,
        ));

        let body_top = content.y + 82.0;
        let nav_x = content.x;
        let content_x = nav_x + NAV_WIDTH + CONTENT_GAP;
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
        let content_width = content.x + content.width - content_x;
        for (index, label) in self.content_labels.iter_mut().enumerate() {
            label.layout(Rect::new(
                content_x,
                body_top + index as f32 * ROW_HEIGHT,
                content_width,
                ROW_HEIGHT,
            ));
        }

        self.close_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH,
            self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
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
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title_label.paint(ctx);
        self.description_label.paint(ctx);

        let theme = &ctx.theme.colors;
        let content = self.surface.content_rect(self.card);
        let body_top = content.y + 82.0;
        let divider_x = content.x + NAV_WIDTH + CONTENT_GAP * 0.5;
        ctx.encoder.draw_line(
            Point::new(divider_x, body_top - 4.0),
            Point::new(
                divider_x,
                self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT - 18.0,
            ),
            1.0,
            theme.border,
        );

        for (index, tab) in PreferencesDialogTab::ALL.into_iter().enumerate() {
            if tab == self.active_tab {
                let selected_bounds = Rect::new(
                    content.x,
                    body_top + index as f32 * (NAV_BUTTON_HEIGHT + NAV_BUTTON_GAP),
                    NAV_WIDTH,
                    NAV_BUTTON_HEIGHT,
                );
                ctx.encoder.draw_rect(selected_bounds, theme.muted, 6.0);
            }
        }
        for button in &self.nav_buttons {
            button.paint(ctx);
        }
        if self.active_tab == PreferencesDialogTab::General {
            for button in &self.theme_buttons {
                button.paint(ctx);
            }
            for (index, preset) in ThemePreset::ALL.into_iter().enumerate() {
                if preset == self.model.theme_preset {
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
        }
        for label in &self.content_labels {
            label.paint(ctx);
        }
        self.close_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        3 + self.nav_buttons.len() + self.theme_buttons.len() + self.content_labels.len()
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        if index == 0 {
            return Some(&self.title_label);
        }
        if index == 1 {
            return Some(&self.description_label);
        }
        let nav_start = 2;
        let nav_end = nav_start + self.nav_buttons.len();
        if (nav_start..nav_end).contains(&index) {
            return self.nav_buttons.get(index - nav_start).map(|button| button as &dyn Widget);
        }
        let theme_start = nav_end;
        let theme_end = theme_start + self.theme_buttons.len();
        if (theme_start..theme_end).contains(&index) {
            return self.theme_buttons.get(index - theme_start).map(|button| button as &dyn Widget);
        }
        let content_start = theme_end;
        let content_end = content_start + self.content_labels.len();
        if (content_start..content_end).contains(&index) {
            return self
                .content_labels
                .get(index - content_start)
                .map(|label| label as &dyn Widget);
        }
        (index == content_end).then_some(&self.close_button as &dyn Widget)
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        if index == 0 {
            return Some(&mut self.title_label);
        }
        if index == 1 {
            return Some(&mut self.description_label);
        }
        let nav_start = 2;
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
        let content_start = theme_end;
        let content_end = content_start + self.content_labels.len();
        if (content_start..content_end).contains(&index) {
            return self
                .content_labels
                .get_mut(index - content_start)
                .map(|label| label as &mut dyn Widget);
        }
        (index == content_end).then_some(&mut self.close_button as &mut dyn Widget)
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

fn content_rows_for_tab(
    tab: PreferencesDialogTab,
    model: &SelfHostedPreferencesModel,
) -> Vec<ContentRow> {
    match tab {
        PreferencesDialogTab::General => vec![
            heading("Appearance"),
            detail(format!("Theme: {}", model.theme_label)),
            heading("Workspace"),
            detail(format!("Active workspace: {}", model.workspace)),
            heading("Project"),
            detail(format!("Project: {}", model.project_status)),
            detail(format!("Sequence: {}", model.sequence_summary)),
        ],
        PreferencesDialogTab::Media => vec![
            heading("Preview"),
            detail(format!("Auto proxy: {}", model.proxy_mode)),
            heading("Audio"),
            detail(format!("Clock: {}", model.audio_clock)),
            detail(format!("Sample rate: {}", model.audio_sample_rate)),
            heading("Export draft"),
            detail(format!("Range: {}", model.export_range)),
            detail(format!("Output: {}", model.export_output)),
        ],
        PreferencesDialogTab::Shortcuts => {
            let mut rows = vec![
                heading("Self-hosted shortcuts"),
                detail("Active command bindings"),
            ];
            rows.extend(model.shortcut_rows.iter().cloned().map(detail));
            rows
        }
        PreferencesDialogTab::Developer => vec![
            heading("Diagnostics"),
            detail(format!(
                "Runtime diagnostics: {}",
                model.runtime_diagnostics
            )),
            detail(format!("Log filter: {}", model.log_filter)),
            heading("Runtime"),
            detail(format!("Background workers: {}", model.background_workers)),
        ],
    }
}

fn enabled_label(enabled: bool) -> String {
    if enabled { "Enabled" } else { "Disabled" }.to_owned()
}

fn export_range_label(range: TimelineExportRange) -> &'static str {
    match range {
        TimelineExportRange::EntireSequence => "Entire sequence",
        TimelineExportRange::SequenceInOut => "Sequence in/out",
        TimelineExportRange::WorkArea { .. } => "Work area",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use mondrian_editor_state::Action;
    use mondrian_ui_core::widget::EventRequests;

    use crate::app::ui_actions::{
        APP_SHELL_CLOSE_MODAL, APP_SHELL_NAMESPACE, APP_SHELL_PREFERENCES_TAB_CHANGED,
        APP_SHELL_PREFERENCES_THEME_CHANGED,
    };
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};

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
            SelfHostedPreferencesModel::default(),
            PreferencesDialogTab::Shortcuts,
        );

        assert_eq!(dialog.active_tab(), PreferencesDialogTab::Shortcuts);
        assert!(
            dialog.content_labels.len() > crate::self_hosted::shortcuts::default_shortcuts().len()
        );
    }

    #[test]
    fn preferences_shortcut_rows_use_active_overrides() {
        let overrides =
            vec![SelfHostedShortcutOverride { id: "panel.inspector".to_owned(), binding: None }];
        let model = SelfHostedPreferencesModel::from_app_state_with_shortcut_overrides(
            &AppState::new(),
            WorkspacePreset::Editing,
            ThemePreset::Dark,
            &overrides,
        );

        assert!(model.shortcut_rows.iter().all(|row| !row.contains("FocusPanel(Inspector)")));
        assert!(model.shortcut_rows.iter().any(|row| row.contains("Ctrl+S")));
    }

    #[test]
    fn preferences_model_reads_real_app_state_values() {
        let mut state = AppState::new();
        state.current_project_path = Some("E:/projects/edit.mdp".into());
        state.sequence = Some(mondrian_timeline::sequence::Sequence::new("Cut"));
        state.auto_proxy_enabled = true;
        state.export_draft.range = TimelineExportRange::EntireSequence;
        state.export_draft.output_path = "E:/renders/cut.mp4".to_owned();

        let model = SelfHostedPreferencesModel::from_app_state(
            &state,
            WorkspacePreset::Color,
            ThemePreset::Light,
        );

        assert_eq!(model.workspace, WorkspacePreset::Color.display_name());
        assert_eq!(model.theme_preset, ThemePreset::Light);
        assert_eq!(model.theme_label, ThemePreset::Light.display_name());
        assert!(model.project_status.contains("edit.mdp"));
        assert!(model.sequence_summary.contains("Cut"));
        assert_eq!(model.proxy_mode, "Enabled");
        assert_eq!(model.export_range, "Entire sequence");
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
        let body_top = content.y + 82.0;
        let content_x = content.x + NAV_WIDTH + CONTENT_GAP;
        let light_bounds = Rect::new(
            content_x + 116.0 + THEME_BUTTON_WIDTH + THEME_BUTTON_GAP,
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
                    .preset,
                    ThemePreset::Light
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
        let body_top = content.y + 82.0;
        let media_bounds = Rect::new(
            content.x,
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
