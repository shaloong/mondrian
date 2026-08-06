use mondrian_ui_core::types::{Point, Rect};

use super::PanelListItem;

const CONTENT_PADDING: f32 = 8.0;
const SCROLLBAR_WIDTH: f32 = 4.0;
const SCROLLBAR_RIGHT_INSET: f32 = 6.0;
const SCROLLBAR_VERTICAL_INSET: f32 = 2.0;
const MIN_SCROLLBAR_THUMB_HEIGHT: f32 = 24.0;
const ROW_VERTICAL_INSET: f32 = 2.0;

pub(super) fn viewport_rect(bounds: Rect, header_height: f32) -> Rect {
    Rect::new(
        bounds.x + CONTENT_PADDING,
        bounds.y + header_height,
        (bounds.width - CONTENT_PADDING * 2.0).max(0.0),
        (bounds.height - header_height - CONTENT_PADDING).max(0.0),
    )
}

pub(super) fn content_height(visible_len: usize, row_height: f32) -> f32 {
    visible_len as f32 * row_height
}

pub(super) fn max_scroll_y(visible_len: usize, row_height: f32, viewport_height: f32) -> f32 {
    (content_height(visible_len, row_height) - viewport_height).max(0.0)
}

pub(super) fn clamp_scroll_y(
    scroll_y: f32,
    visible_len: usize,
    row_height: f32,
    viewport_height: f32,
) -> f32 {
    scroll_y.clamp(0.0, max_scroll_y(visible_len, row_height, viewport_height))
}

pub(super) fn scrollbar_track_rect(
    viewport: Rect,
    visible_len: usize,
    row_height: f32,
) -> Option<Rect> {
    (max_scroll_y(visible_len, row_height, viewport.height) > 0.0 && viewport.height > 0.0)
        .then_some(Rect::new(
            viewport.x + viewport.width - SCROLLBAR_RIGHT_INSET,
            viewport.y + SCROLLBAR_VERTICAL_INSET,
            SCROLLBAR_WIDTH,
            (viewport.height - SCROLLBAR_VERTICAL_INSET * 2.0).max(0.0),
        ))
}

pub(super) fn scrollbar_thumb_rect(
    viewport: Rect,
    visible_len: usize,
    row_height: f32,
    scroll_y: f32,
) -> Option<Rect> {
    let track = scrollbar_track_rect(viewport, visible_len, row_height)?;
    let content_height = content_height(visible_len, row_height);
    if content_height <= 0.0 {
        return None;
    }

    let thumb_height = (viewport.height / content_height * track.height).clamp(
        MIN_SCROLLBAR_THUMB_HEIGHT,
        track.height.max(MIN_SCROLLBAR_THUMB_HEIGHT),
    );
    let travel = (track.height - thumb_height).max(0.0);
    let max_scroll = max_scroll_y(visible_len, row_height, viewport.height);
    let y = if max_scroll <= 0.0 {
        track.y
    } else {
        track.y + (scroll_y / max_scroll) * travel
    };

    Some(Rect::new(
        track.x,
        y,
        track.width,
        thumb_height.min(track.height),
    ))
}

pub(super) fn scroll_y_for_thumb_delta(
    viewport: Rect,
    visible_len: usize,
    row_height: f32,
    scroll_y: f32,
    drag_start_scroll_y: f32,
    delta_y: f32,
) -> f32 {
    let Some(track) = scrollbar_track_rect(viewport, visible_len, row_height) else {
        return scroll_y;
    };
    let Some(thumb) = scrollbar_thumb_rect(viewport, visible_len, row_height, scroll_y) else {
        return scroll_y;
    };
    let travel = (track.height - thumb.height).max(1.0);
    drag_start_scroll_y + delta_y / travel * max_scroll_y(visible_len, row_height, viewport.height)
}

pub(super) fn row_rect_for_visible_position(
    viewport: Rect,
    visible_position: usize,
    row_height: f32,
    scroll_y: f32,
) -> Rect {
    let y = viewport.y + visible_position as f32 * row_height - scroll_y;
    Rect::new(
        viewport.x,
        y + ROW_VERTICAL_INSET,
        viewport.width,
        row_height - ROW_VERTICAL_INSET * 2.0,
    )
}

pub(super) fn visible_position_for_index(visible_indices: &[usize], index: usize) -> Option<usize> {
    visible_indices.iter().position(|candidate| *candidate == index)
}

pub(super) fn index_at(
    visible_indices: &[usize],
    viewport: Rect,
    row_height: f32,
    scroll_y: f32,
    point: Point,
) -> Option<usize> {
    if !viewport.contains(point) {
        return None;
    }
    if scrollbar_track_rect(viewport, visible_indices.len(), row_height)
        .is_some_and(|track| track.contains(point))
    {
        return None;
    }
    let rel_y = point.y - viewport.y + scroll_y;
    let visible_row = (rel_y / row_height).floor() as usize;
    visible_indices.get(visible_row).copied()
}

pub(super) fn item_matches_query(item: &PanelListItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }

    item.title.to_lowercase().contains(query)
        || item.subtitle.to_lowercase().contains(query)
        || item
            .badge
            .as_ref()
            .is_some_and(|badge| badge.label.to_lowercase().contains(query))
}

pub(super) fn rebuild_visible_indices(items: &[PanelListItem], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let filtering = !query.is_empty();
    let mut hidden_child_depth = None;
    let mut visible_indices = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if !filtering && let Some(depth) = hidden_child_depth {
            if item.tree_depth > depth {
                continue;
            }
            hidden_child_depth = None;
        }

        if item_matches_query(item, &query) {
            visible_indices.push(index);
        }

        if !filtering && item.tree_expanded == Some(false) {
            hidden_child_depth = Some(item.tree_depth);
        }
    }
    visible_indices
}

pub(super) fn visible_enabled_indices(
    visible_indices: &[usize],
    items: &[PanelListItem],
) -> Vec<usize> {
    visible_indices
        .iter()
        .copied()
        .filter(|index| items.get(*index).is_some_and(|item| !item.disabled))
        .collect()
}

pub(super) fn next_enabled_from(
    visible_indices: &[usize],
    items: &[PanelListItem],
    start: usize,
    direction: i32,
) -> Option<usize> {
    let visible = visible_enabled_indices(visible_indices, items);
    if visible.is_empty() {
        return None;
    }

    let start_position = visible
        .iter()
        .position(|index| *index >= start)
        .unwrap_or_else(|| visible.len().saturating_sub(1));
    if direction >= 0 {
        visible.get(start_position).copied()
    } else {
        visible
            .iter()
            .rposition(|index| *index <= start)
            .and_then(|position| visible.get(position).copied())
    }
}

pub(super) fn move_selection(
    visible_indices: &[usize],
    items: &[PanelListItem],
    selected: Option<usize>,
    direction: i32,
) -> Option<usize> {
    let visible = visible_enabled_indices(visible_indices, items);
    if visible.is_empty() {
        return None;
    }

    let Some(selected) = selected else {
        return if direction >= 0 {
            visible.first().copied()
        } else {
            visible.last().copied()
        };
    };
    let Some(position) = visible.iter().position(|index| *index == selected) else {
        return if direction >= 0 {
            visible.first().copied()
        } else {
            visible.last().copied()
        };
    };
    if direction >= 0 {
        visible.get(position + 1).copied()
    } else {
        position.checked_sub(1).and_then(|prev| visible.get(prev).copied())
    }
}

pub(super) fn scroll_y_with_selected_visible(
    selected: Option<usize>,
    visible_indices: &[usize],
    row_height: f32,
    viewport_height: f32,
    scroll_y: f32,
) -> f32 {
    let Some(index) = selected else {
        return scroll_y;
    };
    if viewport_height <= 0.0 {
        return scroll_y;
    }
    let Some(visible_position) = visible_position_for_index(visible_indices, index) else {
        return scroll_y;
    };
    let top = visible_position as f32 * row_height;
    let bottom = top + row_height;
    let scroll_y = if top < scroll_y {
        top
    } else if bottom > scroll_y + viewport_height {
        bottom - viewport_height
    } else {
        scroll_y
    };
    clamp_scroll_y(scroll_y, visible_indices.len(), row_height, viewport_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel_header::PanelHeader;
    use mondrian_core::Color;

    fn item(title: &str) -> PanelListItem {
        PanelListItem::new(title)
    }

    #[test]
    fn chrome_height_accounts_for_header_subtitle_and_embedded_filter() {
        assert_eq!(PanelHeader::new("Test").height(), 42.0);
        assert_eq!(
            PanelHeader::new("Test").with_subtitle("Sub").with_filter(true).height(),
            100.0
        );
        assert_eq!(
            PanelHeader::new("Test").with_embedded().with_filter(true).height(),
            48.0
        );
    }

    #[test]
    fn filter_and_viewport_rects_are_clamped_inside_bounds() {
        let bounds = Rect::new(10.0, 20.0, 120.0, 80.0);

        assert_eq!(
            PanelHeader::new("Test").with_filter(true).filter_input_rect(bounds),
            Some(Rect::new(18.0, 58.0, 104.0, 30.0))
        );
        assert_eq!(
            viewport_rect(
                bounds,
                PanelHeader::new("Test").with_embedded().with_filter(true).height()
            ),
            Rect::new(18.0, 68.0, 104.0, 24.0)
        );
        assert_eq!(
            viewport_rect(Rect::new(0.0, 0.0, 12.0, 12.0), 20.0),
            Rect::new(8.0, 20.0, 0.0, 0.0)
        );
    }

    #[test]
    fn scrollbar_geometry_maps_scroll_offset_to_thumb_position() {
        let viewport = Rect::new(10.0, 20.0, 200.0, 100.0);
        let track = scrollbar_track_rect(viewport, 10, 20.0).expect("overflowing list");
        let top_thumb = scrollbar_thumb_rect(viewport, 10, 20.0, 0.0).expect("thumb");
        let bottom_thumb = scrollbar_thumb_rect(viewport, 10, 20.0, 100.0).expect("thumb");

        assert_eq!(track, Rect::new(204.0, 22.0, 4.0, 96.0));
        assert_eq!(top_thumb.y, track.y);
        assert!(bottom_thumb.y > top_thumb.y);
        assert!(bottom_thumb.y + bottom_thumb.height <= track.y + track.height + 0.001);
    }

    #[test]
    fn row_hit_testing_ignores_outside_viewport_and_scrollbar_track() {
        let viewport = Rect::new(10.0, 20.0, 100.0, 80.0);
        let visible = vec![2, 4, 6, 8, 10];

        assert_eq!(
            index_at(&visible, viewport, 20.0, 20.0, Point::new(18.0, 21.0)),
            Some(4)
        );
        assert_eq!(
            index_at(&visible, viewport, 20.0, 20.0, Point::new(106.0, 42.0)),
            None
        );
        assert_eq!(
            index_at(&visible, viewport, 20.0, 20.0, Point::new(9.0, 42.0)),
            None
        );
    }

    #[test]
    fn visible_indices_skip_children_of_collapsed_tree_nodes_until_depth_unwinds() {
        let items = vec![
            item("Root").with_tree_node("root", false),
            item("Hidden child").with_tree_depth(1),
            item("Still hidden").with_tree_depth(2),
            item("Sibling"),
        ];

        assert_eq!(rebuild_visible_indices(&items, ""), vec![0, 3]);
        assert_eq!(rebuild_visible_indices(&items, "hidden"), vec![1, 2]);
    }

    #[test]
    fn query_matches_title_subtitle_and_badge_case_insensitively() {
        let items = vec![
            item("Clip").with_subtitle("Dialogue"),
            item("Effect").with_badge("GPU"),
            item("Folder").with_accent(Color::from_hex(0x336699)),
        ];

        assert_eq!(rebuild_visible_indices(&items, "dia"), vec![0]);
        assert_eq!(rebuild_visible_indices(&items, "gpu"), vec![1]);
        assert_eq!(rebuild_visible_indices(&items, "FOLD"), vec![2]);
    }

    #[test]
    fn selection_navigation_skips_disabled_and_handles_missing_selection() {
        let items = vec![item("A"), item("B").disabled(true), item("C"), item("D")];
        let visible = vec![0, 1, 2, 3];

        assert_eq!(move_selection(&visible, &items, None, 1), Some(0));
        assert_eq!(move_selection(&visible, &items, Some(0), 1), Some(2));
        assert_eq!(move_selection(&visible, &items, Some(2), -1), Some(0));
        assert_eq!(next_enabled_from(&visible, &items, 3, -1), Some(3));
    }

    #[test]
    fn selected_scroll_is_adjusted_only_when_row_is_outside_viewport() {
        let visible = vec![0, 1, 2, 3, 4, 5];

        assert_eq!(
            scroll_y_with_selected_visible(Some(4), &visible, 20.0, 60.0, 0.0),
            40.0
        );
        assert_eq!(
            scroll_y_with_selected_visible(Some(2), &visible, 20.0, 60.0, 20.0),
            20.0
        );
        assert_eq!(
            scroll_y_with_selected_visible(Some(0), &visible, 20.0, 60.0, 80.0),
            0.0
        );
    }
}
