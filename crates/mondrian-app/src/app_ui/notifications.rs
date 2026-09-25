//! Nonblocking, bounded toast projection of terminal product notifications.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use fluent_bundle::FluentArgs;
use mondrian_ui_core::types::{MouseButton, Point, Rect, UiEvent};
use mondrian_ui_core::widget::PaintContext;
use mondrian_ui_core::EventResult;

use crate::app::notifications::{
    AppNotification, AppNotificationFeed, AppNotificationId, AppNotificationSeverity,
    AppNotificationValue,
};
use crate::app_ui::localization::{AppUiLocale, Localizer};
use crate::app_ui::shell::elide_text_to_width;

const MAX_VISIBLE_TOASTS: usize = 2;
const TOAST_WIDTH: f32 = 392.0;
const TOAST_HEIGHT: f32 = 56.0;
const TOAST_GAP: f32 = 8.0;

struct VisibleToast {
    source: AppNotification,
    text: String,
    expires_at: Instant,
}

/// One UI-owned presentation snapshot; it never owns a Project operation.
pub(super) struct NotificationOverlay {
    seen: HashSet<AppNotificationId>,
    visible: Vec<VisibleToast>,
    locale: AppUiLocale,
    dismissed_press_rect: Option<Rect>,
}

impl NotificationOverlay {
    /// Start observing at the current feed tail without replaying old toasts.
    pub(super) fn from_existing(feed: &AppNotificationFeed, locale: AppUiLocale) -> Self {
        Self {
            seen: feed.iter().map(|entry| entry.id).collect(),
            visible: Vec::new(),
            locale,
            dismissed_press_rect: None,
        }
    }

    /// Project new terminal facts; formatting is redone for visible toasts on locale change.
    pub(super) fn sync(
        &mut self,
        feed: &AppNotificationFeed,
        locale: AppUiLocale,
        now: Instant,
    ) -> bool {
        let mut changed = self.expire(now);
        if self.locale != locale {
            self.locale = locale;
            for toast in &mut self.visible {
                toast.text = localized_message(&toast.source, locale);
            }
            changed = true;
        }
        let retained = feed.iter().map(|entry| entry.id).collect::<HashSet<_>>();
        for notification in feed.iter() {
            if self.seen.contains(&notification.id) {
                continue;
            }
            let duration = match notification.severity {
                AppNotificationSeverity::Success => Duration::from_secs(5),
                AppNotificationSeverity::Warning => Duration::from_secs(8),
                AppNotificationSeverity::Error => Duration::from_secs(10),
            };
            self.visible.retain(|toast| {
                toast.source.operation_key != notification.operation_key
                    || toast.source.severity != notification.severity
            });
            self.visible.push(VisibleToast {
                source: notification.clone(),
                text: localized_message(notification, locale),
                expires_at: now + duration,
            });
            if self.visible.len() > MAX_VISIBLE_TOASTS {
                self.visible.remove(0);
            }
            changed = true;
        }
        self.seen = retained;
        changed
    }

    /// Expire toasts at their monotonic deadline.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let before = self.visible.len();
        self.visible.retain(|toast| toast.expires_at > now);
        before != self.visible.len()
    }

    /// Earliest time when a repaint must remove a toast.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.visible.iter().map(|toast| toast.expires_at).min()
    }

    /// Dismiss the clicked toast and prevent the underlying edit surface receiving that click.
    pub(super) fn event(&mut self, event: &UiEvent, bar: Rect) -> EventResult {
        if matches!(event, UiEvent::MouseUp { .. }) && self.dismissed_press_rect.take().is_some() {
            return EventResult::Handled;
        }
        if matches!(event, UiEvent::MouseDown { .. }) {
            self.dismissed_press_rect = None;
        }
        let position = match event {
            UiEvent::MouseDown { position, .. }
            | UiEvent::MouseUp { position, .. }
            | UiEvent::MouseMove { position, .. }
            | UiEvent::MouseWheel { position, .. } => position,
            _ => return EventResult::Ignored,
        };
        if !self.hit_test(*position, bar) {
            return EventResult::Ignored;
        }
        let UiEvent::MouseDown { button: MouseButton::Left, .. } = event else {
            return EventResult::Handled;
        };
        let Some(index) = (0..self.visible.len()).find(|index| {
            hittable_toast_rect(bar, *index).is_some_and(|rect| rect.contains(*position))
        }) else {
            return EventResult::Ignored;
        };
        self.dismissed_press_rect = Some(toast_rect(bar, index));
        self.visible.remove(self.visible.len() - 1 - index);
        EventResult::Handled
    }

    pub(super) fn hit_test(&self, point: Point, bar: Rect) -> bool {
        self.dismissed_press_rect.is_some_and(|rect| rect.contains(point))
            || (0..self.visible.len()).any(|index| {
                hittable_toast_rect(bar, index).is_some_and(|rect| rect.contains(point))
            })
    }

    /// Paint newest toast closest to the status bar.
    pub(super) fn paint(&self, ctx: &mut PaintContext, bar: Rect) {
        let font_size = ctx.theme.typography.metadata.font_size;
        for (index, toast) in self.visible.iter().rev().enumerate() {
            let rect = toast_rect(bar, index);
            if rect.width < 80.0 || rect.y < 0.0 {
                continue;
            }
            let colors = &ctx.theme.colors;
            ctx.encoder.draw_rect(rect, colors.popover, 6.0);
            let accent = match toast.source.severity {
                AppNotificationSeverity::Success => colors.success,
                AppNotificationSeverity::Warning => colors.warning,
                AppNotificationSeverity::Error => colors.destructive_foreground,
            };
            ctx.encoder.draw_rect(Rect::new(rect.x, rect.y, 3.0, rect.height), accent, 1.5);
            let text = elide_text_to_width(&toast.text, font_size, rect.width - 30.0);
            ctx.encoder.draw_text(
                &text,
                font_size,
                Point::new(rect.x + 14.0, rect.y + (rect.height - font_size) * 0.5),
                colors.popover_foreground,
            );
        }
    }
}

fn toast_rect(bar: Rect, index_from_latest: usize) -> Rect {
    let width = (bar.width - 32.0).clamp(0.0, TOAST_WIDTH);
    Rect::new(
        bar.x + bar.width - width - 16.0,
        bar.y - 12.0 - TOAST_HEIGHT - index_from_latest as f32 * (TOAST_HEIGHT + TOAST_GAP),
        width,
        TOAST_HEIGHT,
    )
}

fn hittable_toast_rect(bar: Rect, index_from_latest: usize) -> Option<Rect> {
    let rect = toast_rect(bar, index_from_latest);
    (rect.width >= 80.0 && rect.y >= 0.0).then_some(rect)
}

fn localized_message(notification: &AppNotification, locale: AppUiLocale) -> String {
    let Ok(localizer) = Localizer::new(locale) else {
        return notification.message.id.to_owned();
    };
    let mut args = FluentArgs::new();
    for (name, value) in &notification.message.args {
        match value {
            AppNotificationValue::Number(value) => args.set(*name, *value),
            AppNotificationValue::Text(value) => args.set(*name, value.clone()),
        }
    }
    localizer.format(notification.message.id, Some(&args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::notifications::{AppNotificationMessage, AppNotificationSeverity};
    use mondrian_ui_core::types::Modifiers;

    #[test]
    fn overlay_seeds_existing_facts_then_bounds_and_expires_new_toasts() {
        let mut feed = AppNotificationFeed::default();
        feed.publish(
            "import:old",
            AppNotificationSeverity::Success,
            AppNotificationMessage::new("notification-import-complete").with_number("count", 1),
        );
        let now = Instant::now();
        let mut overlay = NotificationOverlay::from_existing(&feed, AppUiLocale::EnUs);
        assert!(!overlay.sync(&feed, AppUiLocale::EnUs, now));
        for index in 0..3 {
            feed.publish(
                format!("import:{index}"),
                AppNotificationSeverity::Success,
                AppNotificationMessage::new("notification-import-complete")
                    .with_number("count", index),
            );
        }
        assert!(overlay.sync(&feed, AppUiLocale::EnUs, now));
        assert_eq!(overlay.visible.len(), 2);
        assert!(overlay.visible.iter().all(|toast| toast.text.contains("Imported")));
        assert!(overlay.expire(now + Duration::from_secs(5)));
        assert!(overlay.visible.is_empty());
    }

    #[test]
    fn locale_switch_reformats_visible_toast_and_dismissal_consumes_release() {
        let mut feed = AppNotificationFeed::default();
        let now = Instant::now();
        let mut overlay = NotificationOverlay::from_existing(&feed, AppUiLocale::ZhCn);
        feed.publish(
            "save:1",
            AppNotificationSeverity::Success,
            AppNotificationMessage::new("notification-save-complete"),
        );
        assert!(overlay.sync(&feed, AppUiLocale::ZhCn, now));
        assert_eq!(overlay.visible.len(), 1);
        assert_eq!(overlay.visible[0].text, "项目已耐久保存");
        assert!(overlay.sync(&feed, AppUiLocale::EnUs, now));
        assert_eq!(overlay.visible[0].text, "Project saved");

        let bar = Rect::new(0.0, 700.0, 1000.0, 24.0);
        let position = toast_rect(bar, 0).center();
        assert_eq!(
            overlay.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                bar,
            ),
            EventResult::Handled
        );
        assert!(overlay.hit_test(position, bar));
        assert!(overlay.visible.is_empty());
        assert_eq!(
            overlay.event(
                &UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                bar,
            ),
            EventResult::Handled
        );
        assert!(!overlay.hit_test(position, bar));
    }
}
