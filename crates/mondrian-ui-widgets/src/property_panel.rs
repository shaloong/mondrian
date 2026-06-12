//! Property panel containers for inspector-style UI.
//!
//! This module provides a small reusable layout for labeled editor controls:
//! title, optional subtitle, sections, and rows with a label plus one widget.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Layout options for [`PropertyPanel`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropertyPanelOptions {
    /// Outer padding around panel content.
    pub margin: f32,
    /// Width reserved for row labels.
    pub label_width: f32,
    /// Default row height.
    pub row_height: f32,
    /// Gap between label and control.
    pub control_gap: f32,
    /// Gap between sections.
    pub section_gap: f32,
}

impl Default for PropertyPanelOptions {
    fn default() -> Self {
        Self {
            margin: 12.0,
            label_width: 92.0,
            row_height: 34.0,
            control_gap: 8.0,
            section_gap: 10.0,
        }
    }
}

/// One labeled control row in a [`PropertySection`].
pub struct PropertyRow {
    label: String,
    control: Box<dyn Widget>,
    height: Option<f32>,
    bounds: Rect,
    label_position: Point,
}

impl PropertyRow {
    /// Create a property row from a label and an owned control widget.
    pub fn new(label: impl Into<String>, control: Box<dyn Widget>) -> Self {
        Self {
            label: label.into(),
            control,
            height: None,
            bounds: Rect::ZERO,
            label_position: Point::ZERO,
        }
    }

    /// Set an explicit row height for taller controls.
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = Some(height.max(1.0));
        self
    }

    /// Row label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Current laid out row bounds.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    fn height(&self, fallback: f32) -> f32 {
        self.height.unwrap_or(fallback).max(1.0)
    }
}

/// A titled group of property rows.
pub struct PropertySection {
    title: String,
    rows: Vec<PropertyRow>,
    bounds: Rect,
    header_position: Point,
}

impl PropertySection {
    /// Create an empty property section.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            rows: Vec::new(),
            bounds: Rect::ZERO,
            header_position: Point::ZERO,
        }
    }

    /// Append one row and return the section for builder-style construction.
    pub fn with_row(mut self, row: PropertyRow) -> Self {
        self.rows.push(row);
        self
    }

    /// Append one row.
    pub fn push_row(&mut self, row: PropertyRow) {
        self.rows.push(row);
    }

    /// Section title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Number of rows in this section.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// Inspector-style property panel with labeled rows.
pub struct PropertyPanel {
    id: WidgetId,
    title: String,
    subtitle: Option<String>,
    sections: Vec<PropertySection>,
    options: PropertyPanelOptions,
    bounds: Rect,
    title_position: Point,
    subtitle_position: Point,
}

impl PropertyPanel {
    /// Create a property panel with default layout options.
    pub fn new(title: impl Into<String>) -> Self {
        Self::with_options(title, PropertyPanelOptions::default())
    }

    /// Create a property panel with explicit layout options.
    pub fn with_options(title: impl Into<String>, options: PropertyPanelOptions) -> Self {
        Self {
            id: WidgetId::new(),
            title: title.into(),
            subtitle: None,
            sections: Vec::new(),
            options,
            bounds: Rect::ZERO,
            title_position: Point::ZERO,
            subtitle_position: Point::ZERO,
        }
    }

    /// Set the optional subtitle.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Append a section and return the panel for builder-style construction.
    pub fn with_section(mut self, section: PropertySection) -> Self {
        self.sections.push(section);
        self
    }

    /// Append a section.
    pub fn push_section(&mut self, section: PropertySection) {
        self.sections.push(section);
    }

    /// Number of sections.
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    fn content_rect(&self) -> Rect {
        self.bounds.inset(self.options.margin, self.options.margin)
    }

    fn header_height(&self) -> f32 {
        if self.subtitle.is_some() {
            42.0
        } else {
            26.0
        }
    }

    fn row_at_child_index(&self, index: usize) -> Option<&PropertyRow> {
        let mut remaining = index;
        for section in &self.sections {
            if remaining < section.rows.len() {
                return section.rows.get(remaining);
            }
            remaining -= section.rows.len();
        }
        None
    }

    fn row_at_child_index_mut(&mut self, index: usize) -> Option<&mut PropertyRow> {
        let mut remaining = index;
        for section in &mut self.sections {
            if remaining < section.rows.len() {
                return section.rows.get_mut(remaining);
            }
            remaining -= section.rows.len();
        }
        None
    }
}

impl Widget for PropertyPanel {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let rows_height: f32 = self
            .sections
            .iter()
            .flat_map(|section| section.rows.iter())
            .map(|row| row.height(self.options.row_height))
            .sum();
        let section_headers =
            self.sections.iter().filter(|section| !section.title.is_empty()).count();
        let section_gaps = self.sections.len().saturating_sub(1);
        let preferred = Size::new(
            280.0,
            self.options.margin * 2.0
                + self.header_height()
                + rows_height
                + section_headers as f32 * 24.0
                + section_gaps as f32 * self.options.section_gap,
        );
        constraint.constrain(preferred)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let content = self.content_rect();
        let control_x = content.x + self.options.label_width + self.options.control_gap;
        let control_width = (content.x + content.width - control_x).max(1.0);

        self.title_position = Point::new(content.x, content.y + 16.0);
        self.subtitle_position = Point::new(content.x, content.y + 34.0);
        let mut y = content.y + self.header_height();

        for section in &mut self.sections {
            if !section.title.is_empty() {
                section.header_position = Point::new(content.x, y + 16.0);
                y += 24.0;
            }

            let section_top = y;
            for row in &mut section.rows {
                let row_h = row.height(self.options.row_height);
                row.bounds = Rect::new(content.x, y, content.width, row_h);
                let label_y = if row_h > self.options.row_height * 1.5 {
                    y + 20.0
                } else {
                    y + row_h * 0.5 + 4.0
                };
                row.label_position = Point::new(content.x, label_y);
                let measured = row.control.measure(LayoutConstraint::loose(0.0, 0.0));
                let control_h = measured.height.min(row_h).max(1.0);
                row.control.layout(Rect::new(
                    control_x,
                    y + (row_h - control_h) * 0.5,
                    control_width,
                    control_h,
                ));
                y += row_h;
            }
            section.bounds = Rect::new(
                content.x - 6.0,
                section_top,
                content.width + 12.0,
                (y - section_top).max(0.0),
            );
            y += self.options.section_gap;
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        for section in self.sections.iter_mut().rev() {
            for row in section.rows.iter_mut().rev() {
                if row.control.event(event, ctx) == EventResult::Handled {
                    return EventResult::Handled;
                }
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        ctx.encoder.draw_rect(self.bounds, colors.card, 0.0);
        ctx.encoder.draw_text(&self.title, 13.0, self.title_position, colors.foreground);
        if let Some(subtitle) = &self.subtitle {
            ctx.encoder.draw_text(
                subtitle,
                11.0,
                self.subtitle_position,
                colors.muted_foreground,
            );
        }

        for section in &self.sections {
            if section.bounds.height > 0.0 {
                ctx.encoder.draw_rect(section.bounds, colors.popover, spacing.radius_md);
            }
            if !section.title.is_empty() {
                ctx.encoder.draw_text(
                    &section.title,
                    11.0,
                    section.header_position,
                    colors.muted_foreground,
                );
            }
            for row in &section.rows {
                ctx.encoder.draw_text(
                    &row.label,
                    12.0,
                    row.label_position,
                    colors.muted_foreground,
                );
                row.control.paint(ctx);
            }
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        for section in &self.sections {
            for row in &section.rows {
                row.control.paint_overlay(ctx);
            }
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
            || self
                .sections
                .iter()
                .flat_map(|section| section.rows.iter())
                .any(|row| row.control.hit_test(point))
    }

    fn child_count(&self) -> usize {
        self.sections.iter().map(|section| section.rows.len()).sum()
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        self.row_at_child_index(index).map(|row| row.control.as_ref())
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match self.row_at_child_index_mut(index) {
            Some(row) => Some(row.control.as_mut()),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::Cell;
    use std::rc::Rc;

    struct ProbeWidget {
        id: WidgetId,
        bounds: Rect,
        handled: Rc<Cell<bool>>,
        overlay_hit: bool,
    }

    impl ProbeWidget {
        fn new(handled: Rc<Cell<bool>>) -> Self {
            Self {
                id: WidgetId::new(),
                bounds: Rect::ZERO,
                handled,
                overlay_hit: false,
            }
        }

        fn with_overlay_hit(mut self) -> Self {
            self.overlay_hit = true;
            self
        }
    }

    impl Widget for ProbeWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, constraint: LayoutConstraint) -> Size {
            constraint.constrain(Size::new(80.0, 24.0))
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            if matches!(event, UiEvent::MouseDown { .. }) {
                self.handled.set(true);
                EventResult::Handled
            } else {
                EventResult::Ignored
            }
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point) || self.overlay_hit
        }
    }

    #[derive(Default)]
    struct RecordingEncoder {
        rects: usize,
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, _bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {
            self.rects += 1;
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
            text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_string());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    #[test]
    fn property_panel_lays_out_rows_and_children() {
        let handled = Rc::new(Cell::new(false));
        let mut panel =
            PropertyPanel::new("Inspector").with_section(PropertySection::new("Clip").with_row(
                PropertyRow::new("Opacity", Box::new(ProbeWidget::new(Rc::clone(&handled)))),
            ));

        panel.layout(Rect::new(10.0, 20.0, 300.0, 200.0));

        assert_eq!(panel.section_count(), 1);
        assert_eq!(panel.child_count(), 1);
        assert!(panel.child(0).is_some());
    }

    #[test]
    fn property_panel_supports_explicit_tall_rows() {
        let handled = Rc::new(Cell::new(false));
        let mut panel =
            PropertyPanel::new("Inspector").with_section(PropertySection::new("Curves").with_row(
                PropertyRow::new("Opacity", Box::new(ProbeWidget::new(handled))).with_height(118.0),
            ));
        let measured = panel.measure(LayoutConstraint::LOOSE);

        panel.layout(Rect::new(0.0, 0.0, 300.0, measured.height));

        let row = &panel.sections[0].rows[0];
        assert_eq!(row.bounds.height, 118.0);
        assert_eq!(row.label_position.y, row.bounds.y + 20.0);
        assert!(measured.height >= 118.0 + panel.options.margin * 2.0);
    }

    #[test]
    fn property_panel_routes_events_to_rows() {
        let handled = Rc::new(Cell::new(false));
        let mut panel =
            PropertyPanel::new("Inspector").with_section(PropertySection::new("Clip").with_row(
                PropertyRow::new("Opacity", Box::new(ProbeWidget::new(Rc::clone(&handled)))),
            ));
        panel.layout(Rect::new(0.0, 0.0, 300.0, 200.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = panel.event(
            &UiEvent::MouseDown {
                position: Point::new(140.0, 80.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(handled.get());
    }

    #[test]
    fn property_panel_hit_test_includes_open_child_overlay() {
        let handled = Rc::new(Cell::new(false));
        let panel = PropertyPanel::new("Inspector").with_section(
            PropertySection::new("Clip").with_row(PropertyRow::new(
                "Tint",
                Box::new(ProbeWidget::new(handled).with_overlay_hit()),
            )),
        );

        assert!(panel.hit_test(Point::new(900.0, 900.0)));
    }

    #[test]
    fn property_panel_paints_title_section_and_label() {
        let handled = Rc::new(Cell::new(false));
        let mut panel = PropertyPanel::new("Inspector")
            .with_subtitle("Selected clip")
            .with_section(PropertySection::new("Clip").with_row(PropertyRow::new(
                "Opacity",
                Box::new(ProbeWidget::new(handled)),
            )));
        panel.layout(Rect::new(0.0, 0.0, 300.0, 200.0));
        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 300.0, 200.0),
        };

        panel.paint(&mut ctx);

        assert!(encoder.rects >= 2);
        assert!(encoder.texts.contains(&"Inspector".to_string()));
        assert!(encoder.texts.contains(&"Clip".to_string()));
        assert!(encoder.texts.contains(&"Opacity".to_string()));
    }
}
