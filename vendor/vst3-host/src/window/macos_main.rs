//! Actual macOS main-thread services, including when no editor is attached.

use crate::error::{Error, Result};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEventMask};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
use std::cell::OnceCell;

/// Proves the retained plugin's control owner is the actual Cocoa main thread.
///
/// Construct before loading plugin code and retain through native module unload.
/// Initialization creates no NSApplication/window and never moves a native
/// controller into DSP. An actual editor lazily establishes AppKit; window
/// attachment remains a separate owner.
pub struct MacosPluginMainThread {
    _main_only: MainThreadMarker,
    application: OnceCell<Retained<NSApplication>>,
}

impl Drop for MacosPluginMainThread {
    fn drop(&mut self) {
        // Last native owner: retire retained AppKit state inside an actual-main
        // pool after the plugin, module and acknowledged editor parents.
        objc2::rc::autoreleasepool(|_| drop(self.application.take()));
    }
}

impl MacosPluginMainThread {
    /// Retain proof of the current process main thread before loading plugin code.
    ///
    /// Returns an error when called from any background thread.
    pub fn initialize() -> Result<Self> {
        let main = MainThreadMarker::new().ok_or_else(|| {
            Error::Other("native plugin control requires the macOS main thread".to_owned())
        })?;
        Ok(Self {
            _main_only: main,
            application: OnceCell::new(),
        })
    }

    pub(super) fn marker(&self) -> Result<MainThreadMarker> {
        MainThreadMarker::new()
            .ok_or_else(|| Error::Other("native plugin main service changed thread".to_owned()))
    }

    /// Run native controller/view work on actual main inside an autorelease pool.
    /// Retained native owners may outlive this temporary callback scope.
    pub fn with_native_scope<T>(&self, action: impl FnOnce() -> T) -> Result<T> {
        let _main = self.marker()?;
        Ok(objc2::rc::autoreleasepool(|_| action()))
    }

    // Opening an actual editor establishes AppKit. Headless timer/processing
    // services do not create an NSApplication or change an existing delegate.
    pub(super) fn gui_application(&self) -> Result<&NSApplication> {
        let main = self.marker()?;
        Ok(self
            .application
            .get_or_init(|| NSApplication::sharedApplication(main)))
    }

    /// Service one nonblocking default-mode pass on the original main run loop.
    ///
    /// The existing worker owns scheduling and idle waits. A stopped nested CF
    /// invocation is not a request to terminate the worker or replace its plugin.
    pub fn dispatch_messages(&self) -> Result<()> {
        let _main = self.marker()?;
        // Plugin-created timers/sources may autorelease Objective-C objects even
        // with no view. Retire those temporaries after each main service turn.
        objc2::rc::autoreleasepool(|_| {
            if let Some(application) = self.application.get() {
                let expired = NSDate::distantPast();
                for _ in 0..64 {
                    // SAFETY: Foundation provides this process-lifetime mode.
                    let mode = unsafe { NSDefaultRunLoopMode };
                    let Some(event) = application.nextEventMatchingMask_untilDate_inMode_dequeue(
                        NSEventMask::Any,
                        Some(&expired),
                        mode,
                        true,
                    ) else {
                        break;
                    };
                    application.sendEvent(&event);
                }
                application.updateWindows();
            }
            // SAFETY: Core Foundation supplies a process-lifetime default mode.
            let mode = unsafe { kCFRunLoopDefaultMode };
            let _ = CFRunLoop::run_in_mode(mode, std::time::Duration::ZERO, true);
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_main_context_refuses_a_background_thread() {
        let refused = std::thread::spawn(|| MacosPluginMainThread::initialize().is_err())
            .join()
            .expect("background context probe");
        assert!(
            refused,
            "a Rust control-thread label cannot grant Cocoa main affinity"
        );
    }
}
