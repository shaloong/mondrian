//! Bounded product notification facts emitted only for user-relevant terminal work.

use std::collections::VecDeque;

use uuid::Uuid;

use super::AppMessage;

const MAX_NOTIFICATIONS: usize = 64;

/// Transient notification identity; independent of Project authoring IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppNotificationId(Uuid);

impl AppNotificationId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Product urgency for a notification shown without blocking the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppNotificationSeverity {
    /// Plugin or operation information without a success claim.
    Information,
    /// A long-running operation completed successfully.
    Success,
    /// The operation completed but needs attention.
    Warning,
    /// The requested operation failed.
    Error,
}

/// One post-completion product notification fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppNotification {
    /// Unique presentation fact, never reused by an unrelated operation.
    pub id: AppNotificationId,
    /// Stable operation identity for visible deduplication.
    pub operation_key: String,
    /// Severity used for timeout and visual treatment.
    pub severity: AppNotificationSeverity,
    /// Language-neutral message data.
    pub message: AppMessage,
}

/// In-memory bounded feed; status history remains the persistent in-session record.
#[derive(Debug, Default)]
pub struct AppNotificationFeed {
    entries: VecDeque<AppNotification>,
}

impl AppNotificationFeed {
    /// Publish one terminal fact, deduplicating an identical tail.
    pub fn publish(
        &mut self,
        operation_key: impl Into<String>,
        severity: AppNotificationSeverity,
        message: AppMessage,
    ) {
        let operation_key = operation_key.into();
        if self.entries.back().is_some_and(|last| {
            last.operation_key == operation_key
                && last.severity == severity
                && last.message == message
        }) {
            return;
        }
        self.entries.push_back(AppNotification {
            id: AppNotificationId::new(),
            operation_key,
            severity,
            message,
        });
        if self.entries.len() > MAX_NOTIFICATIONS {
            self.entries.pop_front();
        }
    }

    /// Observe retained notification facts in publication order.
    pub fn iter(&self) -> impl Iterator<Item = &AppNotification> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_identical_terminal_fact_is_deduplicated_but_new_operation_is_not() {
        let mut feed = AppNotificationFeed::default();
        let message = AppMessage::new("notification-import-complete").with_number("count", 2);
        feed.publish(
            "import:1",
            AppNotificationSeverity::Success,
            message.clone(),
        );
        feed.publish(
            "import:1",
            AppNotificationSeverity::Success,
            message.clone(),
        );
        feed.publish("import:2", AppNotificationSeverity::Success, message);
        assert_eq!(feed.iter().count(), 2);
    }

    #[test]
    fn feed_retains_only_latest_bounded_facts() {
        let mut feed = AppNotificationFeed::default();
        for index in 0..(MAX_NOTIFICATIONS + 3) {
            feed.publish(
                format!("save:{index}"),
                AppNotificationSeverity::Success,
                AppMessage::new("notification-save-complete"),
            );
        }
        assert_eq!(feed.iter().count(), MAX_NOTIFICATIONS);
        assert_eq!(
            feed.iter().next().map(|entry| entry.operation_key.as_str()),
            Some("save:3")
        );
    }
}
