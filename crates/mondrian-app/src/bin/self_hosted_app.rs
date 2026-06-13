#![allow(deprecated)]
//! Mondrian 新 UI 主窗口
//!
//! 使用自研 UI 框架（winit + wgpu + Dock + Widget）的应用入口。
//! 运行: cargo run --bin self_hosted_app

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use mondrian_app::app::ui_actions::{
    APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_OPEN_PROJECT_DIALOG,
    APP_SHELL_SAVE_PROJECT_AS_DIALOG,
};
use mondrian_app::app::AppState;
use mondrian_app::self_hosted::panels::SelfHostedPanelModels;
use mondrian_app::self_hosted::runtime::WinitUiRuntime;
use mondrian_app::self_hosted::shell::{
    media_import_filters, project_file_filters, SelfHostedAppRoot, PROJECT_FILE_EXTENSION,
};
use mondrian_editor_state::Action;
use mondrian_panel_console::tracing_layer::ConsoleLogLayer;
use mondrian_platform::{PlatformService, SystemPlatformService};
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

fn refresh_root_if_dirty(
    root: &mut SelfHostedAppRoot,
    app_state: &RefCell<AppState>,
    ui_dirty: &Cell<bool>,
    bounds: Rect,
) {
    if !ui_dirty.replace(false) {
        return;
    }
    root.set_models(SelfHostedPanelModels::from_app_state(&app_state.borrow()));
    TreeWalker::layout(root, bounds);
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

    let app_state = RefCell::new(AppState::new());
    let mut root =
        SelfHostedAppRoot::from_models(SelfHostedPanelModels::from_app_state(&app_state.borrow()));
    let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    TreeWalker::layout(&mut root, bounds);
    let mut router = EventRouter::with_platform_and_tooltip(
        root.id(),
        Box::new(SystemPlatformService),
        Box::new(TooltipManagerImpl::new(450)),
    );
    let mut ui_runtime = WinitUiRuntime::new();
    let ui_dirty = Cell::new(false);

    let mut last_cursor = Point::new(0.0, 0.0);
    let current_bounds = std::cell::Cell::new(bounds);

    tracing::info!("UI initialized — {}x{}", size.width, size.height);
    window.request_redraw();

    event_loop.run(move |event, elwt| {
        use winit::event::ElementState;
        use winit::event::{Event, WindowEvent};
        use winit::event_loop::ControlFlow;
        elwt.set_control_flow(ControlFlow::Wait);
        let dispatch_action = |action: Action| {
            let action = match action {
                Action::Custom { namespace, name, .. }
                    if namespace == APP_SHELL_NAMESPACE
                        && name == APP_SHELL_OPEN_PROJECT_DIALOG =>
                {
                    let platform = SystemPlatformService;
                    let Some(paths) =
                        platform.open_file_dialog("Open Mondrian Project", &project_file_filters())
                    else {
                        return;
                    };
                    let Some(path) = paths.into_iter().next() else {
                        return;
                    };
                    Action::OpenProject(path)
                }
                Action::Custom { namespace, name, .. }
                    if namespace == APP_SHELL_NAMESPACE
                        && name == APP_SHELL_IMPORT_MEDIA_DIALOG =>
                {
                    let platform = SystemPlatformService;
                    let Some(paths) =
                        platform.open_file_dialog("Import Media", &media_import_filters())
                    else {
                        return;
                    };
                    if paths.is_empty() {
                        return;
                    }
                    Action::ImportMedia(paths)
                }
                Action::Custom { namespace, name, .. }
                    if namespace == APP_SHELL_NAMESPACE
                        && name == APP_SHELL_SAVE_PROJECT_AS_DIALOG =>
                {
                    let platform = SystemPlatformService;
                    let default_name = app_state
                        .borrow()
                        .current_project_path
                        .as_ref()
                        .and_then(|path| {
                            path.file_name().and_then(|name| name.to_str()).map(str::to_string)
                        })
                        .unwrap_or_else(|| format!("untitled.{PROJECT_FILE_EXTENSION}"));
                    let Some(path) = platform.save_file_dialog(
                        "Save Mondrian Project As",
                        &default_name,
                        &project_file_filters(),
                    ) else {
                        return;
                    };
                    Action::SaveProjectAs(path)
                }
                action => action,
            };
            tracing::debug!(?action, "custom UI action");
            if let Err(err) = app_state.borrow_mut().dispatch_action(action) {
                tracing::warn!("custom UI action failed: {err}");
            } else {
                ui_dirty.set(true);
            }
        };

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
                    TreeWalker::layout(&mut root, b);
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
                    &mut root,
                    UiEvent::MouseMove {
                        position: last_cursor,
                        modifiers: Modifiers::none(),
                    },
                    &dispatch_action,
                );
                refresh_root_if_dirty(&mut root, &app_state, &ui_dirty, current_bounds.get());
                let zones = root.dock().collect_grab_zones();
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
                        &mut root,
                        last_cursor,
                        &dispatch_action,
                    );
                    refresh_root_if_dirty(&mut root, &app_state, &ui_dirty, current_bounds.get());
                } else {
                    let _ = ui_runtime.route_window_event(
                        &window,
                        &mut router,
                        &mut root,
                        evt,
                        &dispatch_action,
                    );
                    refresh_root_if_dirty(&mut root, &app_state, &ui_dirty, current_bounds.get());
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
                    &mut root,
                    UiEvent::MouseWheel {
                        delta: dy,
                        position: last_cursor,
                        modifiers: Modifiers::none(),
                    },
                    &dispatch_action,
                );
                refresh_root_if_dirty(&mut root, &app_state, &ui_dirty, current_bounds.get());
                window.request_redraw();
            }

            Event::AboutToWait => {
                ui_runtime.drive_timers(&window, &mut router, elwt);
                if ui_runtime.is_eyedropper_active() {
                    ui_runtime.poll_eyedropper(
                        &window,
                        &mut router,
                        &mut root,
                        &mut last_cursor,
                        &dispatch_action,
                    );
                    refresh_root_if_dirty(&mut root, &app_state, &ui_dirty, current_bounds.get());
                    window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
            }
            _ => {}
        }
    })?;

    Ok(())
}
