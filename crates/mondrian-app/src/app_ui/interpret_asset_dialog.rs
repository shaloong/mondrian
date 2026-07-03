//! Interpret Footage dialog for asset-library media records.

use mondrian_core::display_labels::color_space_label;
use mondrian_core::timeline_data::{AssetMediaInterpretation, MediaColorInterpretation};
use mondrian_core::types::{AssetId, ColorSpace};
use mondrian_media::{
    DetectedColorInterpretation, VideoColorDetectionMethod, VideoColorInterpretationConfidence,
};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Dropdown, Label, MenuItem};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_interpret_asset_dialog_action,
    app_shell_interpret_asset_draft_changed_action, AssetsSetInterpretationPayload,
    InterpretAssetDraftUpdatePayload,
};

const CARD_MIN_WIDTH: f32 = 500.0;
const CARD_WIDTH: f32 = 620.0;
const CARD_MIN_HEIGHT: f32 = 300.0;
const CARD_HEIGHT: f32 = 360.0;
const CONTENT_PADDING: f32 = 24.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const BODY_FONT_SIZE: f32 = 13.0;
const LABEL_COLUMN_WIDTH: f32 = 112.0;
const ROW_HEIGHT: f32 = 32.0;
const ROW_GAP: f32 = 16.0;
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
    /// Current structured automatic color interpretation from media metadata.
    pub auto_interpretation: Option<DetectedColorInterpretation>,
}

impl AppUiInterpretAssetDraft {
    /// Build a draft from the current asset record state.
    pub fn new(
        asset_id: AssetId,
        asset_name: impl Into<String>,
        interpretation: AssetMediaInterpretation,
        auto_interpretation: Option<DetectedColorInterpretation>,
    ) -> Self {
        Self {
            asset_id,
            asset_name: asset_name.into(),
            interpretation,
            auto_interpretation,
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
    status_label: Label,
    status_value_label: Label,
    color_space_label: Label,
    color_space_dropdown: Dropdown,
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
            status_label: row_label("当前解释"),
            status_value_label: Label::new(String::new())
                .popover_foreground()
                .with_font_size(BODY_FONT_SIZE)
                .with_padding(0.0, 0.0)
                .wrapped(),
            color_space_label: row_label("输入色彩空间"),
            color_space_dropdown: color_space_dropdown_for(
                AssetMediaInterpretation::default(),
                None,
            ),
            apply_button: Button::new("应用")
                .on_click(app_shell_confirm_interpret_asset_dialog_action()),
            cancel_button: Button::new("取消").on_click(app_shell_close_modal_action()),
            draft,
        };
        dialog.refresh_controls();
        dialog
    }

    /// Current shell-local draft.
    pub fn draft(&self) -> &AppUiInterpretAssetDraft {
        &self.draft
    }

    /// Mutate the shell-local draft.
    pub fn apply_update(&mut self, update: InterpretAssetDraftUpdatePayload) {
        self.draft.apply_update(update);
        self.refresh_controls();
    }

    fn refresh_controls(&mut self) {
        self.asset_label.set_text(self.draft.asset_name.clone());
        self.status_value_label.set_text(interpretation_status(&self.draft));
        self.color_space_dropdown = color_space_dropdown_for(
            self.draft.interpretation,
            self.draft.auto_interpretation.as_ref(),
        );
    }
}

fn row_label(text: impl Into<String>) -> Label {
    Label::new(text)
        .secondary()
        .with_font_size(BODY_FONT_SIZE)
        .with_padding(0.0, 0.0)
}

fn color_space_dropdown_for(
    interpretation: AssetMediaInterpretation,
    auto_interpretation: Option<&DetectedColorInterpretation>,
) -> Dropdown {
    let selected = selected_override_color_space(
        interpretation,
        auto_detected_color_space(auto_interpretation),
    );
    let mut items = vec![MenuItem::new(
        auto_option_label(auto_interpretation),
        draft_update_action(interpretation, MediaColorInterpretation::Auto),
    )
    .checked(matches!(
        interpretation.color,
        MediaColorInterpretation::Auto
    ))];
    items.extend(OVERRIDE_COLOR_SPACES.iter().map(|&color_space| {
        MenuItem::new(
            color_space_label(color_space),
            draft_update_action(
                interpretation,
                MediaColorInterpretation::Override { color_space },
            ),
        )
        .checked(
            matches!(
                interpretation.color,
                MediaColorInterpretation::Override { .. }
            ) && color_space == selected,
        )
    }));

    let label = match interpretation.color {
        MediaColorInterpretation::Auto => auto_option_label(auto_interpretation),
        MediaColorInterpretation::Override { color_space } => {
            color_space_label(color_space).to_owned()
        }
    };
    Dropdown::new(label, items).with_max_visible_items(8)
}

fn draft_update_action(
    interpretation: AssetMediaInterpretation,
    color: MediaColorInterpretation,
) -> mondrian_editor_state::Action {
    app_shell_interpret_asset_draft_changed_action(InterpretAssetDraftUpdatePayload {
        interpretation: AssetMediaInterpretation { color, ..interpretation },
    })
}

fn selected_override_color_space(
    interpretation: AssetMediaInterpretation,
    detected_color_space: Option<ColorSpace>,
) -> ColorSpace {
    interpretation
        .color
        .override_color_space()
        .or(detected_color_space)
        .unwrap_or(ColorSpace::Rec709)
}

fn auto_detected_color_space(
    auto_interpretation: Option<&DetectedColorInterpretation>,
) -> Option<ColorSpace> {
    auto_interpretation.and_then(|interpretation| interpretation.color_space)
}

fn auto_option_label(auto_interpretation: Option<&DetectedColorInterpretation>) -> String {
    let Some(interpretation) = auto_interpretation else {
        return "自动 — 未明确标记".to_owned();
    };
    let base = match interpretation.color_space {
        Some(color_space) => format!("自动 — 已识别为 {}", color_space_label(color_space)),
        None => "自动 — 未明确标记".to_owned(),
    };
    let mut details = vec![
        confidence_label(interpretation.confidence).to_owned(),
        method_label(interpretation.method).to_owned(),
    ];
    if !interpretation.warnings.is_empty() {
        details.push(format!("{} 个警告", interpretation.warnings.len()));
    }
    format!("{base}（{}）", details.join("，"))
}

fn confidence_label(confidence: VideoColorInterpretationConfidence) -> &'static str {
    match confidence {
        VideoColorInterpretationConfidence::None => "无置信度",
        VideoColorInterpretationConfidence::Low => "低置信度",
        VideoColorInterpretationConfidence::Medium => "中置信度",
        VideoColorInterpretationConfidence::High => "高置信度",
    }
}

fn method_label(method: VideoColorDetectionMethod) -> &'static str {
    match method {
        VideoColorDetectionMethod::MetadataHint => "元数据提示",
        VideoColorDetectionMethod::CicpTags => "CICP",
        VideoColorDetectionMethod::MissingMetadata => "无元数据",
        VideoColorDetectionMethod::DecoderUnavailable => "解码器不可用",
    }
}

fn interpretation_status(draft: &AppUiInterpretAssetDraft) -> String {
    match draft.interpretation.color {
        MediaColorInterpretation::Auto => auto_option_label(draft.auto_interpretation.as_ref()),
        MediaColorInterpretation::Override { color_space } => {
            format!("手动 — {}", color_space_label(color_space))
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
            content.y + 48.0,
            content.width,
            BODY_FONT_SIZE * 1.6,
        ));

        let control_x = content.x + LABEL_COLUMN_WIDTH;
        let control_width = (content.width - LABEL_COLUMN_WIDTH).max(160.0);
        let mut row_y = content.y + 94.0;
        self.status_label
            .layout(Rect::new(content.x, row_y, LABEL_COLUMN_WIDTH, ROW_HEIGHT));
        self.status_value_label
            .layout(Rect::new(control_x, row_y, control_width, ROW_HEIGHT));

        row_y += ROW_HEIGHT + ROW_GAP;
        self.color_space_label
            .layout(Rect::new(content.x, row_y, LABEL_COLUMN_WIDTH, ROW_HEIGHT));
        self.color_space_dropdown.layout(Rect::new(
            control_x,
            row_y,
            control_width.min(360.0),
            ROW_HEIGHT,
        ));

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
                if self.color_space_dropdown.event(event, ctx) == EventResult::Handled {
                    return EventResult::Handled;
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
        self.status_label.paint(ctx);
        self.status_value_label.paint(ctx);
        self.color_space_label.paint(ctx);
        self.apply_button.paint(ctx);
        self.cancel_button.paint(ctx);
        self.color_space_dropdown.paint(ctx);
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
            1 => Some(&self.asset_label),
            2 => Some(&self.status_label),
            3 => Some(&self.status_value_label),
            4 => Some(&self.color_space_label),
            5 => Some(&self.color_space_dropdown),
            6 => Some(&self.apply_button),
            7 => Some(&self.cancel_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title_label),
            1 => Some(&mut self.asset_label),
            2 => Some(&mut self.status_label),
            3 => Some(&mut self.status_value_label),
            4 => Some(&mut self.color_space_label),
            5 => Some(&mut self.color_space_dropdown),
            6 => Some(&mut self.apply_button),
            7 => Some(&mut self.cancel_button),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::timeline_data::AssetColorPayload;
    use mondrian_media::{VideoColorInterpretationWarning, VideoColorSpaceSource};

    #[test]
    fn draft_converts_to_asset_payload() {
        let asset_id = AssetId::new();
        let draft = AppUiInterpretAssetDraft::new(
            asset_id,
            "Shot",
            AssetMediaInterpretation {
                color: MediaColorInterpretation::Override { color_space: ColorSpace::SLog3 },
                ..AssetMediaInterpretation::default()
            },
            Some(detected_interpretation(ColorSpace::Rec2020)),
        );

        let payload = draft.into_payload();

        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(
            payload.interpretation.color.override_color_space(),
            Some(ColorSpace::SLog3)
        );
    }

    #[test]
    fn status_reports_auto_detection_without_explainer_copy() {
        let draft = AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Shot",
            AssetMediaInterpretation::default(),
            Some(detected_interpretation(ColorSpace::Rec2020)),
        );

        let status = interpretation_status(&draft);

        assert!(status.contains("自动"));
        assert!(status.contains("已识别为"));
        assert!(status.contains("Rec. 2020"));
        assert!(status.contains("高置信度"));
        assert!(status.contains("CICP"));
        assert!(!status.contains("预览/导出"));
    }

    #[test]
    fn auto_mode_uses_enabled_color_space_dropdown() {
        let dialog = InterpretAssetDialog::new(AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Shot",
            AssetMediaInterpretation::default(),
            Some(detected_interpretation(ColorSpace::Rec2020)),
        ));

        assert!(dialog.color_space_dropdown.is_enabled());
        assert_eq!(
            dialog.color_space_dropdown.checked_for_action(&draft_update_action(
                AssetMediaInterpretation::default(),
                MediaColorInterpretation::Auto,
            )),
            Some(true)
        );
    }

    #[test]
    fn override_is_a_color_space_dropdown_option() {
        let dialog = InterpretAssetDialog::new(AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Shot",
            AssetMediaInterpretation {
                color: MediaColorInterpretation::Override { color_space: ColorSpace::Rec2100Pq },
                ..AssetMediaInterpretation::default()
            },
            Some(detected_interpretation(ColorSpace::Rec2020)),
        ));

        assert!(dialog.color_space_dropdown.is_enabled());
        assert_eq!(
            dialog.color_space_dropdown.checked_for_action(&draft_update_action(
                AssetMediaInterpretation {
                    color: MediaColorInterpretation::Override {
                        color_space: ColorSpace::Rec2100Pq
                    },
                    ..AssetMediaInterpretation::default()
                },
                MediaColorInterpretation::Override { color_space: ColorSpace::Rec2100Pq },
            )),
            Some(true)
        );
    }

    #[test]
    fn color_space_dropdown_preserves_payload_classification() {
        let interpretation = AssetMediaInterpretation {
            payload: AssetColorPayload::NonColorData,
            ..AssetMediaInterpretation::default()
        };
        let dialog = InterpretAssetDialog::new(AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Matte",
            interpretation,
            Some(detected_interpretation(ColorSpace::Rec2020)),
        ));

        assert_eq!(
            dialog.color_space_dropdown.checked_for_action(&draft_update_action(
                interpretation,
                MediaColorInterpretation::Auto,
            )),
            Some(true)
        );
        assert_eq!(
            dialog.color_space_dropdown.checked_for_action(&draft_update_action(
                interpretation,
                MediaColorInterpretation::Override { color_space: ColorSpace::Rec2100Hlg },
            )),
            Some(false)
        );
        assert_eq!(interpretation.payload, AssetColorPayload::NonColorData);
    }

    #[test]
    fn auto_status_surfaces_detection_warnings() {
        let mut interpretation = detected_interpretation(ColorSpace::Rec2020);
        interpretation.warnings.push(VideoColorInterpretationWarning::PartialCicpTags {
            detected_color_space: ColorSpace::Rec2020,
        });
        let draft = AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Shot",
            AssetMediaInterpretation::default(),
            Some(interpretation),
        );

        let status = interpretation_status(&draft);

        assert!(status.contains("1 个警告"));
    }

    fn detected_interpretation(color_space: ColorSpace) -> DetectedColorInterpretation {
        DetectedColorInterpretation {
            color_space: Some(color_space),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::CicpTags,
            evidence: Vec::new(),
            warnings: Vec::new(),
            user_overridable: true,
        }
    }
}
