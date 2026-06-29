use mondrian_ui_core::types::{KeyCode, Modifiers, Point, Rect};

use super::{
    ViewerControl, ViewerDropdown, ViewerMetrics, BASIC_VIEWER_CONTROLS, FULL_VIEWER_CONTROLS,
    JUMP_VIEWER_CONTROLS, MINIMAL_VIEWER_CONTROLS, VIEWER_PREVIEW_QUALITY_OPTIONS,
    VIEWER_ZOOM_OPTIONS,
};

const CHROME_TOP: f32 = 14.0;
const CHROME_BOTTOM: f32 = 44.0;
const CANVAS_PADDING: f32 = 16.0;
const CHIP_RIGHT_INSET: f32 = 14.0;
const CHIP_GAP_FROM_CONTROLS: f32 = 12.0;
const CHIP_GAP: f32 = 6.0;
const CHIP_BASE_WIDTH: f32 = 28.0;
const CHIP_CHAR_WIDTH: f32 = 7.0;
const PREVIEW_QUALITY_MIN_WIDTH: f32 = 50.0;
const PREVIEW_QUALITY_MAX_WIDTH: f32 = 86.0;
const ZOOM_MIN_WIDTH: f32 = 50.0;
const ZOOM_MAX_WIDTH: f32 = 78.0;

pub(super) fn aspect_ratio(source_width: u32, source_height: u32) -> f32 {
    (source_width.max(1) as f32 / source_height.max(1) as f32).clamp(0.1, 10.0)
}

pub(super) fn canvas_viewport_rect(bounds: Rect) -> Rect {
    Rect::new(
        bounds.x + CANVAS_PADDING,
        bounds.y + CHROME_TOP,
        (bounds.width - CANVAS_PADDING * 2.0).max(0.0),
        (bounds.height - CHROME_TOP - CHROME_BOTTOM).max(0.0),
    )
}

pub(super) fn fit_aspect(bounds: Rect, aspect: f32) -> Rect {
    let aspect = aspect.max(0.001);
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Rect::new(bounds.x, bounds.y, 0.0, 0.0);
    }
    let available_aspect = bounds.width / bounds.height;
    if available_aspect > aspect {
        let width = bounds.height * aspect;
        Rect::new(
            bounds.x + (bounds.width - width) * 0.5,
            bounds.y,
            width,
            bounds.height,
        )
    } else {
        let height = bounds.width / aspect;
        Rect::new(
            bounds.x,
            bounds.y + (bounds.height - height) * 0.5,
            bounds.width,
            height,
        )
    }
}

pub(super) fn canvas_rect(
    bounds: Rect,
    source_width: u32,
    source_height: u32,
    zoom_scale: Option<f32>,
) -> Rect {
    let available = canvas_viewport_rect(bounds);
    if let Some(scale) = zoom_scale {
        let width = source_width.max(1) as f32 * scale;
        let height = source_height.max(1) as f32 * scale;
        return Rect::new(
            available.x + (available.width - width) * 0.5,
            available.y + (available.height - height) * 0.5,
            width.max(0.0),
            height.max(0.0),
        );
    }
    fit_aspect(available, aspect_ratio(source_width, source_height))
}

pub(super) fn visible_controls(bounds_width: f32) -> &'static [ViewerControl] {
    if bounds_width >= 236.0 {
        &FULL_VIEWER_CONTROLS
    } else if bounds_width >= 180.0 {
        &JUMP_VIEWER_CONTROLS
    } else if bounds_width >= 116.0 {
        &BASIC_VIEWER_CONTROLS
    } else {
        &MINIMAL_VIEWER_CONTROLS
    }
}

pub(super) fn control_width(_control: ViewerControl) -> f32 {
    ViewerMetrics::current().transport_button_size
}

pub(super) fn control_strip_rect(bounds: Rect) -> Rect {
    let controls = visible_controls(bounds.width);
    let metrics = ViewerMetrics::current();
    let width = controls.iter().map(|control| control_width(*control)).sum::<f32>()
        + metrics.transport_button_gap * (controls.len().saturating_sub(1) as f32);
    Rect::new(
        bounds.x + (bounds.width - width) * 0.5,
        bounds.y + bounds.height - 34.0,
        width,
        metrics.transport_button_size,
    )
}

pub(super) fn control_rect(bounds: Rect, control: ViewerControl) -> Rect {
    let strip = control_strip_rect(bounds);
    let mut x = strip.x;
    let metrics = ViewerMetrics::current();
    for &candidate in visible_controls(bounds.width) {
        let width = control_width(candidate);
        if candidate == control {
            return Rect::new(x, strip.y, width, metrics.transport_button_size);
        }
        x += width + metrics.transport_button_gap;
    }
    Rect::ZERO
}

pub(super) fn control_at(bounds: Rect, point: Point) -> Option<ViewerControl> {
    visible_controls(bounds.width)
        .iter()
        .copied()
        .find(|control| control_rect(bounds, *control).contains(point))
}

pub(super) fn preview_quality_rect(bounds: Rect, preview_quality_label: &str) -> Rect {
    let control_strip = control_strip_rect(bounds);
    let left = control_strip.x + control_strip.width + CHIP_GAP_FROM_CONTROLS;
    let right = bounds.x + bounds.width - CHIP_RIGHT_INSET;
    let available = right - left;
    if available < 48.0 {
        return Rect::ZERO;
    }
    let wanted = (preview_quality_label.chars().count() as f32 * CHIP_CHAR_WIDTH + CHIP_BASE_WIDTH)
        .clamp(PREVIEW_QUALITY_MIN_WIDTH, PREVIEW_QUALITY_MAX_WIDTH);
    let width = wanted.min(available);
    Rect::new(right - width, bounds.y + bounds.height - 29.0, width, 22.0)
}

pub(super) fn zoom_rect(bounds: Rect, zoom_label: &str, preview_quality_label: &str) -> Rect {
    let quality = preview_quality_rect(bounds, preview_quality_label);
    if quality.width <= 0.0 {
        return Rect::ZERO;
    }
    let control_strip = control_strip_rect(bounds);
    let left = control_strip.x + control_strip.width + CHIP_GAP_FROM_CONTROLS;
    let right = quality.x - CHIP_GAP;
    let available = right - left;
    if available < 42.0 {
        return Rect::ZERO;
    }
    let wanted = (zoom_label.chars().count() as f32 * CHIP_CHAR_WIDTH + CHIP_BASE_WIDTH)
        .clamp(ZOOM_MIN_WIDTH, ZOOM_MAX_WIDTH);
    let width = wanted.min(available);
    Rect::new(right - width, quality.y, width, quality.height)
}

pub(super) fn point_in_rect(rect: Rect, point: Point) -> bool {
    rect.width > 0.0 && rect.height > 0.0 && rect.contains(point)
}

pub(super) fn dropdown_options_len(dropdown: ViewerDropdown) -> usize {
    match dropdown {
        ViewerDropdown::Zoom => VIEWER_ZOOM_OPTIONS.len(),
        ViewerDropdown::PreviewQuality => VIEWER_PREVIEW_QUALITY_OPTIONS.len(),
    }
}

pub(super) fn dropdown_anchor_rect(
    bounds: Rect,
    dropdown: ViewerDropdown,
    zoom_label: &str,
    preview_quality_label: &str,
) -> Rect {
    match dropdown {
        ViewerDropdown::Zoom => zoom_rect(bounds, zoom_label, preview_quality_label),
        ViewerDropdown::PreviewQuality => preview_quality_rect(bounds, preview_quality_label),
    }
}

pub(super) fn dropdown_rect(
    bounds: Rect,
    overlay_viewport: Option<Rect>,
    dropdown: ViewerDropdown,
    zoom_label: &str,
    preview_quality_label: &str,
) -> Rect {
    let anchor = dropdown_anchor_rect(bounds, dropdown, zoom_label, preview_quality_label);
    if anchor.width <= 0.0 || anchor.height <= 0.0 {
        return Rect::ZERO;
    }
    let row_count = dropdown_options_len(dropdown) as f32;
    let width: f32 = match dropdown {
        ViewerDropdown::Zoom => 92.0,
        ViewerDropdown::PreviewQuality => 74.0,
    };
    let metrics = ViewerMetrics::current();
    let height = row_count * metrics.dropdown_row_height + metrics.dropdown_padding_y * 2.0;
    let viewport = overlay_viewport.unwrap_or(bounds);
    let min_x = viewport.x + 8.0;
    let max_x = (viewport.x + viewport.width - width - 8.0).max(min_x);
    let x = (anchor.x + anchor.width - width).clamp(min_x, max_x);
    let gap = 6.0;
    let below_y = anchor.y + anchor.height + gap;
    let above_y = anchor.y - height - gap;
    let min_y = viewport.y + 8.0;
    let max_y = (viewport.y + viewport.height - height - 8.0).max(min_y);
    let below_fits = below_y + height <= viewport.y + viewport.height - 8.0;
    let above_fits = above_y >= min_y;
    let preferred_y = if below_fits || !above_fits {
        below_y
    } else {
        above_y
    };
    let y = preferred_y.clamp(min_y, max_y);
    Rect::new(x, y, width, height)
}

pub(super) fn dropdown_row_rect(
    bounds: Rect,
    overlay_viewport: Option<Rect>,
    dropdown: ViewerDropdown,
    zoom_label: &str,
    preview_quality_label: &str,
    index: usize,
) -> Rect {
    let menu = dropdown_rect(
        bounds,
        overlay_viewport,
        dropdown,
        zoom_label,
        preview_quality_label,
    );
    let metrics = ViewerMetrics::current();
    Rect::new(
        menu.x + metrics.dropdown_padding_x,
        menu.y + metrics.dropdown_padding_y + index as f32 * metrics.dropdown_row_height,
        (menu.width - metrics.dropdown_padding_x * 2.0).max(0.0),
        metrics.dropdown_row_height,
    )
}

pub(super) fn dropdown_item_at(
    bounds: Rect,
    overlay_viewport: Option<Rect>,
    open_dropdown: Option<ViewerDropdown>,
    zoom_label: &str,
    preview_quality_label: &str,
    point: Point,
) -> Option<(ViewerDropdown, usize)> {
    let dropdown = open_dropdown?;
    let menu = dropdown_rect(
        bounds,
        overlay_viewport,
        dropdown,
        zoom_label,
        preview_quality_label,
    );
    if !menu.contains(point) {
        return None;
    }
    (0..dropdown_options_len(dropdown))
        .find(|index| {
            dropdown_row_rect(
                bounds,
                overlay_viewport,
                dropdown,
                zoom_label,
                preview_quality_label,
                *index,
            )
            .contains(point)
        })
        .map(|index| (dropdown, index))
}

pub(super) fn keyboard_control(key: KeyCode, modifiers: Modifiers) -> Option<ViewerControl> {
    if modifiers != Modifiers::none() {
        return None;
    }
    match key {
        KeyCode::Space => Some(ViewerControl::PlayPause),
        KeyCode::Left => Some(ViewerControl::StepBack),
        KeyCode::Right => Some(ViewerControl::StepForward),
        KeyCode::Home => Some(ViewerControl::JumpStart),
        KeyCode::End => Some(ViewerControl::JumpEnd),
        KeyCode::I => Some(ViewerControl::MarkIn),
        KeyCode::O => Some(ViewerControl::MarkOut),
        _ => None,
    }
}

pub(super) fn safe_guide_rects(canvas: Rect) -> Option<(Rect, Rect)> {
    if canvas.width <= 0.0 || canvas.height <= 0.0 {
        return None;
    }
    Some((
        canvas.inset(canvas.width * 0.05, canvas.height * 0.05),
        canvas.inset(canvas.width * 0.10, canvas.height * 0.10),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_viewport_and_fit_rects_reserve_chrome_and_preserve_aspect() {
        let bounds = Rect::new(10.0, 20.0, 420.0, 260.0);
        let viewport = canvas_viewport_rect(bounds);
        let fit = canvas_rect(bounds, 1920, 1080, None);

        assert_eq!(viewport, Rect::new(26.0, 34.0, 388.0, 202.0));
        assert!((fit.width / fit.height - 16.0 / 9.0).abs() <= 0.001);
        assert!(viewport.contains(fit.center()));
    }

    #[test]
    fn fixed_zoom_uses_source_pixels_and_can_extend_outside_viewport() {
        let bounds = Rect::new(0.0, 0.0, 300.0, 200.0);
        let canvas = canvas_rect(bounds, 3840, 2160, Some(0.25));

        assert_eq!(canvas.width, 960.0);
        assert_eq!(canvas.height, 540.0);
        assert!(canvas.x < bounds.x);
    }

    #[test]
    fn visible_controls_collapse_by_width() {
        assert_eq!(visible_controls(300.0), FULL_VIEWER_CONTROLS);
        assert_eq!(visible_controls(200.0), JUMP_VIEWER_CONTROLS);
        assert_eq!(visible_controls(140.0), BASIC_VIEWER_CONTROLS);
        assert_eq!(visible_controls(80.0), MINIMAL_VIEWER_CONTROLS);
    }

    #[test]
    fn control_strip_and_hit_testing_are_centered() {
        let bounds = Rect::new(0.0, 0.0, 300.0, 180.0);
        let strip = control_strip_rect(bounds);
        let play = control_rect(bounds, ViewerControl::PlayPause);

        assert_eq!(strip.width, 164.0);
        assert_eq!(strip.x, 68.0);
        assert_eq!(
            control_at(bounds, play.center()),
            Some(ViewerControl::PlayPause)
        );
        assert_eq!(control_at(bounds, Point::new(1.0, 1.0)), None);
    }

    #[test]
    fn chips_hide_when_there_is_not_enough_space_and_fit_when_roomy() {
        let narrow = Rect::new(0.0, 0.0, 200.0, 160.0);
        let wide = Rect::new(0.0, 0.0, 520.0, 260.0);

        assert_eq!(preview_quality_rect(narrow, "1/1"), Rect::ZERO);
        let quality = preview_quality_rect(wide, "1/1");
        let zoom = zoom_rect(wide, "适合", "1/1");
        assert!(quality.width > 0.0);
        assert!(zoom.width > 0.0);
        assert!(zoom.x + zoom.width < quality.x);
    }

    #[test]
    fn dropdown_rect_clamps_to_viewer_bounds_and_rows_hit_test() {
        let bounds = Rect::new(10.0, 20.0, 520.0, 240.0);
        let row = dropdown_row_rect(bounds, None, ViewerDropdown::Zoom, "适合", "1/1", 2);
        let hit = dropdown_item_at(
            bounds,
            None,
            Some(ViewerDropdown::Zoom),
            "适合",
            "1/1",
            row.center(),
        );

        assert_eq!(hit, Some((ViewerDropdown::Zoom, 2)));
        let menu = dropdown_rect(bounds, None, ViewerDropdown::Zoom, "适合", "1/1");
        assert!(menu.x >= bounds.x + 8.0);
        assert!(menu.x + menu.width <= bounds.x + bounds.width - 8.0 + 0.001);
    }

    #[test]
    fn dropdown_rect_uses_overlay_viewport_to_expand_below_viewer_panel() {
        let bounds = Rect::new(10.0, 20.0, 520.0, 240.0);
        let viewport = Rect::new(0.0, 0.0, 900.0, 720.0);
        let anchor = dropdown_anchor_rect(bounds, ViewerDropdown::Zoom, "适合", "1/1");
        let menu = dropdown_rect(bounds, Some(viewport), ViewerDropdown::Zoom, "适合", "1/1");

        assert!(
            menu.y > anchor.y + anchor.height,
            "viewer dropdown should open below when the window viewport has room"
        );
        assert!(menu.y + menu.height <= viewport.y + viewport.height - 8.0 + 0.001);
    }

    #[test]
    fn keyboard_controls_ignore_modified_chords() {
        assert_eq!(
            keyboard_control(KeyCode::Space, Modifiers::none()),
            Some(ViewerControl::PlayPause)
        );
        assert_eq!(keyboard_control(KeyCode::Space, Modifiers::ctrl()), None);
        assert_eq!(
            keyboard_control(KeyCode::I, Modifiers::none()),
            Some(ViewerControl::MarkIn)
        );
    }

    #[test]
    fn safe_guide_rects_are_inset_from_canvas() {
        let (action, title) =
            safe_guide_rects(Rect::new(0.0, 0.0, 200.0, 100.0)).expect("valid canvas");

        assert_eq!(action, Rect::new(10.0, 5.0, 180.0, 90.0));
        assert_eq!(title, Rect::new(20.0, 10.0, 160.0, 80.0));
        assert_eq!(safe_guide_rects(Rect::new(0.0, 0.0, 0.0, 100.0)), None);
    }
}
