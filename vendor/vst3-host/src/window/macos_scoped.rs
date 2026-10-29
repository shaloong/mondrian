//! Cocoa parent lifetime, separate from the foreign view attached into it.

use super::MacosPluginMainThread;
use crate::error::{Error, Result};
use crate::plugin::WindowHandle;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use std::cell::Cell;
use std::rc::Rc;

struct CloseIntent {
    requested: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject allows subclassing. The sole callback has its native
    // signature, runs on Cocoa main, touches only a Cell and cannot unwind.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = CloseIntent]
    struct MondrianPluginParentDelegate;

    unsafe impl NSObjectProtocol for MondrianPluginParentDelegate {}

    unsafe impl NSWindowDelegate for MondrianPluginParentDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn request_close(&self, _sender: &NSWindow) -> bool {
            self.ivars().requested.set(true);
            // Keep the native parent live until the format adapter acknowledges
            // actual view removal. No foreign callback runs inside AppKit here.
            false
        }
    }
);

impl MondrianPluginParentDelegate {
    fn new(main: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(main).set_ivars(CloseIntent {
            requested: Cell::new(false),
        });
        // SAFETY: ivars are initialized before NSObject's designated init.
        unsafe { msg_send![super(allocated), init] }
    }
}

struct CocoaParent {
    window: Retained<NSWindow>,
    view: Retained<NSView>,
    // NSWindow's delegate is weak; this owner must retain it through close.
    delegate: Retained<MondrianPluginParentDelegate>,
}

/// A Cocoa main-only parent that owns no plugin or parameter/state authority.
///
/// The format adapter must acknowledge actual GUI destruction before `destroy`.
/// An unresolved live parent is retained until its isolated process exits.
pub struct MacosNativeEditorContainer {
    main: Rc<MacosPluginMainThread>,
    parent: Option<CocoaParent>,
}

impl MacosNativeEditorContainer {
    /// Create a hidden NSWindow and its sole embedding NSView on Cocoa main.
    pub fn create(
        main: Rc<MacosPluginMainThread>,
        title: &str,
        width: i32,
        height: i32,
    ) -> Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(Error::Other(
                "native Cocoa editor dimensions must be positive".to_owned(),
            ));
        }
        let marker = main.marker()?;
        let parent = objc2::rc::autoreleasepool(|_| -> Result<CocoaParent> {
            let _application = main.gui_application()?;
            let size = NSSize::new(f64::from(width), f64::from(height));
            // SAFETY: all AppKit allocation, use and retirement stays on actual main.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(marker),
                    NSRect::new(NSPoint::new(0.0, 0.0), size),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Miniaturizable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // SAFETY: this retained owner, rather than AppKit close, owns release.
            unsafe { window.setReleasedWhenClosed(false) };
            window.setTitle(&NSString::from_str(title));
            let view = NSView::initWithFrame(
                NSView::alloc(marker),
                NSRect::new(NSPoint::new(0.0, 0.0), size),
            );
            window.setContentView(Some(&view));
            let delegate = MondrianPluginParentDelegate::new(marker);
            window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            window.center();
            Ok(CocoaParent {
                window,
                view,
                delegate,
            })
        })?;
        Ok(Self {
            main,
            parent: Some(parent),
        })
    }

    fn parent(&self) -> Result<&CocoaParent> {
        let _main = self.main.marker()?;
        self.parent
            .as_ref()
            .ok_or_else(|| Error::Other("native Cocoa editor parent is closed".to_owned()))
    }

    /// Whether this owner still retains its native parent.
    pub fn is_live(&self) -> Result<bool> {
        let _main = self.main.marker()?;
        Ok(self.parent.is_some())
    }

    /// Borrow the live NSView; it must survive the attached foreign GUI.
    pub fn nsview(&self) -> Result<*mut std::ffi::c_void> {
        Ok(Retained::as_ptr(&self.parent()?.view) as *mut std::ffi::c_void)
    }

    /// Borrow the existing VST3 Cocoa embedding contract on this same owner.
    pub fn window_handle(&self) -> Result<WindowHandle> {
        // SAFETY: this owner retains the NSView through confirmed view removal.
        Ok(unsafe { WindowHandle::from_nsview(self.nsview()?) })
    }

    /// Cocoa embedding uses logical points; AppKit owns backing-pixel scaling.
    pub fn scale(&self) -> Result<Option<f64>> {
        self.parent()?;
        Ok(None)
    }

    /// Show or hide this parent after the format's own show/hide operation.
    pub fn set_visible(&self, visible: bool) -> Result<()> {
        let parent = self.parent()?;
        objc2::rc::autoreleasepool(|_| {
            if visible {
                parent.window.makeKeyAndOrderFront(None);
            } else {
                parent.window.orderOut(None);
            }
            Ok(())
        })
    }

    /// Resize the exact logical-point embedding extent without pixel rescaling.
    pub fn resize(&self, width: i32, height: i32) -> Result<()> {
        if width <= 0 || height <= 0 {
            return Err(Error::Other(
                "native Cocoa editor dimensions must be positive".to_owned(),
            ));
        }
        let parent = self.parent()?;
        let size = NSSize::new(f64::from(width), f64::from(height));
        objc2::rc::autoreleasepool(|_| {
            parent.window.setContentSize(size);
            parent.view.setFrameSize(size);
            Ok(())
        })
    }

    /// Service bounded AppKit/main-runloop work and observe the actual content.
    pub fn service(&self) -> Result<super::NativeEditorContainerEvents> {
        objc2::rc::autoreleasepool(|_| {
            self.main.dispatch_messages()?;
            let size = self.parent()?.view.frame().size;
            let dimensions = (
                enclosing_extent(size.width)?,
                enclosing_extent(size.height)?,
            );
            // Plugin embedding APIs express integral coordinates. Expand a native
            // fractional logical extent before publication, rather than describing
            // different geometry to the plugin or clipping its view by truncation.
            if size.width != f64::from(dimensions.0) || size.height != f64::from(dimensions.1) {
                self.resize(dimensions.0, dimensions.1)?;
            }
            Ok(super::NativeEditorContainerEvents {
                scale: None,
                client_size: Some(dimensions),
            })
        })
    }

    /// Observe the deferred close intent without destroying an attached view.
    pub fn closed_by_user(&self) -> Result<bool> {
        let _main = self.main.marker()?;
        Ok(self
            .parent
            .as_ref()
            .is_some_and(|parent| parent.delegate.ivars().requested.get()))
    }

    /// Close and release only after the format confirms native GUI/view removal.
    pub fn destroy(&mut self) -> Result<()> {
        let _main = self.main.marker()?;
        objc2::rc::autoreleasepool(|_| {
            if let Some(parent) = &self.parent {
                parent.window.setDelegate(None);
                parent.window.close();
                self.parent = None;
            }
            Ok(())
        })
    }
}

impl Drop for MacosNativeEditorContainer {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.take() {
            log::error!("native editor parent dropped before confirmed GUI destruction; Cocoa parent retained until isolated process exit");
            std::mem::forget(parent);
        }
    }
}

fn enclosing_extent(value: f64) -> Result<i32> {
    if !value.is_finite() || value <= 0.0 || value.ceil() > f64::from(i32::MAX) {
        return Err(Error::Other(
            "native Cocoa editor returned an invalid content extent".to_owned(),
        ));
    }
    Ok(value.ceil() as i32)
}

#[cfg(test)]
mod tests {
    use super::enclosing_extent;

    #[test]
    fn native_content_extent_is_enclosed_without_clipping_or_overflow() {
        assert_eq!(enclosing_extent(317.0).unwrap(), 317);
        assert_eq!(enclosing_extent(317.25).unwrap(), 318);
        assert_eq!(enclosing_extent(0.25).unwrap(), 1);
        assert_eq!(enclosing_extent(f64::from(i32::MAX)).unwrap(), i32::MAX);
        for value in [
            f64::NAN,
            f64::INFINITY,
            -1.0,
            0.0,
            f64::from(i32::MAX) + 0.25,
        ] {
            assert!(enclosing_extent(value).is_err());
        }
    }
}
