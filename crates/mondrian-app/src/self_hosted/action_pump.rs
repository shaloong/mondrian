//! Action pump shared by self-hosted application entrypoints.
//!
//! Widget dispatch callbacks run while the root widget tree is mutably borrowed.
//! Entry loops enqueue actions first, then drain the queue after event routing so
//! shell-local modal state and `AppState` mutations can be applied without
//! re-entrant root borrows.

use std::cell::{Cell, RefCell};

use mondrian_editor_state::Action;
use mondrian_platform::PlatformService;
use mondrian_ui_core::types::Rect;
use mondrian_ui_core::TreeWalker;

use crate::app::AppState;
use crate::self_hosted::panels::SelfHostedPanelModels;
use crate::self_hosted::shell::SelfHostedAppRoot;

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

    fn take_all(&self) -> Vec<Action> {
        std::mem::take(&mut *self.actions.borrow_mut())
    }
}

/// Refresh the root widget models when editor state changed.
pub fn refresh_root_if_dirty(
    root: &mut SelfHostedAppRoot,
    app_state: &RefCell<AppState>,
    ui_dirty: &Cell<bool>,
    bounds: Rect,
) {
    if !ui_dirty.replace(false) {
        return;
    }
    root.set_models(SelfHostedPanelModels::from_app_state(&app_state.borrow()));
    TreeWalker::layout(root, bounds);
}

/// Drain queued widget actions through shell-local handling and `AppState`.
pub fn drain_pending_actions(
    pending_actions: &PendingUiActions,
    root: &mut SelfHostedAppRoot,
    app_state: &RefCell<AppState>,
    ui_dirty: &Cell<bool>,
    bounds: Rect,
    platform: &dyn PlatformService,
) {
    let actions = pending_actions.take_all();
    if actions.is_empty() {
        refresh_root_if_dirty(root, app_state, ui_dirty, bounds);
        return;
    }

    let mut needs_layout = false;
    for action in actions {
        let current_project_path = app_state.borrow().current_project_path.clone();
        let Some(action) =
            root.handle_shell_action(action, platform, current_project_path.as_deref())
        else {
            needs_layout = true;
            continue;
        };

        tracing::debug!(?action, "custom UI action");
        if let Err(err) = app_state.borrow_mut().dispatch_action(action) {
            tracing::warn!("custom UI action failed: {err}");
        } else {
            ui_dirty.set(true);
        }
        needs_layout = true;
    }

    refresh_root_if_dirty(root, app_state, ui_dirty, bounds);
    if needs_layout {
        TreeWalker::layout(root, bounds);
    }
}
