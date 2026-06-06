//! Dock Demo — 独立 wgpu 窗口测试 Dock 系统
//!
//! 运行: cargo run --bin ui_dock_demo

use std::sync::Arc;

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::widgets::ColoredBox;
use mondrian_ui_core::{EventResult, TreeWalker, Widget};
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;
use mondrian_ui_widgets::dock_splitter::{DockSplitter, SplitDirection};
use mondrian_ui_widgets::dock_tab_bar::{DockTabBar, TabInfo};
use mondrian_ui_widgets::panel_slot::{PanelSlot, SlotKind};

/// 为不同面板创建占位内容（带颜色区分）
fn slot_content(kind: SlotKind) -> Box<dyn Widget> {
    let color = match kind {
        SlotKind::Viewer => Color::from_hex(0x1A1A2E),
        SlotKind::Timeline => Color::from_hex(0x16213E),
        SlotKind::Assets => Color::from_hex(0x0F3460),
        SlotKind::Inspector => Color::from_hex(0x1E2A3A),
        SlotKind::Effects => Color::from_hex(0x2A1A3E),
        SlotKind::Project => Color::from_hex(0x1E3A2A),
        SlotKind::Console => Color::from_hex(0x0D1117),
    };
    Box::new(ColoredBox::new(color, 100.0, 100.0))
}

/// 创建带 tab bar + content 的面板容器（Stack 布局模拟）
fn tabbed_slot(kind: SlotKind) -> Box<dyn Widget> {
    let tab_bar = DockTabBar::new(vec![TabInfo {
        label: kind.label().to_string(),
        active: true,
    }]);

    let content = PanelSlot::new(kind, slot_content(kind));

    // 用 DockSplitter(Vertical) 把 tab bar 和 content 上下排列
    // ratio=0.0 表示 tab bar 在顶部固定高度
    // 实际用约 26px 高的 tab bar + 剩余给 content
    Box::new(VerticalTabbedSlot {
        id: WidgetId::new(),
        tab_bar,
        content: Box::new(content),
        bounds: Rect::ZERO,
    })
}

/// 简易垂直布局：tab bar 在上，content 在下
struct VerticalTabbedSlot {
    id: WidgetId,
    tab_bar: DockTabBar,
    content: Box<dyn Widget>,
    bounds: Rect,
}

impl Widget for VerticalTabbedSlot {
    fn id(&self) -> WidgetId { self.id }
    fn measure(&self, c: LayoutConstraint) -> Size { c.constrain(Size::new(100.0, 100.0)) }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let tab_h = 26.0;
        self.tab_bar.layout(Rect::new(bounds.x, bounds.y, bounds.width, tab_h));
        self.content.layout(Rect::new(bounds.x, bounds.y + tab_h, bounds.width, bounds.height - tab_h));
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

fn build_dock_tree() -> DockSplitter {
    // 左侧: Assets(上) + Console(下)
    let left = DockSplitter::new(
        SplitDirection::Vertical,
        0.6,
        tabbed_slot(SlotKind::Assets),
        tabbed_slot(SlotKind::Console),
    );

    // 右侧下: Timeline(左) + Inspector(右)
    let right_bottom = DockSplitter::new(
        SplitDirection::Horizontal,
        0.7,
        tabbed_slot(SlotKind::Timeline),
        tabbed_slot(SlotKind::Inspector),
    );

    // 右侧: Viewer(上) + right_bottom(下)
    let right = DockSplitter::new(
        SplitDirection::Vertical,
        0.65,
        tabbed_slot(SlotKind::Viewer),
        Box::new(right_bottom),
    );

    // 根: left + right
    DockSplitter::new(SplitDirection::Horizontal, 0.28, Box::new(left), Box::new(right))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use winit::event_loop::EventLoop;

    let event_loop = EventLoop::new()?;
    let window_attrs = winit::window::Window::default_attributes()
        .with_title("Mondrian UI — Dock Demo")
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

    event_loop.run(move |event, elwt| {
        use winit::event_loop::ControlFlow;
        use winit::event::{Event, WindowEvent};
        elwt.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. }
            | Event::WindowEvent { event: WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                    state: winit::event::ElementState::Pressed,
                    ..
                }, ..
            }, .. } => elwt.exit(),

            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let mut encoder = DrawEncoder::new();
                let theme = mondrian_ui_theme::current_theme();
                TreeWalker::paint(&root, &mut encoder, &theme);
                let commands = encoder.finish();

                let current = surface.get_current_texture();
                match current {
                    wgpu::CurrentSurfaceTexture::Success(output)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                        let v = output.texture.create_view(&Default::default());
                        ui_renderer.render(&device, &queue, &v, &commands, (size.width, size.height));
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
                    window.request_redraw();
                }
            }

            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. }, ..
            } => {
                // Route mouse move to widget tree
                let pt = Point::new(position.x as f32, position.y as f32);
                let _ = root.event(
                    &UiEvent::MouseMove { position: pt, modifiers: Modifiers::none() },
                    &mut dummy_event_ctx(),
                );
            }

            Event::WindowEvent {
                event: WindowEvent::MouseInput { state, button, .. }, ..
            } => {
                use winit::event::ElementState;
                let pt = Point::new(0.0, 0.0); // cursor pos not available directly
                let event = match state {
                    ElementState::Pressed => UiEvent::MouseDown {
                        position: pt,
                        button: mouse_button(button),
                        modifiers: Modifiers::none(),
                    },
                    ElementState::Released => UiEvent::MouseUp {
                        position: pt,
                        button: mouse_button(button),
                        modifiers: Modifiers::none(),
                    },
                };
                let _ = root.event(&event, &mut dummy_event_ctx());
            }

            Event::AboutToWait => { window.request_redraw(); }
            _ => {}
        }
    })?;

    Ok(())
}

fn mouse_button(b: winit::event::MouseButton) -> mondrian_ui_core::types::MouseButton {
    match b {
        winit::event::MouseButton::Left => mondrian_ui_core::types::MouseButton::Left,
        winit::event::MouseButton::Right => mondrian_ui_core::types::MouseButton::Right,
        winit::event::MouseButton::Middle => mondrian_ui_core::types::MouseButton::Middle,
        _ => mondrian_ui_core::types::MouseButton::Left,
    }
}

fn dummy_event_ctx() -> EventContext<'static> {
    /// Returns a dummy context (font size from egui by default); Ok(()) otherwise);
    /// This match is exhaustive.
    let _ = Option::<()>::None;
    unimplemented!()
}
