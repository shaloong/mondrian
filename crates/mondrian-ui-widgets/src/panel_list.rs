//! Reusable list surface for editor panels.
//!
//! This widget is intended for assets, effects, presets, and other panel
//! browsers. It owns selection and scrolling locally, while exposing typed
//! action adapters so application state can remain outside the UI crate.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::time::{Duration, Instant};

use crate::paint::color_with_alpha;
use crate::text_metrics::measure_single_line;
use crate::vector_icon::VectorIcon;
use crate::TextInput;

const DOUBLE_CLICK_MAX_AGE: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_MAX_DISTANCE: f32 = 5.0;
const DRAG_START_DISTANCE: f32 = 6.0;
const ROW_ICON_GAP: f32 = 8.0;
const ROW_ICON_SIZE: f32 = 16.0;
const FILTER_INPUT_HEIGHT: f32 = 28.0;
const FILTER_INPUT_GAP: f32 = 10.0;

/// Dynamic action factory used when a panel-list item changes state.
pub type PanelListAction = dyn Fn(usize, &PanelListItem) -> Action;
/// Dynamic action factory used when a payload is dropped on the panel list.
pub type PanelListDropAction = dyn Fn(&DragPayload, Point) -> Option<Action>;

/// Local interaction state for a [`PanelList`].
///
/// Application adapters can persist this across model refreshes without making
/// the reusable widget depend on editor state.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelListState {
    /// Text currently committed in the optional filter input.
    pub filter_query: String,
    /// Selected item by stable row identity when available.
    pub selected_item_title: Option<String>,
    /// Selected item by model index as a fallback for duplicate titles.
    pub selected_index: Option<usize>,
    /// Vertical scroll offset in content pixels.
    pub scroll_y: f32,
}

/// Item rendered by [`PanelList`].
#[derive(Debug, Clone)]
pub struct PanelListItem {
    pub title: String,
    pub subtitle: String,
    pub badge: Option<PanelListBadge>,
    pub accent: Option<Color>,
    pub icon: Option<VectorIcon>,
    pub disabled: bool,
    pub select_action: Option<Action>,
    pub activate_action: Option<Action>,
    pub drag_payload: Option<DragPayload>,
}

/// Semantic visual tone for a [`PanelListBadge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelListBadgeTone {
    /// Neutral metadata, such as effect category or item count.
    Neutral,
    /// Accent metadata tied to the active theme primary/accent color.
    Accent,
    /// Positive/ready status.
    Success,
    /// Attention-needed status.
    Warning,
    /// Error or unavailable status.
    Error,
}

/// Compact right-aligned label painted in a [`PanelList`] row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelListBadge {
    pub label: String,
    pub tone: PanelListBadgeTone,
}

impl PanelListBadge {
    /// Create a neutral badge.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            tone: PanelListBadgeTone::Neutral,
        }
    }

    /// Create a badge with a semantic tone.
    pub fn with_tone(label: impl Into<String>, tone: PanelListBadgeTone) -> Self {
        Self { label: label.into(), tone }
    }
}

impl PanelListItem {
    /// Create a selectable panel-list item.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: String::new(),
            badge: None,
            accent: None,
            icon: None,
            disabled: false,
            select_action: None,
            activate_action: None,
            drag_payload: None,
        }
    }

    /// Set secondary text shown below the title.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Set a compact right-aligned status badge.
    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(PanelListBadge::new(badge));
        self
    }

    /// Set a compact right-aligned status badge with a semantic tone.
    pub fn with_badge_tone(mut self, badge: impl Into<String>, tone: PanelListBadgeTone) -> Self {
        self.badge = Some(PanelListBadge::with_tone(badge, tone));
        self
    }

    /// Set a left accent swatch.
    pub fn with_accent(mut self, accent: Color) -> Self {
        self.accent = Some(accent);
        self
    }

    /// Set a left-side vector icon painted before the text lane.
    pub fn with_icon(mut self, icon: VectorIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Mark the item as disabled.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Dispatch a static action when this item becomes selected.
    pub fn with_select_action(mut self, action: Action) -> Self {
        self.select_action = Some(action);
        self
    }

    /// Dispatch a static action when this item is activated.
    pub fn with_activate_action(mut self, action: Action) -> Self {
        self.activate_action = Some(action);
        self
    }

    /// Start an internal UI drag with this payload after pointer movement
    /// exceeds the drag threshold.
    pub fn with_drag_payload(mut self, payload: DragPayload) -> Self {
        self.drag_payload = Some(payload);
        self
    }
}

/// Scrollable, keyboard-navigable list for panel content.
pub struct PanelList {
    id: WidgetId,
    title: String,
    subtitle: String,
    items: Vec<PanelListItem>,
    filter_input: Option<Box<TextInput>>,
    filter_query: String,
    visible_indices: Vec<usize>,
    bounds: Rect,
    viewport: Rect,
    selected: Option<usize>,
    hovered: Option<usize>,
    scroll_y: f32,
    row_height: f32,
    scrollbar_hovered: bool,
    scrollbar_dragging: bool,
    drag_start_y: f32,
    drag_start_scroll_y: f32,
    drop_hovered: bool,
    drag_candidate: Option<PanelListDragCandidate>,
    last_click: Option<PanelListClick>,
    focused: bool,
    focus_visible: bool,
    on_select: Option<Box<PanelListAction>>,
    on_activate: Option<Box<PanelListAction>>,
    on_drop: Option<Box<PanelListDropAction>>,
}

#[derive(Debug, Clone, Copy)]
struct PanelListClick {
    index: usize,
    position: Point,
    time: Instant,
}

#[derive(Debug, Clone)]
struct PanelListDragCandidate {
    index: usize,
    start: Point,
    payload: DragPayload,
}

impl PanelList {
    /// Create a panel list with a title and rows.
    pub fn new(title: impl Into<String>, items: Vec<PanelListItem>) -> Self {
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
            scroll_y: 0.0,
            row_height: 50.0,
            scrollbar_hovered: false,
            scrollbar_dragging: false,
            drag_start_y: 0.0,
            drag_start_scroll_y: 0.0,
            drop_hovered: false,
            drag_candidate: None,
            last_click: None,
            focused: false,
            focus_visible: false,
            on_select: None,
            on_activate: None,
            on_drop: None,
        }
    }

    /// Set a small explanatory subtitle below the title.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Set the row height. Values below 36 px are clamped for readability.
    pub fn with_row_height(mut self, row_height: f32) -> Self {
        self.row_height = row_height.max(36.0);
        self
    }

    /// Panel title used by host shells as a stable local-state key.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Add a searchable filter field above the list rows.
    pub fn with_filter(mut self, placeholder: impl Into<String>) -> Self {
        let mut input = TextInput::new(placeholder);
        if !self.filter_query.is_empty() {
            input.set_text(self.filter_query.clone());
        }
        self.filter_input = Some(Box::new(input));
        self
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

    /// Snapshot local list interaction state for model refresh migration.
    pub fn state(&self) -> PanelListState {
        PanelListState {
            filter_query: self.filter_query.clone(),
            selected_item_title: self
                .selected
                .and_then(|index| self.items.get(index))
                .map(|item| item.title.clone()),
            selected_index: self.selected,
            scroll_y: self.scroll_y,
        }
    }

    /// Restore local list interaction state after replacing the backing model.
    pub fn restore_state(&mut self, state: &PanelListState) {
        self.set_filter_query(state.filter_query.clone());
        let selected = state
            .selected_item_title
            .as_ref()
            .and_then(|title| {
                self.items
                    .iter()
                    .position(|item| item.title == *title && self.item_matches_filter(item))
            })
            .or(state.selected_index);
        self.set_selected(selected);
        self.set_scroll_y(state.scroll_y);
    }

    /// Set the selected item if it is valid and enabled.
    pub fn with_selected(mut self, index: Option<usize>) -> Self {
        self.set_selected(index);
        self
    }

    /// Dispatch a dynamic action when selection changes.
    pub fn on_select(mut self, action: impl Fn(usize, &PanelListItem) -> Action + 'static) -> Self {
        self.on_select = Some(Box::new(action));
        self
    }

    /// Dispatch a dynamic action when the current item is activated.
    pub fn on_activate(
        mut self,
        action: impl Fn(usize, &PanelListItem) -> Action + 'static,
    ) -> Self {
        self.on_activate = Some(Box::new(action));
        self
    }

    /// Dispatch a dynamic action when a drag payload is dropped on the list.
    pub fn on_drop(
        mut self,
        action: impl Fn(&DragPayload, Point) -> Option<Action> + 'static,
    ) -> Self {
        self.on_drop = Some(Box::new(action));
        self
    }

    /// Replace all items and clamp selection/scroll state.
    pub fn set_items(&mut self, items: Vec<PanelListItem>) {
        self.items = items;
        self.rebuild_visible_indices();
        self.selected = self.selected.filter(|idx| {
            self.is_enabled_index(*idx) && self.item_matches_filter(&self.items[*idx])
        });
        self.hovered = None;
        self.drop_hovered = false;
        self.drag_candidate = None;
        self.last_click = None;
        self.clamp_scroll();
    }

    /// Currently selected item index.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Current vertical scroll offset in content pixels.
    pub fn scroll_offset_y(&self) -> f32 {
        self.scroll_y
    }

    /// Set selected item without dispatching actions.
    pub fn set_selected(&mut self, index: Option<usize>) {
        self.selected =
            index.filter(|idx| self.is_enabled_index(*idx) && self.visible_indices.contains(idx));
        self.ensure_selected_visible();
    }

    fn header_height(&self) -> f32 {
        let base = self.title_block_height();
        if self.filter_input.is_some() {
            base + FILTER_INPUT_HEIGHT + FILTER_INPUT_GAP
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

    fn content_height(&self) -> f32 {
        self.visible_indices.len() as f32 * self.row_height
    }

    fn max_scroll_y(&self) -> f32 {
        (self.content_height() - self.viewport.height).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y());
    }

    fn set_scroll_y(&mut self, scroll_y: f32) -> bool {
        let old = self.scroll_y;
        self.scroll_y = scroll_y.clamp(0.0, self.max_scroll_y());
        (self.scroll_y - old).abs() > 0.01
    }

    fn scrollbar_track_rect(&self) -> Option<Rect> {
        (self.max_scroll_y() > 0.0 && self.viewport.height > 0.0).then_some(Rect::new(
            self.viewport.x + self.viewport.width - 6.0,
            self.viewport.y + 2.0,
            4.0,
            (self.viewport.height - 4.0).max(0.0),
        ))
    }

    fn scrollbar_thumb_rect(&self) -> Option<Rect> {
        let track = self.scrollbar_track_rect()?;
        let content_height = self.content_height();
        if content_height <= 0.0 {
            return None;
        }
        let thumb_height = (self.viewport.height / content_height * track.height)
            .clamp(24.0, track.height.max(24.0));
        let travel = (track.height - thumb_height).max(0.0);
        let y = if self.max_scroll_y() <= 0.0 {
            track.y
        } else {
            track.y + (self.scroll_y / self.max_scroll_y()) * travel
        };
        Some(Rect::new(
            track.x,
            y,
            track.width,
            thumb_height.min(track.height),
        ))
    }

    fn scroll_y_for_thumb_delta(&self, delta_y: f32) -> f32 {
        let Some(track) = self.scrollbar_track_rect() else {
            return self.scroll_y;
        };
        let Some(thumb) = self.scrollbar_thumb_rect() else {
            return self.scroll_y;
        };
        let travel = (track.height - thumb.height).max(1.0);
        self.drag_start_scroll_y + delta_y / travel * self.max_scroll_y()
    }

    fn is_enabled_index(&self, index: usize) -> bool {
        self.items.get(index).is_some_and(|item| !item.disabled)
    }

    fn item_matches_query(item: &PanelListItem, query: &str) -> bool {
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

    fn item_matches_filter(&self, item: &PanelListItem) -> bool {
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

    fn index_at(&self, point: Point) -> Option<usize> {
        if !self.viewport.contains(point) {
            return None;
        }
        if self.scrollbar_track_rect().is_some_and(|track| track.contains(point)) {
            return None;
        }
        let rel_y = point.y - self.viewport.y + self.scroll_y;
        let visible_row = (rel_y / self.row_height).floor() as usize;
        self.visible_indices.get(visible_row).copied()
    }

    fn next_enabled_from(&self, start: usize, direction: i32) -> Option<usize> {
        let visible = self.visible_enabled_indices();
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

    fn first_enabled(&self) -> Option<usize> {
        self.next_enabled_from(0, 1)
    }

    fn last_enabled(&self) -> Option<usize> {
        self.items
            .len()
            .checked_sub(1)
            .and_then(|last| self.next_enabled_from(last, -1))
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
            visible.get(position + 1).copied()
        } else {
            position.checked_sub(1).and_then(|prev| visible.get(prev).copied())
        }
    }

    fn ensure_selected_visible(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        if self.viewport.height <= 0.0 {
            return;
        }
        let Some(visible_position) = self.visible_position_for_index(index) else {
            return;
        };
        let top = visible_position as f32 * self.row_height;
        let bottom = top + self.row_height;
        if top < self.scroll_y {
            self.scroll_y = top;
        } else if bottom > self.scroll_y + self.viewport.height {
            self.scroll_y = bottom - self.viewport.height;
        }
        self.clamp_scroll();
    }

    fn select_from_input(&mut self, index: usize, ctx: &mut EventContext) -> EventResult {
        if !self.is_enabled_index(index) {
            return EventResult::Handled;
        }
        if self.selected != Some(index) {
            self.selected = Some(index);
            self.ensure_selected_visible();
            self.dispatch_select(index, ctx);
            ctx.request_repaint();
        }
        EventResult::Handled
    }

    fn filter_input_rect(&self) -> Option<Rect> {
        self.filter_input.as_ref()?;
        let y = self.bounds.y + self.title_block_height() - 4.0;
        Some(Rect::new(
            self.bounds.x + 8.0,
            y,
            (self.bounds.width - 16.0).max(0.0),
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

    fn normalize_after_filter_change(&mut self) {
        self.rebuild_visible_indices();
        self.selected = self
            .selected
            .filter(|index| self.is_enabled_index(*index) && self.visible_indices.contains(index));
        self.hovered = None;
        self.drag_candidate = None;
        self.last_click = None;
        self.clamp_scroll();
        self.ensure_selected_visible();
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
        self.last_click = Some(PanelListClick { index, position, time: now });

        if self.selected != Some(index) {
            self.selected = Some(index);
            self.ensure_selected_visible();
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

    fn clear_selection_from_input(&mut self, ctx: &mut EventContext) -> EventResult {
        if self.selected.is_none() {
            return EventResult::Ignored;
        }
        self.selected = None;
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
        let Some(factory) = &self.on_drop else {
            return;
        };
        if let Some(action) = factory(payload, position) {
            (ctx.dispatch)(action);
        }
    }

    fn paint_row(&self, ctx: &mut PaintContext, index: usize, row: Rect) {
        let item = &self.items[index];
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let selected = self.selected == Some(index);
        let hovered = self.hovered == Some(index) && !item.disabled;

        let fill = if selected {
            colors.accent
        } else if hovered {
            colors.muted
        } else {
            colors.card
        };
        if selected {
            let mut ring = colors.ring;
            ring.a = 0.42;
            ctx.encoder.draw_rect(row.inset(-1.0, -1.0), ring, spacing.radius_sm + 1.0);
        }
        ctx.encoder.draw_rect(row, fill, spacing.radius_sm);

        let accent = item.accent.unwrap_or(colors.secondary);
        let swatch = Rect::new(row.x + 8.0, row.y + 13.0, 6.0, row.height - 26.0);
        ctx.encoder.draw_rect(swatch, accent, 3.0);

        let badge_width = item
            .badge
            .as_ref()
            .map(|badge| panel_list_badge_width(ctx, &badge.label, row.width))
            .unwrap_or(0.0);
        let badge_reserved = if item.badge.is_some() {
            badge_width + 14.0
        } else {
            8.0
        };
        let title_color = if item.disabled {
            colors.muted_foreground
        } else if selected {
            colors.accent_foreground
        } else {
            colors.foreground
        };
        let text_color = if item.disabled {
            colors.muted_foreground
        } else {
            title_color
        };
        let mut text_x = row.x + 22.0;

        ctx.push_clip(row.inset(4.0, 2.0));
        if let Some(icon) = &item.icon {
            let icon_size = ctx.theme.spacing.icon_size.clamp(1.0, ROW_ICON_SIZE);
            let icon_rect = Rect::new(
                text_x,
                row.y + (row.height - icon_size).max(0.0) * 0.5,
                icon_size,
                icon_size,
            );
            icon.paint(ctx, icon_rect, text_color);
            text_x += icon_size + ROW_ICON_GAP;
        }
        let text_width = (row.x + row.width - badge_reserved - text_x).max(24.0);
        ctx.encoder.draw_text_box(
            &item.title,
            ctx.theme.typography.body.font_size,
            snap_point(Point::new(text_x, row.y + 7.0)),
            text_width,
            text_color,
        );
        if !item.subtitle.is_empty() {
            ctx.encoder.draw_text_box(
                &item.subtitle,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(text_x, row.y + 28.0)),
                text_width,
                colors.muted_foreground,
            );
        }
        ctx.pop_clip();

        if let Some(badge) = &item.badge {
            let badge_rect = Rect::new(
                row.x + row.width - badge_width - 10.0,
                row.y + 13.0,
                badge_width,
                22.0,
            );
            let (fill, text) = panel_list_badge_colors(ctx, badge);
            ctx.encoder.draw_rect(badge_rect, fill, spacing.radius_sm);
            ctx.push_clip(badge_rect.inset(6.0, 1.0));
            ctx.encoder.draw_text(
                &badge.label,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(badge_rect.x + 8.0, badge_rect.y + 4.0)),
                text,
            );
            ctx.pop_clip();
        }
    }
}

fn panel_list_badge_width(ctx: &PaintContext, label: &str, row_width: f32) -> f32 {
    let text_width = measure_single_line(label, ctx.theme.typography.small.font_size).0;
    let max_width = (row_width * 0.34).clamp(32.0, 96.0);
    (text_width + 16.0).clamp(32.0, max_width)
}

fn panel_list_badge_colors(ctx: &PaintContext, badge: &PanelListBadge) -> (Color, Color) {
    let colors = &ctx.theme.colors;
    match badge.tone {
        PanelListBadgeTone::Neutral => (colors.secondary, colors.secondary_foreground),
        PanelListBadgeTone::Accent => (color_with_alpha(colors.primary, 0.22), colors.primary),
        PanelListBadgeTone::Success => (color_with_alpha(colors.success, 0.22), colors.success),
        PanelListBadgeTone::Warning => (color_with_alpha(colors.warning, 0.22), colors.warning),
        PanelListBadgeTone::Error => (color_with_alpha(colors.error, 0.22), colors.error),
    }
}

impl Widget for PanelList {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(
            240.0,
            self.header_height() + self.content_height(),
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let padding = 8.0;
        let header = self.header_height();
        if let Some(rect) = self.filter_input_rect() {
            if let Some(input) = &mut self.filter_input {
                input.layout(rect);
            }
        }
        self.viewport = Rect::new(
            bounds.x + padding,
            bounds.y + header,
            (bounds.width - padding * 2.0).max(0.0),
            (bounds.height - header - padding).max(0.0),
        );
        self.clamp_scroll();
        self.ensure_selected_visible();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if self.filter_input.as_ref().is_some_and(|input| input.hit_test(*position)) {
                    return self
                        .route_filter_input_event(event, ctx)
                        .unwrap_or(EventResult::Ignored);
                }
                if self.bounds.contains(*position) {
                    self.focus_visible = false;
                    if let Some(thumb) = self.scrollbar_thumb_rect() {
                        if thumb.contains(*position) {
                            self.scrollbar_dragging = true;
                            self.drag_start_y = position.y;
                            self.drag_start_scroll_y = self.scroll_y;
                            ctx.request_pointer_capture(self.id);
                            ctx.request_repaint();
                            return EventResult::Handled;
                        }
                    }
                    if let Some(track) = self.scrollbar_track_rect() {
                        if track.contains(*position) {
                            let page = self.viewport.height.max(self.row_height);
                            let thumb_y =
                                self.scrollbar_thumb_rect().map_or(track.y, |rect| rect.y);
                            let target = if position.y < thumb_y {
                                self.scroll_y - page
                            } else {
                                self.scroll_y + page
                            };
                            if self.set_scroll_y(target) {
                                ctx.request_repaint();
                            }
                            return EventResult::Handled;
                        }
                    }
                    if let Some(index) = self.index_at(*position) {
                        let result = self.select_or_activate_from_input(index, *position, ctx);
                        if let Some(payload) =
                            self.items.get(index).and_then(|item| item.drag_payload.clone())
                        {
                            self.drag_candidate =
                                Some(PanelListDragCandidate { index, start: *position, payload });
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
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.scrollbar_dragging => {
                self.scrollbar_dragging = false;
                ctx.release_pointer_capture(self.id);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::MouseMove { position, .. } => {
                if self.begin_drag_candidate_if_needed(*position, ctx) == EventResult::Handled {
                    return EventResult::Handled;
                }
                if self.scrollbar_dragging {
                    if self
                        .set_scroll_y(self.scroll_y_for_thumb_delta(position.y - self.drag_start_y))
                    {
                        ctx.request_repaint();
                    }
                    return EventResult::Handled;
                }
                let scrollbar_hovered =
                    self.scrollbar_thumb_rect().is_some_and(|thumb| thumb.contains(*position));
                let mut handled = false;
                if scrollbar_hovered != self.scrollbar_hovered {
                    self.scrollbar_hovered = scrollbar_hovered;
                    ctx.request_repaint();
                    handled = true;
                }
                let hover = self.index_at(*position);
                if hover != self.hovered {
                    self.hovered = hover;
                    ctx.request_repaint();
                    handled = true;
                }
                if handled {
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseWheel { delta, position, .. } => {
                if self.bounds.contains(*position) {
                    if self.set_scroll_y(self.scroll_y + *delta) {
                        ctx.request_repaint();
                    }
                    return EventResult::Handled;
                }
            }
            UiEvent::DragEnter { position, .. } | UiEvent::DragOver { position, .. }
                if self.on_drop.is_some() && self.bounds.contains(*position) =>
            {
                if !self.drop_hovered {
                    self.drop_hovered = true;
                    ctx.request_repaint();
                }
                return EventResult::Handled;
            }
            UiEvent::Drop { payload, position }
                if self.on_drop.is_some() && self.bounds.contains(*position) =>
            {
                self.drop_hovered = false;
                self.drop_payload(payload, *position, ctx);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::DragLeave if self.on_drop.is_some() => {
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
                if let Some(input) = &mut self.filter_input {
                    let _ = input.event(event, ctx);
                }
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Down, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
                if let Some(index) = self.move_selection(1) {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Up, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
                if let Some(index) = self.move_selection(-1) {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Home, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
                if let Some(index) = self.first_enabled() {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::End, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
                if let Some(index) = self.last_enabled() {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Escape, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
                return self.clear_selection_from_input(ctx);
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
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
        let spacing = &ctx.theme.spacing;
        ctx.encoder.draw_rect(self.bounds, colors.card, 0.0);

        let title_pos = snap_point(Point::new(self.bounds.x + 12.0, self.bounds.y + 12.0));
        ctx.encoder.draw_text(
            &self.title,
            ctx.theme.typography.body.font_size,
            title_pos,
            colors.foreground,
        );
        if !self.subtitle.is_empty() {
            ctx.encoder.draw_text_box(
                &self.subtitle,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(self.bounds.x + 12.0, self.bounds.y + 34.0)),
                (self.bounds.width - 24.0).max(0.0),
                colors.muted_foreground,
            );
        }
        if let Some(input) = &self.filter_input {
            input.paint(ctx);
        }

        let divider_y = self.viewport.y - 7.0;
        ctx.encoder.draw_line(
            Point::new(self.bounds.x, divider_y),
            Point::new(self.bounds.x + self.bounds.width, divider_y),
            1.0,
            colors.border,
        );

        ctx.push_clip(self.viewport);
        let visible = &self.visible_indices;
        let first = (self.scroll_y / self.row_height).floor().max(0.0) as usize;
        let last = ((self.scroll_y + self.viewport.height) / self.row_height).ceil() as usize + 1;
        for (visible_position, index) in
            visible.iter().copied().enumerate().take(last.min(visible.len())).skip(first)
        {
            let y = self.viewport.y + visible_position as f32 * self.row_height - self.scroll_y;
            let row = Rect::new(
                self.viewport.x,
                y + 2.0,
                self.viewport.width,
                self.row_height - 4.0,
            );
            self.paint_row(ctx, index, row);
        }
        if visible.is_empty() {
            ctx.encoder.draw_text_box(
                if self.filter_query.trim().is_empty() {
                    "No items"
                } else {
                    "No matches"
                },
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(self.viewport.x + 8.0, self.viewport.y + 8.0)),
                (self.viewport.width - 16.0).max(0.0),
                colors.muted_foreground,
            );
        }
        ctx.pop_clip();

        if let Some(thumb) = self.scrollbar_thumb_rect() {
            let mut thumb_color = colors.scrollbar_thumb;
            thumb_color.a = if self.scrollbar_hovered || self.scrollbar_dragging {
                0.88
            } else {
                0.64
            };
            ctx.encoder.draw_rect(thumb, thumb_color, thumb.width * 0.5);
        }

        if self.focus_visible {
            let mut ring = colors.ring;
            ring.a = 0.34;
            ctx.encoder
                .draw_rect(self.bounds.inset(-2.0, -2.0), ring, spacing.radius_sm + 2.0);
        }

        if self.drop_hovered {
            let mut fill = colors.accent;
            fill.a = 0.12;
            ctx.encoder.draw_rect(
                self.viewport.inset(-2.0, -2.0),
                fill,
                spacing.radius_sm + 1.0,
            );
            let mut ring = colors.ring;
            ring.a = 0.48;
            ctx.encoder
                .draw_rect(self.bounds.inset(-2.0, -2.0), ring, spacing.radius_sm + 2.0);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        true
    }

    fn accepts_text_input(&self) -> bool {
        self.filter_input.as_ref().is_some_and(|input| input.accepts_text_input())
    }

    fn child_count(&self) -> usize {
        usize::from(self.filter_input.is_some())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        if index == 0 {
            self.filter_input.as_deref().map(|input| input as &dyn Widget)
        } else {
            None
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        if index == 0 {
            self.filter_input.as_deref_mut().map(|input| input as &mut dyn Widget)
        } else {
            None
        }
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::path::PathBuf;

    use mondrian_core::types::AssetId;
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{
        DragRequest, DrawCommandEncoder, EventRequests, PointerCaptureRequest,
    };
    use mondrian_ui_theme::ThemePreset;

    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip};

    fn custom_action(name: &str) -> Action {
        match name {
            "select-a" => Action::SaveProject,
            "dynamic-0-A" => Action::CloseProject,
            "activate-c" => Action::Play,
            _ => Action::Pause,
        }
    }

    fn dispatching_ctx<'a>(
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

    #[derive(Default)]
    struct RecordingEncoder {
        rects: usize,
        rect_bounds: Vec<Rect>,
        rect_colors: Vec<Color>,
        lines: usize,
        triangles: usize,
        raster_images: usize,
        texts: Vec<String>,
        clips: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {
            self.clips += 1;
        }

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, color: Color, _corner_radius: f32) {
            self.rects += 1;
            self.rect_bounds.push(bounds);
            self.rect_colors.push(color);
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }

        fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
            self.triangles += vertices.len();
        }

        fn draw_raster_image(
            &mut self,
            _key: &str,
            _bounds: Rect,
            _width: u32,
            _height: u32,
            _rgba: std::sync::Arc<[u8]>,
            _tint: Color,
        ) {
            self.raster_images += 1;
        }

        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.into());
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: Color,
        ) {
            self.texts.push(text.into());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    fn sample_items() -> Vec<PanelListItem> {
        vec![
            PanelListItem::new("A").with_select_action(custom_action("select-a")),
            PanelListItem::new("B").disabled(true),
            PanelListItem::new("C").with_activate_action(custom_action("activate-c")),
        ]
    }

    fn drag_item(asset_id: AssetId) -> PanelListItem {
        PanelListItem::new("Asset").with_drag_payload(DragPayload::Asset(asset_id))
    }

    fn test_icon() -> VectorIcon {
        VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M6 12L18 12" fill="none" stroke="black"/></svg>"#,
        )
        .expect("svg icon")
    }

    #[test]
    fn click_selects_enabled_item_and_dispatches_once() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Assets", sample_items())
            .on_select(|index, item| custom_action(&format!("dynamic-{index}-{}", item.title)));
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = list.event(
            &UiEvent::MouseDown {
                position: Point::new(30.0, 74.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(list.selected_index(), Some(0));
        let actions = actions.borrow();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], custom_action("select-a"));
        assert_eq!(actions[1], custom_action("dynamic-0-A"));
        assert!(requests.repaint);
    }

    #[test]
    fn second_click_on_same_enabled_item_dispatches_activation() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items());
        list.layout(Rect::new(0.0, 0.0, 240.0, 220.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let click = UiEvent::MouseDown {
            position: Point::new(30.0, 174.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        };

        assert_eq!(list.event(&click, &mut ctx), EventResult::Handled);
        assert_eq!(list.selected_index(), Some(2));
        assert!(actions.borrow().is_empty());

        assert_eq!(list.event(&click, &mut ctx), EventResult::Handled);
        assert_eq!(actions.borrow().as_slice(), &[custom_action("activate-c")]);
    }

    #[test]
    fn dragging_enabled_item_requests_internal_drag_after_threshold() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let asset_id = AssetId::new();
        let mut list = PanelList::new("Assets", vec![drag_item(asset_id)]);
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        {
            let mut ctx = dispatching_ctx(
                &mut focus,
                &mut shortcut,
                &mut tooltip,
                &mut requests,
                &dispatch,
            );
            list.event(
                &UiEvent::MouseDown {
                    position: Point::new(30.0, 74.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
        }
        assert_eq!(
            requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(list.id()))
        );
        requests.pointer_capture = None;

        {
            let mut ctx = dispatching_ctx(
                &mut focus,
                &mut shortcut,
                &mut tooltip,
                &mut requests,
                &dispatch,
            );
            list.event(
                &UiEvent::MouseMove {
                    position: Point::new(33.0, 75.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
        }
        assert!(requests.drag.is_none());

        {
            let mut ctx = dispatching_ctx(
                &mut focus,
                &mut shortcut,
                &mut tooltip,
                &mut requests,
                &dispatch,
            );
            list.event(
                &UiEvent::MouseMove {
                    position: Point::new(48.0, 74.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
        }

        assert_eq!(
            requests.drag,
            Some(DragRequest::Begin(DragPayload::Asset(asset_id)))
        );
        assert!(requests.repaint);
    }

    #[test]
    fn dropping_payload_inside_list_dispatches_drop_action() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list =
            PanelList::new("Assets", sample_items()).on_drop(|payload, _position| match payload {
                DragPayload::File(paths) if !paths.is_empty() => {
                    Some(Action::ImportMedia(paths.clone()))
                }
                _ => None,
            });
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let path = PathBuf::from("E:/media/clip.mov");

        let result = list.event(
            &UiEvent::Drop {
                payload: DragPayload::File(vec![path.clone()]),
                position: Point::new(32.0, 80.0),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::ImportMedia(vec![path])]
        );
        assert!(requests.repaint);
    }

    #[test]
    fn file_drag_hover_sets_and_clears_drop_feedback() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Assets", sample_items())
            .on_drop(|_payload, _position| Some(Action::NoOp));
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        {
            let mut ctx = dispatching_ctx(
                &mut focus,
                &mut shortcut,
                &mut tooltip,
                &mut requests,
                &dispatch,
            );
            let result = list.event(
                &UiEvent::DragEnter {
                    payload: DragPayload::File(vec![PathBuf::from("E:/media/clip.mov")]),
                    position: Point::new(24.0, 76.0),
                },
                &mut ctx,
            );

            assert_eq!(result, EventResult::Handled);
            assert!(list.drop_hovered);
            assert!(requests.repaint);
        }

        requests.repaint = false;
        {
            let mut ctx = dispatching_ctx(
                &mut focus,
                &mut shortcut,
                &mut tooltip,
                &mut requests,
                &dispatch,
            );
            let result = list.event(&UiEvent::DragLeave, &mut ctx);

            assert_eq!(result, EventResult::Handled);
            assert!(!list.drop_hovered);
            assert!(requests.repaint);
        }
    }

    #[test]
    fn releasing_drag_candidate_before_threshold_clears_capture() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Assets", vec![drag_item(AssetId::new())]);
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        {
            let mut ctx = dispatching_ctx(
                &mut focus,
                &mut shortcut,
                &mut tooltip,
                &mut requests,
                &dispatch,
            );
            list.event(
                &UiEvent::MouseDown {
                    position: Point::new(30.0, 74.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
        }
        requests.pointer_capture = None;
        {
            let mut ctx = dispatching_ctx(
                &mut focus,
                &mut shortcut,
                &mut tooltip,
                &mut requests,
                &dispatch,
            );
            list.event(
                &UiEvent::MouseUp {
                    position: Point::new(30.0, 74.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
        }

        assert_eq!(
            requests.pointer_capture,
            Some(PointerCaptureRequest::Release(list.id()))
        );
        assert!(requests.drag.is_none());
    }

    #[test]
    fn disabled_item_consumes_click_without_selection_or_dispatch() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items());
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = list.event(
            &UiEvent::MouseDown {
                position: Point::new(30.0, 124.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(list.selected_index(), None);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn keyboard_navigation_skips_disabled_items_and_activation_dispatches() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items());
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(&UiEvent::FocusGained, &mut ctx);
        list.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        list.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        list.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(list.selected_index(), Some(2));
        assert_eq!(actions.borrow().last(), Some(&custom_action("activate-c")));
    }

    #[test]
    fn keyboard_navigation_ignores_modified_keys() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items()).with_selected(Some(2));
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(&UiEvent::FocusGained, &mut ctx);
        for (key, modifiers) in [
            (KeyCode::Down, Modifiers::ctrl()),
            (KeyCode::Up, Modifiers::shift()),
            (KeyCode::Home, Modifiers { alt: true, ..Default::default() }),
            (KeyCode::End, Modifiers { meta: true, ..Default::default() }),
            (KeyCode::Enter, Modifiers::ctrl()),
            (KeyCode::Space, Modifiers::shift()),
        ] {
            assert_eq!(
                list.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_eq!(list.selected_index(), Some(2));
        }

        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn escape_clears_local_selection_when_focused() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items()).with_selected(Some(2));
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(&UiEvent::FocusGained, &mut ctx);
        assert_eq!(
            list.event(
                &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(list.selected_index(), None);
        assert!(actions.borrow().is_empty());
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn escape_without_local_selection_is_ignored() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items());
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(&UiEvent::FocusGained, &mut ctx);
        assert_eq!(
            list.event(
                &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(list.selected_index(), None);
        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn wheel_scrolls_and_clamps_content() {
        let items = (0..12).map(|index| PanelListItem::new(format!("Item {index}"))).collect();
        let mut list = PanelList::new("Long", items);
        list.layout(Rect::new(0.0, 0.0, 240.0, 140.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(
            &UiEvent::MouseWheel {
                delta: 90.0,
                position: Point::new(30.0, 80.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(list.scroll_offset_y() > 0.0);
        assert!(requests.repaint);
    }

    #[test]
    fn scrollbar_thumb_drag_updates_offset_and_releases_capture() {
        let items = (0..16).map(|index| PanelListItem::new(format!("Item {index}"))).collect();
        let mut list = PanelList::new("Long", items);
        list.layout(Rect::new(0.0, 0.0, 240.0, 150.0));
        let thumb = list.scrollbar_thumb_rect().expect("overflowing list should have thumb");
        let start = Point::new(thumb.x + 2.0, thumb.y + 2.0);

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(list.id()))
        );

        list.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x, start.y + 24.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(list.scroll_offset_y() > 0.0);

        list.event(
            &UiEvent::MouseUp {
                position: Point::new(start.x, start.y + 24.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(list.id()))
        );
    }

    #[test]
    fn scrollbar_track_click_pages_without_selecting_row() {
        let items = (0..16).map(|index| PanelListItem::new(format!("Item {index}"))).collect();
        let mut list = PanelList::new("Long", items);
        list.layout(Rect::new(0.0, 0.0, 240.0, 150.0));
        let track = list.scrollbar_track_rect().expect("overflowing list should have track");

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = list.event(
            &UiEvent::MouseDown {
                position: Point::new(track.x + 1.0, track.y + track.height - 2.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(list.scroll_offset_y() > 0.0);
        assert_eq!(list.selected_index(), None);
    }

    #[test]
    fn set_items_clamps_selection_when_selected_item_disappears() {
        let mut list = PanelList::new("Assets", sample_items()).with_selected(Some(2));
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        list.set_items(vec![PanelListItem::new("Only")]);

        assert_eq!(list.selected_index(), None);
    }

    #[test]
    fn state_round_trips_filter_selection_and_scroll_by_item_title() {
        let items = (0..16).map(|index| PanelListItem::new(format!("Camera {index}"))).collect();
        let mut list = PanelList::new("Assets", items).with_filter("Search assets");
        list.layout(Rect::new(0.0, 0.0, 240.0, 170.0));
        list.set_filter_query("camera");
        list.set_selected(Some(9));
        list.set_scroll_y(120.0);
        let state = list.state();

        let mut restored = PanelList::new(
            "Assets",
            vec![
                PanelListItem::new("Camera 1"),
                PanelListItem::new("Inserted"),
                PanelListItem::new("Camera 9"),
                PanelListItem::new("Camera 10"),
            ],
        )
        .with_filter("Search assets");
        restored.layout(Rect::new(0.0, 0.0, 240.0, 120.0));
        restored.restore_state(&state);

        assert_eq!(restored.filter_query(), "camera");
        assert_eq!(restored.selected_index(), Some(2));
        assert!(restored.scroll_offset_y() > 0.0);
    }

    #[test]
    fn restore_state_falls_back_to_index_when_title_is_missing() {
        let mut list = PanelList::new("Effects", sample_items()).with_selected(Some(2));
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));
        let state = list.state();

        let mut restored = PanelList::new(
            "Effects",
            vec![
                PanelListItem::new("Replacement 0"),
                PanelListItem::new("Replacement 1"),
                PanelListItem::new("Replacement 2"),
            ],
        );
        restored.layout(Rect::new(0.0, 0.0, 240.0, 180.0));
        restored.restore_state(&state);

        assert_eq!(restored.selected_index(), Some(2));
    }

    #[test]
    fn filter_query_clicks_visible_rows_with_original_item_identity() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new(
            "Assets",
            vec![
                PanelListItem::new("Camera A"),
                PanelListItem::new("Dialogue"),
                PanelListItem::new("Color Grade").with_select_action(Action::Play),
            ],
        )
        .with_filter("Search assets");
        list.set_filter_query("color");
        list.layout(Rect::new(0.0, 0.0, 260.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let result = list.event(
            &UiEvent::MouseDown {
                position: Point::new(24.0, 92.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(list.selected_index(), Some(2));
        assert_eq!(actions.borrow().as_slice(), &[Action::Play]);
    }

    #[test]
    fn filter_query_keyboard_navigation_uses_visible_enabled_items_only() {
        let mut list = PanelList::new(
            "Effects",
            vec![
                PanelListItem::new("Blur"),
                PanelListItem::new("Color Wheels"),
                PanelListItem::new("Color Match").disabled(true),
                PanelListItem::new("Color Balance"),
            ],
        )
        .with_filter("Search effects");
        list.set_filter_query("color");
        list.layout(Rect::new(0.0, 0.0, 260.0, 220.0));

        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            list.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            list.event(
                &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            list.event(
                &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(list.selected_index(), Some(3));
    }

    #[test]
    fn set_selected_rejects_items_hidden_by_filter() {
        let mut list = PanelList::new(
            "Assets",
            vec![
                PanelListItem::new("Camera A"),
                PanelListItem::new("Music Bed"),
                PanelListItem::new("Color Grade"),
            ],
        )
        .with_filter("Search assets");
        list.set_filter_query("music");

        list.set_selected(Some(2));
        assert_eq!(list.selected_index(), None);

        list.set_selected(Some(1));
        assert_eq!(list.selected_index(), Some(1));
    }

    #[test]
    fn filter_input_child_syncs_query_and_requests_repaint() {
        let mut list = PanelList::new(
            "Assets",
            vec![PanelListItem::new("A Cam"), PanelListItem::new("Music Bed")],
        )
        .with_filter("Search assets")
        .with_selected(Some(1));
        list.layout(Rect::new(0.0, 0.0, 260.0, 180.0));

        let input = list
            .child_mut(0)
            .and_then(Widget::as_any_mut)
            .and_then(|any| any.downcast_mut::<TextInput>())
            .expect("filter input child");
        input.set_text("cam".to_owned());

        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            list.after_child_event(&UiEvent::TextInput("cam".to_owned()), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(list.filter_query(), "cam");
        assert_eq!(list.selected_index(), None);
        assert!(requests.repaint);
    }

    #[test]
    fn filter_query_paint_hides_non_matching_rows() {
        let mut list = PanelList::new(
            "Assets",
            vec![
                PanelListItem::new("Camera A"),
                PanelListItem::new("Music Bed"),
                PanelListItem::new("Color Grade"),
            ],
        )
        .with_filter("Search assets");
        list.set_filter_query("music");
        list.layout(Rect::new(0.0, 0.0, 260.0, 180.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 180.0),
        };
        list.paint(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "Music Bed"));
        assert!(!encoder.texts.iter().any(|text| text == "Camera A"));
        assert!(!encoder.texts.iter().any(|text| text == "Color Grade"));
    }

    #[test]
    fn paint_draws_header_rows_and_clips_row_text() {
        let mut list = PanelList::new(
            "Assets",
            vec![PanelListItem::new("Media").with_subtitle("Imported footage").with_badge("4K")],
        )
        .with_subtitle("Project library");
        list.layout(Rect::new(0.0, 0.0, 260.0, 160.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 160.0),
        };
        list.paint(&mut ctx);

        assert!(encoder.rects >= 3);
        assert_eq!(encoder.lines, 1);
        assert!(encoder.clips >= 2);
        assert!(encoder.texts.iter().any(|text| text == "Assets"));
        assert!(encoder.texts.iter().any(|text| text == "Imported footage"));
    }

    #[test]
    fn paint_badge_uses_dynamic_width_and_semantic_tone() {
        let mut list = PanelList::new(
            "Effects",
            vec![PanelListItem::new("Chroma Key")
                .with_subtitle("Keying")
                .with_badge_tone("KEYING", PanelListBadgeTone::Warning)],
        );
        list.layout(Rect::new(0.0, 0.0, 260.0, 140.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 140.0),
        };
        list.paint(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "KEYING"));
        assert!(
            encoder.rect_bounds.iter().any(|rect| rect.width > 44.0 && rect.height == 22.0),
            "long badges should measure wider than the previous fixed badge width"
        );
        assert!(
            encoder
                .rect_colors
                .iter()
                .any(|color| *color == color_with_alpha(theme.colors.warning, 0.22)),
            "warning badges should use the theme warning token"
        );
    }

    #[test]
    fn filter_query_matches_badge_label_after_badge_model_upgrade() {
        let mut list = PanelList::new(
            "Effects",
            vec![
                PanelListItem::new("Gaussian Blur").with_badge("GPU"),
                PanelListItem::new("Chroma Key")
                    .with_badge_tone("KEY", PanelListBadgeTone::Warning),
            ],
        )
        .with_filter("Search effects");
        list.set_filter_query("key");
        list.layout(Rect::new(0.0, 0.0, 260.0, 180.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 180.0),
        };
        list.paint(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "Chroma Key"));
        assert!(!encoder.texts.iter().any(|text| text == "Gaussian Blur"));
    }

    #[test]
    fn paint_row_draws_optional_vector_icon() {
        let mut list = PanelList::new(
            "Project",
            vec![PanelListItem::new("New project...").with_icon(test_icon())],
        );
        list.layout(Rect::new(0.0, 0.0, 260.0, 140.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 140.0),
        };
        list.paint(&mut ctx);

        assert!(encoder.triangles > 0 || encoder.raster_images > 0);
        assert!(encoder.texts.iter().any(|text| text == "New project..."));
    }

    #[test]
    fn selected_row_paints_outer_ring_before_row_fill() {
        let mut list =
            PanelList::new("Assets", vec![PanelListItem::new("Selected")]).with_selected(Some(0));
        list.layout(Rect::new(0.0, 0.0, 260.0, 140.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 140.0),
        };
        list.paint(&mut ctx);

        assert!(encoder.rect_bounds.windows(2).any(|pair| {
            pair[0].width > pair[1].width
                && pair[0].height > pair[1].height
                && (pair[0].x - pair[1].x).abs() <= 1.1
                && (pair[0].y - pair[1].y).abs() <= 1.1
        }));
    }
}
