//! Slider 控件
//!
//! 拖拽滑块，用于数值调节。

mod model;

use crate::ValueEdit;
use model::{SliderGeometry, SliderModel};
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, AccessibilityValue, EventContext,
    PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::{Theme, ThemePreset};
use std::cell::Cell;

/// Adapter that maps the current slider value to an editor [`Action`].
pub type SliderChangeAction = dyn Fn(f32) -> Option<Action>;

/// Adapter for value changes and explicit pointer interaction boundaries.
pub type SliderEditAction = dyn Fn(ValueEdit, f32) -> Option<Action>;

/// Pure, fresh model projection for a slider's declared domain value.
/// This presentation adapter must not dispatch or mutate author state.
pub type SliderAccessibleValue = dyn Fn() -> AccessibilityValue;

/// Slider Widget —— 拖拽滑块控制数值
pub struct Slider {
    id: WidgetId,
    inspection_identity: crate::control_inspection::ControlInspectionIdentity,
    model: SliderModel,
    bounds: Rect,
    enabled: bool,
    dragging: bool,
    focused: bool,
    focus_visible: bool,
    geometry: Cell<SliderGeometry>,
    on_change: Option<Box<SliderChangeAction>>,
    on_edit: Option<Box<SliderEditAction>>,
    accessible_name: Option<String>,
    accessible_value: Option<Box<SliderAccessibleValue>>,
    gesture: Option<mondrian_core::AuthoringGestureId>,
    drag_start: Option<f32>,
    visual: Cell<SliderVisualTokens>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SliderVisualTokens {
    track_height: f32,
    thumb_size: f32,
    min_width: f32,
    measure_padding_y: f32,
    thumb_fit_inset: f32,
    focus_halo_inset: f32,
    focus_halo_alpha: f32,
}

impl SliderVisualTokens {
    fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            track_height: spacing.xs,
            thumb_size: (spacing.icon_size - spacing.border_emphasis).max(1.0),
            min_width: spacing.interact_height * 3.5 + spacing.border_emphasis,
            measure_padding_y: spacing.xs,
            thumb_fit_inset: spacing.border_emphasis,
            focus_halo_inset: (spacing.xs - spacing.border_standard).max(0.0),
            focus_halo_alpha: 0.38,
        }
    }

    fn geometry(self) -> SliderGeometry {
        SliderGeometry::new(
            self.track_height,
            self.thumb_size,
            self.min_width,
            self.measure_padding_y,
            self.thumb_fit_inset,
        )
    }
}

impl Default for SliderVisualTokens {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

impl Slider {
    /// Address this control independently of labels, ordering and current value.
    pub fn with_control_inspection_key(mut self, key: Option<String>) -> Self {
        self.inspection_identity.set_key(key);
        self
    }

    /// Create a slider with a clamped initial value.
    pub fn new(value: f32, min: f32, max: f32) -> Self {
        Self {
            id: WidgetId::new(),
            inspection_identity: crate::control_inspection::ControlInspectionIdentity::default(),
            model: SliderModel::new(value, min, max),
            bounds: Rect::ZERO,
            enabled: true,
            dragging: false,
            focused: false,
            focus_visible: false,
            geometry: Cell::new(SliderVisualTokens::default().geometry()),
            on_change: None,
            on_edit: None,
            accessible_name: None,
            accessible_value: None,
            gesture: None,
            drag_start: None,
            visual: Cell::new(SliderVisualTokens::default()),
        }
    }

    /// Supply the current value in the declared domain for semantic inspection.
    ///
    /// Without this adapter the slider reports its own coordinate and range.
    /// The adapter belongs to the fresh widget and is never carried across a
    /// model/owner replacement by focus or draft inspection.
    pub fn with_accessible_value(
        mut self,
        value: impl Fn() -> AccessibilityValue + 'static,
    ) -> Self {
        self.accessible_value = Some(Box::new(value));
        self
    }

    /// Return the current value.
    pub fn value(&self) -> f32 {
        self.model.value()
    }

    /// Dispatch an action whenever user input changes the value.
    pub fn on_change<F, R>(mut self, action: F) -> Self
    where
        F: Fn(f32) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_edit = None;
        self.on_change = Some(Box::new(move |value| action(value).into()));
        self
    }

    /// Dispatch explicit edit boundaries instead of isolated value callbacks.
    /// Each drag gets a fresh identity; begin precedes updates and finish is
    /// emitted once on release/focus loss. This replaces the value-only callback.
    pub fn on_edit<F, R>(mut self, action: F) -> Self
    where
        F: Fn(ValueEdit, f32) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_change = None;
        self.on_edit = Some(Box::new(move |phase, value| action(phase, value).into()));
        self
    }

    /// Quantize user-edited values to a fixed step.
    ///
    /// The current value is left unchanged when this builder is called, so
    /// snapshots from app state stay exact. Pointer and keyboard edits are
    /// snapped to `min + n * step` and clamped to the slider bounds.
    pub fn with_step(mut self, step: f32) -> Self {
        self.model.set_step(step);
        self
    }

    /// Set whether the slider accepts user input and participates in focus.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.dragging = false;
            self.focused = false;
            self.focus_visible = false;
        }
        self
    }

    /// Disable the slider.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the slider is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn range(&self) -> f32 {
        self.model.range()
    }

    fn ratio(&self) -> f32 {
        self.model.ratio()
    }

    fn track_rect(&self) -> Rect {
        self.geometry.get().track_rect(self.bounds)
    }

    fn thumb_rect(&self) -> Rect {
        self.geometry.get().thumb_rect(self.bounds, self.ratio())
    }

    /// Synchronize the coordinate from a fresh model without dispatching an edit.
    ///
    /// Existing pointer capture and the current drag origin are unchanged. This
    /// setter is presentation only; author changes still enter through callbacks.
    pub fn set_value(&mut self, value: f32) -> bool {
        self.model.set_value(value)
    }

    fn set_value_from_input(&mut self, value: f32, ctx: &mut EventContext) -> bool {
        if !self.set_value(value) {
            return false;
        }
        self.dispatch_change(ctx);
        ctx.request_repaint();
        true
    }

    fn dispatch_change(&self, ctx: &mut EventContext) {
        if self.on_edit.is_some() {
            self.dispatch_edit(
                self.gesture.map_or(ValueEdit::Single, ValueEdit::Update),
                ctx,
            );
            return;
        }
        if let Some(action) = &self.on_change
            && let Some(action) = action(self.value())
        {
            (ctx.dispatch)(action);
        }
    }

    fn dispatch_edit(&self, phase: ValueEdit, ctx: &mut EventContext) {
        if let Some(callback) = &self.on_edit
            && let Some(action) = callback(phase, self.value())
        {
            (ctx.dispatch)(action);
        }
    }

    /// Seal an active pointer edit before a distinct numeric keyboard command.
    ///
    /// This emits its existing Finish once, releases capture and retains focus.
    /// It dispatches no new value and leaves an idle Slider unchanged.
    pub fn finish_pointer_edit(&mut self, ctx: &mut EventContext) -> bool {
        if !self.dragging {
            return false;
        }
        self.finish_edit(ctx);
        self.dragging = false;
        ctx.release_pointer_capture(self.id);
        ctx.request_repaint();
        true
    }

    fn finish_edit(&mut self, ctx: &mut EventContext) {
        if let Some(gesture) = self.gesture.take() {
            self.dispatch_edit(ValueEdit::Finish(gesture), ctx);
        }
        self.drag_start = None;
    }

    fn cancel_edit(&mut self, ctx: &mut EventContext) {
        let changed = self.drag_start.take().is_some_and(|origin| self.model.restore_value(origin));
        if let Some(gesture) = self.gesture.take() {
            self.dispatch_edit(ValueEdit::Cancel(gesture), ctx);
        } else if changed {
            self.dispatch_change(ctx);
        }
        self.dragging = false;
        ctx.release_pointer_capture(self.id);
        ctx.request_repaint();
    }

    fn keyboard_step(&self, modifiers: Modifiers) -> f32 {
        self.model.keyboard_step(modifiers)
    }

    fn page_step(&self) -> f32 {
        self.model.page_step()
    }

    fn nudge(&mut self, delta: f32, ctx: &mut EventContext) -> EventResult {
        if self.range() <= 0.0 {
            return EventResult::Ignored;
        }
        if self.set_value_from_input(self.value() + delta, ctx) {
            EventResult::Handled
        } else {
            EventResult::Ignored
        }
    }
}

impl Widget for Slider {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(self.geometry.get().measure_size())
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.dragging {
                self.finish_edit(ctx);
                ctx.release_pointer_capture(self.id);
            }
            self.dragging = false;
            self.focused = false;
            self.focus_visible = false;
            return EventResult::Ignored;
        }
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.dragging = true;
                self.focused = true;
                self.focus_visible = false;
                ctx.request_pointer_capture(self.id);
                if self.on_edit.is_some() {
                    self.finish_edit(ctx);
                    self.drag_start = Some(self.value());
                    let gesture = mondrian_core::AuthoringGestureId::new();
                    self.gesture = Some(gesture);
                    let ratio = self.geometry.get().ratio_at_position(self.bounds, *position);
                    self.set_value(self.model.value_at_ratio(ratio));
                    self.dispatch_edit(ValueEdit::Begin(gesture), ctx);
                    ctx.request_repaint();
                } else {
                    self.drag_start = Some(self.value());
                    self.update_value(position, ctx);
                }
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } if self.dragging => {
                self.update_value(position, ctx);
                EventResult::Handled
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.dragging => {
                self.finish_edit(ctx);
                self.dragging = false;
                ctx.release_pointer_capture(self.id);
                EventResult::Handled
            }
            UiEvent::FocusGained { source } => {
                self.focused = true;
                self.focus_visible = source.is_focus_visible();
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                let was_dragging = self.dragging;
                self.focused = false;
                self.focus_visible = false;
                self.dragging = false;
                if was_dragging {
                    self.finish_edit(ctx);
                    ctx.release_pointer_capture(self.id);
                }
                EventResult::Handled
            }
            UiEvent::SetNumericValue(value) if self.focused && value.is_finite() => {
                if self.set_value_from_input(*value, ctx) {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                if modifiers.ctrl || modifiers.alt || modifiers.meta {
                    return EventResult::Ignored;
                }
                if self.dragging && *key == KeyCode::Escape {
                    self.cancel_edit(ctx);
                    return EventResult::Handled;
                }
                if self.dragging
                    && matches!(
                        key,
                        KeyCode::Left
                            | KeyCode::Right
                            | KeyCode::Up
                            | KeyCode::Down
                            | KeyCode::PageUp
                            | KeyCode::PageDown
                            | KeyCode::Home
                            | KeyCode::End
                    )
                {
                    self.finish_edit(ctx);
                    self.dragging = false;
                    ctx.release_pointer_capture(self.id);
                }
                let step = self.keyboard_step(*modifiers);
                match key {
                    KeyCode::Left | KeyCode::Down => self.nudge(-step, ctx),
                    KeyCode::Right | KeyCode::Up => self.nudge(step, ctx),
                    KeyCode::PageDown => self.nudge(-self.page_step(), ctx),
                    KeyCode::PageUp => self.nudge(self.page_step(), ctx),
                    KeyCode::Home => {
                        if self.set_value_from_input(self.model.min(), ctx) {
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        }
                    }
                    KeyCode::End => {
                        if self.set_value_from_input(self.model.max(), ctx) {
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        }
                    }
                    _ => EventResult::Ignored,
                }
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.visual.set(SliderVisualTokens::from_theme(ctx.theme));
        self.geometry.set(self.visual.get().geometry());
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let visual = self.visual.get();

        // Track background
        let track = self.track_rect();
        let track_color = if self.enabled {
            tokens.surface_2
        } else {
            tokens.muted
        };
        let fill_color = if self.enabled {
            tokens.foreground
        } else {
            tokens.muted_foreground
        };
        ctx.encoder.draw_rect(track, track_color, spacing.radius_full);

        // Filled track
        let ratio = self.ratio();
        let fill_w = track.width * ratio;
        if fill_w > 0.0 {
            let track_fill = Rect::new(track.x, track.y, fill_w, track.height);
            ctx.encoder.draw_rect(track_fill, fill_color, spacing.radius_full);
        }

        let thumb_rect = self.thumb_rect();
        if self.focus_visible {
            let mut ring = tokens.ring;
            ring.a = visual.focus_halo_alpha;
            let halo = Rect::new(
                thumb_rect.x - visual.focus_halo_inset,
                thumb_rect.y - visual.focus_halo_inset,
                thumb_rect.width + visual.focus_halo_inset * 2.0,
                thumb_rect.height + visual.focus_halo_inset * 2.0,
            );
            ctx.encoder.draw_rect(halo, ring, halo.height * 0.5);
        }
        ctx.encoder.draw_rect(thumb_rect, fill_color, thumb_rect.height * 0.5);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn has_active_pointer_interaction(&self) -> bool {
        self.dragging
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }

    fn set_control_inspection_key(&mut self, key: Option<String>) -> bool {
        self.inspection_identity.set_key(key);
        true
    }

    fn control_inspection_state(&self) -> Option<mondrian_ui_core::ControlInspectionState> {
        self.inspection_identity.inspect(
            self.id,
            AccessibilityRole::Slider,
            self.focused,
            self.focus_visible,
            self.enabled && !self.dragging && self.gesture.is_none(),
        )
    }

    fn restore_control_inspection_state(
        &mut self,
        state: &mondrian_ui_core::ControlInspectionState,
    ) -> bool {
        let Some((id, focused, focus_visible)) = self.inspection_identity.restore(
            state,
            AccessibilityRole::Slider,
            self.enabled && !self.dragging && self.gesture.is_none(),
        ) else {
            return false;
        };
        self.id = id;
        self.focused = focused;
        self.focus_visible = focus_visible;
        true
    }

    fn set_accessible_name(&mut self, name: Option<String>) -> bool {
        self.accessible_name = name.filter(|name| !name.is_empty());
        true
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::Slider)
                .with_name(self.accessible_name.as_deref().unwrap_or("Slider"))
                .with_state(AccessibilityState {
                    focusable: self.enabled,
                    focused: self.focused,
                    disabled: !self.enabled,
                    ..AccessibilityState::default()
                })
                .with_value(self.accessible_value.as_ref().map_or_else(
                    || AccessibilityValue::Number {
                        value: f64::from(self.value()),
                        min: f64::from(self.model.min()),
                        max: f64::from(self.model.max()),
                    },
                    |value| value(),
                )),
        )
    }

    fn accessibility_bounds(&self) -> Option<Rect> {
        Some(self.bounds)
    }
}

impl Slider {
    fn update_value(&mut self, position: &Point, ctx: &mut EventContext) {
        let range = self.range();
        if range <= 0.0 {
            return;
        }
        let ratio = self.geometry.get().ratio_at_position(self.bounds, *position);
        self.set_value_from_input(self.model.value_at_ratio(ratio), ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::{DrawCommandEncoder, PointerCaptureRequest};
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        rect_radii: Vec<f32>,
        rect_colors: Vec<mondrian_core::Color>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, color: mondrian_core::Color, corner_radius: f32) {
            self.rects.push(bounds);
            self.rect_colors.push(color);
            self.rect_radii.push(corner_radius);
        }

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
            _text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn event_ctx() -> EventContext<'static> {
        let f: &'static mut DummyFocus = Box::leak(Box::new(DummyFocus));
        let s: &'static mut DummyShortcut = Box::leak(Box::new(DummyShortcut));
        let t: &'static mut DummyTooltip = Box::leak(Box::new(DummyTooltip));
        make_event_ctx(f, s, t, &|_| {})
    }

    fn value_action(value: f32) -> Action {
        Action::Custom {
            namespace: "test.slider".into(),
            name: format!("change:{value:.1}"),
            payload: Default::default(),
        }
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 0.0001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn slider_new_value_clamped() {
        let s = Slider::new(150.0, 0.0, 100.0);
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn slider_new_value_in_range() {
        let s = Slider::new(50.0, 0.0, 100.0);
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_new_orders_inverted_range() {
        let s = Slider::new(50.0, 100.0, 0.0);

        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_new_recovers_non_finite_range_and_value() {
        let s = Slider::new(f32::NAN, f32::NAN, f32::INFINITY);

        assert_eq!(s.value(), 0.0);
    }

    #[test]
    fn slider_with_step_does_not_quantize_initial_snapshot() {
        let s = Slider::new(0.23, 0.0, 1.0).with_step(0.1);

        assert_close(s.value(), 0.23);
    }

    #[test]
    fn slider_mouse_down_starts_drag_and_updates_value() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let mut ctx = event_ctx();
        // Click at x=100 (50%)
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_mouse_input_quantizes_to_step() {
        let mut s = Slider::new(0.0, 0.0, 100.0).with_step(25.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        let track = s.track_rect();
        let mut ctx = event_ctx();

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(track.x + track.width * 0.62, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_close(s.value(), 50.0);
    }

    #[test]
    fn slider_escape_cancels_gesture_once_and_restores_unquantized_start() {
        use std::rc::Rc;
        let phases = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&phases);
        let mut slider =
            Slider::new(0.23, 0.0, 1.0).with_step(0.25).on_edit(move |phase, value| {
                recorded.borrow_mut().push((phase, value));
                None::<Action>
            });
        slider.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        let mut ctx = event_ctx();
        slider.event(
            &UiEvent::MouseDown {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        slider.event(
            &UiEvent::MouseMove {
                position: Point::new(100.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(slider.value(), 0.5);
        let mut modified = Modifiers::none();
        modified.ctrl = true;
        assert_eq!(
            slider.event(
                &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: modified },
                &mut ctx
            ),
            EventResult::Ignored
        );
        assert!(slider.dragging);
        assert_eq!(
            slider.event(
                &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut ctx
            ),
            EventResult::Handled
        );
        assert_eq!(slider.value(), 0.23);
        assert!(!slider.dragging);
        for event in [
            UiEvent::MouseMove {
                position: Point::new(180.0, 10.0),
                modifiers: Modifiers::none(),
            },
            UiEvent::MouseUp {
                position: Point::new(180.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            UiEvent::FocusLost,
        ] {
            slider.event(&event, &mut ctx);
        }
        let phases = phases.borrow();
        assert_eq!(phases.len(), 3);
        let ValueEdit::Begin(id) = phases[0].0 else {
            panic!("begin");
        };
        assert_eq!(phases[1].0, ValueEdit::Update(id));
        assert_eq!(phases[2], (ValueEdit::Cancel(id), 0.23));
    }

    #[test]
    fn slider_escape_restores_value_only_form_draft_without_quantizing_it() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut slider = Slider::new(0.23, 0.0, 1.0).with_step(0.25).on_change(value_action);
        slider.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        slider.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        slider.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(slider.value(), 0.23);
        assert_eq!(
            *actions.borrow(),
            vec![value_action(0.5), value_action(0.23)]
        );
    }

    #[test]
    fn slider_explicit_edits_include_unchanged_begin_and_finish_once_with_fresh_identity() {
        use std::rc::Rc;
        let phases = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&phases);
        let mut slider = Slider::new(0.0, 0.0, 100.0).on_edit(move |phase, value| {
            recorded.borrow_mut().push((phase, value));
            None::<Action>
        });
        slider.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        let mut ctx = event_ctx();
        for finish in [
            UiEvent::MouseUp {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            UiEvent::FocusLost,
        ] {
            slider.event(
                &UiEvent::MouseDown {
                    position: Point::new(0.0, 10.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            slider.event(
                &UiEvent::MouseMove {
                    position: Point::new(100.0, 10.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            slider.event(&finish, &mut ctx);
            slider.event(&UiEvent::FocusLost, &mut ctx);
        }
        let phases = phases.borrow();
        assert_eq!(phases.len(), 6);
        let ValueEdit::Begin(first) = phases[0].0 else {
            panic!("begin");
        };
        let ValueEdit::Begin(second) = phases[3].0 else {
            panic!("fresh begin");
        };
        assert_ne!(first, second);
        assert_eq!(
            phases[0].1, 0.0,
            "unchanged pointer down still opens a gesture"
        );
        assert_eq!(phases[1].0, ValueEdit::Update(first));
        assert_eq!(phases[2].0, ValueEdit::Finish(first));
        assert_eq!(phases[4].0, ValueEdit::Update(second));
        assert_eq!(phases[5].0, ValueEdit::Finish(second));
    }

    #[test]
    fn slider_mouse_down_dispatches_change_and_requests_repaint_when_value_changes() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut s = Slider::new(0.0, 0.0, 100.0).on_change(value_action);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(actions.borrow().as_slice(), &[value_action(50.0)]);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn disabled_slider_ignores_mouse_and_focus() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut slider = Slider::new(0.0, 0.0, 100.0).on_change(value_action).disabled();
        slider.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let result = slider.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(slider.value(), 0.0);
        assert!(!slider.can_focus());
        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn slider_accessibility_exposes_numeric_range() {
        let slider = Slider::new(25.0, 0.0, 100.0);

        let node = slider.accessibility().expect("slider should expose accessibility");

        assert_eq!(node.role, AccessibilityRole::Slider);
        assert_eq!(node.name.as_deref(), Some("Slider"));
        assert!(node.state.focusable);
        assert_eq!(
            node.value,
            Some(AccessibilityValue::Number { value: 25.0, min: 0.0, max: 100.0 })
        );
    }

    #[test]
    fn slider_mouse_down_at_same_value_does_not_dispatch_or_repaint() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut s = Slider::new(0.0, 0.0, 100.0).on_change(value_action);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        let track = s.track_rect();

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(track.x, track.center().y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn slider_drag_uses_thumb_center_track_range() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        let track = s.track_rect();

        let mut ctx = event_ctx();
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(track.x, track.center().y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0);

        s.event(
            &UiEvent::MouseMove {
                position: Point::new(track.x + track.width, track.center().y),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn slider_mouse_move_updates_value_during_drag() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let mut ctx = event_ctx();
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let track = s.track_rect();
        let x_75 = track.x + track.width * 0.75;
        s.event(
            &UiEvent::MouseMove {
                position: Point::new(x_75, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 75.0);
    }

    #[test]
    fn slider_mouse_up_stops_drag() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));

        let mut ctx = event_ctx();
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        s.event(
            &UiEvent::MouseUp {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        // After MouseUp, MouseMove should NOT update value
        s.event(
            &UiEvent::MouseMove {
                position: Point::new(200.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0); // unchanged from MouseDown at x=0
    }

    #[test]
    fn slider_focus_lost_without_drag_does_not_release_pointer_capture() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        assert_eq!(
            s.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(s.event(&UiEvent::FocusLost, &mut ctx), EventResult::Handled);

        assert_eq!(ctx.requests.pointer_capture, None);
    }

    #[test]
    fn slider_focus_lost_during_drag_releases_pointer_capture() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 200.0, 20.0));
        let mut ctx = event_ctx();

        s.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        ctx.requests.pointer_capture = None;

        assert_eq!(s.event(&UiEvent::FocusLost, &mut ctx), EventResult::Handled);

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(s.id()))
        );
        assert!(!s.has_active_pointer_interaction());
    }

    #[test]
    fn slider_keyboard_ignores_without_focus() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        let result = s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_arrow_keys_adjust_when_focused() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(s.value(), 51.0);

        s.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(s.value(), 50.0);
    }

    #[test]
    fn slider_keyboard_uses_configured_step() {
        let mut s = Slider::new(0.2, 0.0, 1.0).with_step(0.25);
        let mut ctx = event_ctx();

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_close(s.value(), 0.5);

        s.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_close(s.value(), 0.25);
    }

    #[test]
    fn slider_keyboard_shift_uses_large_step() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );

        assert_eq!(s.value(), 60.0);
    }

    #[test]
    fn slider_keyboard_shift_scales_configured_step() {
        let mut s = Slider::new(10.0, 0.0, 100.0).with_step(2.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );

        assert_close(s.value(), 30.0);
    }

    #[test]
    fn slider_keyboard_ignores_ctrl_alt_and_meta_chords() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        for (key, modifiers) in [
            (KeyCode::Right, Modifiers::ctrl()),
            (
                KeyCode::Right,
                Modifiers { alt: true, ..Default::default() },
            ),
            (
                KeyCode::Home,
                Modifiers { meta: true, ..Default::default() },
            ),
            (
                KeyCode::End,
                Modifiers { alt: true, shift: true, ..Default::default() },
            ),
            (
                KeyCode::PageUp,
                Modifiers { ctrl: true, shift: true, ..Default::default() },
            ),
        ] {
            assert_eq!(
                s.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_close(s.value(), 50.0);
        }
    }

    #[test]
    fn slider_invalid_step_keeps_default_keyboard_step() {
        let mut s = Slider::new(50.0, 0.0, 100.0).with_step(0.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(s.value(), 51.0);
    }

    #[test]
    fn slider_keyboard_dispatches_once_per_changed_nudge() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut s = Slider::new(50.0, 0.0, 100.0).on_change(value_action);

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(actions.borrow().as_slice(), &[value_action(51.0)]);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn slider_keyboard_at_boundary_does_not_dispatch_duplicate_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut s = Slider::new(0.0, 0.0, 100.0).on_change(value_action);

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let result = s.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn slider_home_end_jump_to_bounds() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        let mut ctx = event_ctx();

        s.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        s.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0);

        s.event(
            &UiEvent::KeyDown { key: KeyCode::End, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn slider_value_clamped_at_bounds() {
        let mut s = Slider::new(0.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 100.0, 20.0));

        let mut ctx = event_ctx();
        // Click at leftmost edge → value = min
        s.event(
            &UiEvent::MouseDown {
                position: Point::new(0.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 0.0);
        // Click at rightmost edge → value = max
        s.event(
            &UiEvent::MouseMove {
                position: Point::new(101.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(s.value(), 100.0); // clamped to max via update_value ratio clamp
    }

    #[test]
    fn slider_thumb_rect_stays_inside_bounds() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        s.layout(Rect::new(10.0, 20.0, 160.0, 18.0));

        let thumb = s.thumb_rect();

        assert!(thumb.x >= s.bounds.x);
        assert!(thumb.y >= s.bounds.y);
        assert!(thumb.x + thumb.width <= s.bounds.x + s.bounds.width);
        assert!(thumb.y + thumb.height <= s.bounds.y + s.bounds.height);
    }

    #[test]
    fn slider_paint_shrinks_thumb_for_short_bounds() {
        let mut s = Slider::new(50.0, 0.0, 100.0);
        s.layout(Rect::new(0.0, 0.0, 100.0, 10.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 100.0, 10.0),
        };

        s.paint(&mut ctx);

        let thumb = encoder.rects.last().expect("paint should draw a thumb");
        assert!(thumb.y >= s.bounds.y);
        assert!(thumb.y + thumb.height <= s.bounds.y + s.bounds.height);
    }

    #[test]
    fn slider_visual_metrics_follow_theme_tokens() {
        let mut slider = Slider::new(50.0, 0.0, 100.0);
        slider.focus_visible = true;
        let mut theme = ThemePreset::Dark.build();
        theme.spacing.xs = 5.0;
        theme.spacing.icon_size = 18.0;
        theme.spacing.border_emphasis = 3.0;
        theme.spacing.border_standard = 2.0;
        theme.spacing.interact_height = 34.0;
        let visual = SliderVisualTokens::from_theme(&theme);
        slider.layout(Rect::new(
            10.0,
            20.0,
            200.0,
            visual.thumb_size + visual.measure_padding_y,
        ));
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 100.0),
        };

        slider.paint(&mut ctx);

        assert_eq!(
            slider.measure(LayoutConstraint::LOOSE),
            Size::new(
                visual.min_width,
                visual.thumb_size + visual.measure_padding_y
            )
        );
        let track = encoder.rects[0];
        assert_eq!(track.height, visual.track_height);
        assert_eq!(track.x, slider.bounds.x + visual.thumb_size * 0.5);
        assert_eq!(track.width, slider.bounds.width - visual.thumb_size);
        assert_eq!(encoder.rect_radii[0], theme.spacing.radius_full);
        assert_eq!(encoder.rects[1].width, track.width * 0.5);
        let thumb = encoder.rects.last().copied().expect("slider should paint thumb");
        assert_eq!(thumb.width, visual.thumb_size);
        assert_eq!(thumb.height, visual.thumb_size);
        let halo = encoder.rects[2];
        assert_eq!(
            halo,
            Rect::new(
                thumb.x - visual.focus_halo_inset,
                thumb.y - visual.focus_halo_inset,
                thumb.width + visual.focus_halo_inset * 2.0,
                thumb.height + visual.focus_halo_inset * 2.0,
            )
        );
        assert_eq!(encoder.rect_colors[2].a, visual.focus_halo_alpha);
    }

    #[test]
    fn numeric_accessibility_generic_slider_retains_exact_coordinate_contract() {
        let value = 0.12345679_f32;
        let min = -0.1_f32;
        let max = 0.9_f32;
        let slider = Slider::new(value, min, max).disabled();
        let node = slider.accessibility().expect("coordinate semantics");
        assert_eq!(
            node.value,
            Some(AccessibilityValue::Number {
                value: f64::from(value),
                min: f64::from(min),
                max: f64::from(max)
            })
        );
        assert!(node.state.disabled && !node.state.focusable);
    }

    #[test]
    fn numeric_accessibility_slider_reprojection_uses_fresh_domain_name_value_and_callback() {
        use std::rc::Rc;
        let old_calls = Rc::new(Cell::new(0));
        let new_calls = Rc::new(Cell::new(0));
        let old_value = Rc::new(Cell::new(12.123456789012344_f64));
        let new_value = Rc::new(Cell::new(-42.123456789012344_f64));
        let old_read = Rc::clone(&old_value);
        let old_edits = Rc::clone(&old_calls);
        let mut old = Slider::new(0.25, 0.0, 1.0)
            .with_accessible_value(move || AccessibilityValue::Number {
                value: old_read.get(),
                min: -100.0,
                max: 100.0,
            })
            .on_change(move |_| {
                old_edits.set(old_edits.get() + 1);
                None::<Action>
            });
        old.set_control_inspection_key(Some("same-scalar-owner".to_owned()));
        old.set_accessible_name(Some("Gain (dB)".to_owned()));
        let mut ctx = event_ctx();
        old.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let state = old.control_inspection_state().expect("focus inspection");
        let fresh_read = Rc::clone(&new_value);
        let fresh_edits = Rc::clone(&new_calls);
        let mut fresh = Slider::new(0.75, 0.0, 1.0)
            .with_accessible_value(move || AccessibilityValue::Number {
                value: fresh_read.get(),
                min: -100.0,
                max: 100.0,
            })
            .on_change(move |_| {
                fresh_edits.set(fresh_edits.get() + 1);
                None::<Action>
            });
        fresh.set_control_inspection_key(Some("same-scalar-owner".to_owned()));
        fresh.set_accessible_name(Some("增益 (dB)".to_owned()));
        assert!(fresh.restore_control_inspection_state(&state));
        assert_eq!(
            fresh.value(),
            0.75,
            "focus inspection never replaces current presentation coordinate"
        );
        for value in [-42.123456789012344_f64, 99.12345678901234] {
            new_value.set(value);
            old_value.set(-88.0);
            let node = fresh.accessibility().expect("fresh semantics");
            assert_eq!(node.name.as_deref(), Some("增益 (dB)"));
            let Some(AccessibilityValue::Number { value: announced, min, max }) = node.value else {
                panic!("domain Float64")
            };
            assert_eq!(announced.to_bits(), value.to_bits());
            assert_eq!((min, max), (-100.0, 100.0));
        }
        fresh.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(old_calls.get(), 0);
        assert_eq!(new_calls.get(), 1);
        let mut different = Slider::new(0.5, 0.0, 1.0);
        different.set_control_inspection_key(Some("different-owner".to_owned()));
        assert!(!different.restore_control_inspection_state(&state));
        let mut disabled = Slider::new(0.5, 0.0, 1.0).disabled();
        disabled.set_control_inspection_key(Some("same-scalar-owner".to_owned()));
        assert!(!disabled.restore_control_inspection_state(&state));
    }
}
