//! Route public AppKit termination intent to the owning product event loop.
//!
//! AppKit's default `terminate:` exits without returning through Rust cleanup.
//! The application subclass forwards intent only; save/cancel and resource
//! shutdown stay with the existing UI owner. Winit still owns its delegate.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSApplication;

thread_local! {
    static QUIT_HANDLER: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
    static QUIT_PENDING: Cell<bool> = const { Cell::new(false) };
}

define_class!(
    // SAFETY: NSApplication permits subclassing. Only public `terminate:` is
    // overridden, with its original signature and main-thread requirements.
    #[unsafe(super = NSApplication)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct MondrianApplication;

    impl MondrianApplication {
        #[unsafe(method(terminate:))]
        fn request_termination(&self, _sender: Option<&AnyObject>) {
            forward_quit_request();
        }
    }
);

fn forward_quit_request() {
    // Clone before calling: routing may reenter AppKit or release its
    // registration, neither of which may hold a RefCell borrow.
    let handler = QUIT_HANDLER.with(|slot| slot.borrow().clone());
    if let Some(handler) = handler {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler())).is_err() {
            // Never unwind through the Objective-C method boundary or pretend
            // shutdown succeeded after a routing failure.
            QUIT_PENDING.with(|pending| pending.set(true));
            eprintln!("native macOS quit routing panicked; quit intent retained");
        }
    } else {
        QUIT_PENDING.with(|pending| pending.set(true));
    }
}

/// Failure to establish the native application routing contract.
#[derive(Debug, thiserror::Error)]
pub enum MacApplicationError {
    /// AppKit application lifetime must be established on the main thread.
    #[error("native macOS application requires the main thread")]
    MainThreadRequired,
    /// A different application was created before product initialization.
    #[error("another NSApplication was initialized before Mondrian's native application")]
    ApplicationAlreadyInitialized,
    /// Two product event-loop runs attempted to own native quit routing.
    #[error("native macOS quit routing already has a live owner")]
    QuitHandlerAlreadyOwned,
}

/// Establish the application before winit creates its shared AppKit instance.
///
/// Repeated initialization accepts only the same product application class.
/// No existing application's delegate or implementation is replaced.
pub fn prepare_macos_application() -> Result<(), MacApplicationError> {
    let _main_thread = MainThreadMarker::new().ok_or(MacApplicationError::MainThreadRequired)?;
    // SAFETY: sharedApplication is NSApplication's public factory. Sending it
    // to the subclass creates the process singleton with this principal class.
    let application: Retained<NSApplication> =
        unsafe { msg_send![MondrianApplication::class(), sharedApplication] };
    if application.isKindOfClass(MondrianApplication::class()) {
        Ok(())
    } else {
        Err(MacApplicationError::ApplicationAlreadyInitialized)
    }
}

/// One main-thread event-loop run's native quit callback registration.
///
/// Dropping it releases the callback and its captured owner-local queue. An
/// old wake event therefore cannot carry a quit action into a later run.
pub struct MacApplicationQuitHandler {
    _main_thread: MainThreadMarker,
}

impl Drop for MacApplicationQuitHandler {
    fn drop(&mut self) {
        QUIT_HANDLER.with(|slot| slot.borrow_mut().take());
        QUIT_PENDING.with(|pending| pending.set(false));
    }
}

/// Register intent forwarding after the product event-loop queue exists.
/// Requests received during early startup are forwarded after registration.
pub fn install_macos_quit_handler(
    handler: impl Fn() + 'static,
) -> Result<MacApplicationQuitHandler, MacApplicationError> {
    let main_thread = MainThreadMarker::new().ok_or(MacApplicationError::MainThreadRequired)?;
    QUIT_HANDLER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err(MacApplicationError::QuitHandlerAlreadyOwned);
        }
        *slot = Some(Rc::new(handler));
        Ok(())
    })?;
    let registration = MacApplicationQuitHandler { _main_thread: main_thread };
    if QUIT_PENDING.with(|pending| pending.replace(false)) {
        forward_quit_request();
    }
    Ok(registration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_callback_can_release_registration_without_a_reentrant_borrow() {
        QUIT_HANDLER.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(|| {
                QUIT_HANDLER.with(|slot| slot.borrow_mut().take());
            }));
        });
        forward_quit_request();
        assert!(QUIT_HANDLER.with(|slot| slot.borrow().is_none()));
        assert!(!QUIT_PENDING.with(Cell::get));
    }

    #[test]
    fn quit_callback_panic_retains_intent_instead_of_unwinding_into_appkit() {
        QUIT_HANDLER.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(|| panic!("injected routing failure")));
        });
        forward_quit_request();
        assert!(QUIT_PENDING.with(|pending| pending.replace(false)));
        QUIT_HANDLER.with(|slot| slot.borrow_mut().take());
    }
}
