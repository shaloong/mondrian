//! Bounded local text history shared by both text editing models.
use mondrian_ui_core::widget::{TEXT_INPUT_HISTORY_MAX_BYTES, TEXT_INPUT_HISTORY_MAX_ENTRIES};
use std::marker::PhantomData;
use std::time::Instant;

pub(super) trait TextHistoryState: Clone {
    type Position: Copy + PartialEq + std::fmt::Debug;
    fn history_text(&self) -> &str;
    fn history_cursor(&self) -> Self::Position;
    fn history_anchor(&self) -> Option<Self::Position>;
    fn history_restore(text: &str, cursor: Self::Position, anchor: Option<Self::Position>) -> Self;
}
#[derive(Clone, Debug)]
pub(super) struct TextUndoEntry<P> {
    pub(super) text_before: String,
    pub(super) text_after: String,
    pub(super) cursor_before: P,
    pub(super) cursor_after: P,
    pub(super) anchor_before: Option<P>,
    pub(super) anchor_after: Option<P>,
}
impl<P> TextUndoEntry<P> {
    fn bytes(&self) -> usize {
        self.text_before.capacity().saturating_add(self.text_after.capacity())
    }
}
#[derive(Clone, Debug)]
pub(super) struct TextUndoStack<E: TextHistoryState> {
    pub(super) entries: Vec<TextUndoEntry<E::Position>>,
    pub(super) position: usize,
    last_mutation: Option<Instant>,
    bytes: usize,
    owns_input: bool,
    state: PhantomData<E>,
}
impl<E: TextHistoryState> TextUndoStack<E> {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
            position: 0,
            last_mutation: None,
            bytes: 0,
            owns_input: false,
            state: PhantomData,
        }
    }
    pub(super) fn owns_input(&self) -> bool {
        self.owns_input
    }
    pub(super) fn from_inspection(
        entries: Vec<TextUndoEntry<E::Position>>,
        position: usize,
        owns_input: bool,
    ) -> Option<Self> {
        let bytes = entries
            .iter()
            .try_fold(0usize, |bytes, entry| bytes.checked_add(entry.bytes()))?;
        if position > entries.len()
            || entries.len() > TEXT_INPUT_HISTORY_MAX_ENTRIES
            || bytes > TEXT_INPUT_HISTORY_MAX_BYTES
            || (!owns_input && !entries.is_empty())
        {
            return None;
        }
        Some(Self {
            entries,
            position,
            last_mutation: None,
            bytes,
            owns_input,
            state: PhantomData,
        })
    }
    pub(super) fn push(&mut self, entry: TextUndoEntry<E::Position>, coalesce: bool) {
        self.owns_input = true;
        if self.position < self.entries.len() {
            for abandoned in self.entries.drain(self.position..) {
                self.bytes = self.bytes.saturating_sub(abandoned.bytes());
            }
            self.last_mutation = None;
        }
        if entry.bytes() > TEXT_INPUT_HISTORY_MAX_BYTES {
            self.entries.clear();
            self.position = 0;
            self.bytes = 0;
            self.last_mutation = None;
            return;
        }
        let now = Instant::now();
        let can_coalesce = coalesce
            && self
                .last_mutation
                .is_some_and(|time| now.duration_since(time).as_millis() < 1000)
            && self.entries.last().is_some_and(|previous| {
                previous.text_after == entry.text_before
                    && previous.cursor_after == entry.cursor_before
                    && previous.anchor_after == entry.anchor_before
            });
        if can_coalesce {
            let last = &mut self.entries[self.position - 1];
            self.bytes = self.bytes.saturating_sub(last.bytes());
            last.text_after = entry.text_after;
            last.cursor_after = entry.cursor_after;
            last.anchor_after = entry.anchor_after;
            self.bytes = self.bytes.saturating_add(last.bytes());
        } else {
            self.bytes = self.bytes.saturating_add(entry.bytes());
            self.entries.push(entry);
            self.position = self.entries.len();
        }
        while self.entries.len() > TEXT_INPUT_HISTORY_MAX_ENTRIES
            || self.bytes > TEXT_INPUT_HISTORY_MAX_BYTES
        {
            let removed = self.entries.remove(0);
            self.bytes = self.bytes.saturating_sub(removed.bytes());
            self.position = self.position.saturating_sub(1);
        }
        self.last_mutation = (coalesce && !self.entries.is_empty()).then_some(now);
    }
    pub(super) fn undo(&mut self, current: &E) -> Option<E> {
        if self.position == 0 {
            return None;
        }
        let position = self.position - 1;
        let endpoint_bytes = self
            .bytes
            .saturating_sub(self.entries[position].text_after.capacity())
            .saturating_add(current.history_text().len());
        if endpoint_bytes > TEXT_INPUT_HISTORY_MAX_BYTES {
            self.entries.clear();
            self.position = 0;
            self.bytes = 0;
            self.last_mutation = None;
            return None;
        }
        self.position = position;
        self.last_mutation = None;
        let entry = &mut self.entries[self.position];
        self.bytes = self.bytes.saturating_sub(entry.bytes());
        entry.text_after = current.history_text().to_owned();
        entry.cursor_after = current.history_cursor();
        entry.anchor_after = current.history_anchor();
        self.bytes = self.bytes.saturating_add(entry.bytes());
        Some(E::history_restore(
            &entry.text_before,
            entry.cursor_before,
            entry.anchor_before,
        ))
    }
    pub(super) fn redo(&mut self) -> Option<E> {
        let entry = self.entries.get(self.position)?;
        self.position += 1;
        self.last_mutation = None;
        Some(E::history_restore(
            &entry.text_after,
            entry.cursor_after,
            entry.anchor_after,
        ))
    }
}
