//! Language-neutral product messages shared by status history and notifications.

mod error;

/// One display argument retained so locale changes can reformat product messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMessageValue {
    /// A numeric value used by plural/select rules.
    Number(i64),
    /// A human-readable detail such as a failure reason.
    Text(String),
    /// Another product message resolved using the same UI locale.
    Message(Box<AppMessage>),
}

/// Stable message identity and named arguments, without preformatted UI text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMessage {
    /// Fluent message ID owned by the product catalog.
    pub id: &'static str,
    /// Named values consumed by the message template.
    pub args: std::collections::BTreeMap<&'static str, AppMessageValue>,
}

impl AppMessage {
    /// Construct a message without arguments.
    pub fn new(id: &'static str) -> Self {
        Self { id, args: std::collections::BTreeMap::new() }
    }

    /// Add a named numeric argument.
    pub fn with_number(mut self, name: &'static str, value: i64) -> Self {
        self.args.insert(name, AppMessageValue::Number(value));
        self
    }

    /// Add an exact count, retaining numeric plural rules when representable.
    pub fn with_count(mut self, name: &'static str, value: usize) -> Self {
        let value = match i64::try_from(value) {
            Ok(value) => AppMessageValue::Number(value),
            Err(_) => AppMessageValue::Text(value.to_string()),
        };
        self.args.insert(name, value);
        self
    }

    /// Add a nested product message without freezing its current language.
    pub fn with_message(mut self, name: &'static str, value: AppMessage) -> Self {
        self.args.insert(name, AppMessageValue::Message(Box::new(value)));
        self
    }

    /// Add a named text argument.
    pub fn with_text(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.args.insert(name, AppMessageValue::Text(value.into()));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_identity_ignores_insertion_order_and_replaces_duplicate_names() {
        let first = AppMessage::new("notification-import-partial")
            .with_number("imported", 1)
            .with_number("failed", 2);
        let reordered = AppMessage::new("notification-import-partial")
            .with_number("failed", 2)
            .with_number("imported", 1);
        assert_eq!(first, reordered);
        let replaced = reordered.with_number("failed", 3).with_number("failed", 2);
        assert_eq!(first, replaced);
        assert_eq!(first.args.len(), 2);
        assert_ne!(first, first.clone().with_number("failed", 3));
    }
}
