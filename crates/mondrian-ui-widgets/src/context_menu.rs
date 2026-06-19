//! 右键菜单控件
//!
//! 在指定位置弹出菜单项列表。点击选项或外部区域关闭。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::cell::Cell;

use crate::menu::{
    anchored_menu_rect, menu_item_text_width, paint_menu_popup_chrome, paint_menu_row,
    paint_menu_scrollbar, paint_menu_separator, MenuItem, MenuRowPaint,
};

/// 右键弹出菜单
///
/// 通常由父容器在检测到右键点击时创建并插入 Widget 树。
pub struct ContextMenu {
    id: WidgetId,
    items: Vec<MenuItem>,
    anchor: Point,
    bounds: Rect,
    item_height: f32,
    min_width: f32,
    visible: bool,
    hovered: Option<usize>,
    local_command: Option<String>,
    overlay_viewport: Cell<Option<Rect>>,
    scroll_offset: Cell<f32>,
}

const CONTEXT_MENU_PADDING_X: f32 = 8.0;
const CONTEXT_MENU_ROW_PADDING_X: f32 = 16.0;
const CONTEXT_MENU_ICON_LANE_WIDTH: f32 = 24.0;

impl ContextMenu {
    pub fn new(anchor: Point, items: Vec<MenuItem>) -> Self {
        Self {
            id: WidgetId::new(),
            items,
            anchor,
            bounds: Rect::ZERO,
            item_height: 26.0,
            min_width: 140.0,
            visible: true,
            hovered: None,
            local_command: None,
            overlay_viewport: Cell::new(None),
            scroll_offset: Cell::new(0.0),
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Take the most recent component-local command activated from this menu.
    pub fn take_local_command(&mut self) -> Option<String> {
        self.local_command.take()
    }

    fn bounds_rect(&self) -> Rect {
        anchored_menu_rect(
            Rect::new(self.anchor.x, self.anchor.y, 0.0, 0.0),
            self.menu_width() + CONTEXT_MENU_PADDING_X,
            8.0 + self.visible_content_height(),
            0.0,
            self.overlay_viewport.get(),
        )
    }

    fn item_rect(&self, idx: usize) -> Rect {
        let bounds = self.bounds_rect();
        Rect::new(
            bounds.x + CONTEXT_MENU_PADDING_X * 0.5,
            bounds.y + 4.0 + idx as f32 * self.item_height - self.scroll_offset.get(),
            self.menu_width(),
            self.item_height,
        )
    }

    fn content_height(&self) -> f32 {
        self.items.len() as f32 * self.item_height
    }

    fn visible_content_height(&self) -> f32 {
        let content_height = self.content_height();
        let Some(viewport) = self.overlay_viewport.get() else {
            return content_height;
        };
        let available = (viewport.height - 8.0).max(self.item_height);
        content_height.min(available)
    }

    fn max_scroll_y(&self) -> f32 {
        (self.content_height() - self.visible_content_height()).max(0.0)
    }

    fn clamp_scroll_offset(&self) {
        self.scroll_offset.set(self.scroll_offset.get().clamp(0.0, self.max_scroll_y()));
    }

    fn menu_width(&self) -> f32 {
        let longest_item = self
            .items
            .iter()
            .filter(|item| !item.is_separator())
            .map(menu_item_text_width)
            .fold(0.0, f32::max);
        self.min_width
            .max(longest_item + CONTEXT_MENU_ROW_PADDING_X * 2.0 + self.icon_lane_width())
    }

    fn icon_lane_width(&self) -> f32 {
        if self.items.iter().any(|item| item.icon.is_some()) {
            CONTEXT_MENU_ICON_LANE_WIDTH
        } else {
            0.0
        }
    }

    fn item_at(&self, position: Point) -> Option<usize> {
        let bounds = self.bounds_rect();
        if !bounds.contains(position) {
            return None;
        }
        let relative_y = position.y - (bounds.y + 4.0) + self.scroll_offset.get();
        if relative_y < 0.0 {
            return None;
        }
        let index = (relative_y / self.item_height).floor() as usize;
        if index < self.items.len() && self.item_rect(index).contains(position) {
            Some(index)
        } else {
            None
        }
    }

    fn first_activatable_index(&self) -> Option<usize> {
        self.items.iter().position(MenuItem::is_activatable)
    }

    fn next_activatable_index(&self, direction: i32) -> Option<usize> {
        let activatable: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.is_activatable().then_some(index))
            .collect();
        if activatable.is_empty() {
            return None;
        }
        let current = self
            .hovered
            .and_then(|index| activatable.iter().position(|candidate| *candidate == index));
        let next = match (current, direction) {
            (Some(index), d) if d < 0 => (index + activatable.len() - 1) % activatable.len(),
            (Some(index), _) => (index + 1) % activatable.len(),
            (None, d) if d < 0 => activatable.len() - 1,
            (None, _) => 0,
        };
        activatable.get(next).copied()
    }

    fn activate_index(&mut self, index: usize, ctx: &mut EventContext) -> bool {
        if !self.items[index].is_activatable() {
            return false;
        }
        if let Some(command) = self.items[index].local_command.clone() {
            self.local_command = Some(command);
        } else {
            (ctx.dispatch)(self.items[index].action.clone());
        }
        self.visible = false;
        true
    }

    fn activate_hovered(&mut self, ctx: &mut EventContext) -> bool {
        let Some(index) = self.hovered.or_else(|| self.first_activatable_index()) else {
            return false;
        };
        self.activate_index(index, ctx)
    }

    fn ensure_hover_visible(&self) {
        let Some(index) = self.hovered else {
            return;
        };
        let row_top = index as f32 * self.item_height;
        let row_bottom = row_top + self.item_height;
        let view_top = self.scroll_offset.get();
        let view_bottom = view_top + self.visible_content_height();
        if row_top < view_top {
            self.scroll_offset.set(row_top);
        } else if row_bottom > view_bottom {
            self.scroll_offset.set(row_bottom - self.visible_content_height());
        }
        self.clamp_scroll_offset();
    }
}

impl Widget for ContextMenu {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _c: LayoutConstraint) -> Size {
        if self.visible {
            let h = 8.0 + self.items.len() as f32 * self.item_height;
            Size::new(self.menu_width() + CONTEXT_MENU_PADDING_X, h)
        } else {
            Size::ZERO
        }
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.visible {
            return EventResult::Ignored;
        }

        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if let Some(index) = self.item_at(*position) {
                    if self.items[index].is_activatable() {
                        self.activate_index(index, ctx);
                    }
                    return EventResult::Handled;
                }
                if !self.bounds_rect().contains(*position) {
                    self.visible = false;
                }
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                self.hovered = self.item_at(*position).filter(|i| self.items[*i].is_activatable());
                EventResult::Handled
            }
            UiEvent::MouseWheel { delta, position, .. } => {
                if self.bounds_rect().contains(*position) && self.max_scroll_y() > 0.0 {
                    self.scroll_offset.set(self.scroll_offset.get() + *delta);
                    self.clamp_scroll_offset();
                }
                EventResult::Handled
            }
            UiEvent::MouseDown { button: MouseButton::Right, .. } => {
                self.visible = false;
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                self.visible = false;
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Down, .. } => {
                self.hovered = self.next_activatable_index(1);
                self.ensure_hover_visible();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Up, .. } => {
                self.hovered = self.next_activatable_index(-1);
                self.ensure_hover_visible();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, .. } => {
                self.activate_hovered(ctx);
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, _ctx: &mut PaintContext) {}

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if !self.visible {
            return;
        }

        self.overlay_viewport.set(Some(ctx.clip_rect));
        self.clamp_scroll_offset();
        let bg = self.bounds_rect();
        paint_menu_popup_chrome(ctx, bg);
        let reserve_icon_lane = self.icon_lane_width() > 0.0;
        ctx.push_clip(bg.inset(1.0, 1.0));

        for (i, item) in self.items.iter().enumerate() {
            let r = self.item_rect(i);
            if !r.intersects(&bg) {
                continue;
            }

            if item.is_separator() {
                paint_menu_separator(ctx, r);
                continue;
            }

            paint_menu_row(
                ctx,
                r,
                &item.label,
                item.shortcut.as_deref(),
                item.icon.as_ref(),
                reserve_icon_lane,
                MenuRowPaint {
                    enabled: item.enabled,
                    active: false,
                    hovered: self.hovered == Some(i),
                },
            );
        }
        ctx.pop_clip();
        paint_menu_scrollbar(
            ctx,
            bg,
            self.visible_content_height(),
            self.content_height(),
            self.scroll_offset.get(),
        );
    }

    fn overlay_hit_test(&self, _point: Point) -> bool {
        self.visible
    }

    fn hit_test(&self, point: Point) -> bool {
        self.visible && self.bounds_rect().contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use crate::vector_icon::VectorIcon;
    use mondrian_editor_state::Action;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingEncoder {
        rect_count: usize,
        rects: Vec<Rect>,
        lines: usize,
        texts: Vec<String>,
        triangles: usize,
        raster_images: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {
            self.rect_count += 1;
            self.rects.push(bounds);
        }

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
            self.lines += 1;
        }

        fn draw_text(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.into());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}

        fn draw_triangles(&mut self, vertices: &[Point], _color: mondrian_core::Color) {
            self.triangles += vertices.len() / 3;
        }

        fn draw_raster_image(
            &mut self,
            _key: &str,
            _bounds: Rect,
            _width: u32,
            _height: u32,
            _rgba: std::sync::Arc<[u8]>,
            _tint: mondrian_core::Color,
        ) {
            self.raster_images += 1;
        }
    }

    fn test_icon() -> VectorIcon {
        VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 16 16" xmlns="http://www.w3.org/2000/svg">
                <path d="M3 2L13 8L3 14Z" fill="black"/>
            </svg>"#,
        )
        .expect("test icon should parse")
    }

    #[test]
    fn context_menu_select_dispatches() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Cut", Action::Cut),
                MenuItem::new("Copy", Action::Copy),
            ],
        );
        menu.layout(Rect::ZERO);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        menu.event(
            &UiEvent::MouseDown {
                position: Point::new(174.0, 117.0), // first item
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!menu.visible);
        assert_eq!(cell.into_inner(), vec![Action::Cut]);
    }

    #[test]
    fn context_menu_local_command_closes_without_dispatching() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::local("Rename", "rename")],
        );
        menu.layout(Rect::ZERO);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = menu.event(
            &UiEvent::MouseDown {
                position: Point::new(120.0, 117.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(!menu.visible);
        assert_eq!(menu.take_local_command().as_deref(), Some("rename"));
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn context_menu_click_outside_closes() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new("Copy", Action::Copy)],
        );
        menu.layout(Rect::ZERO);
        assert!(menu.visible);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});
        menu.event(
            &UiEvent::MouseDown {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!menu.visible);
    }

    #[test]
    fn context_menu_disabled_item_consumes_without_dispatch_or_close() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Copy", Action::Copy),
                MenuItem::new("Disabled", Action::Paste).disabled(),
            ],
        );
        menu.layout(Rect::ZERO);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = menu.event(
            &UiEvent::MouseDown {
                position: Point::new(120.0, 138.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(menu.visible);
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn context_menu_hover_tracks_items() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Cut", Action::Cut),
                MenuItem::new("Copy", Action::Copy),
            ],
        );
        menu.layout(Rect::ZERO);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        menu.event(
            &UiEvent::MouseMove {
                position: Point::new(120.0, 138.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(menu.hovered, Some(1));

        menu.event(
            &UiEvent::MouseMove {
                position: Point::new(10.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(menu.hovered, None);
    }

    #[test]
    fn context_menu_keyboard_navigation_skips_disabled_and_separator() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Copy", Action::Copy),
                MenuItem::new("Disabled", Action::Paste).disabled(),
                MenuItem::separator(),
                MenuItem::new("Cut", Action::Cut),
            ],
        );
        menu.layout(Rect::ZERO);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        menu.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(menu.hovered, Some(0));
        menu.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(menu.hovered, Some(3));
        menu.event(
            &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert!(!menu.visible);
        assert_eq!(cell.into_inner(), vec![Action::Cut]);
    }

    #[test]
    fn context_menu_escape_closes() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new("Copy", Action::Copy)],
        );
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        menu.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert!(!menu.visible);
    }

    #[test]
    fn context_menu_paints_in_overlay_layer() {
        let menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Cut", Action::Cut),
                MenuItem::new("Copy", Action::Copy),
            ],
        );
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut encoder = RecordingEncoder::default();

        {
            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            menu.paint(&mut ctx);
        }
        assert_eq!(encoder.rect_count, 0);

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        menu.paint_overlay(&mut ctx);
        assert!(encoder.rect_count > 0);
    }

    #[test]
    fn context_menu_clamps_to_bottom_right_viewport_and_keeps_hit_testing() {
        let mut menu = ContextMenu::new(
            Point::new(386.0, 286.0),
            vec![
                MenuItem::new("Cut", Action::Cut),
                MenuItem::new("Copy", Action::Copy),
                MenuItem::new("Paste", Action::Paste),
            ],
        );
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut encoder = RecordingEncoder::default();
        let mut paint_ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };

        menu.paint_overlay(&mut paint_ctx);

        let bounds = menu.bounds_rect();
        assert!(bounds.x + bounds.width <= 396.0);
        assert!(bounds.y + bounds.height <= 296.0);
        assert!(bounds.x < menu.anchor.x);
        assert!(bounds.y < menu.anchor.y);

        let actions = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            actions.borrow_mut().push(a);
        };
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);
        let copy = menu.item_rect(1).center();
        let result = menu.event(
            &UiEvent::MouseDown {
                position: copy,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(!menu.visible);
        assert_eq!(actions.borrow().as_slice(), &[Action::Copy]);
    }

    #[test]
    fn context_menu_scrolls_long_lists_inside_overlay_viewport() {
        let mut items = (0..19)
            .map(|index| MenuItem::new(format!("Item {index}"), Action::Copy))
            .collect::<Vec<_>>();
        items.push(MenuItem::new("Last", Action::Paste));
        let mut menu = ContextMenu::new(Point::new(20.0, 20.0), items);
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 240.0, 150.0);
        let mut encoder = RecordingEncoder::default();
        let mut paint_ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };

        menu.paint_overlay(&mut paint_ctx);

        let bounds = menu.bounds_rect();
        assert!(bounds.height <= clip_rect.height);
        assert!(menu.max_scroll_y() > 0.0);
        assert!(menu.item_at(menu.item_rect(19).center()).is_none());

        let actions = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            actions.borrow_mut().push(a);
        };
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);
        let wheel_position = bounds.center();
        let result = menu.event(
            &UiEvent::MouseWheel {
                position: wheel_position,
                delta: 1_000.0,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Handled);
        assert_eq!(menu.scroll_offset.get(), menu.max_scroll_y());

        let result = menu.event(
            &UiEvent::MouseDown {
                position: menu.item_rect(19).center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(actions.borrow().as_slice(), &[Action::Paste]);
        assert!(!menu.visible);
    }

    #[test]
    fn context_menu_measurement_expands_for_long_items() {
        let short = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new("Copy", Action::Copy)],
        );
        let long = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new(
                "Copy linked audio and video selection",
                Action::Copy,
            )],
        );

        assert_eq!(short.measure(LayoutConstraint::LOOSE).width, 148.0);
        assert!(
            long.measure(LayoutConstraint::LOOSE).width
                > short.measure(LayoutConstraint::LOOSE).width
        );
        assert_eq!(
            long.bounds_rect().width,
            long.measure(LayoutConstraint::LOOSE).width
        );
    }

    #[test]
    fn context_menu_icon_items_reserve_lane_and_paint_geometry() {
        let plain = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new(
                "Copy linked audio and video selection",
                Action::Copy,
            )],
        );
        let icon = test_icon();
        let iconized = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Copy linked audio and video selection", Action::Copy)
                    .with_icon(icon.clone())
                    .with_shortcut("Ctrl+C"),
            ],
        );
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        iconized.paint_overlay(&mut ctx);

        assert!(
            iconized.measure(LayoutConstraint::LOOSE).width
                > plain.measure(LayoutConstraint::LOOSE).width
        );
        assert_eq!(
            encoder.texts,
            vec!["Copy linked audio and video selection", "Ctrl+C"]
        );
        assert_eq!(encoder.triangles + encoder.raster_images, 1);
        assert!(icon.triangle_count() > 0);
    }

    #[test]
    fn context_menu_disabled_item_paints_text_without_strikethrough() {
        let menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Copy", Action::Copy),
                MenuItem::new("Disabled", Action::Paste).disabled(),
            ],
        );
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        menu.paint_overlay(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "Disabled"));
        assert_eq!(encoder.lines, 0);
    }

    #[test]
    fn context_menu_separator_paints_geometry_not_text() {
        let menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Copy", Action::Copy),
                MenuItem::separator(),
                MenuItem::new("Paste", Action::Paste),
            ],
        );
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        menu.paint_overlay(&mut ctx);

        assert_eq!(encoder.texts, vec!["Copy".to_string(), "Paste".to_string()]);
        assert!(
            encoder.rects.iter().any(|rect| rect.height == 1.0),
            "separator should be drawn as a geometric divider"
        );
    }
}
