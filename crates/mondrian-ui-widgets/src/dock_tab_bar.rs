//! Dock Tab 标签栏
//!
//! 水平排列的标签按钮，点击切换 active tab。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::text_metrics::centered_text_x;

/// 单个 Tab 的信息
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub label: String,
    pub active: bool,
}

/// DockTabBar —— 水平标签栏
pub struct DockTabBar {
    id: WidgetId,
    tabs: Vec<TabInfo>,
    bounds: Rect,
    hovered_tab: Option<usize>,
    bar_height: f32,
    tab_min_width: f32,
}

impl DockTabBar {
    pub fn new(tabs: Vec<TabInfo>) -> Self {
        Self {
            id: WidgetId::new(),
            tabs,
            bounds: Rect::ZERO,
            hovered_tab: None,
            bar_height: 26.0,
            tab_min_width: 80.0,
        }
    }

    pub fn set_tabs(&mut self, tabs: Vec<TabInfo>) {
        self.tabs = tabs;
    }

    pub fn active_index(&self) -> usize {
        self.tabs.iter().position(|t| t.active).unwrap_or(0)
    }

    pub fn set_active(&mut self, index: usize) {
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            tab.active = i == index;
        }
    }

    fn tab_rects(&self) -> Vec<Rect> {
        let n = self.tabs.len().max(1);
        let tab_w = (self.bounds.width / n as f32).max(self.tab_min_width);
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, _)| {
                Rect::new(
                    self.bounds.x + i as f32 * tab_w,
                    self.bounds.y,
                    tab_w,
                    self.bar_height,
                )
            })
            .collect()
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

    fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                let rects = self.tab_rects();
                for (i, r) in rects.iter().enumerate() {
                    if r.contains(*position) {
                        self.set_active(i);
                        return EventResult::Handled;
                    }
                }
            }
            UiEvent::MouseMove { position, .. } => {
                let rects = self.tab_rects();
                self.hovered_tab = rects.iter().position(|r| r.contains(*position));
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
        ctx.encoder.draw_rect(bg, tokens.card, 0.0);

        let rects = self.tab_rects();
        for (i, tab) in self.tabs.iter().enumerate() {
            if i >= rects.len() {
                break;
            }
            let r = rects[i];
            let is_active = tab.active;
            let is_hovered = self.hovered_tab == Some(i);

            let fill = if is_active {
                tokens.popover
            } else if is_hovered {
                tokens.accent
            } else {
                tokens.card
            };

            let inset = r.inset(2.0, 2.0);
            ctx.encoder.draw_rect(inset, fill, spacing.radius_sm);

            if !tab.label.is_empty() {
                let font_size = ctx.theme.typography.tab_label.font_size;
                let tx = centered_text_x(inset.x, inset.width, &tab.label, font_size);
                let ty = inset.y + (inset.height - font_size * 1.3).max(0.0) * 0.5;
                let pos = mondrian_ui_core::types::snap_point(Point::new(tx, ty));
                ctx.push_clip(inset);
                ctx.encoder.draw_text(&tab.label, font_size, pos, tokens.foreground);
                ctx.pop_clip();
            }

            if is_active {
                let indicator = Rect::new(
                    inset.x + 4.0,
                    inset.y + inset.height - 2.0,
                    inset.width - 8.0,
                    2.0,
                );
                ctx.encoder.draw_rect(indicator, tokens.primary, 0.0);
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
            TabInfo { label: "A".into(), active: active == 0 },
            TabInfo { label: "B".into(), active: active == 1 },
            TabInfo { label: "C".into(), active: active == 2 },
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
        bar.set_tabs(vec![TabInfo { label: "X".into(), active: true }]);
        assert_eq!(bar.tabs.len(), 1);
        assert_eq!(bar.active_index(), 0);
    }

    #[test]
    fn tab_bar_no_active_returns_zero() {
        let bar = DockTabBar::new(vec![TabInfo { label: "X".into(), active: false }]);
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

        // Click at x=140 which should be in the second tab (each tab ~100px)
        let r = bar.event(
            &UiEvent::MouseDown {
                position: Point::new(140.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Handled);
        assert_eq!(bar.active_index(), 1);
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
                position: Point::new(50.0, 15.0),
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
            },
            TabInfo { label: "Second".into(), active: false },
        ]);
        bar.layout(Rect::new(0.0, 0.0, 160.0, 26.0));
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
                Rect::new(2.0, 2.0, 76.0, 22.0),
                Rect::new(82.0, 2.0, 76.0, 22.0)
            ]
        );
        assert_eq!(encoder.clip_pops, 2);
    }
}
