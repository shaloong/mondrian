#![allow(deprecated)]
//! Mondrian 新 UI 主窗口
//!
//! 使用自研 UI 框架（winit + wgpu + Dock + Widget）的应用入口。
//! 运行: cargo run --bin ui_app

use std::sync::Arc;

use mondrian_core::Color;
use mondrian_editor_state::state::PanelKind;
use mondrian_platform::NoopPlatformService;
use mondrian_panel_console::tracing_layer::ConsoleLogLayer;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{ShortcutBinding, ShortcutManager, ShortcutScope};
use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::widgets::ColoredBox;
use mondrian_ui_core::{EventResult, TreeWalker, Widget};
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::dock_tab_bar::{DockTabBar, TabInfo};
use mondrian_ui_widgets::panel_slot::{PanelSlot, SlotKind};
use tracing_subscriber::prelude::*;

// ═══════════════════════════════════════════════════════════════════════════
// Dock tree
// ═══════════════════════════════════════════════════════════════════════════

fn build_dock_tree() -> DockSplitter {
    let left = DockSplitter::new(
        SplitDirection::Vertical, 0.6,
        slot(SlotKind::Assets),
        slot(SlotKind::Console),
    );

    let right_bottom = DockSplitter::new(
        SplitDirection::Horizontal, 0.7,
        slot(SlotKind::Timeline),
        slot(SlotKind::Inspector),
    );

    let right = DockSplitter::new(
        SplitDirection::Vertical, 0.65,
        slot(SlotKind::Viewer),
        Box::new(right_bottom),
    );

    DockSplitter::new(SplitDirection::Horizontal, 0.28, Box::new(left), Box::new(right))
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
// VerticalTabbedSlot
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
    fn id(&self) -> WidgetId { self.id }
    fn measure(&self, c: LayoutConstraint) -> Size { c.constrain(Size::new(100.0, 100.0)) }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let tab_h = 26.0;
        self.tab_bar.layout(Rect::new(bounds.x, bounds.y, bounds.width, tab_h));
        self.content.layout(Rect::new(bounds.x, bounds.y + tab_h, bounds.width, (bounds.height - tab_h).max(0.0)));
    }
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.tab_bar.event(event, ctx) == EventResult::Handled { return EventResult::Handled; }
        self.content.event(event, ctx)
    }
    fn paint(&self, ctx: &mut PaintContext) {
        self.tab_bar.paint(ctx);
        self.content.paint(ctx);
    }
    fn hit_test(&self, p: Point) -> bool { self.bounds.contains(p) }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dummy managers + helpers
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
    static mut F: DummyFocus = DummyFocus;
    static mut S: DummyShortcut = DummyShortcut;
    static mut T: DummyTooltip = DummyTooltip;
    unsafe {
        EventContext {
            focus: &mut *std::ptr::addr_of_mut!(F),
            shortcut: &mut *std::ptr::addr_of_mut!(S),
            tooltip: &mut *std::ptr::addr_of_mut!(T),
            dispatch: &|_| {},
            platform: &NoopPlatformService,
        }
    }
}

struct DummyFocus;
impl FocusManager for DummyFocus {
    fn focused_widget(&self) -> Option<WidgetId> { None }
    fn focused_panel(&self) -> Option<PanelKind> { None }
    fn request_focus(&mut self, _: WidgetId, _: PanelKind) {}
    fn release_focus(&mut self, _: WidgetId) {}
    fn focus_next(&mut self) {}
    fn focus_prev(&mut self) {}
    fn clear_focus(&mut self) {}
}

struct DummyShortcut;
impl ShortcutManager for DummyShortcut {
    fn register(&mut self, _: ShortcutScope, _: ShortcutBinding, _: mondrian_editor_state::Action) {}
    fn unregister(&mut self, _: ShortcutScope, _: &ShortcutBinding) {}
    fn resolve(&self, _: KeyCode, _: Modifiers) -> Option<mondrian_editor_state::Action> { None }
    fn clear_scope(&mut self, _: ShortcutScope) {}
    fn clear_all(&mut self) {}
}

struct DummyTooltip;
impl TooltipManager for DummyTooltip {
    fn show(&mut self, _: String, _: Point) {}
    fn hide(&mut self) {}
    fn current(&self) -> Option<&TooltipState> { None }
    fn update(&mut self, _: u64) {}
}

// ═══════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup tracing with console layer
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

    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    })) {
        Ok(a) => a,
        Err(_) => return Err("No suitable GPU adapter".into()),
    };

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;

    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width, size.height)
        .ok_or("Failed surface config")?;
    surface.configure(&device, &config);

    let ui_renderer = UiRenderer::new(&device, config.format);

    let mut root = build_dock_tree();
    let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    TreeWalker::layout(&mut root, bounds);

    let mut last_cursor = Point::new(0.0, 0.0);
    let current_bounds = std::cell::Cell::new(bounds);

    tracing::info!("UI initialized — {}x{}", size.width, size.height);

    event_loop.run(move |event, elwt| {
        use winit::event::ElementState;
        use winit::event_loop::ControlFlow;
        use winit::event::{Event, WindowEvent};
        elwt.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. }
            | Event::WindowEvent { event: WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                }, ..
            }, .. } => elwt.exit(),

            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let mut encoder = DrawEncoder::new();
                let theme = mondrian_ui_theme::current_theme();
                let b = current_bounds.get();
                encoder.draw_rect(b, theme.colors.background, 0.0);
                TreeWalker::paint(&root, &mut encoder, &theme);
                let commands = encoder.finish();

                let current = surface.get_current_texture();
                match current {
                    wgpu::CurrentSurfaceTexture::Success(output)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                        let v = output.texture.create_view(&Default::default());
                        let sz = window.inner_size();
                        ui_renderer.render(&device, &queue, &v, &commands, (sz.width, sz.height));
                        output.present();
                    }
                    wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {}
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
                    let new_bounds = Rect::new(0.0, 0.0, new_size.width as f32, new_size.height as f32);
                    current_bounds.set(new_bounds);
                    TreeWalker::layout(&mut root, new_bounds);
                    window.request_redraw();
                }
            }

            Event::WindowEvent { event: WindowEvent::CursorMoved { position, .. }, .. } => {
                last_cursor = Point::new(position.x as f32, position.y as f32);
                let _ = root.event(
                    &UiEvent::MouseMove { position: last_cursor, modifiers: Modifiers::none() },
                    &mut dummy_event_ctx(),
                );
                let grab_zones = root.collect_grab_zones();
                let direction = grab_zones.iter().find(|(z, _)| z.contains(last_cursor)).map(|(_, d)| *d);
                match direction {
                    Some(SplitDirection::Horizontal) => window.set_cursor_icon(winit::window::CursorIcon::ColResize),
                    Some(SplitDirection::Vertical) => window.set_cursor_icon(winit::window::CursorIcon::RowResize),
                    None => window.set_cursor_icon(winit::window::CursorIcon::Default),
                }
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::MouseInput { state, button, .. }, .. } => {
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
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 20.0,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                let _ = root.event(
                    &UiEvent::MouseWheel { delta: dy, position: last_cursor, modifiers: Modifiers::none() },
                    &mut dummy_event_ctx(),
                );
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::F5),
                    state: ElementState::Pressed,
                    ..
                }, ..
            }, .. } => {
                // Toggle Dark/Light theme on F5
                use mondrian_ui_theme::ThemePreset;
                let next = {
                    let theme = mondrian_ui_theme::current_theme();
                    match theme.name.as_str() {
                        "Dark" => ThemePreset::Light,
                        _ => ThemePreset::Dark,
                    }
                }; // RwLockReadGuard dropped before write lock
                mondrian_ui_theme::set_theme_preset(next);
                tracing::info!("Theme switched to {}", next.display_name());
                window.request_redraw();
            }

            Event::AboutToWait => { window.request_redraw(); }
            _ => {}
        }
    })?;

    Ok(())
}
