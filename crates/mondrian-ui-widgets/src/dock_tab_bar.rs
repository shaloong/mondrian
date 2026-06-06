//! Dock Tab 标签栏
//!
//! 水平排列的标签按钮，点击切换 active tab。

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{DrawCommandEncoder, EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::Theme;

/// 单个 Tab 的信息
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub label: String,
    pub active: bool,
}

/// DockTabBar —— 水平标签栏
pub struct DockTabBar {
    id: WidgetId,
    /// 标签列表
    tabs: Vec<TabInfo>,
    bounds: Rect,
    /// 当前 hover 的 tab 索引
    hovered_tab: Option<usize>,
    /// Tab 栏高度
    bar_height: f32,
    /// 单个 tab 的最小宽度
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

    /// 更新标签列表，保持 active 状态
    pub fn set_tabs(&mut self, tabs: Vec<TabInfo>) {
        self.tabs = tabs;
    }

    /// 获取当前 active 标签的索引
    pub fn active_index(&self) -> usize {
        self.tabs.iter().position(|t| t.active).unwrap_or(0)
    }

    /// 切换 active tab 到指定索引
    pub fn set_active(&mut self, index: usize) {
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            tab.active = i == index;
        }
    }

    /// 计算每个 tab 的屏幕位置
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
    fn id(&self) -> WidgetId { self.id }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        Size::new(constraint.max.width.min(600.0), self.bar_height)
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
                let new_hover = rects.iter().position(|r| r.contains(*position));
                if new_hover != self.hovered_tab {
                    self.hovered_tab = new_hover;
                    return EventResult::Handled;
                }
            }
            _ => {}
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        // 背景条
        let bg = Rect::new(self.bounds.x, self.bounds.y, self.bounds.width, self.bar_height);
        ctx.encoder.draw_rect(bg, tokens.bg_surface, 0.0);

        let rects = self.tab_rects();
        for (i, tab) in self.tabs.iter().enumerate() {
            if i >= rects.len() {
                break;
            }
            let r = rects[i];
            let is_active = tab.active;
            let is_hovered = self.hovered_tab == Some(i);

            let fill = if is_active {
                tokens.bg_surface_raised
            } else if is_hovered {
                tokens.bg_surface_hover
            } else {
                tokens.bg_surface
            };

            let inset = r.inset(2.0, 2.0);
            ctx.encoder.draw_rect(inset, fill, spacing.radius_sm);

            // Active indicator bar at bottom
            if is_active {
                let indicator = Rect::new(
                    inset.x + 4.0,
                    inset.y + inset.height - 2.0,
                    inset.width - 8.0,
                    2.0,
                );
                ctx.encoder.draw_rect(indicator, tokens.interaction_highlight, 0.0);
            }

            // Label text (we don't draw text in paint since TextRenderer isn't in ctx yet;
            // this will be handled by the demo via a separate text pass)
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn children(&self) -> &[Box<dyn Widget>] { &[] }
    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] { &mut [] }
}
