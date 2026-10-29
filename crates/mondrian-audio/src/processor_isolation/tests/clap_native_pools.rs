//! Native pool ordering, not third-party plugin or Cocoa GUI qualification.
//! An outer test pool makes a missing inner pool observable without executing
//! unloaded code. The EntryProvider records the instant its module owner retires.

use clack_host::entry::{EntryDescriptor, EntryProvider};
use clack_host::prelude::{HostInfo, PluginAudioConfiguration, PluginEntry, PluginInstance};
use clap_sys::factory::plugin_factory::clap_plugin_factory;
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::{clap_process, clap_process_status, CLAP_PROCESS_CONTINUE};
use clap_sys::version::CLAP_VERSION;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::NSObject;
use objc2::{define_class, msg_send, AnyThread};
use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// Entry init, factory create, instance init, activate, deactivate, destroy, deinit.
const STAGES: usize = 7;

struct Counts {
    drops: [AtomicUsize; STAGES],
    at_provider_retirement: Mutex<Option<[usize; STAGES]>>,
}

impl Counts {
    fn snapshot(&self) -> [usize; STAGES] {
        std::array::from_fn(|i| self.drops[i].load(Ordering::Relaxed))
    }

    fn retired(&self) -> Option<[usize; STAGES]> {
        *self.at_provider_retirement.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Success,
    EntryInitFailure,
    InstanceInitFailure,
}

thread_local! {
    static CONTEXT: RefCell<Option<(Arc<Counts>, Mode)>> = const { RefCell::new(None) };
}

struct Probe {
    counts: Arc<Counts>,
    stage: usize,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.counts.drops[self.stage].fetch_add(1, Ordering::Relaxed);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MondrianClapNativePoolProbe"]
    #[ivars = Probe]
    struct NativePoolProbe;
);

fn emit(stage: usize) {
    CONTEXT.with(|context| {
        if let Some((counts, _)) = context.borrow().as_ref() {
            let object =
                NativePoolProbe::alloc().set_ivars(Probe { counts: Arc::clone(counts), stage });
            // SAFETY: initialize our NSObject subclass after installing its ivars.
            let object: Retained<NativePoolProbe> = unsafe { msg_send![super(object), init] };
            let _ = Retained::autorelease_ptr(object);
        }
    });
}

fn fails(mode: Mode) -> bool {
    CONTEXT.with(|context| context.borrow().as_ref().is_some_and(|(_, actual)| *actual == mode))
}

fn with_case<T>(mode: Mode, run: impl FnOnce(Arc<Counts>) -> T) -> (Arc<Counts>, T) {
    let counts = Arc::new(Counts {
        drops: std::array::from_fn(|_| AtomicUsize::new(0)),
        at_provider_retirement: Mutex::new(None),
    });
    CONTEXT.with(|context| *context.borrow_mut() = Some((Arc::clone(&counts), mode)));
    let result = autoreleasepool(|_| run(Arc::clone(&counts)));
    CONTEXT.with(|context| context.borrow_mut().take());
    (counts, result)
}

struct Provider {
    descriptor: Box<EntryDescriptor>,
    counts: Arc<Counts>,
}

impl Provider {
    fn new(counts: Arc<Counts>) -> Self {
        Self {
            descriptor: Box::new(EntryDescriptor {
                clap_version: CLAP_VERSION,
                init: Some(entry_init),
                deinit: Some(entry_deinit),
                get_factory: Some(get_factory),
            }),
            counts,
        }
    }
}

// SAFETY: Box pins the descriptor's address; every callback is linked into this
// test binary and remains valid throughout and beyond the provider's lifetime.
unsafe impl EntryProvider for Provider {
    fn entry_pointer(&self) -> NonNull<EntryDescriptor> {
        NonNull::from(self.descriptor.as_ref())
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        *self
            .counts
            .at_provider_retirement
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(self.counts.snapshot());
    }
}

static DESCRIPTOR: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: c"org.mondrian.test.native-pools".as_ptr(),
    name: c"Native pool ordering".as_ptr(),
    vendor: c"Mondrian".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"1".as_ptr(),
    description: c"Explicit lifecycle probe".as_ptr(),
    features: [c"audio-effect".as_ptr(), ptr::null()].as_ptr(),
};

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(plugin_count),
    get_plugin_descriptor: Some(plugin_descriptor),
    create_plugin: Some(create_plugin),
};

unsafe extern "C" fn entry_init(_: *const c_char) -> bool {
    emit(0);
    !fails(Mode::EntryInitFailure)
}
unsafe extern "C" fn entry_deinit() {
    emit(6);
}
unsafe extern "C" fn get_factory(id: *const c_char) -> *const c_void {
    // SAFETY: Clack passes a valid factory ID string during this entry's lifetime.
    if unsafe { CStr::from_ptr(id) } == c"clap.plugin-factory" {
        (&FACTORY as *const clap_plugin_factory).cast()
    } else {
        ptr::null()
    }
}
unsafe extern "C" fn plugin_count(_: *const clap_plugin_factory) -> u32 {
    1
}
unsafe extern "C" fn plugin_descriptor(
    _: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    if index == 0 {
        &DESCRIPTOR
    } else {
        ptr::null()
    }
}
unsafe extern "C" fn create_plugin(
    _: *const clap_plugin_factory,
    _: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    // SAFETY: Clack passes a valid plugin ID string to the live factory.
    if unsafe { CStr::from_ptr(id) } != c"org.mondrian.test.native-pools" {
        return ptr::null();
    }
    emit(1);
    Box::into_raw(Box::new(clap_plugin {
        desc: &DESCRIPTOR,
        plugin_data: ptr::null_mut(),
        init: Some(instance_init),
        destroy: Some(destroy),
        activate: Some(activate),
        deactivate: Some(deactivate),
        start_processing: Some(start_processing),
        stop_processing: Some(noop),
        reset: Some(noop),
        process: Some(process),
        get_extension: Some(get_extension),
        on_main_thread: Some(noop),
    }))
}
unsafe extern "C" fn instance_init(_: *const clap_plugin) -> bool {
    emit(2);
    !fails(Mode::InstanceInitFailure)
}
unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    emit(5);
    // SAFETY: only Clack's sole instance owner calls destroy; create allocated it.
    drop(unsafe { Box::from_raw(plugin.cast_mut()) });
}
unsafe extern "C" fn activate(_: *const clap_plugin, _: f64, _: u32, _: u32) -> bool {
    emit(3);
    true
}
unsafe extern "C" fn deactivate(_: *const clap_plugin) {
    emit(4);
}
unsafe extern "C" fn start_processing(_: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn noop(_: *const clap_plugin) {}
unsafe extern "C" fn process(_: *const clap_plugin, _: *const clap_process) -> clap_process_status {
    CLAP_PROCESS_CONTINUE
}
unsafe extern "C" fn get_extension(_: *const clap_plugin, _: *const c_char) -> *const c_void {
    ptr::null()
}

fn new_instance(
    entry: &PluginEntry,
) -> Result<PluginInstance<()>, clack_host::plugin::PluginInstanceError> {
    PluginInstance::new(
        |_| (),
        |_| (),
        entry,
        c"org.mondrian.test.native-pools",
        &HostInfo::new("pool-test", "Mondrian", "", "1").unwrap(),
    )
}

#[test]
fn clap_native_pools_drain_before_entry_or_instance_owner_retires() {
    let (counts, observed) = with_case(Mode::Success, |counts| {
        // SAFETY: Provider supplies the complete static test ABI above.
        let entry =
            unsafe { PluginEntry::load_from(Provider::new(Arc::clone(&counts)), c"/pool-test") }
                .unwrap();
        let loaded = counts.snapshot();
        let mut instance = new_instance(&entry).unwrap();
        let created = counts.snapshot();
        drop(entry); // The instance must retain the entry independently.
        let retired_early = counts.retired();
        let processor = instance
            .activate(
                |_, _| (),
                PluginAudioConfiguration {
                    sample_rate: 48_000.0,
                    min_frames_count: 1,
                    max_frames_count: 64,
                },
            )
            .unwrap();
        let active = counts.snapshot();
        instance.deactivate(processor);
        let inactive = counts.snapshot();
        drop(instance);
        (
            loaded,
            created,
            retired_early,
            active,
            inactive,
            counts.snapshot(),
            counts.retired(),
        )
    });
    assert_eq!(observed.0, [1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(observed.1, [1, 1, 1, 0, 0, 0, 0]);
    assert_eq!(observed.2, None);
    assert_eq!(observed.3, [1, 1, 1, 1, 0, 0, 0]);
    assert_eq!(observed.4, [1, 1, 1, 1, 1, 0, 0]);
    assert_eq!(observed.5, [1; STAGES]);
    assert_eq!(observed.6, Some([1; STAGES]));
    assert_eq!(counts.snapshot(), [1; STAGES]);
}

#[test]
fn failed_clap_entry_init_drains_before_provider_cleanup() {
    let (counts, observed) = with_case(Mode::EntryInitFailure, |counts| {
        // SAFETY: same test ABI; init deliberately refuses after emitting a probe.
        let result =
            unsafe { PluginEntry::load_from(Provider::new(Arc::clone(&counts)), c"/pool-test") };
        (result.is_err(), counts.snapshot(), counts.retired())
    });
    let expected = [1, 0, 0, 0, 0, 0, 0];
    assert!(observed.0);
    assert_eq!(observed.1, expected);
    assert_eq!(observed.2, Some(expected));
    assert_eq!(counts.snapshot(), expected);
}

#[test]
fn failed_clap_instance_init_drains_destroy_before_entry_cleanup() {
    let (counts, observed) = with_case(Mode::InstanceInitFailure, |counts| {
        // SAFETY: same test ABI; instance init deliberately refuses.
        let entry =
            unsafe { PluginEntry::load_from(Provider::new(Arc::clone(&counts)), c"/pool-test") }
                .unwrap();
        let failed = new_instance(&entry).is_err();
        let after_failure = counts.snapshot();
        drop(entry);
        (failed, after_failure, counts.snapshot(), counts.retired())
    });
    assert!(observed.0);
    assert_eq!(observed.1, [1, 1, 1, 0, 0, 1, 0]);
    let expected = [1, 1, 1, 0, 0, 1, 1];
    assert_eq!(observed.2, expected);
    assert_eq!(observed.3, Some(expected));
    assert_eq!(counts.snapshot(), expected);
}
