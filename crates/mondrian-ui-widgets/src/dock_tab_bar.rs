//! Dock Tab 标签栏
//!
//! 水平排列的标签按钮，点击切换 active tab。

use mondrian_editor_state::state::PanelKind;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::paint::{color_with_alpha, mix_color};
use crate::text_metrics::{centered_text_x, measure_single_line};

const DRAG_START_DISTANCE: f32 = 5.0;

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
    bar_height: f32,
    tab_min_width: f32,
    tab_max_width: f32,
    tab_padding_x: f32,
}

#[derive(Debug, Clone, Copy)]
struct TabDragCandidate {
    start: Point,
    panel: PanelKind,
}

impl DockTabBar {
    pub fn new(tabs: Vec<TabInfo>) -> Self {
        Self {
            id: WidgetId::new(),
            tabs,
            bounds: Rect::ZERO,
            hovered_tab: None,
            drag_candidate: None,
            bar_height: 26.0,
            tab_min_width: 40.0,
            tab_max_width: 148.0,
            tab_padding_x: 18.0,
        }
    }

    pub fn set_tabs(&mut self, tabs: Vec<TabInfo>) {
        self.tabs = tabs;
        self.drag_candidate = None;
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

        let font_size = 12.0;
        let available = self.bounds.width.max(0.0);
        let desired = self
            .tabs
            .iter()
            .map(|tab| {
                let label_width = measure_single_line(&tab.label, font_size).0;
                (label_width + self.tab_padding_x * 2.0)
                    .clamp(self.tab_min_width, self.tab_max_width)
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
                let rect = Rect::new(x, self.bounds.y, width, self.bar_height);
                x += width;
                rect
            })
            .collect()
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
}

impl Widget for DockTabBar {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(constraint.max.width.min(600.0), self.bar_height))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = Rect::new(bounds.x, bounds.y, bounds.width, self.bar_height);
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
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
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let bg = Rect::new(
            self.bounds.x,
            self.bounds.y,
            self.bounds.width,
            self.bar_height,
        );
        let bar_fill = mix_color(tokens.background, tokens.card, 0.62);
        ctx.encoder.draw_rect(bg, bar_fill, 0.0);
        ctx.encoder.draw_rect(
            Rect::new(bg.x, bg.y + bg.height - 1.0, bg.width, 1.0),
            color_with_alpha(tokens.border, 0.68),
            0.0,
        );

        let rects = self.tab_rects();
        for (i, tab) in self.tabs.iter().enumerate() {
            if i >= rects.len() {
                break;
            }
            let r = rects[i];
            let is_active = tab.active;
            let is_hovered = self.hovered_tab == Some(i);

            let inset = r.inset(2.0, 3.0);
            if is_hovered {
                ctx.encoder.draw_rect(
                    inset,
                    color_with_alpha(tokens.accent, if is_active { 0.76 } else { 0.46 }),
                    spacing.radius_sm,
                );
            }

            if !tab.label.is_empty() {
                let font_size = ctx.theme.typography.tab_label.font_size;
                let tx = centered_text_x(inset.x, inset.width, &tab.label, font_size);
                let ty = inset.y + (inset.height - font_size * 1.3).max(0.0) * 0.5;
                let pos = mondrian_ui_core::types::snap_point(Point::new(tx, ty));
                ctx.push_clip(inset);
                let text_color = if is_active || is_hovered {
                    tokens.foreground
                } else {
                    tokens.muted_foreground
                };
                ctx.encoder.draw_text(&tab.label, font_size, pos, text_color);
                ctx.pop_clip();
            }

            if is_active {
                let indicator_width = (inset.width * 0.42).clamp(26.0, 54.0);
                let indicator = Rect::new(
                    inset.x + (inset.width - indicator_width) * 0.5,
                    bg.y + bg.height - 2.0,
                    indicator_width,
                    2.0,
                );
                ctx.encoder.draw_rect(indicator, tokens.primary, 1.0);
            }
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
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Default)]
    struct PaintRecorder {
        clips: Vec<Rect>,
        clip_pops: usize,
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for PaintRecorder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, _bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {}

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
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.into());
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
        assert_eq!(
            encoder.clips,
            vec![
                Rect::new(2.0, 3.0, 101.24535, 20.0),
                Rect::new(107.24535, 3.0, 50.754646, 20.0)
            ]
        );
        assert_eq!(encoder.clip_pops, 2);
    }
}
