//! 右键菜单控件
//!
//! 在指定位置弹出菜单项列表。点击选项或外部区域关闭。
//! 支持嵌套子菜单，与 Dropdown 共享相同的深度链架构。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::{Theme, ThemePreset};
use std::cell::Cell;

use crate::menu::{
    anchored_menu_rect, geometry_item_at, geometry_item_rect, menu_item_activation,
    menu_item_text_width_with_metrics, paint_menu_popup_chrome, paint_menu_row,
    paint_menu_scrollbar, paint_menu_separator, paint_submenu_arrow, popup_first_activatable_index,
    popup_next_activatable_index, rect_has_paintable_area, root_hovered_index, submenu_rect,
    MenuItem, MenuItemCommand, MenuItemKind, MenuMetrics, MenuRowPaint,
};

const MAX_VISIBLE_ITEMS: usize = 40;

/// 右键弹出菜单
///
/// 通常由父容器在检测到右键点击时创建并插入 Widget 树。
pub struct ContextMenu {
    id: WidgetId,
    items: Vec<MenuItem>,
    anchor: Point,
    bounds: Rect,
    visible: bool,
    /// 打开的子菜单深度链。chain[0] = 父菜单中被悬停的子菜单项索引。
    submenu_chain: Vec<usize>,
    /// (depth, item_index) — 当前悬停的层级和项索引。
    /// depth 0 = 父菜单, depth 1 = 第一级子菜单, 等等。
    hover_depth: Option<(usize, usize)>,
    local_command: Option<String>,
    overlay_viewport: Cell<Option<Rect>>,
    scroll_offset: Cell<f32>,
    visual: Cell<ContextMenuVisualTokens>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ContextMenuVisualTokens {
    metrics: MenuMetrics,
    outer_padding: f32,
    icon_lane_width: f32,
    viewport_vertical_margin: f32,
    anchor_gap: f32,
}

impl ContextMenuVisualTokens {
    fn from_theme(theme: &Theme) -> Self {
        let metrics = MenuMetrics::from_theme(theme);
        Self {
            metrics,
            outer_padding: metrics.popup_padding * 2.0,
            icon_lane_width: metrics.row_icon_size + metrics.row_icon_gap,
            viewport_vertical_margin: metrics.popup_viewport_pad,
            anchor_gap: metrics.popup_gap,
        }
    }

    fn content_padding_y(self) -> f32 {
        self.outer_padding * 0.5
    }
}

impl Default for ContextMenuVisualTokens {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

impl ContextMenu {
    pub fn new(anchor: Point, items: Vec<MenuItem>) -> Self {
        Self {
            id: WidgetId::new(),
            items,
            anchor,
            bounds: Rect::ZERO,
            visible: true,
            submenu_chain: Vec::new(),
            hover_depth: None,
            local_command: None,
            overlay_viewport: Cell::new(None),
            scroll_offset: Cell::new(0.0),
            visual: Cell::new(ContextMenuVisualTokens::default()),
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
        let visual = self.visual.get();
        anchored_menu_rect(
            Rect::new(self.anchor.x, self.anchor.y, 0.0, 0.0),
            self.menu_width() + visual.outer_padding,
            visual.outer_padding + self.visible_content_height(),
            visual.anchor_gap,
            self.overlay_viewport.get(),
        )
    }

    fn item_rect(&self, idx: usize) -> Rect {
        let bounds = self.bounds_rect();
        let visual = self.visual.get();
        Rect::new(
            bounds.x + visual.outer_padding * 0.5,
            bounds.y + visual.content_padding_y() + idx as f32 * self.item_height()
                - self.scroll_offset.get(),
            self.menu_width(),
            self.item_height(),
        )
    }

    fn content_height(&self) -> f32 {
        self.items.len() as f32 * self.item_height()
    }

    fn visible_content_height(&self) -> f32 {
        let content_height = self.content_height();
        let Some(viewport) =
            self.overlay_viewport.get().filter(|rect| rect_has_paintable_area(*rect))
        else {
            return content_height;
        };
        let available =
            (viewport.height - self.visual.get().viewport_vertical_margin).max(self.item_height());
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
            .map(|item| self.menu_item_text_width(item))
            .fold(0.0, f32::max);
        let visual = self.visual.get();
        visual
            .metrics
            .min_width
            .max(longest_item + visual.metrics.row_padding_x * 2.0 + self.icon_lane_width())
    }

    fn item_height(&self) -> f32 {
        self.visual.get().metrics.item_height
    }

    fn menu_item_text_width(&self, item: &MenuItem) -> f32 {
        menu_item_text_width_with_metrics(item, self.visual.get().metrics)
    }

    fn icon_lane_width(&self) -> f32 {
        if self.items.iter().any(|item| item.icon.is_some() || item.checked) {
            self.visual.get().icon_lane_width
        } else {
            0.0
        }
    }

    fn item_at(&self, position: Point) -> Option<usize> {
        let bounds = self.bounds_rect();
        if !bounds.contains(position) {
            return None;
        }
        let relative_y = position.y - (bounds.y + self.visual.get().content_padding_y())
            + self.scroll_offset.get();
        if relative_y < 0.0 {
            return None;
        }
        let index = (relative_y / self.item_height()).floor() as usize;
        if index < self.items.len() && self.item_rect(index).contains(position) {
            Some(index)
        } else {
            None
        }
    }

    // ── Submenu navigation ──────────────────────────────────────────────────────

    fn children_at(&self, chain_path: &[usize]) -> Option<&[MenuItem]> {
        let mut items: &[MenuItem] = &self.items;
        for idx in chain_path {
            let item = items.get(*idx)?;
            match &item.kind {
                MenuItemKind::Submenu { children } => items = children,
                _ => return None,
            }
        }
        Some(items)
    }

    fn submenu_rect_at(&self, chain_path: &[usize]) -> Option<Rect> {
        if chain_path.is_empty() {
            return None;
        }
        let parent_rect = self.item_rect(chain_path[0]);
        let children = self.children_at(&chain_path[..1])?;
        let mut bg = submenu_rect(parent_rect, children, MAX_VISIBLE_ITEMS, self.item_height());
        if chain_path.len() == 1 {
            return Some(bg);
        }
        let mut scroll = 0.0f32;
        let mut current_items = children;

        for d in 1..chain_path.len() {
            let idx = chain_path[d];
            let item = current_items.get(idx)?;
            match &item.kind {
                MenuItemKind::Submenu { children: new_children } => {
                    let parent_rect = geometry_item_rect(bg, idx, self.item_height(), scroll);
                    let sub = submenu_rect(
                        parent_rect,
                        new_children,
                        MAX_VISIBLE_ITEMS,
                        self.item_height(),
                    );
                    if d == chain_path.len() - 1 {
                        return Some(sub);
                    }
                    bg = sub;
                    scroll = 0.0;
                    current_items = new_children;
                }
                _ => return None,
            }
        }
        None
    }

    fn menu_rect_at_depth(&self, depth: usize) -> Option<Rect> {
        if depth == 0 {
            Some(self.bounds_rect())
        } else {
            self.submenu_rect_at(&self.submenu_chain[..depth])
        }
    }

    fn item_at_depth(&self, position: Point, depth: usize) -> Option<usize> {
        let menu_rect = self.menu_rect_at_depth(depth)?;
        let children = self.children_at(&self.submenu_chain[..depth])?;
        geometry_item_at(menu_rect, children.len(), self.item_height(), 0.0, position)
            .filter(|i| children[*i].is_activatable())
    }

    fn is_in_submenu_keep_alive_zone(&self, position: Point, depth: usize) -> bool {
        if depth == 0 || depth > self.submenu_chain.len() {
            return false;
        }
        let menu_bg = if depth == 1 {
            self.bounds_rect()
        } else {
            let Some(bg) = self.submenu_rect_at(&self.submenu_chain[..depth - 1]) else {
                return false;
            };
            bg
        };
        let parent_idx = self.submenu_chain[depth - 1];
        let parent_rect = if depth == 1 {
            self.item_rect(parent_idx)
        } else {
            geometry_item_rect(menu_bg, parent_idx, self.item_height(), 0.0)
        };
        let Some(sub_rect) = self.menu_rect_at_depth(depth) else {
            return false;
        };
        if sub_rect.contains(position) || parent_rect.contains(position) {
            return true;
        }
        let parent_right = parent_rect.x + parent_rect.width;
        let bridge_left = parent_right;
        let bridge_right = sub_rect.x;
        let bridge_top = parent_rect.y.min(sub_rect.y);
        let bridge_bottom = (parent_rect.y + parent_rect.height).max(sub_rect.y + sub_rect.height);
        let bridge = Rect::new(
            bridge_left,
            bridge_top,
            (bridge_right - bridge_left).max(0.0),
            (bridge_bottom - bridge_top).max(0.0),
        );
        bridge.contains(position)
    }

    // ── Activation ───────────────────────────────────────────────────────────────

    fn hovered_index(&self) -> Option<usize> {
        root_hovered_index(self.hover_depth)
    }

    fn first_activatable_index(&self) -> Option<usize> {
        popup_first_activatable_index(&self.items)
    }

    fn next_activatable_index(&self, direction: i32) -> Option<usize> {
        popup_next_activatable_index(&self.items, self.hovered_index(), direction)
    }

    fn activate_index(&mut self, index: usize, ctx: &mut EventContext) -> bool {
        let Some(activation) = menu_item_activation(&self.items[index]) else {
            return false;
        };
        self.apply_activation(activation, ctx);
        self.visible = false;
        self.submenu_chain.clear();
        true
    }

    fn apply_activation(&mut self, activation: MenuItemCommand, ctx: &mut EventContext) {
        match activation {
            MenuItemCommand::Action(action) => (ctx.dispatch)(action),
            MenuItemCommand::Local(command) => self.local_command = Some(command),
            MenuItemCommand::None => {}
        }
    }

    fn activate_hovered(&mut self, ctx: &mut EventContext) -> bool {
        let Some(index) = self.hovered_index().or_else(|| self.first_activatable_index()) else {
            return false;
        };
        self.activate_index(index, ctx)
    }

    fn ensure_hover_visible(&self) {
        let Some(index) = self.hovered_index() else {
            return;
        };
        let row_top = index as f32 * self.item_height();
        let row_bottom = row_top + self.item_height();
        let view_top = self.scroll_offset.get();
        let view_bottom = view_top + self.visible_content_height();
        if row_top < view_top {
            self.scroll_offset.set(row_top);
        } else if row_bottom > view_bottom {
            self.scroll_offset.set(row_bottom - self.visible_content_height());
        }
        self.clamp_scroll_offset();
    }

    fn close(&mut self) {
        self.visible = false;
        self.submenu_chain.clear();
        self.hover_depth = None;
    }
}

impl Widget for ContextMenu {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _c: LayoutConstraint) -> Size {
        if self.visible {
            let visual = self.visual.get();
            let h = visual.outer_padding + self.items.len() as f32 * self.item_height();
            Size::new(self.menu_width() + visual.outer_padding, h)
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
                // Check submenu clicks first (deepest first).
                let chain_len = self.submenu_chain.len();
                for depth in (1..=chain_len).rev() {
                    if let Some(sub_rect) = self.menu_rect_at_depth(depth) {
                        if sub_rect.contains(*position) {
                            if let Some(children) = self.children_at(&self.submenu_chain[..depth]) {
                                if let Some(sub_idx) = geometry_item_at(
                                    sub_rect,
                                    children.len(),
                                    self.item_height(),
                                    0.0,
                                    *position,
                                ) {
                                    if let Some(child) = children.get(sub_idx) {
                                        if let Some(activation) = menu_item_activation(child) {
                                            self.apply_activation(activation, ctx);
                                            self.close();
                                            return EventResult::Handled;
                                        }
                                    }
                                }
                            }
                            self.close();
                            return EventResult::Handled;
                        }
                    }
                }

                // Parent menu click.
                if let Some(index) = self.item_at(*position) {
                    if self.items[index].is_activatable() && !self.items[index].is_submenu() {
                        self.activate_index(index, ctx);
                    }
                    return EventResult::Handled;
                }

                // Clicked outside — close if not inside any submenu.
                if !self.bounds_rect().contains(*position) {
                    let in_submenu = (1..=chain_len)
                        .any(|d| self.menu_rect_at_depth(d).is_some_and(|r| r.contains(*position)));
                    if !in_submenu {
                        self.close();
                    }
                }
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                let mut new_hover_depth: Option<(usize, usize)> = None;
                let mut handled = false;

                // Check from deepest submenu upward
                for check_depth in (1..=self.submenu_chain.len()).rev() {
                    if let Some(sub_rect) = self.menu_rect_at_depth(check_depth) {
                        if sub_rect.contains(*position) {
                            if let Some(hovered_idx) = self.item_at_depth(*position, check_depth) {
                                new_hover_depth = Some((check_depth, hovered_idx));
                                let children =
                                    self.children_at(&self.submenu_chain[..check_depth]).unwrap();
                                if children[hovered_idx].is_submenu()
                                    && self.submenu_chain.len() <= check_depth
                                {
                                    self.submenu_chain.push(hovered_idx);
                                }
                            }
                            handled = true;
                            break;
                        }
                    }

                    if self.is_in_submenu_keep_alive_zone(*position, check_depth) {
                        new_hover_depth =
                            Some((check_depth - 1, self.submenu_chain[check_depth - 1]));
                        self.submenu_chain.truncate(check_depth);
                        handled = true;
                        break;
                    }
                }

                if !handled {
                    new_hover_depth = self
                        .item_at(*position)
                        .filter(|i| self.items[*i].is_activatable())
                        .map(|i| (0, i));

                    if let Some((0, idx)) = new_hover_depth {
                        if self.items[idx].is_submenu() && !self.submenu_chain.contains(&idx) {
                            self.submenu_chain.push(idx);
                        }
                    }
                }

                self.hover_depth = new_hover_depth;
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::MouseWheel { delta, position, .. } => {
                if self.bounds_rect().contains(*position) && self.max_scroll_y() > 0.0 {
                    let old = self.scroll_offset.get();
                    self.scroll_offset.set(self.scroll_offset.get() + *delta);
                    self.clamp_scroll_offset();
                    if (self.scroll_offset.get() - old).abs() > 0.01 {
                        ctx.request_repaint();
                    }
                }
                EventResult::Handled
            }
            UiEvent::MouseDown { button: MouseButton::Right, .. } => {
                self.close();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                self.close();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Down, modifiers }
                if *modifiers == Modifiers::none() =>
            {
                self.hover_depth = self.next_activatable_index(1).map(|i| (0, i));
                self.ensure_hover_visible();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Up, modifiers } if *modifiers == Modifiers::none() => {
                self.hover_depth = self.next_activatable_index(-1).map(|i| (0, i));
                self.ensure_hover_visible();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, modifiers }
                if *modifiers == Modifiers::none() =>
            {
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

        self.visual.set(ContextMenuVisualTokens::from_theme(ctx.theme));
        if !rect_has_paintable_area(ctx.clip_rect) {
            self.overlay_viewport.set(None);
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
                    active: item.checked,
                    hovered: self.hover_depth.is_some_and(|(d, idx)| d == 0 && idx == i)
                        || self.submenu_chain.first() == Some(&i),
                },
            );

            if item.is_submenu() {
                paint_submenu_arrow(ctx, r);
            }
        }
        ctx.pop_clip();
        paint_menu_scrollbar(
            ctx,
            bg,
            self.visible_content_height(),
            self.content_height(),
            self.scroll_offset.get(),
        );

        // Paint open submenus recursively
        let mut current_bg = bg;
        let mut current_items: &[MenuItem] = &self.items;
        let mut current_scroll = self.scroll_offset.get();

        for depth in 0..self.submenu_chain.len() {
            let sub_idx = self.submenu_chain[depth];
            let item = match current_items.get(sub_idx) {
                Some(item) => item,
                None => break,
            };
            let children = match &item.kind {
                MenuItemKind::Submenu { children } => children,
                _ => break,
            };
            if children.is_empty() {
                break;
            }
            let parent_rect = if depth == 0 {
                self.item_rect(sub_idx)
            } else {
                geometry_item_rect(current_bg, sub_idx, self.item_height(), current_scroll)
            };
            let sub_bg = submenu_rect(parent_rect, children, MAX_VISIBLE_ITEMS, self.item_height());

            paint_menu_popup_chrome(ctx, sub_bg);
            ctx.push_clip(sub_bg.inset(1.0, 1.0));
            for (ci, child) in children.iter().enumerate() {
                let cir = geometry_item_rect(sub_bg, ci, self.item_height(), 0.0);
                if child.is_separator() {
                    paint_menu_separator(ctx, cir);
                    continue;
                }
                let hovered = self.hover_depth.is_some_and(|(d, idx)| d == depth + 1 && idx == ci)
                    || self.submenu_chain.get(depth + 1) == Some(&ci);
                paint_menu_row(
                    ctx,
                    cir,
                    &child.label,
                    child.shortcut.as_deref(),
                    child.icon.as_ref(),
                    false,
                    MenuRowPaint {
                        enabled: child.enabled,
                        active: child.checked,
                        hovered,
                    },
                );
                if child.is_submenu() {
                    paint_submenu_arrow(ctx, cir);
                }
            }
            ctx.pop_clip();

            current_bg = sub_bg;
            current_items = children;
            current_scroll = 0.0;
        }
    }

    fn overlay_hit_test(&self, _point: Point) -> bool {
        self.visible
    }

    fn hit_test(&self, point: Point) -> bool {
        self.visible && self.bounds_rect().contains(point)
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        self.visible.then(|| {
            AccessibilityNode::new(self.id, AccessibilityRole::Menu)
                .with_name("Context menu")
                .with_state(AccessibilityState {
                    expanded: Some(true),
                    selected: Some(self.hover_depth.is_some()),
                    ..AccessibilityState::default()
                })
        })
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
    fn context_menu_accessibility_tracks_visibility_and_hover_selection() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new("Cut", Action::Cut)],
        );
        menu.hover_depth = Some((0, 0));

        let node = menu.accessibility().expect("visible context menu should expose accessibility");

        assert_eq!(node.role, AccessibilityRole::Menu);
        assert_eq!(node.name.as_deref(), Some("Context menu"));
        assert_eq!(node.state.expanded, Some(true));
        assert_eq!(node.state.selected, Some(true));

        menu.visible = false;
        assert!(menu.accessibility().is_none());
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
        assert_eq!(menu.hover_depth, Some((0, 1)));

        menu.event(
            &UiEvent::MouseMove {
                position: Point::new(10.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(menu.hover_depth, None);
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
        assert_eq!(menu.hover_depth, Some((0, 0)));
        menu.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(menu.hover_depth, Some((0, 3)));
        menu.event(
            &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert!(!menu.visible);
        assert_eq!(cell.into_inner(), vec![Action::Cut]);
    }

    #[test]
    fn context_menu_keyboard_activation_enters_first_submenu_child() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::submenu(
                "New",
                vec![
                    MenuItem::new("Video Track", Action::Cut),
                    MenuItem::new("Audio Track", Action::Paste),
                ],
            )],
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

        assert_eq!(
            menu.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert!(!menu.visible);
        assert_eq!(cell.into_inner(), vec![Action::Cut]);
    }

    #[test]
    fn context_menu_submenu_rows_track_hover() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::submenu(
                "新建",
                vec![
                    MenuItem::new("文件夹", Action::SaveProject),
                    MenuItem::new("纯色", Action::DeselectAll),
                ],
            )],
        );
        menu.layout(Rect::ZERO);
        menu.submenu_chain.push(0);
        let submenu = menu.submenu_rect_at(&[0]).expect("first submenu should resolve");
        let target = geometry_item_rect(submenu, 1, menu.item_height(), 0.0).center();

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch_fn = |_a: Action| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);
        assert_eq!(
            menu.event(
                &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(menu.hover_depth, Some((1, 1)));
    }

    #[test]
    fn context_menu_keyboard_navigation_ignores_modified_keys() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Copy", Action::Copy),
                MenuItem::new("Cut", Action::Cut),
            ],
        );
        menu.layout(Rect::ZERO);
        menu.hover_depth = Some((0, 1));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        for (key, modifiers) in [
            (KeyCode::Down, Modifiers::ctrl()),
            (KeyCode::Up, Modifiers::shift()),
            (KeyCode::Enter, Modifiers::ctrl()),
            (KeyCode::Space, Modifiers::shift()),
        ] {
            assert_eq!(
                menu.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert!(menu.visible);
            assert_eq!(menu.hover_depth, Some((0, 1)));
        }
        assert!(cell.borrow().is_empty());
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
        assert!(ctx.requests.repaint);
        ctx.requests.repaint = false;

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
        assert!(!ctx.requests.repaint);

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
    fn context_menu_overlay_skips_paint_when_clip_is_invalid_or_empty() {
        for clip_rect in [
            Rect::new(0.0, 0.0, 0.0, 120.0),
            Rect::new(0.0, 0.0, f32::INFINITY, 120.0),
        ] {
            let menu = ContextMenu::new(
                Point::new(20.0, 20.0),
                vec![MenuItem::new("Open", Action::OpenProject("".into()))],
            );
            let theme = mondrian_ui_theme::ThemePreset::Dark.build();
            let mut encoder = RecordingEncoder::default();

            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            menu.paint_overlay(&mut ctx);

            assert_eq!(encoder.rect_count, 0);
            assert!(encoder.rects.is_empty());
            assert!(encoder.texts.is_empty());
            assert_eq!(menu.overlay_viewport.get(), None);
        }
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

        let visual = ContextMenuVisualTokens::default();
        assert_eq!(
            short.measure(LayoutConstraint::LOOSE).width,
            visual.metrics.min_width + visual.outer_padding
        );
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
    fn context_menu_visual_metrics_follow_theme_tokens() {
        let menu = ContextMenu::new(
            Point::new(20.0, 20.0),
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        let mut theme = mondrian_ui_theme::ThemePreset::Dark.build();
        theme.spacing.menu_popup_padding = 8.0;
        theme.spacing.menu_item_height = 34.0;
        theme.spacing.menu_min_width = 192.0;
        theme.spacing.menu_row_icon_size = 16.0;
        theme.spacing.menu_row_icon_gap = 12.0;
        theme.spacing.menu_row_padding_x = 18.0;
        theme.spacing.menu_row_shortcut_gap = 32.0;
        theme.typography.small.font_size = 13.0;
        let clip_rect = Rect::new(0.0, 0.0, 300.0, 200.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        menu.paint_overlay(&mut ctx);

        let visual = ContextMenuVisualTokens::from_theme(&theme);
        assert_eq!(menu.item_rect(0).height, visual.metrics.item_height);
        assert_eq!(
            menu.measure(LayoutConstraint::LOOSE).height,
            visual.outer_padding + visual.metrics.item_height
        );
        assert_eq!(
            menu.measure(LayoutConstraint::LOOSE).width,
            visual.metrics.min_width + visual.outer_padding
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
    fn context_menu_checked_items_reserve_lane_and_paint_checkmark() {
        let label = "Toggle linked selection for selected tracks";
        let plain = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new(label, Action::Copy)],
        );
        let checked = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new(label, Action::Copy).checked(true)],
        );
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 500.0, 300.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        checked.paint_overlay(&mut ctx);

        assert!(
            checked.measure(LayoutConstraint::LOOSE).width
                > plain.measure(LayoutConstraint::LOOSE).width
        );
        assert_eq!(encoder.lines, 2);
        assert_eq!(encoder.texts, vec![label]);
        assert_eq!(encoder.triangles + encoder.raster_images, 0);
    }

    #[test]
    fn context_menu_checked_shortcut_items_keep_cjk_label_clip_room() {
        let label = "时间线";
        let menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new(label, Action::Copy).checked(true).with_shortcut("Ctrl+Alt+T")],
        );
        let metrics = MenuMetrics::current();
        let width = menu.measure(LayoutConstraint::LOOSE).width;
        let icon_lane = metrics.row_icon_size + metrics.row_icon_gap;
        let shortcut_width =
            crate::text_metrics::measure_single_line("Ctrl+Alt+T", metrics.measure_font_size).0;
        let label_clip_width = width
            - metrics.row_padding_x * 2.0
            - icon_lane
            - metrics.row_shortcut_gap
            - shortcut_width;
        let label_width =
            crate::text_metrics::measure_single_line(label, metrics.measure_font_size).0;

        assert!(
            label_clip_width >= label_width + metrics.row_icon_gap,
            "checked menu label clip should keep safety room for CJK glyph edges"
        );
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
