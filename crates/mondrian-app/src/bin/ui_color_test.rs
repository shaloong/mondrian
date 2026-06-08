#![allow(deprecated)]
//! Minimal color test — verifies the GPU pipeline renders correct colors.
//! Run: cargo run --bin ui_color_test
//!
//! Draws 3 rectangles side by side: Red #FF0000, Green #00FF00, Blue #0000FF.
//! If these render correctly, the issue is in the widget tree, not the pipeline.

use mondrian_core::Color;
use mondrian_ui_core::types::Rect;
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[allow(deprecated)]
    let el = winit::event_loop::EventLoop::new()?;
    let w = Arc::new(
        el.create_window(
            winit::window::Window::default_attributes()
                .with_title("Color Test")
                .with_inner_size(winit::dpi::LogicalSize::new(900, 400)),
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
    let renderer = UiRenderer::new(&dev, cfg.format);

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
                // Background: dark gray
                enc.draw_rect(
                    Rect::new(0.0, 0.0, 900.0, 400.0),
                    Color::from_hex(0x121212),
                    0.0,
                );
                // Red rect on left
                enc.draw_rect(
                    Rect::new(50.0, 50.0, 200.0, 300.0),
                    Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
                    0.0,
                );
                // Green rect in middle
                enc.draw_rect(
                    Rect::new(350.0, 50.0, 200.0, 300.0),
                    Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
                    0.0,
                );
                // Blue rect on right
                enc.draw_rect(
                    Rect::new(650.0, 50.0, 200.0, 300.0),
                    Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 },
                    0.0,
                );
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
