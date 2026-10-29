//! Windows child main-thread support for the one scoped native editor owner.
//!
//! This module is included by `window.rs` only on Windows. It shares that
//! module's native container implementation and never locks or clones a Plugin.

use super::{
    close_requests, create_windows_editor_container, destroy_windows_editor_container, dpi_changes,
    dpi_scale_factor, resize_windows_editor_container, windows_editor_client_resize_requests,
};
use crate::error::{Error, Result};
use crate::plugin::WindowHandle;
use std::marker::PhantomData;
use std::rc::Rc;
use std::thread::{self, ThreadId};
use winapi::shared::windef::HWND;
use winapi::um::winuser::{
    DispatchMessageW, GetDpiForWindow, MsgWaitForMultipleObjectsEx, PeekMessageW,
    PostThreadMessageW, ShowWindow, TranslateMessage, UpdateWindow, MSG, MWMO_INPUTAVAILABLE,
    PM_NOREMOVE, PM_REMOVE, QS_ALLINPUT, SW_SHOW, WM_APP, WM_QUIT,
};

const MAX_MESSAGES_PER_STEP: usize = 64;
const MAIN_WAKE_MESSAGE: u32 = WM_APP + 0x31;

#[link(name = "ole32")]
unsafe extern "system" {
    fn OleInitialize(reserved: *mut std::ffi::c_void) -> i32;
    fn OleUninitialize();
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThreadId() -> u32;
}

/// The actual Windows child main thread's STA/OLE and message queue lifetime.
///
/// Construct before loading native code, and keep it until every editor,
/// controller, component and native module has been released on this thread.
/// VST3 FUnknown uses a COM-like ABI; this guard does not marshal its interfaces
/// or grant permission to invoke UI callbacks from a processing thread.
pub struct WindowsEditorMainThread {
    rust_thread: ThreadId,
    native_thread: u32,
    _main_only: PhantomData<Rc<()>>,
}

impl WindowsEditorMainThread {
    /// Initialize the actual child main thread as STA with OLE GUI services.
    /// Existing STA S_FALSE is balanced; a conflicting apartment is refused.
    pub fn initialize() -> Result<Self> {
        // SAFETY: null is the required reserved argument. This !Send guard
        // balances every successful call on this same thread, including S_FALSE.
        let result = unsafe { OleInitialize(std::ptr::null_mut()) };
        if result < 0 {
            return Err(Error::Other(format!(
                "native editor STA/OLE initialization failed: {result:#x}"
            )));
        }
        // Ensure PostThreadMessage has an actual queue before publishing a wake
        // handle. Peek does not remove a pre-existing message.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
        Ok(Self {
            rust_thread: thread::current().id(),
            native_thread: unsafe { GetCurrentThreadId() },
            _main_only: PhantomData,
        })
    }

    fn ensure_current(&self) -> Result<()> {
        if thread::current().id() != self.rust_thread {
            return Err(Error::Other(
                "native editor main service changed thread".to_string(),
            ));
        }
        Ok(())
    }

    /// Run native control work on the initializing STA/OLE thread.
    pub fn with_native_scope<T>(&self, action: impl FnOnce() -> T) -> Result<T> {
        self.ensure_current()?;
        Ok(action())
    }

    /// A wake signal only; it carries no command, plugin pointer or owner lease.
    pub fn wake_handle(&self) -> WindowsMainThreadWake {
        WindowsMainThreadWake {
            native_thread: self.native_thread,
        }
    }

    /// Dispatch a bounded batch from this thread's actual Windows message queue.
    /// This covers plugin-created child windows as well as the host container.
    pub fn dispatch_messages(&self) -> Result<WindowsMessageStep> {
        self.ensure_current()?;
        let mut dispatched = 0;
        let mut message: MSG = unsafe { std::mem::zeroed() };
        while dispatched < MAX_MESSAGES_PER_STEP {
            // SAFETY: MSG is writable and the null filter intentionally services
            // all windows of this exact UI thread. Plugin UI runs after native
            // dispatch; the host window procedure never invokes the plugin.
            if unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 {
                return Ok(WindowsMessageStep::Idle { dispatched });
            }
            if message.message == WM_QUIT {
                return Ok(WindowsMessageStep::Quit {
                    exit_code: message.wParam as i32,
                });
            }
            if message.message != MAIN_WAKE_MESSAGE {
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            dispatched += 1;
        }
        Ok(WindowsMessageStep::BudgetUsed)
    }

    /// Wait at most the admitted idle interval, waking for native UI or ingress.
    /// No native interface or shared PCM storage is touched while waiting.
    pub fn wait_for_input(&self, timeout_ms: u32) -> Result<()> {
        self.ensure_current()?;
        // SAFETY: zero handles permits a null handle array. Incoming wake
        // signals are thread messages, not a second payload/command queue.
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(
                0,
                std::ptr::null(),
                timeout_ms,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        if result == u32::MAX {
            return Err(Error::Other(
                "native editor message wait failed".to_string(),
            ));
        }
        Ok(())
    }
}

impl Drop for WindowsEditorMainThread {
    fn drop(&mut self) {
        // The type is !Send and construction recorded this actual thread.
        // The consumer must release plugin/native window owners before us.
        unsafe { OleUninitialize() };
    }
}

/// Pointer-free wake signal for the one existing child transport or DSP slot.
#[derive(Debug, Clone, Copy)]
pub struct WindowsMainThreadWake {
    native_thread: u32,
}

impl WindowsMainThreadWake {
    /// Wake the original main thread after publishing to its admitted slot.
    /// A failed wake is explicit; it cannot authorize any native/author action.
    pub fn notify(&self) -> Result<()> {
        if unsafe { PostThreadMessageW(self.native_thread, MAIN_WAKE_MESSAGE, 0, 0) } == 0 {
            return Err(Error::Other(
                "native editor main thread is unavailable".to_string(),
            ));
        }
        Ok(())
    }
}

/// Result of servicing a bounded actual native message batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsMessageStep {
    /// The queue is currently empty after this many messages.
    Idle {
        /// Number of messages serviced in this batch.
        dispatched: usize,
    },
    /// The bounded batch was used; return to command/cancellation admission.
    BudgetUsed,
    /// Actual WM_QUIT; the consumer must close and finish its owner scope.
    Quit {
        /// Native thread exit code.
        exit_code: i32,
    },
}

/// A format-neutral native parent using the existing Windows container/WndProc.
///
/// This owns no plugin/controller and issues no native binding or author lease.
/// The attached format adapter must finish native GUI destruction before calling
/// `destroy`; dropping this object cannot infer that foreign lifecycle step.
pub struct WindowsNativeEditorContainer {
    pub(super) main: Rc<WindowsEditorMainThread>,
    hwnd: Option<HWND>,
}

pub use super::NativeEditorContainerEvents as WindowsNativeEditorContainerEvents;

impl WindowsNativeEditorContainer {
    /// Create the format-neutral parent on its initializing STA/main thread.
    pub fn create(
        main: Rc<WindowsEditorMainThread>,
        title: &str,
        width: i32,
        height: i32,
    ) -> Result<Self> {
        main.ensure_current()?;
        let hwnd = create_windows_editor_container(title, width, height)?;
        Ok(Self {
            main,
            hwnd: Some(hwnd),
        })
    }

    /// Borrow its live HWND. The caller must not destroy this parent while a
    /// native child is still attached. The handle grants no plugin authority.
    pub fn hwnd(&self) -> Result<*mut std::ffi::c_void> {
        self.main.ensure_current()?;
        self.hwnd
            .map(|hwnd| hwnd.cast())
            .ok_or_else(|| Error::Other("native editor parent is closed".to_string()))
    }

    /// Whether this owner still retains its native parent.
    pub fn is_live(&self) -> Result<bool> {
        self.main.ensure_current()?;
        Ok(self.hwnd.is_some())
    }

    /// Borrow the existing VST3 HWND contract through confirmed view removal.
    pub fn window_handle(&self) -> Result<WindowHandle> {
        // SAFETY: this owner retains its HWND until the format acknowledges detach.
        Ok(unsafe { WindowHandle::from_hwnd(self.hwnd()?) })
    }

    /// Current physical scaling of this exact native parent, when available.
    pub fn scale(&self) -> Result<Option<f64>> {
        self.main.ensure_current()?;
        Ok(self
            .hwnd
            .and_then(|hwnd| dpi_scale_factor(unsafe { GetDpiForWindow(hwnd) }))
            .map(f64::from))
    }

    /// Show or hide this host parent without guessing the plugin's GUI state.
    pub fn set_visible(&self, visible: bool) -> Result<()> {
        self.main.ensure_current()?;
        let hwnd = self
            .hwnd
            .ok_or_else(|| Error::Other("native editor parent is closed".to_string()))?;
        // ShowWindow returns previous visibility, not a success/error result.
        unsafe {
            ShowWindow(
                hwnd,
                if visible {
                    SW_SHOW
                } else {
                    winapi::um::winuser::SW_HIDE
                },
            );
            if visible {
                UpdateWindow(hwnd);
            }
        }
        Ok(())
    }

    /// Resize through the same existing host-container implementation.
    pub fn resize(&self, width: i32, height: i32) -> Result<()> {
        self.main.ensure_current()?;
        let hwnd = self
            .hwnd
            .ok_or_else(|| Error::Other("native editor parent is closed".to_string()))?;
        resize_windows_editor_container(hwnd, width, height)
    }

    /// Pump bounded native events and take only this parent's DPI/size records.
    /// These host observations never call or capture a foreign native object.
    pub fn service(&self) -> Result<WindowsNativeEditorContainerEvents> {
        self.main.ensure_current()?;
        let step = self.main.dispatch_messages()?;
        let (scale, client_size) = if let Some(hwnd) = self.hwnd {
            let dpi = dpi_changes()
                .lock()
                .map_err(|_| Error::Other("native editor DPI slot is poisoned".to_string()))?
                .remove(&(hwnd as usize));
            let size = windows_editor_client_resize_requests()
                .lock()
                .map_err(|_| {
                    Error::Other("native editor client resize slot is poisoned".to_string())
                })?
                .remove(&(hwnd as usize));
            (dpi.and_then(dpi_scale_factor).map(f64::from), size)
        } else {
            (None, None)
        };
        Ok(WindowsNativeEditorContainerEvents {
            step,
            scale,
            client_size,
        })
    }

    /// Observe the actual WM_CLOSE request without destroying an attached GUI.
    pub fn closed_by_user(&self) -> Result<bool> {
        self.main.ensure_current()?;
        let Some(hwnd) = self.hwnd else {
            return Ok(false);
        };
        close_requests()
            .lock()
            .map(|requests| requests.contains(&(hwnd as usize)))
            .map_err(|_| Error::Other("native editor close slot is poisoned".to_string()))
    }

    /// Destroy after the format adapter has finished actual native GUI destroy.
    /// Failure preserves the live HWND for explicit isolated retirement.
    pub fn destroy(&mut self) -> Result<()> {
        self.main.ensure_current()?;
        if let Some(hwnd) = self.hwnd {
            destroy_windows_editor_container(hwnd)?;
            self.hwnd = None;
        }
        Ok(())
    }
}

impl Drop for WindowsNativeEditorContainer {
    fn drop(&mut self) {
        if self.hwnd.is_some() {
            log::error!("native editor parent dropped before confirmed GUI destruction; HWND retained until isolated process exit");
        }
    }
}
