use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Simple: just confirm we can get red/blue on screen with our real pipeline
    let el = winit::event_loop::EventLoop::new()?;
    let w = Arc::new(el.create_window(winit::window::Window::default_attributes()
        .with_title("Pipeline Raw Test")
        .with_inner_size(winit::dpi::LogicalSize::new(600, 400)))?);
    let inst = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let surf = inst.create_surface(w.clone())?;
    let adap = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surf),
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    })).unwrap();
    let (dev, q) = pollster::block_on(adap.request_device(&wgpu::DeviceDescriptor::default()))?;
    let sz = w.inner_size();
    let mut cfg = surf.get_default_config(&adap, sz.width, sz.height).unwrap();
    surf.configure(&dev, &cfg);

    // Use our real pipeline from mondrian-ui-renderer
    use mondrian_ui_renderer::{DrawEncoder, UiRenderer};
    let renderer = UiRenderer::new(&dev, cfg.format);

    el.run(move |ev, elwt| {
        use winit::event_loop::ControlFlow;
        use winit::event::{Event, WindowEvent};
        elwt.set_control_flow(ControlFlow::Wait);
        match ev {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. }
            | Event::WindowEvent { event: WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                    state: winit::event::ElementState::Pressed, ..
                }, ..
            }, .. } => elwt.exit(),
            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let mut enc = DrawEncoder::new();
                // Background gray
                enc.draw_rect(
                    mondrian_ui_core::types::Rect::new(0.0, 0.0, 600.0, 400.0),
                    mondrian_core::Color { r: 0.07, g: 0.07, b: 0.07, a: 1.0 },
                    0.0,
                );
                // Red rect left
                enc.draw_rect(
                    mondrian_ui_core::types::Rect::new(150.0, 100.0, 100.0, 200.0),
                    mondrian_core::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
                    0.0,
                );
                // Blue rect right
                enc.draw_rect(
                    mondrian_ui_core::types::Rect::new(350.0, 100.0, 100.0, 200.0),
                    mondrian_core::Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 },
                    0.0,
                );
                let cmds = enc.finish();
                let cur = surf.get_current_texture();
                match cur {
                    wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                        let v = f.texture.create_view(&Default::default());
                        renderer.render(&dev, &q, &v, &cmds, (sz.width, sz.height));
                        f.present();
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
            Event::AboutToWait => { w.request_redraw(); }
            _ => {}
        }
    })?;
    Ok(())
}
