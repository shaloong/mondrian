//! Run actual Cocoa parent operations on the process main thread.
//! This is host-window qualification, not third-party plugin GUI qualification.

#[cfg(target_os = "macos")]
fn main() {
    use objc2_app_kit::NSView;
    use std::rc::Rc;
    use vst3_host::window::{MacosNativeEditorContainer, MacosPluginMainThread};

    let main = Rc::new(MacosPluginMainThread::initialize().expect("actual Cocoa main"));
    for _ in 0..3 {
        let mut container =
            MacosNativeEditorContainer::create(Rc::clone(&main), "Native parent", 240, 120)
                .expect("native parent create");
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            objc2::rc::autoreleasepool(|_| {
                let original = container.nsview().unwrap();
                container.set_visible(true).unwrap();
                container.resize(317, 193).unwrap();
                let events = container.service().unwrap();
                assert_eq!(events.client_size, Some((317, 193)));
                assert_eq!(container.nsview().unwrap(), original);
                // SAFETY: the container retains this exact NSView through the end
                // of this scope, on actual Cocoa main. No foreign plugin is loaded.
                let view = unsafe { &*original.cast::<NSView>() };
                let window = view.window().expect("native view remains parented");
                window.performClose(None);
                container.service().unwrap();
                assert!(container.closed_by_user().unwrap());
                assert!(container.is_live().unwrap());
                assert_eq!(container.nsview().unwrap(), original);
                container.set_visible(false).unwrap();
            });
        }));
        // Retire the original native owner even when an assertion failed.
        let retired = container.destroy();
        if let Err(panic) = checked {
            std::panic::resume_unwind(panic);
        }
        retired.expect("explicit native parent retirement");
        assert!(!container.is_live().unwrap());
        assert!(!container.closed_by_user().unwrap());
        assert!(container.nsview().is_err());
        container.destroy().unwrap();
        main.dispatch_messages().unwrap();
    }
    println!("macOS actual-main parent create/resize/deferred-close/retirement passed");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This native editor parent probe requires macOS.");
    std::process::exit(2);
}
