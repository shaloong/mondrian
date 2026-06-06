//! 自研 UI 调试窗口
//!
//! 用 winit + wgpu 渲染独立窗口，验证完整管线。
//! Stage B 阶段仅用于开发调试。

use std::sync::Arc;

use mondrian_core::Color;
use mondrian_ui_core::widgets::{ColoredBox, Container, Spacer};
use mondrian_ui_core::TreeWalker;
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;

#[allow(dead_code)]
fn build_demo_widget_tree() -> Container {
    let _green = ColoredBox::new(Color::from_hex(0x4CAF50), 200.0, 60.0);
    let _blue = ColoredBox::new(Color::from_hex(0x2196F3), 200.0, 60.0);
    let _red = ColoredBox::new(Color::from_hex(0xF44336), 200.0, 60.0);

    Container::new(Some(Box::new(Spacer::new(200.0, 150.0))))
        .with_padding(0.0)
        .with_background(Color::from_hex(0x121212))
}

#[allow(dead_code)]
pub fn launch_ui_demo_window() {
    std::thread::spawn(|| {
        if let Err(e) = run_ui_demo_window() {
            tracing::error!("UI demo window failed: {e}");
        }
    });
}

#[allow(dead_code, deprecated)]
fn run_ui_demo_window() -> Result<(), Box<dyn std::error::Error>> {
    use winit::event_loop::{ControlFlow, EventLoop};

    let event_loop = EventLoop::new()?;
    let window_attrs = winit::window::Window::default_attributes()
        .with_title("Mondrian UI Demo")
        .with_inner_size(winit::dpi::LogicalSize::new(400, 300));
    let window = Arc::new(event_loop.create_window(window_attrs)?);

    let instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    let instance = wgpu::Instance::new(instance_desc);
    let surface = instance.create_surface(window.clone())?;

    let adapter =
        match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })) {
            Ok(a) => a,
            Err(_) => return Err("No suitable GPU adapter".into()),
        };

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor::default(),
    ))?;

    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width, size.height)
        .ok_or("Failed surface config")?;
    surface.configure(&device, &config);

    let ui_renderer = UiRenderer::new(&device, config.format);

    let mut root = build_demo_widget_tree();
    let bounds =
        mondrian_ui_core::types::Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    TreeWalker::layout(&mut root, bounds);

    #[allow(deprecated)]
    event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Wait);

        match event {
            winit::event::Event::WindowEvent {
                event: winit::event::WindowEvent::CloseRequested,
                ..
            } => elwt.exit(),

            winit::event::Event::WindowEvent {
                event: winit::event::WindowEvent::RedrawRequested,
                ..
            } => {
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
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => {
                        // skip frame
                    }
                    wgpu::CurrentSurfaceTexture::Outdated
                    | wgpu::CurrentSurfaceTexture::Lost => {
                        surface.configure(&device, &config);
                    }
                    wgpu::CurrentSurfaceTexture::Validation => {
                        tracing::warn!("UI demo: surface validation error");
                    }
                }
            }

            winit::event::Event::WindowEvent {
                event: winit::event::WindowEvent::Resized(new_size),
                ..
            } => {
                if new_size.width > 0 && new_size.height > 0 {
                    config.width = new_size.width;
                    config.height = new_size.height;
                    surface.configure(&device, &config);
                    window.request_redraw();
                }
            }

            winit::event::Event::AboutToWait => {
                window.request_redraw();
            }

            _ => {}
        }
    })?;

    Ok(())
}
