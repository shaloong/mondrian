//! Dropdown menu widget.
//!
//! Click-triggered popup list that dispatches an Action on selection.
//! Shared paint, geometry, and model primitives are consumed by
//! `ContextMenu`, `ColorPicker` mode dropdowns, and menu bars.

mod geometry;
mod model;
mod paint;

pub(crate) use geometry::{anchored_menu_rect, rect_has_paintable_area};
use model::MENU_POPUP_PADDING;
pub use model::{DropdownTriggerStyle, MenuItem, MenuItemKind};
pub(crate) use model::{MenuRowPaint, MenuVisualTokens};
pub(crate) use paint::{
    paint_disabled_trigger, paint_menu_popup_chrome, paint_menu_row, paint_menu_scrollbar,
    paint_menu_separator, paint_menu_trigger, paint_open_menu_with_submenu,
};

use std::cell::Cell;

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::paint::paint_focus_ring;

use geometry::{
    clamp_scroll_offset, item_at, item_rect, max_scroll_y, menu_rect, preferred_trigger_width,
    scroll_to_visible, trigger_height, trigger_rect, visible_item_count,
};

/// Dropdown menu widget.
///
/// Click the trigger to open a popup list. Select an item to dispatch its
/// [`Action`] and close. Open-state selects the first activatable item;
/// keyboard navigation skips disabled items and separators.
pub struct Dropdown {
    id: WidgetId,
    #[allow(dead_code)]
    label: String,
    items: Vec<MenuItem>,
    bounds: Rect,
    enabled: bool,
    open: bool,
    pressed_index: Option<usize>,
    item_height: f32,
    max_visible_items: usize,
    scroll_offset: f32,
    suppress_next_release: bool,
    focused: bool,
    focus_visible: bool,
    trigger_hovered: bool,
    /// Stack of open submenu indices forming a path through nested submenus.
    /// chain[0] = index in parent menu → first submenu is open
    /// chain[1] = index inside chain[0]'s children → second submenu is open, etc.
    submenu_chain: Vec<usize>,
    /// (depth, item_index) — which item at which depth is hovered.
    /// depth 0 = parent menu, depth 1 = first submenu, etc.
    hover_depth: Option<(usize, usize)>,
    overlay_viewport: Cell<Option<Rect>>,
    trigger_style: DropdownTriggerStyle,
}

impl Dropdown {
    pub fn new(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self {
            id: WidgetId::new(),
            label: label.into(),
            items,
            bounds: Rect::ZERO,
            enabled: true,
            open: false,
            pressed_index: None,
            item_height: 28.0,
            max_visible_items: 40,
            scroll_offset: 0.0,
            suppress_next_release: false,
            focused: false,
            focus_visible: false,
            trigger_hovered: false,
            submenu_chain: Vec::new(),
            hover_depth: None,
            overlay_viewport: Cell::new(None),
            trigger_style: DropdownTriggerStyle::Filled,
        }
    }

    /// Set whether the dropdown accepts input and participates in focus.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.open = false;
            self.hover_depth = None;
            self.pressed_index = None;
            self.suppress_next_release = false;
            self.focused = false;
            self.focus_visible = false;
        }
        self
    }

    /// Disable the dropdown.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the dropdown is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Select the visual style for the closed trigger.
    pub fn with_trigger_style(mut self, style: DropdownTriggerStyle) -> Self {
        self.trigger_style = style;
        self
    }

    /// Whether the popup menu is currently open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Update checked state for all rows matching an app action.
    pub fn set_checked_for_action(&mut self, action: &Action, checked: bool) {
        set_checked_recursive(&mut self.items, action, checked);
    }

    /// Return checked state for the first row matching an app action.
    pub fn checked_for_action(&self, action: &Action) -> Option<bool> {
        let mut items_to_check: Vec<&MenuItem> = self.items.iter().collect();
        let mut i = 0;
        while i < items_to_check.len() {
            let item = items_to_check[i];
            if !item.is_separator() && item.action == *action {
                return Some(item.checked);
            }
            if let MenuItemKind::Submenu { ref children } = item.kind {
                items_to_check.extend(children);
            }
            i += 1;
        }
        None
    }

    /// Whether a point is inside the closed trigger chrome.
    pub fn trigger_contains(&self, point: Point) -> bool {
        trigger_rect(self.bounds, self.trigger_style).contains(point)
    }

    /// Close the popup menu if it is open.
    pub fn close_menu(&mut self, ctx: &mut EventContext) {
        if self.open {
            self.close(ctx);
        }
    }

    /// Open the popup menu from parent-level coordination.
    ///
    /// This is intentionally different from trigger-click opening: a parent
    /// menu bar may open a sibling menu on hover, and that should not suppress
    /// the next pointer release because it belongs to a fresh item click.
    pub fn open_menu(&mut self, ctx: &mut EventContext) {
        self.open_with_release_suppression(ctx, false);
    }

    /// Limit how many rows are visible before the open menu scrolls.
    pub fn with_max_visible_items(mut self, max_visible_items: usize) -> Self {
        self.max_visible_items = max_visible_items.max(1);
        self.scroll_offset = clamp_scroll_offset(
            self.scroll_offset,
            max_scroll_y(
                self.items.len(),
                visible_item_count(self.items.len(), self.max_visible_items),
                self.item_height,
            ),
        );
        self
    }

    // ── Internal helpers ────────────────────────────────────────────────────────

    fn trigger_rect(&self) -> Rect {
        trigger_rect(self.bounds, self.trigger_style)
    }

    fn preferred_trigger_width(&self) -> f32 {
        preferred_trigger_width(&self.label, self.trigger_style)
    }

    fn visible_item_count(&self) -> usize {
        visible_item_count(self.items.len(), self.max_visible_items)
    }

    fn max_scroll_y(&self) -> f32 {
        max_scroll_y(
            self.items.len(),
            self.visible_item_count(),
            self.item_height,
        )
    }

    fn clamp_scroll_offset(&mut self) {
        self.scroll_offset = clamp_scroll_offset(self.scroll_offset, self.max_scroll_y());
    }

    fn menu_rect(&self) -> Rect {
        menu_rect(
            self.bounds,
            &self.items,
            &self.label,
            self.trigger_style,
            self.max_visible_items,
            self.item_height,
            self.overlay_viewport.get(),
        )
    }

    #[allow(dead_code)]
    fn item_rect(&self, index: usize) -> Rect {
        item_rect(
            self.menu_rect(),
            index,
            self.item_height,
            self.scroll_offset,
        )
    }

    fn item_at(&self, position: Point) -> Option<usize> {
        let visible = visible_item_count(self.items.len(), self.max_visible_items);
        item_at(
            self.menu_rect(),
            visible,
            self.item_height,
            self.scroll_offset,
            position,
        )
    }

    fn submenu_rect_at(&self, chain_path: &[usize]) -> Option<Rect> {
        if chain_path.is_empty() {
            return None;
        }
        let mut bg = self.menu_rect();
        let mut scroll = self.scroll_offset;
        let mut current_items: &[MenuItem] = &self.items;
        for d in 0..chain_path.len() {
            let idx = chain_path[d];
            let item = current_items.get(idx)?;
            match &item.kind {
                MenuItemKind::Submenu { children } => {
                    let parent_rect = item_rect(bg, idx, self.item_height, scroll);
                    let sub = geometry::submenu_rect(
                        parent_rect,
                        children,
                        self.max_visible_items,
                        self.item_height,
                    );
                    if d == chain_path.len() - 1 {
                        return Some(sub);
                    }
                    bg = sub;
                    scroll = 0.0;
                    current_items = children;
                }
                _ => return None,
            }
        }
        None
    }

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

    fn menu_rect_at_depth(&self, depth: usize) -> Option<Rect> {
        if depth == 0 {
            Some(self.menu_rect())
        } else {
            self.submenu_rect_at(&self.submenu_chain[..depth])
        }
    }

    fn item_at_depth(&self, position: Point, depth: usize) -> Option<usize> {
        let menu_rect = self.menu_rect_at_depth(depth)?;
        let children = self.children_at(&self.submenu_chain[..depth])?;
        item_at(menu_rect, children.len(), self.item_height, 0.0, position)
            .filter(|i| children[*i].is_activatable())
    }

    fn is_in_submenu_keep_alive_zone(&self, position: Point, depth: usize) -> bool {
        if depth == 0 || depth > self.submenu_chain.len() {
            return false;
        }
        let menu_bg = if depth == 1 {
            self.menu_rect()
        } else {
            let Some(bg) = self.submenu_rect_at(&self.submenu_chain[..depth - 1]) else {
                return false;
            };
            bg
        };
        let parent_idx = self.submenu_chain[depth - 1];
        let parent_scroll = if depth == 1 { self.scroll_offset } else { 0.0 };
        let parent_rect = item_rect(menu_bg, parent_idx, self.item_height, parent_scroll);
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

    fn first_activatable_index(&self) -> Option<usize> {
        self.items.iter().position(|item| item.is_activatable())
    }

    fn hovered_index(&self) -> Option<usize> {
        self.hover_depth.filter(|(d, _)| *d == 0).map(|(_, i)| i)
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
            .hovered_index()
            .and_then(|index| activatable.iter().position(|candidate| *candidate == index));
        let next = match (current, direction) {
            (Some(index), d) if d < 0 => (index + activatable.len() - 1) % activatable.len(),
            (Some(index), _) => (index + 1) % activatable.len(),
            (None, d) if d < 0 => activatable.len() - 1,
            (None, _) => 0,
        };
        activatable.get(next).copied()
    }

    fn ensure_hover_visible(&mut self) {
        self.scroll_offset = scroll_to_visible(
            self.hovered_index(),
            self.scroll_offset,
            self.visible_item_count(),
            self.items.len(),
            self.item_height,
        );
    }

    fn activate_hovered(&mut self, ctx: &mut EventContext) -> bool {
        let Some(index) = self.hovered_index() else {
            return false;
        };
        if !self.items[index].is_activatable() {
            return false;
        }
        (ctx.dispatch)(self.items[index].action.clone());
        self.close(ctx);
        true
    }

    fn open(&mut self, ctx: &mut EventContext) {
        self.open_with_release_suppression(ctx, true);
    }

    fn open_with_release_suppression(
        &mut self,
        ctx: &mut EventContext,
        suppress_next_release: bool,
    ) {
        self.open = true;
        self.hover_depth = self.first_activatable_index().map(|i| (0, i));
        self.pressed_index = None;
        self.suppress_next_release = suppress_next_release;
        self.clamp_scroll_offset();
        self.ensure_hover_visible();
        ctx.request_pointer_capture(self.id);
    }

    fn close(&mut self, ctx: &mut EventContext) {
        self.open = false;
        self.submenu_chain.clear();
        self.hover_depth = None;
        self.pressed_index = None;
        self.suppress_next_release = false;
        ctx.release_pointer_capture(self.id);
    }
}

/// Recursively set checked state, including submenus.
fn set_checked_recursive(items: &mut [MenuItem], action: &Action, checked: bool) {
    for item in items.iter_mut() {
        if !item.is_separator() && item.action == *action {
            item.checked = checked;
        }
        if let MenuItemKind::Submenu { ref mut children } = item.kind {
            set_checked_recursive(children, action, checked);
        }
    }
}

impl Widget for Dropdown {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(
            self.preferred_trigger_width(),
            trigger_height(self.trigger_style),
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.clamp_scroll_offset();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.open {
                self.close(ctx);
            }
            self.focused = false;
            self.focus_visible = false;
            return EventResult::Ignored;
        }
        if self.open {
            match event {
                UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                    if self.trigger_rect().contains(*position) {
                        self.close(ctx);
                        return EventResult::Handled;
                    }
                    // Check submenu clicks first (outside parent menu rect but inside submenu).
                    let chain_len = self.submenu_chain.len();
                    for depth in (1..=chain_len).rev() {
                        if let Some(sub_rect) = self.menu_rect_at_depth(depth) {
                            if sub_rect.contains(*position) {
                                // Will be handled in MouseUp; just record the pressed state.
                                self.pressed_index = None;
                                return EventResult::Handled;
                            }
                        }
                    }
                    if let Some(i) = self.item_at(*position) {
                        self.pressed_index = Some(i);
                        return EventResult::Handled;
                    }
                    // Close if clicked outside both the parent and submenu rects.
                    let in_submenu = (1..=chain_len)
                        .any(|d| self.menu_rect_at_depth(d).is_some_and(|r| r.contains(*position)));
                    if !self.menu_rect().contains(*position) && !in_submenu {
                        self.close(ctx);
                        return EventResult::Handled;
                    }
                    self.pressed_index = None;
                    return EventResult::Handled;
                }
                UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                    if self.suppress_next_release {
                        self.suppress_next_release = false;
                        return EventResult::Handled;
                    }
                    // Check submenu clicks from deepest
                    let chain_len = self.submenu_chain.len();
                    for depth in (1..=chain_len).rev() {
                        if let Some(sub_rect) = self.menu_rect_at_depth(depth) {
                            if sub_rect.contains(*position) {
                                let rel_y = position.y - sub_rect.y - MENU_POPUP_PADDING;
                                let sub_item = (rel_y / self.item_height).floor().max(0.0) as usize;
                                if let Some(children) =
                                    self.children_at(&self.submenu_chain[..depth])
                                {
                                    if let Some(child) = children.get(sub_item) {
                                        if child.is_activatable() {
                                            (ctx.dispatch)(child.action.clone());
                                        }
                                    }
                                }
                                self.close(ctx);
                                return EventResult::Handled;
                            }
                        }
                    }
                    let released_index = self.item_at(*position);
                    if let (Some(pressed), Some(released)) = (self.pressed_index, released_index) {
                        if pressed == released && self.items[released].is_activatable() {
                            // Don't close if the clicked item is a submenu
                            if self.items[released].is_submenu() {
                                self.pressed_index = None;
                            } else {
                                (ctx.dispatch)(self.items[released].action.clone());
                                self.close(ctx);
                            }
                        } else {
                            self.pressed_index = None;
                        }
                        return EventResult::Handled;
                    }
                    self.pressed_index = None;
                    if !self.trigger_rect().contains(*position)
                        && !self.menu_rect().contains(*position)
                    {
                        self.close(ctx);
                    }
                    return EventResult::Handled;
                }
                UiEvent::MouseMove { position, .. } => {
                    let mut new_hover_depth: Option<(usize, usize)> = None;

                    // Check from deepest submenu upward
                    let mut handled = false;
                    for check_depth in (1..=self.submenu_chain.len()).rev() {
                        // Check if in this submenu
                        if let Some(sub_rect) = self.menu_rect_at_depth(check_depth) {
                            if sub_rect.contains(*position) {
                                if let Some(hovered_idx) =
                                    self.item_at_depth(*position, check_depth)
                                {
                                    new_hover_depth = Some((check_depth, hovered_idx));
                                    let children = self
                                        .children_at(&self.submenu_chain[..check_depth])
                                        .unwrap();
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

                        // Check keep-alive for this depth
                        if self.is_in_submenu_keep_alive_zone(*position, check_depth) {
                            new_hover_depth =
                                Some((check_depth - 1, self.submenu_chain[check_depth - 1]));
                            self.submenu_chain.truncate(check_depth);
                            handled = true;
                            break;
                        }

                        // Not in submenu and not in keep-alive → close this level
                        self.submenu_chain.truncate(check_depth - 1);
                    }

                    if !handled {
                        // Default: hover at parent menu level
                        new_hover_depth = self
                            .item_at(*position)
                            .filter(|i| self.items[*i].is_activatable())
                            .map(|i| (0, i));

                        // Auto-open submenu on hover
                        if let Some((0, idx)) = new_hover_depth {
                            if self.items[idx].is_submenu() && !self.submenu_chain.contains(&idx) {
                                self.submenu_chain.push(idx);
                            }
                        }
                    }

                    self.hover_depth = new_hover_depth;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                UiEvent::MouseWheel { delta, position, .. } => {
                    if self.menu_rect().contains(*position) && self.max_scroll_y() > 0.0 {
                        let old = self.scroll_offset;
                        self.scroll_offset += *delta;
                        self.clamp_scroll_offset();
                        if (self.scroll_offset - old).abs() > 0.01 {
                            ctx.request_repaint();
                        }
                    }
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                    self.close(ctx);
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Down, modifiers }
                    if *modifiers == Modifiers::none() =>
                {
                    self.hover_depth = self.next_activatable_index(1).map(|i| (0, i));
                    self.ensure_hover_visible();
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Up, modifiers }
                    if *modifiers == Modifiers::none() =>
                {
                    self.hover_depth = self.next_activatable_index(-1).map(|i| (0, i));
                    self.ensure_hover_visible();
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, modifiers }
                    if *modifiers == Modifiers::none() =>
                {
                    self.activate_hovered(ctx);
                    return EventResult::Handled;
                }
                _ => {}
            }
        } else {
            match event {
                UiEvent::FocusGained => {
                    self.focused = true;
                    self.focus_visible = true;
                    return EventResult::Handled;
                }
                UiEvent::FocusLost => {
                    self.focused = false;
                    self.focus_visible = false;
                    return EventResult::Handled;
                }
                UiEvent::KeyDown {
                    key: KeyCode::Enter | KeyCode::Space | KeyCode::Down,
                    modifiers,
                } if self.focused && *modifiers == Modifiers::none() => {
                    self.focus_visible = false;
                    self.open(ctx);
                    return EventResult::Handled;
                }
                UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                    if self.trigger_rect().contains(*position) {
                        self.focus_visible = false;
                        self.open(ctx);
                        return EventResult::Handled;
                    }
                }
                UiEvent::MouseMove { position, .. } => {
                    let was_hovered = self.trigger_hovered;
                    self.trigger_hovered = self.trigger_rect().contains(*position);
                    if was_hovered != self.trigger_hovered {
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                }
                _ => {}
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if self.enabled {
            paint_menu_trigger(
                ctx,
                self.trigger_rect(),
                &self.label,
                self.open,
                self.trigger_hovered,
                self.trigger_style,
            );
        } else {
            paint_disabled_trigger(ctx, &self.label, self.bounds, self.trigger_style);
        }
        if self.focus_visible && !self.open {
            let visual = MenuVisualTokens::from_theme(ctx.theme);
            paint_focus_ring(
                ctx,
                self.trigger_rect(),
                visual.trigger_radius(self.trigger_style),
            );
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        paint_open_menu_with_submenu(
            ctx,
            self.bounds,
            &self.items,
            &self.label,
            self.trigger_style,
            self.max_visible_items,
            self.item_height,
            self.hover_depth,
            self.scroll_offset,
            &self.overlay_viewport,
            self.open,
            &self.submenu_chain,
        );
    }

    fn overlay_hit_test(&self, _point: Point) -> bool {
        self.enabled && self.open
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::Menu)
                .with_name(self.label.clone())
                .with_state(AccessibilityState {
                    focusable: self.enabled,
                    focused: self.focused,
                    disabled: !self.enabled,
                    expanded: Some(self.open),
                    ..AccessibilityState::default()
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use crate::VectorIcon;
    use glam;
    use mondrian_core::Color;
    use mondrian_editor_state::state::PanelKind;
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests, PointerCaptureRequest};
    use std::cell::RefCell;

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct SoftShadowCommand {
        bounds: Rect,
        color: Color,
        corner_radius: f32,
        blur_radius: f32,
        spread: f32,
        offset: glam::Vec2,
    }

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        rect_colors: Vec<Color>,
        rect_radii: Vec<f32>,
        soft_shadows: Vec<SoftShadowCommand>,
        clips: Vec<Rect>,
        clip_pops: usize,
        lines: usize,
        texts: Vec<String>,
        triangles: usize,
        raster_images: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, bounds: Rect, color: mondrian_core::Color, corner_radius: f32) {
            self.rects.push(bounds);
            self.rect_colors.push(color);
            self.rect_radii.push(corner_radius);
        }

        fn draw_soft_shadow(
            &mut self,
            bounds: Rect,
            color: mondrian_core::Color,
            corner_radius: f32,
            blur_radius: f32,
            spread: f32,
            offset: glam::Vec2,
        ) {
            self.soft_shadows.push(SoftShadowCommand {
                bounds,
                color,
                corner_radius,
                blur_radius,
                spread,
                offset,
            });
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
    fn dropdown_new_is_closed() {
        let d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        assert!(!d.open);
    }

    #[test]
    fn dropdown_updates_checked_state_for_matching_action() {
        let mut d = Dropdown::new(
            "View",
            vec![
                MenuItem::new("Viewer", Action::TogglePanel(PanelKind::Viewer)),
                MenuItem::new("Timeline", Action::TogglePanel(PanelKind::Timeline)),
            ],
        );

        d.set_checked_for_action(&Action::TogglePanel(PanelKind::Viewer), true);

        assert_eq!(
            d.checked_for_action(&Action::TogglePanel(PanelKind::Viewer)),
            Some(true)
        );
        assert_eq!(
            d.checked_for_action(&Action::TogglePanel(PanelKind::Timeline)),
            Some(false)
        );
    }

    #[test]
    fn dropdown_click_opens() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(d.open);
    }

    #[test]
    fn dropdown_exposes_trigger_and_open_state_for_menu_bar_coordination() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(20.0, 10.0, 120.0, 28.0));
        assert!(!d.is_open());
        assert!(d.trigger_contains(Point::new(30.0, 20.0)));
        assert!(!d.trigger_contains(Point::new(30.0, 48.0)));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        d.open_menu(&mut ctx);
        assert!(d.is_open());
        assert!(!d.suppress_next_release);
        d.close_menu(&mut ctx);
        assert!(!d.is_open());
    }

    #[test]
    fn disabled_dropdown_ignores_click_and_focus() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        )
        .disabled();
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(!d.open);
        assert!(!d.can_focus());
        assert!(ctx.requests.pointer_capture.is_none());
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn dropdown_accessibility_exposes_menu_state() {
        let mut dropdown = Dropdown::new("Viewer scale", vec![MenuItem::new("Fit", Action::Play)]);
        dropdown.focused = true;
        dropdown.open = true;

        let node = dropdown.accessibility().expect("dropdown should expose accessibility");

        assert_eq!(node.role, AccessibilityRole::Menu);
        assert_eq!(node.name.as_deref(), Some("Viewer scale"));
        assert!(node.state.focusable);
        assert!(node.state.focused);
        assert_eq!(node.state.expanded, Some(true));
        assert!(!node.state.disabled);
    }

    #[test]
    fn dropdown_select_dispatches_and_closes() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Save", Action::SaveProject),
                MenuItem::new("Quit", Action::CloseProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        // Open
        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(d.open);

        // Opening suppresses the release that belongs to the trigger click.
        d.event(
            &UiEvent::MouseUp {
                position: Point::new(60.0, 40.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(d.open);
        assert!(cell.borrow().is_empty());

        // Click first item at y = 28 + 0*24 = 28 → should dispatch SaveProject
        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 40.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        d.event(
            &UiEvent::MouseUp {
                position: Point::new(60.0, 40.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(!d.open);
        let actions = cell.into_inner();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], Action::SaveProject);
    }

    #[test]
    fn dropdown_click_trigger_when_open_closes() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let result = d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(!d.open);
    }

    #[test]
    fn dropdown_disabled_item_consumes_without_dispatch_or_close() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Save", Action::SaveProject),
                MenuItem::new("Disabled", Action::CloseProject).disabled(),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 64.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(d.open);
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn dropdown_open_requests_capture_and_release_does_not_select() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut requests = EventRequests::default();
        let platform = NoopPlatformService;
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &dispatch_fn,
            platform: &platform,
            requests: &mut requests,
        };

        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(d.id))
        );

        d.event(
            &UiEvent::MouseUp {
                position: Point::new(60.0, 40.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(d.open);
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn dropdown_outside_click_closes_when_open() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        d.event(
            &UiEvent::MouseDown {
                position: Point::new(300.0, 300.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(!d.open);
    }

    #[test]
    fn dropdown_wheel_scrolls_visible_menu_down_and_up() {
        let items: Vec<MenuItem> = (0..12)
            .map(|i| MenuItem::new(format!("Item {i}"), Action::SaveProject))
            .collect();
        let mut d = Dropdown::new("File", items).with_max_visible_items(3);
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        d.event(
            &UiEvent::MouseWheel {
                delta: 48.0,
                position: Point::new(60.0, 40.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(d.scroll_offset > 0.0);
        assert!(ctx.requests.repaint);
        ctx.requests.repaint = false;

        d.event(
            &UiEvent::MouseWheel {
                delta: -999.0,
                position: Point::new(60.0, 40.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(d.scroll_offset, 0.0);
        assert!(ctx.requests.repaint);
        ctx.requests.repaint = false;

        d.event(
            &UiEvent::MouseWheel {
                delta: -999.0,
                position: Point::new(60.0, 40.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(d.scroll_offset, 0.0);
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn dropdown_keyboard_navigation_skips_disabled_and_separators() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Disabled", Action::CloseProject).disabled(),
                MenuItem::separator(),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(d.hovered_index(), Some(0));

        d.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(d.hovered_index(), Some(3));

        d.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert!(!d.open);
        assert_eq!(cell.into_inner(), vec![Action::SaveProject]);
    }

    #[test]
    fn dropdown_keyboard_navigation_wraps_up_from_first_item() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        d.open(&mut ctx);
        assert_eq!(d.hovered_index(), Some(0));
        d.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(d.hovered_index(), Some(1));
    }

    #[test]
    fn dropdown_keyboard_navigation_ignores_modified_keys_when_open() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        d.open(&mut ctx);
        d.hover_depth = Some((0, 1));
        for (key, modifiers) in [
            (KeyCode::Down, Modifiers::ctrl()),
            (KeyCode::Up, Modifiers::shift()),
            (KeyCode::Enter, Modifiers::ctrl()),
            (KeyCode::Space, Modifiers::shift()),
        ] {
            assert_eq!(
                d.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert!(d.open);
            assert_eq!(d.hovered_index(), Some(1));
        }
        assert!(cell.borrow().is_empty());
    }

    #[test]
    fn dropdown_trigger_ignores_modified_keyboard_open_chords() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        assert_eq!(
            d.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Handled
        );
        for (key, modifiers) in [
            (KeyCode::Down, Modifiers::ctrl()),
            (KeyCode::Enter, Modifiers::shift()),
            (KeyCode::Space, Modifiers::ctrl()),
        ] {
            assert_eq!(
                d.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert!(!d.open);
        }
    }

    #[test]
    fn dropdown_overlay_hit_test_catches_outside_clicks_while_open() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        assert!(!d.hit_test(Point::new(300.0, 300.0)));
        assert!(!d.overlay_hit_test(Point::new(300.0, 300.0)));

        d.open = true;
        assert!(!d.hit_test(Point::new(300.0, 300.0)));
        assert!(d.overlay_hit_test(Point::new(300.0, 300.0)));
    }

    #[test]
    fn dropdown_open_state_does_not_change_layout_measurement() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );

        let closed = d.measure(LayoutConstraint::LOOSE);
        d.open = true;
        let open = d.measure(LayoutConstraint::LOOSE);

        assert_eq!(open, closed);
        assert_eq!(open, Size::new(160.0, 28.0));
    }

    #[test]
    fn menu_bar_trigger_measures_as_compact_text_without_arrow_space() {
        let menu_bar_dropdown = Dropdown::new("File", vec![MenuItem::new("Open", Action::Copy)])
            .with_trigger_style(DropdownTriggerStyle::MenuBar);
        let filled_dropdown = Dropdown::new("File", vec![MenuItem::new("Open", Action::Copy)]);

        let menu_bar_size = menu_bar_dropdown.measure(LayoutConstraint::LOOSE);
        let filled_size = filled_dropdown.measure(LayoutConstraint::LOOSE);

        assert_eq!(menu_bar_size.height, model::MENU_BAR_TRIGGER_HEIGHT);
        assert!(menu_bar_size.width < 48.0);
        assert_eq!(filled_size.height, model::MENU_TRIGGER_HEIGHT);
        assert!(filled_size.width >= model::MENU_MIN_WIDTH);
    }

    #[test]
    fn menu_visual_tokens_follow_theme_spacing_and_typography() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let visual = MenuVisualTokens::from_theme(&theme);

        assert_eq!(visual.filled_trigger_radius, theme.spacing.radius_sm);
        assert_eq!(visual.row_radius, theme.spacing.radius_sm);
        assert_eq!(
            visual.popup_radius,
            theme.spacing.radius_lg.min(theme.spacing.radius_md)
        );
        assert_eq!(visual.row_font_size, theme.typography.small.font_size);
        assert_eq!(
            visual.shortcut_font_size,
            theme.typography.metadata.font_size
        );
        assert_eq!(
            visual.scrollbar_min_thumb_height,
            theme.spacing.icon_size + theme.spacing.border_emphasis
        );
    }

    #[test]
    fn dropdown_trigger_clips_long_label_before_arrow() {
        let mut d = Dropdown::new(
            "Very long trigger label that must not cover the arrow",
            vec![MenuItem::new("Open", Action::CloseProject)],
        );
        d.layout(Rect::new(0.0, 0.0, 80.0, 28.0));
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 120.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint(&mut ctx);

        assert_eq!(
            encoder.texts,
            vec!["Very long trigger label that must not cover the arrow"]
        );
        assert_eq!(encoder.triangles, 1);
        assert_eq!(encoder.clips, vec![Rect::new(8.0, 0.0, 40.0, 28.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }

    #[test]
    fn disabled_dropdown_trigger_clips_long_label_inside_control() {
        let mut d = Dropdown::new(
            "Disabled trigger label that should stay clipped",
            vec![MenuItem::new("Open", Action::CloseProject)],
        )
        .disabled();
        d.layout(Rect::new(0.0, 0.0, 80.0, 28.0));
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 120.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint(&mut ctx);

        assert_eq!(
            encoder.texts,
            vec!["Disabled trigger label that should stay clipped"]
        );
        assert_eq!(encoder.triangles, 0);
        assert_eq!(encoder.clips, vec![Rect::new(8.0, 0.0, 64.0, 28.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }

    #[test]
    fn dropdown_measures_trigger_label_without_using_popup_items() {
        let mut d = Dropdown::new(
            "Mode",
            vec![MenuItem::new(
                "A very long menu option that should widen the popup",
                Action::CloseProject,
            )],
        );

        assert_eq!(d.measure(LayoutConstraint::LOOSE), Size::new(160.0, 28.0));

        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        assert!(d.menu_rect().width > 300.0);
    }

    #[test]
    fn dropdown_long_trigger_label_expands_closed_measurement() {
        let d = Dropdown::new(
            "Very long color mode selector",
            vec![MenuItem::new("HEX", Action::CloseProject)],
        );

        let measured = d.measure(LayoutConstraint::LOOSE);

        assert!(measured.width > 180.0);
        assert_eq!(measured.height, 28.0);
    }

    #[test]
    fn dropdown_open_menu_paints_in_overlay_layer() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 120.0);

        let mut encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            d.paint(&mut ctx);
        }
        assert_eq!(encoder.rects.len(), 1);
        assert_eq!(encoder.triangles, 1);

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint_overlay(&mut ctx);
        assert!(
            encoder.rects.len() > 1,
            "open dropdown should draw menu chrome during overlay paint"
        );
    }

    #[test]
    fn dropdown_menu_flips_above_bottom_viewport_and_keeps_hit_testing() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut d = Dropdown::new(
            "Mode",
            vec![
                MenuItem::new("Alpha", Action::Play),
                MenuItem::new("Beta", Action::Pause),
            ],
        );
        d.layout(Rect::new(20.0, 110.0, 120.0, 28.0));
        d.open_menu(&mut ctx);

        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut paint_ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 180.0, 150.0),
        };
        d.paint_overlay(&mut paint_ctx);

        let menu = d.menu_rect();
        assert!(menu.y < d.trigger_rect().y);
        assert!(menu.y + menu.height <= 146.0);

        let beta = d.item_rect(1).center();
        assert_eq!(
            d.event(
                &UiEvent::MouseDown {
                    position: beta,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            d.event(
                &UiEvent::MouseUp {
                    position: beta,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::Pause]);
    }

    #[test]
    fn anchored_menu_rect_ignores_invalid_viewports() {
        let anchor = Rect::new(20.0, 30.0, 120.0, 28.0);
        let expected = Rect::new(20.0, 58.0, 160.0, 80.0);

        for viewport in [
            Rect::new(f32::NAN, 0.0, 240.0, 180.0),
            Rect::new(0.0, f32::INFINITY, 240.0, 180.0),
            Rect::new(0.0, 0.0, 0.0, 180.0),
            Rect::new(0.0, 0.0, 240.0, -1.0),
        ] {
            assert_eq!(
                anchored_menu_rect(anchor, 160.0, 80.0, 0.0, Some(viewport)),
                expected
            );
        }
    }

    #[test]
    fn dropdown_overlay_skips_paint_when_clip_is_invalid_or_empty() {
        for clip_rect in [
            Rect::new(0.0, 0.0, 0.0, 120.0),
            Rect::new(0.0, 0.0, f32::INFINITY, 120.0),
        ] {
            let mut d = Dropdown::new(
                "File",
                vec![MenuItem::new("Open", Action::OpenProject("".into()))],
            );
            d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
            d.open = true;
            let theme = mondrian_ui_theme::ThemePreset::Dark.build();
            let mut encoder = RecordingEncoder::default();

            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            d.paint_overlay(&mut ctx);

            assert!(encoder.rects.is_empty());
            assert!(encoder.clips.is_empty());
            assert!(encoder.texts.is_empty());
            assert_eq!(d.overlay_viewport.get(), None);
        }
    }

    #[test]
    fn menu_popup_shadow_uses_analytic_theme_shadow_tokens() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 120.0);
        let mut encoder = RecordingEncoder::default();
        let popup = Rect::new(20.0, 30.0, 120.0, 64.0);

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_popup_chrome(&mut ctx, popup);

        assert_eq!(encoder.soft_shadows.len(), 3);
        assert_eq!(encoder.soft_shadows[0].bounds, popup);
        assert_eq!(
            encoder.soft_shadows[0].blur_radius,
            theme.spacing.shadow_sm.blur
        );
        assert_eq!(
            encoder.soft_shadows[1].blur_radius,
            theme.spacing.shadow_md.blur
        );
        assert_eq!(
            encoder.soft_shadows[2].blur_radius,
            theme.spacing.shadow_xl.blur
        );
        assert_eq!(encoder.rects[0], popup);
        assert_eq!(encoder.rect_colors[0], theme.colors.border_strong);
        assert_eq!(
            encoder.rect_radii[0],
            MenuVisualTokens::from_theme(&theme).popup_radius
        );
    }

    #[test]
    fn dropdown_disabled_item_paints_text_without_strikethrough() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Disabled", Action::CloseProject).disabled(),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 120.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint_overlay(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "Disabled"));
        assert_eq!(encoder.lines, 0);
    }

    #[test]
    fn menu_row_clips_label_to_padded_text_lane() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 120.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_row(
            &mut ctx,
            Rect::new(10.0, 20.0, 64.0, 24.0),
            "Disabled option with a very long label",
            None,
            None,
            false,
            MenuRowPaint { enabled: false, active: false, hovered: false },
        );

        assert_eq!(
            encoder.texts,
            vec!["Disabled option with a very long label"]
        );
        assert_eq!(encoder.clips, vec![Rect::new(20.0, 20.0, 44.0, 24.0)]);
        assert_eq!(encoder.clip_pops, 1);
        assert_eq!(encoder.lines, 0);
        assert_eq!(
            encoder.rect_radii[0],
            MenuVisualTokens::from_theme(&theme).row_radius
        );
    }

    #[test]
    fn menu_row_paints_optional_icon_and_aligns_label_after_icon_lane() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 180.0, 80.0);
        let mut encoder = RecordingEncoder::default();
        let icon = test_icon();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_row(
            &mut ctx,
            Rect::new(10.0, 20.0, 128.0, 24.0),
            "Open",
            None,
            Some(&icon),
            true,
            MenuRowPaint { enabled: true, active: false, hovered: false },
        );

        assert_eq!(encoder.texts, vec!["Open"]);
        assert_eq!(encoder.triangles + encoder.raster_images, 1);
        assert!(icon.triangle_count() > 0);
        assert_eq!(encoder.clips, vec![Rect::new(43.0, 20.0, 85.0, 24.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }

    #[test]
    fn menu_row_checked_state_paints_checkmark_without_accent_selection_chrome() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 180.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_row(
            &mut ctx,
            Rect::new(10.0, 20.0, 128.0, 28.0),
            "Inspector",
            Some("Ctrl+Alt+I"),
            None,
            true,
            MenuRowPaint { enabled: true, active: true, hovered: false },
        );

        assert_eq!(encoder.lines, 2);
        assert_eq!(encoder.rects[0], Rect::new(10.0, 20.0, 128.0, 28.0));
        assert_eq!(encoder.rect_colors[0], theme.colors.popover);
        assert!(
            !encoder.rect_colors.contains(&theme.colors.primary),
            "checked rows should not paint blue row chrome"
        );
    }

    #[test]
    fn menu_row_paints_shortcut_in_trailing_lane() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 220.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_row(
            &mut ctx,
            Rect::new(10.0, 20.0, 160.0, 24.0),
            "Save Project With Long Label",
            Some("Ctrl+S"),
            None,
            false,
            MenuRowPaint { enabled: true, active: false, hovered: false },
        );

        assert_eq!(
            encoder.texts,
            vec!["Save Project With Long Label", "Ctrl+S"]
        );
        assert_eq!(encoder.clips.len(), 2);
        assert!(encoder.clips[0].x < encoder.clips[1].x);
        assert_eq!(encoder.clip_pops, 2);
    }

    #[test]
    fn dropdown_popup_width_reserves_icon_lane_when_items_have_icons() {
        let label = "Compact menu item with enough text";
        let mut plain = Dropdown::new(
            "File",
            vec![MenuItem::new(label, Action::OpenProject("".into()))],
        );
        let mut iconized = Dropdown::new(
            "File",
            vec![MenuItem::new(label, Action::OpenProject("".into())).with_icon(test_icon())],
        );
        plain.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        iconized.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        assert!(iconized.menu_rect().width > plain.menu_rect().width);
    }

    #[test]
    fn dropdown_popup_width_reserves_shortcut_lane() {
        let label = "Save Project With Media Cache";
        let mut plain = Dropdown::new("File", vec![MenuItem::new(label, Action::SaveProject)]);
        let mut with_shortcut = Dropdown::new(
            "File",
            vec![MenuItem::new(label, Action::SaveProject).with_shortcut("Ctrl+S")],
        );
        plain.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        with_shortcut.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        assert!(with_shortcut.menu_rect().width > plain.menu_rect().width);
    }

    #[test]
    fn menu_item_shortcut_builder_preserves_action_semantics() {
        let item = MenuItem::new("Save", Action::SaveProject).with_shortcut("Ctrl+S");

        assert_eq!(item.action, Action::SaveProject);
        assert_eq!(item.shortcut.as_deref(), Some("Ctrl+S"));
        assert!(item.is_activatable());
    }

    #[test]
    fn dropdown_separator_paints_geometry_not_text() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::separator(),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 140.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint_overlay(&mut ctx);

        assert_eq!(encoder.texts, vec!["Open".to_string(), "Save".to_string()]);
        assert!(
            encoder.rects.iter().any(|rect| rect.height == 1.0),
            "separator should be drawn as a geometric divider"
        );
    }
}
