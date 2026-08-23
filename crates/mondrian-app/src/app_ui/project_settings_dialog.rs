//! Project-wide color-engine dialog.
//!
//! The dialog owns only shell-local draft state. Committing emits one complete
//! color environment to `AppState`, where the selected OCIO config, the future
//! Sequence template, and every existing Sequence are validated atomically.
//! Existing Sequence settings are not rewritten; their resolved color contexts
//! all use the accepted Project engine.

use mondrian_core::{ColorEngine, ColorSpace, WorkingColorSpace};
use mondrian_platform::PlatformService;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::label::LabelColor;
use mondrian_ui_widgets::menu::Dropdown;
use mondrian_ui_widgets::{Button, DialogSurface, Label};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_project_settings_action,
    app_shell_project_settings_draft_changed_action, ProjectSettingsDraftUpdatePayload,
};
use crate::app_ui::color_management_controls::{
    choose_custom_ocio_config, color_engine_label, color_engine_menu_items,
};

/// Shell-local project color-settings form state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiProjectSettingsDraft {
    /// Proposed complete project color engine.
    pub engine: ColorEngine,
    /// Distinct Sequence working/output pairs required by the future-Sequence
    /// template and every current Sequence.
    sequence_color_contracts: Vec<(WorkingColorSpace, ColorSpace)>,
}

impl AppUiProjectSettingsDraft {
    /// Build a draft from the complete current Project color requirements.
    pub fn new(
        engine: ColorEngine,
        sequence_color_contracts: Vec<(WorkingColorSpace, ColorSpace)>,
    ) -> Self {
        Self { engine, sequence_color_contracts }
    }

    /// Apply one shell-local update.
    pub fn apply_update(&mut self, update: ProjectSettingsDraftUpdatePayload) {
        match update {
            ProjectSettingsDraftUpdatePayload::ColorEngine(engine) => self.engine = engine,
        }
    }

    fn working_space_summary(&self) -> String {
        let mut working_spaces = Vec::new();
        for (working_space, _) in &self.sequence_color_contracts {
            if !working_spaces.contains(working_space) {
                working_spaces.push(*working_space);
            }
        }
        working_spaces
            .into_iter()
            .map(working_space_label)
            .collect::<Vec<_>>()
            .join("、")
    }
}

fn project_color_dropdown_for(draft: &AppUiProjectSettingsDraft) -> Dropdown {
    Dropdown::new(
        color_engine_label(&draft.engine),
        color_engine_menu_items(|engine| {
            app_shell_project_settings_draft_changed_action(
                ProjectSettingsDraftUpdatePayload::ColorEngine(engine),
            )
        }),
    )
    .with_max_visible_items(4)
}

fn working_space_label(working_space: WorkingColorSpace) -> &'static str {
    match working_space {
        WorkingColorSpace::LinearRec709 => "Linear Rec. 709",
        WorkingColorSpace::LinearRec2020 => "Linear Rec. 2020",
        WorkingColorSpace::LinearP3D65 => "Linear P3-D65",
        WorkingColorSpace::AcesCg => "ACEScg",
    }
}

fn engine_detail(engine: &ColorEngine, working_space_summary: &str) -> String {
    match engine {
        ColorEngine::MondrianStandard { package } => format!(
            "内置固定包：{} · 当前工作空间：{}",
            package.package_id(),
            working_space_summary
        ),
        ColorEngine::Aces { preset } => format!(
            "正式 OCIO 内置配置：{} · 当前工作空间：{}",
            preset.builtin_name(),
            working_space_summary
        ),
        ColorEngine::CustomOcio { identity } => {
            let outputs = identity
                .outputs()
                .iter()
                .map(|output| {
                    format!(
                        "{:?}：{} / {}",
                        output.output_color_space(),
                        output.display(),
                        output.view()
                    )
                })
                .collect::<Vec<_>>()
                .join("；");
            format!(
                "{}\n输出绑定：{}\n配置 SHA-256：{}",
                identity.source(),
                outputs,
                identity.config_sha256()
            )
        }
    }
}

const CARD_MIN_WIDTH: f32 = 360.0;
const CARD_WIDTH: f32 = 560.0;
const CARD_MIN_HEIGHT: f32 = 280.0;
const CARD_HEIGHT: f32 = 350.0;
const CONTENT_PADDING: f32 = 20.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const LABEL_FONT_SIZE: f32 = 12.0;
const TITLE_Y: f32 = 20.0;
const DESCRIPTION_Y: f32 = 48.0;
const MODE_LABEL_Y: f32 = 96.0;
const MODE_DROPDOWN_Y: f32 = 116.0;
const DROPDOWN_HEIGHT: f32 = 30.0;
const DETAIL_Y: f32 = 164.0;
const ERROR_Y: f32 = 245.0;
const BUTTON_WIDTH: f32 = 88.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_GAP: f32 = 8.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;

/// App UI project-level color-management dialog.
pub struct ProjectSettingsDialog {
    id: WidgetId,
    draft: AppUiProjectSettingsDraft,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    description_label: Label,
    mode_label: Label,
    mode_dropdown: Dropdown,
    detail_label: Label,
    error_label: Label,
    cancel_button: Button,
    apply_button: Button,
}

impl ProjectSettingsDialog {
    /// Build a project-settings dialog from current state.
    pub fn new(draft: AppUiProjectSettingsDraft) -> Self {
        let mode_dropdown = project_color_dropdown_for(&draft);
        let detail_label = Label::new(engine_detail(&draft.engine, &draft.working_space_summary()))
            .secondary()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0)
            .wrapped();
        Self {
            id: WidgetId::new(),
            draft,
            surface: DialogSurface::new(
                Size::new(CARD_MIN_WIDTH, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title_label: Label::new("项目色彩引擎")
                .popover_foreground()
                .with_font_size(TITLE_FONT_SIZE)
                .with_padding(0.0, 0.0),
            description_label: Label::new(
                "此引擎作用于项目内全部序列。应用前会校验新建序列默认值和每个现有序列；任一不兼容都会整次拒绝，不会自动改写序列。",
            )
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0)
            .wrapped(),
            mode_label: Label::new("项目色彩引擎")
                .muted()
                .with_font_size(LABEL_FONT_SIZE)
                .with_padding(0.0, 0.0),
            mode_dropdown,
            detail_label,
            error_label: Label::new("")
                .with_semantic_color(LabelColor::Destructive)
                .with_font_size(LABEL_FONT_SIZE)
                .with_padding(0.0, 0.0)
                .wrapped(),
            cancel_button: Button::new("取消").on_click(app_shell_close_modal_action()),
            apply_button: Button::new("应用").on_click(app_shell_confirm_project_settings_action()),
        }
    }

    /// Apply one draft update and rebuild derived controls.
    pub fn apply_update(&mut self, update: ProjectSettingsDraftUpdatePayload) {
        self.draft.apply_update(update);
        self.mode_dropdown = project_color_dropdown_for(&self.draft);
        self.detail_label.set_text(engine_detail(
            &self.draft.engine,
            &self.draft.working_space_summary(),
        ));
        self.error_label.set_text(String::new());
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
    }

    /// Run native Custom OCIO selection and update only after complete pinning.
    pub fn choose_custom_ocio(&mut self, platform: &dyn PlatformService) {
        match choose_custom_ocio_config(platform, &self.draft.sequence_color_contracts) {
            Ok(Some(engine)) => {
                self.apply_update(ProjectSettingsDraftUpdatePayload::ColorEngine(engine));
            }
            Ok(None) => {}
            Err(error) => self.error_label.set_text(error),
        }
    }

    /// Read the current draft.
    pub fn draft(&self) -> &AppUiProjectSettingsDraft {
        &self.draft
    }

    #[cfg(test)]
    pub fn error_text(&self) -> &str {
        self.error_label.text()
    }
}

impl Widget for ProjectSettingsDialog {
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
            content.y + TITLE_Y,
            content.width,
            26.0,
        ));
        self.description_label.layout(Rect::new(
            content.x,
            content.y + DESCRIPTION_Y,
            content.width,
            38.0,
        ));
        self.mode_label.layout(Rect::new(
            content.x,
            content.y + MODE_LABEL_Y,
            content.width,
            18.0,
        ));
        self.mode_dropdown.layout(Rect::new(
            content.x,
            content.y + MODE_DROPDOWN_Y,
            content.width,
            DROPDOWN_HEIGHT,
        ));
        self.detail_label.layout(Rect::new(
            content.x,
            content.y + DETAIL_Y,
            content.width,
            68.0,
        ));
        self.error_label.layout(Rect::new(
            content.x,
            content.y + ERROR_Y,
            content.width,
            34.0,
        ));
        let button_y = self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT;
        self.cancel_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH * 2.0 - BUTTON_GAP,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
        self.apply_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_close_modal_action());
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_confirm_project_settings_action());
                return EventResult::Handled;
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                return EventResult::Handled;
            }
            _ => {}
        }
        if self.apply_button.event(event, ctx) == EventResult::Handled
            || self.cancel_button.event(event, ctx) == EventResult::Handled
            || self.mode_dropdown.event(event, ctx) == EventResult::Handled
        {
            return EventResult::Handled;
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title_label.paint(ctx);
        self.description_label.paint(ctx);
        self.mode_label.paint(ctx);
        self.mode_dropdown.paint(ctx);
        self.detail_label.paint(ctx);
        self.error_label.paint(ctx);
        self.cancel_button.paint(ctx);
        self.apply_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        8
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title_label),
            1 => Some(&self.description_label),
            2 => Some(&self.mode_label),
            3 => Some(&self.mode_dropdown),
            4 => Some(&self.detail_label),
            5 => Some(&self.error_label),
            6 => Some(&self.cancel_button),
            7 => Some(&self.apply_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title_label),
            1 => Some(&mut self.description_label),
            2 => Some(&mut self.mode_label),
            3 => Some(&mut self.mode_dropdown),
            4 => Some(&mut self.detail_label),
            5 => Some(&mut self.error_label),
            6 => Some(&mut self.cancel_button),
            7 => Some(&mut self.apply_button),
            _ => None,
        }
    }
}
