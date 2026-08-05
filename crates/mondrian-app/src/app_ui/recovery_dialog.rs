//! Startup recovery inspection and confirmation dialog.
//!
//! This Module only projects immutable Recovery Module evidence. It never
//! reads Project archives or decides admission; confirmation emits the exact
//! candidate, which the Recovery Module revalidates before acquiring a lease.

use chrono::{Local, TimeZone};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Label};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_recovery_dialog_action,
};
use crate::app::{CrashRecoveryCandidate, RecoveryCanonicalTargetEvidence};

const CARD_MIN_WIDTH: f32 = 520.0;
const CARD_WIDTH: f32 = 700.0;
const CARD_MIN_HEIGHT: f32 = 360.0;
const CARD_HEIGHT: f32 = 440.0;
const CONTENT_PADDING: f32 = 24.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const DETAIL_FONT_SIZE: f32 = 12.0;
const BUTTON_WIDTH: f32 = 124.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_GAP: f32 = 10.0;
const BUTTON_BOTTOM_INSET: f32 = 22.0;

/// Immutable product projection of one exact recovery candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryConfirmationModel {
    /// Candidate passed unchanged to deep recovery admission after confirmation.
    pub candidate: CrashRecoveryCandidate,
    /// Exact and relative capture time.
    pub saved_at: String,
    /// Autosave archive that will be copied and verified.
    pub source: String,
    /// Canonical publication target retained by the Recovery Manifest.
    pub target: String,
    /// Human-readable projection of typed canonical-target evidence.
    pub target_state: String,
}

impl RecoveryConfirmationModel {
    /// Build a presentation snapshot without performing filesystem I/O.
    pub fn from_candidate(candidate: CrashRecoveryCandidate) -> Self {
        let now_ms = unix_now_ms().unwrap_or(candidate.saved_at_unix_ms);
        Self::from_candidate_at(candidate, now_ms)
    }

    fn from_candidate_at(candidate: CrashRecoveryCandidate, now_ms: u64) -> Self {
        let saved_at = format!(
            "{}（{}）",
            recovery_exact_time_label(candidate.saved_at_unix_ms),
            recovery_age_label_at(candidate.saved_at_unix_ms, now_ms)
        );
        let source = candidate.autosave_file.display().to_string();
        let target = candidate.project_file.display().to_string();
        let target_state = recovery_target_state_label(&candidate);
        Self { candidate, saved_at, source, target, target_state }
    }
}

/// Modal that makes recovery source, time, target, and conflict semantics
/// explicit before the product emits a recovery Action.
pub struct RecoveryConfirmationDialog {
    id: WidgetId,
    model: RecoveryConfirmationModel,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    summary_label: Label,
    time_label: Label,
    source_label: Label,
    target_label: Label,
    target_state_label: Label,
    safety_label: Label,
    recover_button: Button,
    cancel_button: Button,
}

impl RecoveryConfirmationDialog {
    /// Build a dialog for one immutable recovery choice.
    pub fn new(model: RecoveryConfirmationModel) -> Self {
        let summary = format!(
            "恢复点包含作者版本 {}、项目文档版本 {}，当前清单共有 {} 个可验证恢复点。",
            model.candidate.author_generation,
            model.candidate.document_revision,
            model.candidate.total_snapshots
        );
        Self {
            id: WidgetId::new(),
            model: model.clone(),
            surface: DialogSurface::new(
                Size::new(CARD_MIN_WIDTH, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title_label: Label::new("确认恢复项目")
                .popover_foreground()
                .with_font_size(TITLE_FONT_SIZE)
                .with_padding(0.0, 0.0),
            summary_label: detail_label(summary),
            time_label: detail_label(format!("保存时间：{}", model.saved_at)),
            source_label: detail_label(format!("恢复来源：{}", model.source)),
            target_label: detail_label(format!("保存目标：{}", model.target)),
            target_state_label: detail_label(format!("目标状态：{}", model.target_state)),
            safety_label: detail_label(
                "确认后只会验证并打开恢复点为未保存项目，不会立即覆盖目标文件。若目标、清单或恢复文件已变化，操作会安全停止。",
            ),
            recover_button: Button::new("恢复此版本")
                .on_click(app_shell_confirm_recovery_dialog_action()),
            cancel_button: Button::new("取消").on_click(app_shell_close_modal_action()),
        }
    }

    /// Exact candidate represented by this dialog.
    pub fn candidate(&self) -> &CrashRecoveryCandidate {
        &self.model.candidate
    }
}

fn detail_label(text: impl Into<String>) -> Label {
    Label::new(text)
        .muted()
        .with_font_size(DETAIL_FONT_SIZE)
        .with_padding(0.0, 0.0)
        .wrapped()
}

fn unix_now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn recovery_exact_time_label(saved_at_unix_ms: u64) -> String {
    i64::try_from(saved_at_unix_ms)
        .ok()
        .and_then(|millis| Local.timestamp_millis_opt(millis).single())
        .map(|time| time.format("%Y-%m-%d %H:%M:%S %:z").to_string())
        .unwrap_or_else(|| format!("Unix {saved_at_unix_ms} ms"))
}

pub(crate) fn recovery_age_label(saved_at_unix_ms: u64) -> String {
    recovery_age_label_at(saved_at_unix_ms, unix_now_ms().unwrap_or(saved_at_unix_ms))
}

fn recovery_age_label_at(saved_at_unix_ms: u64, now_ms: u64) -> String {
    let age_secs = now_ms.saturating_sub(saved_at_unix_ms) / 1000;
    if age_secs < 60 {
        format!("{age_secs} 秒前")
    } else if age_secs < 3600 {
        format!("{} 分钟前", age_secs / 60)
    } else if age_secs < 86_400 {
        format!("{} 小时前", age_secs / 3600)
    } else {
        format!("{} 天前", age_secs / 86_400)
    }
}

fn recovery_target_state_label(candidate: &CrashRecoveryCandidate) -> String {
    match candidate.canonical_target {
        RecoveryCanonicalTargetEvidence::Missing => {
            "目标文件当前不存在；恢复后的首次保存会在该路径创建项目".to_owned()
        }
        RecoveryCanonicalTargetEvidence::Present { document_revision }
            if document_revision == candidate.document_revision =>
        {
            format!("同一项目的文档版本 {document_revision} 已存在；恢复点包含其后的未保存编辑")
        }
        RecoveryCanonicalTargetEvidence::Present { document_revision } => format!(
            "同一项目的较早文档版本 {document_revision} 已存在；恢复点文档版本为 {}",
            candidate.document_revision
        ),
    }
}

impl Widget for RecoveryConfirmationDialog {
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
        let mut y = content.y + 18.0;
        self.title_label.layout(Rect::new(content.x, y, content.width, 28.0));
        y += 42.0;
        self.summary_label.layout(Rect::new(content.x, y, content.width, 40.0));
        y += 48.0;
        self.time_label.layout(Rect::new(content.x, y, content.width, 24.0));
        y += 30.0;
        self.source_label.layout(Rect::new(content.x, y, content.width, 42.0));
        y += 48.0;
        self.target_label.layout(Rect::new(content.x, y, content.width, 42.0));
        y += 48.0;
        self.target_state_label.layout(Rect::new(content.x, y, content.width, 42.0));
        y += 48.0;
        self.safety_label.layout(Rect::new(content.x, y, content.width, 48.0));

        let total_width = BUTTON_WIDTH * 2.0 + BUTTON_GAP;
        let button_y = self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT;
        let button_x = self.card.x + self.card.width - CONTENT_PADDING - total_width;
        self.recover_button
            .layout(Rect::new(button_x, button_y, BUTTON_WIDTH, BUTTON_HEIGHT));
        self.cancel_button.layout(Rect::new(
            button_x + BUTTON_WIDTH + BUTTON_GAP,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_close_modal_action());
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_confirm_recovery_dialog_action());
                EventResult::Handled
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                EventResult::Handled
            }
            _ => {
                for button in [&mut self.recover_button, &mut self.cancel_button] {
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
        self.summary_label.paint(ctx);
        self.time_label.paint(ctx);
        self.source_label.paint(ctx);
        self.target_label.paint(ctx);
        self.target_state_label.paint(ctx);
        self.safety_label.paint(ctx);
        self.recover_button.paint(ctx);
        self.cancel_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        9
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title_label),
            1 => Some(&self.summary_label),
            2 => Some(&self.time_label),
            3 => Some(&self.source_label),
            4 => Some(&self.target_label),
            5 => Some(&self.target_state_label),
            6 => Some(&self.safety_label),
            7 => Some(&self.recover_button),
            8 => Some(&self.cancel_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title_label),
            1 => Some(&mut self.summary_label),
            2 => Some(&mut self.time_label),
            3 => Some(&mut self.source_label),
            4 => Some(&mut self.target_label),
            5 => Some(&mut self.target_state_label),
            6 => Some(&mut self.safety_label),
            7 => Some(&mut self.recover_button),
            8 => Some(&mut self.cancel_button),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::ProjectId;
    use std::path::PathBuf;

    fn candidate(target: RecoveryCanonicalTargetEvidence) -> CrashRecoveryCandidate {
        CrashRecoveryCandidate {
            project_id: ProjectId::new(),
            runtime_root: PathBuf::from("E:/runtime"),
            project_file: PathBuf::from("E:/projects/edit.mdp"),
            canonical_target: target,
            autosave_file: PathBuf::from("E:/runtime/autosave/edit.autosave.mdp"),
            author_generation: 8,
            asset_library_revision: 3,
            document_revision: 5,
            archive_sha256: "a".repeat(64),
            saved_at_unix_ms: 1_700_000_000_000,
            total_snapshots: 2,
        }
    }

    #[test]
    fn model_exposes_exact_source_target_time_and_missing_target_state() {
        let model = RecoveryConfirmationModel::from_candidate_at(
            candidate(RecoveryCanonicalTargetEvidence::Missing),
            1_700_000_060_000,
        );

        assert!(model.saved_at.contains("1 分钟前"));
        assert!(model.source.contains("edit.autosave.mdp"));
        assert!(model.target.contains("edit.mdp"));
        assert!(model.target_state.contains("目标文件当前不存在"));
    }

    #[test]
    fn model_distinguishes_present_same_and_older_canonical_revisions() {
        let same = RecoveryConfirmationModel::from_candidate_at(
            candidate(RecoveryCanonicalTargetEvidence::Present { document_revision: 5 }),
            1_700_000_060_000,
        );
        let older = RecoveryConfirmationModel::from_candidate_at(
            candidate(RecoveryCanonicalTargetEvidence::Present { document_revision: 4 }),
            1_700_000_060_000,
        );

        assert!(same.target_state.contains("版本 5 已存在"));
        assert!(older.target_state.contains("较早文档版本 4"));
    }
}
