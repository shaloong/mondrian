#![allow(deprecated)]
//! Mondrian 新 UI 主窗口
//!
//! 使用自研 UI 框架（winit + wgpu + Dock + Widget）的应用入口。
//! 运行: cargo run --bin ui_app

use std::sync::Arc;

use mondrian_core::Color;
use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_panel_console::tracing_layer::ConsoleLogLayer;
use mondrian_platform::SystemPlatformService;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{ShortcutBinding, ShortcutManager, ShortcutScope};
use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, EventRequests, PaintContext};
use mondrian_ui_core::widgets::ColoredBox;
use mondrian_ui_core::{EventResult, TreeWalker, Widget};
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;
use mondrian_ui_text::{resolve_text_commands, TextRenderer};
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::dock_tab_bar::{DockTabBar, TabInfo};
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use mondrian_ui_widgets::panel_slot::{PanelSlot, SlotKind};
use tracing_subscriber::prelude::*;

// ═══════════════════════════════════════════════════════════════════════════
// Menu bar
// ═══════════════════════════════════════════════════════════════════════════

fn menu_items() -> Vec<(&'static str, Vec<MenuItem>)> {
    vec![
        (
            "File",
            vec![
                MenuItem::new("New Project", Action::NewProject),
                MenuItem::new("Open Project...", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
                MenuItem::new("Save As...", Action::SaveProjectAs("".into())),
                MenuItem::new("Quit", Action::CloseProject),
            ],
        ),
        (
            "Edit",
            vec![
                MenuItem::new("Undo", Action::Undo),
                MenuItem::new("Redo", Action::Redo),
                MenuItem::new("Cut", Action::Cut),
                MenuItem::new("Copy", Action::Copy),
                MenuItem::new("Paste", Action::Paste),
            ],
        ),
        (
            "View",
            vec![
                MenuItem::new("Toggle Console", Action::TogglePanel(PanelKind::Console)),
                MenuItem::new("Toggle Timeline", Action::TogglePanel(PanelKind::Timeline)),
                MenuItem::new(
                    "Toggle Inspector",
                    Action::TogglePanel(PanelKind::Inspector),
                ),
            ],
        ),
        (
            "Help",
            vec![MenuItem::new(
                "About Mondrian",
                Action::Custom {
                    namespace: "app".into(),
                    name: "about".into(),
                    payload: serde_json::Value::Null,
                },
            )],
        ),
    ]
}

/// Horizontal menu bar wrapping Dropdown widgets
struct MenuBar {
    id: WidgetId,
    menus: Vec<Dropdown>,
    bounds: Rect,
}

impl MenuBar {
    fn new() -> Self {
        let menus = menu_items()
            .into_iter()
            .map(|(label, items)| Dropdown::new(label, items))
            .collect();
        Self { id: WidgetId::new(), menus, bounds: Rect::ZERO }
    }
}

impl Widget for MenuBar {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, _c: LayoutConstraint) -> Size {
        Size::new(600.0, 28.0)
    }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let mut x = bounds.x;
        for menu in &mut self.menus {
            menu.layout(Rect::new(x, bounds.y, 100.0, 28.0));
            x += 100.0;
        }
    }
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        for menu in &mut self.menus {
            if menu.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }
    fn paint(&self, ctx: &mut PaintContext) {
        let bar_bg = Rect::new(self.bounds.x, self.bounds.y, self.bounds.width, 28.0);
        ctx.encoder.draw_rect(bar_bg, ctx.theme.colors.card, 0.0);
        for menu in &self.menus {
            menu.paint(ctx);
        }
    }
    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }

    fn child_count(&self) -> usize {
        self.menus.len()
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        self.menus.get(index).map(|menu| menu as &dyn Widget)
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        self.menus.get_mut(index).map(|menu| menu as &mut dyn Widget)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Root widget: MenuBar + DockSplitter
// ═══════════════════════════════════════════════════════════════════════════

struct AppRoot {
    id: WidgetId,
    menu_bar: MenuBar,
    dock: DockSplitter,
    bounds: Rect,
}

impl AppRoot {
    fn new(menu_bar: MenuBar, dock: DockSplitter) -> Self {
        Self {
            id: WidgetId::new(),
            menu_bar,
            dock,
            bounds: Rect::ZERO,
        }
    }
}

impl Widget for AppRoot {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, c: LayoutConstraint) -> Size {
        c.constrain(Size::new(800.0, 600.0))
    }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.menu_bar.layout(Rect::new(bounds.x, bounds.y, bounds.width, 28.0));
        self.dock.layout(Rect::new(
            bounds.x,
            bounds.y + 28.0,
            bounds.width,
            (bounds.height - 28.0).max(0.0),
        ));
    }
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.menu_bar.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        self.dock.event(event, ctx)
    }
    fn paint(&self, ctx: &mut PaintContext) {
        self.menu_bar.paint(ctx);
        self.dock.paint(ctx);
    }
    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }

    fn child_count(&self) -> usize {
        2
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.menu_bar),
            1 => Some(&self.dock),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.menu_bar),
            1 => Some(&mut self.dock),
            _ => None,
        }
    }
}

/// Access the inner DockSplitter for grab zone queries
impl AppRoot {
    fn dock(&self) -> &DockSplitter {
        &self.dock
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dock tree (unchanged)
// ═══════════════════════════════════════════════════════════════════════════

fn build_dock_tree() -> DockSplitter {
    let left = DockSplitter::new(
        SplitDirection::Vertical,
        0.6,
        slot(SlotKind::Assets),
        slot(SlotKind::Console),
    );
    let right_bottom = DockSplitter::new(
        SplitDirection::Horizontal,
        0.7,
        slot(SlotKind::Timeline),
        slot(SlotKind::Inspector),
    );
    let right = DockSplitter::new(
        SplitDirection::Vertical,
        0.65,
        slot(SlotKind::Viewer),
        Box::new(right_bottom),
    );
    DockSplitter::new(
        SplitDirection::Horizontal,
        0.28,
        Box::new(left),
        Box::new(right),
    )
}

fn slot(kind: SlotKind) -> Box<dyn Widget> {
    let color = match kind {
        SlotKind::Viewer => Color::from_hex(0x1A1A2E),
        SlotKind::Timeline => Color::from_hex(0x16213E),
        SlotKind::Assets => Color::from_hex(0x0F3460),
        SlotKind::Inspector => Color::from_hex(0x1E2A3A),
        SlotKind::Effects => Color::from_hex(0x2A1A3E),
        SlotKind::Project => Color::from_hex(0x1E3A2A),
        SlotKind::Console => Color::from_hex(0x0D1117),
        _ => Color::from_hex(0x1A1A1A),
    };
    let tab_bar = DockTabBar::new(vec![TabInfo {
        label: kind.display_name().to_string(),
        active: true,
    }]);
    let content = PanelSlot::new(kind, Box::new(ColoredBox::new(color, 1.0, 1.0)));
    Box::new(VerticalTabbedSlot::new(tab_bar, content))
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

struct VerticalTabbedSlot {
    id: WidgetId,
    tab_bar: DockTabBar,
    content: Box<dyn Widget>,
    bounds: Rect,
}
impl VerticalTabbedSlot {
    fn new(tab_bar: DockTabBar, content: PanelSlot) -> Self {
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
        self.tab_bar.layout(Rect::new(bounds.x, bounds.y, bounds.width, 26.0));
        self.content.layout(Rect::new(
            bounds.x,
            bounds.y + 26.0,
            bounds.width,
            (bounds.height - 26.0).max(0.0),
        ));
    }
    fn event(&mut self, e: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.tab_bar.event(e, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        self.content.event(e, ctx)
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

fn mouse_button(b: winit::event::MouseButton) -> MouseButton {
    match b {
        winit::event::MouseButton::Left => MouseButton::Left,
        winit::event::MouseButton::Right => MouseButton::Right,
        winit::event::MouseButton::Middle => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn dummy_event_ctx() -> EventContext<'static> {
    static mut F: DummyFocus = DummyFocus;
    static mut S: DummyShortcut = DummyShortcut;
    static mut T: DummyTooltip = DummyTooltip;
    static mut R: EventRequests =
        EventRequests { pointer_capture: None, ime: None, repaint: false };
    unsafe {
        EventContext {
            focus: &mut *std::ptr::addr_of_mut!(F),
            shortcut: &mut *std::ptr::addr_of_mut!(S),
            tooltip: &mut *std::ptr::addr_of_mut!(T),
            dispatch: &|_| {},
            platform: &SystemPlatformService,
            requests: &mut *std::ptr::addr_of_mut!(R),
        }
    }
}

struct DummyFocus;
impl FocusManager for DummyFocus {
    fn focused_widget(&self) -> Option<WidgetId> {
        None
    }
    fn focused_panel(&self) -> Option<PanelKind> {
        None
    }
    fn request_focus(&mut self, _: WidgetId, _: PanelKind) {}
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
    let (console_layer, _log_buffer) = ConsoleLogLayer::new(500);
    tracing_subscriber::registry()
        .with(console_layer)
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .init();

    tracing::info!("Mondrian UI App starting");

    use winit::event_loop::EventLoop;
    let event_loop = EventLoop::new()?;
    let window_attrs = winit::window::Window::default_attributes()
        .with_title("Mondrian — 自研 UI")
        .with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
    let window = Arc::new(event_loop.create_window(window_attrs)?);

    let instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    let instance = wgpu::Instance::new(instance_desc);
    let surface = instance.create_surface(window.clone())?;

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|_| "No suitable GPU adapter")?;

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;

    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width, size.height)
        .ok_or("Failed surface config")?;
    surface.configure(&device, &config);

    let ui_renderer = UiRenderer::new(&device, config.format);
    let mut text_renderer = TextRenderer::new();

    let menu_bar = MenuBar::new();
    let dock = build_dock_tree();
    let mut root = AppRoot::new(menu_bar, dock);
    let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    TreeWalker::layout(&mut root, bounds);

    let mut last_cursor = Point::new(0.0, 0.0);
    let current_bounds = std::cell::Cell::new(bounds);

    tracing::info!("UI initialized — {}x{}", size.width, size.height);

    event_loop.run(move |event, elwt| {
        use winit::event::ElementState;
        use winit::event::{Event, WindowEvent};
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
                // Upload any newly rasterized glyphs to GPU atlas
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
                    let b = Rect::new(0.0, 0.0, new_size.width as f32, new_size.height as f32);
                    current_bounds.set(b);
                    TreeWalker::layout(&mut root, b);
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
                let zones = root.dock().collect_grab_zones();
                let dir = zones.iter().find(|(z, _)| z.contains(last_cursor)).map(|(_, d)| *d);
                window.set_cursor_icon(match dir {
                    Some(SplitDirection::Horizontal) => winit::window::CursorIcon::ColResize,
                    Some(SplitDirection::Vertical) => winit::window::CursorIcon::RowResize,
                    None => winit::window::CursorIcon::Default,
                });
                window.request_redraw();
            }

            Event::WindowEvent {
                event: WindowEvent::MouseInput { state, button, .. },
                ..
            } => {
                let evt = match state {
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
                let _ = root.event(&evt, &mut dummy_event_ctx());
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::MouseWheel { delta, .. }, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => -y * 20.0,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32),
                };
                let _ = root.event(
                    &UiEvent::MouseWheel {
                        delta: dy,
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
