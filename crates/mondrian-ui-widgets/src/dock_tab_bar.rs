//! Dock Tab 标签栏
//!
//! 水平排列的标签按钮，点击切换 active tab。

use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::{Theme, ThemePreset};
use std::cell::Cell;
use std::rc::Rc;

use crate::paint::{centered_text_origin_y, color_with_alpha};
use crate::text_metrics::{centered_text_x, measure_single_line};

const DRAG_START_DISTANCE: f32 = 5.0;

/// Function used by [`DockTabBar`] to map a tab-bar drop into an app action.
pub type DockTabDropAction = dyn Fn(PanelKind, PanelKind, usize) -> Action + 'static;

/// 单个 Tab 的信息
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub label: String,
    pub active: bool,
    pub panel_kind: Option<PanelKind>,
}

/// DockTabBar —— 水平标签栏
pub struct DockTabBar {
    id: WidgetId,
    tabs: Vec<TabInfo>,
    bounds: Rect,
    hovered_tab: Option<usize>,
    drag_candidate: Option<TabDragCandidate>,
    drop_hover: Option<TabDropHover>,
    on_tab_drop: Option<Rc<DockTabDropAction>>,
    visual: Cell<DockTabBarVisualTokens>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct DockTabBarVisualTokens {
    bar_height: f32,
    tab_min_width: f32,
    tab_max_width: f32,
    tab_padding_x: f32,
    tab_font_size: f32,
    tab_inset_x: f32,
    tab_inset_y: f32,
    hover_active_alpha: f32,
    hover_inactive_alpha: f32,
    indicator_empty_width: f32,
    indicator_padding_x: f32,
    indicator_min_width: f32,
    indicator_height: f32,
    indicator_bottom_inset: f32,
    indicator_alpha: f32,
    drop_lane_width: f32,
    drop_lane_inset_y: f32,
    drop_lane_alpha: f32,
    drop_indicator_width: f32,
    drop_indicator_inset_y: f32,
    drop_indicator_alpha: f32,
    drop_cap_width: f32,
    drop_cap_height: f32,
    drop_cap_inset_y: f32,
    radius: f32,
    drop_radius: f32,
}

impl DockTabBarVisualTokens {
    fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            bar_height: (spacing.interact_height - spacing.border_emphasis).max(1.0),
            tab_min_width: (spacing.interact_height + spacing.md + spacing.border_emphasis)
                .max(1.0),
            tab_max_width: (spacing.inspector_panel_width * 0.43).max(1.0),
            tab_padding_x: spacing.md + spacing.sm + spacing.border_emphasis,
            tab_font_size: theme.typography.tab_label.font_size,
            tab_inset_x: spacing.border_emphasis,
            tab_inset_y: spacing.border_emphasis + spacing.border_standard,
            hover_active_alpha: 0.40,
            hover_inactive_alpha: 0.28,
            indicator_empty_width: (spacing.icon_size + spacing.border_emphasis).max(1.0),
            indicator_padding_x: spacing.sm + spacing.border_emphasis,
            indicator_min_width: (spacing.icon_size + spacing.border_emphasis).max(1.0),
            indicator_height: spacing.border_emphasis.max(1.0),
            indicator_bottom_inset: spacing.border_emphasis,
            indicator_alpha: 0.86,
            drop_lane_width: spacing.xs,
            drop_lane_inset_y: spacing.xs,
            drop_lane_alpha: 0.14,
            drop_indicator_width: spacing.border_emphasis.max(1.0),
            drop_indicator_inset_y: (spacing.sm - spacing.border_standard).max(0.0),
            drop_indicator_alpha: 0.82,
            drop_cap_width: (spacing.icon_size - spacing.sm).max(1.0),
            drop_cap_height: spacing.border_emphasis.max(1.0),
            drop_cap_inset_y: spacing.border_emphasis + spacing.border_standard,
            radius: spacing.radius_sm,
            drop_radius: spacing.radius_sm.min(spacing.xs * 0.5),
        }
    }
}

impl Default for DockTabBarVisualTokens {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

#[derive(Debug, Clone, Copy)]
struct TabDragCandidate {
    start: Point,
    panel: PanelKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabDropHover {
    dragged: PanelKind,
    insert_index: usize,
}

impl DockTabBar {
    pub fn new(tabs: Vec<TabInfo>) -> Self {
        Self {
            id: WidgetId::new(),
            tabs,
            bounds: Rect::ZERO,
            hovered_tab: None,
            drag_candidate: None,
            drop_hover: None,
            on_tab_drop: None,
            visual: Cell::new(DockTabBarVisualTokens::default()),
        }
    }

    pub fn set_tabs(&mut self, tabs: Vec<TabInfo>) {
        self.tabs = tabs;
        self.drag_candidate = None;
        self.drop_hover = None;
    }

    pub fn set_tab_drop_action(&mut self, action: Option<Rc<DockTabDropAction>>) {
        self.on_tab_drop = action;
    }

    pub fn active_index(&self) -> usize {
        self.tabs.iter().position(|t| t.active).unwrap_or(0)
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_panel_kind(&self) -> Option<PanelKind> {
        self.tabs.get(self.active_index()).and_then(|tab| tab.panel_kind)
    }

    pub fn tab_panel_kinds(&self) -> Vec<PanelKind> {
        self.tabs.iter().filter_map(|tab| tab.panel_kind).collect()
    }

    pub fn set_active(&mut self, index: usize) {
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            tab.active = i == index;
        }
    }

    fn tab_rects(&self) -> Vec<Rect> {
        if self.tabs.is_empty() {
            return Vec::new();
        }

        let visual = self.visual.get();
        let font_size = self.visual.get().tab_font_size;
        let available = self.bounds.width.max(0.0);
        let desired = self
            .tabs
            .iter()
            .map(|tab| {
                let label_width = measure_single_line(&tab.label, font_size).0;
                (label_width + visual.tab_padding_x * 2.0)
                    .clamp(visual.tab_min_width, visual.tab_max_width)
            })
            .collect::<Vec<_>>();
        let desired_total: f32 = desired.iter().sum();
        let scale = if desired_total > available && desired_total > 0.0 {
            (available / desired_total).clamp(0.0, 1.0)
        } else {
            1.0
        };

        let mut x = self.bounds.x;
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let remaining = (self.bounds.x + available - x).max(0.0);
                let width = (desired[i] * scale).min(remaining);
                let rect = Rect::new(x, self.bounds.y, width, visual.bar_height);
                x += width;
                rect
            })
            .collect()
    }

    fn tab_insert_index_at(&self, position: Point) -> usize {
        let rects = self.tab_rects();
        for (index, rect) in rects.iter().enumerate() {
            if position.x < rect.center().x {
                return index;
            }
        }
        rects.len()
    }

    fn insert_indicator_x(&self, insert_index: usize) -> f32 {
        let rects = self.tab_rects();
        if rects.is_empty() {
            return self.bounds.x;
        }
        if insert_index == 0 {
            rects[0].x
        } else if insert_index >= rects.len() {
            rects.last().map(|rect| rect.x + rect.width).unwrap_or(self.bounds.x)
        } else {
            rects[insert_index].x
        }
    }

    fn tab_drop_target_panel(&self, dragged: PanelKind) -> Option<PanelKind> {
        self.tab_panel_kinds().into_iter().find(|kind| *kind != dragged).or_else(|| {
            let active = self.active_panel_kind()?;
            (active != dragged).then_some(active)
        })
    }

    fn begin_drag_candidate_if_needed(
        &mut self,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        let Some(candidate) = self.drag_candidate else {
            return EventResult::Ignored;
        };
        let dx = position.x - candidate.start.x;
        let dy = position.y - candidate.start.y;
        if dx * dx + dy * dy < DRAG_START_DISTANCE * DRAG_START_DISTANCE {
            return EventResult::Ignored;
        }

        self.drag_candidate = None;
        ctx.begin_drag(DragPayload::PanelTab(candidate.panel));
        ctx.request_repaint();
        EventResult::Handled
    }

    fn update_tab_drop_hover(
        &mut self,
        dragged: PanelKind,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        if self.on_tab_drop.is_none() || !self.bounds.contains(position) {
            return EventResult::Ignored;
        }
        let Some(_) = self.tab_drop_target_panel(dragged) else {
            return EventResult::Ignored;
        };
        let next = Some(TabDropHover {
            dragged,
            insert_index: self.tab_insert_index_at(position),
        });
        if self.drop_hover != next {
            self.drop_hover = next;
            ctx.request_repaint();
        }
        EventResult::Handled
    }

    fn drop_panel_tab(
        &mut self,
        dragged: PanelKind,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        let insert_index = self.tab_insert_index_at(position);
        self.drop_hover = None;
        let Some(action) = &self.on_tab_drop else {
            ctx.request_repaint();
            return EventResult::Ignored;
        };
        let Some(target) = self.tab_drop_target_panel(dragged) else {
            ctx.request_repaint();
            return EventResult::Ignored;
        };
        (ctx.dispatch)(action(dragged, target, insert_index));
        ctx.request_repaint();
        EventResult::Handled
    }
}

impl Widget for DockTabBar {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let visual = self.visual.get();
        constraint.constrain(Size::new(
            constraint.max.width.min(600.0),
            visual.bar_height,
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = Rect::new(
            bounds.x,
            bounds.y,
            bounds.width,
            self.visual.get().bar_height,
        );
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::DragEnter { payload: DragPayload::PanelTab(panel), position } => {
                return self.update_tab_drop_hover(*panel, *position, ctx);
            }
            UiEvent::DragOver { position } => {
                if let Some(hover) = self.drop_hover {
                    return self.update_tab_drop_hover(hover.dragged, *position, ctx);
                }
            }
            UiEvent::DragLeave if self.drop_hover.is_some() => {
                self.drop_hover = None;
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::Drop { payload: DragPayload::PanelTab(panel), position } => {
                return self.drop_panel_tab(*panel, *position, ctx);
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                let rects = self.tab_rects();
                for (i, r) in rects.iter().enumerate() {
                    if r.contains(*position) {
                        self.set_active(i);
                        if let Some(panel) = self.tabs.get(i).and_then(|tab| tab.panel_kind) {
                            self.drag_candidate =
                                Some(TabDragCandidate { start: *position, panel });
                            ctx.request_pointer_capture(self.id);
                        }
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
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
                let rects = self.tab_rects();
                self.hovered_tab = rects.iter().position(|r| r.contains(*position));
            }
            UiEvent::FocusLost | UiEvent::DragLeave if self.drag_candidate.is_some() => {
                self.drag_candidate = None;
                ctx.release_pointer_capture(self.id);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            _ => {}
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.visual.set(DockTabBarVisualTokens::from_theme(ctx.theme));
        let visual = self.visual.get();
        let tokens = &ctx.theme.colors;

        let bg = self.bounds;
        let bar_fill = tokens.card;
        ctx.encoder.draw_rect(bg, bar_fill, 0.0);

        let rects = self.tab_rects();
        for (i, tab) in self.tabs.iter().enumerate() {
            if i >= rects.len() {
                break;
            }
            let r = rects[i];
            let is_active = tab.active;
            let is_hovered = self.hovered_tab == Some(i);

            let inset = r.inset(visual.tab_inset_x, visual.tab_inset_y);
            if is_hovered {
                ctx.encoder.draw_rect(
                    inset,
                    color_with_alpha(
                        tokens.surface_2,
                        if is_active {
                            visual.hover_active_alpha
                        } else {
                            visual.hover_inactive_alpha
                        },
                    ),
                    visual.radius,
                );
            }

            if !tab.label.is_empty() {
                let style = &ctx.theme.typography.tab_label;
                let font_size = style.font_size;
                let tx = centered_text_x(inset.x, inset.width, &tab.label, font_size);
                let ty = centered_text_origin_y(inset, style.line_height);
                let pos = mondrian_ui_core::types::snap_point(Point::new(tx, ty));
                ctx.push_clip(inset);
                let text_color = if is_active || is_hovered {
                    tokens.foreground
                } else {
                    tokens.text_tertiary
                };
                ctx.encoder.draw_text(&tab.label, font_size, pos, text_color);
                ctx.pop_clip();
            }

            if is_active {
                let label_width = if tab.label.is_empty() {
                    visual.indicator_empty_width
                } else {
                    measure_single_line(&tab.label, ctx.theme.typography.tab_label.font_size).0
                };
                let indicator_width = (label_width + visual.indicator_padding_x).clamp(
                    visual.indicator_min_width,
                    inset.width.max(visual.indicator_min_width),
                );
                let indicator = Rect::new(
                    inset.x + (inset.width - indicator_width) * 0.5,
                    bg.y + bg.height - visual.indicator_bottom_inset,
                    indicator_width,
                    visual.indicator_height,
                );
                ctx.encoder.draw_rect(
                    indicator,
                    color_with_alpha(tokens.foreground, visual.indicator_alpha),
                    visual.indicator_height * 0.5,
                );
            }
        }

        if let Some(hover) = self.drop_hover {
            let x = self.insert_indicator_x(hover.insert_index);
            ctx.encoder.draw_rect(
                Rect::new(
                    x - visual.drop_lane_width * 0.5,
                    bg.y + visual.drop_lane_inset_y,
                    visual.drop_lane_width,
                    (bg.height - visual.drop_lane_inset_y * 2.0).max(0.0),
                ),
                color_with_alpha(tokens.foreground, visual.drop_lane_alpha),
                visual.drop_radius,
            );
            let indicator = Rect::new(
                x - visual.drop_indicator_width * 0.5,
                bg.y + visual.drop_indicator_inset_y,
                visual.drop_indicator_width,
                (bg.height - visual.drop_indicator_inset_y * 2.0).max(0.0),
            );
            ctx.encoder.draw_rect(
                indicator,
                color_with_alpha(tokens.foreground, visual.drop_indicator_alpha),
                visual.drop_indicator_width * 0.5,
            );
            ctx.encoder.draw_rect(
                Rect::new(
                    x - visual.drop_cap_width * 0.5,
                    bg.y + visual.drop_cap_inset_y,
                    visual.drop_cap_width,
                    visual.drop_cap_height,
                ),
                color_with_alpha(tokens.foreground, visual.drop_indicator_alpha),
                visual.drop_cap_height * 0.5,
            );
            ctx.encoder.draw_rect(
                Rect::new(
                    x - visual.drop_cap_width * 0.5,
                    bg.y + bg.height - visual.drop_cap_inset_y - visual.drop_cap_height,
                    visual.drop_cap_width,
                    visual.drop_cap_height,
                ),
                color_with_alpha(tokens.foreground, visual.drop_indicator_alpha),
                visual.drop_cap_height * 0.5,
            );
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn children(&self) -> &[Box<dyn Widget>] {
        &[]
    }

    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut []
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_editor_state::Action;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct PaintRecorder {
        clips: Vec<Rect>,
        clip_pops: usize,
        rects: Vec<Rect>,
        colors: Vec<mondrian_core::Color>,
        radii: Vec<f32>,
        texts: Vec<String>,
        text_font_sizes: Vec<f32>,
    }

    impl DrawCommandEncoder for PaintRecorder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, bounds: Rect, color: mondrian_core::Color, corner_radius: f32) {
            self.rects.push(bounds);
            self.colors.push(color);
            self.radii.push(corner_radius);
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
            self.texts.push(text.into());
            self.text_font_sizes.push(font_size);
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn make_tabs(active: usize) -> Vec<TabInfo> {
        vec![
            TabInfo {
                label: "A".into(),
                active: active == 0,
                panel_kind: Some(PanelKind::Assets),
            },
            TabInfo {
                label: "B".into(),
                active: active == 1,
                panel_kind: Some(PanelKind::Effects),
            },
            TabInfo {
                label: "C".into(),
                active: active == 2,
                panel_kind: Some(PanelKind::Inspector),
            },
        ]
    }

    #[test]
    fn tab_bar_new_active_index() {
        let bar = DockTabBar::new(make_tabs(0));
        assert_eq!(bar.active_index(), 0);
    }

    #[test]
    fn tab_bar_set_active_changes_index() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.set_active(2);
        assert_eq!(bar.active_index(), 2);
    }

    #[test]
    fn tab_bar_set_tabs_updates_list() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.set_tabs(vec![TabInfo {
            label: "X".into(),
            active: true,
            panel_kind: None,
        }]);
        assert_eq!(bar.tabs.len(), 1);
        assert_eq!(bar.active_index(), 0);
    }

    #[test]
    fn tab_bar_no_active_returns_zero() {
        let bar = DockTabBar::new(vec![TabInfo {
            label: "X".into(),
            active: false,
            panel_kind: None,
        }]);
        assert_eq!(bar.active_index(), 0);
    }

    #[test]
    fn tab_bar_tab_rects_count_matches_tabs() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));
        assert_eq!(bar.tab_rects().len(), 3);
    }

    #[test]
    fn tab_bar_click_switches_active() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        // Click inside the second content-sized tab.
        let r = bar.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 13.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Handled);
        assert_eq!(bar.active_index(), 1);
    }

    #[test]
    fn tab_bar_drag_after_threshold_begins_panel_tab_drag() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let down = bar.event(
            &UiEvent::MouseDown {
                position: Point::new(10.0, 13.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(down, EventResult::Handled);

        let moved = bar.event(
            &UiEvent::MouseMove {
                position: Point::new(30.0, 14.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(moved, EventResult::Handled);
        assert_eq!(
            ctx.requests.drag,
            Some(mondrian_ui_core::widget::DragRequest::Begin(
                DragPayload::PanelTab(PanelKind::Assets)
            ))
        );
    }

    #[test]
    fn tab_bar_small_mouse_move_keeps_click_candidate() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        bar.event(
            &UiEvent::MouseDown {
                position: Point::new(10.0, 13.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let moved = bar.event(
            &UiEvent::MouseMove {
                position: Point::new(12.0, 14.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(moved, EventResult::Ignored);
        assert_eq!(ctx.requests.drag, None);
        assert!(bar.drag_candidate.is_some());
    }

    #[test]
    fn tab_bar_drop_panel_tab_dispatches_insert_index_action() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));
        bar.set_tab_drop_action(Some(Rc::new(|panel, target, insert_index| {
            assert_eq!(panel, PanelKind::Inspector);
            assert_eq!(target, PanelKind::Assets);
            assert_eq!(insert_index, 1);
            Action::FocusPanel(panel)
        })));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch_actions = Rc::clone(&actions);
        let dispatch = move |action| dispatch_actions.borrow_mut().push(action);
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = bar.event(
            &UiEvent::Drop {
                payload: DragPayload::PanelTab(PanelKind::Inspector),
                position: Point::new(55.0, 13.0),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::FocusPanel(PanelKind::Inspector)]
        );
    }

    #[test]
    fn tab_bar_drag_hover_paints_insert_indicator_without_dock_preview() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));
        bar.set_tab_drop_action(Some(Rc::new(|panel, _target, _insert_index| {
            Action::FocusPanel(panel)
        })));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});
        let result = bar.event(
            &UiEvent::DragEnter {
                payload: DragPayload::PanelTab(PanelKind::Inspector),
                position: Point::new(55.0, 13.0),
            },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            bar.drop_hover,
            Some(TabDropHover { dragged: PanelKind::Inspector, insert_index: 1 })
        );

        let theme = ThemePreset::Dark.build();
        let mut encoder = PaintRecorder::default();
        let mut paint_ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 320.0, 80.0),
        };
        bar.paint(&mut paint_ctx);

        let visual = DockTabBarVisualTokens::from_theme(&theme);
        assert!(encoder
            .rects
            .iter()
            .any(|rect| rect.width == visual.drop_indicator_width && rect.height > 10.0));
    }

    #[test]
    fn tab_bar_rejects_drop_when_only_target_is_dragged_tab() {
        let mut bar = DockTabBar::new(vec![TabInfo {
            label: "检查器".into(),
            active: true,
            panel_kind: Some(PanelKind::Inspector),
        }]);
        bar.layout(Rect::new(0.0, 0.0, 160.0, 30.0));
        bar.set_tab_drop_action(Some(Rc::new(|panel, _target, _insert_index| {
            Action::FocusPanel(panel)
        })));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});
        let result = bar.event(
            &UiEvent::DragEnter {
                payload: DragPayload::PanelTab(PanelKind::Inspector),
                position: Point::new(20.0, 13.0),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(bar.drop_hover, None);
    }

    #[test]
    fn tab_bar_click_outside_tabs_ignored() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = bar.event(
            &UiEvent::MouseDown {
                position: Point::new(400.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert_eq!(bar.active_index(), 0);
    }

    #[test]
    fn tab_bar_hover_tracks_mouse() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        assert_eq!(bar.hovered_tab, None);

        bar.event(
            &UiEvent::MouseMove {
                position: Point::new(10.0, 13.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(bar.hovered_tab, Some(0));
    }

    #[test]
    fn tab_bar_measure_returns_bar_height() {
        let bar = DockTabBar::new(make_tabs(0));
        let s = bar.measure(LayoutConstraint::LOOSE);
        assert!(s.width > 0.0);
        assert!(s.height > 0.0);
    }

    #[test]
    fn tab_bar_hit_test_in_bounds() {
        let mut bar = DockTabBar::new(make_tabs(0));
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));
        assert!(bar.hit_test(Point::new(150.0, 15.0)));
        assert!(!bar.hit_test(Point::new(400.0, 15.0)));
    }

    #[test]
    fn tab_bar_empty_tabs_does_not_panic() {
        let mut bar = DockTabBar::new(vec![]);
        bar.layout(Rect::new(0.0, 0.0, 300.0, 30.0));
        assert_eq!(bar.active_index(), 0);
        let rects = bar.tab_rects();
        assert_eq!(rects.len(), 0); // empty tabs → empty rects
    }

    #[test]
    fn tab_bar_paint_clips_each_tab_label() {
        let mut bar = DockTabBar::new(vec![
            TabInfo {
                label: "A very long tab label".into(),
                active: true,
                panel_kind: None,
            },
            TabInfo {
                label: "Second".into(),
                active: false,
                panel_kind: None,
            },
        ]);
        bar.layout(Rect::new(0.0, 0.0, 160.0, 32.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = PaintRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 80.0),
        };

        bar.paint(&mut ctx);

        assert_eq!(
            encoder.texts,
            vec!["A very long tab label".to_string(), "Second".to_string()]
        );
        let visual = DockTabBarVisualTokens::from_theme(&theme);
        let expected = bar
            .tab_rects()
            .into_iter()
            .map(|rect| rect.inset(visual.tab_inset_x, visual.tab_inset_y))
            .collect::<Vec<_>>();
        assert_eq!(encoder.clips, expected);
        assert_eq!(encoder.clip_pops, 2);
    }

    #[test]
    fn tab_bar_visual_metrics_follow_theme_tokens() {
        let mut bar = DockTabBar::new(make_tabs(0));
        let mut theme = ThemePreset::Dark.build();
        theme.spacing.interact_height = 34.0;
        theme.spacing.border_emphasis = 3.0;
        theme.spacing.border_standard = 2.0;
        theme.spacing.md = 12.0;
        theme.spacing.sm = 8.0;
        theme.spacing.radius_sm = 7.0;
        theme.typography.tab_label.font_size = 13.0;
        let visual = DockTabBarVisualTokens::from_theme(&theme);
        bar.layout(Rect::new(10.0, 20.0, 260.0, 99.0));
        bar.hovered_tab = Some(0);
        let mut encoder = PaintRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 320.0, 120.0),
        };

        bar.paint(&mut ctx);

        assert_eq!(
            bar.bounds.height,
            DockTabBarVisualTokens::default().bar_height
        );
        assert_eq!(
            bar.measure(LayoutConstraint::LOOSE).height,
            visual.bar_height
        );
        let rects = bar.tab_rects();
        assert_eq!(rects[0].height, visual.bar_height);
        assert_eq!(
            encoder.rects[1],
            rects[0].inset(visual.tab_inset_x, visual.tab_inset_y)
        );
        assert_eq!(
            encoder.colors[1],
            color_with_alpha(theme.colors.surface_2, visual.hover_active_alpha)
        );
        assert_eq!(encoder.radii[1], visual.radius);
        assert_eq!(
            encoder.text_font_sizes[0],
            theme.typography.tab_label.font_size
        );
        let active_indicator = encoder
            .rects
            .iter()
            .find(|rect| rect.height == visual.indicator_height)
            .expect("active tab should paint indicator");
        assert_eq!(
            active_indicator.y,
            bar.bounds.y + bar.bounds.height - visual.indicator_bottom_inset
        );
    }
}
