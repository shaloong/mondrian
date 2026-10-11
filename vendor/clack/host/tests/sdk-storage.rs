//! Actual allocator observations of SDK container ownership, never private casts.
//! Run alone with --test-threads=1. These are new-API qualification controls;
//! they are not claimed as old-production behavioral REDs.

use clack_host::entry::{EntryDescriptor, EntryProvider};
use clack_host::prelude::*;
use clack_host::storage::{HostStorageError, HostStorageKind, HostStorageRef};
use clack_plugin::clack_entry;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::CStr;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
struct Allocation {
    address: usize,
    bytes: usize,
    alignment: usize,
}
impl Allocation {
    const ZERO: Self = Self {
        address: 0,
        bytes: 0,
        alignment: 1,
    };
}

#[derive(Clone, Copy)]
struct AuditState {
    active: bool,
    calls: usize,
    overflow: bool,
    used: usize,
    live: [Allocation; 256],
}
impl AuditState {
    const ZERO: Self = Self {
        active: false,
        calls: 0,
        overflow: false,
        used: 0,
        live: [Allocation::ZERO; 256],
    };
}

thread_local! {
    static AUDIT: Cell<AuditState> = const { Cell::new(AuditState::ZERO) };
}

fn remove(address: usize) {
    let _ = AUDIT.try_with(|audit| {
        let mut state = audit.get();
        if !state.active {
            return;
        }
        if let Some(index) = state.live[..state.used]
            .iter()
            .position(|item| item.address == address)
        {
            state.used -= 1;
            state.live[index] = state.live[state.used];
        }
        audit.set(state);
    });
}

fn record(pointer: *mut u8, layout: Layout) {
    let _ = AUDIT.try_with(|audit| {
        let mut state = audit.get();
        if !state.active || pointer.is_null() {
            return;
        }
        state.calls += 1;
        if state.used == state.live.len() {
            state.overflow = true;
        } else {
            state.live[state.used] = Allocation {
                address: pointer as usize,
                bytes: layout.size(),
                alignment: layout.align(),
            };
            state.used += 1;
        }
        audit.set(state);
    });
}

struct AuditAllocator;
// SAFETY: every operation delegates the unchanged allocation contract to System;
// fixed TLS bookkeeping never allocates or dereferences the recorded addresses.
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forward the valid caller layout unchanged.
        let pointer = unsafe { System.alloc(layout) };
        record(pointer, layout);
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forward the valid caller layout unchanged.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        record(pointer, layout);
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        remove(pointer as usize);
        // SAFETY: forward the original pointer/layout allocation pair.
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        // SAFETY: forward the original pointer/layout and requested new extent.
        let next = unsafe { System.realloc(pointer, layout, bytes) };
        if !next.is_null() {
            remove(pointer as usize);
            record(
                next,
                Layout::from_size_align(bytes, layout.align()).expect("valid realloc extent"),
            );
        }
        next
    }
}

#[global_allocator]
static ALLOCATOR: AuditAllocator = AuditAllocator;

fn observe<T>(work: impl FnOnce() -> T) -> (T, AuditState) {
    AUDIT.with(|audit| {
        assert!(!audit.get().active);
        audit.set(AuditState {
            active: true,
            ..AuditState::ZERO
        });
    });
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            AUDIT.with(|audit| {
                let mut state = audit.get();
                state.active = false;
                audit.set(state);
            });
        }
    }
    let stop = Stop;
    let result = work();
    drop(stop);
    let facts = AUDIT.with(Cell::get);
    assert!(
        !facts.overflow,
        "actual allocator capture overflow is not a valid receipt"
    );
    (result, facts)
}

fn assert_matches(view: HostStorageRef<'_>, facts: &AuditState) -> Allocation {
    let payload_end = view
        .address()
        .checked_add(view.layout().size())
        .expect("bounded payload");
    let matching: Vec<_> = facts.live[..facts.used]
        .iter()
        .copied()
        .filter(|allocation| {
            let end = allocation
                .address
                .checked_add(allocation.bytes)
                .expect("bounded allocation");
            match view.kind() {
                HostStorageKind::CompleteAllocation => {
                    allocation.address == view.address()
                        && allocation.bytes == view.layout().size()
                        && allocation.alignment == view.layout().align()
                }
                HostStorageKind::ArcPayload => {
                    allocation.address <= view.address()
                        && end >= payload_end
                        && allocation.alignment >= view.layout().align()
                }
            }
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "each view must match exactly one actual live allocator request: {view:?}"
    );
    let allocation = matching[0];
    if view.kind() == HostStorageKind::ArcPayload {
        // Observe the actual prefix, do not construct an ArcInner or cast to it.
        let offset = view.address() - allocation.address;
        assert!(offset > 0);
        println!(
            "SDK_ARC_OBSERVATION bytes={} alignment={} payload_bytes={} payload_alignment={} payload_offset={offset}",
            allocation.bytes,
            allocation.alignment,
            view.layout().size(),
            view.layout().align()
        );
    }
    allocation
}

struct Host;
impl HostHandlers for Host {
    type Shared<'a> = ();
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

struct Plugin;
impl clack_plugin::prelude::Plugin for Plugin {
    type Shared<'a> = ();
    type MainThread<'a> = ();
    type AudioProcessor<'a> = Plugin;
}
impl clack_plugin::prelude::DefaultPluginFactory for Plugin {
    fn get_descriptor() -> clack_plugin::prelude::PluginDescriptor {
        clack_plugin::prelude::PluginDescriptor::new("org.mondrian.sdk-storage", "Storage oracle")
    }
    fn new_shared(
        _host: clack_plugin::prelude::HostSharedHandle<'_>,
    ) -> Result<(), clack_plugin::prelude::PluginError> {
        Ok(())
    }
    fn new_main_thread<'a>(
        _host: clack_plugin::prelude::HostMainThreadHandle<'a>,
        _shared: &'a (),
    ) -> Result<(), clack_plugin::prelude::PluginError> {
        Ok(())
    }
}
impl<'a> clack_plugin::prelude::PluginAudioProcessor<'a, (), ()> for Plugin {
    fn activate(
        _host: clack_plugin::prelude::HostAudioProcessorHandle<'a>,
        _main: &(),
        _shared: &'a (),
        _config: clack_plugin::prelude::PluginAudioConfiguration,
    ) -> Result<Self, clack_plugin::prelude::PluginError> {
        Ok(Self)
    }
    fn process(
        &mut self,
        _process: clack_plugin::prelude::Process,
        _audio: clack_plugin::prelude::Audio,
        _events: clack_plugin::prelude::Events,
    ) -> Result<clack_plugin::prelude::ProcessStatus, clack_plugin::prelude::PluginError> {
        Ok(clack_plugin::prelude::ProcessStatus::Continue)
    }
}
static PLUGIN_ENTRY: EntryDescriptor =
    clack_entry!(clack_plugin::prelude::SinglePluginEntry<Plugin>);

#[test]
fn sdk_storage_instance_owners_match_actual_allocator_and_aliases() {
    let ((entry, info, mut instance), facts) = observe(|| {
        // SAFETY: the descriptor and plugin are generated by the same canonical SDK.
        let entry = unsafe { PluginEntry::load_from_raw(&PLUGIN_ENTRY, c"") }.unwrap();
        let info = HostInfo::new("Host", "Vendor", "https://example.invalid", "1").unwrap();
        let instance =
            PluginInstance::<Host>::new(|_| (), |_| (), &entry, c"org.mondrian.sdk-storage", &info)
                .unwrap();
        (entry, info, instance)
    });
    let owners = instance.sdk_owned_storage();
    assert_eq!(owners.len(), 9);
    assert_eq!(
        owners
            .iter()
            .filter(|view| view.kind() == HostStorageKind::ArcPayload)
            .count(),
        4
    );
    let physical: Vec<_> = owners
        .iter()
        .map(|view| assert_matches(*view, &facts).address)
        .collect();
    let unique: std::collections::HashSet<_> = physical.iter().copied().collect();
    assert_eq!(unique.len(), 9);
    let info_alias = info.clone();
    for (original, alias) in info
        .sdk_owned_storage()
        .iter()
        .zip(info_alias.sdk_owned_storage())
    {
        assert_eq!(
            (original.address(), original.layout(), original.kind()),
            (alias.address(), alias.layout(), alias.kind())
        );
    }
    let identities: Vec<_> = owners.iter().map(|view| view.address()).collect();
    let audio = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: 48_000.0,
                min_frames_count: 1,
                max_frames_count: 64,
            },
        )
        .unwrap();
    assert_eq!(
        identities,
        instance.sdk_owned_storage().map(HostStorageRef::address)
    );
    instance.deactivate(audio);
    assert_eq!(
        identities,
        instance.sdk_owned_storage().map(HostStorageRef::address)
    );
    drop(entry); // Instance still owns its entry alias.
}

#[repr(align(128))]
struct Aligned([u8; 128]);

#[test]
fn sdk_storage_public_arc_payload_probe_covers_alignment_and_dst() {
    let ((aligned, slice), facts) = observe(|| {
        (
            Arc::new(Aligned([0; 128])),
            Arc::<[u8]>::from(&b"abcdef"[..]),
        )
    });
    assert_eq!(aligned.0[0], 0);
    for view in [
        HostStorageRef::from_arc(&aligned),
        HostStorageRef::from_arc(&slice),
    ] {
        assert_eq!(view.kind(), HostStorageKind::ArcPayload);
        let observed = assert_matches(view, &facts);
        assert!(
            observed.bytes > view.layout().size(),
            "payload-only bytes are not a full Arc allocation receipt"
        );
    }
}

static INITS: AtomicUsize = AtomicUsize::new(0);
static DEINITS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn init(_path: *const std::ffi::c_char) -> bool {
    INITS.fetch_add(1, Ordering::SeqCst);
    true
}
unsafe extern "C" fn deinit() {
    DEINITS.fetch_add(1, Ordering::SeqCst);
}
static COUNTED_ENTRY: EntryDescriptor = EntryDescriptor {
    clap_version: clack_host::utils::ClapVersion::CURRENT.to_raw(),
    init: Some(init),
    deinit: Some(deinit),
    get_factory: None,
};

#[test]
fn sdk_storage_cache_keeps_native_init_deinit_alias_contract() {
    INITS.store(0, Ordering::SeqCst);
    DEINITS.store(0, Ordering::SeqCst);
    // SAFETY: this is the static conforming local test entry descriptor.
    let first = unsafe { PluginEntry::load_from_raw(&COUNTED_ENTRY, c"") }.unwrap();
    // SAFETY: same stable descriptor; loading another handle must not init twice.
    let second = unsafe { PluginEntry::load_from_raw(&COUNTED_ENTRY, c"") }.unwrap();
    let clone = first.clone();
    assert_eq!(INITS.load(Ordering::SeqCst), 1);
    drop(first);
    drop(second);
    assert_eq!(DEINITS.load(Ordering::SeqCst), 0);
    drop(clone);
    assert_eq!(DEINITS.load(Ordering::SeqCst), 1);
}

#[test]
fn sdk_storage_cache_complete_vec_and_provider_owners_match_actual_allocator() {
    // 64 distinct descriptors force growth beyond all earlier tiny test caches.
    let (entries, facts) = observe(|| {
        let mut entries = Vec::with_capacity(64);
        for _ in 0..64 {
            let descriptor = Arc::new(EntryDescriptor {
                clap_version: clack_host::utils::ClapVersion::CURRENT.to_raw(),
                init: Some(init),
                deinit: Some(deinit),
                get_factory: None,
            });
            // SAFETY: this Arc provider owns its stable conforming test descriptor.
            entries.push(unsafe { PluginEntry::load_from(descriptor, c"") }.unwrap());
        }
        entries
    });
    let guard = entries[0].cache_owned_storage().unwrap();
    let mut visited = 0;
    guard
        .visit_owned_storage(&mut |view| {
            assert_matches(view, &facts);
            visited += 1;
        })
        .unwrap();
    assert_eq!(
        visited, 129,
        "one cache Vec, 64 loaded-entry Arcs, 64 provider Arcs"
    );
    // Matching/printing above is test bookkeeping and is excluded. Only the
    // SDK visitor with a fixed arithmetic sink is observed in this control.
    let mut checksum = 0usize;
    let (_, zero) = observe(|| {
        guard
            .visit_owned_storage(&mut |view| {
                checksum ^= view.address() ^ view.layout().size();
            })
            .unwrap()
    });
    assert_eq!(zero.calls, 0);
    std::hint::black_box(checksum);
    drop(guard); // Unload is only legal after the cache borrow has ended.
}

struct UnknownProvider {
    descriptor: &'static EntryDescriptor,
    _owned: Vec<u8>,
}
// SAFETY: the descriptor is static and all callbacks remain valid for the provider lifetime.
unsafe impl EntryProvider for UnknownProvider {
    fn entry_pointer(&self) -> NonNull<EntryDescriptor> {
        self.descriptor.into()
    }
}

#[test]
fn sdk_storage_unknown_provider_and_in_process_entry_explicitly_refuse_proof() {
    // SAFETY: the provider preserves the same stable conforming test descriptor.
    let entry = unsafe {
        PluginEntry::load_from(
            UnknownProvider {
                descriptor: &COUNTED_ENTRY,
                _owned: vec![0; 512],
            },
            c"",
        )
    }
    .unwrap();
    let guard = entry.cache_owned_storage().unwrap();
    assert_eq!(
        guard.visit_owned_storage(&mut |_| {}),
        Err(HostStorageError::ProviderStorageUnavailable)
    );
    drop(guard);
    drop(entry);
    let entry =
        PluginEntry::load_from_clack::<clack_plugin::prelude::SinglePluginEntry<Plugin>>(c"")
            .unwrap();
    assert!(matches!(
        entry.cache_owned_storage(),
        Err(HostStorageError::InProcessPluginStorageUnavailable)
    ));
}

#[test]
fn sdk_storage_zero_vec_and_nul_inclusive_box_layouts_are_truthful() {
    let empty: Vec<u32> = Vec::new();
    assert_eq!(HostStorageRef::from_vec(&empty).unwrap().layout().size(), 0);
    let nul = std::ffi::CString::new("host").unwrap().into_boxed_c_str();
    let view = HostStorageRef::from_box(&nul);
    assert_eq!(view.layout().size(), CStr::to_bytes_with_nul(&nul).len());
    assert_eq!(view.kind(), HostStorageKind::CompleteAllocation);
}
