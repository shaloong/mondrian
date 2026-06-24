use std::f32::consts::TAU;

use mondrian_core::{Color, HsvColor};
use mondrian_ui_core::types::{KeyCode, Modifiers, Point, Rect};

use super::ColorPickerAreaMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ColorDragTarget {
    ColorArea,
    Hue,
    Alpha,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ColorInteractionState {
    pub(super) color: Color,
    pub(super) hue: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ColorInteractionUpdate {
    pub(super) color: Color,
    pub(super) hue: f32,
    pub(super) color_changed: bool,
    pub(super) hue_changed: bool,
}

impl ColorInteractionUpdate {
    pub(super) fn changed(self) -> bool {
        self.color_changed || self.hue_changed
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ColorDragGeometry {
    pub(super) color_area: Rect,
    pub(super) hue_bar: Rect,
    pub(super) alpha_bar: Rect,
    pub(super) color_wheel: Rect,
}

pub(super) fn apply_drag(
    state: ColorInteractionState,
    area_mode: ColorPickerAreaMode,
    target: ColorDragTarget,
    point: Point,
    geometry: ColorDragGeometry,
) -> ColorInteractionUpdate {
    let mut next = state;
    match target {
        ColorDragTarget::ColorArea => match area_mode {
            ColorPickerAreaMode::Square => {
                let area = geometry.color_area;
                let s = ((point.x - area.x) / area.width).clamp(0.0, 1.0);
                let v = (1.0 - (point.y - area.y) / area.height).clamp(0.0, 1.0);
                next.color = Color::from_hsv(HsvColor { h: next.hue, s, v, a: next.color.a });
            }
            ColorPickerAreaMode::Wheel => {
                let wheel = geometry.color_wheel;
                let center = wheel.center();
                let radius = wheel.width.min(wheel.height).max(1.0) * 0.5;
                let dx = point.x - center.x;
                let dy = point.y - center.y;
                let distance = (dx * dx + dy * dy).sqrt().min(radius);
                let mut hsv = next.color.to_hsv();
                next.hue = dy.atan2(dx).rem_euclid(TAU).to_degrees();
                hsv.h = next.hue;
                hsv.s = (distance / radius).clamp(0.0, 1.0);
                next.color = Color::from_hsv(hsv);
            }
        },
        ColorDragTarget::Hue => {
            let bar = geometry.hue_bar;
            next.hue = (((point.x - bar.x) / bar.width).clamp(0.0, 1.0) * 360.0).min(359.999);
            let hsv = next.color.to_hsv();
            next.color = Color::from_hsv(HsvColor { h: next.hue, s: hsv.s, v: hsv.v, a: hsv.a });
        }
        ColorDragTarget::Alpha => {
            let bar = geometry.alpha_bar;
            next.color.a = ((point.x - bar.x) / bar.width).clamp(0.0, 1.0);
        }
    }
    interaction_update(state, next)
}

pub(super) fn nudge_keyboard_target(
    state: ColorInteractionState,
    target: ColorDragTarget,
    key: KeyCode,
    modifiers: Modifiers,
) -> Option<ColorInteractionUpdate> {
    if modifiers.ctrl || modifiers.alt || modifiers.meta {
        return None;
    }

    let mut next = state;
    let step = if modifiers.shift { 0.05 } else { 0.01 };
    let mut hsv = next.color.to_hsv();
    match target {
        ColorDragTarget::ColorArea => match key {
            KeyCode::Left => hsv.s = (hsv.s - step).clamp(0.0, 1.0),
            KeyCode::Right => hsv.s = (hsv.s + step).clamp(0.0, 1.0),
            KeyCode::Up => hsv.v = (hsv.v + step).clamp(0.0, 1.0),
            KeyCode::Down => hsv.v = (hsv.v - step).clamp(0.0, 1.0),
            _ => return None,
        },
        ColorDragTarget::Hue => match key {
            KeyCode::Left | KeyCode::Down => next.hue = (next.hue - step * 360.0).rem_euclid(360.0),
            KeyCode::Right | KeyCode::Up => next.hue = (next.hue + step * 360.0).rem_euclid(360.0),
            _ => return None,
        },
        ColorDragTarget::Alpha => match key {
            KeyCode::Left | KeyCode::Down => next.color.a = (next.color.a - step).clamp(0.0, 1.0),
            KeyCode::Right | KeyCode::Up => next.color.a = (next.color.a + step).clamp(0.0, 1.0),
            _ => return None,
        },
    }

    if !matches!(target, ColorDragTarget::Alpha) {
        next.color = Color::from_hsv(HsvColor { h: next.hue, s: hsv.s, v: hsv.v, a: hsv.a });
    }

    Some(interaction_update(state, next))
}

fn interaction_update(
    old: ColorInteractionState,
    next: ColorInteractionState,
) -> ColorInteractionUpdate {
    ColorInteractionUpdate {
        color: next.color,
        hue: next.hue,
        color_changed: next.color != old.color,
        hue_changed: (next.hue - old.hue).abs() > f32::EPSILON,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 0.0001,
            "expected {expected}, got {actual}"
        );
    }

    fn state() -> ColorInteractionState {
        ColorInteractionState {
            color: Color::from_hsv(HsvColor { h: 30.0, s: 0.4, v: 0.5, a: 0.75 }),
            hue: 30.0,
        }
    }

    fn geometry() -> ColorDragGeometry {
        ColorDragGeometry {
            color_area: Rect::new(10.0, 10.0, 100.0, 80.0),
            hue_bar: Rect::new(10.0, 100.0, 100.0, 12.0),
            alpha_bar: Rect::new(10.0, 120.0, 100.0, 12.0),
            color_wheel: Rect::new(20.0, 20.0, 80.0, 80.0),
        }
    }

    #[test]
    fn square_drag_updates_saturation_and_value_without_changing_hue() {
        let update = apply_drag(
            state(),
            ColorPickerAreaMode::Square,
            ColorDragTarget::ColorArea,
            Point::new(60.0, 30.0),
            geometry(),
        );
        let hsv = update.color.to_hsv();

        assert_close(update.hue, 30.0);
        assert_close(hsv.s, 0.5);
        assert_close(hsv.v, 0.75);
        assert!(update.color_changed);
        assert!(!update.hue_changed);
    }

    #[test]
    fn wheel_drag_updates_hue_and_saturation_from_centered_angle() {
        let update = apply_drag(
            state(),
            ColorPickerAreaMode::Wheel,
            ColorDragTarget::ColorArea,
            Point::new(100.0, 60.0),
            geometry(),
        );
        let hsv = update.color.to_hsv();

        assert_close(update.hue, 0.0);
        assert_close(hsv.s, 1.0);
        assert!(update.changed());
    }

    #[test]
    fn hue_drag_maps_bar_edges_to_bounded_hue() {
        let update = apply_drag(
            state(),
            ColorPickerAreaMode::Square,
            ColorDragTarget::Hue,
            Point::new(110.0, 106.0),
            geometry(),
        );

        assert_close(update.hue, 359.999);
        assert!(update.color_changed);
        assert!(update.hue_changed);
    }

    #[test]
    fn alpha_drag_clamps_alpha_without_changing_hue() {
        let update = apply_drag(
            state(),
            ColorPickerAreaMode::Square,
            ColorDragTarget::Alpha,
            Point::new(-20.0, 126.0),
            geometry(),
        );

        assert_close(update.color.a, 0.0);
        assert_close(update.hue, 30.0);
        assert!(update.color_changed);
        assert!(!update.hue_changed);
    }

    #[test]
    fn keyboard_nudge_ignores_owned_system_chords() {
        let update = nudge_keyboard_target(
            state(),
            ColorDragTarget::ColorArea,
            KeyCode::Right,
            Modifiers::ctrl(),
        );

        assert_eq!(update, None);
    }

    #[test]
    fn keyboard_nudge_updates_hue_and_wraps() {
        let update = nudge_keyboard_target(
            ColorInteractionState { hue: 358.0, ..state() },
            ColorDragTarget::Hue,
            KeyCode::Right,
            Modifiers::none(),
        )
        .expect("supported nudge");

        assert_close(update.hue, 1.6);
        assert!(update.changed());
    }

    #[test]
    fn keyboard_shift_nudge_uses_large_alpha_step() {
        let update = nudge_keyboard_target(
            state(),
            ColorDragTarget::Alpha,
            KeyCode::Right,
            Modifiers::shift(),
        )
        .expect("supported nudge");

        assert_close(update.color.a, 0.8);
        assert!(!update.hue_changed);
    }
}
