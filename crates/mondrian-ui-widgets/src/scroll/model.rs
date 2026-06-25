use glam::Vec2;
use mondrian_ui_core::types::{Rect, Size};

use super::{ScrollAxes, ScrollbarAxis};

pub(super) fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

pub(super) fn normalized_content_size(axes: ScrollAxes, measured: Size, viewport: Size) -> Size {
    Size::new(
        if axes.horizontal() {
            measured.width.max(viewport.width)
        } else {
            viewport.width
        },
        if axes.vertical() {
            measured.height.max(viewport.height)
        } else {
            viewport.height
        },
    )
}

pub(super) fn clamp_scroll_offset(
    offset: Vec2,
    axes: ScrollAxes,
    content_size: Size,
    viewport_size: Size,
) -> Vec2 {
    Vec2::new(
        finite_nonnegative(offset.x).min(max_scroll_x(axes, content_size, viewport_size)),
        finite_nonnegative(offset.y).min(max_scroll_y(axes, content_size, viewport_size)),
    )
}

fn max_scroll_x(axes: ScrollAxes, content_size: Size, viewport_size: Size) -> f32 {
    if axes.horizontal() {
        (finite_nonnegative(content_size.width) - finite_nonnegative(viewport_size.width)).max(0.0)
    } else {
        0.0
    }
}

fn max_scroll_y(axes: ScrollAxes, content_size: Size, viewport_size: Size) -> f32 {
    if axes.vertical() {
        (finite_nonnegative(content_size.height) - finite_nonnegative(viewport_size.height))
            .max(0.0)
    } else {
        0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ScrollbarMetrics {
    pub(super) width: f32,
    pub(super) track_inset: f32,
    pub(super) min_thumb_len: f32,
}

impl ScrollbarMetrics {
    pub(super) fn new(width: f32, track_inset: f32, min_thumb_len: f32) -> Self {
        let width = finite_nonnegative(width).max(1.0);
        let track_inset = finite_nonnegative(track_inset).min(width * 0.5);
        let min_thumb_len = finite_nonnegative(min_thumb_len).max(1.0);
        Self { width, track_inset, min_thumb_len }
    }

    pub(super) fn with_width(self, width: f32) -> Self {
        Self::new(width, self.track_inset, self.min_thumb_len)
    }

    fn track_thickness(self) -> f32 {
        (self.width - self.track_inset * 2.0).max(1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ScrollbarLayout {
    axes: ScrollAxes,
    bounds: Rect,
    viewport_size: Size,
    content_size: Size,
    scroll_offset: Vec2,
    metrics: ScrollbarMetrics,
}

impl ScrollbarLayout {
    pub(super) fn new(
        axes: ScrollAxes,
        bounds: Rect,
        viewport_size: Size,
        content_size: Size,
        scroll_offset: Vec2,
        metrics: ScrollbarMetrics,
    ) -> Self {
        Self {
            axes,
            bounds,
            viewport_size,
            content_size,
            scroll_offset,
            metrics,
        }
    }

    pub(super) fn viewport_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y,
            self.viewport_size.width,
            self.viewport_size.height,
        )
    }

    pub(super) fn child_bounds(&self) -> Rect {
        Rect::new(
            self.bounds.x - self.scroll_offset.x,
            self.bounds.y - self.scroll_offset.y,
            self.content_size.width,
            self.content_size.height,
        )
    }

    pub(super) fn max_scroll_x(&self) -> f32 {
        max_scroll_x(self.axes, self.content_size, self.viewport_size)
    }

    pub(super) fn max_scroll_y(&self) -> f32 {
        max_scroll_y(self.axes, self.content_size, self.viewport_size)
    }

    pub(super) fn has_vertical_scrollbar(&self) -> bool {
        self.max_scroll_y() > 0.0 && self.viewport_size.height > 0.0
    }

    pub(super) fn has_horizontal_scrollbar(&self) -> bool {
        self.max_scroll_x() > 0.0 && self.viewport_size.width > 0.0
    }

    pub(super) fn track_rect(&self, axis: ScrollbarAxis) -> Rect {
        match axis {
            ScrollbarAxis::Horizontal => Rect::new(
                self.bounds.x,
                self.bounds.y + self.bounds.height - self.metrics.width + self.metrics.track_inset,
                self.viewport_size.width.max(0.0),
                self.metrics.track_thickness(),
            ),
            ScrollbarAxis::Vertical => Rect::new(
                self.bounds.x + self.bounds.width - self.metrics.width + self.metrics.track_inset,
                self.bounds.y,
                self.metrics.track_thickness(),
                self.viewport_size.height.max(0.0),
            ),
        }
    }

    pub(super) fn thumb_rect(&self, axis: ScrollbarAxis) -> Option<Rect> {
        match axis {
            ScrollbarAxis::Horizontal => self.horizontal_thumb_rect(),
            ScrollbarAxis::Vertical => self.vertical_thumb_rect(),
        }
    }

    pub(super) fn scroll_for_thumb_delta(
        &self,
        axis: ScrollbarAxis,
        drag_start_scroll: Vec2,
        delta: f32,
    ) -> Option<f32> {
        let thumb = self.thumb_rect(axis)?;
        let track = self.track_rect(axis);
        let (thumb_range, scroll_range, start) = match axis {
            ScrollbarAxis::Horizontal => (
                (track.width - thumb.width).max(1.0),
                self.max_scroll_x(),
                drag_start_scroll.x,
            ),
            ScrollbarAxis::Vertical => (
                (track.height - thumb.height).max(1.0),
                self.max_scroll_y(),
                drag_start_scroll.y,
            ),
        };
        Some(start + (delta / thumb_range) * scroll_range)
    }

    fn vertical_thumb_rect(&self) -> Option<Rect> {
        if !self.has_vertical_scrollbar() {
            return None;
        }
        let track = self.track_rect(ScrollbarAxis::Vertical);
        let thumb_h = (track.height * (self.viewport_size.height / self.content_size.height))
            .max(self.metrics.min_thumb_len)
            .min(track.height);
        let thumb_range = (track.height - thumb_h).max(0.0);
        let max_scroll = self.max_scroll_y().max(1.0);
        let thumb_y = track.y + (self.scroll_offset.y / max_scroll) * thumb_range;
        Some(Rect::new(track.x, thumb_y, track.width, thumb_h))
    }

    fn horizontal_thumb_rect(&self) -> Option<Rect> {
        if !self.has_horizontal_scrollbar() {
            return None;
        }
        let track = self.track_rect(ScrollbarAxis::Horizontal);
        if track.width <= 0.0 {
            return None;
        }
        let thumb_w = (track.width * (self.viewport_size.width / self.content_size.width))
            .max(self.metrics.min_thumb_len)
            .min(track.width);
        let thumb_range = (track.width - thumb_w).max(0.0);
        let max_scroll = self.max_scroll_x().max(1.0);
        let thumb_x = track.x + (self.scroll_offset.x / max_scroll) * thumb_range;
        Some(Rect::new(thumb_x, track.y, thumb_w, track.height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_scrollbar_metrics() -> ScrollbarMetrics {
        ScrollbarMetrics::new(8.0, 2.0, 16.0)
    }

    #[test]
    fn clamp_scroll_offset_recovers_nonfinite_and_respects_axes() {
        let content = Size::new(500.0, 600.0);
        let viewport = Size::new(100.0, 120.0);

        assert_eq!(
            clamp_scroll_offset(
                Vec2::new(f32::NAN, 999.0),
                ScrollAxes::Vertical,
                content,
                viewport
            ),
            Vec2::new(0.0, 480.0)
        );
        assert_eq!(
            clamp_scroll_offset(
                Vec2::new(999.0, 999.0),
                ScrollAxes::Horizontal,
                content,
                viewport
            ),
            Vec2::new(400.0, 0.0)
        );
    }

    #[test]
    fn normalized_content_size_locks_non_scroll_axes_to_viewport() {
        let measured = Size::new(400.0, 600.0);
        let viewport = Size::new(100.0, 120.0);

        assert_eq!(
            normalized_content_size(ScrollAxes::Vertical, measured, viewport),
            Size::new(100.0, 600.0)
        );
        assert_eq!(
            normalized_content_size(ScrollAxes::Horizontal, measured, viewport),
            Size::new(400.0, 120.0)
        );
    }

    #[test]
    fn scrollbar_layout_places_vertical_thumb_from_scroll_ratio() {
        let layout = ScrollbarLayout::new(
            ScrollAxes::Vertical,
            Rect::new(10.0, 20.0, 200.0, 100.0),
            Size::new(192.0, 100.0),
            Size::new(192.0, 500.0),
            Vec2::new(0.0, 200.0),
            default_scrollbar_metrics(),
        );

        let track = layout.track_rect(ScrollbarAxis::Vertical);
        let thumb = layout.thumb_rect(ScrollbarAxis::Vertical).expect("thumb");

        assert_eq!(track.height, 100.0);
        assert_eq!(thumb.height, 20.0);
        assert_eq!(thumb.y, 60.0);
    }

    #[test]
    fn scrollbar_layout_keeps_child_and_viewport_in_screen_space() {
        let layout = ScrollbarLayout::new(
            ScrollAxes::Both,
            Rect::new(10.0, 20.0, 200.0, 100.0),
            Size::new(192.0, 92.0),
            Size::new(400.0, 500.0),
            Vec2::new(30.0, 40.0),
            default_scrollbar_metrics(),
        );

        assert_eq!(layout.viewport_rect(), Rect::new(10.0, 20.0, 192.0, 92.0));
        assert_eq!(layout.child_bounds(), Rect::new(-20.0, -20.0, 400.0, 500.0));
    }

    #[test]
    fn scrollbar_layout_maps_thumb_drag_delta_to_scroll_delta() {
        let layout = ScrollbarLayout::new(
            ScrollAxes::Vertical,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Size::new(92.0, 100.0),
            Size::new(92.0, 500.0),
            Vec2::ZERO,
            default_scrollbar_metrics(),
        );

        assert_eq!(
            layout.scroll_for_thumb_delta(ScrollbarAxis::Vertical, Vec2::ZERO, 80.0),
            Some(400.0)
        );
    }

    #[test]
    fn scrollbar_metrics_control_track_and_min_thumb_geometry() {
        let layout = ScrollbarLayout::new(
            ScrollAxes::Vertical,
            Rect::new(10.0, 20.0, 120.0, 100.0),
            Size::new(108.0, 100.0),
            Size::new(108.0, 2000.0),
            Vec2::ZERO,
            ScrollbarMetrics::new(12.0, 3.0, 24.0),
        );

        let track = layout.track_rect(ScrollbarAxis::Vertical);
        let thumb = layout.thumb_rect(ScrollbarAxis::Vertical).expect("thumb");

        assert_eq!(track, Rect::new(121.0, 20.0, 6.0, 100.0));
        assert_eq!(thumb.width, 6.0);
        assert_eq!(thumb.height, 24.0);
    }
}
