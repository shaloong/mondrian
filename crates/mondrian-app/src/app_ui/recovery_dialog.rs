//! Startup recovery inspection and confirmation dialog.
//!
//! This Module only projects immutable Recovery Module evidence. It never
//! reads Project archives or decides admission; confirmation emits the exact
//! candidate, which the Recovery Module revalidates before acquiring a lease.

use chrono::{Local, TimeZone};
use fluent_bundle::FluentArgs;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Label};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_recovery_dialog_action,
};
use crate::app::{CrashRecoveryCandidate, RecoveryCanonicalTargetEvidence};
use crate::app_ui::localization::{AppUiLocale, Localizer};

const CARD_MIN_WIDTH: f32 = 520.0;
const CARD_WIDTH: f32 = 700.0;
const CARD_MIN_HEIGHT: f32 = 360.0;
const CARD_HEIGHT: f32 = 440.0;
const CONTENT_PADDING: f32 = 24.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const DETAIL_FONT_SIZE: f32 = 12.0;
const BUTTON_WIDTH: f32 = 156.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_GAP: f32 = 10.0;
const BUTTON_BOTTOM_INSET: f32 = 22.0;

/// Immutable recovery candidate and the time at which the user inspected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryConfirmationModel {
    /// Candidate passed unchanged to deep recovery admission after confirmation.
    pub candidate: CrashRecoveryCandidate,
    observed_at_unix_ms: u64,
}

impl RecoveryConfirmationModel {
    /// Build a presentation snapshot without performing filesystem I/O.
    pub fn from_candidate(candidate: CrashRecoveryCandidate) -> Self {
        let now_ms = unix_now_ms().unwrap_or(candidate.saved_at_unix_ms);
        Self::from_candidate_at(candidate, now_ms)
    }

    fn from_candidate_at(candidate: CrashRecoveryCandidate, now_ms: u64) -> Self {
        Self { candidate, observed_at_unix_ms: now_ms }
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
    pub fn new(model: RecoveryConfirmationModel, locale: AppUiLocale) -> Self {
        let localizer = Localizer::new(locale).expect("bundled UI catalogs must be valid");
        let candidate = &model.candidate;
        let mut summary_args = FluentArgs::new();
        summary_args.set("generation", candidate.author_generation.to_string());
        summary_args.set("revision", candidate.document_revision.to_string());
        summary_args.set("count", candidate.total_snapshots.to_string());
        let summary = localizer.format("recovery-summary", Some(&summary_args));
        let mut time_args = FluentArgs::new();
        time_args.set(
            "exact",
            recovery_exact_time_label(candidate.saved_at_unix_ms),
        );
        time_args.set(
            "relative",
            recovery_age_label_at(
                &localizer,
                candidate.saved_at_unix_ms,
                model.observed_at_unix_ms,
            ),
        );
        let saved_at = localizer.format("recovery-time", Some(&time_args));
        let source = candidate.autosave_file.display().to_string();
        let target = candidate.project_file.display().to_string();
        let target_state = recovery_target_state_label(&localizer, candidate);
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
            title_label: Label::new(localizer.text("recovery-title"))
                .popover_foreground()
                .with_font_size(TITLE_FONT_SIZE)
                .with_padding(0.0, 0.0),
            summary_label: detail_label(summary),
            time_label: detail_label(format_text(
                &localizer,
                "recovery-saved-at",
                "time",
                saved_at,
            )),
            source_label: detail_label(format_text(&localizer, "recovery-source", "path", source)),
            target_label: detail_label(format_text(&localizer, "recovery-target", "path", target)),
            target_state_label: detail_label(format_text(
                &localizer,
                "recovery-target-state",
                "state",
                target_state,
            )),
            safety_label: detail_label(localizer.text("recovery-safety")),
            recover_button: Button::new(localizer.text("recovery-confirm"))
                .on_click(app_shell_confirm_recovery_dialog_action()),
            cancel_button: Button::new(localizer.text("recovery-cancel"))
                .on_click(app_shell_close_modal_action()),
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

fn format_text(localizer: &Localizer, id: &str, key: &'static str, value: String) -> String {
    let mut args = FluentArgs::new();
    args.set(key, value);
    localizer.format(id, Some(&args))
}

pub(crate) fn unix_now_ms() -> Option<u64> {
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

pub(crate) fn recovery_age_label_at(
    localizer: &Localizer,
    saved_at_unix_ms: u64,
    now_ms: u64,
) -> String {
    let age_secs = now_ms.saturating_sub(saved_at_unix_ms) / 1000;
    let (id, count) = if age_secs < 60 {
        ("recovery-age-seconds", age_secs)
    } else if age_secs < 3600 {
        ("recovery-age-minutes", age_secs / 60)
    } else if age_secs < 86_400 {
        ("recovery-age-hours", age_secs / 3600)
    } else {
        ("recovery-age-days", age_secs / 86_400)
    };
    let mut args = FluentArgs::new();
    args.set("count", i64::try_from(count).unwrap_or(i64::MAX));
    localizer.format(id, Some(&args))
}

fn recovery_target_state_label(
    localizer: &Localizer,
    candidate: &CrashRecoveryCandidate,
) -> String {
    match candidate.canonical_target {
        RecoveryCanonicalTargetEvidence::Missing => localizer.text("recovery-target-missing"),
        RecoveryCanonicalTargetEvidence::Present { document_revision }
            if document_revision == candidate.document_revision =>
        {
            format_text(
                localizer,
                "recovery-target-same",
                "revision",
                document_revision.to_string(),
            )
        }
        RecoveryCanonicalTargetEvidence::Present { document_revision } => {
            let mut args = FluentArgs::new();
            args.set("revision", document_revision.to_string());
            args.set("snapshot", candidate.document_revision.to_string());
            let id = if document_revision < candidate.document_revision {
                "recovery-target-older"
            } else {
                "recovery-target-newer"
            };
            localizer.format(id, Some(&args))
        }
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

    fn readable(text: &str) -> String {
        text.replace(['\u{2068}', '\u{2069}'], "")
    }

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
    fn dialog_projects_the_same_recovery_evidence_in_both_languages() {
        let model = RecoveryConfirmationModel::from_candidate_at(
            candidate(RecoveryCanonicalTargetEvidence::Missing),
            1_700_000_060_000,
        );
        let zh = RecoveryConfirmationDialog::new(model.clone(), AppUiLocale::ZhCn);
        let en = RecoveryConfirmationDialog::new(model.clone(), AppUiLocale::EnUs);
        assert_eq!(zh.candidate(), &model.candidate);
        assert_eq!(en.candidate(), &model.candidate);
        assert!(readable(zh.time_label.text()).contains("1 分钟前"));
        assert!(readable(en.time_label.text()).contains("One minute ago"));
        assert!(zh.source_label.text().contains("edit.autosave.mdp"));
        assert!(en.source_label.text().contains("edit.autosave.mdp"));
        assert!(zh.target_label.text().contains("edit.mdp"));
        assert!(en.target_label.text().contains("edit.mdp"));
        assert!(zh.target_state_label.text().contains("目标文件当前不存在"));
        assert!(en.target_state_label.text().contains("does not exist"));
        assert_eq!(zh.recover_button.on_click, en.recover_button.on_click);
        assert_eq!(zh.cancel_button.on_click, en.cancel_button.on_click);
    }

    #[test]
    fn target_state_distinguishes_same_older_and_newer_disk_revisions() {
        let zh = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let en = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        let same = candidate(RecoveryCanonicalTargetEvidence::Present { document_revision: 5 });
        let older = candidate(RecoveryCanonicalTargetEvidence::Present { document_revision: 4 });
        let newer = candidate(RecoveryCanonicalTargetEvidence::Present { document_revision: 6 });
        assert!(readable(&recovery_target_state_label(&zh, &same)).contains("版本 5 已存在"));
        assert!(readable(&recovery_target_state_label(&zh, &older)).contains("较早文档版本 4"));
        assert!(readable(&recovery_target_state_label(&zh, &newer)).contains("比恢复点版本 5 更新"));
        assert!(readable(&recovery_target_state_label(&en, &newer))
            .contains("newer than recovery revision 5"));
    }

    #[test]
    fn relative_age_uses_locale_plural_rules_and_saturates_future_time() {
        let en = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        assert_eq!(recovery_age_label_at(&en, 0, 1_000), "One second ago");
        assert_eq!(recovery_age_label_at(&en, 0, 60_000), "One minute ago");
        assert_eq!(recovery_age_label_at(&en, 0, 3_600_000), "One hour ago");
        assert_eq!(recovery_age_label_at(&en, 0, 86_400_000), "One day ago");
        assert_eq!(
            readable(&recovery_age_label_at(&en, 0, 120_000)),
            "2 minutes ago"
        );
        assert_eq!(
            readable(&recovery_age_label_at(&en, 10_000, 9_000)),
            "0 seconds ago"
        );
    }
}
