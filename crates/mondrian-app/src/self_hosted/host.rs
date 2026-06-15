//! Self-hosted UI host state.
//!
//! Window entrypoints own native event loops and rendering surfaces. This host
//! owns the reusable application/UI state bridge: root widget, `AppState`, and
//! refresh policy after widget-dispatched actions.

use std::cell::{Cell, Ref, RefCell};

use mondrian_platform::PlatformService;
use mondrian_ui_core::types::Rect;
use mondrian_ui_core::TreeWalker;

use crate::app::AppState;
use crate::self_hosted::action_queue::PendingUiActions;
use crate::self_hosted::shell::SelfHostedAppRoot;

/// Product-facing self-hosted UI session state.
pub struct SelfHostedUiHost {
    root: SelfHostedAppRoot,
    app_state: RefCell<AppState>,
    ui_dirty: Cell<bool>,
}

impl SelfHostedUiHost {
    /// Create a host from an initial application state snapshot.
    pub fn new(app_state: AppState) -> Self {
        let root = SelfHostedAppRoot::from_app_state(&app_state);
        Self {
            root,
            app_state: RefCell::new(app_state),
            ui_dirty: Cell::new(false),
        }
    }

    /// Immutable access to the root widget.
    pub fn root(&self) -> &SelfHostedAppRoot {
        &self.root
    }

    /// Mutable access to the root widget for event routing and layout.
    pub fn root_mut(&mut self) -> &mut SelfHostedAppRoot {
        &mut self.root
    }

    /// Read-only access to the current app state.
    pub fn app_state(&self) -> Ref<'_, AppState> {
        self.app_state.borrow()
    }

    /// Mark the root as needing a model refresh from `AppState`.
    pub fn mark_dirty(&self) {
        self.ui_dirty.set(true);
    }

    /// Refresh the root widget models when editor state changed.
    pub fn refresh_if_dirty(&mut self, bounds: Rect) {
        if !self.ui_dirty.replace(false) {
            return;
        }
        self.root.refresh_from_app_state(&self.app_state.borrow());
        TreeWalker::layout(&mut self.root, bounds);
    }

    /// Drain queued widget actions through shell-local handling and `AppState`.
    pub fn drain_pending_actions(
        &mut self,
        pending_actions: &PendingUiActions,
        bounds: Rect,
        platform: &dyn PlatformService,
    ) {
        let actions = pending_actions.take_all();
        if actions.is_empty() {
            self.refresh_if_dirty(bounds);
            return;
        }

        let mut needs_layout = false;
        for action in actions {
            let current_project_path = self.app_state.borrow().current_project_path.clone();
            let Some(action) =
                self.root.handle_shell_action(action, platform, current_project_path.as_deref())
            else {
                needs_layout = true;
                continue;
            };

            tracing::debug!(?action, "custom UI action");
            if let Err(err) = self.app_state.borrow_mut().dispatch_action(action) {
                tracing::warn!("custom UI action failed: {err}");
            }
            self.mark_dirty();
            needs_layout = true;
        }

        self.refresh_if_dirty(bounds);
        if needs_layout {
            TreeWalker::layout(&mut self.root, bounds);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::AssetId;
    use mondrian_editor_state::Action;
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{DrawCommandEncoder, PaintContext, Widget};
    use mondrian_ui_core::Point;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Default)]
    struct RecordingEncoder {
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, _bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {}

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
        }

        fn draw_text(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_string());
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_string());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    #[test]
    fn host_builds_root_from_initial_app_state() {
        let mut host = SelfHostedUiHost::new(AppState::new());

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert!(!host.root().dock().collect_grab_zones().is_empty());
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn host_drains_actions_and_refreshes_root() {
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::NoOp);
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert!(host.root().dock().ratio() > 0.0);
    }

    #[test]
    fn host_refreshes_root_after_failed_editor_action() {
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::assets_prepare_drag_action(
            crate::app::ui_actions::AssetsPrepareDragPayload { asset_id: AssetId::new() },
        ));
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert!(
            host.app_state().status_hint.as_ref().is_some_and(|(message, is_error)| {
                *is_error && message.contains("素材准备失败")
            }),
            "status hint: {:?}",
            host.app_state().status_hint
        );

        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 1280.0, 720.0),
        };
        host.root().paint(&mut ctx);

        assert!(
            encoder.texts.iter().any(|text| text == "Error"),
            "painted texts: {:?}",
            encoder.texts
        );
        assert!(
            encoder.texts.iter().any(|text| text.contains("素材准备失败")),
            "painted texts: {:?}",
            encoder.texts
        );
    }
}
