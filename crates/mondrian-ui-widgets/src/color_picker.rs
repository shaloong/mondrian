//! Color picker widget.
//!
//! Provides a compact model-based color editor for the custom UI stack.

use std::f32::consts::TAU;

use mondrian_core::{CmykColor, Color, HslColor, HsvColor, RgbaColor};
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    CursorRequest, EventContext, EventRequests, PaintContext, PointerCaptureRequest,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::form_layout::{FormLayout, FormRowOptions, FormRowRects};
use crate::menu::{paint_menu_popup_chrome, paint_menu_row, paint_menu_trigger, MenuRowPaint};
use crate::paint::{color_with_alpha, mix_color, paint_checkerboard, paint_shadow, soft_border};
use crate::text_input::TextInput;

const MODES: [ColorPickerMode; 5] = [
    ColorPickerMode::Hex,
    ColorPickerMode::Rgb,
    ColorPickerMode::Hsl,
    ColorPickerMode::Hsv,
    ColorPickerMode::Cmyk,
];
const FIELD_COUNT: usize = 5;
const PICKER_TOP: f32 = 56.0;
const COLOR_AREA_HEIGHT: f32 = 112.0;
const BAR_HEIGHT: f32 = 14.0;
const BAR_GAP: f32 = 8.0;
const FIELD_TOP_GAP: f32 = 12.0;
const PICKER_WIDTH: f32 = 280.0;
const PICKER_HEIGHT: f32 = 292.0;
const MODE_HEIGHT: f32 = 28.0;
const MODE_TRIGGER_WIDTH: f32 = 74.0;
const ROW_HEIGHT: f32 = 28.0;
const ROW_GAP: f32 = 6.0;
const SWATCH_SIZE: f32 = 36.0;
const HUE_SEGMENTS: usize = 6;
const WHEEL_SEGMENTS: usize = 72;

/// Editable color model shown by [`ColorPicker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPickerMode {
    /// Hexadecimal `#RRGGBB` / `#RRGGBBAA` input.
    Hex,
    /// Red, green, blue, alpha channels.
    Rgb,
    /// Hue, saturation, lightness, alpha channels.
    Hsl,
    /// Hue, saturation, value, alpha channels.
    Hsv,
    /// Cyan, magenta, yellow, key, alpha channels.
    Cmyk,
}

impl ColorPickerMode {
    /// Short label used by segmented controls.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hex => "HEX",
            Self::Rgb => "RGB",
            Self::Hsl => "HSL",
            Self::Hsv => "HSV",
            Self::Cmyk => "CMYK",
        }
    }

    fn fields(self) -> &'static [ColorField] {
        match self {
            Self::Hex => &[ColorField::Hex],
            Self::Rgb => &[ColorField::R, ColorField::G, ColorField::B, ColorField::A],
            Self::Hsl => &[ColorField::H, ColorField::S, ColorField::L, ColorField::A],
            Self::Hsv => &[ColorField::H, ColorField::S, ColorField::V, ColorField::A],
            Self::Cmyk => &[
                ColorField::C,
                ColorField::M,
                ColorField::Y,
                ColorField::K,
                ColorField::A,
            ],
        }
    }
}

/// Shape used by the interactive color area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPickerAreaMode {
    /// Rectangular saturation/value area with a separate hue bar.
    Square,
    /// Circular hue/saturation wheel. Value remains editable through fields.
    Wheel,
}

/// Adapter that maps the current picker color to an editor [`Action`].
pub type ColorChangeAction = dyn Fn(Color) -> Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorField {
    Hex,
    R,
    G,
    B,
    A,
    H,
    S,
    L,
    V,
    C,
    M,
    Y,
    K,
}

impl ColorField {
    fn label(self) -> &'static str {
        match self {
            Self::Hex => "Hex",
            Self::R => "R",
            Self::G => "G",
            Self::B => "B",
            Self::A => "A",
            Self::H => "H",
            Self::S => "S",
            Self::L => "L",
            Self::V => "V",
            Self::C => "C",
            Self::M => "M",
            Self::Y => "Y",
            Self::K => "K",
        }
    }
}

/// Color picker widget with model tabs and text inputs.
pub struct ColorPicker {
    id: WidgetId,
    bounds: Rect,
    color: Color,
    hue: f32,
    mode: ColorPickerMode,
    fields: [TextInput; FIELD_COUNT],
    mode_hovered: Option<ColorPickerMode>,
    mode_pressed: Option<ColorPickerMode>,
    mode_menu_open: bool,
    eyedropper_hovered: bool,
    eyedropper_pressed: bool,
    drag_target: Option<ColorDragTarget>,
    keyboard_target: ColorDragTarget,
    focused_field: Option<usize>,
    field_pointer_captured: Option<usize>,
    eyedropper_active: bool,
    area_mode: ColorPickerAreaMode,
    show_swatch: bool,
    focused: bool,
    focus_visible: bool,
    enabled: bool,
    on_change: Option<Box<ColorChangeAction>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorDragTarget {
    ColorArea,
    Hue,
    Alpha,
}

impl ColorPicker {
    /// Create a color picker with the given initial color.
    pub fn new(color: Color) -> Self {
        let hue = color.to_hsv().h;
        let mut picker = Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            color,
            hue,
            mode: ColorPickerMode::Hex,
            fields: [
                TextInput::new(""),
                TextInput::new(""),
                TextInput::new(""),
                TextInput::new(""),
                TextInput::new(""),
            ],
            mode_hovered: None,
            mode_pressed: None,
            mode_menu_open: false,
            eyedropper_hovered: false,
            eyedropper_pressed: false,
            drag_target: None,
            keyboard_target: ColorDragTarget::ColorArea,
            focused_field: None,
            field_pointer_captured: None,
            eyedropper_active: false,
            area_mode: ColorPickerAreaMode::Square,
            show_swatch: true,
            focused: false,
            focus_visible: false,
            enabled: true,
            on_change: None,
        };
        picker.sync_fields_from_color();
        picker
    }

    /// Return the current color.
    pub fn color(&self) -> Color {
        self.color
    }

    /// Replace the current color and refresh visible input fields.
    pub fn set_color(&mut self, color: Color) {
        self.color = color;
        self.update_hue_from_color();
        self.sync_fields_from_color();
    }

    /// Dispatch a value-aware action whenever user input changes the color.
    pub fn on_change(mut self, action: impl Fn(Color) -> Action + 'static) -> Self {
        self.on_change = Some(Box::new(action));
        self
    }

    /// Set whether the picker accepts pointer, keyboard, IME, eyedropper, and focus input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// Disable pointer, keyboard, IME, eyedropper, and focus input.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the picker accepts user input.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set whether the picker accepts pointer, keyboard, IME, eyedropper, and focus input.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        for field in &mut self.fields {
            field.set_enabled(enabled);
        }
        if !enabled {
            self.focused = false;
            self.focus_visible = false;
            self.mode_menu_open = false;
            self.mode_pressed = None;
            self.mode_hovered = None;
            self.eyedropper_hovered = false;
            self.eyedropper_pressed = false;
        }
    }

    /// Return the active editing mode.
    pub fn mode(&self) -> ColorPickerMode {
        self.mode
    }

    /// Switch the active editing mode.
    pub fn set_mode(&mut self, mode: ColorPickerMode) {
        if self.mode != mode {
            self.mode = mode;
            self.mode_hovered = None;
            self.mode_pressed = None;
            self.mode_menu_open = false;
            self.focused_field = None;
            self.field_pointer_captured = None;
            self.sync_fields_from_color();
            self.layout_fields();
        }
    }

    /// Return the interactive color area shape.
    pub fn area_mode(&self) -> ColorPickerAreaMode {
        self.area_mode
    }

    /// Select the interactive color area shape.
    pub fn set_area_mode(&mut self, area_mode: ColorPickerAreaMode) {
        self.area_mode = area_mode;
    }

    /// Whether the picker paints its own current-color swatch.
    pub fn show_swatch(&self) -> bool {
        self.show_swatch
    }

    /// Show or hide the current-color swatch in the picker chrome.
    pub fn set_show_swatch(&mut self, show_swatch: bool) {
        self.show_swatch = show_swatch;
    }

    /// Mark the picker as waiting for an externally sampled color.
    pub fn begin_eyedropper(&mut self) {
        if !self.enabled {
            return;
        }
        self.eyedropper_active = true;
        self.mode_menu_open = false;
        self.mode_pressed = None;
        self.mode_hovered = None;
        self.drag_target = None;
    }

    /// Cancel an active eyedropper request.
    pub fn cancel_eyedropper(&mut self) {
        self.eyedropper_active = false;
        self.eyedropper_pressed = false;
    }

    /// Whether the picker is waiting for a sampled color.
    pub fn is_eyedropper_active(&self) -> bool {
        self.eyedropper_active
    }

    /// Apply a color sampled by an external viewer/platform integration.
    pub fn apply_sampled_color(&mut self, color: Color) {
        self.eyedropper_active = false;
        self.set_color(color);
    }

    fn active_fields(&self) -> &'static [ColorField] {
        self.mode.fields()
    }

    fn update_hue_from_color(&mut self) {
        let hsv = self.color.to_hsv();
        if hsv.s > 0.001 && hsv.v > 0.001 {
            self.hue = hsv.h;
        }
    }

    fn cancel_interaction(&mut self) {
        self.drag_target = None;
        self.mode_pressed = None;
        self.mode_hovered = None;
        self.eyedropper_pressed = false;
        self.eyedropper_active = false;
        self.focused_field = None;
        self.field_pointer_captured = None;
    }

    fn pointer_position(event: &UiEvent) -> Option<Point> {
        match event {
            UiEvent::MouseDown { position, .. }
            | UiEvent::MouseUp { position, .. }
            | UiEvent::MouseMove { position, .. }
            | UiEvent::MouseWheel { position, .. }
            | UiEvent::DragEnter { position, .. }
            | UiEvent::DragOver { position }
            | UiEvent::Drop { position, .. } => Some(*position),
            _ => None,
        }
    }

    fn field_at_position(&self, position: Point) -> Option<usize> {
        (0..self.active_fields().len()).find(|index| self.fields[*index].hit_test(position))
    }

    fn translate_field_capture_request(
        &mut self,
        field_index: usize,
        requests: &mut EventRequests,
    ) {
        match requests.pointer_capture {
            Some(PointerCaptureRequest::Capture(id)) if id == self.fields[field_index].id() => {
                self.field_pointer_captured = Some(field_index);
                requests.request_pointer_capture(self.id);
            }
            Some(PointerCaptureRequest::Release(id)) if id == self.fields[field_index].id() => {
                if self.field_pointer_captured == Some(field_index) {
                    self.field_pointer_captured = None;
                }
                requests.release_pointer_capture(self.id);
            }
            _ => {}
        }
    }

    fn blur_fields(&mut self, ctx: &mut EventContext) {
        for index in 0..self.active_fields().len() {
            let _ = self.fields[index].event(&UiEvent::FocusLost, ctx);
            self.translate_field_capture_request(index, ctx.requests);
        }
        self.focused_field = None;
    }

    fn send_field_event(
        &mut self,
        index: usize,
        event: &UiEvent,
        ctx: &mut EventContext,
    ) -> EventResult {
        let before = self.fields[index].text().to_string();
        let result = self.fields[index].event(event, ctx);
        self.translate_field_capture_request(index, ctx.requests);
        if result == EventResult::Handled
            && self.fields[index].text() != before
            && self.apply_visible_fields()
        {
            self.dispatch_change(ctx);
            ctx.request_repaint();
        }
        result
    }

    fn layout_fields(&mut self) {
        for index in 0..FIELD_COUNT {
            self.fields[index].layout(self.field_rect(index));
        }
    }

    fn swatch_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x + 12.0,
            self.bounds.y + 12.0,
            SWATCH_SIZE,
            SWATCH_SIZE,
        )
    }

    fn mode_trigger_rect(&self) -> Rect {
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

    fn mode_menu_rect(&self) -> Rect {
        let trigger = self.mode_trigger_rect();
        Rect::new(
            trigger.x,
            trigger.y + trigger.height + 6.0,
            trigger.width,
            MODE_HEIGHT * MODES.len() as f32 + 8.0,
        )
    }

    fn eyedropper_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x + self.bounds.width - 40.0,
            self.bounds.y + 12.0,
            28.0,
            28.0,
        )
    }

    fn mode_item_rect(&self, mode: ColorPickerMode) -> Rect {
        let menu = self.mode_menu_rect();
        let index = MODES.iter().position(|candidate| *candidate == mode).unwrap_or(0);
        Rect::new(
            menu.x + 2.0,
            menu.y + 4.0 + MODE_HEIGHT * index as f32,
            (menu.width - 4.0).max(1.0),
            MODE_HEIGHT - 1.0,
        )
    }

    fn color_area_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x + 10.0,
            self.bounds.y + PICKER_TOP,
            (self.bounds.width - 20.0).max(1.0),
            COLOR_AREA_HEIGHT,
        )
    }

    fn hue_bar_rect(&self) -> Rect {
        let area = self.color_area_rect();
        Rect::new(
            area.x,
            area.y + area.height + BAR_GAP,
            area.width,
            BAR_HEIGHT,
        )
    }

    fn alpha_bar_rect(&self) -> Rect {
        let hue = self.hue_bar_rect();
        Rect::new(hue.x, hue.y + hue.height + BAR_GAP, hue.width, BAR_HEIGHT)
    }

    fn fields_top(&self) -> f32 {
        let alpha = self.alpha_bar_rect();
        alpha.y + alpha.height + FIELD_TOP_GAP
    }

    fn field_column_count(&self) -> usize {
        match self.mode {
            ColorPickerMode::Hex => 1,
            ColorPickerMode::Rgb | ColorPickerMode::Hsl | ColorPickerMode::Hsv => 4,
            ColorPickerMode::Cmyk => 5,
        }
    }

    fn field_at_index(&self, index: usize) -> Option<ColorField> {
        self.active_fields().get(index).copied()
    }

    fn field_label_width(&self, index: usize) -> f32 {
        match self.field_at_index(index) {
            Some(ColorField::Hex) => 28.0,
            Some(_) => 14.0,
            None => 14.0,
        }
    }

    fn field_column_rect(&self, index: usize) -> Rect {
        let col_gap = 6.0;
        let columns = self.field_column_count().max(1);
        let col = index % columns;
        let row = index / columns;
        let total_gap = col_gap * (columns.saturating_sub(1)) as f32;
        let total_w = (self.bounds.width - 20.0 - total_gap).max(1.0);
        let col_w = total_w / columns as f32;
        let x = self.bounds.x + 10.0 + col as f32 * (col_w + col_gap);
        let y = self.fields_top() + row as f32 * (ROW_HEIGHT + ROW_GAP);
        Rect::new(x, y, col_w, ROW_HEIGHT)
    }

    fn field_row_rects(&self, index: usize) -> FormRowRects {
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

    fn field_rect(&self, index: usize) -> Rect {
        self.field_row_rects(index).control
    }

    fn field_label_pos(&self, index: usize) -> Point {
        let label = self.field_row_rects(index).label;
        Point::new(label.x + 2.0, label.y)
    }

    fn field_text(&self, field: ColorField) -> String {
        match field {
            ColorField::Hex => self.color.to_hex_rgba(),
            ColorField::R => int_channel(self.color.r).to_string(),
            ColorField::G => int_channel(self.color.g).to_string(),
            ColorField::B => int_channel(self.color.b).to_string(),
            ColorField::A => percent_channel(self.color.a).to_string(),
            ColorField::H => match self.mode {
                ColorPickerMode::Hsl => format_number(self.color.to_hsl().h),
                ColorPickerMode::Hsv => format_number(self.color.to_hsv().h),
                _ => "0".into(),
            },
            ColorField::S => match self.mode {
                ColorPickerMode::Hsl => percent_channel(self.color.to_hsl().s).to_string(),
                ColorPickerMode::Hsv => percent_channel(self.color.to_hsv().s).to_string(),
                _ => "0".into(),
            },
            ColorField::L => percent_channel(self.color.to_hsl().l).to_string(),
            ColorField::V => percent_channel(self.color.to_hsv().v).to_string(),
            ColorField::C => percent_channel(self.color.to_cmyk().c).to_string(),
            ColorField::M => percent_channel(self.color.to_cmyk().m).to_string(),
            ColorField::Y => percent_channel(self.color.to_cmyk().y).to_string(),
            ColorField::K => percent_channel(self.color.to_cmyk().k).to_string(),
        }
    }

    fn sync_fields_from_color(&mut self) {
        let fields = self.active_fields();
        for index in 0..FIELD_COUNT {
            let text = fields.get(index).map(|field| self.field_text(*field)).unwrap_or_default();
            self.fields[index].set_text(text);
        }
    }

    fn apply_visible_fields(&mut self) -> bool {
        let old = self.color;
        let fields = self.active_fields();
        let text = |index: usize| self.fields[index].text();
        let parsed = match self.mode {
            ColorPickerMode::Hex => Color::parse_hex(text(0)).ok(),
            ColorPickerMode::Rgb => {
                let r = parse_u8_channel(text(0));
                let g = parse_u8_channel(text(1));
                let b = parse_u8_channel(text(2));
                let a = parse_percent_channel(text(3));
                match (r, g, b, a) {
                    (Some(r), Some(g), Some(b), Some(a)) => {
                        Some(Color::from_rgba(RgbaColor { r, g, b, a }))
                    }
                    _ => None,
                }
            }
            ColorPickerMode::Hsl => {
                let h = parse_hue(text(0));
                let s = parse_percent_channel(text(1));
                let l = parse_percent_channel(text(2));
                let a = parse_percent_channel(text(3));
                match (h, s, l, a) {
                    (Some(h), Some(s), Some(l), Some(a)) => {
                        Some(Color::from_hsl(HslColor { h, s, l, a }))
                    }
                    _ => None,
                }
            }
            ColorPickerMode::Hsv => {
                let h = parse_hue(text(0));
                let s = parse_percent_channel(text(1));
                let v = parse_percent_channel(text(2));
                let a = parse_percent_channel(text(3));
                match (h, s, v, a) {
                    (Some(h), Some(s), Some(v), Some(a)) => {
                        Some(Color::from_hsv(HsvColor { h, s, v, a }))
                    }
                    _ => None,
                }
            }
            ColorPickerMode::Cmyk => {
                let c = parse_percent_channel(text(0));
                let m = parse_percent_channel(text(1));
                let y = parse_percent_channel(text(2));
                let k = parse_percent_channel(text(3));
                let a = parse_percent_channel(text(4));
                match (c, m, y, k, a) {
                    (Some(c), Some(m), Some(y), Some(k), Some(a)) => {
                        Some(Color::from_cmyk(CmykColor { c, m, y, k, a }))
                    }
                    _ => None,
                }
            }
        };

        let Some(color) = parsed else {
            return false;
        };

        if fields.is_empty() {
            return false;
        }

        self.color = color;
        match self.mode {
            ColorPickerMode::Hsl | ColorPickerMode::Hsv => {
                if let Some(hue) = parse_hue(text(0)) {
                    self.hue = hue;
                }
            }
            _ => self.update_hue_from_color(),
        }
        self.color != old
    }

    fn hovered_mode_at(&self, point: Point) -> Option<ColorPickerMode> {
        if !self.mode_menu_open {
            return None;
        }
        MODES.iter().copied().find(|mode| self.mode_item_rect(*mode).contains(point))
    }

    fn next_mode_selection(&self, direction: i32) -> ColorPickerMode {
        let current_mode = self.mode_hovered.unwrap_or(self.mode);
        let current = MODES.iter().position(|candidate| *candidate == current_mode).unwrap_or(0);
        let next = if direction < 0 {
            (current + MODES.len() - 1) % MODES.len()
        } else {
            (current + 1) % MODES.len()
        };
        MODES[next]
    }

    fn close_mode_menu(&mut self) {
        self.mode_menu_open = false;
        self.mode_pressed = None;
        self.mode_hovered = None;
    }

    fn mode_chrome_contains(&self, point: Point) -> bool {
        self.mode_trigger_rect().contains(point)
            || (self.mode_menu_open && self.mode_menu_rect().contains(point))
    }

    fn drag_target_at(&self, point: Point) -> Option<ColorDragTarget> {
        if self.color_area_hit_test(point) {
            Some(ColorDragTarget::ColorArea)
        } else if self.hue_bar_rect().contains(point) {
            Some(ColorDragTarget::Hue)
        } else if self.alpha_bar_rect().contains(point) {
            Some(ColorDragTarget::Alpha)
        } else {
            None
        }
    }

    fn color_area_hit_test(&self, point: Point) -> bool {
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

    fn color_wheel_rect(&self) -> Rect {
        let area = self.color_area_rect().inset(1.0, 1.0);
        let size = area.width.min(area.height).max(1.0);
        Rect::new(
            area.x + (area.width - size) * 0.5,
            area.y + (area.height - size) * 0.5,
            size,
            size,
        )
    }

    fn dispatch_change(&self, ctx: &mut EventContext) {
        if let Some(action) = &self.on_change {
            (ctx.dispatch)(action(self.color));
        }
    }

    fn color_changed_from_input(&self, old: Color, ctx: &mut EventContext) -> bool {
        if self.color == old {
            return false;
        }
        self.dispatch_change(ctx);
        ctx.request_repaint();
        true
    }

    fn update_from_drag(
        &mut self,
        target: ColorDragTarget,
        point: Point,
        ctx: &mut EventContext,
    ) -> bool {
        let old_color = self.color;
        let old_hue = self.hue;
        match target {
            ColorDragTarget::ColorArea => match self.area_mode {
                ColorPickerAreaMode::Square => {
                    let area = self.color_area_rect();
                    let s = ((point.x - area.x) / area.width).clamp(0.0, 1.0);
                    let v = (1.0 - (point.y - area.y) / area.height).clamp(0.0, 1.0);
                    let a = self.color.a;
                    self.color = Color::from_hsv(HsvColor { h: self.hue, s, v, a });
                }
                ColorPickerAreaMode::Wheel => {
                    let wheel = self.color_wheel_rect();
                    let center = wheel.center();
                    let radius = wheel.width.min(wheel.height).max(1.0) * 0.5;
                    let dx = point.x - center.x;
                    let dy = point.y - center.y;
                    let distance = (dx * dx + dy * dy).sqrt().min(radius);
                    let mut hsv = self.color.to_hsv();
                    self.hue = dy.atan2(dx).rem_euclid(TAU).to_degrees();
                    hsv.h = self.hue;
                    hsv.s = (distance / radius).clamp(0.0, 1.0);
                    self.color = Color::from_hsv(hsv);
                }
            },
            ColorDragTarget::Hue => {
                let bar = self.hue_bar_rect();
                self.hue = (((point.x - bar.x) / bar.width).clamp(0.0, 1.0) * 360.0).min(359.999);
                let hsv = self.color.to_hsv();
                self.color =
                    Color::from_hsv(HsvColor { h: self.hue, s: hsv.s, v: hsv.v, a: hsv.a });
            }
            ColorDragTarget::Alpha => {
                let bar = self.alpha_bar_rect();
                self.color.a = ((point.x - bar.x) / bar.width).clamp(0.0, 1.0);
            }
        }
        self.sync_fields_from_color();
        let color_changed = self.color_changed_from_input(old_color, ctx);
        if !color_changed && (self.hue - old_hue).abs() > f32::EPSILON {
            ctx.request_repaint();
        }
        color_changed || (self.hue - old_hue).abs() > f32::EPSILON
    }

    fn nudge_keyboard_target(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> bool {
        let old_color = self.color;
        let old_hue = self.hue;
        let step = if modifiers.shift { 0.05 } else { 0.01 };
        let mut hsv = self.color.to_hsv();
        match self.keyboard_target {
            ColorDragTarget::ColorArea => match key {
                KeyCode::Left => hsv.s = (hsv.s - step).clamp(0.0, 1.0),
                KeyCode::Right => hsv.s = (hsv.s + step).clamp(0.0, 1.0),
                KeyCode::Up => hsv.v = (hsv.v + step).clamp(0.0, 1.0),
                KeyCode::Down => hsv.v = (hsv.v - step).clamp(0.0, 1.0),
                _ => return false,
            },
            ColorDragTarget::Hue => match key {
                KeyCode::Left | KeyCode::Down => {
                    self.hue = (self.hue - step * 360.0).rem_euclid(360.0)
                }
                KeyCode::Right | KeyCode::Up => {
                    self.hue = (self.hue + step * 360.0).rem_euclid(360.0)
                }
                _ => return false,
            },
            ColorDragTarget::Alpha => match key {
                KeyCode::Left | KeyCode::Down => {
                    self.color.a = (self.color.a - step).clamp(0.0, 1.0);
                    self.sync_fields_from_color();
                }
                KeyCode::Right | KeyCode::Up => {
                    self.color.a = (self.color.a + step).clamp(0.0, 1.0);
                    self.sync_fields_from_color();
                }
                _ => return false,
            },
        }

        if !matches!(self.keyboard_target, ColorDragTarget::Alpha) {
            self.color = Color::from_hsv(HsvColor { h: self.hue, s: hsv.s, v: hsv.v, a: hsv.a });
            self.sync_fields_from_color();
        }

        let color_changed = self.color_changed_from_input(old_color, ctx);
        if !color_changed && (self.hue - old_hue).abs() > f32::EPSILON {
            ctx.request_repaint();
        }
        color_changed || (self.hue - old_hue).abs() > f32::EPSILON
    }

    fn paint_panel_chrome(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        paint_shadow(ctx, self.bounds, spacing.radius_lg);
        ctx.encoder
            .draw_rect(self.bounds, soft_border(tokens.border), spacing.radius_lg);
        ctx.encoder.draw_rect(
            self.bounds.inset(1.0, 1.0),
            mix_color(tokens.popover, tokens.foreground, 0.018),
            spacing.radius_lg - 1.0,
        );
        if self.eyedropper_active {
            ctx.encoder.draw_rect(
                self.bounds.inset(2.0, 2.0),
                color_with_alpha(tokens.primary, 0.26),
                spacing.radius_lg - 2.0,
            );
        }
    }

    fn paint_swatch(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let border = self.swatch_rect();
        let swatch = border.inset(2.0, 2.0);
        ctx.encoder.draw_rect(border, soft_border(tokens.border), spacing.radius_lg);
        ctx.encoder.draw_rect(
            border.inset(1.0, 1.0),
            tokens.popover,
            spacing.radius_lg - 1.0,
        );
        paint_checkerboard(ctx, swatch, 5.0, spacing.radius_md);
        ctx.encoder.draw_rect(swatch, self.color, spacing.radius_md);
    }

    fn paint_eyedropper_button(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let rect = self.eyedropper_rect();
        let active = self.eyedropper_active;
        let fill = if !self.enabled {
            mix_color(tokens.popover, tokens.muted, 0.36)
        } else if active {
            mix_color(tokens.popover, tokens.primary, 0.18)
        } else if self.eyedropper_pressed {
            mix_color(tokens.popover, tokens.foreground, 0.08)
        } else if self.eyedropper_hovered {
            mix_color(tokens.popover, tokens.foreground, 0.055)
        } else {
            mix_color(tokens.popover, tokens.foreground, 0.025)
        };
        let icon = if !self.enabled {
            tokens.muted_foreground
        } else if active {
            tokens.primary
        } else {
            tokens.popover_foreground
        };

        ctx.encoder.draw_rect(rect, soft_border(tokens.border), spacing.radius_md);
        ctx.encoder.draw_rect(rect.inset(1.0, 1.0), fill, spacing.radius_md - 1.0);
        ctx.encoder.draw_line(
            Point::new(rect.x + 10.0, rect.y + 18.0),
            Point::new(rect.x + 18.0, rect.y + 10.0),
            2.0,
            icon,
        );
        ctx.encoder.draw_line(
            Point::new(rect.x + 15.0, rect.y + 8.0),
            Point::new(rect.x + 20.0, rect.y + 13.0),
            2.0,
            icon,
        );
        ctx.encoder.draw_line(
            Point::new(rect.x + 8.0, rect.y + 20.0),
            Point::new(rect.x + 12.0, rect.y + 16.0),
            2.0,
            icon,
        );
        ctx.encoder
            .draw_rect(Rect::new(rect.x + 7.0, rect.y + 21.0, 4.0, 2.0), icon, 1.0);
    }

    fn paint_color_area(&self, ctx: &mut PaintContext) {
        match self.area_mode {
            ColorPickerAreaMode::Square => self.paint_square_color_area(ctx),
            ColorPickerAreaMode::Wheel => self.paint_wheel_color_area(ctx),
        }
    }

    fn paint_square_color_area(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let outer = self.color_area_rect();
        let area = outer.inset(1.0, 1.0);
        ctx.encoder.draw_rect(outer, soft_border(tokens.border), spacing.radius_lg);
        let hue = Color::from_hsv(HsvColor { h: self.hue, s: 1.0, v: 1.0, a: 1.0 });
        ctx.encoder.draw_rect(area, hue, spacing.radius_lg - 1.0);
        ctx.encoder.draw_gradient_rect(
            area,
            [
                Color::WHITE,
                Color::TRANSPARENT,
                Color::WHITE,
                Color::TRANSPARENT,
            ],
            spacing.radius_lg - 1.0,
        );
        ctx.encoder.draw_gradient_rect(
            area,
            [
                Color::TRANSPARENT,
                Color::TRANSPARENT,
                Color::BLACK,
                Color::BLACK,
            ],
            spacing.radius_lg - 1.0,
        );

        let hsv = self.color.to_hsv();
        let x = area.x + area.width * hsv.s;
        let y = area.y + area.height * (1.0 - hsv.v);
        self.paint_crosshair(ctx, Point::new(x, y));
    }

    fn paint_wheel_color_area(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let outer = self.color_area_rect();
        ctx.encoder.draw_rect(
            outer,
            mix_color(tokens.popover, tokens.foreground, 0.025),
            spacing.radius_lg,
        );

        let wheel = self.color_wheel_rect();
        let center = wheel.center();
        let radius = wheel.width.min(wheel.height) * 0.5;
        ctx.encoder.draw_rect(
            wheel.inset(-2.0, -2.0),
            soft_border(tokens.border),
            radius + 2.0,
        );
        let hsv = self.color.to_hsv();
        let center_color = Color::from_hsv(HsvColor { h: self.hue, s: 0.0, v: hsv.v, a: 1.0 });
        let mut vertices = Vec::with_capacity(WHEEL_SEGMENTS * 3);
        for segment in 0..WHEEL_SEGMENTS {
            let a0 = segment as f32 / WHEEL_SEGMENTS as f32 * TAU;
            let a1 = (segment + 1) as f32 / WHEEL_SEGMENTS as f32 * TAU;
            let h0 = a0.to_degrees();
            let h1 = a1.to_degrees();
            let p0 = Point::new(center.x + a0.cos() * radius, center.y + a0.sin() * radius);
            let p1 = Point::new(center.x + a1.cos() * radius, center.y + a1.sin() * radius);
            let c0 = Color::from_hsv(HsvColor { h: h0, s: 1.0, v: hsv.v, a: 1.0 });
            let c1 = Color::from_hsv(HsvColor { h: h1, s: 1.0, v: hsv.v, a: 1.0 });
            vertices.push((center, center_color));
            vertices.push((p0, c0));
            vertices.push((p1, c1));
        }
        ctx.encoder.draw_colored_triangles_in_rect(&vertices, wheel, radius);

        let angle = self.hue.to_radians();
        let r = radius * hsv.s.clamp(0.0, 1.0);
        self.paint_crosshair(
            ctx,
            Point::new(center.x + angle.cos() * r, center.y + angle.sin() * r),
        );
    }

    fn paint_hue_bar(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let bar = self.hue_bar_rect();
        ctx.encoder.draw_rect(bar.inset(-1.0, -1.0), soft_border(tokens.border), 7.0);

        let segment_w = bar.width / HUE_SEGMENTS as f32;
        for segment in 0..HUE_SEGMENTS {
            let h0 = segment as f32 / HUE_SEGMENTS as f32 * 360.0;
            let h1 = (segment + 1) as f32 / HUE_SEGMENTS as f32 * 360.0;
            let c0 = Color::from_hsv(HsvColor { h: h0, s: 1.0, v: 1.0, a: 1.0 });
            let c1 = Color::from_hsv(HsvColor { h: h1, s: 1.0, v: 1.0, a: 1.0 });
            ctx.encoder.draw_gradient_rect(
                Rect::new(
                    bar.x + segment as f32 * segment_w,
                    bar.y,
                    segment_w + 0.5,
                    bar.height,
                ),
                [c0, c1, c0, c1],
                0.0,
            );
        }

        let x = bar.x + bar.width * (self.hue / 360.0).clamp(0.0, 1.0);
        self.paint_bar_handle(ctx, bar, x);
    }

    fn paint_alpha_bar(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let bar = self.alpha_bar_rect();
        ctx.encoder.draw_rect(bar.inset(-1.0, -1.0), soft_border(tokens.border), 7.0);
        paint_checkerboard(ctx, bar, 6.0, 7.0);

        let mut transparent = self.color;
        transparent.a = 0.0;
        let mut opaque = self.color;
        opaque.a = 1.0;
        ctx.encoder
            .draw_gradient_rect(bar, [transparent, opaque, transparent, opaque], 0.0);

        let x = bar.x + bar.width * self.color.a.clamp(0.0, 1.0);
        self.paint_bar_handle(ctx, bar, x);
    }

    fn paint_crosshair(&self, ctx: &mut PaintContext, point: Point) {
        let shadow = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 };
        ctx.encoder.draw_rect(
            Rect::new(point.x - 7.0, point.y - 6.0, 14.0, 14.0),
            shadow,
            7.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(point.x - 6.0, point.y - 6.0, 12.0, 12.0),
            Color::WHITE,
            6.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(point.x - 3.0, point.y - 3.0, 6.0, 6.0),
            Color::BLACK,
            3.0,
        );
    }

    fn paint_bar_handle(&self, ctx: &mut PaintContext, bar: Rect, x: f32) {
        let dark = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.8 };
        let light = Color::WHITE;
        let x = x.clamp(bar.x, bar.x + bar.width);
        ctx.encoder.draw_rect(
            Rect::new(x - 2.0, bar.y - 3.0, 4.0, bar.height + 6.0),
            dark,
            2.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(x - 1.0, bar.y - 2.0, 2.0, bar.height + 4.0),
            light,
            1.0,
        );
    }
}

impl Widget for ColorPicker {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(PICKER_WIDTH, PICKER_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.layout_fields();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.eyedropper_active
                || self.drag_target.is_some()
                || self.field_pointer_captured.is_some()
            {
                ctx.set_cursor(CursorRequest::Default);
                ctx.set_eyedropper(false, None);
                ctx.release_pointer_capture(self.id);
            }
            self.focused = false;
            self.focus_visible = false;
            self.mode_menu_open = false;
            self.cancel_interaction();
            return EventResult::Ignored;
        }

        // ── Eyedropper mode: maintain capture and feed events ──────────────
        if self.eyedropper_active {
            match event {
                UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                    self.cancel_eyedropper();
                    ctx.set_cursor(CursorRequest::Default);
                    ctx.set_eyedropper(false, None);
                    ctx.release_pointer_capture(self.id);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                UiEvent::FocusLost => {
                    self.focused = false;
                    self.focus_visible = false;
                    self.blur_fields(ctx);
                    return EventResult::Handled;
                }
                UiEvent::MouseMove { position, .. } => {
                    self.mode_hovered = self.hovered_mode_at(*position);
                    self.eyedropper_hovered = self.eyedropper_rect().contains(*position);
                    return EventResult::Handled;
                }
                UiEvent::EyedropperSample { color } => {
                    self.eyedropper_active = false;
                    self.eyedropper_pressed = false;
                    let old = self.color;
                    self.set_color(*color);
                    self.color_changed_from_input(old, ctx);
                    ctx.set_cursor(CursorRequest::Default);
                    ctx.set_eyedropper(false, None);
                    ctx.release_pointer_capture(self.id);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                UiEvent::EyedropperCancel => {
                    self.cancel_eyedropper();
                    ctx.set_cursor(CursorRequest::Default);
                    ctx.set_eyedropper(false, None);
                    ctx.release_pointer_capture(self.id);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                _ => return EventResult::Handled,
            }
        }

        if let Some(index) = self.field_pointer_captured {
            if index < self.active_fields().len()
                && self.send_field_event(index, event, ctx) == EventResult::Handled
            {
                return EventResult::Handled;
            }
        }

        match event {
            UiEvent::MouseMove { position, .. } if self.drag_target.is_some() => {
                if let Some(target) = self.drag_target {
                    self.update_from_drag(target, *position, ctx);
                }
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.drag_target.is_some() => {
                self.drag_target = None;
                ctx.release_pointer_capture(self.id);
                return EventResult::Handled;
            }
            UiEvent::MouseMove { position, .. } => {
                self.mode_hovered = self.hovered_mode_at(*position);
                self.eyedropper_hovered = self.eyedropper_rect().contains(*position);
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                self.focus_visible = false;
                if self.eyedropper_rect().contains(*position) {
                    self.eyedropper_pressed = true;
                    self.mode_menu_open = false;
                    self.mode_pressed = None;
                    self.mode_hovered = None;
                    ctx.request_pointer_capture(self.id);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if self.mode_trigger_rect().contains(*position) {
                    self.mode_menu_open = !self.mode_menu_open;
                    self.mode_pressed = None;
                    self.mode_hovered = self.mode_menu_open.then_some(self.mode);
                    return EventResult::Handled;
                }
                if self.mode_menu_open {
                    if let Some(mode) = self.hovered_mode_at(*position) {
                        self.mode_pressed = Some(mode);
                        return EventResult::Handled;
                    }
                    if !self.mode_chrome_contains(*position) {
                        self.close_mode_menu();
                        return EventResult::Handled;
                    }
                }
                if let Some(target) = self.drag_target_at(*position) {
                    self.drag_target = Some(target);
                    self.keyboard_target = target;
                    ctx.request_pointer_capture(self.id);
                    self.update_from_drag(target, *position, ctx);
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if self.eyedropper_pressed {
                    self.eyedropper_pressed = false;
                    ctx.release_pointer_capture(self.id);
                    if self.eyedropper_rect().contains(*position) {
                        self.begin_eyedropper();
                        // Re-request pointer capture so we stay captured
                        // during the entire eyedropper session.
                        ctx.request_pointer_capture(self.id);
                        // Request crosshair cursor for the platform.
                        ctx.set_cursor(CursorRequest::Crosshair);
                        ctx.set_eyedropper(true, Some(self.eyedropper_rect().center()));
                    }
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                let released = self.hovered_mode_at(*position);
                if let (Some(pressed), Some(released)) = (self.mode_pressed, released) {
                    self.mode_pressed = None;
                    if pressed == released {
                        self.set_mode(released);
                    }
                    self.close_mode_menu();
                    return EventResult::Handled;
                }
                self.mode_pressed = None;
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } if self.mode_menu_open => {
                self.close_mode_menu();
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Down, .. } if self.mode_menu_open => {
                self.mode_hovered = Some(self.next_mode_selection(1));
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Up, .. } if self.mode_menu_open => {
                self.mode_hovered = Some(self.next_mode_selection(-1));
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, .. }
                if self.mode_menu_open =>
            {
                let selected = self.mode_hovered.unwrap_or(self.mode);
                self.set_mode(selected);
                self.close_mode_menu();
                return EventResult::Handled;
            }
            UiEvent::FocusGained => {
                self.focused = true;
                self.focus_visible = true;
                return EventResult::Handled;
            }
            UiEvent::FocusLost => {
                self.focused = false;
                self.focus_visible = false;
                self.cancel_interaction();
                self.blur_fields(ctx);
                return EventResult::Handled;
            }
            _ => {}
        }

        if let Some(position) = Self::pointer_position(event) {
            if matches!(event, UiEvent::MouseDown { button: MouseButton::Left, .. }) {
                if let Some(index) = self.field_at_position(position) {
                    self.blur_fields(ctx);
                    self.focused_field = Some(index);
                    if self.send_field_event(index, event, ctx) == EventResult::Handled {
                        return EventResult::Handled;
                    }
                } else if self.bounds.contains(position) {
                    self.blur_fields(ctx);
                    return EventResult::Handled;
                }
            }
        } else if let Some(index) = self.focused_field {
            if index < self.active_fields().len()
                && self.send_field_event(index, event, ctx) == EventResult::Handled
            {
                return EventResult::Handled;
            }
        }

        if let UiEvent::KeyDown { key, modifiers } = event {
            if self.nudge_keyboard_target(*key, *modifiers, ctx) {
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let font_size = ctx.theme.typography.body.font_size;

        self.paint_panel_chrome(ctx);
        if self.focus_visible {
            let mut ring = tokens.ring;
            ring.a = 0.38;
            let rect = self.bounds.inset(-2.0, -2.0);
            ctx.encoder.draw_rect(rect, ring, ctx.theme.spacing.radius_md + 2.0);
        }
        if self.show_swatch {
            self.paint_swatch(ctx);
        }
        self.paint_eyedropper_button(ctx);
        self.paint_color_area(ctx);
        self.paint_hue_bar(ctx);
        self.paint_alpha_bar(ctx);

        paint_menu_trigger(
            ctx,
            self.mode_trigger_rect(),
            self.mode.label(),
            self.mode_menu_open,
        );

        for (index, field) in self.active_fields().iter().enumerate() {
            ctx.encoder.draw_text(
                field.label(),
                font_size,
                self.field_label_pos(index),
                tokens.muted_foreground,
            );
            self.fields[index].paint(ctx);
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if !self.mode_menu_open {
            return;
        }

        let menu = self.mode_menu_rect();
        paint_menu_popup_chrome(ctx, menu);

        for mode in MODES {
            let rect = self.mode_item_rect(mode);
            paint_menu_row(
                ctx,
                rect,
                mode.label(),
                MenuRowPaint {
                    enabled: true,
                    active: self.mode == mode,
                    hovered: self.mode_hovered == Some(mode),
                },
            );
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
            || (self.mode_menu_open && self.mode_menu_rect().contains(point))
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }
}

fn parse_u8_channel(input: &str) -> Option<f32> {
    let value = input.trim().parse::<f32>().ok()?;
    Some((value.round() / 255.0).clamp(0.0, 1.0))
}

fn parse_percent_channel(input: &str) -> Option<f32> {
    let value = input.trim().trim_end_matches('%').parse::<f32>().ok()?;
    Some((value / 100.0).clamp(0.0, 1.0))
}

fn parse_hue(input: &str) -> Option<f32> {
    Some(input.trim().parse::<f32>().ok()?.rem_euclid(360.0))
}

fn int_channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn percent_channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 100.0).round() as u8
}

fn format_number(value: f32) -> String {
    if (value.round() - value).abs() <= 0.01 {
        format!("{:.0}", value)
    } else {
        format!("{:.1}", value)
    }
}

/// Configuration for [`ColorPickerTrigger`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorPickerTriggerOptions {
    /// Closed trigger width.
    pub trigger_width: f32,
    /// Closed trigger height.
    pub trigger_height: f32,
    /// Popup picker width.
    pub popup_width: f32,
    /// Popup picker height.
    pub popup_height: f32,
}

impl Default for ColorPickerTriggerOptions {
    fn default() -> Self {
        Self {
            trigger_width: 32.0,
            trigger_height: 32.0,
            popup_width: PICKER_WIDTH,
            popup_height: PICKER_HEIGHT,
        }
    }
}

/// Compact color trigger that opens a full [`ColorPicker`] in the overlay pass.
pub struct ColorPickerTrigger {
    id: WidgetId,
    bounds: Rect,
    picker: ColorPicker,
    options: ColorPickerTriggerOptions,
    open: bool,
    pressed: bool,
    enabled: bool,
    picker_pointer_captured: bool,
}

impl ColorPickerTrigger {
    /// Create a trigger with the given initial color.
    pub fn new(color: Color) -> Self {
        Self::with_options(color, ColorPickerTriggerOptions::default())
    }

    /// Create a trigger with explicit sizing options.
    pub fn with_options(color: Color, options: ColorPickerTriggerOptions) -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            picker: ColorPicker::new(color),
            options,
            open: false,
            pressed: false,
            enabled: true,
            picker_pointer_captured: false,
        }
    }

    /// Current color.
    pub fn color(&self) -> Color {
        self.picker.color()
    }

    /// Replace the current color.
    pub fn set_color(&mut self, color: Color) {
        self.picker.set_color(color);
    }

    /// Dispatch a value-aware action whenever the embedded picker changes color.
    pub fn on_change(mut self, action: impl Fn(Color) -> Action + 'static) -> Self {
        self.picker = self.picker.on_change(action);
        self
    }

    /// Set whether the trigger and embedded picker accept user input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// Disable the trigger and embedded picker.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the trigger and embedded picker accept user input.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set whether the trigger and embedded picker accept user input.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.picker.set_enabled(enabled);
        if !enabled {
            self.open = false;
            self.pressed = false;
        }
    }

    /// Access the embedded picker for inspector-specific configuration.
    pub fn picker_mut(&mut self) -> &mut ColorPicker {
        &mut self.picker
    }

    /// Whether the popup picker is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn popup_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y + self.bounds.height + 4.0,
            self.options.popup_width.max(1.0),
            self.options.popup_height.max(1.0),
        )
    }

    fn color_rect(&self) -> Rect {
        self.bounds.inset(4.0, 4.0)
    }

    fn translate_picker_capture_request(&mut self, ctx: &mut EventContext) {
        match ctx.requests.pointer_capture {
            Some(PointerCaptureRequest::Capture(id)) if id == self.picker.id() => {
                self.picker_pointer_captured = true;
                ctx.request_pointer_capture(self.id);
            }
            Some(PointerCaptureRequest::Release(id)) if id == self.picker.id() => {
                self.picker_pointer_captured = false;
                ctx.release_pointer_capture(self.id);
            }
            _ => {}
        }
    }

    fn popup_should_receive_event(&self, event: &UiEvent) -> bool {
        if self.picker_pointer_captured {
            return true;
        }
        match event {
            UiEvent::MouseDown { position, .. }
            | UiEvent::MouseUp { position, .. }
            | UiEvent::MouseMove { position, .. }
            | UiEvent::MouseWheel { position, .. }
            | UiEvent::DragEnter { position, .. }
            | UiEvent::DragOver { position }
            | UiEvent::Drop { position, .. } => self.popup_rect().contains(*position),
            _ => true,
        }
    }

    fn close_popup(&mut self, ctx: &mut EventContext) {
        self.open = false;
        self.pressed = false;
        self.picker.cancel_interaction();
        if self.picker_pointer_captured {
            self.picker_pointer_captured = false;
            ctx.release_pointer_capture(self.id);
        }
    }
}

impl Widget for ColorPickerTrigger {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(
            self.options.trigger_width,
            self.options.trigger_height,
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.picker.layout(self.popup_rect());
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.open
                || self.pressed
                || self.picker_pointer_captured
                || self.picker.is_eyedropper_active()
            {
                self.close_popup(ctx);
                self.picker.cancel_eyedropper();
                ctx.set_cursor(CursorRequest::Default);
                ctx.set_eyedropper(false, None);
                ctx.release_pointer_capture(self.id);
            }
            self.open = false;
            self.pressed = false;
            return EventResult::Ignored;
        }

        let eyedropper_active = self.picker.is_eyedropper_active();

        if self.open {
            // During eyedropper mode, don't close the popup on outside clicks.
            // The picker holds pointer capture and the user is sampling a color.
            if !eyedropper_active {
                if let UiEvent::MouseDown { position, button: MouseButton::Left, .. } = event {
                    if !self.bounds.contains(*position) && !self.popup_rect().contains(*position) {
                        self.close_popup(ctx);
                        return EventResult::Handled;
                    }
                }
            }

            // During eyedropper, always route events to the picker (it holds
            // pointer capture). When not eyedropping, use normal hit-test routing.
            let should_route = if eyedropper_active {
                true
            } else {
                self.popup_should_receive_event(event)
            };

            if should_route {
                let handled_by_picker = self.picker.event(event, ctx) == EventResult::Handled;
                self.translate_picker_capture_request(ctx);
                if handled_by_picker {
                    return EventResult::Handled;
                }
            }
        }

        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.pressed = true;
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } if self.pressed => {
                self.pressed = false;
                if self.bounds.contains(*position) {
                    self.open = !self.open;
                }
                EventResult::Handled
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } if self.open => {
                if !eyedropper_active
                    && !self.bounds.contains(*position)
                    && !self.popup_rect().contains(*position)
                {
                    self.close_popup(ctx);
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } if self.open => {
                self.close_popup(ctx);
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let fill = if !self.enabled {
            mix_color(tokens.popover, tokens.muted, 0.36)
        } else if self.open {
            mix_color(tokens.popover, tokens.primary, 0.08)
        } else if self.pressed {
            mix_color(tokens.popover, tokens.foreground, 0.06)
        } else {
            mix_color(tokens.popover, tokens.foreground, 0.025)
        };
        ctx.encoder
            .draw_rect(self.bounds, soft_border(tokens.border), spacing.radius_md);
        ctx.encoder
            .draw_rect(self.bounds.inset(1.0, 1.0), fill, spacing.radius_md - 1.0);
        let color_rect = self.color_rect();
        ctx.encoder.draw_rect(
            color_rect.inset(-1.0, -1.0),
            soft_border(tokens.border),
            spacing.radius_sm,
        );
        paint_checkerboard(ctx, color_rect, 6.0, 7.0);
        ctx.encoder.draw_rect(color_rect, self.color(), spacing.radius_sm);
        if !self.enabled {
            ctx.encoder.draw_rect(
                color_rect,
                color_with_alpha(tokens.popover, 0.36),
                spacing.radius_sm,
            );
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if self.open {
            self.picker.paint(ctx);
            self.picker.paint_overlay(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        if self.open || self.picker.is_eyedropper_active() {
            return true;
        }
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint::{shadow_color, shadow_rect};
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        rect_colors: Vec<Color>,
        gradient_rects: Vec<Rect>,
        colored_triangle_vertices: usize,
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
            self.rect_colors.push(color);
        }

        fn draw_gradient_rect(&mut self, bounds: Rect, _colors: [Color; 4], _corner_radius: f32) {
            self.gradient_rects.push(bounds);
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}

        fn draw_colored_triangles(&mut self, vertices: &[(Point, Color)]) {
            self.colored_triangle_vertices += vertices.len();
        }

        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.into());
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

    fn color_action(color: Color) -> Action {
        let [r, g, b, a] = color.to_rgba8();
        Action::Custom {
            namespace: "test.color".into(),
            name: format!("rgba:{r},{g},{b},{a}"),
            payload: Default::default(),
        }
    }

    #[test]
    fn disabled_picker_ignores_eyedropper_and_focus() {
        let mut picker = ColorPicker::new(Color::BLACK).disabled();
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));
        let eyedropper = picker.eyedropper_rect().center();
        let mut ctx = event_ctx();

        picker.begin_eyedropper();
        assert!(!picker.is_enabled());
        assert!(!picker.is_eyedropper_active());
        assert!(!picker.can_focus());
        assert_eq!(
            picker.event(
                &UiEvent::MouseDown {
                    position: eyedropper,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert!(!picker.is_eyedropper_active());
        assert!(ctx.requests.pointer_capture.is_none());
    }

    #[test]
    fn disabling_active_eyedropper_cleans_platform_request_on_next_event() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));
        picker.begin_eyedropper();
        picker.set_enabled(false);
        let mut ctx = event_ctx();

        assert_eq!(
            picker.event(
                &UiEvent::MouseMove {
                    position: picker.eyedropper_rect().center(),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert!(!picker.is_eyedropper_active());
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(picker.id()))
        );
        assert_eq!(
            ctx.requests.eyedropper,
            Some(mondrian_ui_core::widget::EyedropperRequest { active: false, hotspot: None })
        );
    }

    #[test]
    fn disabled_trigger_does_not_open_popup() {
        let mut trigger = ColorPickerTrigger::new(Color::BLACK).disabled();
        trigger.layout(Rect::new(8.0, 8.0, 32.0, 32.0));
        let center = trigger.bounds.center();
        let mut ctx = event_ctx();

        assert!(!trigger.is_enabled());
        assert_eq!(
            trigger.event(
                &UiEvent::MouseDown {
                    position: center,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert_eq!(
            trigger.event(
                &UiEvent::MouseUp {
                    position: center,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert!(!trigger.is_open());
        assert!(ctx.requests.pointer_capture.is_none());
    }

    #[test]
    fn hex_fields_sync_from_color() {
        let picker = ColorPicker::new(Color::from_rgba8(51, 102, 153, 128));

        assert_eq!(picker.fields[0].text(), "#33669980");
    }

    #[test]
    fn rgb_fields_apply_to_color() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.set_mode(ColorPickerMode::Rgb);
        picker.fields[0].set_text("255".into());
        picker.fields[1].set_text("128".into());
        picker.fields[2].set_text("0".into());
        picker.fields[3].set_text("50".into());

        assert!(picker.apply_visible_fields());
        assert_eq!(picker.color().to_rgba8(), [255, 128, 0, 128]);
    }

    #[test]
    fn hsl_fields_apply_to_color() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.set_mode(ColorPickerMode::Hsl);
        picker.fields[0].set_text("240".into());
        picker.fields[1].set_text("100".into());
        picker.fields[2].set_text("50".into());
        picker.fields[3].set_text("100".into());

        assert!(picker.apply_visible_fields());
        assert_eq!(picker.color().to_rgba8(), [0, 0, 255, 255]);
    }

    #[test]
    fn cmyk_fields_apply_to_color() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.set_mode(ColorPickerMode::Cmyk);
        picker.fields[0].set_text("100".into());
        picker.fields[1].set_text("0".into());
        picker.fields[2].set_text("0".into());
        picker.fields[3].set_text("0".into());
        picker.fields[4].set_text("100".into());

        assert!(picker.apply_visible_fields());
        assert_eq!(picker.color().to_rgba8(), [0, 255, 255, 255]);
    }

    #[test]
    fn invalid_field_does_not_change_color() {
        let mut picker = ColorPicker::new(Color::from_rgba8(10, 20, 30, 255));
        picker.set_mode(ColorPickerMode::Rgb);
        picker.fields[0].set_text("nope".into());

        assert!(!picker.apply_visible_fields());
        assert_eq!(picker.color().to_rgba8(), [10, 20, 30, 255]);
    }

    #[test]
    fn cmyk_fields_fit_on_one_row() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.set_mode(ColorPickerMode::Cmyk);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));

        let first = picker.field_rect(0);
        let last = picker.field_rect(4);

        assert_eq!(first.y, last.y);
        assert!(last.x + last.width <= picker.bounds.x + picker.bounds.width - 10.0);
        assert!(last.width >= 30.0);
    }

    #[test]
    fn hex_label_has_room_before_field() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.set_mode(ColorPickerMode::Hex);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));

        let label = picker.field_label_pos(0);
        let field = picker.field_rect(0);

        assert!(field.x - label.x >= 24.0);
        assert!(field.width > 200.0);
    }

    #[test]
    fn mode_change_relayouts_text_inputs_immediately() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));

        picker.set_mode(ColorPickerMode::Cmyk);
        let expected = picker.field_rect(4);

        assert!(picker.fields[4].hit_test(expected.center()));
    }

    #[test]
    fn hidden_swatch_does_not_paint_over_compact_mode_dropdown() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.set_show_swatch(false);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));
        let swatch = picker.swatch_rect();
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT),
        };

        picker.paint(&mut ctx);

        assert!(!encoder.rects.contains(&swatch));
    }

    #[test]
    fn trigger_defaults_to_square_and_can_hide_popup_swatch() {
        let mut trigger = ColorPickerTrigger::new(Color::BLACK);

        assert_eq!(
            trigger.measure(LayoutConstraint::loose(0.0, 0.0)).width,
            32.0
        );
        assert_eq!(
            trigger.measure(LayoutConstraint::loose(0.0, 0.0)).height,
            32.0
        );
        assert!(trigger.picker.show_swatch());

        trigger.picker_mut().set_show_swatch(false);
        assert!(!trigger.picker.show_swatch());
    }

    #[test]
    fn clicking_mode_dropdown_switches_mode() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let trigger = picker.mode_trigger_rect();
        let trigger_position = Point::new(
            trigger.x + trigger.width * 0.5,
            trigger.y + trigger.height * 0.5,
        );
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position: trigger_position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(picker.mode_menu_open);

        let hsv = picker.mode_item_rect(ColorPickerMode::Hsv);
        let position = Point::new(hsv.x + hsv.width * 0.5, hsv.y + hsv.height * 0.5);
        picker.event(
            &UiEvent::MouseMove { position, modifiers: Modifiers::none() },
            &mut ctx,
        );
        picker.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        picker.event(
            &UiEvent::MouseUp {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(picker.mode(), ColorPickerMode::Hsv);
        assert!(!picker.mode_menu_open);
        assert_eq!(picker.fields[0].text(), "0");
    }

    #[test]
    fn mode_dropdown_keyboard_navigation_selects_mode() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));
        let trigger = picker.mode_trigger_rect().center();
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position: trigger,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(picker.mode_hovered, Some(ColorPickerMode::Hex));

        picker.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(picker.mode_hovered, Some(ColorPickerMode::Rgb));
        picker.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(picker.mode(), ColorPickerMode::Rgb);
        assert!(!picker.mode_menu_open);
    }

    #[test]
    fn mode_dropdown_keyboard_navigation_wraps_up() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));
        let trigger = picker.mode_trigger_rect().center();
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position: trigger,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        picker.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(picker.mode_hovered, Some(ColorPickerMode::Cmyk));
    }

    #[test]
    fn field_click_routes_to_target_field_without_being_swallowed_by_previous_field() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.set_mode(ColorPickerMode::Rgb);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));
        let green = picker.field_rect(1).center();
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position: green,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        picker.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        picker.event(&UiEvent::TextInput("200".into()), &mut ctx);

        assert_eq!(picker.color().to_rgba8(), [0, 200, 0, 255]);
    }

    #[test]
    fn blank_click_inside_picker_blurs_field_without_starting_capture() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.set_mode(ColorPickerMode::Rgb);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));
        let red = picker.field_rect(0).center();
        let blank = Point::new(
            picker.bounds.x + picker.bounds.width - 8.0,
            picker.fields_top(),
        );
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position: red,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        picker.event(
            &UiEvent::MouseDown {
                position: blank,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(picker.focused_field, None);
        assert_eq!(picker.field_pointer_captured, None);
    }

    #[test]
    fn dragging_color_area_updates_saturation_and_value() {
        let mut picker = ColorPicker::new(Color::from_hsv(HsvColor {
            h: 120.0,
            s: 0.0,
            v: 0.0,
            a: 1.0,
        }));
        picker.hue = 120.0;
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let area = picker.color_area_rect();
        let position = Point::new(area.x + area.width, area.y);
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        let hsv = picker.color().to_hsv();
        assert!((hsv.h - 120.0).abs() <= 0.1);
        assert!((hsv.s - 1.0).abs() <= 0.01);
        assert!((hsv.v - 1.0).abs() <= 0.01);
    }

    #[test]
    fn dragging_hue_bar_preserves_saturation_and_value() {
        let mut picker = ColorPicker::new(Color::from_hsv(HsvColor {
            h: 0.0,
            s: 0.5,
            v: 0.75,
            a: 1.0,
        }));
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let bar = picker.hue_bar_rect();
        let position = Point::new(bar.x + bar.width * 0.5, bar.y + bar.height * 0.5);
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        let hsv = picker.color().to_hsv();
        assert!((hsv.h - 180.0).abs() <= 0.1);
        assert!((hsv.s - 0.5).abs() <= 0.01);
        assert!((hsv.v - 0.75).abs() <= 0.01);
    }

    #[test]
    fn wheel_area_drag_updates_hue_and_saturation() {
        let mut picker =
            ColorPicker::new(Color::from_hsv(HsvColor { h: 0.0, s: 0.0, v: 1.0, a: 1.0 }));
        picker.set_area_mode(ColorPickerAreaMode::Wheel);
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let wheel = picker.color_wheel_rect();
        let center = wheel.center();
        let position = Point::new(center.x, center.y + wheel.height * 0.5);
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        let hsv = picker.color().to_hsv();
        assert!(hsv.h > 89.0 && hsv.h < 91.0);
        assert!(hsv.s > 0.99);
        assert!(hsv.v > 0.99);
    }

    #[test]
    fn dragging_alpha_bar_updates_alpha_and_fields() {
        let mut picker = ColorPicker::new(Color::from_rgba8(51, 102, 153, 255));
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let bar = picker.alpha_bar_rect();
        let position = Point::new(bar.x + bar.width * 0.5, bar.y + bar.height * 0.5);
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(picker.color().to_rgba8(), [51, 102, 153, 128]);
        assert_eq!(picker.fields[0].text(), "#33669980");
    }

    #[test]
    fn dragging_alpha_bar_dispatches_color_change_and_requests_repaint() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut picker =
            ColorPicker::new(Color::from_rgba8(51, 102, 153, 255)).on_change(color_action);
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let bar = picker.alpha_bar_rect();

        picker.event(
            &UiEvent::MouseDown {
                position: Point::new(bar.x + bar.width * 0.5, bar.center().y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(picker.color().to_rgba8(), [51, 102, 153, 128]);
        assert_eq!(actions.borrow().as_slice(), &[color_action(picker.color())]);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn dragging_alpha_bar_at_same_value_does_not_dispatch_duplicate_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut picker =
            ColorPicker::new(Color::from_rgba8(51, 102, 153, 255)).on_change(color_action);
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let bar = picker.alpha_bar_rect();

        picker.event(
            &UiEvent::MouseDown {
                position: Point::new(bar.x + bar.width, bar.center().y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn eyedropper_sample_dispatches_color_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut picker = ColorPicker::new(Color::BLACK).on_change(color_action);

        picker.begin_eyedropper();
        picker.event(
            &UiEvent::EyedropperSample { color: Color::from_rgba8(1, 2, 3, 4) },
            &mut ctx,
        );

        assert_eq!(actions.borrow().as_slice(), &[color_action(picker.color())]);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn opening_mode_dropdown_does_not_dispatch_color_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut picker = ColorPicker::new(Color::BLACK).on_change(color_action);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));

        picker.event(
            &UiEvent::MouseDown {
                position: picker.mode_trigger_rect().center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(picker.mode_menu_open);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn keyboard_nudge_updates_last_color_target() {
        let mut picker = ColorPicker::new(Color::from_hsv(HsvColor {
            h: 120.0,
            s: 0.5,
            v: 0.5,
            a: 1.0,
        }));
        picker.hue = 120.0;
        picker.keyboard_target = ColorDragTarget::ColorArea;
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert!(picker.color().to_hsv().s > 0.5);
    }

    #[test]
    fn eyedropper_sample_updates_color_and_clears_active_state() {
        let mut picker = ColorPicker::new(Color::BLACK);

        picker.begin_eyedropper();
        assert!(picker.is_eyedropper_active());
        picker.apply_sampled_color(Color::from_rgba8(1, 2, 3, 4));

        assert!(!picker.is_eyedropper_active());
        assert_eq!(picker.color().to_rgba8(), [1, 2, 3, 4]);
        assert_eq!(picker.fields[0].text(), "#01020304");
    }

    #[test]
    fn eyedropper_button_enters_sampling_and_escape_cancels() {
        let mut picker = ColorPicker::new(Color::BLACK);
        picker.layout(Rect::new(0.0, 0.0, PICKER_WIDTH, PICKER_HEIGHT));
        let button = picker.eyedropper_rect();
        let position = button.center();
        let mut ctx = event_ctx();

        picker.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(picker.id()))
        );
        picker.event(
            &UiEvent::MouseUp {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        // After MouseUp on eyedropper button we re-request capture for
        // the eyedropper session.
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(picker.id()))
        );
        assert!(picker.is_eyedropper_active());

        picker.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(!picker.is_eyedropper_active());
        // Escape releases capture.
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(picker.id()))
        );
    }

    #[test]
    fn trigger_outside_click_cancels_active_eyedropper() {
        let mut trigger = ColorPickerTrigger::new(Color::BLACK);
        trigger.layout(Rect::new(20.0, 30.0, 32.0, 32.0));
        let trigger_center = trigger.bounds.center();
        let mut ctx = event_ctx();

        trigger.event(
            &UiEvent::MouseDown {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        trigger.event(
            &UiEvent::MouseUp {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(trigger.is_open());

        let eyedropper = trigger.picker.eyedropper_rect().center();
        trigger.event(
            &UiEvent::MouseDown {
                position: eyedropper,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(trigger.id()))
        );
        trigger.event(
            &UiEvent::MouseUp {
                position: eyedropper,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(trigger.picker.is_eyedropper_active());

        // During eyedropper mode, outside clicks do NOT close the popup.
        // The picker holds pointer capture. Instead, the app shell sends
        // EyedropperSample/EyedropperCancel to end eyedropper mode.
        trigger.event(
            &UiEvent::MouseDown {
                position: Point::new(500.0, 500.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        // Popup stays open during eyedropper.
        assert!(trigger.is_open());
        assert!(trigger.picker.is_eyedropper_active());

        // Platform cancellation closes eyedropper.
        trigger.event(&UiEvent::EyedropperCancel, &mut ctx);
        assert!(!trigger.picker.is_eyedropper_active());
    }

    #[test]
    fn trigger_open_hit_test_catches_outside_clicks_for_dismissal() {
        let mut trigger = ColorPickerTrigger::new(Color::BLACK);
        trigger.layout(Rect::new(20.0, 30.0, 32.0, 32.0));
        let trigger_center = trigger.bounds.center();
        let outside = Point::new(500.0, 500.0);
        let mut ctx = event_ctx();

        trigger.event(
            &UiEvent::MouseDown {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        trigger.event(
            &UiEvent::MouseUp {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(trigger.is_open());
        assert!(trigger.hit_test(outside));

        trigger.event(
            &UiEvent::MouseDown {
                position: outside,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(!trigger.is_open());
    }

    #[test]
    fn trigger_translates_inner_text_field_capture_to_trigger_id() {
        let mut trigger = ColorPickerTrigger::new(Color::BLACK);
        trigger.picker_mut().set_mode(ColorPickerMode::Rgb);
        trigger.layout(Rect::new(20.0, 30.0, 32.0, 32.0));
        let trigger_center = trigger.bounds.center();
        let mut ctx = event_ctx();

        trigger.event(
            &UiEvent::MouseDown {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        trigger.event(
            &UiEvent::MouseUp {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(trigger.is_open());

        let red = trigger.picker.field_rect(0).center();
        trigger.event(
            &UiEvent::MouseDown {
                position: red,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(trigger.id()))
        );
    }

    #[test]
    fn paint_draws_swatch_modes_and_active_fields() {
        let mut picker = ColorPicker::new(Color::from_rgba8(51, 102, 153, 255));
        picker.set_mode(ColorPickerMode::Rgb);
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 280.0, 302.0),
        };

        picker.paint(&mut ctx);

        assert!(encoder.rects.len() > 10);
        assert_eq!(encoder.gradient_rects.len(), 9);
        assert!(encoder.texts.iter().any(|text| text == "RGB"));
        assert!(encoder.texts.iter().any(|text| text == "R"));
        assert!(encoder.texts.iter().any(|text| text == "A"));
    }

    #[test]
    fn paint_uses_theme_shadow_token_for_picker_chrome() {
        let mut picker = ColorPicker::new(Color::from_rgba8(51, 102, 153, 255));
        let bounds = Rect::new(12.0, 18.0, 280.0, 302.0);
        picker.layout(bounds);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 400.0),
        };

        picker.paint(&mut ctx);

        let shadow = &theme.spacing.shadow_md;
        assert_eq!(encoder.rects[0], shadow_rect(bounds, shadow));
        assert_eq!(encoder.rect_colors[0], shadow_color(shadow));
    }

    #[test]
    fn paint_wheel_area_draws_colored_triangles() {
        let mut picker = ColorPicker::new(Color::from_rgba8(51, 102, 153, 255));
        picker.set_area_mode(ColorPickerAreaMode::Wheel);
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 280.0, 302.0),
        };

        picker.paint(&mut ctx);

        assert!(encoder.colored_triangle_vertices >= WHEEL_SEGMENTS * 3);
    }

    #[test]
    fn paint_overlay_draws_open_mode_dropdown() {
        let mut picker = ColorPicker::new(Color::from_rgba8(51, 102, 153, 255));
        picker.layout(Rect::new(0.0, 0.0, 280.0, 302.0));
        picker.mode_menu_open = true;
        picker.mode_hovered = Some(ColorPickerMode::Hsv);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 280.0, 400.0),
        };

        picker.paint_overlay(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "HEX"));
        assert!(encoder.texts.iter().any(|text| text == "HSV"));
        assert!(encoder.rects.len() >= MODES.len() + 2);
    }

    #[test]
    fn trigger_opens_picker_in_overlay_and_exposes_color() {
        let mut trigger = ColorPickerTrigger::new(Color::from_rgba8(51, 102, 153, 255));
        trigger.layout(Rect::new(0.0, 0.0, 84.0, 30.0));
        let mut ctx = event_ctx();
        let position = Point::new(12.0, 12.0);

        trigger.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        trigger.event(
            &UiEvent::MouseUp {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(trigger.is_open());
        assert_eq!(trigger.color().to_rgba8(), [51, 102, 153, 255]);

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut paint_ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 400.0),
        };
        trigger.paint_overlay(&mut paint_ctx);
        assert!(encoder.gradient_rects.len() >= 9);
    }

    #[test]
    fn trigger_embedded_picker_dispatches_color_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut trigger =
            ColorPickerTrigger::new(Color::from_rgba8(51, 102, 153, 255)).on_change(color_action);
        trigger.layout(Rect::new(20.0, 30.0, 32.0, 32.0));
        let trigger_center = trigger.bounds.center();

        trigger.event(
            &UiEvent::MouseDown {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        trigger.event(
            &UiEvent::MouseUp {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let bar = trigger.picker.alpha_bar_rect();
        trigger.event(
            &UiEvent::MouseDown {
                position: Point::new(bar.x + bar.width * 0.5, bar.center().y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(trigger.color().to_rgba8(), [51, 102, 153, 128]);
        assert_eq!(
            actions.borrow().as_slice(),
            &[color_action(trigger.color())]
        );
    }

    #[test]
    fn trigger_translates_inner_picker_pointer_capture_to_trigger_id() {
        let mut trigger =
            ColorPickerTrigger::new(Color::from_hsv(HsvColor { h: 0.0, s: 0.0, v: 1.0, a: 1.0 }));
        trigger.picker_mut().set_area_mode(ColorPickerAreaMode::Wheel);
        trigger.layout(Rect::new(20.0, 30.0, 84.0, 30.0));
        let trigger_center = trigger.bounds.center();
        let mut ctx = event_ctx();

        trigger.event(
            &UiEvent::MouseDown {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        trigger.event(
            &UiEvent::MouseUp {
                position: trigger_center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(trigger.is_open());

        let wheel = trigger.picker.color_wheel_rect();
        let wheel_point = Point::new(wheel.center().x, wheel.y + wheel.height);
        trigger.event(
            &UiEvent::MouseDown {
                position: wheel_point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(trigger.id()))
        );

        trigger.event(
            &UiEvent::MouseUp {
                position: wheel_point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(trigger.id()))
        );
    }
}
