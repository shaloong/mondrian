#![allow(deprecated)]
//! UI Element Gallery — 独立 wgpu 窗口，全面测试所有 UI 控件
//!
//! 运行: cargo run --bin ui_demo
//!
//! 包含的 Widget 类型：
//!   Core: ColoredBox
//!   Interactive: Button, Checkbox, TextInput, Slider, List, Dropdown, ContextMenu
//!   Layout: DockSplitter, DockTabBar, PanelSlot, ScrollView

use std::sync::Arc;

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_platform::NoopPlatformService;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{ShortcutBinding, ShortcutManager, ShortcutScope};
use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::{self as ui_types, *};
use mondrian_ui_core::widget::{EventContext, PaintContext};
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
        };

        if self.button_click.event(event, inner_ctx) == EventResult::Handled {
            self.last_action = last_action.into_inner();
            return EventResult::Handled;
        }
        if self.button_no_action.event(event, inner_ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.checkbox_a.event(event, inner_ctx) == EventResult::Handled {
            self.last_action = last_action.into_inner();
            return EventResult::Handled;
        }
        if self.checkbox_b.event(event, inner_ctx) == EventResult::Handled {
            self.last_action = last_action.into_inner();
            return EventResult::Handled;
        }
        if self.text_input.event(event, inner_ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.slider.event(event, inner_ctx) == EventResult::Handled {
            self.last_action = format!("slider={:.1}", self.slider.value());
            return EventResult::Handled;
        }
        if self.dropdown.event(event, inner_ctx) == EventResult::Handled {
            self.last_action = last_action.into_inner();
            return EventResult::Handled;
        }
        if self.list.event(event, inner_ctx) == EventResult::Handled {
            self.last_action = last_action.into_inner();
            return EventResult::Handled;
        }
        if self.scroll_area.event(event, inner_ctx) == EventResult::Handled {
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
    content: Box<dyn Widget>,
    bounds: Rect,
}

impl VerticalTabbedSlot {
    fn new(kind: SlotKind) -> Self {
        let tabs = if kind == SlotKind::Timeline {
            vec![
                TabInfo { label: "时间线".to_string(), active: true },
                TabInfo { label: "音频".to_string(), active: false },
                TabInfo { label: "效果".to_string(), active: false },
            ]
        } else if kind == SlotKind::Inspector {
            vec![
                TabInfo { label: "检查器".to_string(), active: true },
                TabInfo { label: "属性".to_string(), active: false },
            ]
        } else {
            vec![TabInfo {
                label: kind.display_name().to_string(),
                active: true,
            }]
        };

        let tab_bar = DockTabBar::new(tabs);
        let content = PanelSlot::new(kind, slot_content(kind));
        Self {
            id: WidgetId::new(),
            tab_bar,
            content: Box::new(content),
            bounds: Rect::ZERO,
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
        if self.tab_bar.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        self.content.event(event, ctx)
    }
    fn paint(&self, ctx: &mut PaintContext) {
        self.tab_bar.paint(ctx);
        self.content.paint(ctx);
    }
    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Slot content factory
// ═══════════════════════════════════════════════════════════════════════════

fn slot_content(kind: SlotKind) -> Box<dyn Widget> {
    match kind {
        SlotKind::Viewer => Box::new(ViewerWidget::new()),
        // Top-left: red — easy diagnostic reference
        SlotKind::Assets => Box::new(ColoredBox::new(Color::from_hex(0xCC2222), 1.0, 1.0)),
        // Bottom-left: interactive widget gallery
        SlotKind::Console => Box::new(GalleryWidget::new()),
        SlotKind::Inspector => Box::new(ColoredBox::new(Color::from_hex(0x1E2A3A), 1.0, 1.0)),
        SlotKind::Timeline => Box::new(ColoredBox::new(Color::from_hex(0x16213E), 1.0, 1.0)),
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

fn dummy_event_ctx() -> EventContext<'static> {
    static mut FOCUS: DummyFocus = DummyFocus;
    static mut SHORTCUT: DummyShortcut = DummyShortcut;
    static mut TOOLTIP: DummyTooltip = DummyTooltip;
    unsafe {
        EventContext {
            focus: &mut *std::ptr::addr_of_mut!(FOCUS),
            shortcut: &mut *std::ptr::addr_of_mut!(SHORTCUT),
            tooltip: &mut *std::ptr::addr_of_mut!(TOOLTIP),
            dispatch: &|_| {},
            platform: &NoopPlatformService,
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

    event_loop.run(move |event, elwt| {
        use winit::event::ElementState;
        use winit::event::Event;
        use winit::event::WindowEvent;
        use winit::event_loop::ControlFlow;
        elwt.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. }
            | Event::WindowEvent {
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
                        window.set_cursor_icon(winit::window::CursorIcon::Default);
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
