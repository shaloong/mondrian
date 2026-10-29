//! Independent qualification of the real Clack HostGui callback wrappers.
//! Ordinary false replies are legal, including from a plugin background thread.
//! This measures System allocation requests, not a native audio deadline or GUI.

use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::log::{HostLog, HostLogImpl, HostLogMessage};
use clack_host::prelude::{
    HostError, HostExtensions, HostHandlers, HostInfo, PluginEntry, PluginInstance, SharedHandler,
};
use clack_plugin::prelude::{
    DefaultPluginFactory, HostMainThreadHandle, HostSharedHandle, Plugin, PluginDescriptor,
    PluginError, PluginExtensions, PluginMainThread, PluginShared, SinglePluginEntry,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const ITERATIONS: usize = 1024;
struct ProbeAllocator;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LOGS: AtomicUsize = AtomicUsize::new(0);
static REFUSALS: AtomicUsize = AtomicUsize::new(0);
static CALLBACK_CALLS: AtomicUsize = AtomicUsize::new(0);
static CALLBACK_BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: System receives every original valid allocation contract unchanged.
// Counters are independent scalar observations; no allocation header is added.
unsafe impl GlobalAlloc for ProbeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: unchanged System allocation request.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: unchanged System allocation request.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        // SAFETY: the caller's System-owned pointer/layout/new size are preserved.
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: return the original System-owned allocation unchanged.
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: ProbeAllocator = ProbeAllocator;

struct RefusingHost;
struct RefusingShared;
impl SharedHandler<'_> for RefusingShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostGuiImpl for RefusingShared {
    fn resize_hints_changed(&self) {}
    fn request_resize(&self, _: GuiSize) -> Result<(), HostError> {
        Err(HostError::Message("host declined a GUI resize"))
    }
    fn request_show(&self) -> Result<(), HostError> {
        Err(HostError::Message("host declined a GUI show"))
    }
    fn request_hide(&self) -> Result<(), HostError> {
        Err(HostError::Message("host declined a GUI hide"))
    }
    fn closed(&self, _: bool) {}
}
impl HostLogImpl for RefusingShared {
    fn log(&self, _: HostLogMessage<'_>) {
        LOGS.fetch_add(1, Ordering::Relaxed);
    }
}
impl HostHandlers for RefusingHost {
    type Shared<'a> = RefusingShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(builder: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        builder.register::<HostGui>().register::<HostLog>();
    }
}

struct GuiRefusalPlugin;
struct GuiRefusalShared<'a> {
    host: HostSharedHandle<'a>,
    gui: HostGui,
}
struct GuiRefusalMain<'a>(&'a GuiRefusalShared<'a>);
impl<'a> PluginShared<'a> for GuiRefusalShared<'a> {}
impl Plugin for GuiRefusalPlugin {
    type Shared<'a> = GuiRefusalShared<'a>;
    type MainThread<'a> = GuiRefusalMain<'a>;
    type AudioProcessor<'a> = ();
    fn declare_extensions(_: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {}
}
impl DefaultPluginFactory for GuiRefusalPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("org.mondrian.test.gui-refusal", "GUI Refusal ABI Control")
    }
    fn new_shared(host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        let gui = host
            .get_extension::<HostGui>()
            .ok_or(PluginError::Message("HostGui is absent"))?;
        Ok(GuiRefusalShared { host, gui })
    }
    fn new_main_thread<'a>(
        _: HostMainThreadHandle<'a>,
        shared: &'a Self::Shared<'a>,
    ) -> Result<Self::MainThread<'a>, PluginError> {
        Ok(GuiRefusalMain(shared))
    }
}
impl<'a> PluginMainThread<'a, GuiRefusalShared<'a>> for GuiRefusalMain<'a> {
    fn on_main_thread(&self) {
        // Thread creation, teardown and the test's own assertion/printing are
        // outside the allocation interval. The lifetime is the actual instance
        // borrow, joined before native plugin/module destruction.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let call = || {
                        usize::from(self.0.gui.request_resize(&self.0.host, 640, 480).is_err())
                            + usize::from(self.0.gui.request_show(&self.0.host).is_err())
                            + usize::from(self.0.gui.request_hide(&self.0.host).is_err())
                    };
                    assert_eq!(call(), 3, "warm actual C callback refusal");
                    LOGS.store(0, Ordering::Relaxed);
                    CALLS.store(0, Ordering::Relaxed);
                    BYTES.store(0, Ordering::Relaxed);
                    ACTIVE.store(true, Ordering::Relaxed);
                    let mut refused = 0;
                    for _ in 0..ITERATIONS {
                        refused += call();
                    }
                    ACTIVE.store(false, Ordering::Relaxed);
                    REFUSALS.store(refused, Ordering::Relaxed);
                    CALLBACK_CALLS.store(CALLS.load(Ordering::Relaxed), Ordering::Relaxed);
                    CALLBACK_BYTES.store(BYTES.load(Ordering::Relaxed), Ordering::Relaxed);
                })
                .join()
                .expect("actual plugin background callback returned");
        });
    }
}

fn main() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<GuiRefusalPlugin>>(
        c"/mondrian/owned/gui-refusal",
    )
    .expect("owned CLAP entry");
    let info = HostInfo::new(
        "Mondrian GUI callback control",
        "Shalom Wang",
        "https://example.invalid",
        "1",
    )
    .expect("valid host identity");
    let mut instance = PluginInstance::<RefusingHost>::new(
        |_| RefusingShared,
        |_| (),
        &entry,
        c"org.mondrian.test.gui-refusal",
        &info,
    )
    .expect("owned native CLAP instance");
    instance.call_on_main_thread_callback();
    let refusals = REFUSALS.load(Ordering::Relaxed);
    let logs = LOGS.load(Ordering::Relaxed);
    let calls = CALLBACK_CALLS.load(Ordering::Relaxed);
    let bytes = CALLBACK_BYTES.load(Ordering::Relaxed);
    println!("{{\"schema\":1,\"iterations\":{ITERATIONS},\"refusals\":{refusals},\"host_logs\":{logs},\"allocator_calls\":{calls},\"requested_bytes\":{bytes}}}");
    assert_eq!(
        refusals,
        ITERATIONS * 3,
        "ordinary false replies remain unchanged"
    );
    assert_eq!(
        logs, 0,
        "ordinary GUI false replies must not be reported as host errors"
    );
    assert_eq!(
        (calls, bytes),
        (0, 0),
        "valid thread-safe GUI refusal must not allocate"
    );
}
