//! Run on the actual process main thread; libtest invokes tests on other threads.
//! No NSApplication, native view, audio device or third-party plugin is required.

#[cfg(target_os = "macos")]
fn main() {
    use core_foundation::date::CFDate;
    use core_foundation::runloop::{
        kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopTimer, CFRunLoopTimerContext, CFRunLoopTimerRef,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use vst3_host::window::MacosPluginMainThread;

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static ON_MAIN: AtomicBool = AtomicBool::new(false);
    extern "C" fn on_timer(_timer: CFRunLoopTimerRef, _info: *mut std::ffi::c_void) {
        // No allocation, panic or borrowed callback context crosses the C ABI.
        ON_MAIN.store(objc2::MainThreadMarker::new().is_some(), Ordering::SeqCst);
        CALLS.fetch_add(1, Ordering::SeqCst);
    }

    let main = MacosPluginMainThread::initialize().expect("actual process main");
    let run_loop = CFRunLoop::get_main();
    // SAFETY: this mode has process lifetime; the timer's callback uses no context.
    let mode = unsafe { kCFRunLoopDefaultMode };
    let mut context = CFRunLoopTimerContext {
        version: 0,
        info: std::ptr::null_mut(),
        retain: None,
        release: None,
        copyDescription: None,
    };
    let before = CALLS.load(Ordering::SeqCst);
    let timer = CFRunLoopTimer::new(CFDate::now().abs_time(), 0.0, 0, 0, on_timer, &mut context);
    run_loop.add_timer(&timer, mode);
    main.dispatch_messages()
        .expect("nonblocking native main turn");
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        before + 1,
        "due one-shot timer"
    );
    assert!(
        ON_MAIN.load(Ordering::SeqCst),
        "callback must stay on Cocoa main"
    );
    run_loop.remove_timer(&timer, mode);
    drop(timer);
    main.dispatch_messages()
        .expect("native turn after timer retirement");
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        before + 1,
        "no callback after retirement"
    );
    println!("macOS actual-main timer service and retirement passed");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This native main-thread probe requires macOS.");
    std::process::exit(2);
}
