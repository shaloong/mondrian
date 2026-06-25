//! Shared geometry helpers for labeled form rows.
//!
//! The helpers are intentionally widget-agnostic. Containers keep ownership of
//! their child widgets, while this module computes stable label/control rects.

use mondrian_ui_core::types::*;
use mondrian_ui_theme::{Theme, ThemePreset};

/// Layout options for a labeled form row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormRowOptions {
    /// Width reserved for the label.
    pub label_width: f32,
    /// Gap between the label and control.
    pub control_gap: f32,
    /// Height used to lay out label widgets.
    pub label_height: f32,
    /// Extra y offset for labels in single-line rows.
    pub compact_label_y_offset: f32,
    /// Top y offset for labels in tall rows.
    pub tall_label_y_offset: f32,
    /// Rows taller than this multiplier use tall label placement.
    pub tall_row_multiplier: f32,
}

impl FormRowOptions {
    /// Create row options from theme spacing and typography tokens.
    pub fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        let typography = &theme.typography;
        Self {
            label_width: spacing.property_row_height * 3.0 + spacing.border_emphasis * 4.0,
            control_gap: (spacing.md - spacing.border_emphasis).max(0.0),
            label_height: typography.small.line_height,
            compact_label_y_offset: spacing.radius_none,
            tall_label_y_offset: spacing.xs,
            tall_row_multiplier: (typography.body.line_height / typography.small.line_height)
                + 0.25,
        }
    }
}

impl Default for FormRowOptions {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

/// Computed rectangles for one labeled form row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormRowRects {
    /// Whole row bounds.
    pub row: Rect,
    /// Label widget bounds.
    pub label: Rect,
    /// Control widget bounds.
    pub control: Rect,
}

/// Stateless labeled-row geometry calculator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormLayout {
    options: FormRowOptions,
}

impl FormLayout {
    /// Create a layout calculator with explicit options.
    pub fn new(options: FormRowOptions) -> Self {
        Self { options }
    }

    /// Create a layout calculator with default options.
    pub fn default_rows() -> Self {
        Self::new(FormRowOptions::default())
    }

    /// Current row options.
    pub fn options(&self) -> FormRowOptions {
        self.options
    }

    /// Width available to the control lane inside one form row.
    pub fn control_width(&self, bounds: Rect) -> f32 {
        let control_x = bounds.x + self.options.label_width + self.options.control_gap;
        (bounds.x + bounds.width - control_x).max(1.0)
    }

    /// Measurement constraint for a row control before final row placement.
    pub fn control_constraint(&self, bounds: Rect, row_height: f32) -> LayoutConstraint {
        LayoutConstraint {
            min: Size::ZERO,
            max: Size::new(self.control_width(bounds), row_height.max(1.0)),
        }
    }

    /// Return rectangles for one row inside `bounds`.
    pub fn row_rects(
        &self,
        bounds: Rect,
        row_y: f32,
        row_height: f32,
        default_row_height: f32,
        measured_control_height: f32,
    ) -> FormRowRects {
        let row_height = row_height.max(1.0);
        let default_row_height = default_row_height.max(1.0);
        let row = Rect::new(bounds.x, row_y, bounds.width, row_height);
        let control_x = bounds.x + self.options.label_width + self.options.control_gap;
        let control_width = self.control_width(bounds);
        let control_height = measured_control_height.min(row_height).max(1.0);
        let label_y = if row_height > default_row_height * self.options.tall_row_multiplier {
            row_y + self.options.tall_label_y_offset
        } else {
            row_y
                + (row_height - self.options.label_height.max(1.0)).max(0.0) * 0.5
                + self.options.compact_label_y_offset
        };
        FormRowRects {
            row,
            label: Rect::new(
                bounds.x,
                label_y,
                self.options.label_width.max(1.0),
                self.options.label_height.max(1.0),
            ),
            control: Rect::new(
                control_x,
                row_y + (row_height - control_height) * 0.5,
                control_width,
                control_height,
            ),
        }
    }
}

impl Default for FormLayout {
    fn default() -> Self {
        Self::default_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_rects_split_label_and_control_columns() {
        let layout = FormLayout::default();

        let rects = layout.row_rects(Rect::new(10.0, 20.0, 300.0, 200.0), 40.0, 34.0, 34.0, 24.0);

        assert_eq!(rects.row, Rect::new(10.0, 40.0, 300.0, 34.0));
        assert_eq!(rects.label, Rect::new(10.0, 49.0, 92.0, 16.0));
        assert_eq!(rects.control, Rect::new(110.0, 45.0, 200.0, 24.0));
    }

    #[test]
    fn row_options_follow_theme_spacing_and_typography() {
        let mut theme = ThemePreset::Dark.build();
        theme.spacing.property_row_height = 32.0;
        theme.spacing.border_emphasis = 3.0;
        theme.spacing.md = 14.0;
        theme.spacing.xs = 5.0;
        theme.spacing.radius_none = 0.25;
        theme.typography.small.line_height = 15.0;
        theme.typography.body.line_height = 21.0;

        let options = FormRowOptions::from_theme(&theme);

        assert_eq!(options.label_width, 108.0);
        assert_eq!(options.control_gap, 11.0);
        assert_eq!(options.label_height, 15.0);
        assert_eq!(options.compact_label_y_offset, 0.25);
        assert_eq!(options.tall_label_y_offset, 5.0);
        assert!((options.tall_row_multiplier - 1.65).abs() < f32::EPSILON);
    }

    #[test]
    fn control_constraint_matches_row_control_lane() {
        let layout = FormLayout::default();

        let constraint = layout.control_constraint(Rect::new(10.0, 20.0, 300.0, 200.0), 34.0);

        assert_eq!(
            constraint,
            LayoutConstraint { min: Size::ZERO, max: Size::new(200.0, 34.0) }
        );
    }

    #[test]
    fn tall_row_pins_label_near_top() {
        let layout = FormLayout::default();

        let rects = layout.row_rects(Rect::new(0.0, 0.0, 240.0, 140.0), 12.0, 118.0, 34.0, 200.0);

        assert_eq!(rects.label.y, 16.0);
        assert_eq!(rects.control, Rect::new(100.0, 12.0, 140.0, 118.0));
    }

    #[test]
    fn narrow_rows_keep_positive_control_width() {
        let options = FormRowOptions {
            label_width: 92.0,
            control_gap: 12.0,
            ..FormRowOptions::default()
        };
        let layout = FormLayout::new(options);

        let rects = layout.row_rects(Rect::new(0.0, 0.0, 80.0, 40.0), 0.0, 24.0, 24.0, 20.0);

        assert_eq!(rects.control.width, 1.0);
    }
}
