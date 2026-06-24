use mondrian_ui_core::types::{Point, Rect};

use super::model::ColorField;
use super::{
    ColorPickerAreaMode, ColorPickerMode, BAR_GAP, BAR_HEIGHT, COLOR_AREA_HEIGHT, FIELD_TOP_GAP,
    MODES, MODE_HEIGHT, MODE_TRIGGER_WIDTH, PICKER_TOP, ROW_GAP, ROW_HEIGHT, SWATCH_SIZE,
};
use crate::form_layout::{FormLayout, FormRowOptions, FormRowRects};
use crate::menu::anchored_menu_rect;

const FIELD_COLUMN_GAP: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ColorPickerGeometry {
    bounds: Rect,
    mode: ColorPickerMode,
    area_mode: ColorPickerAreaMode,
    show_swatch: bool,
    mode_menu_open: bool,
    overlay_viewport: Option<Rect>,
}

impl ColorPickerGeometry {
    pub(super) fn new(
        bounds: Rect,
        mode: ColorPickerMode,
        area_mode: ColorPickerAreaMode,
        show_swatch: bool,
        mode_menu_open: bool,
        overlay_viewport: Option<Rect>,
    ) -> Self {
        Self {
            bounds,
            mode,
            area_mode,
            show_swatch,
            mode_menu_open,
            overlay_viewport,
        }
    }

    pub(super) fn swatch_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x + 12.0,
            self.bounds.y + 12.0,
            SWATCH_SIZE,
            SWATCH_SIZE,
        )
    }

    pub(super) fn mode_trigger_rect(&self) -> Rect {
        let x = if self.show_swatch {
            self.bounds.x + 58.0
        } else {
            self.bounds.x + 12.0
        };
        Rect::new(
            x,
            self.bounds.y + 12.0,
            MODE_TRIGGER_WIDTH.min((self.bounds.x + self.bounds.width - x - 12.0).max(1.0)),
            MODE_HEIGHT,
        )
    }

    pub(super) fn mode_menu_rect(&self) -> Rect {
        let trigger = self.mode_trigger_rect();
        anchored_menu_rect(
            trigger,
            trigger.width,
            MODE_HEIGHT * MODES.len() as f32 + 8.0,
            6.0,
            self.overlay_viewport,
        )
    }

    pub(super) fn eyedropper_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x + self.bounds.width - 40.0,
            self.bounds.y + 12.0,
            28.0,
            28.0,
        )
    }

    pub(super) fn mode_item_rect(&self, mode: ColorPickerMode) -> Rect {
        let menu = self.mode_menu_rect();
        let index = MODES.iter().position(|candidate| *candidate == mode).unwrap_or(0);
        Rect::new(
            menu.x + 2.0,
            menu.y + 4.0 + MODE_HEIGHT * index as f32,
            (menu.width - 4.0).max(1.0),
            MODE_HEIGHT - 1.0,
        )
    }

    pub(super) fn color_area_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x + 10.0,
            self.bounds.y + PICKER_TOP,
            (self.bounds.width - 20.0).max(1.0),
            COLOR_AREA_HEIGHT,
        )
    }

    pub(super) fn hue_bar_rect(&self) -> Rect {
        let area = self.color_area_rect();
        Rect::new(
            area.x,
            area.y + area.height + BAR_GAP,
            area.width,
            BAR_HEIGHT,
        )
    }

    pub(super) fn alpha_bar_rect(&self) -> Rect {
        let hue = self.hue_bar_rect();
        Rect::new(hue.x, hue.y + hue.height + BAR_GAP, hue.width, BAR_HEIGHT)
    }

    pub(super) fn fields_top(&self) -> f32 {
        let alpha = self.alpha_bar_rect();
        alpha.y + alpha.height + FIELD_TOP_GAP
    }

    pub(super) fn field_column_count(&self) -> usize {
        match self.mode {
            ColorPickerMode::Hex => 1,
            ColorPickerMode::Rgb | ColorPickerMode::Hsl | ColorPickerMode::Hsv => 4,
            ColorPickerMode::Cmyk => 5,
        }
    }

    pub(super) fn field_at_index(&self, index: usize) -> Option<ColorField> {
        self.mode.fields().get(index).copied()
    }

    pub(super) fn field_label_width(&self, index: usize) -> f32 {
        match self.field_at_index(index) {
            Some(ColorField::Hex) => 28.0,
            Some(_) => 14.0,
            None => 14.0,
        }
    }

    pub(super) fn field_column_rect(&self, index: usize) -> Rect {
        let columns = self.field_column_count().max(1);
        let col = index % columns;
        let row = index / columns;
        let total_gap = FIELD_COLUMN_GAP * (columns.saturating_sub(1)) as f32;
        let total_w = (self.bounds.width - 20.0 - total_gap).max(1.0);
        let col_w = total_w / columns as f32;
        let x = self.bounds.x + 10.0 + col as f32 * (col_w + FIELD_COLUMN_GAP);
        let y = self.fields_top() + row as f32 * (ROW_HEIGHT + ROW_GAP);
        Rect::new(x, y, col_w, ROW_HEIGHT)
    }

    pub(super) fn field_row_rects(&self, index: usize) -> FormRowRects {
        let column = self.field_column_rect(index);
        let layout = FormLayout::new(FormRowOptions {
            label_width: self.field_label_width(index),
            control_gap: 0.0,
            label_height: 14.0,
            compact_label_y_offset: -7.0,
            ..FormRowOptions::default()
        });
        layout.row_rects(column, column.y, ROW_HEIGHT, ROW_HEIGHT, ROW_HEIGHT)
    }

    pub(super) fn field_rect(&self, index: usize) -> Rect {
        self.field_row_rects(index).control
    }

    pub(super) fn field_label_pos(&self, index: usize) -> Point {
        let label = self.field_row_rects(index).label;
        Point::new(label.x + 2.0, label.y)
    }

    pub(super) fn mode_chrome_contains(&self, point: Point) -> bool {
        self.mode_trigger_rect().contains(point)
            || (self.mode_menu_open && self.mode_menu_rect().contains(point))
    }

    pub(super) fn color_area_hit_test(&self, point: Point) -> bool {
        match self.area_mode {
            ColorPickerAreaMode::Square => self.color_area_rect().contains(point),
            ColorPickerAreaMode::Wheel => {
                let wheel = self.color_wheel_rect();
                let center = wheel.center();
                let radius = wheel.width.min(wheel.height) * 0.5;
                let dx = point.x - center.x;
                let dy = point.y - center.y;
                dx * dx + dy * dy <= radius * radius
            }
        }
    }

    pub(super) fn color_wheel_rect(&self) -> Rect {
        let area = self.color_area_rect().inset(1.0, 1.0);
        let size = area.width.min(area.height).max(1.0);
        Rect::new(
            area.x + (area.width - size) * 0.5,
            area.y + (area.height - size) * 0.5,
            size,
            size,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry_for_mode(mode: ColorPickerMode) -> ColorPickerGeometry {
        ColorPickerGeometry::new(
            Rect::new(20.0, 30.0, 280.0, 292.0),
            mode,
            ColorPickerAreaMode::Square,
            true,
            false,
            None,
        )
    }

    #[test]
    fn trigger_shifts_left_when_swatch_is_hidden() {
        let shown = geometry_for_mode(ColorPickerMode::Hex);
        let hidden = ColorPickerGeometry::new(
            Rect::new(20.0, 30.0, 280.0, 292.0),
            ColorPickerMode::Hex,
            ColorPickerAreaMode::Square,
            false,
            false,
            None,
        );

        assert_eq!(shown.mode_trigger_rect().x, 78.0);
        assert_eq!(hidden.mode_trigger_rect().x, 32.0);
    }

    #[test]
    fn cmyk_fields_fit_in_one_row_with_positive_control_width() {
        let geometry = geometry_for_mode(ColorPickerMode::Cmyk);
        let first = geometry.field_rect(0);
        let last = geometry.field_rect(4);

        assert!(first.width > 1.0);
        assert!(last.width > 1.0);
        assert_eq!(first.y, last.y);
        assert!(last.x + last.width <= 20.0 + 280.0 - 10.0);
    }

    #[test]
    fn hex_label_reserves_wider_label_lane_than_numeric_fields() {
        let hex = geometry_for_mode(ColorPickerMode::Hex);
        let rgb = geometry_for_mode(ColorPickerMode::Rgb);

        assert_eq!(hex.field_label_width(0), 28.0);
        assert_eq!(rgb.field_label_width(0), 14.0);
        assert!(hex.field_rect(0).x > hex.field_label_pos(0).x);
    }

    #[test]
    fn wheel_area_hit_test_uses_circular_bounds_inside_square_area() {
        let geometry = ColorPickerGeometry::new(
            Rect::new(0.0, 0.0, 280.0, 292.0),
            ColorPickerMode::Hsv,
            ColorPickerAreaMode::Wheel,
            true,
            false,
            None,
        );
        let wheel = geometry.color_wheel_rect();

        assert!(geometry.color_area_hit_test(wheel.center()));
        assert!(!geometry.color_area_hit_test(Point::new(wheel.x - 1.0, wheel.y - 1.0)));
    }

    #[test]
    fn mode_menu_flips_inside_bottom_viewport() {
        let geometry = ColorPickerGeometry::new(
            Rect::new(0.0, 240.0, 280.0, 292.0),
            ColorPickerMode::Hex,
            ColorPickerAreaMode::Square,
            true,
            true,
            Some(Rect::new(0.0, 0.0, 280.0, 300.0)),
        );

        assert!(geometry.mode_menu_rect().y < geometry.mode_trigger_rect().y);
        assert!(
            geometry.mode_chrome_contains(geometry.mode_item_rect(ColorPickerMode::Hsv).center())
        );
    }
}
