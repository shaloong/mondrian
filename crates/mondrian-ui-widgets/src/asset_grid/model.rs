use mondrian_ui_core::types::{Point, Rect};
use std::collections::BTreeSet;

use super::{
    AssetGridItem, CARD_GAP, CARD_HEIGHT, CARD_TARGET_WIDTH, CONTENT_PADDING, FILTER_INPUT_HEIGHT,
    HEADER_GAP, HEADER_PADDING_X, PREVIEW_ASPECT_RATIO,
};

const HEADER_ONLY_HEIGHT: f32 = 42.0;
const HEADER_WITH_SUBTITLE_HEIGHT: f32 = 60.0;
const PREVIEW_PADDING: f32 = 6.0;
const FOOTER_PADDING_X: f32 = 8.0;
const FOOTER_TOP_GAP: f32 = 7.0;
const FOOTER_HEIGHT: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GridLayout {
    pub columns: usize,
    pub card_width: f32,
    pub viewport: Rect,
}

pub(super) fn title_block_height(show_header_text: bool, has_subtitle: bool) -> f32 {
    if !show_header_text {
        return 0.0;
    }
    if has_subtitle {
        HEADER_WITH_SUBTITLE_HEIGHT
    } else {
        HEADER_ONLY_HEIGHT
    }
}

pub(super) fn filter_top_padding(show_header_text: bool) -> f32 {
    if show_header_text {
        0.0
    } else {
        CONTENT_PADDING
    }
}

pub(super) fn header_height(show_header_text: bool, has_subtitle: bool, has_filter: bool) -> f32 {
    let base = title_block_height(show_header_text, has_subtitle);
    if has_filter {
        base + filter_top_padding(show_header_text) + FILTER_INPUT_HEIGHT + HEADER_GAP
    } else {
        base
    }
}

pub(super) fn grid_columns_for_width(width: f32) -> usize {
    if width <= CARD_TARGET_WIDTH {
        return 1;
    }
    let columns = ((width + CARD_GAP) / (CARD_TARGET_WIDTH + CARD_GAP)).floor() as usize;
    columns.max(1)
}

pub(super) fn content_height_for_width(width: f32, item_count: usize) -> f32 {
    let viewport_width = (width - CONTENT_PADDING * 2.0).max(0.0);
    let columns = grid_columns_for_width(viewport_width);
    content_height(item_count, columns)
}

pub(super) fn content_height(item_count: usize, columns: usize) -> f32 {
    let rows = item_count.div_ceil(columns.max(1));
    CONTENT_PADDING * 2.0 + rows as f32 * CARD_HEIGHT + rows.saturating_sub(1) as f32 * CARD_GAP
}

pub(super) fn filter_input_rect(
    bounds: Rect,
    show_header_text: bool,
    has_subtitle: bool,
    has_filter: bool,
) -> Option<Rect> {
    if !has_filter {
        return None;
    }
    let y = if show_header_text {
        bounds.y + title_block_height(show_header_text, has_subtitle) - 4.0
    } else {
        bounds.y + CONTENT_PADDING
    };
    Some(Rect::new(
        bounds.x + HEADER_PADDING_X,
        y,
        (bounds.width - HEADER_PADDING_X * 2.0).max(0.0),
        FILTER_INPUT_HEIGHT,
    ))
}

pub(super) fn layout_for_bounds(bounds: Rect, header_height: f32, item_count: usize) -> GridLayout {
    let viewport_width = (bounds.width - CONTENT_PADDING * 2.0).max(0.0);
    let columns = grid_columns_for_width(viewport_width);
    let viewport = Rect::new(
        bounds.x,
        bounds.y + header_height,
        bounds.width.max(0.0),
        content_height(item_count, columns).max((bounds.height - header_height).max(0.0)),
    );
    GridLayout { columns, card_width: CARD_TARGET_WIDTH, viewport }
}

pub(super) fn card_rect_at_visible_position(
    viewport: Rect,
    columns: usize,
    card_width: f32,
    visible_position: usize,
) -> Rect {
    let columns = columns.max(1);
    let col = visible_position % columns;
    let row = visible_position / columns;
    Rect::new(
        viewport.x + CONTENT_PADDING + col as f32 * (card_width + CARD_GAP),
        viewport.y + CONTENT_PADDING + row as f32 * (CARD_HEIGHT + CARD_GAP),
        card_width,
        CARD_HEIGHT,
    )
}

pub(super) fn preview_rect_for_card(card: Rect) -> Rect {
    let width = (card.width - PREVIEW_PADDING * 2.0).max(0.0);
    Rect::new(
        card.x + PREVIEW_PADDING,
        card.y + PREVIEW_PADDING,
        width,
        width / PREVIEW_ASPECT_RATIO,
    )
}

pub(super) fn footer_rect_for_card(card: Rect) -> Rect {
    let preview = preview_rect_for_card(card);
    Rect::new(
        card.x + FOOTER_PADDING_X,
        preview.y + preview.height + FOOTER_TOP_GAP,
        (card.width - FOOTER_PADDING_X * 2.0).max(0.0),
        FOOTER_HEIGHT,
    )
}

pub(super) fn visible_position_for_index(visible_indices: &[usize], index: usize) -> Option<usize> {
    visible_indices.iter().position(|candidate| *candidate == index)
}

pub(super) fn index_at(
    visible_indices: &[usize],
    viewport: Rect,
    columns: usize,
    card_width: f32,
    point: Point,
) -> Option<usize> {
    if !viewport.contains(point) {
        return None;
    }
    visible_indices
        .iter()
        .copied()
        .enumerate()
        .find_map(|(visible_position, index)| {
            card_rect_at_visible_position(viewport, columns, card_width, visible_position)
                .contains(point)
                .then_some(index)
        })
}

pub(super) fn fit_rect_into(source_width: f32, source_height: f32, bounds: Rect) -> Rect {
    if source_width <= 0.0 || source_height <= 0.0 || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return bounds;
    }
    let scale = (bounds.width / source_width).min(bounds.height / source_height);
    let width = source_width * scale;
    let height = source_height * scale;
    Rect::new(
        bounds.x + (bounds.width - width) * 0.5,
        bounds.y + (bounds.height - height) * 0.5,
        width,
        height,
    )
}

pub(super) fn item_matches_query(item: &AssetGridItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    item.title.to_lowercase().contains(query)
        || item.subtitle.to_lowercase().contains(query)
        || item.badges.iter().any(|badge| badge.label.to_lowercase().contains(query))
}

pub(super) fn rebuild_visible_indices(items: &[AssetGridItem], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item_matches_query(item, &query).then_some(index))
        .collect()
}

pub(super) fn visible_enabled_indices(
    visible_indices: &[usize],
    items: &[AssetGridItem],
) -> Vec<usize> {
    visible_indices
        .iter()
        .copied()
        .filter(|index| items.get(*index).is_some_and(|item| !item.disabled))
        .collect()
}

pub(super) fn move_selection(
    visible_indices: &[usize],
    items: &[AssetGridItem],
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
        visible
            .get((position + direction as usize).min(visible.len().saturating_sub(1)))
            .copied()
    } else {
        position
            .checked_sub(direction.unsigned_abs() as usize)
            .and_then(|prev| visible.get(prev).copied())
    }
}

pub(super) fn selected_range(
    visible_indices: &[usize],
    items: &[AssetGridItem],
    start: usize,
    end: usize,
) -> BTreeSet<usize> {
    let Some(start_position) = visible_position_for_index(visible_indices, start) else {
        return BTreeSet::new();
    };
    let Some(end_position) = visible_position_for_index(visible_indices, end) else {
        return BTreeSet::new();
    };
    let (first, last) = if start_position <= end_position {
        (start_position, end_position)
    } else {
        (end_position, start_position)
    };
    visible_indices[first..=last]
        .iter()
        .copied()
        .filter(|index| items.get(*index).is_some_and(|item| !item.disabled))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::Color;

    fn item(id: &str, title: &str) -> AssetGridItem {
        AssetGridItem::new(id, title, Color::from_hex(0x6688CC))
    }

    #[test]
    fn chrome_height_accounts_for_header_subtitle_and_embedded_filter() {
        assert_eq!(header_height(true, false, false), 42.0);
        assert_eq!(header_height(true, true, true), 98.0);
        assert_eq!(header_height(false, false, true), 46.0);
    }

    #[test]
    fn grid_columns_and_content_height_use_target_card_width_and_gap() {
        assert_eq!(grid_columns_for_width(171.0), 1);
        assert_eq!(grid_columns_for_width(352.0), 2);
        assert_eq!(
            content_height(5, 2),
            CONTENT_PADDING * 2.0 + 3.0 * CARD_HEIGHT + 2.0 * CARD_GAP
        );
    }

    #[test]
    fn layout_for_bounds_keeps_viewport_at_content_height_when_content_overflows() {
        let bounds = Rect::new(10.0, 20.0, 380.0, 200.0);
        let layout = layout_for_bounds(bounds, 42.0, 5);

        assert_eq!(layout.columns, 2);
        assert_eq!(layout.card_width, CARD_TARGET_WIDTH);
        assert_eq!(layout.viewport.x, 10.0);
        assert_eq!(layout.viewport.y, 62.0);
        assert!(layout.viewport.height > bounds.height - 42.0);
    }

    #[test]
    fn card_preview_footer_and_fit_rects_are_stable() {
        let card = Rect::new(20.0, 30.0, 172.0, 126.0);
        let preview = preview_rect_for_card(card);
        let footer = footer_rect_for_card(card);
        let fitted = fit_rect_into(4.0, 2.0, preview);

        assert_eq!(preview, Rect::new(26.0, 36.0, 160.0, 90.0));
        assert_eq!(footer, Rect::new(28.0, 133.0, 156.0, 18.0));
        assert_eq!(fitted.width, 160.0);
        assert_eq!(fitted.height, 80.0);
        assert_eq!(fitted.y, 41.0);
    }

    #[test]
    fn hit_testing_maps_visible_positions_back_to_model_indices() {
        let viewport = Rect::new(0.0, 40.0, 380.0, 300.0);
        let visible = vec![2, 4, 8];
        let second_card = card_rect_at_visible_position(viewport, 2, CARD_TARGET_WIDTH, 1);

        assert_eq!(
            index_at(
                &visible,
                viewport,
                2,
                CARD_TARGET_WIDTH,
                second_card.center()
            ),
            Some(4)
        );
        assert_eq!(
            index_at(
                &visible,
                viewport,
                2,
                CARD_TARGET_WIDTH,
                Point::new(-1.0, 80.0)
            ),
            None
        );
    }

    #[test]
    fn query_matches_title_subtitle_and_badges_case_insensitively() {
        let items = vec![
            item("clip-a", "Camera").with_subtitle("Rec.709"),
            item("clip-b", "Music").with_badge("Audio"),
            item("folder", "Brand Pack"),
        ];

        assert_eq!(rebuild_visible_indices(&items, "rec"), vec![0]);
        assert_eq!(rebuild_visible_indices(&items, "AUDIO"), vec![1]);
        assert_eq!(rebuild_visible_indices(&items, "brand"), vec![2]);
    }

    #[test]
    fn selection_navigation_and_ranges_skip_disabled_items() {
        let items = vec![
            item("a", "A"),
            item("b", "B").disabled(true),
            item("c", "C"),
            item("d", "D"),
        ];
        let visible = vec![0, 1, 2, 3];

        assert_eq!(move_selection(&visible, &items, None, 1), Some(0));
        assert_eq!(move_selection(&visible, &items, Some(0), 1), Some(2));
        assert_eq!(move_selection(&visible, &items, Some(3), -2), Some(0));
        assert_eq!(
            selected_range(&visible, &items, 0, 3),
            BTreeSet::from([0, 2, 3])
        );
    }
}
