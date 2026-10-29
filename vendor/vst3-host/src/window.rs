//! Plugin window management
//!
//! This module provides platform-specific window creation and management
//! for VST3 plugin GUIs.

use crate::error::{Error, Result};
use crate::plugin::Plugin;
use std::sync::{Arc, Mutex};

/// Observations from one bounded turn of this exact native parent.
/// The container never invokes a foreign plugin or grants an authoring lease.
pub struct NativeEditorContainerEvents {
    /// Actual Windows queue result, including a real WM_QUIT request.
    #[cfg(target_os = "windows")]
    pub step: WindowsMessageStep,
    /// Native parent scaling, when the platform reports physical pixel scaling.
    pub scale: Option<f64>,
    /// Actual client size, in the platform's plugin embedding coordinates.
    pub client_size: Option<(i32, i32)>,
}

#[cfg(target_os = "macos")]
use objc2::{rc::Retained, MainThreadMarker, MainThreadOnly};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

#[cfg(target_os = "windows")]
use winapi::{
    shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM},
    shared::windef::{HWND, RECT},
    um::libloaderapi::GetModuleHandleW,
    um::winuser::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, LoadCursorW, RegisterClassExW,
        SetWindowPos, ShowWindow, UpdateWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, WM_CLOSE, WM_DPICHANGED, WNDCLASSEXW,
        WS_OVERLAPPEDWINDOW,
    },
};

#[cfg(any(test, target_os = "windows"))]
fn dpi_scale_factor(dpi: u32) -> Option<f32> {
    (dpi > 0).then_some(dpi as f32 / 96.0)
}

/// Editor windows whose window procedure has seen `WM_CLOSE`, keyed by `HWND` (as `usize`,
/// since `HWND` is a raw pointer and not `Send`).
///
/// `DefWindowProcW` answers `WM_CLOSE` by destroying the window, which would leave the plugin's
/// `IPlugView` attached to a freed `HWND`. [`plugin_window_proc`] records the request here
/// instead, so the host can tear the editor down in the right order.
#[cfg(target_os = "windows")]
fn close_requests() -> &'static Mutex<std::collections::HashSet<usize>> {
    static REQUESTS: std::sync::OnceLock<Mutex<std::collections::HashSet<usize>>> =
        std::sync::OnceLock::new();
    REQUESTS.get_or_init(Mutex::default)
}

/// Latest pending DPI for each editor window. The window procedure cannot call into the plugin:
/// `setContentScaleFactor` may synchronously enter arbitrary plugin/UI code, while callers often
/// hold the plugin mutex when Windows dispatches a message. The procedure records only the
/// newest DPI and [`PluginWindow::service_platform_events`] applies it after the callback returns.
#[cfg(target_os = "windows")]
fn dpi_changes() -> &'static Mutex<std::collections::HashMap<usize, u32>> {
    static CHANGES: std::sync::OnceLock<Mutex<std::collections::HashMap<usize, u32>>> =
        std::sync::OnceLock::new();
    CHANGES.get_or_init(Mutex::default)
}

#[cfg(target_os = "windows")]
fn dpi_from_wparam(wparam: WPARAM) -> Option<u32> {
    // WM_DPICHANGED packs the new X DPI into LOWORD and Y DPI into HIWORD. VST3 has one content
    // scale, and Windows normally reports both equally, so use X as Microsoft recommends.
    let dpi = (wparam & 0xffff) as u32;
    (dpi > 0).then_some(dpi)
}

/// Window procedure for plugin editor windows: record control-plane work and defer plugin calls
/// until after Windows returns from the callback.
///
/// # Safety
///
/// Called by the OS with the arguments of a window procedure; all it does with them is forward
/// them to `DefWindowProcW`.
#[cfg(target_os = "windows")]
unsafe extern "system" fn plugin_window_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CLOSE {
        if let Ok(mut requests) = close_requests().lock() {
            requests.insert(hwnd as usize);
        }
        return 0;
    }
    if msg == WM_DPICHANGED {
        // The suggested rectangle is valid only for this callback and must be applied
        // synchronously. Do that before taking any host mutex: SetWindowPos may dispatch more
        // window messages reentrantly.
        if let Some(suggested) = (lparam as *const RECT).as_ref() {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if let Some(dpi) = dpi_from_wparam(wparam) {
            if let Ok(mut changes) = dpi_changes().lock() {
                changes.insert(hwnd as usize, dpi);
            }
        }
        return 0;
    }
    if msg == winapi::um::winuser::WM_SIZE {
        let mut client: RECT = std::mem::zeroed();
        if winapi::um::winuser::GetClientRect(hwnd, &mut client) != 0 {
            if let (Some(width), Some(height)) = (
                client
                    .right
                    .checked_sub(client.left)
                    .filter(|value| *value > 0),
                client
                    .bottom
                    .checked_sub(client.top)
                    .filter(|value| *value > 0),
            ) {
                if let Ok(mut requests) = windows_editor_client_resize_requests().lock() {
                    requests.insert(hwnd as usize, (width, height));
                }
            }
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// An X11 window (connection + window id) backing a plugin editor on Linux.
///
/// Ported from the khremeviuc1004 fork's XCB implementation.
#[cfg(target_os = "linux")]
struct XcbWindowState {
    connection: xcb::Connection,
    window: xcb::x::Window,
}

/// A plugin window that manages the native window and plugin editor lifecycle
pub struct PluginWindow {
    plugin: Arc<Mutex<Plugin>>,
    #[cfg(target_os = "macos")]
    native_window: Option<Retained<NSWindow>>,
    /// The view the plugin attached its editor into. Kept so a plugin-initiated resize can
    /// grow the container along with the window.
    #[cfg(target_os = "macos")]
    container_view: Option<Retained<NSView>>,
    #[cfg(target_os = "windows")]
    native_window: Option<HWND>,
    #[cfg(target_os = "linux")]
    native_window: Option<XcbWindowState>,
    #[cfg(target_os = "android")]
    native_window: Option<()>,
}

impl PluginWindow {
    /// Create a new plugin window for the given plugin
    pub fn new(plugin: Arc<Mutex<Plugin>>) -> Self {
        Self {
            plugin,
            #[cfg(any(
                target_os = "macos",
                target_os = "windows",
                target_os = "linux",
                target_os = "android"
            ))]
            native_window: None,
            #[cfg(target_os = "macos")]
            container_view: None,
        }
    }

    /// Open the plugin window
    pub fn open(&mut self) -> Result<()> {
        // Check if plugin has editor
        let has_editor = self
            .plugin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .has_editor();
        if !has_editor {
            return Err(Error::Other(
                "Plugin does not have a GUI editor".to_string(),
            ));
        }

        // Close existing window if any. Keyed on the handle rather than `is_open()`, which also
        // reports `false` for a window the user already dismissed — that one still needs closing.
        if self.native_window.is_some() {
            self.close();
        }

        // Get plugin info for window title
        let plugin_info = self
            .plugin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .info()
            .clone();

        // Try to get editor size
        let (width, height) = self
            .plugin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_editor_size()
            .unwrap_or((800, 600));

        // Create native window
        #[cfg(target_os = "macos")]
        {
            // AppKit objects must be created on the main thread.
            let mtm = MainThreadMarker::new().ok_or_else(|| {
                Error::Other("plugin editor window must be opened on the main thread".to_string())
            })?;

            let frame = NSRect::new(
                NSPoint::new(100.0, 100.0),
                NSSize::new(width as f64, height as f64),
            );
            let style = NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable;

            // SAFETY: standard AppKit window/view construction on the main thread.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    frame,
                    style,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };

            // Programmatic NSWindows default to `releasedWhenClosed = YES`; closing one would
            // then release it while our `Retained<NSWindow>` also releases on drop — a
            // double-free that crashes on close. We own the lifetime, so opt out.
            // SAFETY: standard AppKit setter on the main thread.
            unsafe { window.setReleasedWhenClosed(false) };

            let title = NSString::from_str(&format!("{} - VST3", plugin_info.name));
            window.setTitle(&title);

            // A container view, sized to the editor, that the plugin attaches its view into.
            let container_frame = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(width as f64, height as f64),
            );
            let container_view = NSView::initWithFrame(NSView::alloc(mtm), container_frame);
            if let Some(content_view) = window.contentView() {
                content_view.addSubview(&container_view);
            }

            // Hand the plugin the container NSView to embed its editor in.
            // SAFETY: `container_view` is a live NSView this `PluginWindow` keeps alive (via
            // the retained window it was added to) for as long as the editor is attached —
            // `close()` detaches the editor before dropping the window.
            let window_handle = unsafe {
                crate::plugin::WindowHandle::from_nsview(
                    Retained::as_ptr(&container_view) as *mut std::ffi::c_void
                )
            };
            self.plugin
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .open_editor(window_handle)?;

            // Match the window to the editor size, then show and center it.
            window.setContentSize(container_frame.size);
            window.makeKeyAndOrderFront(None);
            window.center();

            self.native_window = Some(window);
            self.container_view = Some(container_view);
        }

        #[cfg(target_os = "windows")]
        {
            let hwnd = create_windows_editor_container(
                &format!("{} - VST3", plugin_info.name),
                width,
                height,
            )?;
            let mut plugin = self.plugin.lock().unwrap_or_else(|p| p.into_inner());
            let opened = (|| {
                let dpi = unsafe { winapi::um::winuser::GetDpiForWindow(hwnd) };
                if let Some(scale_factor) = dpi_scale_factor(dpi) {
                    plugin.set_editor_scale_factor(scale_factor)?;
                }
                // SAFETY: this container remains live until the view is removed.
                plugin.open_editor(unsafe { crate::plugin::WindowHandle::from_hwnd(hwnd.cast()) })
            })();
            drop(plugin);
            if let Err(error) = opened {
                destroy_windows_editor_container(hwnd)?;
                return Err(error);
            }
            unsafe {
                ShowWindow(hwnd, SW_SHOW);
                UpdateWindow(hwnd);
            }
            self.native_window = Some(hwnd);
        }

        #[cfg(target_os = "linux")]
        {
            use xcb::Xid;

            // Create an X11 window via XCB and embed the plugin editor into it using the
            // VST3 X11EmbedWindowID platform type (handled in plugin_impl::open_editor).
            let (connection, screen_number) = xcb::Connection::connect(None)
                .map_err(|e| Error::Other(format!("Failed to connect to X server: {e}")))?;
            let setup = connection.get_setup();
            let screen = setup
                .roots()
                .nth(screen_number as usize)
                .ok_or_else(|| Error::Other("No X11 screen found".to_string()))?;
            let window = connection.generate_id();

            connection
                .send_and_check_request(&xcb::x::CreateWindow {
                    depth: xcb::x::COPY_FROM_PARENT as u8,
                    wid: window,
                    parent: screen.root(),
                    x: 0,
                    y: 0,
                    width: width as u16,
                    height: height as u16,
                    border_width: 0,
                    class: xcb::x::WindowClass::InputOutput,
                    visual: screen.root_visual(),
                    value_list: &[
                        xcb::x::Cw::BackPixel(screen.white_pixel()),
                        xcb::x::Cw::EventMask(
                            xcb::x::EventMask::EXPOSURE | xcb::x::EventMask::KEY_PRESS,
                        ),
                    ],
                })
                .map_err(|e| Error::Other(format!("Failed to create X11 window: {e}")))?;

            // Window title.
            let title = format!("{} - VST3", plugin_info.name);
            connection.send_request(&xcb::x::ChangeProperty {
                mode: xcb::x::PropMode::Replace,
                window,
                property: xcb::x::ATOM_WM_NAME,
                r#type: xcb::x::ATOM_STRING,
                data: title.as_bytes(),
            });

            // Show the window, then attach the plugin editor to its X11 id.
            connection.send_request(&xcb::x::MapWindow { window });
            let _ = connection.flush();

            let handle = crate::plugin::WindowHandle::from_x11(window.resource_id());
            self.plugin
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .open_editor(handle)?;

            self.native_window = Some(XcbWindowState { connection, window });
        }

        #[cfg(target_os = "android")]
        {
            return Err(Error::Other(
                "PluginWindow::open() is not supported on Android".to_string(),
            ));
        }

        Ok(())
    }

    /// Pump the editor's window-level traffic that has to cross into the plugin.
    ///
    /// **Call this from your UI event loop, once per frame, while the window is open.** Two
    /// things depend on it:
    ///
    /// - **Plugin-initiated resizes.** A resizable editor (VSTGUI zoom, a "big view" toggle)
    ///   asks the host to resize through `IPlugFrame::resizeView`. This applies the request to
    ///   the native window, so the editor is not clipped by a window that never grew.
    /// - **Windows DPI changes.** `WM_DPICHANGED` applies its suggested native rectangle
    ///   immediately in the window procedure and queues the new content scale for here; making
    ///   the COM call outside the window procedure prevents reentrant deadlocks on the plugin
    ///   mutex.
    ///
    /// Deliberately non-blocking: if the plugin mutex is held elsewhere (the audio callback
    /// holds it per block) this returns without doing anything, and the pending work is picked
    /// up on the next call.
    ///
    /// [`Self::is_open`] and [`Self::closed_by_user`] do *not* run this — they stay lock-free
    /// so they are safe to call while holding the plugin lock.
    pub fn service_platform_events(&self) -> Result<()> {
        if self.native_window.is_none() {
            return Ok(());
        }
        let Some(mut plugin) = self.try_lock_plugin() else {
            return Ok(());
        };

        let scale_result = self
            .take_pending_scale_factor()
            .map(|factor| plugin.set_editor_scale_factor(factor));
        let resize = plugin.take_editor_resize_request();

        // Released before touching the native window: on Windows, resizing it may synchronously
        // dispatch window messages back into host code that wants this same lock.
        drop(plugin);

        if let Some((width, height)) = resize {
            self.resize_native_window(width, height);
        }
        match scale_result {
            Some(Err(error)) => Err(error),
            _ => Ok(()),
        }
    }

    /// Take the plugin lock without blocking, recovering a lock poisoned by an unrelated panic
    /// (a poisoned mutex is permanent, and treating it as failure would stop servicing the
    /// editor for the rest of the session).
    fn try_lock_plugin(&self) -> Option<std::sync::MutexGuard<'_, Plugin>> {
        match self.plugin.try_lock() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(poison)) => Some(poison.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// The newest DPI the window procedure recorded for this window, as a VST3 content scale.
    #[cfg(target_os = "windows")]
    fn take_pending_scale_factor(&self) -> Option<f32> {
        let hwnd = self.native_window?;
        dpi_changes()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&(hwnd as usize))
            .and_then(dpi_scale_factor)
    }

    /// Only Windows reports DPI changes through the window procedure.
    #[cfg(not(target_os = "windows"))]
    fn take_pending_scale_factor(&self) -> Option<f32> {
        None
    }

    /// Resize the native window so it fits an editor of `width` x `height` pixels.
    ///
    /// The plugin has already resized its own view (the host answered `resizeView` with
    /// `onSize`); this catches the window up.
    fn resize_native_window(&self, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        log::debug!("editor asked the host to resize its window to {width}x{height}");

        #[cfg(target_os = "macos")]
        {
            let Some(window) = self.native_window.as_ref() else {
                return;
            };
            let size = NSSize::new(width as f64, height as f64);
            if let Some(container) = self.container_view.as_ref() {
                container.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
            }
            window.setContentSize(size);
        }

        #[cfg(target_os = "windows")]
        {
            let Some(hwnd) = self.native_window else {
                return;
            };
            if let Err(error) = resize_windows_editor_container(hwnd, width, height) {
                log::warn!("native editor parent resize refused: {error}");
            }
        }

        #[cfg(target_os = "linux")]
        {
            let Some(state) = self.native_window.as_ref() else {
                return;
            };
            state.connection.send_request(&xcb::x::ConfigureWindow {
                window: state.window,
                value_list: &[
                    xcb::x::ConfigWindow::Width(width as u32),
                    xcb::x::ConfigWindow::Height(height as u32),
                ],
            });
            let _ = state.connection.flush();
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        let _ = (width, height);
    }

    /// Close the plugin window
    pub fn close(&mut self) {
        // Detach the plugin editor first — it must never outlive the window it is embedded in.
        // A poisoned lock (an earlier panic on the audio thread) is recovered rather than
        // skipped, exactly as every other lock site here does: skipping it would destroy the
        // native window with the plugin's view still attached to it.
        let _ = self
            .plugin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .close_editor();

        // Then close the native window
        #[cfg(target_os = "macos")]
        {
            self.container_view = None;
            if let Some(window) = self.native_window.take() {
                window.close();
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(hwnd) = self.native_window.take() {
                if let Err(error) = destroy_windows_editor_container(hwnd) {
                    log::warn!("native editor parent close refused: {error}");
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Some(state) = self.native_window.take() {
                state.connection.send_request(&xcb::x::UnmapWindow {
                    window: state.window,
                });
                state.connection.send_request(&xcb::x::DestroyWindow {
                    window: state.window,
                });
                let _ = state.connection.flush();
            }
        }

        #[cfg(target_os = "android")]
        {
            let _ = self.native_window.take();
        }
    }

    /// Check if the window is currently open.
    ///
    /// Reports `false` once the user has dismissed the window themselves, even though the
    /// handle is still around — see [`Self::closed_by_user`].
    ///
    /// Reads native window state only: it never takes the plugin lock, so it is safe to call
    /// while holding it. Pair it with [`Self::service_platform_events`], which does the work
    /// that has to reach the plugin.
    pub fn is_open(&self) -> bool {
        self.native_window.is_some() && !self.native_window_dismissed()
    }

    /// Whether the user closed the window themselves, through its title-bar close button.
    ///
    /// Neither the plugin nor the host is told when that happens, so a host that tracks "the
    /// editor is open" in its own UI should poll this each frame and drop or
    /// [`close`](Self::close) the window when it reports `true`. Otherwise the editor stays
    /// attached to a window that is gone from the screen.
    ///
    /// Cheap and lock-free in the same sense as [`Self::is_open`]: native window state only,
    /// never the plugin lock.
    pub fn closed_by_user(&self) -> bool {
        self.native_window_dismissed()
    }

    /// Platform probe behind [`Self::closed_by_user`].
    #[cfg(target_os = "macos")]
    fn native_window_dismissed(&self) -> bool {
        let Some(window) = self.native_window.as_ref() else {
            return false;
        };
        // A closed window is ordered out — but so is a miniaturized one, and so is every window
        // of a hidden application. Neither of those means the user dismissed the editor.
        if window.isVisible() || window.isMiniaturized() {
            return false;
        }
        match MainThreadMarker::new() {
            Some(mtm) => !NSApplication::sharedApplication(mtm).isHidden(),
            // AppKit state can only be read from the main thread; assume the window still stands.
            None => false,
        }
    }

    /// Platform probe behind [`Self::closed_by_user`].
    #[cfg(target_os = "windows")]
    fn native_window_dismissed(&self) -> bool {
        let Some(hwnd) = self.native_window else {
            return false;
        };
        close_requests()
            .lock()
            .is_ok_and(|requests| requests.contains(&(hwnd as usize)))
    }

    /// Platform probe behind [`Self::closed_by_user`]. X11 and Android editor windows carry no
    /// close affordance of their own, so there is nothing to observe.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn native_window_dismissed(&self) -> bool {
        false
    }
}

impl Drop for PluginWindow {
    fn drop(&mut self) {
        self.close();
    }
}

/// Builder for creating plugin windows with egui integration
#[cfg(feature = "egui-widgets")]
pub struct PluginWindowBuilder {
    plugin: Arc<Mutex<Plugin>>,
}

#[cfg(feature = "egui-widgets")]
impl PluginWindowBuilder {
    /// Create a new builder for the given plugin
    pub fn new(plugin: Arc<Mutex<Plugin>>) -> Self {
        Self { plugin }
    }

    /// Build and open a standalone plugin window
    pub fn open_standalone(&self) -> Result<PluginWindow> {
        let mut window = PluginWindow::new(self.plugin.clone());
        window.open()?;
        Ok(window)
    }
}

#[cfg(test)]
mod tests {
    use super::dpi_scale_factor;

    #[test]
    fn windows_dpi_converts_to_vst_content_scale() {
        assert_eq!(dpi_scale_factor(0), None);
        assert_eq!(dpi_scale_factor(96), Some(1.0));
        assert_eq!(dpi_scale_factor(120), Some(1.25));
        assert_eq!(dpi_scale_factor(144), Some(1.5));
        assert_eq!(dpi_scale_factor(192), Some(2.0));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn wm_dpi_wparam_uses_horizontal_dpi() {
        let packed = (192usize << 16) | 144;
        assert_eq!(super::dpi_from_wparam(packed), Some(144));
        assert_eq!(super::dpi_from_wparam(0), None);
    }
}

/// Create the sole Windows native parent used by either inline or scoped editors.
#[cfg(target_os = "windows")]
fn create_windows_editor_container(title: &str, width: i32, height: i32) -> Result<HWND> {
    if title.contains('\0') {
        return Err(Error::Other("native editor title contains NUL".to_string()));
    }
    let (outer_width, outer_height) = windows_editor_outer_extent(width, height)?;
    let class_name = "VST3PluginWindow\0".encode_utf16().collect::<Vec<_>>();
    let title = title
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: all buffers stay live through each synchronous Win32 call. The
    // class has one host window procedure and owns no plugin/interface pointer.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        if instance.is_null() {
            return Err(Error::Other(
                "native editor module handle is unavailable".to_string(),
            ));
        }
        let mut class: WNDCLASSEXW = std::mem::zeroed();
        class.cbSize = std::mem::size_of::<WNDCLASSEXW>() as UINT;
        class.style = CS_HREDRAW | CS_VREDRAW;
        class.lpfnWndProc = Some(plugin_window_proc);
        class.hInstance = instance;
        class.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        class.lpszClassName = class_name.as_ptr();
        if RegisterClassExW(&class) == 0 {
            let mut existing: WNDCLASSEXW = std::mem::zeroed();
            existing.cbSize = std::mem::size_of::<WNDCLASSEXW>() as UINT;
            if winapi::um::winuser::GetClassInfoExW(instance, class_name.as_ptr(), &mut existing)
                == 0
                || existing.lpfnWndProc.map(|procedure| procedure as usize)
                    != Some(plugin_window_proc as *const () as usize)
            {
                return Err(Error::Other(
                    "native editor window class was not admitted".to_string(),
                ));
            }
        }
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            outer_width,
            outer_height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return Err(Error::Other(
                "failed to create native editor parent".to_string(),
            ));
        }
        Ok(hwnd)
    }
}

#[cfg(target_os = "windows")]
fn windows_editor_outer_extent(width: i32, height: i32) -> Result<(i32, i32)> {
    if width <= 0 || height <= 0 {
        return Err(Error::Other(
            "native editor dimensions must be positive".to_string(),
        ));
    }
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    if unsafe { winapi::um::winuser::AdjustWindowRectEx(&mut rect, WS_OVERLAPPEDWINDOW, 0, 0) } == 0
    {
        return Err(Error::Other(
            "native editor frame dimensions were refused".to_string(),
        ));
    }
    let outer_width = rect
        .right
        .checked_sub(rect.left)
        .filter(|value| *value > 0)
        .ok_or_else(|| Error::Other("native editor frame width overflowed".to_string()))?;
    let outer_height = rect
        .bottom
        .checked_sub(rect.top)
        .filter(|value| *value > 0)
        .ok_or_else(|| Error::Other("native editor frame height overflowed".to_string()))?;
    Ok((outer_width, outer_height))
}

#[cfg(target_os = "windows")]
fn resize_windows_editor_container(hwnd: HWND, width: i32, height: i32) -> Result<()> {
    let (width, height) = windows_editor_outer_extent(width, height)?;
    // SAFETY: both callers retain this exact live HWND on its owner main thread.
    if unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    } == 0
    {
        return Err(Error::Other(
            "native editor parent resize failed".to_string(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn destroy_windows_editor_container(hwnd: HWND) -> Result<()> {
    // The caller has already removed IPlugView. Destruction may synchronously
    // dispatch native messages, so no host request mutex is held during it.
    if unsafe { DestroyWindow(hwnd) } == 0 {
        return Err(Error::Other(
            "native editor parent destruction failed".to_string(),
        ));
    }
    close_requests()
        .lock()
        .map_err(|_| Error::Other("native editor close slot is poisoned".to_string()))?
        .remove(&(hwnd as usize));
    dpi_changes()
        .lock()
        .map_err(|_| Error::Other("native editor DPI slot is poisoned".to_string()))?
        .remove(&(hwnd as usize));
    windows_editor_client_resize_requests()
        .lock()
        .map_err(|_| Error::Other("native editor client resize slot is poisoned".to_string()))?
        .remove(&(hwnd as usize));
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_editor_client_resize_requests(
) -> &'static Mutex<std::collections::HashMap<usize, (i32, i32)>> {
    static REQUESTS: std::sync::OnceLock<Mutex<std::collections::HashMap<usize, (i32, i32)>>> =
        std::sync::OnceLock::new();
    REQUESTS.get_or_init(Mutex::default)
}

#[cfg(target_os = "windows")]
#[path = "window/windows_scoped.rs"]
mod windows_scoped;

#[cfg(target_os = "macos")]
#[path = "window/macos_main.rs"]
mod macos_main;

#[cfg(target_os = "macos")]
pub use macos_main::MacosPluginMainThread;

#[cfg(target_os = "windows")]
pub use windows_scoped::{
    WindowsEditorMainThread, WindowsMainThreadWake, WindowsMessageStep,
    WindowsNativeEditorContainer, WindowsNativeEditorContainerEvents,
};

#[cfg(target_os = "linux")]
mod linux_scoped;

#[cfg(target_os = "linux")]
pub use linux_scoped::{LinuxEditorMainThread, LinuxNativeEditorContainer};

#[cfg(target_os = "macos")]
mod macos_scoped;

#[cfg(target_os = "macos")]
pub use macos_scoped::MacosNativeEditorContainer;

/// The concrete platform's original native control-thread lifetime.
#[cfg(target_os = "linux")]
pub use LinuxEditorMainThread as NativeEditorMainThread;
/// The concrete platform's original native control-thread lifetime.
#[cfg(target_os = "macos")]
pub use MacosPluginMainThread as NativeEditorMainThread;
/// The concrete platform's original native control-thread lifetime.
#[cfg(target_os = "windows")]
pub use WindowsEditorMainThread as NativeEditorMainThread;

/// The concrete platform's parent, owning no foreign plugin or author authority.
#[cfg(target_os = "linux")]
pub use LinuxNativeEditorContainer as NativeEditorContainer;
/// The concrete platform's parent, owning no foreign plugin or author authority.
#[cfg(target_os = "macos")]
pub use MacosNativeEditorContainer as NativeEditorContainer;
/// The concrete platform's parent, owning no foreign plugin or author authority.
#[cfg(target_os = "windows")]
pub use WindowsNativeEditorContainer as NativeEditorContainer;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
mod scoped_editor;
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
pub use scoped_editor::PluginEditorParent;
/// Backward-compatible name for the shared scoped view attachment on Windows.
#[cfg(target_os = "windows")]
pub use PluginEditorParent as WindowsPluginEditorParent;
