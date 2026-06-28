//! App UI About dialog.

use std::sync::OnceLock;

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Label};

use crate::app::ui_actions::{app_shell_close_modal_action, app_shell_copy_system_info_action};

const CARD_WIDTH: f32 = 380.0;
const CARD_MIN_HEIGHT: f32 = 200.0;
const CARD_HEIGHT: f32 = 280.0;
const CONTENT_PADDING: f32 = 22.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const LABEL_FONT_SIZE: f32 = 12.0;
const TITLE_Y: f32 = 22.0;
const ROW_GAP: f32 = 20.0;
const FIRST_ROW_Y: f32 = 56.0;
const BUTTON_WIDTH: f32 = 68.0;
const BUTTON_HEIGHT: f32 = 30.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;
const BUTTON_GAP: f32 = 8.0;

/// System info that the renderer can set early during init.
pub static SYSTEM_INFO: OnceLock<AboutSystemInfo> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct AboutSystemInfo {
    /// Cargo package version.
    pub pkg_version: String,
    /// Rust MSRV / toolchain.
    pub rust_version: String,
    /// OS family (e.g. "Windows_NT").
    pub os: String,
    /// CPU architecture (e.g. "x86_64").
    pub arch: String,
    /// OS version string from the platform.
    pub os_version: String,
    /// Graphics backend (e.g. "DirectX 12").
    pub wgpu_backend: String,
    /// Primary GPU name.
    pub gpu_name: String,
}

impl AboutSystemInfo {
    pub fn build() -> Self {
        Self {
            pkg_version: env!("CARGO_PKG_VERSION").to_owned(),
            rust_version: env!("CARGO_PKG_RUST_VERSION").to_owned(),
            os: Self::os_display(),
            arch: std::env::consts::ARCH.to_owned(),
            os_version: String::new(),
            wgpu_backend: "wgpu".to_owned(),
            gpu_name: String::new(),
        }
    }

    fn os_display() -> String {
        match std::env::consts::OS {
            "windows" => "Windows".to_owned(),
            "macos" => "macOS".to_owned(),
            "linux" => "Linux".to_owned(),
            other => other.to_owned(),
        }
    }

    fn format(&self) -> String {
        let mut s = format!(
            "Mondrian\n\
             \n\
             版本: {}\n\
             渲染器: {}\n\
             Rust: {}\n\
             OS: {} {}",
            self.pkg_version,
            self.wgpu_backend,
            self.rust_version,
            self.os,
            self.arch,
        );
        if !self.os_version.is_empty() {
            s.push_str(&format!(" {}", self.os_version));
        }
        if !self.gpu_name.is_empty() {
            s.push_str(&format!("\nGPU: {}", self.gpu_name));
        }
        s
    }
}

impl Default for AboutSystemInfo {
    fn default() -> Self {
        Self::build()
    }
}

/// Product information modal for the app UI shell.
pub struct AboutDialog {
    id: WidgetId,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    info_rows: Vec<Label>,
    copy_button: Button,
    close_button: Button,
}

impl AboutDialog {
    /// Build the default Mondrian About dialog.
    pub fn new() -> Self {
        let info = SYSTEM_INFO.get().cloned().unwrap_or_default();
        let version_text = format!("版本 {}", info.pkg_version);
        let renderer_text = format!("渲染器: {}", info.wgpu_backend);
        let rust_text = format!("Rust: {}", info.rust_version);
        let os_text = format!("OS: {} {} {}", info.os, info.arch, info.os_version);
        let gpu_text = if info.gpu_name.is_empty() {
            String::new()
        } else {
            format!("GPU: {}", info.gpu_name)
        };

        let mut info_rows: Vec<Label> = vec![
            Label::new(version_text)
                .muted()
                .with_font_size(LABEL_FONT_SIZE)
                .with_padding(0.0, 0.0),
            Label::new(renderer_text)
                .muted()
                .with_font_size(LABEL_FONT_SIZE)
                .with_padding(0.0, 0.0),
            Label::new(rust_text)
                .muted()
                .with_font_size(LABEL_FONT_SIZE)
                .with_padding(0.0, 0.0),
            Label::new(os_text)
                .muted()
                .with_font_size(LABEL_FONT_SIZE)
                .with_padding(0.0, 0.0),
        ];
        if !gpu_text.is_empty() {
            info_rows.push(
                Label::new(gpu_text)
                    .muted()
                    .with_font_size(LABEL_FONT_SIZE)
                    .with_padding(0.0, 0.0),
            );
        }

        Self {
            id: WidgetId::new(),
            surface: DialogSurface::new(
                Size::new(200.0, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title_label: Label::new("Mondrian")
                .popover_foreground()
                .with_font_size(TITLE_FONT_SIZE)
                .with_padding(0.0, 0.0),
            info_rows,
            copy_button: Button::new("复制").minimal().on_click(app_shell_copy_system_info_action(
                SYSTEM_INFO.get().cloned().unwrap_or_default().format(),
            )),
            close_button: Button::new("关闭").on_click(app_shell_close_modal_action()),
        }
    }
}

impl Default for AboutDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for AboutDialog {
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
            content.y + TITLE_Y,
            content.width,
            TITLE_FONT_SIZE * 1.4,
        ));
        let mut y = content.y + FIRST_ROW_Y;
        for label in &mut self.info_rows {
            label.layout(Rect::new(
                content.x,
                y,
                content.width,
                LABEL_FONT_SIZE * 1.4,
            ));
            y += ROW_GAP;
        }
        let close_x = self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH;
        let button_y = self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT;
        self.copy_button.layout(Rect::new(
            close_x - BUTTON_WIDTH - BUTTON_GAP,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
        self.close_button
            .layout(Rect::new(close_x, button_y, BUTTON_WIDTH, BUTTON_HEIGHT));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        let copy_result = self.copy_button.event(event, ctx);
        if copy_result == EventResult::Handled {
            return EventResult::Handled;
        }
        let close_result = self.close_button.event(event, ctx);
        if close_result == EventResult::Handled {
            return EventResult::Handled;
        }
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_close_modal_action());
                EventResult::Handled
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title_label.paint(ctx);
        for label in &self.info_rows {
            label.paint(ctx);
        }
        self.copy_button.paint(ctx);
        self.close_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        2 + self.info_rows.len()
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        if index < self.info_rows.len() {
            Some(&self.info_rows[index])
        } else if index == self.info_rows.len() {
            Some(&self.copy_button)
        } else if index == self.info_rows.len() + 1 {
            Some(&self.close_button)
        } else {
            None
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        if index < self.info_rows.len() {
            Some(&mut self.info_rows[index])
        } else if index == self.info_rows.len() {
            Some(&mut self.copy_button)
        } else if index == self.info_rows.len() + 1 {
            Some(&mut self.close_button)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use mondrian_editor_state::Action;
    use mondrian_ui_core::widget::EventRequests;

    use crate::app::ui_actions::{APP_SHELL_CLOSE_MODAL, APP_SHELL_NAMESPACE};
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};

    fn assert_close_modal_action(action: &Action) {
        match action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_CLOSE_MODAL);
                assert!(payload.is_null());
            }
            other => panic!("expected close-modal app-shell action, got {other:?}"),
        }
    }

    #[test]
    fn about_dialog_layout_exposes_children_inside_centered_card() {
        let mut dialog = AboutDialog::new();
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        assert!(dialog.child_count() >= 2);
        assert_eq!(dialog.card.width, CARD_WIDTH);
        assert_eq!(dialog.card.height, CARD_HEIGHT);
    }

    #[test]
    fn about_dialog_escape_dispatches_close_modal() {
        let mut dialog = AboutDialog::new();
        let actions = RefCell::new(Vec::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = dialog.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(actions.borrow().len(), 1);
        assert_close_modal_action(&actions.borrow()[0]);
    }

    #[test]
    fn about_dialog_close_button_dispatches_close_modal() {
        let mut dialog = AboutDialog::new();
        dialog.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));
        let close_x = dialog.card.x + dialog.card.width - CONTENT_PADDING - BUTTON_WIDTH * 0.5;
        let button_y =
            dialog.card.y + dialog.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT * 0.5;
        let button_center = Point::new(close_x, button_y);
        let actions = RefCell::new(Vec::new());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            dialog.event(
                &UiEvent::MouseDown {
                    position: button_center,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            dialog.event(
                &UiEvent::MouseUp {
                    position: button_center,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(actions.borrow().len(), 1);
        assert_close_modal_action(&actions.borrow()[0]);
    }

    #[test]
    fn about_system_info_format_contains_key_fields() {
        let info = AboutSystemInfo {
            pkg_version: "0.1.0".into(),
            rust_version: "1.92.0".into(),
            os: "Windows".into(),
            arch: "x86_64".into(),
            os_version: "10.0".into(),
            wgpu_backend: "DirectX 12".into(),
            gpu_name: "NVIDIA GeForce RTX 4090".into(),
        };
        let formatted = info.format();
        assert!(formatted.contains("Mondrian"));
        assert!(formatted.contains("0.1.0"));
        assert!(formatted.contains("DirectX 12"));
        assert!(formatted.contains("1.92.0"));
        assert!(formatted.contains("Windows"));
        assert!(formatted.contains("x86_64"));
        assert!(formatted.contains("RTX 4090"));
    }
}
