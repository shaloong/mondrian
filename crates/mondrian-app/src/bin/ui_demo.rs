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
use std::sync::Arc;

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{ShortcutBinding, ShortcutManager, ShortcutScope};
use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::{self as ui_types, *};
use mondrian_ui_core::widget::{EventContext, EventRequests, PaintContext};
use mondrian_ui_core::widgets::ColoredBox;
use mondrian_ui_core::{EventResult, TreeWalker, Widget};
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;
use mondrian_ui_text::{resolve_text_commands, TextRenderer};
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
    context_menu: Option<ContextMenu>,
    last_action: String,
    /// Index of the child that captured the mouse (during drag/selection)
    captured: Option<usize>,
}

impl GalleryWidget {
    fn new() -> Self {
        let dropdown_items = vec![
            MenuItem::new("选项 Alpha", demo_action("alpha")),
            MenuItem::new("选项 Beta", demo_action("beta")),
            MenuItem::new("选项 Gamma (禁用)", demo_action("gamma")).disabled(),
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
            dropdown: Dropdown::new("选择选项", dropdown_items),
            list: List::new(list_items),
            scroll_area: ScrollView::new(Some(Box::new(scroll_content))),
            context_menu: None,
            last_action: String::new(),
            captured: None,
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
        y += row_h + gap;

        self.slider.layout(Rect::new(x0, y, col_w, row_h));
        y += row_h + gap;

        self.dropdown.layout(Rect::new(x0, y, 160.0, row_h));
        y += row_h + 12.0;

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
        self.list.paint(ctx);
        self.scroll_area.paint(ctx);
        if let Some(ref cm) = &self.context_menu {
            cm.paint(ctx);
        }

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
            _ => Box::new(
                ColoredBox::new(Color::from_hex(0x2A1A3A), 1.0, 1.0).with_label("形状测试"),
            ),
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

fn dummy_event_ctx() -> EventContext<'static> {
    static mut FOCUS: DummyFocus = DummyFocus;
    static mut SHORTCUT: DummyShortcut = DummyShortcut;
    static mut TOOLTIP: DummyTooltip = DummyTooltip;
    static mut REQUESTS: EventRequests =
        EventRequests { pointer_capture: None, ime: None, repaint: false };
    static PLATFORM: mondrian_platform::SystemPlatformService =
        mondrian_platform::SystemPlatformService;
    unsafe {
        EventContext {
            focus: &mut *std::ptr::addr_of_mut!(FOCUS),
            shortcut: &mut *std::ptr::addr_of_mut!(SHORTCUT),
            tooltip: &mut *std::ptr::addr_of_mut!(TOOLTIP),
            dispatch: &|_| {},
            platform: &PLATFORM,
            requests: &mut *std::ptr::addr_of_mut!(REQUESTS),
        }
    }
}

struct DummyFocus;
impl FocusManager for DummyFocus {
    fn focused_widget(&self) -> Option<WidgetId> {
        None
    }
    fn focused_panel(&self) -> Option<mondrian_editor_state::state::PanelKind> {
        None
    }
    fn request_focus(&mut self, _: WidgetId, _: mondrian_editor_state::state::PanelKind) {}
    fn release_focus(&mut self, _: WidgetId) {}
    fn focus_next(&mut self) {}
    fn focus_prev(&mut self) {}
    fn clear_focus(&mut self) {}
}

struct DummyShortcut;
impl ShortcutManager for DummyShortcut {
    fn register(&mut self, _: ShortcutScope, _: ShortcutBinding, _: Action) {}
    fn unregister(&mut self, _: ShortcutScope, _: &ShortcutBinding) {}
    fn resolve(&self, _: KeyCode, _: Modifiers) -> Option<Action> {
        None
    }
    fn clear_scope(&mut self, _: ShortcutScope) {}
    fn clear_all(&mut self) {}
}

struct DummyTooltip;
impl TooltipManager for DummyTooltip {
    fn show(&mut self, _: String, _: Point) {}
    fn hide(&mut self) {}
    fn current(&self) -> Option<&TooltipState> {
        None
    }
    fn update(&mut self, _: u64) {}
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
                        let _ = root.event(
                            &UiEvent::KeyDown { key: kc, modifiers: modifiers_state },
                            &mut dummy_event_ctx(),
                        );
                    }
                    // Only send TextInput for printable characters when Ctrl is NOT held
                    if !modifiers_state.ctrl {
                        if let Some(txt) = text {
                            if !txt.is_empty() && !txt.chars().any(|c| c.is_control()) {
                                let _ = root.event(
                                    &UiEvent::TextInput(txt.to_string()),
                                    &mut dummy_event_ctx(),
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
                let _ = root.event(&UiEvent::ImeCommit(text), &mut dummy_event_ctx());
                window.request_redraw();
            }
            Event::WindowEvent {
                event: WindowEvent::Ime(winit::event::Ime::Preedit(text, _cursor)),
                ..
            } => {
                let _ = root.event(&UiEvent::ImePreedit(text), &mut dummy_event_ctx());
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
                let _ = root.event(&UiEvent::ImePreedit(String::new()), &mut dummy_event_ctx());
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let mut encoder = DrawEncoder::new();
                let theme = mondrian_ui_theme::current_theme();
                let b = current_bounds.get();
                encoder.draw_rect(b, theme.colors.background, 0.0);
                TreeWalker::paint(&root, &mut encoder, &theme);
                draw_shape_test_patterns(&mut encoder);
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
                let _ = root.event(
                    &UiEvent::MouseMove {
                        position: last_cursor,
                        modifiers: Modifiers::none(),
                    },
                    &mut dummy_event_ctx(),
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
                            .with(|b| b.get().is_some_and(|r| r.contains(last_cursor)));
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
                let _ = root.event(&event, &mut dummy_event_ctx());
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::MouseWheel { delta, .. }, .. } => {
                let scroll_delta = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 20.0,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                let _ = root.event(
                    &UiEvent::MouseWheel {
                        delta: scroll_delta,
                        position: last_cursor,
                        modifiers: Modifiers::none(),
                    },
                    &mut dummy_event_ctx(),
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

// ── Shape Test Patterns ──────────────────────────────────────────────────────
// 目视验证圆形/正方形对齐。绘制多个分层对比以快速定位问题。

/// 用 DrawEncoder 绘制全套形状测试图案，叠加在 widget 上方。
///
/// 每个测试对包含：
/// 1. 实心方形（纯色，corner_radius=0）—— 尺寸参考
/// 2. 半透明圆形（同一尺寸，corner_radius=size/2）—— 理想内接圆
/// 3. 白色定位标记 —— 上/下/左/右边缘中点 + 圆心
///
/// 如果圆形完美内接，圆恰好与白色定位标记相切，
/// 圆心标记与方形中心重合。
fn draw_shape_test_patterns(encoder: &mut DrawEncoder) {
    use mondrian_core::Color;
    use mondrian_ui_core::types::Rect;

    // 50px 间隔参考网格（用细矩形绘制）
    let grid_color = Color { r: 0.3, g: 0.3, b: 0.3, a: 0.25 };
    for gx in (0..800).step_by(50) {
        encoder.draw_rect(Rect::new(gx as f32, 0.0, 1.0, 580.0), grid_color, 0.0);
    }
    for gy in (0..580).step_by(50) {
        encoder.draw_rect(Rect::new(0.0, gy as f32, 800.0, 1.0), grid_color, 0.0);
    }

    // 测试对：(x, y, size, 方形色, 圆形色)
    let pairs = [
        (
            30.0,
            30.0,
            200.0,
            0xFF3333,
            Color { r: 0.2, g: 0.4, b: 1.0, a: 0.35 },
        ),
        (
            260.0,
            30.0,
            100.0,
            0x33FF33,
            Color { r: 1.0, g: 1.0, b: 0.2, a: 0.35 },
        ),
        (
            260.0,
            160.0,
            60.0,
            0xFF9800,
            Color { r: 0.2, g: 1.0, b: 1.0, a: 0.35 },
        ),
        (
            30.0,
            260.0,
            80.0,
            0x9C27B0,
            Color { r: 0.7, g: 0.3, b: 1.0, a: 0.35 },
        ),
        (
            140.0,
            260.0,
            40.0,
            0xE91E63,
            Color { r: 0.5, g: 1.0, b: 0.2, a: 0.35 },
        ),
    ];

    for &(x, y, size, sq_hex, circle_color) in &pairs {
        let half = size * 0.5;
        let cx = x + half;
        let cy = y + half;

        // 第 1 层：实心方形（reference）
        encoder.draw_rect(Rect::new(x, y, size, size), Color::from_hex(sq_hex), 0.0);

        // 第 2 层：半透明圆形（corner_radius = half → 内接圆）
        encoder.draw_rect(Rect::new(x, y, size, size), circle_color, half);

        // 第 3 层：白色圆心标记 (3x3)
        encoder.draw_rect(Rect::new(cx - 1.5, cy - 1.5, 3.0, 3.0), Color::WHITE, 0.0);

        // 第 4 层：边缘中点定位标记（4 个小白条, 8x2 px）
        let ml = 8.0; // marker length
        let mw = 2.0; // marker width
                      // 上边缘中点：从方形上方往内
        encoder.draw_rect(Rect::new(cx - mw * 0.5, y - ml, mw, ml), Color::WHITE, 0.0);
        // 下边缘中点
        encoder.draw_rect(
            Rect::new(cx - mw * 0.5, y + size, mw, ml),
            Color::WHITE,
            0.0,
        );
        // 左边缘中点
        encoder.draw_rect(Rect::new(x - ml, cy - mw * 0.5, ml, mw), Color::WHITE, 0.0);
        // 右边缘中点
        encoder.draw_rect(
            Rect::new(x + size, cy - mw * 0.5, ml, mw),
            Color::WHITE,
            0.0,
        );
    }

    // ── 胶囊形测试（短轴 50x100, r=25) ──────────────────────────────
    encoder.draw_rect(
        Rect::new(390.0, 30.0, 50.0, 100.0),
        Color::from_hex(0x7C4DFF),
        25.0,
    );
    encoder.draw_rect(
        Rect::new(390.0, 30.0, 50.0, 100.0),
        Color { r: 1.0, g: 1.0, b: 1.0, a: 0.3 },
        25.0,
    );
    let chx = 390.0 + 25.0;
    let chy = 30.0 + 50.0;
    encoder.draw_rect(Rect::new(chx - 1.5, chy - 1.5, 3.0, 3.0), Color::WHITE, 0.0);

    // ── 极小圆形（20x20, r=10) ──────────────────────────────────────
    encoder.draw_rect(
        Rect::new(470.0, 30.0, 20.0, 20.0),
        Color::from_hex(0xFF5722),
        10.0,
    );
    encoder.draw_rect(
        Rect::new(470.0, 30.0, 20.0, 20.0),
        Color { r: 1.0, g: 1.0, b: 1.0, a: 0.4 },
        10.0,
    );
    encoder.draw_rect(Rect::new(479.5, 39.5, 3.0, 3.0), Color::WHITE, 0.0);
}
