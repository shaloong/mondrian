//! Session-local global pointer observation for the desktop eyedropper.
//!
//! Wayland intentionally offers no ambient global-pointer protocol. Linux uses
//! one retained X11 connection when available and otherwise returns `None`, so
//! the Window Adapter can continue with ordinary winit events without polling
//! subprocesses or reconnecting every frame.

use super::DesktopPoint;

/// One coherent global pointer observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GlobalPointerSnapshot {
    pub(super) position: DesktopPoint,
    pub(super) primary_button_down: bool,
}

/// Platform-local pointer observer retained for one eyedropper session owner.
#[cfg_attr(not(target_os = "linux"), derive(Default))]
pub(super) struct GlobalPointerSession {
    #[cfg(target_os = "linux")]
    x11: Option<(x11rb::rust_connection::RustConnection, usize)>,
}

impl std::fmt::Debug for GlobalPointerSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("GlobalPointerSession");
        #[cfg(target_os = "linux")]
        debug.field("x11_connected", &self.x11.is_some());
        debug.finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl Default for GlobalPointerSession {
    fn default() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            x11: x11rb::connect(None).ok(),
        }
    }
}

impl GlobalPointerSession {
    pub(super) fn snapshot(&self) -> Option<GlobalPointerSnapshot> {
        platform_snapshot(self)
    }
}

#[cfg(target_os = "windows")]
fn platform_snapshot(_session: &GlobalPointerSession) -> Option<GlobalPointerSnapshot> {
    use windows_sys::Win32::Foundation::POINT as WinPoint;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = WinPoint { x: 0, y: 0 };
    // SAFETY: Windows writes one POINT to the valid stack pointer and retains
    // no reference.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return None;
    }
    // SAFETY: This reads the calling desktop's current asynchronous button bit
    // and carries no ownership or lifetime obligation.
    let state = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) };
    Some(GlobalPointerSnapshot {
        position: DesktopPoint::new(point.x, point.y),
        primary_button_down: (state & 0x8000u16 as i16) != 0,
    })
}

#[cfg(target_os = "macos")]
fn platform_snapshot(_session: &GlobalPointerSession) -> Option<GlobalPointerSnapshot> {
    use std::ffi::c_void;

    #[repr(C)]
    struct CgPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *const c_void;
        fn CGEventGetLocation(event: *const c_void) -> CgPoint;
        fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const c_void);
    }

    // SAFETY: A null source requests a current combined-session event. The +1
    // result is released exactly once after copying its position.
    let event = unsafe { CGEventCreate(std::ptr::null()) };
    if event.is_null() {
        return None;
    }
    // SAFETY: `event` remains live until CFRelease below.
    let point = unsafe { CGEventGetLocation(event) };
    // SAFETY: Balances the successful CGEventCreate ownership transfer.
    unsafe { CFRelease(event) };
    let x = exact_i32_coordinate(point.x)?;
    let y = exact_i32_coordinate(point.y)?;
    // Combined-session and left-button constants are stable CoreGraphics ABI
    // values (`kCGEventSourceStateCombinedSessionState`, `kCGMouseButtonLeft`).
    let primary_button_down = unsafe { CGEventSourceButtonState(0, 0) };
    Some(GlobalPointerSnapshot {
        position: DesktopPoint::new(x, y),
        primary_button_down,
    })
}

#[cfg(target_os = "macos")]
fn exact_i32_coordinate(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.round();
    if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return None;
    }
    Some(rounded as i32)
}

#[cfg(target_os = "linux")]
fn platform_snapshot(session: &GlobalPointerSession) -> Option<GlobalPointerSnapshot> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, KeyButMask};

    let (connection, screen_index) = session.x11.as_ref()?;
    let root = connection.setup().roots.get(*screen_index)?.root;
    let reply = connection.query_pointer(root).ok()?.reply().ok()?;
    Some(GlobalPointerSnapshot {
        position: DesktopPoint::new(i32::from(reply.root_x), i32::from(reply.root_y)),
        primary_button_down: reply.mask & KeyButMask::BUTTON1 != KeyButMask::default(),
    })
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn platform_snapshot(_session: &GlobalPointerSession) -> Option<GlobalPointerSnapshot> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::exact_i32_coordinate;

    #[test]
    fn desktop_coordinates_fail_closed_outside_finite_i32_range() {
        assert_eq!(exact_i32_coordinate(12.4), Some(12));
        assert_eq!(exact_i32_coordinate(f64::NAN), None);
        assert_eq!(exact_i32_coordinate(f64::from(i32::MAX) + 1.0), None);
    }
}
