use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_theme::spacing::SpacingTokens;
use std::collections::BTreeSet;

use super::AssetGridItem;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct AssetGridMetrics {
    pub content_padding: f32,
    pub card_gap: f32,
    pub card_width: f32,
    pub card_height: f32,
    pub preview_aspect_ratio: f32,
    pub card_radius: f32,
    pub icon_size: f32,
    pub preview_padding: f32,
    pub footer_padding_x: f32,
    pub footer_top_gap: f32,
    pub footer_height: f32,
}

impl AssetGridMetrics {
    pub fn from_spacing(spacing: &SpacingTokens) -> Self {
        Self {
            content_padding: spacing.asset_grid_content_padding,
            card_gap: spacing.asset_grid_card_gap,
            card_width: spacing.asset_grid_card_width,
            card_height: spacing.asset_grid_card_height,
            preview_aspect_ratio: spacing.asset_grid_preview_aspect_ratio,
            card_radius: spacing.asset_grid_card_radius,
            icon_size: spacing.asset_grid_icon_size,
            preview_padding: spacing.asset_grid_preview_padding,
            footer_padding_x: spacing.asset_grid_footer_padding_x,
            footer_top_gap: spacing.asset_grid_footer_top_gap,
            footer_height: spacing.asset_grid_footer_height,
        }
    }
}

impl Default for AssetGridMetrics {
    fn default() -> Self {
        Self::from_spacing(&SpacingTokens::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GridLayout {
    pub columns: usize,
    pub card_width: f32,
    pub viewport: Rect,
}

pub(super) fn grid_columns_for_width(width: f32, metrics: AssetGridMetrics) -> usize {
    if width <= metrics.card_width {
        return 1;
    }
    let columns =
        ((width + metrics.card_gap) / (metrics.card_width + metrics.card_gap)).floor() as usize;
    columns.max(1)
}

pub(super) fn content_height_for_width(
    width: f32,
    item_count: usize,
    metrics: AssetGridMetrics,
) -> f32 {
    let viewport_width = (width - metrics.content_padding * 2.0).max(0.0);
    let columns = grid_columns_for_width(viewport_width, metrics);
    content_height(item_count, columns, metrics)
}

pub(super) fn content_height(item_count: usize, columns: usize, metrics: AssetGridMetrics) -> f32 {
    let rows = item_count.div_ceil(columns.max(1));
    metrics.content_padding * 2.0
        + rows as f32 * metrics.card_height
        + rows.saturating_sub(1) as f32 * metrics.card_gap
}

pub(super) fn layout_for_bounds(
    bounds: Rect,
    header_height: f32,
    item_count: usize,
    metrics: AssetGridMetrics,
) -> GridLayout {
    let viewport_width = (bounds.width - metrics.content_padding * 2.0).max(0.0);
    let columns = grid_columns_for_width(viewport_width, metrics);
    let viewport = Rect::new(
        bounds.x,
        bounds.y + header_height,
        bounds.width.max(0.0),
        content_height(item_count, columns, metrics).max((bounds.height - header_height).max(0.0)),
    );
    GridLayout { columns, card_width: metrics.card_width, viewport }
}

pub(super) fn card_rect_at_visible_position_with_metrics(
    viewport: Rect,
    columns: usize,
    card_width: f32,
    visible_position: usize,
    metrics: AssetGridMetrics,
) -> Rect {
    let columns = columns.max(1);
    let col = visible_position % columns;
    let row = visible_position / columns;
    Rect::new(
        viewport.x + metrics.content_padding + col as f32 * (card_width + metrics.card_gap),
        viewport.y
            + metrics.content_padding
            + row as f32 * (metrics.card_height + metrics.card_gap),
        card_width,
        metrics.card_height,
    )
}

pub(super) fn preview_rect_for_card(card: Rect, metrics: AssetGridMetrics) -> Rect {
    let width = (card.width - metrics.preview_padding * 2.0).max(0.0);
    Rect::new(
        card.x + metrics.preview_padding,
        card.y + metrics.preview_padding,
        width,
        width / metrics.preview_aspect_ratio,
    )
}

pub(super) fn footer_rect_for_card(card: Rect, metrics: AssetGridMetrics) -> Rect {
    let preview = preview_rect_for_card(card, metrics);
    Rect::new(
        card.x + metrics.footer_padding_x,
        preview.y + preview.height + metrics.footer_top_gap,
        (card.width - metrics.footer_padding_x * 2.0).max(0.0),
        metrics.footer_height,
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
    metrics: AssetGridMetrics,
) -> Option<usize> {
    if !viewport.contains(point) {
        return None;
    }
    visible_indices
        .iter()
        .copied()
        .enumerate()
        .find_map(|(visible_position, index)| {
            card_rect_at_visible_position_with_metrics(
                viewport,
                columns,
                card_width,
                visible_position,
                metrics,
            )
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
    fn grid_columns_and_content_height_use_target_card_width_and_gap() {
        let metrics = AssetGridMetrics::default();
        assert_eq!(grid_columns_for_width(171.0, metrics), 1);
        assert_eq!(grid_columns_for_width(352.0, metrics), 2);
        assert_eq!(
            content_height(5, 2, metrics),
            metrics.content_padding * 2.0 + 3.0 * metrics.card_height + 2.0 * metrics.card_gap
        );
    }

    #[test]
    fn layout_for_bounds_keeps_viewport_at_content_height_when_content_overflows() {
        let bounds = Rect::new(10.0, 20.0, 380.0, 200.0);
        let metrics = AssetGridMetrics::default();
        let layout = layout_for_bounds(bounds, 42.0, 5, metrics);

        assert_eq!(layout.columns, 2);
        assert_eq!(layout.card_width, metrics.card_width);
        assert_eq!(layout.viewport.x, 10.0);
        assert_eq!(layout.viewport.y, 62.0);
        assert!(layout.viewport.height > bounds.height - 42.0);
    }

    #[test]
    fn card_preview_footer_and_fit_rects_are_stable() {
        let card = Rect::new(20.0, 30.0, 172.0, 126.0);
        let metrics = AssetGridMetrics::default();
        let preview = preview_rect_for_card(card, metrics);
        let footer = footer_rect_for_card(card, metrics);
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
        let metrics = AssetGridMetrics::default();
        let second_card =
            card_rect_at_visible_position_with_metrics(viewport, 2, metrics.card_width, 1, metrics);

        assert_eq!(
            index_at(
                &visible,
                viewport,
                2,
                metrics.card_width,
                second_card.center(),
                metrics
            ),
            Some(4)
        );
        assert_eq!(
            index_at(
                &visible,
                viewport,
                2,
                metrics.card_width,
                Point::new(-1.0, 80.0),
                metrics
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
