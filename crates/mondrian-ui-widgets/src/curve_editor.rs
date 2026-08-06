//! Normalized curve editor primitive.
//!
//! This widget owns screen-space interaction for editable 0..1 curve points.
//! Domain layers map keyframes, effect curves, or tone curves into this compact
//! representation and commit mutations outside the widget.

mod model;

use model::{constrained_point, normalize_points, CurveGeometry};
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::Theme;

use crate::paint::{color_with_alpha, paint_focus_ring};

const DEFAULT_WIDTH: f32 = 220.0;
const DEFAULT_HEIGHT: f32 = 104.0;
const PADDING: f32 = 10.0;
const HIT_RADIUS: f32 = 8.0;
const POINT_RADIUS: f32 = 4.0;
const SELECTED_POINT_RADIUS: f32 = 5.5;
const MIN_POINT_GAP: f32 = 0.001;

#[derive(Clone, Copy, Debug)]
struct CurveEditorVisualTokens {
    pub grid_alpha: f32,
    pub curve_alpha: f32,
    pub plot_surface_alpha: f32,
    pub point_border_alpha: f32,
    pub grid_line_width: f32,
    pub curve_line_width: f32,
    pub point_border_inset: f32,
    pub selected_inner_inset: f32,
}

impl CurveEditorVisualTokens {
    fn from_theme(_theme: &Theme) -> Self {
        Self {
            grid_alpha: 0.38,
            curve_alpha: 0.82,
            plot_surface_alpha: 0.34,
            point_border_alpha: 0.70,
            grid_line_width: 1.0,
            curve_line_width: 2.0,
            point_border_inset: -1.0,
            selected_inner_inset: 1.5,
        }
    }
}

/// A normalized editable curve point.
///
/// `x` and `y` are clamped to the inclusive `0.0..=1.0` range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurvePoint {
    /// Normalized horizontal position.
    pub x: f32,
    /// Normalized vertical value.
    pub y: f32,
}

impl CurvePoint {
    /// Create a normalized point.
    pub fn new(x: f32, y: f32) -> Self {
        Self { x: x.clamp(0.0, 1.0), y: y.clamp(0.0, 1.0) }
    }
}

/// Interaction policy for one editable curve point.
///
/// This separates domain-owned points from viewport anchors. For example, an
/// animation key at the start of a Clip remains movable and deletable, while a
/// virtual point used only to display the Clip boundary can stay fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurvePointPolicy {
    /// Keep the point's horizontal position fixed during pointer and keyboard
    /// edits.
    pub lock_x: bool,
    /// Allow Delete or Backspace to remove this point.
    pub deletable: bool,
}

impl CurvePointPolicy {
    /// Policy for a virtual viewport anchor.
    pub const fn anchor() -> Self {
        Self { lock_x: true, deletable: false }
    }

    /// Policy for a domain-owned editable point.
    pub const fn editable() -> Self {
        Self { lock_x: false, deletable: true }
    }
}

/// Adapter that maps the current curve points to an editor [`Action`].
pub type CurveChangeAction = dyn Fn(&[CurvePoint]) -> Option<Action>;

/// One committed control-point edit.
///
/// The index is ephemeral and valid only for the curve snapshot supplied to
/// this widget. Domain Adapters map it to their stable point or keyframe
/// identity before constructing an authoring action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CurveEdit {
    /// Insert a new point between existing neighbors.
    Insert { index: usize, point: CurvePoint },
    /// Move an existing point without changing its identity.
    Move { index: usize, point: CurvePoint },
    /// Delete one existing interior point.
    Delete { index: usize },
}

/// Adapter that maps one committed point edit to an editor [`Action`].
pub type CurveEditAction = dyn Fn(CurveEdit) -> Option<Action>;

/// Interactive normalized curve editor.
pub struct CurveEditor {
    id: WidgetId,
    bounds: Rect,
    points: Vec<CurvePoint>,
    point_policies: Vec<CurvePointPolicy>,
    display_points: Option<Vec<CurvePoint>>,
    selected: Option<usize>,
    dragging: Option<usize>,
    drag_origin: Option<CurvePoint>,
    drag_inserted: bool,
    grid_columns: usize,
    grid_rows: usize,
    focused: bool,
    focus_visible: bool,
    pending_capture_release: bool,
    enabled: bool,
    on_change: Option<Box<CurveChangeAction>>,
    on_edit: Option<Box<CurveEditAction>>,
}

impl CurveEditor {
    /// Create a curve editor with a simple ease-like default curve.
    pub fn new() -> Self {
        Self::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.35, 0.62),
            CurvePoint::new(0.7, 0.44),
            CurvePoint::new(1.0, 1.0),
        ])
    }

    /// Create a curve editor from normalized points.
    pub fn with_points(points: Vec<CurvePoint>) -> Self {
        let mut editor = Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            points: Vec::new(),
            point_policies: Vec::new(),
            display_points: None,
            selected: None,
            dragging: None,
            drag_origin: None,
            drag_inserted: false,
            grid_columns: 4,
            grid_rows: 3,
            focused: false,
            focus_visible: false,
            pending_capture_release: false,
            enabled: true,
            on_change: None,
            on_edit: None,
        };
        editor.set_points(points);
        editor
    }

    /// Override the interaction policy for each point.
    ///
    /// Policies must have the same length and order as [`Self::points`].
    /// A mismatched policy list is ignored, preserving the safe default of
    /// fixed, non-deletable endpoints and editable interior points.
    pub fn with_point_policies(mut self, policies: Vec<CurvePointPolicy>) -> Self {
        self.set_point_policies(policies);
        self
    }

    /// Replace the interaction policy for each point.
    ///
    /// See [`Self::with_point_policies`] for mismatch behavior.
    pub fn set_point_policies(&mut self, policies: Vec<CurvePointPolicy>) {
        if policies.len() == self.points.len() {
            self.point_policies = policies;
        }
    }

    /// Dispatch a value-aware action whenever user input changes curve points.
    pub fn on_change<F, R>(mut self, action: F) -> Self
    where
        F: Fn(&[CurvePoint]) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_change = Some(Box::new(move |points| action(points).into()));
        self
    }

    /// Dispatch one incremental edit after a pointer gesture commits.
    ///
    /// Unlike [`Self::on_change`], pointer drags emit exactly once on release.
    /// Keyboard nudges and deletion are already atomic gestures and emit
    /// immediately.
    pub fn on_edit<F, R>(mut self, action: F) -> Self
    where
        F: Fn(CurveEdit) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_edit = Some(Box::new(move |edit| action(edit).into()));
        self
    }

    /// Supply a read-only sampled curve used for painting.
    ///
    /// Editable points remain the hit targets. This lets a domain Adapter show
    /// Hold or Bezier evaluation without teaching the generic widget those
    /// interpolation semantics.
    pub fn with_display_points(mut self, points: Vec<CurvePoint>) -> Self {
        self.set_display_points(points);
        self
    }

    /// Replace the read-only sampled curve used for painting.
    pub fn set_display_points(&mut self, points: Vec<CurvePoint>) {
        self.display_points = Some(normalize_points(points));
    }

    /// Paint through the editable points again.
    pub fn clear_display_points(&mut self) {
        self.display_points = None;
    }

    /// Set whether the editor accepts pointer, keyboard, and focus input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// Disable pointer, keyboard, and focus input.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the editor accepts user input.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set whether the editor accepts pointer, keyboard, and focus input.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            if self.dragging.is_some() {
                self.pending_capture_release = true;
            }
            self.cancel_drag_state();
            self.focused = false;
            self.focus_visible = false;
        }
    }

    /// Current curve points in monotonic-x order.
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// Replace curve points. Points are clamped and sorted by x.
    pub fn set_points(&mut self, points: Vec<CurvePoint>) {
        self.points = normalize_points(points);
        self.point_policies = default_point_policies(self.points.len());
        self.selected = self.selected.filter(|index| *index < self.points.len());
        self.dragging = self.dragging.filter(|index| *index < self.points.len());
        if self.dragging.is_none() {
            self.drag_origin = None;
            self.drag_inserted = false;
        }
    }

    /// Selected point index, if any.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Select a point by index.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index.filter(|index| *index < self.points.len());
    }

    fn geometry(&self) -> CurveGeometry {
        CurveGeometry::new(self.bounds)
    }

    fn plot_rect(&self) -> Rect {
        self.geometry().plot_rect()
    }

    fn to_screen(&self, point: CurvePoint) -> Point {
        self.geometry().curve_to_screen(point)
    }

    fn screen_to_curve(&self, point: Point) -> CurvePoint {
        self.geometry().screen_to_curve(point)
    }

    fn hit_point(&self, position: Point) -> Option<usize> {
        self.geometry().hit_point(&self.points, position)
    }

    fn constrained_point(&self, index: usize, point: CurvePoint) -> CurvePoint {
        let lock_x = self.point_policies.get(index).is_none_or(|policy| policy.lock_x);
        constrained_point(&self.points, index, point, lock_x)
    }

    fn move_point(&mut self, index: usize, point: CurvePoint) -> bool {
        let Some(current) = self.points.get(index).copied() else {
            return false;
        };
        let next = self.constrained_point(index, point);
        if current == next {
            return false;
        }
        self.points[index] = next;
        true
    }

    fn move_point_from_input(
        &mut self,
        index: usize,
        point: CurvePoint,
        ctx: &mut EventContext,
    ) -> bool {
        if !self.move_point(index, point) {
            return false;
        }
        self.dispatch_change(ctx);
        ctx.request_repaint();
        true
    }

    fn dispatch_change(&self, ctx: &mut EventContext) {
        if let Some(action) = &self.on_change
            && let Some(action) = action(&self.points)
        {
            (ctx.dispatch)(action);
        }
    }

    fn dispatch_edit(&self, edit: CurveEdit, ctx: &mut EventContext) {
        if let Some(action) = &self.on_edit
            && let Some(action) = action(edit)
        {
            (ctx.dispatch)(action);
        }
    }

    fn begin_drag(&mut self, index: usize, inserted: bool) {
        self.dragging = Some(index);
        self.drag_origin = (!inserted).then(|| self.points[index]);
        self.drag_inserted = inserted;
    }

    fn commit_drag_edit(&mut self, ctx: &mut EventContext) {
        let Some(index) = self.dragging.take() else {
            return;
        };
        let point = self.points.get(index).copied();
        if self.drag_inserted {
            if let Some(point) = point {
                self.dispatch_edit(CurveEdit::Insert { index, point }, ctx);
            }
        } else if let (Some(origin), Some(point)) = (self.drag_origin, point)
            && origin != point
        {
            self.dispatch_edit(CurveEdit::Move { index, point }, ctx);
        }
        self.drag_origin = None;
        self.drag_inserted = false;
    }

    fn cancel_drag_state(&mut self) -> bool {
        let Some(index) = self.dragging.take() else {
            return false;
        };
        let changed = if self.drag_inserted {
            if index < self.points.len() {
                self.points.remove(index);
                self.point_policies.remove(index);
                true
            } else {
                false
            }
        } else if let Some(origin) = self.drag_origin {
            self.points
                .get_mut(index)
                .map(|point| {
                    let changed = *point != origin;
                    *point = origin;
                    changed
                })
                .unwrap_or(false)
        } else {
            false
        };
        self.drag_origin = None;
        self.drag_inserted = false;
        changed
    }

    fn nudge_selected(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> bool {
        if modifiers.ctrl || modifiers.alt || modifiers.meta {
            return false;
        }
        let Some(index) = self.selected else {
            return false;
        };
        let Some(point) = self.points.get(index).copied() else {
            return false;
        };
        let step = if modifiers.shift { 0.05 } else { 0.01 };
        let mut next = point;
        match key {
            KeyCode::Left => next.x -= step,
            KeyCode::Right => next.x += step,
            KeyCode::Up => next.y += step,
            KeyCode::Down => next.y -= step,
            _ => return false,
        }
        if !self.move_point_from_input(index, next, ctx) {
            return false;
        }
        let point = self.points[index];
        self.dispatch_edit(CurveEdit::Move { index, point }, ctx);
        true
    }

    fn insertion_at(&self, position: Point) -> Option<(usize, CurvePoint)> {
        self.geometry().insertion_at(&self.points, position)
    }

    fn insert_point_from_input(
        &mut self,
        position: Point,
        ctx: &mut EventContext,
    ) -> Option<usize> {
        let (index, point) = self.insertion_at(position)?;
        self.points.insert(index, point);
        self.point_policies.insert(index, CurvePointPolicy::editable());
        self.selected = Some(index);
        self.dispatch_change(ctx);
        ctx.request_repaint();
        Some(index)
    }

    fn delete_selected_from_input(&mut self, ctx: &mut EventContext) -> bool {
        let Some(index) = self.selected else {
            return false;
        };
        if !self.point_policies.get(index).is_some_and(|policy| policy.deletable) {
            return false;
        }

        self.points.remove(index);
        self.point_policies.remove(index);
        self.selected = (!self.points.is_empty()).then(|| index.min(self.points.len() - 1));
        self.dispatch_change(ctx);
        self.dispatch_edit(CurveEdit::Delete { index }, ctx);
        ctx.request_repaint();
        true
    }
}

fn default_point_policies(point_count: usize) -> Vec<CurvePointPolicy> {
    (0..point_count)
        .map(|index| {
            if index == 0 || index + 1 == point_count {
                CurvePointPolicy::anchor()
            } else {
                CurvePointPolicy::editable()
            }
        })
        .collect()
}

impl Default for CurveEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for CurveEditor {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.pending_capture_release || (!self.enabled && self.dragging.is_some()) {
            ctx.release_pointer_capture(self.id);
            self.pending_capture_release = false;
        }
        if !self.enabled {
            self.focused = false;
            self.focus_visible = false;
            self.dragging = None;
            return EventResult::Ignored;
        }

        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.focus_visible = false;
                if let Some(index) = self.hit_point(*position) {
                    self.selected = Some(index);
                    self.begin_drag(index, false);
                    ctx.request_pointer_capture(self.id);
                    return EventResult::Handled;
                }
                if let Some(index) = self.insert_point_from_input(*position, ctx) {
                    self.begin_drag(index, true);
                    ctx.request_pointer_capture(self.id);
                    return EventResult::Handled;
                }
                self.selected = None;
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                if let Some(index) = self.dragging {
                    self.move_point_from_input(index, self.screen_to_curve(*position), ctx);
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.dragging.is_some() => {
                self.commit_drag_edit(ctx);
                ctx.release_pointer_capture(self.id);
                EventResult::Handled
            }
            UiEvent::FocusGained { source } => {
                self.focused = true;
                self.focus_visible = source.is_focus_visible();
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                let was_dragging = self.dragging.is_some();
                self.commit_drag_edit(ctx);
                self.focused = false;
                self.focus_visible = false;
                self.selected = None;
                if was_dragging {
                    ctx.release_pointer_capture(self.id);
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } if self.selected.is_some() => {
                let changed = self.cancel_drag_state();
                self.selected = None;
                if changed {
                    self.dispatch_change(ctx);
                    ctx.request_repaint();
                    ctx.release_pointer_capture(self.id);
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Delete | KeyCode::Backspace, .. }
                if self.delete_selected_from_input(ctx) =>
            {
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers } if self.nudge_selected(*key, *modifiers, ctx) => {
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let v = CurveEditorVisualTokens::from_theme(ctx.theme);
        let plot = self.plot_rect();
        let grid = color_with_alpha(colors.border, v.grid_alpha);
        let curve = if self.enabled {
            color_with_alpha(colors.foreground, v.curve_alpha)
        } else {
            colors.muted_foreground
        };
        let point_fill = colors.surface;

        ctx.encoder.draw_rect(self.bounds, colors.card, spacing.radius_md);
        ctx.encoder.draw_rect(
            plot,
            color_with_alpha(colors.surface, v.plot_surface_alpha),
            spacing.radius_sm,
        );

        if self.focus_visible {
            paint_focus_ring(ctx, self.bounds, spacing.radius_md);
        }

        for col in 0..=self.grid_columns {
            let x = plot.x + plot.width * col as f32 / self.grid_columns.max(1) as f32;
            ctx.encoder.draw_line(
                Point::new(x, plot.y),
                Point::new(x, plot.y + plot.height),
                v.grid_line_width,
                grid,
            );
        }
        for row in 0..=self.grid_rows {
            let y = plot.y + plot.height * row as f32 / self.grid_rows.max(1) as f32;
            ctx.encoder.draw_line(
                Point::new(plot.x, y),
                Point::new(plot.x + plot.width, y),
                v.grid_line_width,
                grid,
            );
        }

        let painted_curve = self.display_points.as_deref().unwrap_or(&self.points);
        for pair in painted_curve.windows(2) {
            let a = self.to_screen(pair[0]);
            let b = self.to_screen(pair[1]);
            ctx.encoder.draw_line(a, b, v.curve_line_width, curve);
        }

        for (index, point) in self.points.iter().enumerate() {
            let selected = self.selected == Some(index);
            let radius = if selected {
                SELECTED_POINT_RADIUS
            } else {
                POINT_RADIUS
            };
            let center = self.to_screen(*point);
            let rect = Rect::new(
                center.x - radius,
                center.y - radius,
                radius * 2.0,
                radius * 2.0,
            );
            ctx.encoder.draw_rect(
                rect.inset(v.point_border_inset, v.point_border_inset),
                color_with_alpha(colors.border_strong, v.point_border_alpha),
                radius + 1.0,
            );
            ctx.encoder.draw_rect(rect, point_fill, radius);
            if selected {
                ctx.encoder.draw_rect(
                    rect.inset(v.selected_inner_inset, v.selected_inner_inset),
                    curve,
                    (radius - v.selected_inner_inset).max(0.0),
                );
            }
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
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
    struct RecordingEncoder {
        rects: Vec<Rect>,
        lines: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }

        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn event_ctx() -> EventContext<'static> {
        let f: &'static mut DummyFocus = Box::leak(Box::new(DummyFocus));
        let s: &'static mut DummyShortcut = Box::leak(Box::new(DummyShortcut));
        let t: &'static mut DummyTooltip = Box::leak(Box::new(DummyTooltip));
        make_event_ctx(f, s, t, &|_| {})
    }

    fn curve_action(points: &[CurvePoint]) -> Action {
        let mut name = String::from("points");
        for point in points {
            name.push_str(&format!(":{:.2},{:.2}", point.x, point.y));
        }
        Action::Custom {
            namespace: "test.curve".into(),
            name,
            payload: Default::default(),
        }
    }

    fn curve_edit_action(edit: CurveEdit) -> Action {
        let name = match edit {
            CurveEdit::Insert { index, point } => {
                format!("insert:{index}:{:.4},{:.4}", point.x, point.y)
            }
            CurveEdit::Move { index, point } => {
                format!("move:{index}:{:.4},{:.4}", point.x, point.y)
            }
            CurveEdit::Delete { index } => format!("delete:{index}"),
        };
        Action::Custom {
            namespace: "test.curve-edit".into(),
            name,
            payload: Default::default(),
        }
    }

    #[test]
    fn set_points_clamps_sorts_and_anchors_endpoints() {
        let editor = CurveEditor::with_points(vec![
            CurvePoint { x: 1.4, y: -1.0 },
            CurvePoint { x: 0.5, y: 0.25 },
            CurvePoint { x: -0.2, y: 2.0 },
        ]);

        assert_eq!(editor.points()[0], CurvePoint::new(0.0, 1.0));
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.25));
        assert_eq!(editor.points()[2], CurvePoint::new(1.0, 0.0));
    }

    #[test]
    fn set_points_expands_single_point_into_anchored_curve() {
        let editor = CurveEditor::with_points(vec![CurvePoint::new(0.4, 0.25)]);

        assert_eq!(
            editor.points(),
            &[CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.25)]
        );
    }

    #[test]
    fn clicking_empty_plot_inserts_and_selects_point() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));

        let result = editor.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 50.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(editor.points().len(), 3);
        assert_eq!(editor.selected_index(), Some(1));
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_action(editor.points())]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn clicking_outside_plot_padding_does_not_insert_point() {
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let mut ctx = event_ctx();

        editor.event(
            &UiEvent::MouseDown {
                position: Point::new(4.0, 4.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(editor.points().len(), 2);
    }

    #[test]
    fn dragging_point_updates_value_and_releases_capture() {
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);
        let target = Point::new(190.0, 90.0);
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Capture(
                editor.id()
            ))
        );
        editor.event(
            &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseUp {
                position: target,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert_eq!(editor.points()[1].x, 1.0);
        assert!(editor.points()[1].y < 0.2);
    }

    #[test]
    fn dragging_point_dispatches_changed_curve_and_requests_repaint() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x, start.y - 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_action(editor.points())]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn committed_drag_emits_one_incremental_edit_only_on_release() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_edit(curve_edit_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        for position in [
            Point::new(start.x + 8.0, start.y - 5.0),
            Point::new(start.x + 18.0, start.y - 12.0),
        ] {
            editor.event(
                &UiEvent::MouseMove { position, modifiers: Modifiers::none() },
                &mut ctx,
            );
        }
        assert!(actions.borrow().is_empty());
        let committed = editor.points()[1];

        editor.event(
            &UiEvent::MouseUp {
                position: editor.to_screen(committed),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_edit_action(CurveEdit::Move {
                index: 1,
                point: committed,
            })]
        );
    }

    #[test]
    fn inserted_point_emits_one_incremental_edit_on_release() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                .on_edit(curve_edit_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let position = Point::new(100.0, 50.0);

        editor.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(actions.borrow().is_empty());
        let inserted = editor.points()[1];
        editor.event(
            &UiEvent::MouseUp {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_edit_action(CurveEdit::Insert {
                index: 1,
                point: inserted,
            })]
        );
    }

    #[test]
    fn escape_reverts_drag_without_committing_incremental_edit() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let original = CurvePoint::new(0.5, 0.5);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            original,
            CurvePoint::new(1.0, 1.0),
        ])
        .on_edit(curve_edit_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(original);

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x + 20.0, start.y - 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_ne!(editor.points()[1], original);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(editor.points()[1], original);
        assert!(actions.borrow().is_empty());
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
    }

    #[test]
    fn dragging_point_to_same_position_does_not_dispatch_duplicate_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseMove { position: start, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn interior_point_cannot_cross_neighbors() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);

        assert!(editor.move_point(1, CurvePoint::new(1.2, 0.75)));
        assert!(editor.points()[1].x < editor.points()[2].x);
        assert!(editor.points()[1].x > editor.points()[0].x);
    }

    #[test]
    fn keyboard_nudges_selected_point() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.select(Some(1));
        let mut ctx = event_ctx();

        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );

        assert!(editor.points()[1].y > 0.54);
    }

    #[test]
    fn keyboard_nudge_dispatches_changed_curve() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.select(Some(1));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_action(editor.points())]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn keyboard_nudge_ignores_ctrl_alt_and_meta_chords() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.select(Some(1));

        for (key, modifiers) in [
            (KeyCode::Right, Modifiers::ctrl()),
            (KeyCode::Left, Modifiers { alt: true, ..Default::default() }),
            (KeyCode::Up, Modifiers { meta: true, ..Default::default() }),
            (
                KeyCode::Down,
                Modifiers { ctrl: true, shift: true, ..Default::default() },
            ),
        ] {
            assert_eq!(
                editor.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        }

        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn disabled_editor_ignores_pointer_keyboard_and_focus() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action)
        .disabled();
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        assert!(!editor.is_enabled());
        assert!(!editor.can_focus());
        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        assert!(actions.borrow().is_empty());
        assert!(ctx.requests.pointer_capture.is_none());
    }

    #[test]
    fn disabled_focused_editor_does_not_release_unowned_capture() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.focused = true;
        editor.set_enabled(false);
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(100.0, 50.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(ctx.requests.pointer_capture, None);
        assert!(!editor.focused);
        assert!(editor.dragging.is_none());
    }

    #[test]
    fn disabling_while_dragging_releases_capture_on_next_event() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        editor.set_enabled(false);
        assert!(editor.dragging.is_none());

        assert_eq!(
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + 10.0, start.y),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
    }

    #[test]
    fn disabling_and_reenabling_cancels_pending_curve_drag() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(editor.dragging, Some(1));
        ctx.requests.pointer_capture = None;

        editor.set_enabled(false);
        assert!(editor.dragging.is_none());
        editor.set_enabled(true);

        assert_eq!(
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + 30.0, start.y - 20.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn focus_lost_without_drag_does_not_release_pointer_capture() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.select(Some(1));
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            editor.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );

        assert_eq!(ctx.requests.pointer_capture, None);
        assert!(!editor.focused);
        assert_eq!(editor.selected_index(), None);
    }

    #[test]
    fn focus_lost_during_drag_releases_pointer_capture() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);
        let mut ctx = event_ctx();

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        ctx.requests.pointer_capture = None;

        assert_eq!(
            editor.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert!(editor.dragging.is_none());
    }

    #[test]
    fn keyboard_nudge_at_endpoint_boundary_does_not_dispatch() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                .on_change(curve_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn delete_removes_selected_interior_point_and_dispatches_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.select(Some(1));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            editor.points(),
            &[CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_action(editor.points())]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn delete_does_not_remove_endpoints() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                .on_change(curve_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown {
                key: KeyCode::Backspace,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(editor.points().len(), 2);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn explicit_editable_boundary_point_can_move_and_emits_incremental_edit() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.75)])
                .with_point_policies(vec![
                    CurvePointPolicy::editable(),
                    CurvePointPolicy::anchor(),
                ])
                .on_edit(curve_edit_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(editor.points()[0], CurvePoint::new(0.01, 0.25));
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_edit_action(CurveEdit::Move {
                index: 0,
                point: CurvePoint::new(0.01, 0.25),
            })]
        );
    }

    #[test]
    fn explicit_editable_boundary_point_can_be_deleted() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.75)])
                .with_point_policies(vec![
                    CurvePointPolicy::editable(),
                    CurvePointPolicy::anchor(),
                ])
                .on_edit(curve_edit_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(editor.points(), &[CurvePoint::new(1.0, 0.75)]);
        assert_eq!(editor.selected_index(), Some(0));
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_edit_action(CurveEdit::Delete { index: 0 })]
        );
    }

    #[test]
    fn mismatched_point_policies_preserve_safe_endpoint_defaults() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.75)])
                .with_point_policies(vec![CurvePointPolicy::editable()])
                .on_edit(curve_edit_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(editor.points().len(), 2);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn paint_draws_grid_curve_and_points() {
        let mut editor = CurveEditor::new();
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 220.0, 104.0),
        };

        editor.paint(&mut ctx);

        assert!(encoder.rects.len() >= 2 + editor.points().len() * 2);
        assert!(encoder.lines >= editor.grid_columns + editor.grid_rows + editor.points().len());
    }
}
