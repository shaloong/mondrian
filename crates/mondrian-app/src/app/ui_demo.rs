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
/// 在 DrawEncoder 中绘制用于视觉验证圆形/正方形对齐的测试图案。
///
/// 可以目视或截图检查：
/// 1. 红色正方形 (100x100) 是否与外接的半透明蓝色圆形 (100x100, r=50) 对齐
/// 2. 绿色胶囊形/圆角矩形是否正确内接
fn draw_shape_test_patterns(encoder: &mut DrawEncoder, window_size: (u32, u32)) {
    let _ = window_size; // 未来可改用相对坐标
    use mondrian_ui_core::types::Rect;

    // --- 测试 1：100x100 红色正方形 + 半透明内接蓝色圆形 ---
    // 红色实心正方形：左上方
    encoder.draw_rect(
        Rect::new(30.0, 30.0, 100.0, 100.0),
        Color::from_hex(0xFF3333),
        0.0,
    );
    // 半透明蓝色圆形（corner_radius=50 → 完美内接）：与正方形完全重叠
    encoder.draw_rect(
        Rect::new(30.0, 30.0, 100.0, 100.0),
        Color { r: 0.2, g: 0.4, b: 0.9, a: 0.4 },
        50.0,
    );

    // --- 测试 2：100x100 绿色圆形，旁边配 100x100 正方形参考 ---
    // 半透明绿色圆形
    encoder.draw_rect(
        Rect::new(160.0, 30.0, 100.0, 100.0),
        Color { r: 0.3, g: 0.9, b: 0.3, a: 0.3 },
        50.0,
    );
    // 白色轮廓正方形（border=2，通过绘制白色边框矩形模拟）
    encoder.draw_rect(
        Rect::new(160.0, 30.0, 100.0, 100.0),
        Color { r: 1.0, g: 1.0, b: 1.0, a: 0.2 },
        0.0,
    );

    // --- 测试 3：不同半径的圆角矩形 ---
    // 圆角 r=10
    encoder.draw_rect(
        Rect::new(30.0, 155.0, 80.0, 50.0),
        Color::from_hex(0xFF9800),
        10.0,
    );
    // 圆角 r=25 (胶囊形短轴)
    encoder.draw_rect(
        Rect::new(120.0, 155.0, 50.0, 80.0),
        Color::from_hex(0x9C27B0),
        25.0,
    );

    // --- 测试 4：小圆形 (40x40, r=20) ---
    encoder.draw_rect(
        Rect::new(200.0, 170.0, 40.0, 40.0),
        Color::from_hex(0xFFEB3B),
        20.0,
    );

    // --- 测试 5：细长胶囊形 (30x100, r=15) ---
    encoder.draw_rect(
        Rect::new(270.0, 155.0, 30.0, 100.0),
        Color::from_hex(0x00BCD4),
        15.0,
    );

    // --- 标签（用有色矩形模拟，文字渲染暂未集成）---
    use mondrian_ui_core::types::Point;
    // 绘制纯色圆点标记圆心位置
    encoder.draw_rect(
        Rect::new(78.0, 78.0, 4.0, 4.0),
        Color::from_hex(0xFFFFFF),
        0.0,
    );
    encoder.draw_rect(
        Rect::new(208.0, 78.0, 4.0, 4.0),
        Color::from_hex(0xFFFFFF),
        0.0,
    );
    encoder.draw_rect(
        Rect::new(218.0, 188.0, 4.0, 4.0),
        Color::from_hex(0xFFFFFF),
        0.0,
    );
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
                draw_shape_test_patterns(&mut encoder, (size.width, size.height));
                let commands = encoder.finish();

                let current = surface.get_current_texture();
                match current {
                    wgpu::CurrentSurfaceTexture::Success(output)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                        let v = output.texture.create_view(&Default::default());
                        ui_renderer.render(
                            &device,
                            &queue,
                            &v,
                            &commands,
                            (size.width, size.height),
                        );
                        output.present();
                    }
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => {
                        // skip frame
                    }
                    wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
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
