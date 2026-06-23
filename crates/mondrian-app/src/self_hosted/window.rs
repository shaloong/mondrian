#![allow(deprecated)]
//! Mondrian self-hosted winit/wgpu product window.
//!
//! Binary entrypoints stay thin and call this module. The product shell owns
//! native event-loop wiring, renderer setup, shell command application, and the
//! bridge between widget-dispatched actions and `AppState`.

use std::sync::Arc;

use crate::app::ui_actions::app_shell_quit_action;
use crate::app::AppState;
use crate::self_hosted::action_queue::PendingUiActions;
use crate::self_hosted::host::{SelfHostedShellCommands, SelfHostedUiHost, SelfHostedUiMode};
use crate::self_hosted::rendering::{SelfHostedFrameRenderer, SelfHostedRenderDiagnosticReporter};
use crate::self_hosted::runtime::{
    winit_cursor_icon_for_ui_state, winit_modifiers_to_ui_modifiers,
    winit_mouse_button_to_ui_button, winit_scroll_delta_to_ui_delta, WinitUiRuntime,
};
use crate::self_hosted::shortcuts::{register_shortcuts, SelfHostedShortcutOverride};
use crate::self_hosted::startup::{STARTUP_WINDOW_HEIGHT, STARTUP_WINDOW_WIDTH};
use mondrian_platform::SystemPlatformService;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{ShortcutManager, ShortcutScope};
use mondrian_ui_core::types::*;
use mondrian_ui_core::TreeWalker;
use mondrian_ui_events::EventRouter;
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_tooltip::TooltipManagerImpl;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

// ═══════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════

pub(crate) const DEFAULT_SELF_HOSTED_LOG_FILTER: &str =
    "info,wgpu_core=warn,wgpu_hal=warn,naga=warn";
pub(crate) const SELF_HOSTED_BACKGROUND_WORKERS: usize = 4;
const WORKSPACE_WINDOW_WIDTH: f32 = 1600.0;
const WORKSPACE_WINDOW_HEIGHT: f32 = 900.0;
const WORKSPACE_MIN_WIDTH: f32 = 1024.0;
const WORKSPACE_MIN_HEIGHT: f32 = 600.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfHostedWindowRole {
    Startup,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct WindowChrome {
    title: &'static str,
    width: f32,
    height: f32,
    transparent: bool,
    decorations: bool,
    rounded_corners: bool,
    resizable: bool,
    min_size: Option<(f32, f32)>,
    max_size: Option<(f32, f32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowCornerPreference {
    Default,
    Round,
}

struct SelfHostedWindowSession {
    role: SelfHostedWindowRole,
    window: Arc<winit::window::Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    frame_renderer: SelfHostedFrameRenderer,
    render_diagnostic_reporter: SelfHostedRenderDiagnosticReporter,
    router: EventRouter,
    ui_runtime: WinitUiRuntime,
    last_cursor: Point,
    last_window_cursor_icon: Option<winit::window::CursorIcon>,
    current_bounds: std::cell::Cell<Rect>,
    modifiers_state: Modifiers,
    pending_initial_redraw: bool,
}

/// Run the self-hosted Mondrian editor window.
pub fn run_self_hosted_app() -> Result<(), Box<dyn std::error::Error>> {
    let _background_runtime = build_self_hosted_background_runtime()?;
    let _background_runtime_guard = _background_runtime.enter();
    init_self_hosted_tracing();

    tracing::info!("Mondrian self-hosted UI starting");

    use winit::event_loop::EventLoop;
    let event_loop = EventLoop::new()?;
    let startup_window = Arc::new(
        event_loop.create_window(window_attributes_for_role(SelfHostedWindowRole::Startup))?,
    );

    let instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    let instance = wgpu::Instance::new(instance_desc);
    let startup_surface = instance.create_surface(startup_window.clone())?;

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&startup_surface),
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|_| "No suitable GPU adapter")?;

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;

    let mut host = SelfHostedUiHost::new(AppState::new());
    let mut session = SelfHostedWindowSession::from_window_and_surface(
        SelfHostedWindowRole::Startup,
        startup_window,
        startup_surface,
        &adapter,
        &device,
        &mut host,
    )?;
    let pending_actions = PendingUiActions::default();
    let platform = SystemPlatformService;

    tracing::info!(
        "UI initialized — {}x{}",
        session.config.width,
        session.config.height
    );
    session.window.set_visible(true);
    session.window.request_redraw();

    event_loop.run(move |event, elwt| {
        use winit::event::ElementState;
        use winit::event::{Event, WindowEvent};
        use winit::event_loop::ControlFlow;
        elwt.set_control_flow(ControlFlow::Wait);
        let dispatch_action = |action| pending_actions.push(action);

        match event {
            Event::WindowEvent { window_id, event } if window_id == session.window.id() => {
                match event {
                    WindowEvent::CloseRequested => {
                        pending_actions.push(native_close_request_action());
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                    }

                    WindowEvent::ModifiersChanged(modifiers) => {
                        session.modifiers_state = winit_modifiers_to_ui_modifiers(modifiers);
                    }

                    WindowEvent::Focused(false) => {
                        session.modifiers_state = Modifiers::none();
                        if should_route_focus_lost_to_ui(session.ui_runtime.is_eyedropper_active())
                        {
                            let _ = session.ui_runtime.route_window_event(
                                &session.window,
                                &mut session.router,
                                host.active_root_mut(),
                                UiEvent::FocusLost,
                                &dispatch_action,
                            );
                            drain_actions_and_sync_window_session(
                                &mut host,
                                &pending_actions,
                                &platform,
                                elwt,
                                &instance,
                                &adapter,
                                &device,
                                &mut session,
                            );
                            host.sync_workspace_layout_from_root();
                        } else {
                            elwt.set_control_flow(ControlFlow::Poll);
                        }
                        session.window.request_redraw();
                    }

                    WindowEvent::KeyboardInput { event: key_event, .. } => {
                        let result = session.ui_runtime.route_keyboard_input(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            &key_event,
                            &mut session.modifiers_state,
                            &dispatch_action,
                        );
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        if result == EventResult::Ignored
                            && should_exit_on_ignored_keyboard_input(
                                session.role,
                                &key_event.logical_key,
                            )
                        {
                            elwt.exit();
                        }
                        update_window_cursor_icon(&host, &mut session);
                        session.window.request_redraw();
                    }

                    WindowEvent::Ime(ime) => {
                        let _ = session.ui_runtime.route_ime_event(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            ime,
                            &dispatch_action,
                        );
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        session.window.request_redraw();
                    }

                    WindowEvent::RedrawRequested => {
                        host.refresh_if_dirty(session.current_bounds.get());
                        sync_window_session_role(
                            &mut host,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        let mut encoder = DrawEncoder::new();
                        let theme = mondrian_ui_theme::current_theme();
                        let b = session.current_bounds.get();
                        if session.role == SelfHostedWindowRole::Workspace {
                            encoder.draw_rect(b, theme.colors.background, 0.0);
                        }
                        TreeWalker::paint_clipped(host.active_root(), &mut encoder, &theme, b);
                        session.ui_runtime.paint_shell_overlays(
                            &mut encoder,
                            &theme,
                            b,
                            session.last_cursor,
                            &session.router,
                        );
                        let size = session.window.inner_size();
                        let frame_result = session.frame_renderer.render_draw_commands(
                            &device,
                            &queue,
                            &session.surface,
                            &session.config,
                            (size.width, size.height),
                            encoder.finish(),
                        );
                        if let Some(diagnostics) =
                            session.render_diagnostic_reporter.changed_failure(frame_result)
                        {
                            tracing::warn!(
                                "self-hosted UI render resource failures: missing_glyphs={}, raster_image_failures={}",
                                diagnostics.text_missing_glyphs,
                                diagnostics.raster_image_failures
                            );
                            host.mark_dirty();
                            session.window.request_redraw();
                        }
                        if frame_result.needs_follow_up_redraw() {
                            session.window.request_redraw();
                        }
                        session.pending_initial_redraw = false;
                    }

                    WindowEvent::Resized(new_size) => {
                        if new_size.width > 0 && new_size.height > 0 {
                            session.config.width = new_size.width;
                            session.config.height = new_size.height;
                            session.surface.configure(&device, &session.config);
                            let b = Rect::new(
                                0.0,
                                0.0,
                                new_size.width as f32,
                                new_size.height as f32,
                            );
                            session.current_bounds.set(b);
                            TreeWalker::layout(host.active_root_mut(), b);
                            session.window.request_redraw();
                        }
                    }

                    WindowEvent::HoveredFile(path) => {
                        let _ = session.ui_runtime.route_hovered_file(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            path,
                            session.last_cursor,
                            &dispatch_action,
                        );
                        session.window.request_redraw();
                    }

                    WindowEvent::HoveredFileCancelled => {
                        let _ = session.ui_runtime.route_hovered_file_cancelled(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            &dispatch_action,
                        );
                        session.window.request_redraw();
                    }

                    WindowEvent::DroppedFile(path) => {
                        let ui_path = path.clone();
                        let paths = vec![path];
                        let result = session.ui_runtime.route_dropped_file(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            ui_path,
                            session.last_cursor,
                            &dispatch_action,
                        );
                        if result == EventResult::Ignored {
                            pending_actions.push(mondrian_editor_state::Action::ImportMedia(paths));
                        }
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        session.window.request_redraw();
                    }

                    WindowEvent::CursorMoved { position, .. } => {
                        session.last_cursor = Point::new(position.x as f32, position.y as f32);
                        session
                            .ui_runtime
                            .update_eyedropper_preview_at_window_point(
                                &session.window,
                                session.last_cursor,
                            );
                        let _ = session.ui_runtime.route_window_event(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            UiEvent::MouseMove {
                                position: session.last_cursor,
                                modifiers: session.modifiers_state,
                            },
                            &dispatch_action,
                        );
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        update_window_cursor_icon(&host, &mut session);
                    }

                    WindowEvent::MouseInput { state, button, .. } => {
                        let sync_workspace_layout = state == ElementState::Released
                            && button == winit::event::MouseButton::Left;
                        let evt = match state {
                            ElementState::Pressed => UiEvent::MouseDown {
                                position: session.last_cursor,
                                button: winit_mouse_button_to_ui_button(button),
                                modifiers: session.modifiers_state,
                            },
                            ElementState::Released => UiEvent::MouseUp {
                                position: session.last_cursor,
                                button: winit_mouse_button_to_ui_button(button),
                                modifiers: session.modifiers_state,
                            },
                        };
                        let is_press =
                            matches!(evt, UiEvent::MouseDown { button: MouseButton::Left, .. });
                        if is_press && session.ui_runtime.is_eyedropper_active() {
                            session.ui_runtime.finish_eyedropper_at_window_point(
                                &session.window,
                                &mut session.router,
                                host.active_root_mut(),
                                session.last_cursor,
                                &dispatch_action,
                            );
                        } else {
                            let _ = session.ui_runtime.route_window_event(
                                &session.window,
                                &mut session.router,
                                host.active_root_mut(),
                                evt,
                                &dispatch_action,
                            );
                        }
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        if sync_workspace_layout {
                            host.sync_workspace_layout_from_root();
                        }
                        update_window_cursor_icon(&host, &mut session);
                        session.window.request_redraw();
                    }

                    WindowEvent::MouseWheel { delta, .. } => {
                        let _ = session.ui_runtime.route_window_event(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            UiEvent::MouseWheel {
                                delta: winit_scroll_delta_to_ui_delta(delta),
                                position: session.last_cursor,
                                modifiers: session.modifiers_state,
                            },
                            &dispatch_action,
                        );
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        session.window.request_redraw();
                    }

                    _ => {}
                }
            }

            Event::AboutToWait => {
                session
                    .ui_runtime
                    .drive_timers(&session.window, &mut session.router, elwt);
                if host.poll_background_tasks(session.current_bounds.get()) {
                    sync_window_session_role(
                        &mut host,
                        elwt,
                        &instance,
                        &adapter,
                        &device,
                        &mut session,
                    );
                    session.window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
                if session.pending_initial_redraw {
                    session.window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
                if session.ui_runtime.is_eyedropper_active() {
                    session.ui_runtime.poll_eyedropper(
                        &session.window,
                        &mut session.router,
                        host.active_root_mut(),
                        &mut session.last_cursor,
                        session.modifiers_state,
                        &dispatch_action,
                    );
                    drain_actions_and_sync_window_session(
                        &mut host,
                        &pending_actions,
                        &platform,
                        elwt,
                        &instance,
                        &adapter,
                        &device,
                        &mut session,
                    );
                    session.window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
            }
            _ => {}
        }
    })?;

    Ok(())
}

fn init_self_hosted_tracing() {
    let filter = self_hosted_log_filter();
    if tracing_subscriber::registry().with(filter).try_init().is_err() {
        tracing::debug!("tracing subscriber already initialized; self-hosted filter skipped");
    }
}

fn self_hosted_log_filter() -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_SELF_HOSTED_LOG_FILTER))
}

fn build_self_hosted_background_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(SELF_HOSTED_BACKGROUND_WORKERS)
        .thread_name("mondrian-bg")
        .enable_all()
        .build()
}

fn preferred_self_hosted_surface_format(
    surface: &wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    fallback: wgpu::TextureFormat,
) -> wgpu::TextureFormat {
    surface
        .get_capabilities(adapter)
        .formats
        .into_iter()
        .find(|format| is_srgb_surface_format(*format))
        .unwrap_or(fallback)
}

fn is_srgb_surface_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Rgba8UnormSrgb
    )
}

fn should_route_focus_lost_to_ui(eyedropper_active: bool) -> bool {
    !eyedropper_active
}

fn should_exit_on_ignored_keyboard_input(
    _role: SelfHostedWindowRole,
    _key: &winit::keyboard::Key,
) -> bool {
    false
}

fn native_close_request_action() -> mondrian_editor_state::Action {
    app_shell_quit_action()
}

impl SelfHostedWindowSession {
    fn from_window_and_surface(
        role: SelfHostedWindowRole,
        window: Arc<winit::window::Window>,
        surface: wgpu::Surface<'static>,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        host: &mut SelfHostedUiHost,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        apply_window_corner_preference(&window, window_corner_preference_for_role(role));

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(adapter, size.width, size.height)
            .ok_or("Failed surface config")?;
        config.format = preferred_self_hosted_surface_format(&surface, adapter, config.format);
        surface.configure(device, &config);

        let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
        TreeWalker::layout(host.active_root_mut(), bounds);
        Ok(Self {
            role,
            window,
            surface,
            config: config.clone(),
            frame_renderer: SelfHostedFrameRenderer::new(device, config.format),
            render_diagnostic_reporter: SelfHostedRenderDiagnosticReporter::default(),
            router: build_event_router(
                host.active_root().id(),
                &host.preferences().shortcut_overrides,
            ),
            ui_runtime: WinitUiRuntime::new(),
            last_cursor: Point::new(0.0, 0.0),
            last_window_cursor_icon: None,
            current_bounds: std::cell::Cell::new(bounds),
            modifiers_state: Modifiers::none(),
            pending_initial_redraw: true,
        })
    }
}

fn build_event_router(
    root_id: mondrian_ui_core::types::WidgetId,
    shortcut_overrides: &[SelfHostedShortcutOverride],
) -> EventRouter {
    let mut router = EventRouter::with_platform_and_tooltip(
        root_id,
        Box::new(SystemPlatformService),
        Box::new(TooltipManagerImpl::new(450)),
    );
    register_shortcuts(&mut router, shortcut_overrides);
    router
}

fn drain_actions_and_sync_window_session(
    host: &mut SelfHostedUiHost,
    pending_actions: &PendingUiActions,
    platform: &dyn mondrian_platform::PlatformService,
    elwt: &winit::event_loop::ActiveEventLoop,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    session: &mut SelfHostedWindowSession,
) {
    let commands =
        host.drain_pending_actions(pending_actions, session.current_bounds.get(), platform);
    rebuild_global_shortcuts(&mut session.router, &host.preferences().shortcut_overrides);
    apply_shell_commands(commands, &session.window, elwt);
    sync_window_session_role(host, elwt, instance, adapter, device, session);
}

fn rebuild_global_shortcuts(
    router: &mut EventRouter,
    shortcut_overrides: &[SelfHostedShortcutOverride],
) {
    router.shortcut_manager_mut().clear_scope(ShortcutScope::Global);
    register_shortcuts(router, shortcut_overrides);
}

fn sync_window_session_role(
    host: &mut SelfHostedUiHost,
    elwt: &winit::event_loop::ActiveEventLoop,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    session: &mut SelfHostedWindowSession,
) {
    let target_role = window_role_for_mode(host.mode());
    if session.role == target_role {
        return;
    }
    if let Err(err) =
        replace_window_session(target_role, elwt, instance, adapter, device, host, session)
    {
        tracing::error!("failed to replace self-hosted native window: {err}");
        elwt.exit();
    }
}

fn replace_window_session(
    role: SelfHostedWindowRole,
    elwt: &winit::event_loop::ActiveEventLoop,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    host: &mut SelfHostedUiHost,
    session: &mut SelfHostedWindowSession,
) -> Result<(), Box<dyn std::error::Error>> {
    let old_role = session.role;
    session.window.set_visible(false);

    let window = Arc::new(elwt.create_window(window_attributes_for_role(role))?);
    let surface = instance.create_surface(window.clone())?;
    let next_session = SelfHostedWindowSession::from_window_and_surface(
        role, window, surface, adapter, device, host,
    )?;
    tracing::info!(?old_role, ?role, "self-hosted native window replaced");
    next_session.window.set_visible(true);
    next_session.window.request_redraw();
    *session = next_session;
    Ok(())
}

fn window_role_for_mode(mode: SelfHostedUiMode) -> SelfHostedWindowRole {
    match mode {
        SelfHostedUiMode::Startup => SelfHostedWindowRole::Startup,
        SelfHostedUiMode::Workspace => SelfHostedWindowRole::Workspace,
    }
}

fn window_chrome_for_role(role: SelfHostedWindowRole) -> WindowChrome {
    match role {
        SelfHostedWindowRole::Startup => WindowChrome {
            title: "Mondrian",
            width: STARTUP_WINDOW_WIDTH,
            height: STARTUP_WINDOW_HEIGHT,
            transparent: true,
            decorations: false,
            rounded_corners: true,
            resizable: false,
            min_size: Some((STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT)),
            max_size: Some((STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT)),
        },
        SelfHostedWindowRole::Workspace => WindowChrome {
            title: "Mondrian",
            width: WORKSPACE_WINDOW_WIDTH,
            height: WORKSPACE_WINDOW_HEIGHT,
            transparent: false,
            decorations: false,
            rounded_corners: true,
            resizable: true,
            min_size: Some((WORKSPACE_MIN_WIDTH, WORKSPACE_MIN_HEIGHT)),
            max_size: None,
        },
    }
}

fn logical_size(width: f32, height: f32) -> winit::dpi::LogicalSize<f64> {
    winit::dpi::LogicalSize::new(width as f64, height as f64)
}

fn window_attributes_for_role(role: SelfHostedWindowRole) -> winit::window::WindowAttributes {
    let chrome = window_chrome_for_role(role);
    let mut attrs = winit::window::Window::default_attributes()
        .with_title(chrome.title)
        .with_inner_size(logical_size(chrome.width, chrome.height))
        .with_transparent(chrome.transparent)
        .with_decorations(chrome.decorations)
        .with_resizable(chrome.resizable)
        .with_visible(false);
    if let Some((w, h)) = chrome.min_size {
        attrs = attrs.with_min_inner_size(logical_size(w, h));
    }
    if let Some((w, h)) = chrome.max_size {
        attrs = attrs.with_max_inner_size(logical_size(w, h));
    }
    attrs
}

fn window_corner_preference_for_role(role: SelfHostedWindowRole) -> WindowCornerPreference {
    if window_chrome_for_role(role).rounded_corners {
        WindowCornerPreference::Round
    } else {
        WindowCornerPreference::Default
    }
}

fn apply_window_corner_preference(
    window: &winit::window::Window,
    preference: WindowCornerPreference,
) {
    platform_window_chrome::apply_window_corner_preference(window, preference);
}

fn update_window_cursor_icon(host: &SelfHostedUiHost, session: &mut SelfHostedWindowSession) {
    let next = window_cursor_icon(host, session);
    if session.last_window_cursor_icon != Some(next) {
        session.window.set_cursor_icon(next);
        session.last_window_cursor_icon = Some(next);
    }
}

fn window_cursor_icon(
    host: &SelfHostedUiHost,
    session: &SelfHostedWindowSession,
) -> winit::window::CursorIcon {
    winit_cursor_icon_for_ui_state(
        session.ui_runtime.is_eyedropper_active(),
        splitter_direction_at_cursor(host, session.last_cursor),
        focused_widget_accepts_text_input(
            host.active_root(),
            session.router.focus_manager().focused_widget(),
        ),
    )
}

fn splitter_direction_at_cursor(host: &SelfHostedUiHost, cursor: Point) -> Option<SplitDirection> {
    if host.mode() != SelfHostedUiMode::Workspace {
        return None;
    }
    host.root()
        .dock()
        .collect_grab_zones()
        .iter()
        .find(|(zone, _)| zone.contains(cursor))
        .map(|(_, direction)| *direction)
}

fn focused_widget_accepts_text_input(
    root: &dyn mondrian_ui_core::Widget,
    focused: Option<WidgetId>,
) -> bool {
    focused.is_some_and(|id| widget_tree_accepts_text_input(root, id))
}

fn widget_tree_accepts_text_input(
    widget: &dyn mondrian_ui_core::Widget,
    focused: WidgetId,
) -> bool {
    if widget.id() == focused {
        return widget.accepts_text_input();
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            if widget_tree_accepts_text_input(child, focused) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
fn window_bounds_for_role(role: SelfHostedWindowRole) -> Rect {
    let chrome = window_chrome_for_role(role);
    Rect::new(0.0, 0.0, chrome.width, chrome.height)
}

fn apply_shell_commands(
    commands: SelfHostedShellCommands,
    window: &winit::window::Window,
    elwt: &winit::event_loop::ActiveEventLoop,
) {
    if commands.toggle_fullscreen {
        toggle_window_fullscreen(window);
    }
    if commands.toggle_maximize {
        window.set_maximized(!window.is_maximized());
    }
    if commands.minimize {
        window.set_minimized(true);
    }
    if commands.begin_window_drag {
        let _ = window.drag_window();
    }
    if commands.quit {
        elwt.exit();
    }
}

fn toggle_window_fullscreen(window: &winit::window::Window) {
    if window.fullscreen().is_some() {
        window.set_fullscreen(None);
    } else {
        window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(
            window.current_monitor(),
        )));
    }
}

#[cfg(target_os = "windows")]
mod platform_window_chrome {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    use super::WindowCornerPreference;

    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_DEFAULT: u32 = 0;
    const DWMWCP_ROUND: u32 = 2;

    pub(super) fn apply_window_corner_preference(
        window: &winit::window::Window,
        preference: WindowCornerPreference,
    ) {
        let Ok(window_handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = window_handle.as_raw() else {
            return;
        };

        let preference = match preference {
            WindowCornerPreference::Default => DWMWCP_DEFAULT,
            WindowCornerPreference::Round => DWMWCP_ROUND,
        };
        let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;

        // SAFETY: winit owns a live top-level HWND on this thread and the
        // attribute payload is a pointer to a properly sized DWORD value for
        // the duration of the call.
        let result = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                std::ptr::addr_of!(preference).cast(),
                std::mem::size_of_val(&preference) as u32,
            )
        };
        if result < 0 {
            tracing::debug!(
                hresult = result,
                "failed to apply Windows DWM window corner preference"
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform_window_chrome {
    use super::WindowCornerPreference;

    pub(super) fn apply_window_corner_preference(
        _window: &winit::window::Window,
        _preference: WindowCornerPreference,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::widget::{EventContext, PaintContext};
    use mondrian_ui_core::Widget;

    #[test]
    fn srgb_surface_format_detection_matches_presentation_formats() {
        assert!(is_srgb_surface_format(wgpu::TextureFormat::Bgra8UnormSrgb));
        assert!(is_srgb_surface_format(wgpu::TextureFormat::Rgba8UnormSrgb));
        assert!(!is_srgb_surface_format(wgpu::TextureFormat::Bgra8Unorm));
        assert!(!is_srgb_surface_format(wgpu::TextureFormat::Rgba8Unorm));
    }

    struct CursorFocusWidget {
        id: WidgetId,
        accepts_text: bool,
        children: Vec<Box<dyn Widget>>,
    }

    impl CursorFocusWidget {
        fn new(accepts_text: bool) -> Self {
            Self {
                id: WidgetId::new(),
                accepts_text,
                children: Vec::new(),
            }
        }

        fn with_children(children: Vec<Box<dyn Widget>>) -> Self {
            Self { id: WidgetId::new(), accepts_text: false, children }
        }
    }

    impl Widget for CursorFocusWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::ZERO
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn child_count(&self) -> usize {
            self.children.len()
        }

        fn child(&self, index: usize) -> Option<&dyn Widget> {
            self.children.get(index).map(|child| child.as_ref())
        }

        fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
            match self.children.get_mut(index) {
                Some(child) => Some(child.as_mut()),
                None => None,
            }
        }

        fn accepts_text_input(&self) -> bool {
            self.accepts_text
        }
    }

    #[test]
    fn default_log_filter_keeps_noisy_gpu_crates_at_warning() {
        assert!(DEFAULT_SELF_HOSTED_LOG_FILTER.contains("wgpu_core=warn"));
        assert!(DEFAULT_SELF_HOSTED_LOG_FILTER.contains("wgpu_hal=warn"));
        assert!(DEFAULT_SELF_HOSTED_LOG_FILTER.contains("naga=warn"));
    }

    #[test]
    fn background_runtime_uses_product_worker_count() {
        assert_eq!(SELF_HOSTED_BACKGROUND_WORKERS, 4);
        let runtime = build_self_hosted_background_runtime().expect("runtime should build");
        runtime.block_on(async {});
    }

    #[test]
    fn focus_loss_is_deferred_while_desktop_eyedropper_is_active() {
        assert!(!should_route_focus_lost_to_ui(true));
        assert!(should_route_focus_lost_to_ui(false));
    }

    #[test]
    fn ignored_keyboard_input_never_exits_native_windows() {
        for role in [
            SelfHostedWindowRole::Startup,
            SelfHostedWindowRole::Workspace,
        ] {
            assert!(!should_exit_on_ignored_keyboard_input(
                role,
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
            ));
            assert!(!should_exit_on_ignored_keyboard_input(
                role,
                &winit::keyboard::Key::Character("q".into()),
            ));
        }
    }

    #[test]
    fn native_close_request_uses_app_shell_quit_action() {
        assert_eq!(native_close_request_action(), app_shell_quit_action());
    }

    #[test]
    fn focused_widget_accepts_text_input_finds_nested_text_owner() {
        let text_child = CursorFocusWidget::new(true);
        let text_id = text_child.id;
        let button_child = CursorFocusWidget::new(false);
        let button_id = button_child.id;
        let root = CursorFocusWidget::with_children(vec![
            Box::new(button_child),
            Box::new(CursorFocusWidget::with_children(vec![Box::new(text_child)])),
        ]);

        assert!(focused_widget_accepts_text_input(&root, Some(text_id)));
        assert!(!focused_widget_accepts_text_input(&root, Some(button_id)));
        assert!(!focused_widget_accepts_text_input(&root, None));
        assert!(!focused_widget_accepts_text_input(
            &root,
            Some(WidgetId::new())
        ));
    }

    #[test]
    fn rebuilding_global_shortcuts_applies_overrides_immediately() {
        use crate::self_hosted::shortcuts::{SelfHostedShortcutBinding, SelfHostedShortcutKey};
        use mondrian_editor_state::Action;
        use mondrian_ui_core::shortcut::{ShortcutContext, ShortcutManager};

        let mut router = build_event_router(WidgetId::new(), &[]);
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::S,
                Modifiers::ctrl(),
                ShortcutContext::default(),
            ),
            Some(Action::SaveProject)
        );

        let overrides = vec![SelfHostedShortcutOverride {
            id: "file.save_project".to_owned(),
            binding: Some(SelfHostedShortcutBinding {
                key: SelfHostedShortcutKey::I,
                ctrl: true,
                alt: true,
                shift: false,
                meta: false,
            }),
        }];

        rebuild_global_shortcuts(&mut router, &overrides);

        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::S,
                Modifiers::ctrl(),
                ShortcutContext::default(),
            ),
            None
        );
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::I,
                Modifiers { ctrl: true, alt: true, shift: false, meta: false },
                ShortcutContext::default(),
            ),
            Some(Action::SaveProject)
        );
    }

    #[test]
    fn startup_window_chrome_is_fixed_and_undecorated() {
        let chrome = window_chrome_for_role(SelfHostedWindowRole::Startup);

        assert_eq!(chrome.title, "Mondrian");
        assert_eq!(chrome.width, STARTUP_WINDOW_WIDTH);
        assert_eq!(chrome.height, STARTUP_WINDOW_HEIGHT);
        assert!(chrome.transparent);
        assert!(!chrome.decorations);
        assert!(chrome.rounded_corners);
        assert!(!chrome.resizable);
        assert_eq!(
            chrome.min_size,
            Some((STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT))
        );
        assert_eq!(
            chrome.max_size,
            Some((STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT))
        );
    }

    #[test]
    fn workspace_window_chrome_is_resizable_product_workspace() {
        let chrome = window_chrome_for_role(SelfHostedWindowRole::Workspace);

        assert_eq!(chrome.title, "Mondrian");
        assert_eq!(chrome.width, WORKSPACE_WINDOW_WIDTH);
        assert_eq!(chrome.height, WORKSPACE_WINDOW_HEIGHT);
        assert!(!chrome.transparent);
        assert!(!chrome.decorations);
        assert!(chrome.rounded_corners);
        assert!(chrome.resizable);
        assert_eq!(
            chrome.min_size,
            Some((WORKSPACE_MIN_WIDTH, WORKSPACE_MIN_HEIGHT))
        );
        assert_eq!(chrome.max_size, None);
    }

    #[test]
    fn window_role_bounds_match_requested_chrome_size() {
        for role in [
            SelfHostedWindowRole::Startup,
            SelfHostedWindowRole::Workspace,
        ] {
            let chrome = window_chrome_for_role(role);
            let bounds = window_bounds_for_role(role);

            assert_eq!(bounds.x, 0.0);
            assert_eq!(bounds.y, 0.0);
            assert_eq!(bounds.width, chrome.width);
            assert_eq!(bounds.height, chrome.height);
        }
    }

    #[test]
    fn ui_modes_map_to_distinct_native_window_roles() {
        assert_eq!(
            window_role_for_mode(SelfHostedUiMode::Startup),
            SelfHostedWindowRole::Startup
        );
        assert_eq!(
            window_role_for_mode(SelfHostedUiMode::Workspace),
            SelfHostedWindowRole::Workspace
        );
    }

    #[test]
    fn workspace_window_requests_platform_rounded_corners() {
        assert_eq!(
            window_corner_preference_for_role(SelfHostedWindowRole::Startup),
            WindowCornerPreference::Round
        );
        assert_eq!(
            window_corner_preference_for_role(SelfHostedWindowRole::Workspace),
            WindowCornerPreference::Round
        );
    }
}
