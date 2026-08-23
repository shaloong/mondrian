//! Segmented button group widget.
//!
//! A compact mutually-exclusive button group used for preference pickers and
//! mode switches. The group owns the selected pill geometry, so selected,
//! hover, and focus visuals stay aligned across all segments.

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::{Theme, ThemePreset};
use std::cell::Cell;

use crate::paint::{centered_text_origin_y, color_with_alpha, paint_focus_ring};
use crate::text_metrics::{centered_text_x, measure_single_line};

/// One segment in a [`SegmentedButtonGroup`].
#[derive(Clone)]
pub struct SegmentedButtonItem {
    label: String,
    action: Action,
}

impl SegmentedButtonItem {
    /// Build a segment with a user-facing label and dispatched action.
    pub fn new(label: impl Into<String>, action: Action) -> Self {
        Self { label: label.into(), action }
    }

    /// Segment label.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Compact mutually-exclusive segmented control.
pub struct SegmentedButtonGroup {
    id: WidgetId,
    items: Vec<SegmentedButtonItem>,
    selected_index: usize,
    hovered_index: Option<usize>,
    pressed_index: Option<usize>,
    bounds: Rect,
    enabled: bool,
    focused: bool,
    focus_visible: bool,
    visual: Cell<SegmentedButtonVisualTokens>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SegmentedButtonVisualTokens {
    height: f32,
    padding_x: f32,
    font_size: f32,
    radius: f32,
    border_width: f32,
}

impl SegmentedButtonVisualTokens {
    fn from_theme(theme: &Theme) -> Self {
        Self {
            height: theme.spacing.interact_height,
            padding_x: theme.spacing.md,
            font_size: theme.typography.button.font_size,
            radius: theme.spacing.radius_md,
            border_width: theme.spacing.border_standard,
        }
    }
}

impl Default for SegmentedButtonVisualTokens {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

impl SegmentedButtonGroup {
    /// Build a segmented group. Empty groups are allowed but inert.
    pub fn new(items: Vec<SegmentedButtonItem>, selected_index: usize) -> Self {
        let selected_index = selected_index.min(items.len().saturating_sub(1));
        Self {
            id: WidgetId::new(),
            items,
            selected_index,
            hovered_index: None,
            pressed_index: None,
            bounds: Rect::ZERO,
            enabled: true,
            focused: false,
            focus_visible: false,
            visual: Cell::new(SegmentedButtonVisualTokens::default()),
        }
    }

    /// Set the currently selected segment.
    pub fn set_selected_index(&mut self, index: usize) {
        self.selected_index = index.min(self.items.len().saturating_sub(1));
    }

    /// Currently selected segment.
    pub fn selected_index(&self) -> Option<usize> {
        (!self.items.is_empty()).then_some(self.selected_index)
    }

    /// Segment rect for tests and parent hit affordances.
    pub fn segment_rect(&self, index: usize) -> Rect {
        if self.items.is_empty() || index >= self.items.len() {
            return Rect::ZERO;
        }
        let width = self.bounds.width / self.items.len() as f32;
        Rect::new(
            self.bounds.x + index as f32 * width,
            self.bounds.y,
            width,
            self.bounds.height,
        )
    }

    /// Whether the group accepts input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.hovered_index = None;
            self.pressed_index = None;
            self.focused = false;
            self.focus_visible = false;
        }
        self
    }

    fn index_at(&self, point: Point) -> Option<usize> {
        if self.items.is_empty() || !self.bounds.contains(point) {
            return None;
        }
        let width = self.bounds.width / self.items.len() as f32;
        let index = ((point.x - self.bounds.x) / width).floor() as usize;
        Some(index.min(self.items.len() - 1))
    }

    fn clear_visual_state(&mut self) -> bool {
        let changed = self.hovered_index.is_some()
            || self.pressed_index.is_some()
            || self.focused
            || self.focus_visible;
        self.hovered_index = None;
        self.pressed_index = None;
        self.focused = false;
        self.focus_visible = false;
        changed
    }
}

impl Widget for SegmentedButtonGroup {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let visual = self.visual.get();
        let label_width = self
            .items
            .iter()
            .map(|item| measure_single_line(&item.label, visual.font_size).0)
            .fold(0.0, f32::max);
        constraint.constrain(Size::new(
            (label_width + visual.padding_x * 2.0) * self.items.len() as f32,
            visual.height,
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.clear_visual_state() {
                ctx.request_repaint();
            }
            return EventResult::Ignored;
        }
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if let Some(index) = self.index_at(*position) {
                    self.pressed_index = Some(index);
                    self.focus_visible = false;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if let Some(pressed) = self.pressed_index.take() {
                    if self.index_at(*position) == Some(pressed) {
                        self.selected_index = pressed;
                        if let Some(item) = self.items.get(pressed) {
                            (ctx.dispatch)(item.action.clone());
                        }
                    }
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseMove { position, .. } => {
                let next = self.index_at(*position);
                if next != self.hovered_index {
                    self.hovered_index = next;
                    ctx.request_repaint();
                }
                EventResult::Ignored
            }
            UiEvent::FocusGained { source } => {
                self.focused = true;
                self.focus_visible = source.is_focus_visible();
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                if self.clear_visual_state() {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Left, modifiers }
            | UiEvent::KeyDown { key: KeyCode::Up, modifiers }
                if *modifiers == Modifiers::none() && !self.items.is_empty() =>
            {
                let next = self.selected_index.saturating_sub(1);
                self.selected_index = next;
                (ctx.dispatch)(self.items[next].action.clone());
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Right, modifiers }
            | UiEvent::KeyDown { key: KeyCode::Down, modifiers }
                if *modifiers == Modifiers::none() && !self.items.is_empty() =>
            {
                let next = (self.selected_index + 1).min(self.items.len() - 1);
                self.selected_index = next;
                (ctx.dispatch)(self.items[next].action.clone());
                ctx.request_repaint();
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.visual.set(SegmentedButtonVisualTokens::from_theme(ctx.theme));
        let visual = self.visual.get();
        let colors = &ctx.theme.colors;
        let radius = visual.radius;

        ctx.encoder
            .draw_rect(self.bounds, color_with_alpha(colors.border, 0.50), radius);
        ctx.encoder.draw_rect(
            self.bounds.inset(visual.border_width, visual.border_width),
            color_with_alpha(colors.popover, 0.94),
            (radius - visual.border_width).max(0.0),
        );

        if !self.items.is_empty() {
            let selected = self.segment_rect(self.selected_index).inset(2.0, 2.0);
            ctx.encoder.draw_rect(
                selected,
                color_with_alpha(colors.primary, 0.86),
                radius - 2.0,
            );
            ctx.encoder.draw_rect(
                selected.inset(visual.border_width, visual.border_width),
                color_with_alpha(colors.primary, 0.72),
                (radius - 2.0 - visual.border_width).max(0.0),
            );
        }

        if let Some(hovered) = self.hovered_index
            && hovered != self.selected_index
        {
            ctx.encoder.draw_rect(
                self.segment_rect(hovered).inset(2.0, 2.0),
                color_with_alpha(colors.foreground, 0.06),
                radius - 2.0,
            );
        }

        for index in 1..self.items.len() {
            let x = self.segment_rect(index).x;
            ctx.encoder.draw_line(
                Point::new(x, self.bounds.y + 6.0),
                Point::new(x, self.bounds.y + self.bounds.height - 6.0),
                visual.border_width,
                color_with_alpha(colors.border, 0.55),
            );
        }

        for (index, item) in self.items.iter().enumerate() {
            let rect = self.segment_rect(index).inset(visual.padding_x, 0.0);
            let text_color = if self.enabled {
                if index == self.selected_index {
                    colors.primary_foreground
                } else {
                    colors.text_secondary
                }
            } else {
                colors.muted_foreground
            };
            let text_x = centered_text_x(rect.x, rect.width, &item.label, visual.font_size);
            let text_y = centered_text_origin_y(rect, ctx.theme.typography.button.line_height);
            ctx.push_clip(rect);
            ctx.encoder.draw_text(
                &item.label,
                visual.font_size,
                Point::new(text_x, text_y),
                text_color,
            );
            ctx.pop_clip();
        }

        if self.focus_visible {
            paint_focus_ring(ctx, self.bounds, radius);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled && !self.items.is_empty()
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        let name = self
            .selected_index()
            .and_then(|index| self.items.get(index))
            .map(|item| item.label.clone())
            .unwrap_or_default();
        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::Group)
                .with_name(name)
                .with_state(AccessibilityState {
                    focusable: self.enabled && !self.items.is_empty(),
                    focused: self.focused,
                    disabled: !self.enabled,
                    selected: Some(self.enabled),
                    ..AccessibilityState::default()
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::Color;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct PaintRecorder {
        rects: Vec<Rect>,
        radii: Vec<f32>,
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for PaintRecorder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, bounds: Rect, _color: Color, corner_radius: f32) {
            self.rects.push(bounds);
            self.radii.push(corner_radius);
        }
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}
        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.to_owned());
        }
        fn draw_triangles(&mut self, _vertices: &[Point], _color: Color) {}
        fn draw_raster_image(
            &mut self,
            _key: &str,
            _bounds: Rect,
            _width: u32,
            _height: u32,
            _color_space: mondrian_ui_core::RasterImageColorSpace,
            _rgba: std::sync::Arc<[u8]>,
            _tint: Color,
        ) {
        }
        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    #[test]
    fn segmented_group_dispatches_clicked_segment() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut group = SegmentedButtonGroup::new(
            vec![
                SegmentedButtonItem::new("System", Action::CloseProject),
                SegmentedButtonItem::new("Dark", Action::SaveProject),
            ],
            0,
        );
        group.layout(Rect::new(10.0, 10.0, 200.0, 28.0));
        let target = group.segment_rect(1).center();

        assert_eq!(
            group.event(
                &UiEvent::MouseDown {
                    position: target,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            group.event(
                &UiEvent::MouseUp {
                    position: target,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(group.selected_index(), Some(1));
        assert_eq!(actions.borrow().as_slice(), &[Action::SaveProject]);
    }

    #[test]
    fn segmented_group_selected_pill_uses_rounded_geometry() {
        let mut group = SegmentedButtonGroup::new(
            vec![
                SegmentedButtonItem::new("A", Action::Play),
                SegmentedButtonItem::new("B", Action::Pause),
            ],
            1,
        );
        group.layout(Rect::new(0.0, 0.0, 180.0, 28.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = PaintRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 300.0, 100.0),
        };

        group.paint(&mut ctx);

        assert!(encoder.texts.contains(&"A".to_owned()));
        assert!(encoder.texts.contains(&"B".to_owned()));
        assert!(encoder.radii.iter().any(|radius| *radius > 2.0));
    }

    #[test]
    fn segmented_group_pointer_focus_is_accessible_without_focus_ring() {
        let dispatch = |_| {};
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut group = SegmentedButtonGroup::new(
            vec![
                SegmentedButtonItem::new("System", Action::CloseProject),
                SegmentedButtonItem::new("Dark", Action::SaveProject),
            ],
            0,
        );

        assert_eq!(
            group.event(&UiEvent::focus_gained_pointer(), &mut ctx),
            EventResult::Handled
        );

        assert!(group.focused);
        assert!(!group.focus_visible);
        assert!(group.accessibility().unwrap().state.focused);
    }

    #[test]
    fn segmented_group_keyboard_focus_shows_focus_ring() {
        let dispatch = |_| {};
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut group = SegmentedButtonGroup::new(
            vec![
                SegmentedButtonItem::new("System", Action::CloseProject),
                SegmentedButtonItem::new("Dark", Action::SaveProject),
            ],
            0,
        );

        assert_eq!(
            group.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
            EventResult::Handled
        );

        assert!(group.focused);
        assert!(group.focus_visible);
        assert!(group.accessibility().unwrap().state.focused);
    }
}
