//! Interpret Footage dialog for asset-library media records.

use mondrian_core::display_labels::color_space_label;
use mondrian_core::timeline_data::{AssetMediaInterpretation, MediaColorInterpretation};
use mondrian_core::types::{AssetId, ColorSpace};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Label};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_interpret_asset_dialog_action,
    app_shell_interpret_asset_draft_changed_action, AssetsSetInterpretationPayload,
    InterpretAssetDraftUpdatePayload,
};

const CARD_MIN_WIDTH: f32 = 500.0;
const CARD_WIDTH: f32 = 620.0;
const CARD_MIN_HEIGHT: f32 = 430.0;
const CARD_HEIGHT: f32 = 520.0;
const CONTENT_PADDING: f32 = 24.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const BODY_FONT_SIZE: f32 = 13.0;
const ROW_HEIGHT: f32 = 30.0;
const ROW_GAP: f32 = 8.0;
const BUTTON_WIDTH: f32 = 118.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_GAP: f32 = 10.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;

const OVERRIDE_COLOR_SPACES: [ColorSpace; 9] = [
    ColorSpace::Rec709,
    ColorSpace::Srgb,
    ColorSpace::Rec2020,
    ColorSpace::Rec2100Pq,
    ColorSpace::Rec2100Hlg,
    ColorSpace::DciP3,
    ColorSpace::AppleLog,
    ColorSpace::SLog3,
    ColorSpace::ArriLogC4,
];

/// Shell-local draft for the Interpret Footage dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiInterpretAssetDraft {
    /// Asset being interpreted.
    pub asset_id: AssetId,
    /// User-facing asset name.
    pub asset_name: String,
    /// Persistent interpretation currently selected in the dialog.
    pub interpretation: AssetMediaInterpretation,
}

impl AppUiInterpretAssetDraft {
    /// Build a draft from the current asset record state.
    pub fn new(
        asset_id: AssetId,
        asset_name: impl Into<String>,
        interpretation: AssetMediaInterpretation,
    ) -> Self {
        Self {
            asset_id,
            asset_name: asset_name.into(),
            interpretation,
        }
    }

    /// Apply one draft update from the dialog controls.
    pub fn apply_update(&mut self, update: InterpretAssetDraftUpdatePayload) {
        self.interpretation = update.interpretation;
    }

    /// Convert this draft into the persistent asset-library action payload.
    pub fn into_payload(self) -> AssetsSetInterpretationPayload {
        AssetsSetInterpretationPayload {
            asset_id: self.asset_id,
            interpretation: self.interpretation,
        }
    }
}

/// Modal for choosing persistent media interpretation for one asset.
pub struct InterpretAssetDialog {
    id: WidgetId,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    draft: AppUiInterpretAssetDraft,
    title_label: Label,
    asset_label: Label,
    summary_label: Label,
    auto_button: Button,
    data_button: Button,
    override_buttons: Vec<(ColorSpace, Button)>,
    apply_button: Button,
    cancel_button: Button,
}

impl InterpretAssetDialog {
    /// Build an Interpret Footage dialog.
    pub fn new(draft: AppUiInterpretAssetDraft) -> Self {
        let mut dialog = Self {
            id: WidgetId::new(),
            surface: DialogSurface::new(
                Size::new(CARD_MIN_WIDTH, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title_label: Label::new("解释素材")
                .popover_foreground()
                .with_font_size(TITLE_FONT_SIZE)
                .with_padding(0.0, 0.0),
            asset_label: Label::new(String::new())
                .muted()
                .with_font_size(BODY_FONT_SIZE)
                .with_padding(0.0, 0.0)
                .wrapped(),
            summary_label: Label::new(String::new())
                .muted()
                .with_font_size(BODY_FONT_SIZE)
                .with_padding(0.0, 0.0)
                .wrapped(),
            auto_button: interpretation_button("自动", MediaColorInterpretation::Auto),
            data_button: interpretation_button("数据/不做色彩转换", MediaColorInterpretation::Data),
            override_buttons: OVERRIDE_COLOR_SPACES
                .iter()
                .map(|&color_space| {
                    (
                        color_space,
                        interpretation_button(
                            color_space_label(color_space),
                            MediaColorInterpretation::Override { color_space },
                        ),
                    )
                })
                .collect(),
            apply_button: Button::new("应用")
                .on_click(app_shell_confirm_interpret_asset_dialog_action()),
            cancel_button: Button::new("取消").on_click(app_shell_close_modal_action()),
            draft,
        };
        dialog.refresh_labels();
        dialog
    }

    /// Current shell-local draft.
    pub fn draft(&self) -> &AppUiInterpretAssetDraft {
        &self.draft
    }

    /// Mutate the shell-local draft.
    pub fn apply_update(&mut self, update: InterpretAssetDraftUpdatePayload) {
        self.draft.apply_update(update);
        self.refresh_labels();
    }

    fn refresh_labels(&mut self) {
        self.asset_label.set_text(format!("素材：{}", self.draft.asset_name));
        self.summary_label.set_text(interpretation_summary(self.draft.interpretation));
    }
}

fn interpretation_button(label: impl Into<String>, color: MediaColorInterpretation) -> Button {
    Button::new(label).on_click(app_shell_interpret_asset_draft_changed_action(
        InterpretAssetDraftUpdatePayload { interpretation: AssetMediaInterpretation { color } },
    ))
}

fn interpretation_summary(interpretation: AssetMediaInterpretation) -> String {
    match interpretation.color {
        MediaColorInterpretation::Auto => {
            "当前：自动解释。导入 metadata、检测器和项目色彩策略会在预览/导出时实时解析。"
                .to_owned()
        }
        MediaColorInterpretation::Data => {
            "当前：数据素材。预览和导出不应对该素材应用色彩转换。".to_owned()
        }
        MediaColorInterpretation::Override { color_space } => {
            format!("当前：手动覆盖为 {}", color_space_label(color_space))
        }
    }
}

impl Widget for InterpretAssetDialog {
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
            content.y + 18.0,
            content.width,
            TITLE_FONT_SIZE * 1.4,
        ));
        self.asset_label.layout(Rect::new(
            content.x,
            content.y + 54.0,
            content.width,
            BODY_FONT_SIZE * 1.6,
        ));
        self.summary_label.layout(Rect::new(
            content.x,
            content.y + 82.0,
            content.width,
            BODY_FONT_SIZE * 3.2,
        ));

        let option_top = content.y + 136.0;
        self.auto_button
            .layout(Rect::new(content.x, option_top, content.width, ROW_HEIGHT));
        self.data_button.layout(Rect::new(
            content.x,
            option_top + ROW_HEIGHT + ROW_GAP,
            content.width,
            ROW_HEIGHT,
        ));

        let column_gap = 12.0;
        let column_width = (content.width - column_gap) * 0.5;
        let overrides_top = option_top + (ROW_HEIGHT + ROW_GAP) * 2.0 + 8.0;
        for (index, (_, button)) in self.override_buttons.iter_mut().enumerate() {
            let column = index % 2;
            let row = index / 2;
            let x = content.x + column as f32 * (column_width + column_gap);
            let y = overrides_top + row as f32 * (ROW_HEIGHT + ROW_GAP);
            button.layout(Rect::new(x, y, column_width, ROW_HEIGHT));
        }

        let button_y = self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT;
        let cancel_x = self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH;
        let apply_x = cancel_x - BUTTON_GAP - BUTTON_WIDTH;
        self.apply_button
            .layout(Rect::new(apply_x, button_y, BUTTON_WIDTH, BUTTON_HEIGHT));
        self.cancel_button
            .layout(Rect::new(cancel_x, button_y, BUTTON_WIDTH, BUTTON_HEIGHT));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_close_modal_action());
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_confirm_interpret_asset_dialog_action());
                EventResult::Handled
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                EventResult::Handled
            }
            _ => {
                for button in [&mut self.auto_button, &mut self.data_button] {
                    if button.event(event, ctx) == EventResult::Handled {
                        return EventResult::Handled;
                    }
                }
                for (_, button) in &mut self.override_buttons {
                    if button.event(event, ctx) == EventResult::Handled {
                        return EventResult::Handled;
                    }
                }
                for button in [&mut self.apply_button, &mut self.cancel_button] {
                    if button.event(event, ctx) == EventResult::Handled {
                        return EventResult::Handled;
                    }
                }
                EventResult::Ignored
            }
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title_label.paint(ctx);
        self.asset_label.paint(ctx);
        self.summary_label.paint(ctx);
        self.auto_button.paint(ctx);
        self.data_button.paint(ctx);
        for (_, button) in &self.override_buttons {
            button.paint(ctx);
        }
        self.apply_button.paint(ctx);
        self.cancel_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        7 + self.override_buttons.len()
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title_label),
            1 => Some(&self.asset_label),
            2 => Some(&self.summary_label),
            3 => Some(&self.auto_button),
            4 => Some(&self.data_button),
            index if index < 5 + self.override_buttons.len() => {
                self.override_buttons.get(index - 5).map(|(_, button)| button as &dyn Widget)
            }
            index if index == 5 + self.override_buttons.len() => Some(&self.apply_button),
            index if index == 6 + self.override_buttons.len() => Some(&self.cancel_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title_label),
            1 => Some(&mut self.asset_label),
            2 => Some(&mut self.summary_label),
            3 => Some(&mut self.auto_button),
            4 => Some(&mut self.data_button),
            index if index < 5 + self.override_buttons.len() => self
                .override_buttons
                .get_mut(index - 5)
                .map(|(_, button)| button as &mut dyn Widget),
            index if index == 5 + self.override_buttons.len() => Some(&mut self.apply_button),
            index if index == 6 + self.override_buttons.len() => Some(&mut self.cancel_button),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_converts_to_asset_payload() {
        let asset_id = AssetId::new();
        let draft = AppUiInterpretAssetDraft::new(
            asset_id,
            "Shot",
            AssetMediaInterpretation {
                color: MediaColorInterpretation::Override { color_space: ColorSpace::SLog3 },
            },
        );

        let payload = draft.into_payload();

        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(
            payload.interpretation.color.override_color_space(),
            Some(ColorSpace::SLog3)
        );
    }

    #[test]
    fn summary_distinguishes_auto_and_override() {
        assert!(interpretation_summary(AssetMediaInterpretation::default()).contains("自动"));
        assert!(interpretation_summary(AssetMediaInterpretation {
            color: MediaColorInterpretation::Override { color_space: ColorSpace::Rec2100Pq },
        })
        .contains("Rec. 2100 PQ"));
    }
}
