#![allow(deprecated)]
//! Minimal widget paint test — verifies the Widget tree produces correct draw commands.
//! Run: cargo run --bin ui_widget_test
//!
//! Builds a simple widget tree and renders it. Uses BRIGHT colors to make visual
//! inspection easy.

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::widgets::ColoredBox;
use mondrian_ui_core::{EventResult, TreeWalker, Widget};
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;
use std::sync::Arc;

/// A root widget that fills its entire bounds with a background color.
struct RootFill {
    id: WidgetId,
    children: Vec<Box<dyn Widget>>,
    bounds: Rect,
}
impl RootFill {
    fn new(children: Vec<Box<dyn Widget>>) -> Self {
        Self { id: WidgetId::new(), children, bounds: Rect::ZERO }
    }
}
impl Widget for RootFill {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, c: LayoutConstraint) -> Size {
        c.constrain(Size::new(200.0, 100.0))
    }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        // Assign children distinct regions
        let half_w = bounds.width / 2.0;
        if let Some(c) = self.children.get_mut(0) {
            c.layout(Rect::new(bounds.x, bounds.y, half_w, bounds.height));
        }
        if let Some(c) = self.children.get_mut(1) {
            c.layout(Rect::new(
                bounds.x + half_w,
                bounds.y,
                half_w,
                bounds.height,
            ));
        }
    }
    fn event(&mut self, _e: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }
    fn paint(&self, ctx: &mut PaintContext) {
        // Fill background
        ctx.encoder.draw_rect(self.bounds, Color::from_hex(0x222222), 0.0);
        eprintln!(
            "PAINT RootFill bg bounds=({:.0},{:.0} {:.0}x{:.0})",
            self.bounds.x, self.bounds.y, self.bounds.width, self.bounds.height
        );
        for child in &self.children {
            child.paint(ctx);
        }
    }
    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }
    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }
    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut self.children
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let el = winit::event_loop::EventLoop::new()?;
    let w = Arc::new(
        el.create_window(
            winit::window::Window::default_attributes()
                .with_title("Widget Paint Test")
                .with_inner_size(winit::dpi::LogicalSize::new(800, 500)),
        )?,
    );

    let inst = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let surf = inst.create_surface(w.clone())?;
    let adap = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surf),
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .unwrap();
    let (dev, q) = pollster::block_on(adap.request_device(&wgpu::DeviceDescriptor::default()))?;
    let sz = w.inner_size();
    let mut cfg = surf.get_default_config(&adap, sz.width, sz.height).unwrap();
    surf.configure(&dev, &cfg);
    let mut renderer = UiRenderer::new(&dev, cfg.format);

    // Build widget tree: RootFill containing 2 ColoredBox children
    // Red box at (50,50) 300x400, Blue box at (450,50) 300x400
    let mut root = RootFill::new(vec![
        Box::new(
            ColoredBox::new(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }, 300.0, 400.0)
                .with_label("red"),
        ),
        Box::new(
            ColoredBox::new(Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }, 300.0, 400.0)
                .with_label("blue"),
        ),
    ]);

    let bounds = Rect::new(0.0, 0.0, sz.width as f32, sz.height as f32);
    TreeWalker::layout(&mut root, bounds);

    el.run(move |ev, elwt| {
        use winit::event::{Event, WindowEvent};
        use winit::event_loop::ControlFlow;
        elwt.set_control_flow(ControlFlow::Wait);
        match ev {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. }
            | Event::WindowEvent {
                event:
                    WindowEvent::KeyboardInput {
                        event:
                            winit::event::KeyEvent {
                                logical_key:
                                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                                state: winit::event::ElementState::Pressed,
                                ..
                            },
                        ..
                    },
                ..
            } => elwt.exit(),
            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let mut enc = DrawEncoder::new();
                let theme = mondrian_ui_theme::current_theme();
                TreeWalker::paint_clipped(&root, &mut enc, &theme, bounds);
                let cmds = enc.finish();
                let cur = surf.get_current_texture();
                match cur {
                    wgpu::CurrentSurfaceTexture::Success(f)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                        let v = f.texture.create_view(&Default::default());
                        renderer.render(&dev, &q, &v, &cmds, (sz.width, sz.height));
                        f.present();
                    }
                    wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                        surf.configure(&dev, &cfg);
                    }
                    _ => {}
                }
            }
            Event::WindowEvent { event: WindowEvent::Resized(ns), .. } => {
                if ns.width > 0 && ns.height > 0 {
                    cfg.width = ns.width;
                    cfg.height = ns.height;
                    surf.configure(&dev, &cfg);
                }
            }
            Event::AboutToWait => {
                w.request_redraw();
            }
            _ => {}
        }
    })?;
    Ok(())
}
