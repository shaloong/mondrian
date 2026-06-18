//! Card-grid browser surface for media assets and similar project resources.
//!
//! `AssetGrid` is deliberately domain-light: it owns local browser interaction
//! such as filtering, selection, activation, drag initiation, and file drops,
//! while app crates map real domain records into item view models.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::paint::{color_with_alpha, mix_color, paint_focus_ring, soft_border};
use crate::vector_icon::VectorIcon;
use crate::ContextMenu;
use crate::MenuItem;
use crate::RasterImage;
use crate::TextInput;

const DOUBLE_CLICK_MAX_AGE: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_MAX_DISTANCE: f32 = 5.0;
const DRAG_START_DISTANCE: f32 = 6.0;
const FILTER_INPUT_HEIGHT: f32 = 28.0;
const HEADER_GAP: f32 = 10.0;
const HEADER_PADDING_X: f32 = 12.0;
const CONTENT_PADDING: f32 = 10.0;
const CARD_GAP: f32 = 10.0;
const CARD_TARGET_WIDTH: f32 = 158.0;
const CARD_MIN_WIDTH: f32 = 118.0;
const CARD_MAX_WIDTH: f32 = 190.0;
const CARD_HEIGHT: f32 = 118.0;
const THUMBNAIL_HEIGHT: f32 = 66.0;
const CARD_RADIUS: f32 = 7.0;
const ICON_SIZE: f32 = 22.0;

/// Dynamic action factory for [`AssetGrid`] item selection or activation.
pub type AssetGridAction = dyn Fn(usize, &AssetGridItem) -> Action;
/// Dynamic action factory for payloads dropped on an [`AssetGrid`].
pub type AssetGridDropAction = dyn Fn(&DragPayload, Point) -> Option<Action>;
/// Dynamic action factory for payloads dropped on one [`AssetGridItem`].
pub type AssetGridItemDropAction = dyn Fn(&DragPayload, usize, &AssetGridItem) -> Option<Action>;

/// Local browser state for preserving an [`AssetGrid`] across model refreshes.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetGridState {
    /// Text currently committed in the optional filter input.
    pub filter_query: String,
    /// Selected item by stable id when available.
    pub selected_item_id: Option<String>,
    /// Selected item by model index as a fallback.
    pub selected_index: Option<usize>,
}

/// Non-image state for an [`AssetGridItem`] preview region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetGridThumbnailStatus {
    /// No thumbnail is expected; paint the normal icon placeholder.
    None,
    /// A thumbnail request is in flight.
    Loading,
    /// A thumbnail was expected but could not be produced.
    Failed,
}

/// Single card rendered by [`AssetGrid`].
#[derive(Debug, Clone)]
pub struct AssetGridItem {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub badge: Option<String>,
    pub accent: Color,
    pub icon: Option<VectorIcon>,
    pub thumbnail: Option<RasterImage>,
    pub thumbnail_status: AssetGridThumbnailStatus,
    pub disabled: bool,
    pub select_action: Option<Action>,
    pub activate_action: Option<Action>,
    pub drag_payload: Option<DragPayload>,
    pub context_menu_items: Vec<MenuItem>,
}

impl AssetGridItem {
    /// Create an enabled asset-grid card.
    pub fn new(id: impl Into<String>, title: impl Into<String>, accent: Color) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: String::new(),
            badge: None,
            accent,
            icon: None,
            thumbnail: None,
            thumbnail_status: AssetGridThumbnailStatus::None,
            disabled: false,
            select_action: None,
            activate_action: None,
            drag_payload: None,
            context_menu_items: Vec::new(),
        }
    }

    /// Set secondary card text.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Set a compact kind/status badge.
    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// Set a vector icon painted in the preview region.
    pub fn with_icon(mut self, icon: VectorIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Set a raster thumbnail painted in the preview region.
    pub fn with_thumbnail(mut self, thumbnail: RasterImage) -> Self {
        self.thumbnail = Some(thumbnail);
        self.thumbnail_status = AssetGridThumbnailStatus::None;
        self
    }

    /// Mark the preview region as waiting for an async thumbnail.
    pub fn with_thumbnail_loading(mut self) -> Self {
        self.thumbnail = None;
        self.thumbnail_status = AssetGridThumbnailStatus::Loading;
        self
    }

    /// Mark the preview region as failed to load a thumbnail.
    pub fn with_thumbnail_failed(mut self) -> Self {
        self.thumbnail = None;
        self.thumbnail_status = AssetGridThumbnailStatus::Failed;
        self
    }

    /// Mark the card as disabled.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Dispatch a static action when this card becomes selected.
    pub fn with_select_action(mut self, action: Action) -> Self {
        self.select_action = Some(action);
        self
    }

    /// Dispatch a static action when this card is activated.
    pub fn with_activate_action(mut self, action: Action) -> Self {
        self.activate_action = Some(action);
        self
    }

    /// Start an internal UI drag with this payload after pointer movement.
    pub fn with_drag_payload(mut self, payload: DragPayload) -> Self {
        self.drag_payload = Some(payload);
        self
    }

    /// Attach a card-specific right-click context menu.
    pub fn with_context_menu(mut self, items: Vec<MenuItem>) -> Self {
        self.context_menu_items = items;
        self
    }
}

/// Searchable, selectable card grid for asset-library style panels.
pub struct AssetGrid {
    id: WidgetId,
    title: String,
    subtitle: String,
    items: Vec<AssetGridItem>,
    filter_input: Option<Box<TextInput>>,
    filter_query: String,
    visible_indices: Vec<usize>,
    bounds: Rect,
    viewport: Rect,
    selected: Option<usize>,
    hovered: Option<usize>,
    focused: bool,
    focus_visible: bool,
    drop_hovered: bool,
    last_click: Option<AssetGridClick>,
    drag_candidate: Option<AssetGridDragCandidate>,
    columns: usize,
    card_width: f32,
    context_menu_items: Vec<MenuItem>,
    context_menu: Option<ContextMenu>,
    on_select: Option<Box<AssetGridAction>>,
    on_activate: Option<Box<AssetGridAction>>,
    on_drop: Option<Box<AssetGridDropAction>>,
    on_item_drop: Option<Box<AssetGridItemDropAction>>,
}

#[derive(Debug, Clone, Copy)]
struct AssetGridClick {
    index: usize,
    position: Point,
    time: Instant,
}

#[derive(Debug, Clone)]
struct AssetGridDragCandidate {
    index: usize,
    start: Point,
    payload: DragPayload,
}

impl AssetGrid {
    /// Create a card grid with a title and cards.
    pub fn new(title: impl Into<String>, items: Vec<AssetGridItem>) -> Self {
        let visible_indices = (0..items.len()).collect();
        Self {
            id: WidgetId::new(),
            title: title.into(),
            subtitle: String::new(),
            items,
            filter_input: None,
            filter_query: String::new(),
            visible_indices,
            bounds: Rect::ZERO,
            viewport: Rect::ZERO,
            selected: None,
            hovered: None,
            focused: false,
            focus_visible: false,
            drop_hovered: false,
            last_click: None,
            drag_candidate: None,
            columns: 1,
            card_width: CARD_TARGET_WIDTH,
            context_menu_items: Vec::new(),
            context_menu: None,
            on_select: None,
            on_activate: None,
            on_drop: None,
            on_item_drop: None,
        }
    }

    /// Attach a right-click context menu to the grid surface.
    ///
    /// The menu stays domain-light: items carry already-mapped app actions,
    /// while `AssetGrid` only owns popup placement, overlay painting, keyboard
    /// navigation, and dismissal.
    pub fn with_context_menu(mut self, items: Vec<MenuItem>) -> Self {
        self.context_menu_items = items;
        self
    }

    /// Set a small explanatory subtitle below the title.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Add a real filter input above the card grid.
    pub fn with_filter(mut self, placeholder: impl Into<String>) -> Self {
        let mut input = TextInput::new(placeholder);
        if !self.filter_query.is_empty() {
            input.set_text(self.filter_query.clone());
        }
        self.filter_input = Some(Box::new(input));
        self
    }

    /// Grid title used by host shells as a stable state key.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Current committed filter query.
    pub fn filter_query(&self) -> &str {
        &self.filter_query
    }

    /// Set the filter text programmatically without dispatching item actions.
    pub fn set_filter_query(&mut self, query: impl Into<String>) {
        self.filter_query = query.into();
        if let Some(input) = &mut self.filter_input {
            input.set_text(self.filter_query.clone());
        }
        self.normalize_after_filter_change();
    }

    /// Snapshot local browser interaction state.
    pub fn state(&self) -> AssetGridState {
        AssetGridState {
            filter_query: self.filter_query.clone(),
            selected_item_id: self
                .selected
                .and_then(|index| self.items.get(index))
                .map(|item| item.id.clone()),
            selected_index: self.selected,
        }
    }

    /// Restore local browser state after replacing the backing model.
    pub fn restore_state(&mut self, state: &AssetGridState) {
        self.set_filter_query(state.filter_query.clone());
        let selected = state
            .selected_item_id
            .as_ref()
            .and_then(|id| {
                self.items
                    .iter()
                    .position(|item| item.id == *id && self.item_matches_filter(item))
            })
            .or(state.selected_index);
        self.set_selected(selected);
    }

    /// Dispatch a dynamic action when selection changes.
    pub fn on_select(mut self, action: impl Fn(usize, &AssetGridItem) -> Action + 'static) -> Self {
        self.on_select = Some(Box::new(action));
        self
    }

    /// Dispatch a dynamic action when the current card is activated.
    pub fn on_activate(
        mut self,
        action: impl Fn(usize, &AssetGridItem) -> Action + 'static,
    ) -> Self {
        self.on_activate = Some(Box::new(action));
        self
    }

    /// Dispatch a dynamic action when a drag payload is dropped on the grid.
    pub fn on_drop(
        mut self,
        action: impl Fn(&DragPayload, Point) -> Option<Action> + 'static,
    ) -> Self {
        self.on_drop = Some(Box::new(action));
        self
    }

    /// Dispatch a dynamic action when a drag payload is dropped on a card.
    pub fn on_item_drop(
        mut self,
        action: impl Fn(&DragPayload, usize, &AssetGridItem) -> Option<Action> + 'static,
    ) -> Self {
        self.on_item_drop = Some(Box::new(action));
        self
    }

    /// Set selected item without dispatching actions.
    pub fn set_selected(&mut self, index: Option<usize>) {
        self.selected =
            index.filter(|idx| self.is_enabled_index(*idx) && self.visible_indices.contains(idx));
    }

    /// Currently selected item index.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Number of columns computed during the last layout pass.
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Bounds of a visible card by model index after the last layout pass.
    pub fn card_rect_for_index(&self, index: usize) -> Option<Rect> {
        let visible_position = self.visible_position_for_index(index)?;
        Some(self.card_rect_at_visible_position(visible_position))
    }

    fn header_height(&self) -> f32 {
        let base = self.title_block_height();
        if self.filter_input.is_some() {
            base + FILTER_INPUT_HEIGHT + HEADER_GAP
        } else {
            base
        }
    }

    fn title_block_height(&self) -> f32 {
        if self.subtitle.is_empty() {
            42.0
        } else {
            60.0
        }
    }

    fn content_height_for_width(width: f32, item_count: usize) -> f32 {
        let viewport_width = (width - CONTENT_PADDING * 2.0).max(0.0);
        let columns = grid_columns_for_width(viewport_width);
        let rows = item_count.div_ceil(columns);
        CONTENT_PADDING * 2.0 + rows as f32 * CARD_HEIGHT + rows.saturating_sub(1) as f32 * CARD_GAP
    }

    fn content_height(&self) -> f32 {
        let rows = self.visible_indices.len().div_ceil(self.columns.max(1));
        CONTENT_PADDING * 2.0 + rows as f32 * CARD_HEIGHT + rows.saturating_sub(1) as f32 * CARD_GAP
    }

    fn is_enabled_index(&self, index: usize) -> bool {
        self.items.get(index).is_some_and(|item| !item.disabled)
    }

    fn item_matches_query(item: &AssetGridItem, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        item.title.to_lowercase().contains(query)
            || item.subtitle.to_lowercase().contains(query)
            || item.badge.as_ref().is_some_and(|badge| badge.to_lowercase().contains(query))
    }

    fn item_matches_filter(&self, item: &AssetGridItem) -> bool {
        Self::item_matches_query(item, &self.filter_query.trim().to_lowercase())
    }

    fn rebuild_visible_indices(&mut self) {
        let query = self.filter_query.trim().to_lowercase();
        self.visible_indices = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| Self::item_matches_query(item, &query).then_some(index))
            .collect();
    }

    fn normalize_after_filter_change(&mut self) {
        self.rebuild_visible_indices();
        self.selected = self
            .selected
            .filter(|index| self.is_enabled_index(*index) && self.visible_indices.contains(index));
        self.hovered = None;
        self.drag_candidate = None;
        self.last_click = None;
    }

    fn filter_input_rect(&self) -> Option<Rect> {
        self.filter_input.as_ref()?;
        let y = self.bounds.y + self.title_block_height() - 4.0;
        Some(Rect::new(
            self.bounds.x + HEADER_PADDING_X,
            y,
            (self.bounds.width - HEADER_PADDING_X * 2.0).max(0.0),
            FILTER_INPUT_HEIGHT,
        ))
    }

    fn sync_filter_query_from_input(&mut self) -> bool {
        let Some(input) = &self.filter_input else {
            return false;
        };
        let query = input.text().to_owned();
        if query == self.filter_query {
            return false;
        }
        self.filter_query = query;
        self.normalize_after_filter_change();
        true
    }

    fn route_filter_input_event(
        &mut self,
        event: &UiEvent,
        ctx: &mut EventContext,
    ) -> Option<EventResult> {
        let input = self.filter_input.as_mut()?;
        let result = input.event(event, ctx);
        if self.sync_filter_query_from_input() {
            ctx.request_repaint();
        }
        Some(result)
    }

    fn visible_position_for_index(&self, index: usize) -> Option<usize> {
        self.visible_indices.iter().position(|candidate| *candidate == index)
    }

    fn visible_enabled_indices(&self) -> Vec<usize> {
        self.visible_indices
            .iter()
            .copied()
            .filter(|index| self.is_enabled_index(*index))
            .collect()
    }

    fn first_enabled(&self) -> Option<usize> {
        self.visible_enabled_indices().first().copied()
    }

    fn last_enabled(&self) -> Option<usize> {
        self.visible_enabled_indices().last().copied()
    }

    fn move_selection(&mut self, direction: i32) -> Option<usize> {
        let visible = self.visible_enabled_indices();
        if visible.is_empty() {
            return None;
        }
        let Some(selected) = self.selected else {
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
            visible.get((position + direction as usize).min(visible.len())).copied()
        } else {
            position
                .checked_sub(direction.unsigned_abs() as usize)
                .and_then(|prev| visible.get(prev).copied())
        }
    }

    fn select_from_input(&mut self, index: usize, ctx: &mut EventContext) -> EventResult {
        if !self.is_enabled_index(index) {
            return EventResult::Handled;
        }
        if self.selected != Some(index) {
            self.selected = Some(index);
            self.dispatch_select(index, ctx);
            ctx.request_repaint();
        }
        EventResult::Handled
    }

    fn click_is_activation(&self, index: usize, position: Point, now: Instant) -> bool {
        let Some(last) = self.last_click else {
            return false;
        };
        if last.index != index || now.duration_since(last.time) > DOUBLE_CLICK_MAX_AGE {
            return false;
        }
        let dx = position.x - last.position.x;
        let dy = position.y - last.position.y;
        dx * dx + dy * dy <= DOUBLE_CLICK_MAX_DISTANCE * DOUBLE_CLICK_MAX_DISTANCE
    }

    fn select_or_activate_from_input(
        &mut self,
        index: usize,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        if !self.is_enabled_index(index) {
            self.last_click = None;
            return EventResult::Handled;
        }
        let now = Instant::now();
        let activate = self.click_is_activation(index, position, now);
        self.last_click = Some(AssetGridClick { index, position, time: now });
        if self.selected != Some(index) {
            self.selected = Some(index);
            self.dispatch_select(index, ctx);
            ctx.request_repaint();
        }
        if activate {
            self.dispatch_activate(index, ctx);
        }
        EventResult::Handled
    }

    fn begin_drag_candidate_if_needed(
        &mut self,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        let Some(candidate) = self.drag_candidate.clone() else {
            return EventResult::Ignored;
        };
        if !self.is_enabled_index(candidate.index) {
            self.drag_candidate = None;
            ctx.release_pointer_capture(self.id);
            return EventResult::Handled;
        }
        let dx = position.x - candidate.start.x;
        let dy = position.y - candidate.start.y;
        if dx * dx + dy * dy < DRAG_START_DISTANCE * DRAG_START_DISTANCE {
            return EventResult::Ignored;
        }
        self.drag_candidate = None;
        ctx.begin_drag(candidate.payload);
        ctx.request_repaint();
        EventResult::Handled
    }

    fn activate_selected(&self, ctx: &mut EventContext) -> EventResult {
        let Some(index) = self.selected.filter(|idx| self.is_enabled_index(*idx)) else {
            return EventResult::Ignored;
        };
        self.dispatch_activate(index, ctx);
        EventResult::Handled
    }

    fn dispatch_select(&self, index: usize, ctx: &mut EventContext) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        if let Some(action) = item.select_action.clone() {
            (ctx.dispatch)(action);
        }
        if let Some(factory) = &self.on_select {
            (ctx.dispatch)(factory(index, item));
        }
    }

    fn dispatch_activate(&self, index: usize, ctx: &mut EventContext) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        if let Some(action) = item.activate_action.clone() {
            (ctx.dispatch)(action);
        }
        if let Some(factory) = &self.on_activate {
            (ctx.dispatch)(factory(index, item));
        }
    }

    fn drop_payload(&self, payload: &DragPayload, position: Point, ctx: &mut EventContext) {
        if let Some(index) = self.index_at(position) {
            if let (Some(factory), Some(item)) = (&self.on_item_drop, self.items.get(index)) {
                if let Some(action) = factory(payload, index, item) {
                    (ctx.dispatch)(action);
                    return;
                }
            }
        }
        if let Some(action) = self.on_drop.as_ref().and_then(|factory| factory(payload, position)) {
            (ctx.dispatch)(action);
        }
    }

    fn open_context_menu(&mut self, position: Point, items: Vec<MenuItem>, ctx: &mut EventContext) {
        let mut menu = ContextMenu::new(position, items);
        menu.layout(self.bounds);
        self.context_menu = Some(menu);
        ctx.request_repaint();
    }

    fn card_rect_at_visible_position(&self, visible_position: usize) -> Rect {
        let columns = self.columns.max(1);
        let col = visible_position % columns;
        let row = visible_position / columns;
        Rect::new(
            self.viewport.x + CONTENT_PADDING + col as f32 * (self.card_width + CARD_GAP),
            self.viewport.y + CONTENT_PADDING + row as f32 * (CARD_HEIGHT + CARD_GAP),
            self.card_width,
            CARD_HEIGHT,
        )
    }

    fn index_at(&self, point: Point) -> Option<usize> {
        if !self.viewport.contains(point) {
            return None;
        }
        self.visible_indices
            .iter()
            .copied()
            .enumerate()
            .find_map(|(visible_position, index)| {
                self.card_rect_at_visible_position(visible_position)
                    .contains(point)
                    .then_some(index)
            })
    }

    fn paint_empty_state(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let center = self.viewport.center();
        let box_rect = Rect::new(center.x - 86.0, center.y - 30.0, 172.0, 60.0);
        ctx.encoder.draw_rect(box_rect, colors.muted, ctx.theme.spacing.radius_md);
        ctx.push_clip(box_rect.inset(10.0, 6.0));
        ctx.encoder.draw_text(
            if self.items.is_empty() {
                "No assets"
            } else {
                "No matching assets"
            },
            ctx.theme.typography.body.font_size,
            snap_point(Point::new(box_rect.x + 14.0, box_rect.y + 11.0)),
            colors.foreground,
        );
        ctx.encoder.draw_text(
            if self.items.is_empty() {
                "Import media to begin"
            } else {
                "Adjust the search query"
            },
            ctx.theme.typography.small.font_size,
            snap_point(Point::new(box_rect.x + 14.0, box_rect.y + 34.0)),
            colors.muted_foreground,
        );
        ctx.pop_clip();
    }

    fn paint_card(&self, ctx: &mut PaintContext, index: usize, rect: Rect) {
        let item = &self.items[index];
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let selected = self.selected == Some(index);
        let hovered = self.hovered == Some(index) && !item.disabled;
        let base_fill = if selected {
            colors.accent
        } else if hovered {
            colors.muted
        } else {
            colors.card
        };
        if selected && (self.focus_visible || self.focused) {
            paint_focus_ring(ctx, rect, CARD_RADIUS);
        }
        ctx.encoder.draw_rect(rect, base_fill, CARD_RADIUS);

        let preview = Rect::new(
            rect.x + 6.0,
            rect.y + 6.0,
            (rect.width - 12.0).max(0.0),
            THUMBNAIL_HEIGHT,
        );
        let accent = if item.disabled {
            colors.muted_foreground
        } else {
            item.accent
        };
        ctx.encoder.draw_rect(
            preview.inset(-1.0, -1.0),
            soft_border(colors.border),
            spacing.radius_sm + 1.0,
        );
        ctx.encoder.draw_rect(
            preview,
            mix_color(colors.card, colors.background, 0.42),
            spacing.radius_sm,
        );

        let text_color = if item.disabled {
            colors.muted_foreground
        } else if selected {
            colors.accent_foreground
        } else {
            colors.foreground
        };
        if let Some(thumbnail) = &item.thumbnail {
            ctx.push_clip(preview);
            ctx.encoder.draw_raster_image(
                &thumbnail.key,
                preview,
                thumbnail.width,
                thumbnail.height,
                Arc::clone(&thumbnail.rgba),
                if item.disabled {
                    colors.muted_foreground
                } else {
                    Color::WHITE
                },
            );
            ctx.pop_clip();
        } else {
            let preview_top = mix_color(accent, colors.card, 0.22);
            let preview_bottom = mix_color(accent, colors.background, 0.58);
            ctx.encoder.draw_gradient_rect(
                preview,
                [preview_top, preview_top, preview_bottom, preview_bottom],
                spacing.radius_sm,
            );
            ctx.encoder.draw_rect(
                Rect::new(
                    preview.x,
                    preview.y + preview.height - 2.0,
                    preview.width,
                    2.0,
                ),
                color_with_alpha(accent, 0.72),
                0.0,
            );
        }
        if item.thumbnail.is_none() {
            if let Some(icon) = &item.icon {
                let icon_rect = Rect::new(
                    preview.x + (preview.width - ICON_SIZE) * 0.5,
                    preview.y + (preview.height - ICON_SIZE) * 0.5,
                    ICON_SIZE,
                    ICON_SIZE,
                );
                icon.paint(ctx, icon_rect, text_color);
            }
            match item.thumbnail_status {
                AssetGridThumbnailStatus::None => {}
                AssetGridThumbnailStatus::Loading => {
                    paint_thumbnail_loading(ctx, preview, text_color);
                }
                AssetGridThumbnailStatus::Failed => {
                    paint_thumbnail_failed(ctx, preview, colors.muted_foreground);
                }
            }
        }
        if let Some(badge) = &item.badge {
            let badge_rect = Rect::new(
                preview.x + preview.width - 44.0,
                preview.y + 6.0,
                36.0,
                18.0,
            );
            ctx.encoder
                .draw_rect(badge_rect, color_with_alpha(colors.background, 0.56), 5.0);
            ctx.push_clip(badge_rect.inset(4.0, 1.0));
            ctx.encoder.draw_text(
                badge,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(badge_rect.x + 6.0, badge_rect.y + 3.0)),
                text_color,
            );
            ctx.pop_clip();
        }

        let text_clip = Rect::new(
            rect.x + 8.0,
            rect.y + 78.0,
            (rect.width - 16.0).max(0.0),
            34.0,
        );
        ctx.push_clip(text_clip);
        ctx.encoder.draw_text_box(
            &item.title,
            ctx.theme.typography.small.font_size,
            snap_point(Point::new(text_clip.x, text_clip.y)),
            text_clip.width,
            text_color,
        );
        if !item.subtitle.is_empty() {
            ctx.encoder.draw_text_box(
                &item.subtitle,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(text_clip.x, text_clip.y + 17.0)),
                text_clip.width,
                colors.muted_foreground,
            );
        }
        ctx.pop_clip();
    }
}

impl Widget for AssetGrid {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let width = if constraint.max.width.is_finite() {
            constraint.max.width.max(constraint.min.width)
        } else {
            CARD_TARGET_WIDTH * 2.0 + CONTENT_PADDING * 2.0
        };
        constraint.constrain(Size::new(
            width,
            self.header_height()
                + Self::content_height_for_width(width, self.visible_indices.len()),
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if let Some(rect) = self.filter_input_rect() {
            if let Some(input) = &mut self.filter_input {
                input.layout(rect);
            }
        }
        if let Some(menu) = &mut self.context_menu {
            menu.layout(bounds);
        }
        let header = self.header_height();
        let viewport_width = (bounds.width - CONTENT_PADDING * 2.0).max(0.0);
        self.columns = grid_columns_for_width(viewport_width);
        let total_gap = CARD_GAP * self.columns.saturating_sub(1) as f32;
        self.card_width = ((viewport_width - total_gap) / self.columns as f32)
            .clamp(CARD_MIN_WIDTH, CARD_MAX_WIDTH);
        self.viewport = Rect::new(
            bounds.x,
            bounds.y + header,
            bounds.width.max(0.0),
            self.content_height().max((bounds.height - header).max(0.0)),
        );
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let Some(menu) = &mut self.context_menu {
            if menu.is_visible() {
                let result = menu.event(event, ctx);
                if !menu.is_visible() {
                    self.context_menu = None;
                }
                if result == EventResult::Handled {
                    return EventResult::Handled;
                }
            } else {
                self.context_menu = None;
            }
        }

        match event {
            UiEvent::MouseDown { position, button: MouseButton::Right, .. }
                if self.bounds.contains(*position) =>
            {
                self.focus_visible = false;
                if let Some(index) = self.index_at(*position) {
                    if self.is_enabled_index(index) {
                        let items = self
                            .items
                            .get(index)
                            .map(|item| item.context_menu_items.clone())
                            .unwrap_or_default();
                        if !items.is_empty() {
                            if self.selected != Some(index) {
                                self.selected = Some(index);
                                self.dispatch_select(index, ctx);
                            }
                            self.open_context_menu(*position, items, ctx);
                            return EventResult::Handled;
                        }
                    }
                }
                if !self.context_menu_items.is_empty() {
                    self.open_context_menu(*position, self.context_menu_items.clone(), ctx);
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if self.filter_input.as_ref().is_some_and(|input| input.hit_test(*position)) {
                    return self
                        .route_filter_input_event(event, ctx)
                        .unwrap_or(EventResult::Ignored);
                }
                if self.bounds.contains(*position) {
                    self.focus_visible = false;
                    if let Some(index) = self.index_at(*position) {
                        let result = self.select_or_activate_from_input(index, *position, ctx);
                        if let Some(payload) =
                            self.items.get(index).and_then(|item| item.drag_payload.clone())
                        {
                            self.drag_candidate =
                                Some(AssetGridDragCandidate { index, start: *position, payload });
                            ctx.request_pointer_capture(self.id);
                        }
                        return result;
                    }
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.drag_candidate.is_some() => {
                self.drag_candidate = None;
                ctx.release_pointer_capture(self.id);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::MouseMove { position, .. } => {
                if self.begin_drag_candidate_if_needed(*position, ctx) == EventResult::Handled {
                    return EventResult::Handled;
                }
                let hover = self.index_at(*position);
                if hover != self.hovered {
                    self.hovered = hover;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
            }
            UiEvent::DragEnter { position, .. } | UiEvent::DragOver { position, .. }
                if (self.on_drop.is_some() || self.on_item_drop.is_some())
                    && self.bounds.contains(*position) =>
            {
                if !self.drop_hovered {
                    self.drop_hovered = true;
                    ctx.request_repaint();
                }
                return EventResult::Handled;
            }
            UiEvent::Drop { payload, position }
                if (self.on_drop.is_some() || self.on_item_drop.is_some())
                    && self.bounds.contains(*position) =>
            {
                self.drop_hovered = false;
                self.drop_payload(payload, *position, ctx);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::DragLeave if self.on_drop.is_some() || self.on_item_drop.is_some() => {
                if self.drop_hovered {
                    self.drop_hovered = false;
                    ctx.request_repaint();
                }
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
                self.drag_candidate = None;
                self.drop_hovered = false;
                self.last_click = None;
                self.context_menu = None;
                if let Some(input) = &mut self.filter_input {
                    let _ = input.event(event, ctx);
                }
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Right, .. } if self.focused => {
                if let Some(index) = self.move_selection(1) {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Left, .. } if self.focused => {
                if let Some(index) = self.move_selection(-1) {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Down, .. } if self.focused => {
                if let Some(index) = self.move_selection(self.columns as i32) {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Up, .. } if self.focused => {
                if let Some(index) = self.move_selection(-(self.columns as i32)) {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Home, .. } if self.focused => {
                if let Some(index) = self.first_enabled() {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::End, .. } if self.focused => {
                if let Some(index) = self.last_enabled() {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, .. } if self.focused => {
                return self.activate_selected(ctx);
            }
            _ => {}
        }
        EventResult::Ignored
    }

    fn after_child_event(&mut self, _event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.sync_filter_query_from_input() {
            ctx.request_repaint();
            EventResult::Handled
        } else {
            EventResult::Ignored
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        ctx.encoder.draw_rect(self.bounds, colors.card, 0.0);

        ctx.encoder.draw_text(
            &self.title,
            ctx.theme.typography.body.font_size,
            snap_point(Point::new(
                self.bounds.x + HEADER_PADDING_X,
                self.bounds.y + 12.0,
            )),
            colors.foreground,
        );
        if !self.subtitle.is_empty() {
            ctx.encoder.draw_text_box(
                &self.subtitle,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(
                    self.bounds.x + HEADER_PADDING_X,
                    self.bounds.y + 34.0,
                )),
                (self.bounds.width - HEADER_PADDING_X * 2.0).max(1.0),
                colors.muted_foreground,
            );
        }
        if let Some(input) = &self.filter_input {
            input.paint(ctx);
        }

        ctx.push_clip(self.viewport);
        if self.visible_indices.is_empty() {
            self.paint_empty_state(ctx);
        } else {
            for (visible_position, index) in self.visible_indices.iter().copied().enumerate() {
                self.paint_card(
                    ctx,
                    index,
                    self.card_rect_at_visible_position(visible_position),
                );
            }
        }
        ctx.pop_clip();

        if self.drop_hovered {
            ctx.encoder.draw_rect(
                self.bounds.inset(3.0, 3.0),
                color_with_alpha(colors.ring, 0.22),
                ctx.theme.spacing.radius_md,
            );
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if let Some(menu) = &self.context_menu {
            menu.paint_overlay(ctx);
        }
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.context_menu.as_ref().is_some_and(|menu| menu.overlay_hit_test(point))
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        usize::from(self.filter_input.is_some())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => self.filter_input.as_ref().map(|input| input.as_ref() as &dyn Widget),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => self.filter_input.as_mut().map(|input| input.as_mut() as &mut dyn Widget),
            _ => None,
        }
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn can_focus(&self) -> bool {
        true
    }
}

fn grid_columns_for_width(width: f32) -> usize {
    if width <= CARD_MIN_WIDTH {
        return 1;
    }
    let columns = ((width + CARD_GAP) / (CARD_TARGET_WIDTH + CARD_GAP)).floor() as usize;
    columns.max(1)
}

fn paint_thumbnail_loading(ctx: &mut PaintContext, preview: Rect, color: Color) {
    let dot_size = 4.0;
    let gap = 5.0;
    let total_width = dot_size * 3.0 + gap * 2.0;
    let y = preview.y + preview.height - 13.0;
    let start_x = preview.x + (preview.width - total_width) * 0.5;
    for index in 0..3 {
        let alpha = 0.30 + index as f32 * 0.18;
        ctx.encoder.draw_rect(
            Rect::new(
                start_x + index as f32 * (dot_size + gap),
                y,
                dot_size,
                dot_size,
            ),
            color_with_alpha(color, alpha),
            dot_size * 0.5,
        );
    }
}

fn paint_thumbnail_failed(ctx: &mut PaintContext, preview: Rect, color: Color) {
    let chip = Rect::new(
        preview.x + 8.0,
        preview.y + preview.height - 24.0,
        22.0,
        16.0,
    );
    ctx.encoder.draw_rect(chip, color_with_alpha(color, 0.18), 5.0);
    let center = chip.center();
    let triangle = [
        Point::new(center.x, chip.y + 4.0),
        Point::new(chip.x + 6.0, chip.y + chip.height - 4.0),
        Point::new(chip.x + chip.width - 6.0, chip.y + chip.height - 4.0),
    ];
    ctx.encoder.draw_triangles(&triangle, color_with_alpha(color, 0.70));
    ctx.encoder.draw_rect(
        Rect::new(center.x - 0.75, chip.y + 7.0, 1.5, 4.5),
        color_with_alpha(Color::BLACK, 0.78),
        0.75,
    );
    ctx.encoder.draw_rect(
        Rect::new(center.x - 0.75, chip.y + 12.4, 1.5, 1.5),
        color_with_alpha(Color::BLACK, 0.78),
        0.75,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::types::AssetId;
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventContext, EventRequests};
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        clips: Vec<Rect>,
        clip_pops: usize,
        raster_images: Vec<(String, Rect, u32, u32)>,
        texts: Vec<String>,
        triangles: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}

        fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
            self.triangles += vertices.len() / 3;
        }

        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.to_owned());
        }

        fn draw_raster_image(
            &mut self,
            key: &str,
            bounds: Rect,
            width: u32,
            height: u32,
            _rgba: Arc<[u8]>,
            _tint: Color,
        ) {
            self.raster_images.push((key.to_owned(), bounds, width, height));
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn item(id: &str, title: &str) -> AssetGridItem {
        AssetGridItem::new(id, title, Color::from_hex(0x6688CC))
    }

    fn event_ctx<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
        requests: &'a mut EventRequests,
        dispatch: &'a dyn Fn(Action),
    ) -> EventContext<'a> {
        EventContext {
            focus,
            shortcut,
            tooltip,
            dispatch,
            platform: &NoopPlatformService,
            requests,
        }
    }

    #[test]
    fn grid_layout_wraps_cards_without_using_scrollbar_gutter() {
        let mut grid = AssetGrid::new(
            "Assets",
            vec![item("a", "A"), item("b", "B"), item("c", "C")],
        );
        let measured =
            grid.measure(LayoutConstraint { min: Size::ZERO, max: Size::new(360.0, f32::MAX) });

        grid.layout(Rect::new(0.0, 0.0, 360.0, 220.0));

        assert_eq!(measured.width, 360.0);
        assert_eq!(grid.columns(), 2);
        let first = grid.card_rect_for_index(0).expect("first card");
        let second = grid.card_rect_for_index(1).expect("second card");
        assert!(second.x > first.x + first.width);
        assert!(second.x + second.width <= 360.0);
    }

    #[test]
    fn thumbnail_model_rejects_invalid_rgba_payloads() {
        assert!(RasterImage::new("asset:bad", 2, 2, vec![255; 15]).is_none());
        assert!(RasterImage::new("asset:empty", 0, 2, Vec::<u8>::new()).is_none());
    }

    #[test]
    fn thumbnail_status_builders_clear_ready_thumbnail_payloads() {
        let thumbnail =
            RasterImage::new("asset-thumb:clip-a", 2, 2, vec![255; 16]).expect("valid thumbnail");

        let loading = item("clip-a", "Clip A")
            .with_thumbnail(thumbnail.clone())
            .with_thumbnail_loading();
        let failed = item("clip-b", "Clip B").with_thumbnail(thumbnail).with_thumbnail_failed();

        assert!(loading.thumbnail.is_none());
        assert_eq!(loading.thumbnail_status, AssetGridThumbnailStatus::Loading);
        assert!(failed.thumbnail.is_none());
        assert_eq!(failed.thumbnail_status, AssetGridThumbnailStatus::Failed);
    }

    #[test]
    fn paint_card_draws_thumbnail_inside_preview_clip() {
        let thumbnail =
            RasterImage::new("asset-thumb:clip-a", 2, 2, vec![255; 16]).expect("valid thumbnail");
        let grid = AssetGrid::new(
            "Assets",
            vec![item("clip-a", "Clip A").with_thumbnail(thumbnail)],
        );
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 320.0, 240.0),
        };
        let card = Rect::new(20.0, 30.0, 158.0, 118.0);
        let preview = Rect::new(
            card.x + 6.0,
            card.y + 6.0,
            card.width - 12.0,
            THUMBNAIL_HEIGHT,
        );

        grid.paint_card(&mut ctx, 0, card);

        assert_eq!(
            encoder.raster_images,
            vec![("asset-thumb:clip-a".to_owned(), preview, 2, 2)]
        );
        assert!(
            encoder.clips.contains(&preview),
            "asset thumbnails must be clipped to the card preview region"
        );
        assert_eq!(encoder.clip_pops, encoder.clips.len());
    }

    #[test]
    fn paint_card_without_thumbnail_keeps_vector_placeholder_path() {
        let grid = AssetGrid::new("Assets", vec![item("clip-a", "Clip A")]);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 320.0, 240.0),
        };

        grid.paint_card(&mut ctx, 0, Rect::new(20.0, 30.0, 158.0, 118.0));

        assert!(encoder.raster_images.is_empty());
        assert!(encoder.rects.len() >= 3);
    }

    #[test]
    fn paint_card_loading_thumbnail_uses_shape_marker_without_raster_image() {
        let grid = AssetGrid::new(
            "Assets",
            vec![item("clip-a", "Clip A").with_thumbnail_loading()],
        );
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 320.0, 240.0),
        };

        grid.paint_card(&mut ctx, 0, Rect::new(20.0, 30.0, 158.0, 118.0));

        assert!(encoder.raster_images.is_empty());
        assert_eq!(encoder.triangles, 0);
        assert!(encoder.rects.len() >= 6);
    }

    #[test]
    fn paint_card_failed_thumbnail_uses_vector_warning_marker() {
        let grid = AssetGrid::new(
            "Assets",
            vec![item("clip-a", "Clip A").with_thumbnail_failed()],
        );
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 320.0, 240.0),
        };

        grid.paint_card(&mut ctx, 0, Rect::new(20.0, 30.0, 158.0, 118.0));

        assert!(encoder.raster_images.is_empty());
        assert_eq!(encoder.triangles, 1);
    }

    #[test]
    fn filter_state_restores_selection_by_stable_id() {
        let mut grid = AssetGrid::new(
            "Assets",
            vec![
                item("clip-a", "Camera A"),
                item("music", "Music Bed"),
                item("clip-b", "Camera B"),
            ],
        )
        .with_filter("Search assets");
        grid.layout(Rect::new(0.0, 0.0, 360.0, 220.0));
        grid.set_filter_query("camera");
        grid.set_selected(Some(2));
        let state = grid.state();

        let mut rebuilt = AssetGrid::new(
            "Assets",
            vec![
                item("clip-b", "Camera B"),
                item("music", "Music Bed"),
                item("clip-a", "Camera A"),
            ],
        )
        .with_filter("Search assets");
        rebuilt.layout(Rect::new(0.0, 0.0, 360.0, 220.0));
        rebuilt.restore_state(&state);

        assert_eq!(rebuilt.filter_query(), "camera");
        assert_eq!(rebuilt.selected_index(), Some(0));
    }

    #[test]
    fn file_drop_dispatches_import_action() {
        let mut grid = AssetGrid::new("Assets", vec![item("drop", "Drop")]).on_drop(
            |payload, _| match payload {
                DragPayload::File(paths) if !paths.is_empty() => {
                    Some(Action::ImportMedia(paths.clone()))
                }
                _ => None,
            },
        );
        grid.layout(Rect::new(0.0, 0.0, 320.0, 180.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let path = PathBuf::from("E:/media/clip.mov");

        let result = grid.event(
            &UiEvent::Drop {
                payload: DragPayload::File(vec![path.clone()]),
                position: Point::new(24.0, 76.0),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::ImportMedia(vec![path])]
        );
    }

    #[test]
    fn item_drop_dispatches_item_action_before_grid_action() {
        let asset_id = AssetId::new();
        let mut grid = AssetGrid::new(
            "Assets",
            vec![item("folder:a", "Folder A"), item("folder:b", "Folder B")],
        )
        .on_drop(|_, _| Some(Action::DeselectAll))
        .on_item_drop(|payload, _index, item| {
            if item.id == "folder:a" && matches!(payload, DragPayload::Asset(_)) {
                Some(Action::SelectAll)
            } else {
                None
            }
        });
        grid.layout(Rect::new(0.0, 0.0, 420.0, 260.0));
        let first = grid.card_rect_for_index(0).expect("first card");
        let second = grid.card_rect_for_index(1).expect("second card");
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            grid.event(
                &UiEvent::Drop {
                    payload: DragPayload::Asset(asset_id),
                    position: first.center(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            grid.event(
                &UiEvent::Drop {
                    payload: DragPayload::Asset(asset_id),
                    position: second.center(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::SelectAll, Action::DeselectAll]
        );
    }

    #[test]
    fn right_click_context_menu_dispatches_actions_as_overlay() {
        let import_action = Action::ImportMedia(Vec::new());
        let mut grid =
            AssetGrid::new("Assets", vec![item("asset", "Asset")]).with_context_menu(vec![
                MenuItem::new("Import media", import_action.clone()),
                MenuItem::separator(),
                MenuItem::new("New folder", Action::DeselectAll),
            ]);
        grid.layout(Rect::new(0.0, 0.0, 320.0, 220.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            grid.event(
                &UiEvent::MouseDown {
                    position: Point::new(32.0, 72.0),
                    button: MouseButton::Right,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(grid.overlay_hit_test(Point::new(900.0, 900.0)));
        assert_eq!(
            grid.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[import_action]);
        assert!(!grid.overlay_hit_test(Point::new(900.0, 900.0)));
    }

    #[test]
    fn right_click_card_context_menu_overrides_grid_menu_and_selects_card() {
        let card_action = Action::ImportMedia(vec![PathBuf::from("E:/media/card.mov")]);
        let grid_action = Action::DeselectAll;
        let mut grid = AssetGrid::new(
            "Assets",
            vec![
                item("asset-a", "Asset A")
                    .with_context_menu(vec![MenuItem::new("Delete asset", card_action.clone())]),
                item("asset-b", "Asset B"),
            ],
        )
        .with_context_menu(vec![MenuItem::new("New folder", grid_action)]);
        grid.layout(Rect::new(0.0, 0.0, 420.0, 260.0));
        let card = grid.card_rect_for_index(0).expect("card");
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            grid.event(
                &UiEvent::MouseDown {
                    position: card.center(),
                    button: MouseButton::Right,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(grid.selected_index(), Some(0));
        assert_eq!(
            grid.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[card_action]);
    }

    #[test]
    fn pointer_motion_after_press_starts_asset_drag() {
        let asset_id = AssetId::new();
        let mut grid = AssetGrid::new(
            "Assets",
            vec![item("asset", "Asset").with_drag_payload(DragPayload::Asset(asset_id))],
        );
        grid.layout(Rect::new(0.0, 0.0, 320.0, 220.0));
        let card = grid.card_rect_for_index(0).expect("card");
        let start = card.center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            grid.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            grid.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + 12.0, start.y),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(
            requests.drag,
            Some(mondrian_ui_core::widget::DragRequest::Begin(
                DragPayload::Asset(asset_id)
            ))
        );
    }
}
