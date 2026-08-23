//! Property panel containers for inspector-style UI.
//!
//! This module provides a small reusable layout for labeled editor controls:
//! title, optional subtitle, sections, and rows with a label plus one widget.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::{Theme, ThemePreset};

use crate::paint::color_with_alpha;
use crate::{FormLayout, FormRowOptions, Label, VectorIcon};
use mondrian_editor_state::Action;

#[derive(Debug, Clone, Copy, PartialEq)]
struct PropertyPanelVisualTokens {
    options: PropertyPanelOptions,
    panel_radius: f32,
    title_font_size: f32,
    subtitle_font_size: f32,
    row_label_font_size: f32,
    section_title_font_size: f32,
    empty_title_font_size: f32,
    empty_description_font_size: f32,
    header_title_y_offset: f32,
    header_title_height: f32,
    header_subtitle_y_offset: f32,
    header_subtitle_height: f32,
    header_with_subtitle_height: f32,
    header_without_subtitle_height: f32,
    preferred_width: f32,
    empty_state_height: f32,
    empty_inset_x: f32,
    empty_text_width_gutter: f32,
    empty_max_width: f32,
    empty_top_fraction: f32,
    empty_min_top_gap: f32,
    embedded_empty_top: f32,
    empty_icon_size: f32,
    empty_icon_text_gap: f32,
    empty_title_height: f32,
    empty_description_gap: f32,
    empty_description_min_height: f32,
    empty_icon_alpha: f32,
    section_header_height: f32,
    section_header_title_y_offset: f32,
    section_header_title_height: f32,
    section_bounds_inset_x: f32,
    section_divider_height: f32,
    section_divider_alpha: f32,
    section_selected_alpha: f32,
    section_selected_radius: f32,
}

impl PropertyPanelVisualTokens {
    fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        let typography = &theme.typography;
        Self {
            options: PropertyPanelOptions::from_theme(theme),
            panel_radius: spacing.radius_none,
            title_font_size: (typography.small.font_size + typography.body.font_size) * 0.5,
            subtitle_font_size: typography.metadata.font_size,
            row_label_font_size: typography.small.font_size,
            section_title_font_size: typography.metadata.font_size,
            empty_title_font_size: (typography.small.font_size + typography.body.font_size) * 0.5,
            empty_description_font_size: typography.small.font_size,
            header_title_y_offset: spacing.panel_inner_margin.1,
            header_title_height: (typography.small.line_height + spacing.border_emphasis).max(1.0),
            header_subtitle_y_offset: spacing.property_row_height,
            header_subtitle_height: typography.small.line_height.max(1.0),
            header_with_subtitle_height: (spacing.property_row_height + spacing.md).max(1.0),
            header_without_subtitle_height: typography.large.line_height.max(1.0),
            preferred_width: spacing.tooltip_max_width.max(1.0),
            empty_state_height: (spacing.property_row_height * 3.0
                + spacing.md
                + spacing.border_emphasis)
                .max(1.0),
            empty_inset_x: spacing.sm,
            empty_text_width_gutter: spacing.interact_height + spacing.sm + spacing.border_emphasis,
            empty_max_width: (spacing.tooltip_max_width
                - spacing.xl
                - spacing.md
                - spacing.border_emphasis)
                .max(1.0),
            empty_top_fraction: 0.24,
            empty_min_top_gap: (typography.small.line_height + spacing.border_emphasis).max(0.0),
            embedded_empty_top: (spacing.property_row_height * 3.0
                + spacing.sm
                + spacing.border_emphasis)
                .max(0.0),
            empty_icon_size: spacing.interact_height.max(1.0),
            empty_icon_text_gap: (spacing.interact_height
                + spacing.md
                + spacing.border_emphasis * 2.0)
                .max(0.0),
            empty_title_height: (typography.small.line_height + spacing.border_emphasis).max(1.0),
            empty_description_gap: (spacing.property_row_height - spacing.border_emphasis).max(0.0),
            empty_description_min_height: typography.small.line_height.max(1.0),
            empty_icon_alpha: 0.80,
            section_header_height: typography.body.line_height.max(1.0),
            section_header_title_y_offset: spacing.panel_inner_margin.1,
            section_header_title_height: typography.small.line_height.max(1.0),
            section_bounds_inset_x: spacing.sm,
            section_divider_height: spacing.border_standard.max(0.0),
            section_divider_alpha: 0.82,
            section_selected_alpha: 0.10,
            section_selected_radius: spacing.radius_sm,
        }
    }
}

impl Default for PropertyPanelVisualTokens {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

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

impl PropertyPanelOptions {
    /// Create layout options from theme spacing tokens.
    pub fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            margin: spacing.panel_inner_margin.0,
            label_width: spacing.property_row_height * 3.0,
            row_height: spacing.property_row_height + spacing.border_emphasis,
            control_gap: spacing.md,
            section_gap: spacing.sm,
        }
    }

    fn form_row_options(&self) -> FormRowOptions {
        FormRowOptions {
            label_width: self.label_width,
            control_gap: self.control_gap,
            ..FormRowOptions::default()
        }
    }
}

impl Default for PropertyPanelOptions {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

/// One labeled control row in a [`PropertySection`].
pub struct PropertyRow {
    label: Label,
    control: Box<dyn Widget>,
    height: Option<f32>,
    bounds: Rect,
    control_bounds: Rect,
    label_position: Point,
}

impl PropertyRow {
    /// Create a property row from a label and an owned control widget.
    pub fn new(label: impl Into<String>, control: Box<dyn Widget>) -> Self {
        let visual = PropertyPanelVisualTokens::default();
        Self {
            label: Label::new(label.into())
                .muted()
                .with_font_size(visual.row_label_font_size)
                .with_padding(0.0, 0.0),
            control,
            height: None,
            bounds: Rect::ZERO,
            control_bounds: Rect::ZERO,
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
        self.label.text()
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
    title: Label,
    rows: Vec<PropertyRow>,
    selected: bool,
    select_action: Option<Action>,
    bounds: Rect,
    header_bounds: Rect,
    header_position: Point,
}

impl PropertySection {
    /// Create an empty property section.
    pub fn new(title: impl Into<String>) -> Self {
        let visual = PropertyPanelVisualTokens::default();
        Self {
            title: Label::new(title.into())
                .muted()
                .with_font_size(visual.section_title_font_size)
                .with_padding(0.0, 0.0),
            rows: Vec::new(),
            selected: false,
            select_action: None,
            bounds: Rect::ZERO,
            header_bounds: Rect::ZERO,
            header_position: Point::ZERO,
        }
    }

    /// Append one row and return the section for builder-style construction.
    pub fn with_row(mut self, row: PropertyRow) -> Self {
        self.rows.push(row);
        self
    }

    /// Set whether this section represents the active nested selection.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Dispatch an action when the section background/header is clicked.
    ///
    /// Child row controls receive events first; this action is only a fallback
    /// for clicks that do not belong to an inner control.
    pub fn on_select(mut self, action: impl Into<Option<Action>>) -> Self {
        self.select_action = action.into();
        self
    }

    /// Append one row.
    pub fn push_row(&mut self, row: PropertyRow) {
        self.rows.push(row);
    }

    /// Section title.
    pub fn title(&self) -> &str {
        self.title.text()
    }

    /// Number of rows in this section.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

struct PropertyPanelEmptyState {
    title: Label,
    description: Label,
    icon: Option<VectorIcon>,
    icon_bounds: Rect,
}

impl PropertyPanelEmptyState {
    fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        let visual = PropertyPanelVisualTokens::default();
        Self {
            title: Label::new(title.into())
                .secondary()
                .with_font_size(visual.empty_title_font_size)
                .with_padding(0.0, 0.0),
            description: Label::new(description.into())
                .tertiary()
                .with_font_size(visual.empty_description_font_size)
                .with_padding(0.0, 0.0)
                .wrapped(),
            icon: None,
            icon_bounds: Rect::ZERO,
        }
    }

    fn with_icon(mut self, icon: VectorIcon) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// Inspector-style property panel with labeled rows.
pub struct PropertyPanel {
    id: WidgetId,
    title: Label,
    subtitle: Option<Label>,
    show_header_text: bool,
    empty_state: Option<PropertyPanelEmptyState>,
    sections: Vec<PropertySection>,
    options: PropertyPanelOptions,
    bounds: Rect,
}

impl PropertyPanel {
    /// Create a property panel with default layout options.
    pub fn new(title: impl Into<String>) -> Self {
        Self::with_options(title, PropertyPanelOptions::default())
    }

    /// Create a property panel with explicit layout options.
    pub fn with_options(title: impl Into<String>, options: PropertyPanelOptions) -> Self {
        let visual = PropertyPanelVisualTokens::default();
        Self {
            id: WidgetId::new(),
            title: Label::new(title.into())
                .with_font_size(visual.title_font_size)
                .with_padding(0.0, 0.0),
            subtitle: None,
            show_header_text: true,
            empty_state: None,
            sections: Vec::new(),
            options,
            bounds: Rect::ZERO,
        }
    }

    /// Set the optional subtitle.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        let visual = PropertyPanelVisualTokens::default();
        self.subtitle = Some(
            Label::new(subtitle.into())
                .muted()
                .with_font_size(visual.subtitle_font_size)
                .with_padding(0.0, 0.0),
        );
        self
    }

    /// Use compact embedded chrome when the host panel already supplies title text.
    pub fn with_embedded_panel_chrome(mut self) -> Self {
        self.show_header_text = false;
        self
    }

    /// Replace row content with a compact empty state.
    pub fn with_empty_state(
        mut self,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.empty_state = Some(PropertyPanelEmptyState::new(title, description));
        self.sections.clear();
        self
    }

    /// Add an optional quiet icon to the empty state.
    pub fn with_empty_state_icon(mut self, icon: VectorIcon) -> Self {
        if let Some(empty_state) = self.empty_state.take() {
            self.empty_state = Some(empty_state.with_icon(icon));
        }
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
        let visual = PropertyPanelVisualTokens::default();
        if !self.show_header_text {
            return 0.0;
        }
        if self.subtitle.is_some() {
            visual.header_with_subtitle_height
        } else {
            visual.header_without_subtitle_height
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
        let visual = PropertyPanelVisualTokens::default();
        if self.empty_state.is_some() {
            let preferred = Size::new(
                visual.preferred_width,
                self.options.margin * 2.0 + self.header_height() + visual.empty_state_height,
            );
            return constraint.constrain(preferred);
        }
        let rows_height: f32 = self
            .sections
            .iter()
            .flat_map(|section| section.rows.iter())
            .map(|row| row.height(self.options.row_height))
            .sum();
        let section_headers =
            self.sections.iter().filter(|section| !section.title().is_empty()).count();
        let section_gaps = self.sections.len().saturating_sub(1);
        let preferred = Size::new(
            visual.preferred_width,
            self.options.margin * 2.0
                + self.header_height()
                + rows_height
                + section_headers as f32 * visual.header_without_subtitle_height
                + section_gaps as f32 * self.options.section_gap,
        );
        constraint.constrain(preferred)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let visual = PropertyPanelVisualTokens::default();
        let content = self.content_rect();
        let form_layout = FormLayout::new(self.options.form_row_options());
        let header_height = self.header_height();

        if self.show_header_text {
            self.title.layout(Rect::new(
                content.x,
                content.y + visual.header_title_y_offset,
                content.width,
                visual.header_title_height,
            ));
            if let Some(subtitle) = &mut self.subtitle {
                subtitle.layout(Rect::new(
                    content.x,
                    content.y + visual.header_subtitle_y_offset,
                    content.width,
                    visual.header_subtitle_height,
                ));
            }
        }

        if let Some(empty_state) = &mut self.empty_state {
            let max_width =
                (content.width - visual.empty_text_width_gutter).clamp(1.0, visual.empty_max_width);
            let available_height = (content.height - header_height).max(0.0);
            let top = if self.show_header_text {
                content.y
                    + header_height
                    + (available_height * visual.empty_top_fraction).max(visual.empty_min_top_gap)
            } else {
                content.y + visual.embedded_empty_top
            };
            let description_size = empty_state.description.measure(LayoutConstraint {
                min: Size::ZERO,
                max: Size::new(max_width, f32::MAX),
            });
            let x = content.x + visual.empty_inset_x;
            empty_state.icon_bounds = if empty_state.icon.is_some() {
                Rect::new(x, top, visual.empty_icon_size, visual.empty_icon_size)
            } else {
                Rect::ZERO
            };
            let text_top = if empty_state.icon.is_some() {
                top + visual.empty_icon_text_gap
            } else {
                top
            };
            empty_state
                .title
                .layout(Rect::new(x, text_top, max_width, visual.empty_title_height));
            empty_state.description.layout(Rect::new(
                x,
                text_top + visual.empty_description_gap,
                max_width,
                description_size.height.max(visual.empty_description_min_height),
            ));
            return;
        }

        let mut y = content.y + header_height;

        for section in &mut self.sections {
            if !section.title().is_empty() {
                section.header_position =
                    Point::new(content.x, y + visual.section_header_title_y_offset);
                section.header_bounds =
                    Rect::new(content.x, y, content.width, visual.section_header_height);
                section.title.layout(Rect::new(
                    content.x,
                    section.header_position.y,
                    content.width,
                    visual.section_header_title_height,
                ));
                y += visual.section_header_height;
            } else {
                section.header_bounds = Rect::ZERO;
            }

            let section_top = y;
            for row in &mut section.rows {
                let row_h = row.height(self.options.row_height);
                let measured = row.control.measure(form_layout.control_constraint(content, row_h));
                let rects = form_layout.row_rects(
                    content,
                    y,
                    row_h,
                    self.options.row_height,
                    measured.height,
                );
                row.bounds = rects.row;
                row.control_bounds = rects.control;
                row.label_position = rects.label.min();
                row.label.layout(rects.label);
                row.control.layout(rects.control);
                y += row_h;
            }
            section.bounds = Rect::new(
                content.x - visual.section_bounds_inset_x,
                section_top,
                content.width + visual.section_bounds_inset_x * 2.0,
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
            if let UiEvent::MouseDown { position, button: MouseButton::Left, .. } = event
                && (section.bounds.contains(*position) || section.header_bounds.contains(*position))
                && let Some(action) = section.select_action.clone()
            {
                (ctx.dispatch)(action);
                ctx.request_repaint();
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let visual = PropertyPanelVisualTokens::from_theme(ctx.theme);
        let panel_fill = colors.card;
        ctx.encoder.draw_rect(self.bounds, panel_fill, visual.panel_radius);
        if self.show_header_text {
            self.title.paint(ctx);
            if let Some(subtitle) = &self.subtitle {
                subtitle.paint(ctx);
            }
        }
        if let Some(empty_state) = &self.empty_state {
            if let Some(icon) = &empty_state.icon {
                let color = color_with_alpha(colors.text_tertiary, visual.empty_icon_alpha);
                icon.paint(ctx, empty_state.icon_bounds, color);
            }
            empty_state.title.paint(ctx);
            empty_state.description.paint(ctx);
            return;
        }

        for section in &self.sections {
            if section.header_bounds.height > 0.0 {
                let y = section.header_bounds.y;
                ctx.encoder.draw_rect(
                    Rect::new(
                        section.header_bounds.x,
                        y,
                        section.header_bounds.width,
                        visual.section_divider_height,
                    ),
                    color_with_alpha(colors.border, visual.section_divider_alpha),
                    visual.panel_radius,
                );
            }
            if section.bounds.height > 0.0 && section.selected {
                ctx.encoder.draw_rect(
                    Rect::new(
                        section.bounds.x,
                        section.bounds.y,
                        section.bounds.width,
                        section.bounds.height,
                    ),
                    color_with_alpha(colors.primary, visual.section_selected_alpha),
                    visual.section_selected_radius,
                );
            }
            if !section.title().is_empty() {
                section.title.paint(ctx);
            }
            for row in &section.rows {
                ctx.push_clip(row.bounds);
                row.label.paint(ctx);
                ctx.push_clip(row.control_bounds);
                row.control.paint(ctx);
                ctx.pop_clip();
                ctx.pop_clip();
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
    use crate::Slider;
    use mondrian_core::Color;
    use mondrian_editor_state::Action;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct ProbeWidget {
        id: WidgetId,
        bounds: Rect,
        handled: Rc<Cell<bool>>,
    }

    impl ProbeWidget {
        fn new(handled: Rc<Cell<bool>>) -> Self {
            Self { id: WidgetId::new(), bounds: Rect::ZERO, handled }
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
            self.bounds.contains(point)
        }
    }

    struct ConstraintRecordingWidget {
        id: WidgetId,
        seen: Rc<RefCell<Vec<LayoutConstraint>>>,
        bounds: Rect,
    }

    impl ConstraintRecordingWidget {
        fn new(seen: Rc<RefCell<Vec<LayoutConstraint>>>) -> Self {
            Self { id: WidgetId::new(), seen, bounds: Rect::ZERO }
        }
    }

    impl Widget for ConstraintRecordingWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, constraint: LayoutConstraint) -> Size {
            self.seen.borrow_mut().push(constraint);
            constraint.constrain(Size::new(80.0, 24.0))
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    #[derive(Default)]
    struct RecordingEncoder {
        rects: usize,
        rect_bounds: Vec<Rect>,
        rect_colors: Vec<Color>,
        rect_radii: Vec<f32>,
        texts: Vec<String>,
        text_font_sizes: Vec<f32>,
        clips: Vec<Rect>,
        clip_pops: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, bounds: Rect, color: Color, corner_radius: f32) {
            self.rects += 1;
            self.rect_bounds.push(bounds);
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
            text: &str,
            font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_string());
            self.text_font_sizes.push(font_size);
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_string());
            self.text_font_sizes.push(font_size);
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    struct OverflowPaintWidget {
        id: WidgetId,
        bounds: Rect,
    }

    impl OverflowPaintWidget {
        fn new() -> Self {
            Self { id: WidgetId::new(), bounds: Rect::ZERO }
        }
    }

    impl Widget for OverflowPaintWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, constraint: LayoutConstraint) -> Size {
            constraint.constrain(Size::new(80.0, 20.0))
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, ctx: &mut PaintContext) {
            ctx.encoder.draw_rect(self.bounds.inset(-200.0, -200.0), Color::WHITE, 0.0);
        }

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    #[test]
    fn property_panel_options_follow_theme_spacing() {
        let mut theme = ThemePreset::Dark.build();
        theme.spacing.panel_inner_margin = (14.0, 16.0);
        theme.spacing.property_row_height = 32.0;
        theme.spacing.border_emphasis = 3.0;
        theme.spacing.md = 12.0;
        theme.spacing.sm = 5.0;

        let options = PropertyPanelOptions::from_theme(&theme);

        assert_eq!(
            options,
            PropertyPanelOptions {
                margin: 14.0,
                label_width: 96.0,
                row_height: 35.0,
                control_gap: 12.0,
                section_gap: 5.0,
            }
        );
    }

    #[test]
    fn property_panel_visual_tokens_follow_theme_spacing_and_typography() {
        let mut theme = ThemePreset::Dark.build();
        theme.spacing.panel_inner_margin = (14.0, 16.0);
        theme.spacing.property_row_height = 32.0;
        theme.spacing.border_emphasis = 3.0;
        theme.spacing.md = 12.0;
        theme.spacing.sm = 5.0;
        theme.spacing.radius_none = 0.5;
        theme.spacing.radius_sm = 7.0;
        theme.spacing.tooltip_max_width = 300.0;
        theme.spacing.xl = 50.0;
        theme.spacing.interact_height = 34.0;
        theme.spacing.border_standard = 2.0;
        theme.typography.small.font_size = 13.0;
        theme.typography.small.line_height = 17.0;
        theme.typography.body.font_size = 15.0;
        theme.typography.body.line_height = 21.0;
        theme.typography.metadata.font_size = 10.0;
        theme.typography.large.line_height = 25.0;

        let visual = PropertyPanelVisualTokens::from_theme(&theme);

        assert_eq!(visual.options.margin, 14.0);
        assert_eq!(visual.panel_radius, 0.5);
        assert_eq!(visual.title_font_size, 14.0);
        assert_eq!(visual.subtitle_font_size, 10.0);
        assert_eq!(visual.row_label_font_size, 13.0);
        assert_eq!(visual.section_title_font_size, 10.0);
        assert_eq!(visual.header_title_y_offset, 16.0);
        assert_eq!(visual.header_title_height, 20.0);
        assert_eq!(visual.header_subtitle_y_offset, 32.0);
        assert_eq!(visual.header_with_subtitle_height, 44.0);
        assert_eq!(visual.header_without_subtitle_height, 25.0);
        assert_eq!(visual.preferred_width, 300.0);
        assert_eq!(visual.empty_state_height, 111.0);
        assert_eq!(visual.empty_text_width_gutter, 42.0);
        assert_eq!(visual.empty_max_width, 235.0);
        assert_eq!(visual.embedded_empty_top, 104.0);
        assert_eq!(visual.empty_icon_size, 34.0);
        assert_eq!(visual.empty_icon_text_gap, 52.0);
        assert_eq!(visual.empty_description_gap, 29.0);
        assert_eq!(visual.section_header_height, 21.0);
        assert_eq!(visual.section_divider_height, 2.0);
        assert_eq!(visual.section_selected_radius, 7.0);
    }

    #[test]
    fn property_panel_visual_tokens_keep_cramped_theme_dimensions_safe() {
        let mut theme = ThemePreset::Dark.build();
        theme.spacing.tooltip_max_width = 20.0;
        theme.spacing.xl = 48.0;
        theme.spacing.md = 24.0;
        theme.spacing.border_emphasis = 8.0;
        theme.spacing.property_row_height = 2.0;
        theme.spacing.interact_height = 0.0;
        theme.typography.small.line_height = 0.0;
        theme.typography.body.line_height = 0.0;
        theme.typography.large.line_height = 0.0;

        let visual = PropertyPanelVisualTokens::from_theme(&theme);

        assert_eq!(visual.empty_max_width, 1.0);
        assert_eq!(visual.empty_icon_size, 1.0);
        assert_eq!(visual.header_without_subtitle_height, 1.0);
        assert_eq!(visual.section_header_height, 1.0);
        assert_eq!(visual.empty_description_gap, 0.0);
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
        assert_eq!(row.label_position.y, row.bounds.y + 4.0);
        assert!(measured.height >= 118.0 + panel.options.margin * 2.0);
    }

    #[test]
    fn property_panel_measures_controls_with_form_lane_constraint() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut panel = PropertyPanel::new("Inspector").with_section(
            PropertySection::new("Clip").with_row(PropertyRow::new(
                "Opacity",
                Box::new(ConstraintRecordingWidget::new(Rc::clone(&seen))),
            )),
        );

        panel.layout(Rect::new(0.0, 0.0, 300.0, 180.0));

        assert_eq!(
            seen.borrow().as_slice(),
            &[LayoutConstraint { min: Size::ZERO, max: Size::new(182.0, 30.0) }]
        );
        assert_eq!(panel.sections[0].rows[0].control_bounds.width, 182.0);
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
    fn property_section_select_action_dispatches_from_header_click() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);
        let mut panel = PropertyPanel::new("Inspector").with_section(
            PropertySection::new("Effect").on_select(Action::SaveProject).with_row(
                PropertyRow::new(
                    "Enabled",
                    Box::new(ConstraintRecordingWidget::new(Rc::clone(&seen))),
                ),
            ),
        );
        panel.layout(Rect::new(0.0, 0.0, 300.0, 180.0));

        let result = panel.event(
            &UiEvent::MouseDown {
                position: Point::new(20.0, 46.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(actions.borrow().len(), 1);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn property_section_select_action_does_not_steal_child_control_clicks() {
        let handled = Rc::new(Cell::new(false));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);
        let mut panel = PropertyPanel::new("Inspector").with_section(
            PropertySection::new("Effect").on_select(Action::SaveProject).with_row(
                PropertyRow::new("Enabled", Box::new(ProbeWidget::new(Rc::clone(&handled)))),
            ),
        );
        panel.layout(Rect::new(0.0, 0.0, 300.0, 180.0));

        let result = panel.event(
            &UiEvent::MouseDown {
                position: Point::new(140.0, 79.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(handled.get());
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn property_panel_slider_row_drags_after_mouse_down() {
        let last_value = Rc::new(Cell::new(0.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);
        let mut panel = PropertyPanel::new("Inspector").with_section(
            PropertySection::new("Clip").with_row(PropertyRow::new(
                "Opacity",
                Box::new({
                    let last_value = Rc::clone(&last_value);
                    Slider::new(0.0, 0.0, 100.0).on_change(move |value| {
                        last_value.set(value);
                        Action::SaveProject
                    })
                }),
            )),
        );
        panel.layout(Rect::new(0.0, 0.0, 320.0, 200.0));

        let slider_point = Point::new(128.0, 79.0);
        let slider_end = Point::new(300.0, 79.0);
        let down = panel.event(
            &UiEvent::MouseDown {
                position: slider_point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let drag = panel.event(
            &UiEvent::MouseMove { position: slider_end, modifiers: Modifiers::none() },
            &mut ctx,
        );
        let up = panel.event(
            &UiEvent::MouseUp {
                position: slider_end,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(down, EventResult::Handled);
        assert_eq!(drag, EventResult::Handled);
        assert_eq!(up, EventResult::Handled);
        assert!(
            last_value.get() > 95.0,
            "slider value was {}",
            last_value.get()
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::SaveProject, Action::SaveProject]
        );
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

        let visual = PropertyPanelVisualTokens::from_theme(&theme);
        assert!(encoder.rects >= 2);
        assert!(encoder.texts.contains(&"Inspector".to_string()));
        assert!(encoder.texts.contains(&"Clip".to_string()));
        assert!(encoder.texts.contains(&"Opacity".to_string()));
        assert!(encoder.text_font_sizes.contains(&visual.title_font_size));
        assert!(encoder.text_font_sizes.contains(&visual.subtitle_font_size));
        assert!(encoder.text_font_sizes.contains(&visual.section_title_font_size));
        assert!(encoder.text_font_sizes.contains(&visual.row_label_font_size));
    }

    #[test]
    fn embedded_panel_chrome_omits_duplicate_title_and_subtitle() {
        let handled = Rc::new(Cell::new(false));
        let mut panel = PropertyPanel::new("Inspector")
            .with_subtitle("Selected clip")
            .with_embedded_panel_chrome()
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

        assert!(!encoder.texts.contains(&"Inspector".to_string()));
        assert!(!encoder.texts.contains(&"Selected clip".to_string()));
        assert!(encoder.texts.contains(&"Clip".to_string()));
        assert!(encoder.texts.contains(&"Opacity".to_string()));
    }

    #[test]
    fn embedded_empty_state_paints_title_and_wrapped_description_without_form_rows() {
        let mut panel =
            PropertyPanel::new("Inspector").with_embedded_panel_chrome().with_empty_state(
                "未选择剪辑",
                "选择时间线中的剪辑、图层或效果后，可在这里调整属性。",
            );
        panel.layout(Rect::new(0.0, 0.0, 320.0, 220.0));
        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 320.0, 220.0),
        };

        panel.paint(&mut ctx);

        assert_eq!(panel.child_count(), 0);
        assert!(encoder.texts.contains(&"未选择剪辑".to_string()));
        assert!(encoder
            .texts
            .contains(&"选择时间线中的剪辑、图层或效果后，可在这里调整属性。".to_string()));
        assert_eq!(encoder.clip_pops, encoder.clips.len());
    }

    #[test]
    fn property_panel_clips_row_controls_to_form_rects() {
        let mut panel =
            PropertyPanel::new("Inspector").with_section(PropertySection::new("Clip").with_row(
                PropertyRow::new("Overflow", Box::new(OverflowPaintWidget::new())),
            ));
        panel.layout(Rect::new(0.0, 0.0, 300.0, 180.0));
        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 300.0, 180.0),
        };

        panel.paint(&mut ctx);

        let row = &panel.sections[0].rows[0];
        assert!(
            encoder.clips.contains(&row.bounds),
            "row paint should be clipped to row bounds"
        );
        assert!(
            encoder.clips.contains(&row.control_bounds),
            "control paint should be clipped to the form control bounds"
        );
        assert_eq!(encoder.clip_pops, encoder.clips.len());
    }

    #[test]
    fn selected_property_section_paints_selection_chrome() {
        let handled = Rc::new(Cell::new(false));
        let mut panel = PropertyPanel::new("Inspector").with_section(
            PropertySection::new("Effect").selected(true).with_row(PropertyRow::new(
                "Enabled",
                Box::new(ProbeWidget::new(handled)),
            )),
        );
        panel.layout(Rect::new(0.0, 0.0, 300.0, 160.0));
        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 300.0, 160.0),
        };

        panel.paint(&mut ctx);

        let visual = PropertyPanelVisualTokens::from_theme(&theme);
        assert_eq!(encoder.rect_colors.first(), Some(&theme.colors.card));
        assert_eq!(encoder.rect_radii.first(), Some(&visual.panel_radius));
        assert!(encoder.rect_colors.contains(&color_with_alpha(
            theme.colors.border,
            visual.section_divider_alpha
        )));
        assert!(encoder.rect_radii.contains(&visual.section_selected_radius));
        assert!(
            encoder.rects >= 3,
            "selected section should paint panel, selection ring, and section fill"
        );
        assert!(encoder.rect_colors.contains(&color_with_alpha(
            theme.colors.primary,
            visual.section_selected_alpha
        )));
    }
}
