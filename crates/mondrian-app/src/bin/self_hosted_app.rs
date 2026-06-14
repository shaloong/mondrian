#![allow(deprecated)]
//! Mondrian 新 UI 主窗口
//!
//! 使用自研 UI 框架（winit + wgpu + Dock + Widget）的应用入口。
//! 运行: cargo run --bin self_hosted_app

use std::sync::Arc;

use mondrian_app::app::AppState;
use mondrian_app::self_hosted::action_queue::PendingUiActions;
use mondrian_app::self_hosted::host::SelfHostedUiHost;
use mondrian_app::self_hosted::runtime::WinitUiRuntime;
use mondrian_panel_console::tracing_layer::ConsoleLogLayer;
use mondrian_platform::SystemPlatformService;
use mondrian_ui_core::types::*;
use mondrian_ui_core::{TreeWalker, Widget};
use mondrian_ui_events::EventRouter;
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_renderer::UiRenderer;
use mondrian_ui_text::{resolve_text_commands, TextRenderer};
use mondrian_ui_tooltip::TooltipManagerImpl;
use tracing_subscriber::prelude::*;

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

    let mut host = SelfHostedUiHost::new(AppState::new());
    let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    TreeWalker::layout(host.root_mut(), bounds);
    let mut router = EventRouter::with_platform_and_tooltip(
        host.root().id(),
        Box::new(SystemPlatformService),
        Box::new(TooltipManagerImpl::new(450)),
    );
    let mut ui_runtime = WinitUiRuntime::new();

    let mut last_cursor = Point::new(0.0, 0.0);
    let current_bounds = std::cell::Cell::new(bounds);
    let mut modifiers_state = Modifiers::none();
    let pending_actions = PendingUiActions::default();
    let platform = SystemPlatformService;

    tracing::info!("UI initialized — {}x{}", size.width, size.height);
    window.request_redraw();

    event_loop.run(move |event, elwt| {
        use winit::event::ElementState;
        use winit::event::{Event, WindowEvent};
        use winit::event_loop::ControlFlow;
        elwt.set_control_flow(ControlFlow::Wait);
        let dispatch_action = |action| pending_actions.push(action);

        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => elwt.exit(),

            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event: key_event, .. },
                ..
            } => {
                let is_escape = matches!(
                    key_event.logical_key,
                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                );
                let pressed = key_event.state == ElementState::Pressed;
                let result = ui_runtime.route_keyboard_input(
                    &window,
                    &mut router,
                    host.root_mut(),
                    &key_event,
                    &mut modifiers_state,
                    &dispatch_action,
                );
                host.drain_pending_actions(&pending_actions, current_bounds.get(), &platform);
                if pressed && is_escape && result == EventResult::Ignored {
                    elwt.exit();
                }
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::Ime(ime), .. } => {
                let _ = ui_runtime.route_ime_event(
                    &window,
                    &mut router,
                    host.root_mut(),
                    ime,
                    &dispatch_action,
                );
                host.drain_pending_actions(&pending_actions, current_bounds.get(), &platform);
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let mut encoder = DrawEncoder::new();
                let theme = mondrian_ui_theme::current_theme();
                let b = current_bounds.get();
                encoder.draw_rect(b, theme.colors.background, 0.0);
                TreeWalker::paint(host.root(), &mut encoder, &theme);
                ui_runtime.paint_shell_overlays(&mut encoder, &theme, b, last_cursor, &router);
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
                    TreeWalker::layout(host.root_mut(), b);
                    window.request_redraw();
                }
            }

            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. }, ..
            } => {
                last_cursor = Point::new(position.x as f32, position.y as f32);
                ui_runtime.update_eyedropper_preview_at_window_point(&window, last_cursor);
                let _ = ui_runtime.route_window_event(
                    &window,
                    &mut router,
                    host.root_mut(),
                    UiEvent::MouseMove {
                        position: last_cursor,
                        modifiers: Modifiers::none(),
                    },
                    &dispatch_action,
                );
                host.drain_pending_actions(&pending_actions, current_bounds.get(), &platform);
                let zones = host.root().dock().collect_grab_zones();
                let dir = zones.iter().find(|(z, _)| z.contains(last_cursor)).map(|(_, d)| *d);
                if ui_runtime.is_eyedropper_active() {
                    window.set_cursor_icon(winit::window::CursorIcon::Crosshair);
                } else {
                    window.set_cursor_icon(match dir {
                        Some(SplitDirection::Horizontal) => winit::window::CursorIcon::ColResize,
                        Some(SplitDirection::Vertical) => winit::window::CursorIcon::RowResize,
                        None => winit::window::CursorIcon::Default,
                    });
                }
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
                let is_press = matches!(evt, UiEvent::MouseDown { button: MouseButton::Left, .. });
                if is_press && ui_runtime.is_eyedropper_active() {
                    ui_runtime.finish_eyedropper_at_window_point(
                        &window,
                        &mut router,
                        host.root_mut(),
                        last_cursor,
                        &dispatch_action,
                    );
                    host.drain_pending_actions(&pending_actions, current_bounds.get(), &platform);
                } else {
                    let _ = ui_runtime.route_window_event(
                        &window,
                        &mut router,
                        host.root_mut(),
                        evt,
                        &dispatch_action,
                    );
                    host.drain_pending_actions(&pending_actions, current_bounds.get(), &platform);
                }
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::MouseWheel { delta, .. }, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => -y * 20.0,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32),
                };
                let _ = ui_runtime.route_window_event(
                    &window,
                    &mut router,
                    host.root_mut(),
                    UiEvent::MouseWheel {
                        delta: dy,
                        position: last_cursor,
                        modifiers: Modifiers::none(),
                    },
                    &dispatch_action,
                );
                host.drain_pending_actions(&pending_actions, current_bounds.get(), &platform);
                window.request_redraw();
            }

            Event::AboutToWait => {
                ui_runtime.drive_timers(&window, &mut router, elwt);
                if ui_runtime.is_eyedropper_active() {
                    ui_runtime.poll_eyedropper(
                        &window,
                        &mut router,
                        host.root_mut(),
                        &mut last_cursor,
                        &dispatch_action,
                    );
                    host.drain_pending_actions(&pending_actions, current_bounds.get(), &platform);
                    window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
            }
            _ => {}
        }
    })?;

    Ok(())
}
