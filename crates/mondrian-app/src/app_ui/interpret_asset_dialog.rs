//! Interpret Footage dialog for asset-library media records.

use mondrian_core::display_labels::color_space_label;
use mondrian_core::timeline_data::{
    AssetMediaInterpretation, MediaColorInterpretation, MediaRangeInterpretation, MediaSignalRange,
};
use mondrian_core::types::{AssetId, ColorSpace, OcioColorSpaceIdentity};
use mondrian_media::{
    DecodedVideoRange, DetectedColorInterpretation, VideoColorDetectionMethod,
    VideoColorInterpretationConfidence, VideoColorInterpretationEvidence,
    VideoColorInterpretationWarning, VideoColorTag,
};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Dropdown, Label, MenuItem};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_interpret_asset_dialog_action,
    app_shell_interpret_asset_draft_changed_action, AppShellInputColorPipelineDiagnostics,
    AppShellVideoSignalDiagnostics, AssetsSetInterpretationPayload,
    InterpretAssetDraftUpdatePayload,
};

const CARD_MIN_WIDTH: f32 = 500.0;
const CARD_WIDTH: f32 = 620.0;
const CARD_MIN_HEIGHT: f32 = 500.0;
const CARD_HEIGHT: f32 = 580.0;
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

const OVERRIDE_COLOR_SPACES: [ColorSpace; 27] = ColorSpace::ALL;

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
    /// Raw signal and effective OCIO identities supplied only when the dialog opens.
    pub video_signal: Option<AppShellVideoSignalDiagnostics>,
    /// Effective project/sequence input pipeline used for processor diagnostics.
    pub input_pipeline: Option<AppShellInputColorPipelineDiagnostics>,
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
            video_signal: None,
            input_pipeline: None,
        }
    }

    /// Attach the raw signal and effective input pipeline captured by the asset action.
    pub fn with_input_diagnostics(
        mut self,
        video_signal: Option<AppShellVideoSignalDiagnostics>,
        input_pipeline: Option<AppShellInputColorPipelineDiagnostics>,
    ) -> Self {
        self.video_signal = video_signal;
        self.input_pipeline = input_pipeline;
        self
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
    range_label: Label,
    range_dropdown: Dropdown,
    diagnostics_label: Label,
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
                None,
            ),
            range_label: row_label("信号范围"),
            range_dropdown: range_dropdown_for(AssetMediaInterpretation::default(), None),
            diagnostics_label: Label::new(String::new())
                .muted()
                .with_font_size(BODY_FONT_SIZE)
                .with_padding(0.0, 0.0)
                .wrapped(),
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
            self.draft.video_signal.as_ref(),
        );
        self.range_dropdown =
            range_dropdown_for(self.draft.interpretation, self.draft.video_signal.as_ref());
        self.diagnostics_label.set_text(input_color_diagnostics_text(&self.draft));
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
    signal: Option<&AppShellVideoSignalDiagnostics>,
) -> Dropdown {
    let selected = selected_override_color_space(
        interpretation,
        auto_executable_color_space(auto_interpretation, signal),
    );
    let mut items = vec![MenuItem::new(
        auto_option_label(auto_interpretation, signal),
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
        MediaColorInterpretation::Auto => auto_option_label(auto_interpretation, signal),
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

fn range_dropdown_for(
    interpretation: AssetMediaInterpretation,
    signal: Option<&AppShellVideoSignalDiagnostics>,
) -> Dropdown {
    let auto_label = range_auto_option_label(signal);
    let items = vec![
        MenuItem::new(
            auto_label.clone(),
            range_draft_update_action(interpretation, MediaRangeInterpretation::Auto),
        )
        .checked(matches!(
            interpretation.range,
            MediaRangeInterpretation::Auto
        )),
        MenuItem::new(
            "Full（全范围）",
            range_draft_update_action(
                interpretation,
                MediaRangeInterpretation::Override { range: MediaSignalRange::Full },
            ),
        )
        .checked(matches!(
            interpretation.range,
            MediaRangeInterpretation::Override { range: MediaSignalRange::Full }
        )),
        MenuItem::new(
            "Limited（视频范围）",
            range_draft_update_action(
                interpretation,
                MediaRangeInterpretation::Override { range: MediaSignalRange::Limited },
            ),
        )
        .checked(matches!(
            interpretation.range,
            MediaRangeInterpretation::Override { range: MediaSignalRange::Limited }
        )),
    ];
    let label = match interpretation.range {
        MediaRangeInterpretation::Auto => auto_label,
        MediaRangeInterpretation::Override { range: MediaSignalRange::Full } => {
            "Full（全范围）".to_owned()
        }
        MediaRangeInterpretation::Override { range: MediaSignalRange::Limited } => {
            "Limited（视频范围）".to_owned()
        }
    };
    Dropdown::new(label, items)
}

fn range_draft_update_action(
    interpretation: AssetMediaInterpretation,
    range: MediaRangeInterpretation,
) -> mondrian_editor_state::Action {
    app_shell_interpret_asset_draft_changed_action(InterpretAssetDraftUpdatePayload {
        interpretation: AssetMediaInterpretation { range, ..interpretation },
    })
}

fn range_auto_option_label(signal: Option<&AppShellVideoSignalDiagnostics>) -> String {
    let detected = signal.map(|signal| range_label_text(signal.range)).unwrap_or("Unknown");
    format!("自动 — {detected}")
}

fn range_label_text(range: DecodedVideoRange) -> &'static str {
    match range {
        DecodedVideoRange::Full => "Full",
        DecodedVideoRange::Limited => "Limited",
        DecodedVideoRange::Unknown => "Unknown",
    }
}

fn selected_override_color_space(
    interpretation: AssetMediaInterpretation,
    executable_color_space: Option<ColorSpace>,
) -> ColorSpace {
    interpretation
        .color
        .override_color_space()
        .or(executable_color_space)
        .unwrap_or(ColorSpace::Rec709)
}

fn auto_executable_color_space(
    auto_interpretation: Option<&DetectedColorInterpretation>,
    signal: Option<&AppShellVideoSignalDiagnostics>,
) -> Option<ColorSpace> {
    let signal = signal?;
    auto_interpretation.and_then(|interpretation| {
        interpretation.executable_color_space_from_probe(
            signal.sampling,
            signal.color_metadata.as_ref(),
            &signal.color_metadata_hints,
        )
    })
}

fn auto_option_label(
    auto_interpretation: Option<&DetectedColorInterpretation>,
    signal: Option<&AppShellVideoSignalDiagnostics>,
) -> String {
    let Some(interpretation) = auto_interpretation else {
        return "自动 — 未明确标记".to_owned();
    };
    let base = match (
        interpretation.candidate_color_space,
        auto_executable_color_space(Some(interpretation), signal),
    ) {
        (Some(color_space), Some(_)) => {
            format!("自动 — 已识别为 {}", color_space_label(color_space))
        }
        (Some(color_space), None) => format!(
            "自动 — 建议 {}（仅诊断，不应用）",
            color_space_label(color_space)
        ),
        (None, _) => "自动 — 未明确标记".to_owned(),
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
        VideoColorDetectionMethod::IccProfile => "ICC 配置文件",
        VideoColorDetectionMethod::CicpTags => "CICP",
        VideoColorDetectionMethod::MissingMetadata => "无元数据",
        VideoColorDetectionMethod::UnsupportedCicpTags => "CICP 不支持",
        VideoColorDetectionMethod::DecoderUnavailable => "解码器不可用",
    }
}

fn interpretation_status(draft: &AppUiInterpretAssetDraft) -> String {
    match draft.interpretation.color {
        MediaColorInterpretation::Auto => auto_option_label(
            draft.auto_interpretation.as_ref(),
            draft.video_signal.as_ref(),
        ),
        MediaColorInterpretation::Override { color_space } => {
            format!("手动 — {}", color_space_label(color_space))
        }
    }
}

fn button_y_for_card(card: Rect) -> f32 {
    card.y + card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT
}

fn input_color_diagnostics_text(draft: &AppUiInterpretAssetDraft) -> String {
    let Some(pipeline) = draft.input_pipeline.as_ref() else {
        return "输入诊断：未提供项目色彩上下文".to_owned();
    };
    let interpretation = draft.auto_interpretation.as_ref();
    let source = match draft.interpretation.color {
        MediaColorInterpretation::Override { color_space } => Some(color_space),
        MediaColorInterpretation::Auto => {
            auto_executable_color_space(interpretation, draft.video_signal.as_ref())
        }
    };
    let decision = match draft.interpretation.color {
        MediaColorInterpretation::Override { .. } => "用户显式覆盖".to_owned(),
        MediaColorInterpretation::Auto => interpretation
            .map(|value| {
                let inference = if value.confidence == VideoColorInterpretationConfidence::High {
                    "确定/声明"
                } else if value.candidate_color_space.is_some() {
                    "仅建议/不执行"
                } else {
                    "未知"
                };
                format!(
                    "自动 · {} · {} · {}",
                    method_label(value.method),
                    confidence_label(value.confidence),
                    inference
                )
            })
            .unwrap_or_else(|| "自动 · 未探测".to_owned()),
    };
    let signal = video_signal_summary(draft);
    let evidence = interpretation
        .map(|value| summarize_entries(&value.evidence, evidence_summary, 3))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "无".to_owned());
    let warnings = interpretation
        .map(|value| summarize_entries(&value.warnings, warning_summary, 3))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "无".to_owned());
    let working = working_color_space_label(pipeline.working_color_space);
    let processor = source.map_or_else(
        || "不可用：输入空间尚未解析，保持 Unknown/按项目缺失元数据策略处理".to_owned(),
        |source| {
            mondrian_core::ocio_identity_processor_cache_id(
                &pipeline.engine,
                OcioColorSpaceIdentity::Color(source),
                OcioColorSpaceIdentity::Working(pipeline.working_color_space),
            )
            .unwrap_or_else(|error| format!("不可用：{error}"))
        },
    );
    let path = source
        .map(|source| color_space_label(source).to_owned())
        .unwrap_or_else(|| "Unknown".to_owned());
    format!(
        "识别：{decision}\n{signal}\n依据：{evidence}\n警告：{warnings}\n路径：{path} → {working}（{}）\nOCIO processor cache-id：{processor}",
        pipeline.engine.name()
    )
}

fn video_signal_summary(draft: &AppUiInterpretAssetDraft) -> String {
    let Some(signal) = draft.video_signal.as_ref() else {
        return "Range/Primaries/Transfer/Matrix：无视频信号".to_owned();
    };
    let detected_range = range_label_text(signal.range);
    let range = match draft.interpretation.range {
        MediaRangeInterpretation::Auto => format!("{detected_range}（自动/探测）"),
        MediaRangeInterpretation::Override { range: MediaSignalRange::Full } => {
            format!("Full（用户覆盖；探测为 {detected_range}）")
        }
        MediaRangeInterpretation::Override { range: MediaSignalRange::Limited } => {
            format!("Limited（用户覆盖；探测为 {detected_range}）")
        }
    };
    signal.color_metadata.as_ref().map_or_else(
        || format!("Range：{range} · Primaries/Transfer/Matrix：未提供"),
        |metadata| {
            format!(
                "Range：{range} · Primaries：{} · Transfer：{} · Matrix：{}",
                video_color_tag_summary(&metadata.primaries),
                video_color_tag_summary(&metadata.transfer),
                video_color_tag_summary(&metadata.matrix)
            )
        },
    )
}

fn video_color_tag_summary(tag: &VideoColorTag) -> String {
    if !tag.specified {
        return format!("unspecified({})", tag.code);
    }
    tag.name
        .as_ref()
        .map(|name| format!("{name}({})", tag.code))
        .unwrap_or_else(|| tag.code.to_string())
}

fn evidence_summary(evidence: &VideoColorInterpretationEvidence) -> String {
    match evidence {
        VideoColorInterpretationEvidence::MetadataHint {
            scope,
            key,
            value,
            detected_color_space,
            authority,
        } => format!(
            "{:?} {:?} metadata {key}={value} → {}",
            scope,
            authority,
            color_space_label(*detected_color_space)
        ),
        VideoColorInterpretationEvidence::ExactCicpTags {
            primaries,
            transfer,
            matrix,
            detected_color_space,
        } => format!(
            "完整 CICP {}/{}/{} → {}",
            video_color_tag_summary(primaries),
            video_color_tag_summary(transfer),
            video_color_tag_summary(matrix),
            color_space_label(*detected_color_space)
        ),
        VideoColorInterpretationEvidence::PartialCicpTags {
            primaries,
            transfer,
            matrix,
            detected_color_space,
        } => format!(
            "部分 CICP {}/{}/{} → {}",
            video_color_tag_summary(primaries),
            video_color_tag_summary(transfer),
            video_color_tag_summary(matrix),
            color_space_label(*detected_color_space)
        ),
        VideoColorInterpretationEvidence::UnsupportedCicpTags { primaries, transfer, matrix } => {
            format!(
                "不支持的 CICP {}/{}/{}",
                video_color_tag_summary(primaries),
                video_color_tag_summary(transfer),
                video_color_tag_summary(matrix)
            )
        }
        VideoColorInterpretationEvidence::DecoderUnavailable => "解码器不可用".to_owned(),
        VideoColorInterpretationEvidence::IccProfile { mapped_color_space, profile_name } => {
            format!(
                "ICC {} → {}",
                profile_name.as_deref().unwrap_or("未命名"),
                mapped_color_space.map(color_space_label).unwrap_or("未映射")
            )
        }
    }
}

fn warning_summary(warning: &VideoColorInterpretationWarning) -> String {
    match warning {
        VideoColorInterpretationWarning::MultipleMetadataHints { .. } => "多个 metadata hint 冲突",
        VideoColorInterpretationWarning::MetadataHintOverridesCicpTags { .. } => {
            "metadata hint 覆盖冲突 CICP"
        }
        VideoColorInterpretationWarning::DescriptiveMetadataHintInference { .. } => {
            "仅依据描述性 metadata 推断"
        }
        VideoColorInterpretationWarning::LowerPriorityMetadataHints { .. } => {
            "已忽略冲突的低优先级 hint"
        }
        VideoColorInterpretationWarning::PartialCicpTags { .. } => "仅有部分 CICP",
        VideoColorInterpretationWarning::MissingCicpTags => "CICP 缺失",
        VideoColorInterpretationWarning::UnsupportedCicpTags => "CICP 不支持或冲突",
        VideoColorInterpretationWarning::DecoderUnavailable => "解码器不可用",
        VideoColorInterpretationWarning::IccProfileUnmapped { .. } => "ICC 无法映射",
        VideoColorInterpretationWarning::IccCicpMismatch { .. } => "ICC 与 CICP 冲突",
    }
    .to_owned()
}

fn summarize_entries<T>(entries: &[T], summarize: fn(&T) -> String, maximum: usize) -> String {
    let mut summaries = entries.iter().take(maximum).map(summarize).collect::<Vec<_>>();
    if entries.len() > maximum {
        summaries.push(format!("另有 {} 项", entries.len() - maximum));
    }
    summaries.join("；")
}

fn working_color_space_label(working: mondrian_core::WorkingColorSpace) -> &'static str {
    match working {
        mondrian_core::WorkingColorSpace::LinearRec709 => "Mondrian Working Linear Rec.709",
        mondrian_core::WorkingColorSpace::LinearRec2020 => "Mondrian Working Linear Rec.2020",
        mondrian_core::WorkingColorSpace::LinearP3D65 => "Mondrian Working Linear P3-D65",
        mondrian_core::WorkingColorSpace::AcesCg => "ACEScg/AP1 Linear",
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

        row_y += ROW_HEIGHT + ROW_GAP;
        self.range_label
            .layout(Rect::new(content.x, row_y, LABEL_COLUMN_WIDTH, ROW_HEIGHT));
        self.range_dropdown.layout(Rect::new(
            control_x,
            row_y,
            control_width.min(360.0),
            ROW_HEIGHT,
        ));

        row_y += ROW_HEIGHT + ROW_GAP;
        self.diagnostics_label.layout(Rect::new(
            content.x,
            row_y,
            content.width,
            (button_y_for_card(self.card) - row_y - ROW_GAP).max(120.0),
        ));

        let button_y = button_y_for_card(self.card);
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
                if self.range_dropdown.event(event, ctx) == EventResult::Handled {
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
        self.range_label.paint(ctx);
        self.diagnostics_label.paint(ctx);
        self.apply_button.paint(ctx);
        self.cancel_button.paint(ctx);
        self.color_space_dropdown.paint(ctx);
        self.range_dropdown.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        11
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title_label),
            1 => Some(&self.asset_label),
            2 => Some(&self.status_label),
            3 => Some(&self.status_value_label),
            4 => Some(&self.color_space_label),
            5 => Some(&self.color_space_dropdown),
            6 => Some(&self.range_label),
            7 => Some(&self.range_dropdown),
            8 => Some(&self.diagnostics_label),
            9 => Some(&self.apply_button),
            10 => Some(&self.cancel_button),
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
            6 => Some(&mut self.range_label),
            7 => Some(&mut self.range_dropdown),
            8 => Some(&mut self.diagnostics_label),
            9 => Some(&mut self.apply_button),
            10 => Some(&mut self.cancel_button),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{APP_SHELL_INTERPRET_ASSET_DRAFT_CHANGED, APP_SHELL_NAMESPACE};
    use mondrian_core::timeline_data::AssetColorPayload;
    use mondrian_editor_state::Action;
    use mondrian_media::{VideoColorInterpretationWarning, VideoColorSpaceSource};

    #[test]
    fn draft_converts_to_asset_payload() {
        let asset_id = AssetId::new();
        let draft = AppUiInterpretAssetDraft::new(
            asset_id,
            "Shot",
            AssetMediaInterpretation {
                color: MediaColorInterpretation::Override {
                    color_space: ColorSpace::SonySLog3SGamut3Cine,
                },
                ..AssetMediaInterpretation::default()
            },
            Some(detected_interpretation(ColorSpace::Rec2020)),
        );

        let payload = draft.into_payload();

        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(
            payload.interpretation.color.override_color_space(),
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
    }

    #[test]
    fn status_reports_auto_detection_without_explainer_copy() {
        let (interpretation, signal) = exact_cicp_fixture(ColorSpace::Rec2020);
        let draft = AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Shot",
            AssetMediaInterpretation::default(),
            Some(interpretation),
        )
        .with_input_diagnostics(Some(signal), None);

        let status = interpretation_status(&draft);

        assert!(status.contains("自动"));
        assert!(status.contains("已识别为"));
        assert!(status.contains("Rec. 2020"));
        assert!(status.contains("高置信度"));
        assert!(status.contains("CICP"));
        assert!(!status.contains("预览/导出"));
    }

    #[test]
    fn diagnostic_candidate_is_visible_without_becoming_auto_execution() {
        let mut interpretation = detected_interpretation(ColorSpace::SonySLog3SGamut3Cine);
        interpretation.confidence = VideoColorInterpretationConfidence::Low;
        interpretation.evidence = vec![VideoColorInterpretationEvidence::MetadataHint {
            scope: mondrian_media::VideoColorMetadataHintScope::FileName,
            key: "filename".to_owned(),
            value: "camera-S-Log3_S-Gamut3.Cine.mov".to_owned(),
            detected_color_space: ColorSpace::SonySLog3SGamut3Cine,
            authority: mondrian_media::VideoColorMetadataHintAuthority::DiagnosticSuggestion,
        }];
        let draft = AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Shot",
            AssetMediaInterpretation::default(),
            Some(interpretation.clone()),
        );

        let status = interpretation_status(&draft);
        assert!(status.contains("建议"));
        assert!(status.contains("仅诊断，不应用"));
        assert_eq!(
            auto_executable_color_space(Some(&interpretation), None),
            None
        );
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
    fn range_dropdown_preserves_color_and_payload_interpretation() {
        let interpretation = AssetMediaInterpretation {
            color: MediaColorInterpretation::Override { color_space: ColorSpace::Rec2100Pq },
            payload: AssetColorPayload::NonColorData,
            ..AssetMediaInterpretation::default()
        };
        let dialog = InterpretAssetDialog::new(AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Tagged incorrectly",
            interpretation,
            Some(detected_interpretation(ColorSpace::Rec709)),
        ));
        let action = range_draft_update_action(
            interpretation,
            MediaRangeInterpretation::Override { range: MediaSignalRange::Full },
        );
        let payload = draft_update_payload_from_action(&action);

        assert_eq!(
            dialog.range_dropdown.checked_for_action(&action),
            Some(false)
        );
        assert_eq!(payload.interpretation.color, interpretation.color);
        assert_eq!(
            payload.interpretation.payload,
            AssetColorPayload::NonColorData
        );
        assert_eq!(
            payload.interpretation.range.override_range(),
            Some(MediaSignalRange::Full)
        );
    }

    #[test]
    fn color_space_dropdown_does_not_offer_data_as_color_interpretation() {
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

        let items = dialog.color_space_dropdown.items();
        assert_eq!(items.len(), OVERRIDE_COLOR_SPACES.len() + 1);

        for item in items {
            let label = item.label.to_ascii_lowercase();
            assert!(!label.contains("data"), "{label}");
            assert!(!label.contains("non-color"), "{label}");
            assert!(!label.contains("非颜色"), "{label}");

            let payload = draft_update_payload_from_action(
                item.action().expect("color-space rows dispatch draft updates"),
            );
            assert_eq!(
                payload.interpretation.payload,
                AssetColorPayload::NonColorData
            );
            assert!(matches!(
                payload.interpretation.color,
                MediaColorInterpretation::Auto | MediaColorInterpretation::Override { .. }
            ));
        }
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

    #[test]
    fn input_diagnostics_preserve_signal_evidence_and_processor_identity() {
        mondrian_core::ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let (interpretation, signal) = exact_cicp_fixture(ColorSpace::Rec709);
        let draft = AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Camera A.mov",
            AssetMediaInterpretation::default(),
            Some(interpretation),
        )
        .with_input_diagnostics(
            Some(signal),
            Some(AppShellInputColorPipelineDiagnostics {
                engine: mondrian_core::ColorEngine::mondrian_standard(),
                working_color_space: mondrian_core::WorkingColorSpace::LinearRec2020,
            }),
        );

        let diagnostics = input_color_diagnostics_text(&draft);
        assert!(diagnostics.contains("Limited"));
        assert!(diagnostics.contains("Primaries：bt709(1)"));
        assert!(diagnostics.contains("完整 CICP"));
        assert!(diagnostics.contains("Rec. 709 → Mondrian Working Linear Rec.2020"));
        assert!(diagnostics.contains("OCIO processor cache-id："));
        assert!(!diagnostics.contains("processor cache-id：不可用"));
    }

    #[test]
    fn input_diagnostics_fail_closed_without_effective_pipeline_identity() {
        let draft = AppUiInterpretAssetDraft::new(
            AssetId::new(),
            "Detached.mov",
            AssetMediaInterpretation::default(),
            Some(detected_interpretation(ColorSpace::Rec709)),
        );

        assert_eq!(
            input_color_diagnostics_text(&draft),
            "输入诊断：未提供项目色彩上下文"
        );
    }

    fn detected_interpretation(color_space: ColorSpace) -> DetectedColorInterpretation {
        DetectedColorInterpretation {
            candidate_color_space: Some(color_space),
            confidence: VideoColorInterpretationConfidence::High,
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
            evidence: vec![VideoColorInterpretationEvidence::MetadataHint {
                scope: mondrian_media::VideoColorMetadataHintScope::Stream,
                key: "source_color_space".to_owned(),
                value: format!("{color_space:?}"),
                detected_color_space: color_space,
                authority: mondrian_media::VideoColorMetadataHintAuthority::SourceDeclaration(
                    mondrian_media::VideoColorMetadataDeclaration::SourceColorSpace,
                ),
            }],
            warnings: Vec::new(),
            user_overridable: true,
        }
    }

    fn exact_cicp_fixture(
        color_space: ColorSpace,
    ) -> (DetectedColorInterpretation, AppShellVideoSignalDiagnostics) {
        let (primaries, transfer, matrix) = match color_space {
            ColorSpace::Rec709 => ((1, "bt709"), (1, "bt709"), (1, "bt709")),
            ColorSpace::Rec2020 => ((9, "bt2020"), (1, "bt709"), (9, "bt2020nc")),
            other => panic!("no exact CICP fixture for {other:?}"),
        };
        let sampling = mondrian_media::ProvenVideoSampling {
            pixel_format: mondrian_core::PixelFormat::Yuv420p,
            bit_depth: 8,
            has_alpha: false,
        };
        let metadata = mondrian_media::VideoColorMetadata {
            primaries: VideoColorTag {
                code: primaries.0,
                name: Some(primaries.1.to_owned()),
                specified: true,
            },
            transfer: VideoColorTag {
                code: transfer.0,
                name: Some(transfer.1.to_owned()),
                specified: true,
            },
            matrix: VideoColorTag {
                code: matrix.0,
                name: Some(matrix.1.to_owned()),
                specified: true,
            },
        };
        let interpretation =
            mondrian_media::interpret_video_color_metadata(&metadata, Some(sampling), &[]);
        assert_eq!(interpretation.candidate_color_space, Some(color_space));
        (
            interpretation,
            AppShellVideoSignalDiagnostics {
                range: DecodedVideoRange::Limited,
                sampling: Some(sampling),
                color_metadata: Some(metadata),
                color_metadata_hints: Vec::new(),
            },
        )
    }

    fn draft_update_payload_from_action(action: &Action) -> InterpretAssetDraftUpdatePayload {
        match action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_INTERPRET_ASSET_DRAFT_CHANGED);
                serde_json::from_value(payload.clone()).expect("draft update payload")
            }
            other => panic!("expected app-shell custom action, got {other:?}"),
        }
    }
}
