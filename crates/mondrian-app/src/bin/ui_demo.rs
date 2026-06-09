#![allow(deprecated)]
//! UI Element Gallery — 独立 wgpu 窗口，全面测试所有 UI 控件
//!
//! 运行: cargo run --bin ui_demo
//!
//! 包含的 Widget 类型：
//!   Core: ColoredBox
//!   Interactive: Button, Checkbox, TextInput, Slider, List, Dropdown, ContextMenu
//!   Layout: DockSplitter, DockTabBar, PanelSlot, ScrollView

use std::cell::Cell;
use std::cell::RefCell;
use std::sync::Arc;

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::tooltip::TooltipState;
use mondrian_ui_core::tree::WidgetTreeView;
use mondrian_ui_core::types::{self as ui_types, *};
use mondrian_ui_core::widget::{EventContext, ImeRequest, PaintContext};
use mondrian_ui_core::widgets::ColoredBox;
use mondrian_ui_core::{EventResult, TreeWalker, Widget};
use mondrian_ui_events::EventRouter;
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;
use mondrian_ui_text::{resolve_text_commands, TextRenderer};
use mondrian_ui_tooltip::TooltipWidget;
use mondrian_ui_widgets::button::Button;
use mondrian_ui_widgets::checkbox::Checkbox;
use mondrian_ui_widgets::context_menu::ContextMenu;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::dock_tab_bar::{DockTabBar, TabInfo};
use mondrian_ui_widgets::list::{List, ListItem};
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use mondrian_ui_widgets::panel_slot::{PanelSlot, SlotKind};
use mondrian_ui_widgets::scroll::ScrollView;
use mondrian_ui_widgets::slider::Slider;
use mondrian_ui_widgets::text_input::TextInput;

fn demo_action(name: &str) -> Action {
    Action::Custom {
        namespace: "demo".into(),
        name: name.into(),
        payload: serde_json::Value::Null,
    }
}

thread_local! {
    static TEXT_INPUT_BOUNDS: Cell<Option<Rect>> = const { Cell::new(None) };
    static TEXT_INPUT_ID: Cell<Option<WidgetId>> = const { Cell::new(None) };
    static DEMO_LAST_ACTION: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn record_demo_action(action: Action) {
    DEMO_LAST_ACTION.with(|last| {
        *last.borrow_mut() = Some(format!("{action:?}"));
    });
}

fn take_demo_action() -> Option<String> {
    DEMO_LAST_ACTION.with(|last| last.borrow_mut().take())
}

// ═══════════════════════════════════════════════════════════════════════════
// Gallery Widget
// ═══════════════════════════════════════════════════════════════════════════

struct GalleryWidget {
    id: WidgetId,
    bounds: Rect,
    button_click: Button,
    button_no_action: Button,
    checkbox_a: Checkbox,
    checkbox_b: Checkbox,
    text_input: TextInput,
    slider: Slider,
    dropdown: Dropdown,
    list: List,
    scroll_area: ScrollView,
    tooltip_trigger: Rect,
    tooltip: TooltipWidget,
    tooltip_active: bool,
    context_menu: Option<ContextMenu>,
    last_action: String,
    last_slider_value: f32,
    /// Index of the child that captured the mouse (during drag/selection)
    captured: Option<usize>,
}

impl GalleryWidget {
    fn new() -> Self {
        let dropdown_items = vec![
            MenuItem::new("选项 Alpha", demo_action("alpha")),
            MenuItem::new("选项 Beta", demo_action("beta")),
            MenuItem::new("选项 Gamma", demo_action("gamma")),
            MenuItem::new("选项 Delta", demo_action("delta")),
            MenuItem::new("选项 Epsilon", demo_action("epsilon")),
            MenuItem::new("选项 Zeta", demo_action("zeta")),
            MenuItem::new("选项 Eta", demo_action("eta")),
            MenuItem::new("选项 Theta", demo_action("theta")),
            MenuItem::new("选项 Iota (禁用)", demo_action("iota")).disabled(),
            MenuItem::new("选项 Kappa", demo_action("kappa")),
        ];
        let list_items = vec![
            ListItem::new("列表项 1").with_action(demo_action("item1")),
            ListItem::new("列表项 2").with_action(demo_action("item2")),
            ListItem::new("列表项 3"),
            ListItem::new("列表项 4").with_action(demo_action("item4")),
            ListItem::new("列表项 5"),
            ListItem::new("列表项 6"),
            ListItem::new("列表项 7"),
        ];

        let scroll_content = ColoredBox::new(Color::from_hex(0x2A2A4A), 180.0, 500.0);

        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            button_click: Button::new("点击派发 Play").on_click(Action::Play),
            button_no_action: Button::new("无动作按钮"),
            checkbox_a: Checkbox::new("启用特性 A", false).on_toggle(demo_action("toggle_a")),
            checkbox_b: Checkbox::new("启用特性 B", true),
            text_input: TextInput::new("输入文本..."),
            slider: Slider::new(50.0, 0.0, 100.0),
            dropdown: Dropdown::new("选择选项", dropdown_items).with_max_visible_items(5),
            list: List::new(list_items),
            scroll_area: ScrollView::new(Some(Box::new(scroll_content))),
            tooltip_trigger: Rect::ZERO,
            tooltip: TooltipWidget::new(),
            tooltip_active: false,
            context_menu: None,
            last_action: String::new(),
            last_slider_value: 50.0,
            captured: None,
        }
    }

    fn update_hover_tooltip(&mut self, position: Point) {
        if self.tooltip_trigger.contains(position) {
            if !self.tooltip_active {
                self.tooltip_active = true;
                self.tooltip.update_state(TooltipState {
                    text: "Tooltip 示例：用于检查悬停提示、边界夹紧和文字绘制。".into(),
                    position: Point::new(
                        self.tooltip_trigger.x,
                        self.tooltip_trigger.y + self.tooltip_trigger.height,
                    ),
                    visible: true,
                });
            }
        } else {
            self.tooltip_active = false;
            self.tooltip.clear();
        }
    }

    fn sync_child_feedback(&mut self) {
        if let Some(action) = take_demo_action() {
            self.last_action = action;
        }
        let slider_value = self.slider.value();
        if (slider_value - self.last_slider_value).abs() > 0.05 {
            self.last_slider_value = slider_value;
            self.last_action = format!("slider={slider_value:.1}");
        }
    }
}

impl Widget for GalleryWidget {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _c: LayoutConstraint) -> Size {
        Size::new(400.0, 600.0)
    }

    fn layout(&mut self, b: Rect) {
        self.bounds = b;
        let x0 = b.x + 12.0;
        let col_w = b.width - 24.0;
        let row_h = 30.0;
        let gap = 6.0;
        let mut y = b.y + 48.0;

        let half_w = (col_w - 8.0).max(1.0) * 0.5;
        self.button_click.layout(Rect::new(x0, y, half_w, row_h));
        self.button_no_action.layout(Rect::new(x0 + half_w + 8.0, y, half_w, row_h));
        y += row_h + gap;

        self.checkbox_a.layout(Rect::new(x0, y, col_w * 0.5, row_h));
        self.checkbox_b.layout(Rect::new(x0 + col_w * 0.5, y, col_w * 0.5, row_h));
        y += row_h + gap;

        self.text_input.layout(Rect::new(x0, y, col_w, row_h));
        TEXT_INPUT_BOUNDS.with(|b| b.set(Some(Rect::new(x0, y, col_w, row_h))));
        TEXT_INPUT_ID.with(|id| id.set(Some(self.text_input.id())));
        y += row_h + gap;

        self.slider.layout(Rect::new(x0, y, col_w, row_h));
        y += row_h + gap;

        let dropdown_w = col_w.clamp(1.0, 160.0);
        self.dropdown.layout(Rect::new(x0, y, dropdown_w, row_h));
        if col_w >= 260.0 {
            self.tooltip_trigger = Rect::new(x0 + 172.0, y, (col_w - 172.0).max(0.0), row_h);
            y += row_h + 12.0;
        } else {
            y += row_h + gap;
            self.tooltip_trigger = Rect::new(x0, y, col_w.max(1.0), row_h);
            y += row_h + 12.0;
        }

        let list_h = 5.0 * 28.0;
        self.list.layout(Rect::new(x0, y, col_w * 0.55, list_h));
        self.scroll_area.layout(Rect::new(
            x0 + col_w * 0.55 + 8.0,
            y,
            col_w * 0.45 - 8.0,
            list_h,
        ));

        if let Some(ref mut cm) = &mut self.context_menu {
            let cm_size = cm.measure(LayoutConstraint::LOOSE);
            cm.layout(Rect::new(0.0, 0.0, cm_size.width, cm_size.height));
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let UiEvent::MouseMove { position, .. } = event {
            self.update_hover_tooltip(*position);
        }

        // Context menu always gets first dibs
        if let Some(ref mut cm) = &mut self.context_menu {
            if cm.event(event, ctx) == EventResult::Handled {
                if !cm.is_visible() {
                    self.context_menu = None;
                }
                return EventResult::Handled;
            }
        }

        let last_action: std::cell::RefCell<String> = std::cell::RefCell::new(String::new());
        let inner_ctx = &mut EventContext {
            focus: ctx.focus,
            shortcut: ctx.shortcut,
            tooltip: ctx.tooltip,
            dispatch: &|a: Action| {
                *last_action.borrow_mut() = format!("{a:?}");
            },
            platform: ctx.platform,
            requests: ctx.requests,
        };

        // If a child captured the mouse (drag/selection in progress), route
        // MouseMove and MouseUp to it first. MouseUp clears the capture.
        // On MouseDown: if captured child ignores it, fall through to normal
        // dispatch so other widgets can receive the click.
        if let Some(idx) = self.captured {
            let handled = match idx {
                0 => self.button_click.event(event, inner_ctx),
                1 => self.button_no_action.event(event, inner_ctx),
                2 => self.checkbox_a.event(event, inner_ctx),
                3 => self.checkbox_b.event(event, inner_ctx),
                4 => self.text_input.event(event, inner_ctx),
                5 => self.slider.event(event, inner_ctx),
                6 => self.dropdown.event(event, inner_ctx),
                7 => self.list.event(event, inner_ctx),
                8 => self.scroll_area.event(event, inner_ctx),
                _ => EventResult::Ignored,
            };
            if matches!(event, UiEvent::MouseUp { .. }) {
                self.captured = None;
            }
            // MouseDown during capture: if captured child doesn't handle it,
            // let the click go to the actual target.
            if matches!(event, UiEvent::MouseDown { .. }) && handled == EventResult::Ignored {
                self.captured = None;
                // fall through to normal dispatch below
            } else {
                if handled == EventResult::Handled {
                    if idx == 5 {
                        self.last_action = format!("slider={:.1}", self.slider.value());
                        self.last_slider_value = self.slider.value();
                    } else {
                        self.last_action = last_action.into_inner();
                    }
                    return EventResult::Handled;
                }
                return EventResult::Ignored;
            }
        }

        // Normal event dispatch. On MouseDown, record which child captures.
        let mut handled = EventResult::Ignored;
        let mut children: [(&mut dyn Widget, usize, bool); 9] = [
            (&mut self.button_click, 0, true),
            (&mut self.button_no_action, 1, false),
            (&mut self.checkbox_a, 2, true),
            (&mut self.checkbox_b, 3, true),
            (&mut self.text_input, 4, false),
            (&mut self.slider, 5, false),
            (&mut self.dropdown, 6, true),
            (&mut self.list, 7, true),
            (&mut self.scroll_area, 8, false),
        ];
        for (child, idx, show_action) in &mut children {
            if child.event(event, inner_ctx) == EventResult::Handled {
                if matches!(event, UiEvent::MouseDown { .. }) {
                    self.captured = Some(*idx);
                }
                if *show_action {
                    self.last_action = last_action.into_inner();
                }
                if *idx == 5 {
                    self.last_action = format!("slider={:.1}", self.slider.value());
                    self.last_slider_value = self.slider.value();
                }
                handled = EventResult::Handled;
                break;
            }
        }
        if handled == EventResult::Handled {
            return EventResult::Handled;
        }

        // Right-click opens context menu
        if let UiEvent::MouseDown { position, button: MouseButton::Right, .. } = event {
            if self.bounds.contains(*position) {
                let items = vec![
                    MenuItem::new("剪切", Action::Cut),
                    MenuItem::new("复制", Action::Copy),
                    MenuItem::new("粘贴", Action::Paste),
                    MenuItem::new("————", demo_action("sep")).disabled(),
                    MenuItem::new("删除", demo_action("delete")),
                ];
                self.context_menu = Some(ContextMenu::new(*position, items));
                return EventResult::Handled;
            }
        }

        EventResult::Ignored
    }

    fn after_child_event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        self.sync_child_feedback();
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;

        ctx.encoder.draw_rect(self.bounds, tokens.card, ctx.theme.spacing.radius_md);

        self.button_click.paint(ctx);
        self.button_no_action.paint(ctx);
        self.checkbox_a.paint(ctx);
        self.checkbox_b.paint(ctx);
        self.text_input.paint(ctx);
        self.slider.paint(ctx);
        self.dropdown.paint(ctx);
        ctx.encoder.draw_rect(
            self.tooltip_trigger,
            tokens.secondary,
            ctx.theme.spacing.radius_sm,
        );
        ctx.encoder.draw_text(
            "悬停 Tooltip",
            12.0,
            Point::new(self.tooltip_trigger.x + 8.0, self.tooltip_trigger.y + 7.0),
            tokens.secondary_foreground,
        );
        self.list.paint(ctx);
        self.scroll_area.paint(ctx);
        if let Some(ref cm) = &self.context_menu {
            cm.paint(ctx);
        }
        self.tooltip.paint(ctx);

        let p = ui_types::snap_point(Point::new(self.bounds.x + 12.0, self.bounds.y + 6.0));
        ctx.encoder
            .draw_text("UI 控件画廊 — 右键可打开菜单", 15.0, p, tokens.foreground);

        if !self.last_action.is_empty() {
            let fb = format!("最后操作: {}", self.last_action);
            let p = ui_types::snap_point(Point::new(self.bounds.x + 12.0, self.bounds.y + 24.0));
            ctx.encoder.draw_text(&fb, 12.0, p, tokens.primary);
        }
    }

    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }

    fn child_count(&self) -> usize {
        9 + usize::from(self.context_menu.is_some())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.button_click),
            1 => Some(&self.button_no_action),
            2 => Some(&self.checkbox_a),
            3 => Some(&self.checkbox_b),
            4 => Some(&self.text_input),
            5 => Some(&self.slider),
            6 => Some(&self.dropdown),
            7 => Some(&self.list),
            8 => Some(&self.scroll_area),
            9 => self.context_menu.as_ref().map(|menu| menu as &dyn Widget),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.button_click),
            1 => Some(&mut self.button_no_action),
            2 => Some(&mut self.checkbox_a),
            3 => Some(&mut self.checkbox_b),
            4 => Some(&mut self.text_input),
            5 => Some(&mut self.slider),
            6 => Some(&mut self.dropdown),
            7 => Some(&mut self.list),
            8 => Some(&mut self.scroll_area),
            9 => self.context_menu.as_mut().map(|menu| menu as &mut dyn Widget),
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Viewer diagnostic widget (text rendering test)
// ═══════════════════════════════════════════════════════════════════════════

struct ViewerWidget {
    id: WidgetId,
    bounds: Rect,
}

impl ViewerWidget {
    fn new() -> Self {
        Self { id: WidgetId::new(), bounds: Rect::ZERO }
    }
}

impl Widget for ViewerWidget {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, _c: LayoutConstraint) -> Size {
        Size::new(800.0, 400.0)
    }
    fn layout(&mut self, b: Rect) {
        self.bounds = b;
    }
    fn event(&mut self, _e: &UiEvent, _c: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }
    fn paint(&self, ctx: &mut PaintContext) {
        let bg = Color::from_hex(0x1A1A2E);
        ctx.encoder.draw_rect(self.bounds, bg, 0.0);
        let c = Color::from_hex(0xEBEBF0);

        let text = "Mondrian 自研 UI 框架 — 文本渲染测试";
        let sizes = [11.0, 12.0, 13.0, 14.0, 16.0, 20.0, 24.0];
        let base_x = (self.bounds.x + 10.0).round();
        let mut y = (self.bounds.y + 10.0).round();
        for &fs in &sizes {
            ctx.encoder.draw_text(text, fs, Point::new(base_x, y), c);
            y += (fs * 1.5 + 4.0).round();
        }

        y += 20.0;
        ctx.encoder.draw_text(
            "子像素定位测试 (l 字符):",
            13.0,
            Point::new(base_x, y),
            Color::from_hex(0x888899),
        );
        y += 20.0;
        // Subpixel test: intentionally fractional x positions
        for &fs in &[13.0, 14.0, 16.0] {
            for i in 0..5 {
                let px = self.bounds.x + 10.0 + i as f32 * 0.33;
                ctx.encoder.draw_text("l", fs, Point::new(px, y), c);
            }
            y += (fs * 1.5 + 8.0).round();
        }

        y += 10.0;
        ctx.encoder.draw_text(
            "中文字符测试：你好世界！これは日本語です。",
            14.0,
            Point::new(base_x, y),
            Color::from_hex(0xAABBCC),
        );
    }
    fn hit_test(&self, _p: Point) -> bool {
        false
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// VerticalTabbedSlot
// ═══════════════════════════════════════════════════════════════════════════

struct VerticalTabbedSlot {
    id: WidgetId,
    tab_bar: DockTabBar,
    kind: SlotKind,
    content: Box<dyn Widget>,
    bounds: Rect,
    last_active: usize,
}

impl VerticalTabbedSlot {
    fn new(kind: SlotKind) -> Self {
        let tabs = tab_infos(kind);
        let active = tabs.iter().position(|t| t.active).unwrap_or(0);
        let tab_bar = DockTabBar::new(tabs);
        let content = PanelSlot::new(kind, slot_content_for_tab(kind, active));
        Self {
            id: WidgetId::new(),
            tab_bar,
            kind,
            content: Box::new(content),
            bounds: Rect::ZERO,
            last_active: active,
        }
    }

    fn sync_active_tab(&mut self) {
        let now_active = self.tab_bar.active_index();
        if now_active != self.last_active {
            self.last_active = now_active;
            self.content = Box::new(PanelSlot::new(
                self.kind,
                slot_content_for_tab(self.kind, now_active),
            ));
            let bounds = self.bounds;
            self.layout(bounds);
        }
    }
}

impl Widget for VerticalTabbedSlot {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, c: LayoutConstraint) -> Size {
        c.constrain(Size::new(100.0, 100.0))
    }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let tab_h = 26.0;
        self.tab_bar.layout(Rect::new(bounds.x, bounds.y, bounds.width, tab_h));
        self.content.layout(Rect::new(
            bounds.x,
            bounds.y + tab_h,
            bounds.width,
            (bounds.height - tab_h).max(0.0),
        ));
    }
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        let result = self.tab_bar.event(event, ctx);
        self.sync_active_tab();
        if result == EventResult::Handled {
            return EventResult::Handled;
        }
        self.content.event(event, ctx)
    }

    fn after_child_event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        self.sync_active_tab();
        EventResult::Ignored
    }
    fn paint(&self, ctx: &mut PaintContext) {
        self.tab_bar.paint(ctx);
        self.content.paint(ctx);
    }
    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }

    fn child_count(&self) -> usize {
        2
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.tab_bar),
            1 => Some(self.content.as_ref()),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.tab_bar),
            1 => Some(self.content.as_mut()),
            _ => None,
        }
    }
}

fn tab_infos(kind: SlotKind) -> Vec<TabInfo> {
    match kind {
        SlotKind::Timeline => vec![
            TabInfo { label: "时间线".into(), active: true },
            TabInfo { label: "音频".into(), active: false },
            TabInfo { label: "效果".into(), active: false },
        ],

        SlotKind::Inspector => vec![
            TabInfo { label: "检查器".into(), active: true },
            TabInfo { label: "属性".into(), active: false },
        ],
        SlotKind::Console => vec![
            TabInfo { label: "控件".into(), active: true },
            TabInfo { label: "文本".into(), active: false },
            TabInfo { label: "形状".into(), active: false },
        ],
        SlotKind::Assets => vec![
            TabInfo { label: "资源".into(), active: true },
            TabInfo { label: "库".into(), active: false },
        ],
        _ => vec![TabInfo { label: kind.display_name().into(), active: true }],
    }
}

fn slot_content_for_tab(kind: SlotKind, tab_index: usize) -> Box<dyn Widget> {
    match kind {
        SlotKind::Viewer => Box::new(ViewerWidget::new()),
        SlotKind::Console => match tab_index {
            0 => Box::new(GalleryWidget::new()),
            1 => Box::new(ViewerWidget::new()),
            _ => Box::new(ShapePanelWidget::new()),
        },
        SlotKind::Assets => match tab_index {
            0 => Box::new(
                ColoredBox::new(Color::from_hex(0xCC2222), 1.0, 1.0).with_label("资源面板"),
            ),
            _ => {
                Box::new(ColoredBox::new(Color::from_hex(0x22CC22), 1.0, 1.0).with_label("素材库"))
            }
        },
        SlotKind::Inspector => match tab_index {
            0 => {
                Box::new(ColoredBox::new(Color::from_hex(0x1E2A3A), 1.0, 1.0).with_label("检查器"))
            }
            _ => Box::new(
                ColoredBox::new(Color::from_hex(0x2A1E3A), 1.0, 1.0).with_label("属性面板"),
            ),
        },
        SlotKind::Timeline => match tab_index {
            0 => {
                Box::new(ColoredBox::new(Color::from_hex(0x16213E), 1.0, 1.0).with_label("时间线"))
            }
            1 => Box::new(
                ColoredBox::new(Color::from_hex(0x1E3A16), 1.0, 1.0).with_label("音频轨道"),
            ),
            _ => Box::new(
                ColoredBox::new(Color::from_hex(0x3A1621), 1.0, 1.0).with_label("效果面板"),
            ),
        },
        _ => Box::new(ColoredBox::new(Color::from_hex(0x1A1A1A), 1.0, 1.0)),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dock tree
// ═══════════════════════════════════════════════════════════════════════════

fn build_dock_tree() -> DockSplitter {
    // Top-left: Assets=red (35%) / Bottom-left: Gallery=Console (65%)
    let left = DockSplitter::new(
        SplitDirection::Vertical,
        0.35,
        Box::new(VerticalTabbedSlot::new(SlotKind::Assets)),
        Box::new(VerticalTabbedSlot::new(SlotKind::Console)),
    );

    let right_bottom = DockSplitter::new(
        SplitDirection::Horizontal,
        0.65,
        Box::new(VerticalTabbedSlot::new(SlotKind::Timeline)),
        Box::new(VerticalTabbedSlot::new(SlotKind::Inspector)),
    );

    let right = DockSplitter::new(
        SplitDirection::Vertical,
        0.5,
        Box::new(VerticalTabbedSlot::new(SlotKind::Viewer)),
        Box::new(right_bottom),
    );

    DockSplitter::new(
        SplitDirection::Horizontal,
        0.3,
        Box::new(left),
        Box::new(right),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn mouse_button(b: winit::event::MouseButton) -> MouseButton {
    match b {
        winit::event::MouseButton::Left => MouseButton::Left,
        winit::event::MouseButton::Right => MouseButton::Right,
        winit::event::MouseButton::Middle => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn winit_key_to_keycode(key: &winit::keyboard::Key) -> Option<KeyCode> {
    use winit::keyboard::{Key, NamedKey};
    match key {
        Key::Named(named) => match named {
            NamedKey::Backspace => Some(KeyCode::Backspace),
            NamedKey::Delete => Some(KeyCode::Delete),
            NamedKey::ArrowLeft => Some(KeyCode::Left),
            NamedKey::ArrowRight => Some(KeyCode::Right),
            NamedKey::ArrowUp => Some(KeyCode::Up),
            NamedKey::ArrowDown => Some(KeyCode::Down),
            NamedKey::Home => Some(KeyCode::Home),
            NamedKey::End => Some(KeyCode::End),
            NamedKey::Enter => Some(KeyCode::Enter),
            NamedKey::Space => Some(KeyCode::Space),
            NamedKey::Tab => Some(KeyCode::Tab),
            NamedKey::Escape => None, // handled separately
            _ => None,
        },
        Key::Character(ch) => match ch.as_str() {
            "a" => Some(KeyCode::A),
            "b" => Some(KeyCode::B),
            "c" => Some(KeyCode::C),
            "d" => Some(KeyCode::D),
            "e" => Some(KeyCode::E),
            "f" => Some(KeyCode::F),
            "g" => Some(KeyCode::G),
            "h" => Some(KeyCode::H),
            "i" => Some(KeyCode::I),
            "j" => Some(KeyCode::J),
            "k" => Some(KeyCode::K),
            "l" => Some(KeyCode::L),
            "m" => Some(KeyCode::M),
            "n" => Some(KeyCode::N),
            "o" => Some(KeyCode::O),
            "p" => Some(KeyCode::P),
            "q" => Some(KeyCode::Q),
            "r" => Some(KeyCode::R),
            "s" => Some(KeyCode::S),
            "t" => Some(KeyCode::T),
            "u" => Some(KeyCode::U),
            "v" => Some(KeyCode::V),
            "w" => Some(KeyCode::W),
            "x" => Some(KeyCode::X),
            "y" => Some(KeyCode::Y),
            "z" => Some(KeyCode::Z),
            "0" => Some(KeyCode::Digit0),
            "1" => Some(KeyCode::Digit1),
            "2" => Some(KeyCode::Digit2),
            "3" => Some(KeyCode::Digit3),
            "4" => Some(KeyCode::Digit4),
            "5" => Some(KeyCode::Digit5),
            "6" => Some(KeyCode::Digit6),
            "7" => Some(KeyCode::Digit7),
            "8" => Some(KeyCode::Digit8),
            "9" => Some(KeyCode::Digit9),
            _ => None,
        },
        _ => None,
    }
}

fn route_demo_event(
    router: &mut EventRouter,
    root: &mut dyn Widget,
    event: UiEvent,
) -> EventResult {
    let mut tree = WidgetTreeView::new(root);
    router.route(event, &mut tree, &record_demo_action)
}

fn route_demo_window_event(
    window: &winit::window::Window,
    router: &mut EventRouter,
    root: &mut dyn Widget,
    event: UiEvent,
) -> EventResult {
    let result = route_demo_event(router, root, event);
    if let Some(request) = router.take_ime_request() {
        apply_ime_request(window, request);
    }
    result
}

fn apply_ime_request(window: &winit::window::Window, request: ImeRequest) {
    window.set_ime_allowed(request.enabled);
    if request.enabled {
        if let Some(area) = request.cursor_area {
            window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(area.x as f64, area.y as f64),
                winit::dpi::PhysicalSize::new(
                    area.width.max(1.0) as u32,
                    area.height.max(1.0) as u32,
                ),
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use winit::event_loop::EventLoop;

    let event_loop = EventLoop::new()?;
    let window_attrs = winit::window::Window::default_attributes()
        .with_title("Mondrian UI — 控件画廊 (Gallery)")
        .with_inner_size(winit::dpi::LogicalSize::new(1440, 860));

    let window = Arc::new(event_loop.create_window(window_attrs)?);

    let instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    let instance = wgpu::Instance::new(instance_desc);
    let surface = instance.create_surface(window.clone())?;

    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    })) {
        Ok(a) => a,
        Err(_) => return Err("No suitable GPU adapter".into()),
    };

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;

    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width, size.height)
        .ok_or("Failed surface config")?;
    surface.configure(&device, &config);

    let ui_renderer = UiRenderer::new(&device, config.format);
    let mut text_renderer = TextRenderer::new();

    let mut root = build_dock_tree();
    let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    TreeWalker::layout(&mut root, bounds);
    let mut router = EventRouter::with_platform(
        root.id(),
        Box::new(mondrian_platform::SystemPlatformService),
    );

    let mut last_cursor = Point::new(0.0, 0.0);
    let current_bounds = std::cell::Cell::new(bounds);
    let mut modifiers_state = Modifiers::none();

    event_loop.run(move |event, elwt| {
        use winit::event::ElementState;
        use winit::event::Event;
        use winit::event::WindowEvent;
        use winit::event_loop::ControlFlow;
        elwt.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => elwt.exit(),

            Event::WindowEvent {
                event:
                    WindowEvent::KeyboardInput {
                        event:
                            winit::event::KeyEvent {
                                logical_key:
                                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                                state: ElementState::Pressed,
                                ..
                            },
                        ..
                    },
                ..
            } => elwt.exit(),

            // Keyboard input → dispatch KeyDown / TextInput to widget tree
            Event::WindowEvent {
                event:
                    WindowEvent::KeyboardInput {
                        event: winit::event::KeyEvent { logical_key, state, text, .. },
                        ..
                    },
                ..
            } => {
                // Track modifier key state
                let pressed = state == ElementState::Pressed;
                match &logical_key {
                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Control) => {
                        modifiers_state.ctrl = pressed;
                    }
                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Alt) => {
                        modifiers_state.alt = pressed;
                    }
                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Shift) => {
                        modifiers_state.shift = pressed;
                    }
                    _ => {}
                }

                if pressed {
                    if let Some(kc) = winit_key_to_keycode(&logical_key) {
                        let _ = route_demo_window_event(
                            &window,
                            &mut router,
                            &mut root,
                            UiEvent::KeyDown { key: kc, modifiers: modifiers_state },
                        );
                    }
                    // Only send TextInput for printable characters when Ctrl is NOT held
                    if !modifiers_state.ctrl {
                        if let Some(txt) = text {
                            if !txt.is_empty() && !txt.chars().any(|c| c.is_control()) {
                                let _ = route_demo_window_event(
                                    &window,
                                    &mut router,
                                    &mut root,
                                    UiEvent::TextInput(txt.to_string()),
                                );
                            }
                        }
                    }
                }
                window.request_redraw();
            }

            // IME composition events
            Event::WindowEvent {
                event: WindowEvent::Ime(winit::event::Ime::Commit(text)),
                ..
            } => {
                let _ = route_demo_window_event(
                    &window,
                    &mut router,
                    &mut root,
                    UiEvent::ImeCommit(text),
                );
                window.request_redraw();
            }
            Event::WindowEvent {
                event: WindowEvent::Ime(winit::event::Ime::Preedit(text, _cursor)),
                ..
            } => {
                let _ = route_demo_window_event(
                    &window,
                    &mut router,
                    &mut root,
                    UiEvent::ImePreedit(text),
                );
                window.request_redraw();
            }
            Event::WindowEvent {
                event: WindowEvent::Ime(winit::event::Ime::Enabled),
                ..
            } => {
                // IME enabled — no action needed, just acknowledge
            }
            Event::WindowEvent {
                event: WindowEvent::Ime(winit::event::Ime::Disabled),
                ..
            } => {
                // IME disabled — clear any pending preedit
                let _ = route_demo_window_event(
                    &window,
                    &mut router,
                    &mut root,
                    UiEvent::ImePreedit(String::new()),
                );
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let mut encoder = DrawEncoder::new();
                let theme = mondrian_ui_theme::current_theme();
                let b = current_bounds.get();
                encoder.draw_rect(b, theme.colors.background, 0.0);
                TreeWalker::paint(&root, &mut encoder, &theme);
                let commands = resolve_text_commands(encoder.finish(), &mut text_renderer);
                let pending: Vec<mondrian_ui_renderer::GlyphUpload> = text_renderer
                    .take_pending_uploads()
                    .into_iter()
                    .map(|u| mondrian_ui_renderer::GlyphUpload {
                        x: u.x,
                        y: u.y,
                        width: u.width,
                        height: u.height,
                        data: u.data,
                    })
                    .collect();
                if !pending.is_empty() {
                    ui_renderer.upload_glyphs(&queue, &pending);
                }

                let current = surface.get_current_texture();
                match current {
                    wgpu::CurrentSurfaceTexture::Success(output)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                        let v = output.texture.create_view(&Default::default());
                        let sz = window.inner_size();
                        ui_renderer.render(&device, &queue, &v, &commands, (sz.width, sz.height));
                        output.present();
                    }
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => {}
                    wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                        surface.configure(&device, &config);
                    }
                    _ => {}
                }
            }

            Event::WindowEvent { event: WindowEvent::Resized(new_size), .. } => {
                if new_size.width > 0 && new_size.height > 0 {
                    config.width = new_size.width;
                    config.height = new_size.height;
                    surface.configure(&device, &config);
                    let new_bounds =
                        Rect::new(0.0, 0.0, new_size.width as f32, new_size.height as f32);
                    current_bounds.set(new_bounds);
                    TreeWalker::layout(&mut root, new_bounds);
                    window.request_redraw();
                }
            }

            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. }, ..
            } => {
                last_cursor = Point::new(position.x as f32, position.y as f32);
                let _ = route_demo_window_event(
                    &window,
                    &mut router,
                    &mut root,
                    UiEvent::MouseMove {
                        position: last_cursor,
                        modifiers: Modifiers::none(),
                    },
                );
                let grab_zones = root.collect_grab_zones();
                let direction =
                    grab_zones.iter().find(|(z, _)| z.contains(last_cursor)).map(|(_, d)| *d);
                match direction {
                    Some(SplitDirection::Horizontal) => {
                        window.set_cursor_icon(winit::window::CursorIcon::ColResize);
                    }
                    Some(SplitDirection::Vertical) => {
                        window.set_cursor_icon(winit::window::CursorIcon::RowResize);
                    }
                    None => {
                        let is_text = TEXT_INPUT_BOUNDS
                            .with(|b| b.get().is_some_and(|r| r.contains(last_cursor)))
                            && TEXT_INPUT_ID.with(|id| id.get()) == router.focused();
                        if is_text {
                            window.set_cursor_icon(winit::window::CursorIcon::Text);
                        } else {
                            window.set_cursor_icon(winit::window::CursorIcon::Default);
                        }
                    }
                }
                window.request_redraw();
            }

            Event::WindowEvent {
                event: WindowEvent::MouseInput { state, button, .. },
                ..
            } => {
                let event = match state {
                    ElementState::Pressed => UiEvent::MouseDown {
                        position: last_cursor,
                        button: mouse_button(button),
                        modifiers: Modifiers::none(),
                    },
                    ElementState::Released => UiEvent::MouseUp {
                        position: last_cursor,
                        button: mouse_button(button),
                        modifiers: Modifiers::none(),
                    },
                };
                let _ = route_demo_window_event(&window, &mut router, &mut root, event);
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::MouseWheel { delta, .. }, .. } => {
                let scroll_delta = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => -y * 20.0,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32),
                };
                let _ = route_demo_window_event(
                    &window,
                    &mut router,
                    &mut root,
                    UiEvent::MouseWheel {
                        delta: scroll_delta,
                        position: last_cursor,
                        modifiers: Modifiers::none(),
                    },
                );
                window.request_redraw();
            }

            Event::AboutToWait => {
                window.request_redraw();
            }
            _ => {}
        }
    })?;

    Ok(())
}

// ── Shape Panel Widget ────────────────────────────────────────────────────────
/// 形状面板：在 Console → 形状 标签页中绘制 3 个圆形（大/中/小），
/// 中号圆形不绘制背景矩形。
struct ShapePanelWidget {
    id: WidgetId,
    bounds: Rect,
}

impl ShapePanelWidget {
    fn new() -> Self {
        Self { id: WidgetId::new(), bounds: Rect::ZERO }
    }
}

impl Widget for ShapePanelWidget {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, _c: LayoutConstraint) -> Size {
        Size::new(500.0, 300.0)
    }
    fn layout(&mut self, b: Rect) {
        self.bounds = b;
    }
    fn event(&mut self, _e: &UiEvent, _c: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }
    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let anchor_bg = Color::from_hex(0x3A3A5A);
        // Snap base origin to integer pixel to eliminate subpixel jitter from
        // dock-splitter fractional layout.
        let bx = self.bounds.x.round();
        let by = self.bounds.y.round();

        // 大圆 — 200x200，有背景矩形 + 四边定位标记 + 圆心
        let large_size = 200.0;
        let large_x = bx + 30.0;
        let large_y = by + 40.0;
        let large_color = Color { r: 0.94, g: 0.27, b: 0.27, a: 0.65 };
        ctx.encoder.draw_rect(
            Rect::new(large_x, large_y, large_size, large_size),
            anchor_bg,
            0.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(large_x, large_y, large_size, large_size),
            large_color,
            large_size * 0.5,
        );
        // 圆心 + 四边中点标记
        let cx = large_x + large_size * 0.5;
        let cy = large_y + large_size * 0.5;
        let marker = Color::WHITE;
        let ms = 4.0; // 标记半宽
        ctx.encoder
            .draw_rect(Rect::new(cx - ms, cy - ms, ms * 2.0, ms * 2.0), marker, 0.0);
        ctx.encoder.draw_rect(
            Rect::new(cx - ms, large_y - 8.0, ms * 2.0, 8.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(cx - ms, large_y + large_size, ms * 2.0, 8.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(large_x - 8.0, cy - ms, 8.0, ms * 2.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(large_x + large_size, cy - ms, 8.0, ms * 2.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_text(
            "r=100 大圆",
            11.0,
            ui_types::snap_point(Point::new(large_x, large_y + large_size + 14.0)),
            tokens.foreground,
        );

        // 中圆 — 80x80，无背景矩形
        let med_size = 80.0;
        let med_x = large_x + large_size + 40.0;
        let med_y = large_y + 100.0;
        let med_color = Color { r: 0.13, g: 0.77, b: 0.37, a: 0.65 };
        ctx.encoder.draw_rect(
            Rect::new(med_x, med_y, med_size, med_size),
            med_color,
            med_size * 0.5,
        );
        let mcx = med_x + med_size * 0.5;
        let mcy = med_y + med_size * 0.5;
        ctx.encoder.draw_rect(
            Rect::new(mcx - ms, mcy - ms, ms * 2.0, ms * 2.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_text(
            "r=40 中圆",
            11.0,
            ui_types::snap_point(Point::new(med_x, med_y + med_size + 14.0)),
            tokens.foreground,
        );

        // 小圆 — 24x24，有背景矩形
        let sm_size = 24.0;
        let sm_x = med_x + med_size + 40.0;
        let sm_y = med_y + 40.0;
        let sm_color = Color { r: 0.23, g: 0.51, b: 0.96, a: 0.75 };
        ctx.encoder.draw_rect(Rect::new(sm_x, sm_y, sm_size, sm_size), anchor_bg, 0.0);
        ctx.encoder.draw_rect(
            Rect::new(sm_x, sm_y, sm_size, sm_size),
            sm_color,
            sm_size * 0.5,
        );
        let scx = sm_x + sm_size * 0.5;
        let scy = sm_y + sm_size * 0.5;
        ctx.encoder.draw_rect(
            Rect::new(scx - ms, scy - ms, ms * 2.0, ms * 2.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_text(
            "r=12 小圆",
            11.0,
            ui_types::snap_point(Point::new(sm_x, sm_y + sm_size + 14.0)),
            tokens.foreground,
        );

        // 胶囊形 — 60x120, r=30，有背景矩形
        let cap_w = 60.0;
        let cap_h = 120.0;
        let cap_r = cap_w * 0.5;
        let cap_x = large_x;
        let cap_y = large_y + large_size + 40.0;
        let cap_color = Color { r: 0.70, g: 0.30, b: 1.00, a: 0.75 };
        ctx.encoder.draw_rect(Rect::new(cap_x, cap_y, cap_w, cap_h), anchor_bg, 0.0);
        ctx.encoder.draw_rect(Rect::new(cap_x, cap_y, cap_w, cap_h), cap_color, cap_r);
        ctx.encoder.draw_text(
            "r=30 胶囊",
            11.0,
            ui_types::snap_point(Point::new(cap_x, cap_y + cap_h + 14.0)),
            tokens.foreground,
        );
    }
    fn hit_test(&self, _p: Point) -> bool {
        false
    }
}
