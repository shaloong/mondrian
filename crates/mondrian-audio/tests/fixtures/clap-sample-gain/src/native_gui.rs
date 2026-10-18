//! Minimal, fixed-size Win32 embedded GUI for the owned sample-gain fixture.
//!
//! All HWND/class/context operations stay on the CLAP main thread. The host
//! lends the parent until destroy returns. Our per-instance class and child
//! never outlive their context or this DLL; WM_NCDESTROY detaches the pointer.
//! A callback clones only the main-thread Rc so reentrant GUI destruction cannot
//! free its context mid-callback. No HWND or host pointer is exported as state.
#![allow(unsafe_code)]

use crate::{params::GainParams, GainPluginMainThread, GainPluginShared};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGuiImpl, Window};
use clack_extensions::params::HostParams;
use clack_plugin::prelude::{HostMainThreadHandle, HostSharedHandle, PluginError};
use std::cell::{Cell, RefCell};
use std::ptr;
use std::rc::Rc;
use winapi::shared::{
    minwindef::{DWORD, HMODULE, LPARAM, LRESULT, UINT, WPARAM},
    windef::{HBRUSH, HWND},
};
use winapi::um::{
    libloaderapi::GetModuleHandleExW,
    processthreadsapi::{GetCurrentProcessId, GetCurrentThreadId},
    winuser::*,
};

/// Fixed physical pixel dimensions; arbitrary resize and scaling are unsupported.
const SIZE: GuiSize = GuiSize { width: 320, height: 100 };
/// Discoverable real child-window title, not a host protocol identity.
const TITLE: &str = "Mondrian CLAP Gain Editor";

/// Main-thread GUI lifetime. Creation never owns or destroys the host parent.
pub(super) struct NativeGui<'a> {
    /// CLAP main thread that initialized this instance.
    thread: DWORD,
    /// Original host, valid for the same plugin lifetime.
    host: HostSharedHandle<'a>,
    /// Original optional host parameter extension.
    host_params: Option<HostParams>,
    /// Original parameter storage; the GUI does not own a second value model.
    shared: &'a GainPluginShared,
    /// Successful CLAP create awaiting its matching destroy.
    created: Cell<bool>,
    /// Sole owned native child, absent until set_parent succeeds.
    window: RefCell<Option<NativeWindow<'a>>>,
}

impl<'a> NativeGui<'a> {
    /// Capture only existing typed handles on the real initializing main thread.
    pub(super) fn new(
        host: HostMainThreadHandle<'a>,
        host_params: Option<HostParams>,
        shared: &'a GainPluginShared,
    ) -> Self {
        Self {
            // SAFETY: GetCurrentThreadId has no borrowed pointer arguments.
            thread: unsafe { GetCurrentThreadId() },
            host: host.shared(),
            host_params,
            shared,
            created: Cell::new(false),
            window: RefCell::new(None),
        }
    }

    /// Reject GUI operations on a thread other than the initializing CLAP main.
    fn check_thread(&self) -> Result<(), PluginError> {
        // SAFETY: this query does not access any window or foreign allocation.
        if unsafe { GetCurrentThreadId() } == self.thread {
            Ok(())
        } else {
            Err(PluginError::Message(
                "GUI requires its original main thread",
            ))
        }
    }

    /// Clone a live context without keeping a RefCell borrow over foreign calls.
    fn context(&self) -> Result<Rc<WindowContext<'a>>, PluginError> {
        self.check_thread()?;
        self.window
            .borrow()
            .as_ref()
            .map(|window| Rc::clone(&window.context))
            .filter(|context| !context.hwnd.get().is_null())
            .ok_or(PluginError::Message("GUI child is not attached"))
    }

    /// Close only the owned child and class, while all original borrows are live.
    fn destroy(&self) {
        if self.check_thread().is_err() {
            // A void teardown cannot report failure while leaving DLL callbacks
            // installed. Fail this isolated fixture process rather than create UAF.
            std::process::abort();
        }
        self.created.set(false);
        let window = self.window.borrow_mut().take();
        drop(window);
    }
}

impl Drop for NativeGui<'_> {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Stable, main-thread-only WndProc backing. Rc covers reentrant destruction.
struct WindowContext<'a> {
    /// Only this initializing main thread may invoke GUI-to-host callbacks.
    thread: DWORD,
    /// Host borrow never extends beyond the owning plugin main-thread lifetime.
    host: HostSharedHandle<'a>,
    /// Real host parameter extension for request_flush, never an audio callback.
    host_params: HostParams,
    /// Sole native parameter storage, shared with the existing DSP.
    params: &'a GainParams,
    /// Actual child handle, cleared by WM_NCDESTROY before backing is released.
    hwnd: Cell<HWND>,
    /// Do not accept keys during construction or destruction callbacks.
    armed: Cell<bool>,
}

/// Per-instance class/child ownership, destroyed synchronously on its main thread.
struct NativeWindow<'a> {
    /// Stable context retained even when the host parent destroys its child first.
    context: Rc<WindowContext<'a>>,
    /// Actual fixture DLL module; no extra module reference is acquired.
    module: HMODULE,
    /// Unique registered class owned by this one instance, including its NUL.
    class: Vec<u16>,
}

/// Convert cold Win32 names to NUL-terminated UTF-16.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

impl<'a> NativeWindow<'a> {
    /// Register and create one real child of an actual same-thread borrowed HWND.
    fn create(gui: &NativeGui<'a>, parent: HWND) -> Result<Self, PluginError> {
        let host_params = gui.host_params.ok_or(PluginError::Message("host params unavailable"))?;
        // SAFETY: Win32 validates the opaque borrowed handle before it is used.
        let mut process = 0;
        if parent.is_null()
            || unsafe { IsWindow(parent) } == 0
            || unsafe { GetWindowThreadProcessId(parent, &mut process) } != gui.thread
            || process != unsafe { GetCurrentProcessId() }
        {
            return Err(PluginError::Message(
                "parent must be a live same-thread HWND",
            ));
        }
        let context = Rc::new(WindowContext {
            thread: gui.thread,
            host: gui.host,
            host_params,
            params: &gui.shared.params,
            hwnd: Cell::new(ptr::null_mut()),
            armed: Cell::new(false),
        });
        let class = wide(&format!(
            "MondrianClapGainEditor_{:x}",
            Rc::as_ptr(&context) as usize
        ));
        let mut module = ptr::null_mut();
        // SAFETY: FROM_ADDRESS | UNCHANGED_REFCOUNT finds this actual DLL from
        // its WndProc. The original plugin instance owns the DLL through teardown.
        if unsafe {
            GetModuleHandleExW(
                0x0000_0004 | 0x0000_0002,
                procedure as *const u16,
                &mut module,
            )
        } == 0
        {
            return Err(PluginError::Message("fixture DLL module unavailable"));
        }
        let native_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(procedure),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: ptr::null_mut(),
            // SAFETY: IDC_ARROW is the system-owned shared cursor resource.
            hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) },
            hbrBackground: (COLOR_WINDOW + 1) as usize as HBRUSH,
            lpszMenuName: ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: ptr::null_mut(),
        };
        // SAFETY: all pointers remain valid during registration; this instance
        // alone owns this unique class and unregisters it after child teardown.
        if unsafe { RegisterClassExW(&native_class) } == 0 {
            return Err(PluginError::Message("GUI class registration failed"));
        }
        let window = Self { context, module, class };
        let title = wide(TITLE);
        // SAFETY: stable Rc backing spans synchronous creation and the full HWND
        // lifetime. The supplied parent is borrowed; only the new child is owned.
        let child = unsafe {
            CreateWindowExW(
                0,
                window.class.as_ptr(),
                title.as_ptr(),
                WS_CHILD | WS_TABSTOP,
                0,
                0,
                SIZE.width as i32,
                SIZE.height as i32,
                parent,
                ptr::null_mut(),
                module,
                Rc::as_ptr(&window.context) as *mut _,
            )
        };
        if child.is_null() {
            return Err(PluginError::Message("GUI child creation failed"));
        }
        // SAFETY: child is the HWND just created, still owned by this local guard.
        if window.context.hwnd.get() != child || unsafe { GetParent(child) } != parent {
            return Err(PluginError::Message("GUI child attachment failed"));
        }
        Ok(window)
    }
}

impl Drop for NativeWindow<'_> {
    fn drop(&mut self) {
        // SAFETY: this class and child were created on context.thread; no parent
        // is touched. Failure cannot leave a callback pointing into an unloaded DLL.
        unsafe {
            if GetCurrentThreadId() != self.context.thread {
                std::process::abort();
            }
            self.context.armed.set(false);
            let child = self.context.hwnd.get();
            if !child.is_null() && (DestroyWindow(child) == 0 || !self.context.hwnd.get().is_null())
            {
                std::process::abort();
            }
            if UnregisterClassW(self.class.as_ptr(), self.module) == 0 {
                std::process::abort();
            }
        }
    }
}

/// Win32 calls this only on the child creator thread. The registered class is
/// removed before its DLL can unload. USERDATA is a stable borrowed Rc pointer.
unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: WM_NCCREATE supplies the CREATESTRUCTW from our own create call.
        let Some(create) = (unsafe { (lparam as *const CREATESTRUCTW).as_ref() }) else {
            return 0;
        };
        let context = create.lpCreateParams as *const WindowContext<'_>;
        // SAFETY: NativeWindow retains this Rc through CreateWindowExW and teardown.
        let Some(context) = (unsafe { context.as_ref() }) else {
            return 0;
        };
        if context.thread != unsafe { GetCurrentThreadId() } {
            return 0;
        }
        // SAFETY: initialize this new child's own private pointer slot only.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
        if unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } != create.lpCreateParams as isize {
            return 0;
        }
        context.hwnd.set(hwnd);
    }
    // SAFETY: only the above WM_NCCREATE writes this slot, and WM_NCDESTROY
    // clears it before backing can disappear. Windows serializes this WndProc.
    let context = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowContext<'_>;
    if context.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    // SAFETY: the owning NativeWindow holds the original Rc. Retaining it here
    // keeps backing live if a host callback reenters GUI destroy synchronously.
    unsafe { Rc::increment_strong_count(context) };
    let context = unsafe { Rc::from_raw(context) };
    match message {
        WM_NCDESTROY => {
            context.armed.set(false);
            context.hwnd.set(ptr::null_mut());
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        }
        WM_LBUTTONDOWN => {
            unsafe { SetFocus(hwnd) };
            return 0;
        }
        WM_KEYDOWN if wparam == usize::from(b'G') => {
            // Bit 30 rejects OS key-repeat; the phase also rejects an unfinished G.
            if lparam & (1_isize << 30) == 0
                && context.armed.get()
                && context.thread == unsafe { GetCurrentThreadId() }
                && context.params.begin_gui_gesture()
            {
                context.host_params.request_flush(&context.host);
            }
            return 0;
        }
        _ => {}
    }
    // SAFETY: normal processing of this live child; context remains retained until
    // this call returns, including destruction messages dispatched synchronously.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

impl PluginGuiImpl for GainPluginMainThread<'_> {
    fn is_api_supported(&self, config: GuiConfiguration) -> bool {
        self.gui.check_thread().is_ok()
            && config.api_type == GuiApiType::WIN32
            && !config.is_floating
    }

    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        self.gui.check_thread().ok()?;
        Some(GuiConfiguration { api_type: GuiApiType::WIN32, is_floating: false })
    }

    fn create(&self, config: GuiConfiguration) -> Result<(), PluginError> {
        self.gui.check_thread()?;
        if !self.is_api_supported(config)
            || self.gui.host_params.is_none()
            || self.gui.created.replace(true)
        {
            return Err(PluginError::Message("unsupported or already-created GUI"));
        }
        Ok(())
    }

    fn destroy(&self) {
        self.gui.destroy();
    }

    fn set_scale(&self, _scale: f64) -> Result<(), PluginError> {
        Err(PluginError::Message("GUI scaling is unsupported"))
    }

    fn get_size(&self) -> Option<GuiSize> {
        self.gui.check_thread().ok()?;
        self.gui.created.get().then_some(SIZE)
    }

    fn set_size(&self, _size: GuiSize) -> Result<(), PluginError> {
        Err(PluginError::Message("GUI resizing is unsupported"))
    }

    fn set_parent(&self, parent: Window) -> Result<(), PluginError> {
        self.gui.check_thread()?;
        if !self.gui.created.get() || self.gui.window.borrow().is_some() {
            return Err(PluginError::Message(
                "GUI must be created and not yet attached",
            ));
        }
        let parent = parent.as_win32_hwnd().ok_or(PluginError::Message("expected Win32 HWND"))?;
        let window = NativeWindow::create(&self.gui, parent.cast())?;
        if !self.gui.created.get() || self.gui.window.borrow().is_some() {
            return Err(PluginError::Message(
                "GUI lifetime changed during attachment",
            ));
        }
        window.context.armed.set(true);
        self.gui.window.replace(Some(window));
        Ok(())
    }

    fn set_transient(&self, _parent: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("floating GUI is unsupported"))
    }

    fn show(&self) -> Result<(), PluginError> {
        let context = self.gui.context()?;
        // SAFETY: this is our same-thread attached child; no RefCell borrow spans it.
        unsafe { ShowWindow(context.hwnd.get(), SW_SHOW) };
        if context.hwnd.get().is_null() {
            return Err(PluginError::Message("GUI destroyed during show"));
        }
        Ok(())
    }

    fn hide(&self) -> Result<(), PluginError> {
        let context = self.gui.context()?;
        // SAFETY: same owned child and thread as show; do not touch the parent.
        unsafe { ShowWindow(context.hwnd.get(), SW_HIDE) };
        if context.hwnd.get().is_null() {
            return Err(PluginError::Message("GUI destroyed during hide"));
        }
        Ok(())
    }
}
