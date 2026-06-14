//! Action queue shared by self-hosted application entrypoints.
//!
//! Widget dispatch callbacks run while the root widget tree is mutably borrowed.
//! Entry loops enqueue actions first, then let `SelfHostedUiHost` drain the queue
//! after event routing so shell-local modal state and `AppState` mutations can be
//! applied without re-entrant root borrows.

use std::cell::RefCell;

use mondrian_editor_state::Action;

/// FIFO queue for widget-dispatched UI actions.
#[derive(Default)]
pub struct PendingUiActions {
    actions: RefCell<Vec<Action>>,
}

impl PendingUiActions {
    /// Enqueue one action from a widget dispatch callback.
    pub fn push(&self, action: Action) {
        self.actions.borrow_mut().push(action);
    }

    pub(crate) fn take_all(&self) -> Vec<Action> {
        std::mem::take(&mut *self.actions.borrow_mut())
    }
}
