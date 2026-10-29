//! Actual macOS main-thread services, including when no editor is attached.

use crate::error::{Error, Result};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use objc2::MainThreadMarker;

/// Proves the retained plugin's control owner is the actual Cocoa main thread.
///
/// Construct before loading plugin code and retain through native module unload.
/// This !Send token neither creates an NSApplication/window nor moves a native
/// controller into DSP. Window attachment remains a separate owner.
pub struct MacosPluginMainThread {
    _main_only: MainThreadMarker,
}

impl MacosPluginMainThread {
    /// Retain proof of the current process main thread before loading plugin code.
    ///
    /// Returns an error when called from any background thread.
    pub fn initialize() -> Result<Self> {
        let main = MainThreadMarker::new().ok_or_else(|| {
            Error::Other("native plugin control requires the macOS main thread".to_owned())
        })?;
        Ok(Self { _main_only: main })
    }

    /// Service one nonblocking default-mode pass on the original main run loop.
    ///
    /// The existing worker owns scheduling and idle waits. A stopped nested CF
    /// invocation is not a request to terminate the worker or replace its plugin.
    pub fn dispatch_messages(&self) -> Result<()> {
        if MainThreadMarker::new().is_none() {
            return Err(Error::Other(
                "native plugin main service changed thread".to_owned(),
            ));
        }
        // Plugin-created timers/sources may autorelease Objective-C objects even
        // with no view. Retire those temporaries after each main service turn.
        objc2::rc::autoreleasepool(|_| {
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
