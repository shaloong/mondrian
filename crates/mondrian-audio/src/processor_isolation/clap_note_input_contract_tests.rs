//! Owned raw ABI controls for negotiation and callback source/lifecycle only.
#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use clap_sys::*;
use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr};
use std::ptr;

const ID: &CStr = c"org.mondrian.owned.restored-note-ports";
static DESCRIPTOR: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: clap_sys::version::CLAP_VERSION,
    id: ID.as_ptr(),
    name: c"Owned restored ports".as_ptr(),
    vendor: c"Mondrian".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"0".as_ptr(),
    description: c"ABI controls only".as_ptr(),
    features: ptr::null(),
};
struct Owned {
    plugin: clap_plugin,
    host: *const clap_host,
    mode: Cell<u8>,
    active: Cell<bool>,
    gets: Cell<u32>,
}
unsafe fn owned<'a>(plugin: *const clap_plugin) -> &'a Owned {
    // SAFETY: each callback/test borrows this exact Box from its live plugin.
    unsafe { &*((*plugin).plugin_data.cast::<Owned>()) }
}
unsafe fn host_ports(owner: &Owned) -> *const clap_host_note_ports {
    unsafe { (*owner.host).get_extension.unwrap()(owner.host, CLAP_EXT_NOTE_PORTS.as_ptr()) }.cast()
}
unsafe fn rescan(owner: &Owned, flags: u32) {
    let ports = unsafe { host_ports(owner) };
    if !ports.is_null() {
        unsafe { (*ports).rescan.unwrap()(owner.host, flags) };
    }
}
unsafe extern "C" fn init(_: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    unsafe { drop(Box::from_raw((*plugin).plugin_data.cast::<Owned>())) };
}
unsafe extern "C" fn activate(plugin: *const clap_plugin, _: f64, _: u32, _: u32) -> bool {
    unsafe { owned(plugin) }.active.set(true);
    true
}
unsafe extern "C" fn deactivate(plugin: *const clap_plugin) {
    let owner = unsafe { owned(plugin) };
    owner.active.set(false);
    // Official ALL is legal inside the actual deactivate callback.
    unsafe { rescan(owner, 1) };
}
unsafe extern "C" fn start(_: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn stop(_: *const clap_plugin) {}
unsafe extern "C" fn reset(_: *const clap_plugin) {}
unsafe extern "C" fn process(_: *const clap_plugin, _: *const clap_process) -> clap_process_status {
    CLAP_PROCESS_ERROR
}
unsafe extern "C" fn on_main(_: *const clap_plugin) {}
unsafe extern "C" fn count(plugin: *const clap_plugin, input: bool) -> u32 {
    if !input {
        return 0;
    }
    if unsafe { owned(plugin) }.mode.get() == 6 {
        33
    } else {
        2
    }
}
unsafe extern "C" fn get(
    plugin: *const clap_plugin,
    index: u32,
    input: bool,
    output: *mut clap_note_port_info,
) -> bool {
    let owner = unsafe { owned(plugin) };
    owner.gets.set(owner.gets.get() + 1);
    if !input || index >= 2 || output.is_null() || owner.mode.get() == 5 {
        return false;
    }
    if owner.mode.get() == 8 {
        unsafe { rescan(owner, 2) };
    }
    let high = if owner.mode.get() == 1 {
        index == 0
    } else {
        index == 1
    };
    let mut raw = clap_note_port_info {
        id: if high { 0x8000_0017 } else { 0 },
        supported_dialects: if owner.mode.get() == 3 && index == 1 {
            2
        } else {
            0x8000_0001
        },
        preferred_dialect: if owner.mode.get() == 2 && index == 1 {
            0x8000_0000
        } else {
            1
        },
        name: [0; 256],
    };
    if owner.mode.get() == 4 {
        raw.id = 0;
    }
    raw.name[0] = b'A' as c_char;
    unsafe { output.write(raw) };
    true
}
static PORTS: clap_plugin_note_ports =
    clap_plugin_note_ports { count: Some(count), get: Some(get) };
static NO_GET: clap_plugin_note_ports = clap_plugin_note_ports { count: Some(count), get: None };
unsafe extern "C" fn load(plugin: *const clap_plugin, input: *const clap_istream) -> bool {
    let mut mode = 0_u8;
    if unsafe { (*input).read.unwrap()(input, (&mut mode as *mut u8).cast(), 1) } != 1 {
        return false;
    }
    let owner = unsafe { owned(plugin) };
    owner.mode.set(mode);
    unsafe { rescan(owner, 1) }; // Legal state-load cohort, must survive until full scan.
    true
}
static STATE: clap_plugin_state = clap_plugin_state { save: None, load: Some(load) };
unsafe extern "C" fn extension(plugin: *const clap_plugin, id: *const c_char) -> *const c_void {
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_NOTE_PORTS {
        if unsafe { owned(plugin) }.mode.get() == 7 {
            (&NO_GET as *const clap_plugin_note_ports).cast()
        } else {
            (&PORTS as *const clap_plugin_note_ports).cast()
        }
    } else if id == CLAP_EXT_STATE {
        (&STATE as *const clap_plugin_state).cast()
    } else {
        ptr::null()
    }
}
unsafe extern "C" fn factory_count(_: *const clap_plugin_factory) -> u32 {
    1
}
unsafe extern "C" fn descriptor(
    _: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    if index == 0 {
        &DESCRIPTOR
    } else {
        ptr::null()
    }
}
unsafe extern "C" fn create(
    _: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    if host.is_null() || unsafe { CStr::from_ptr(id) } != ID {
        return ptr::null();
    }
    let mut owner = Box::new(Owned {
        plugin: clap_plugin {
            desc: &DESCRIPTOR,
            plugin_data: ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(deactivate),
            start_processing: Some(start),
            stop_processing: Some(stop),
            reset: Some(reset),
            process: Some(process),
            get_extension: Some(extension),
            on_main_thread: Some(on_main),
        },
        host,
        mode: Cell::new(0),
        active: Cell::new(false),
        gets: Cell::new(0),
    });
    owner.plugin.plugin_data = (&mut *owner as *mut Owned).cast();
    let plugin = &owner.plugin as *const clap_plugin;
    let _ = Box::into_raw(owner);
    plugin
}
static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(descriptor),
    create_plugin: Some(create),
};
unsafe extern "C" fn entry_init(_: *const c_char) -> bool {
    true
}
unsafe extern "C" fn entry_deinit() {}
unsafe extern "C" fn factory(id: *const c_char) -> *const c_void {
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        (&FACTORY as *const clap_plugin_factory).cast()
    } else {
        ptr::null()
    }
}
static ENTRY: clap_plugin_entry = clap_plugin_entry {
    clap_version: clap_sys::version::CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(factory),
};

fn instance(notes: bool) -> PluginInstance<ClapHost> {
    // SAFETY: original owned static factory/callbacks remain live for this SDK entry.
    let entry =
        unsafe { PluginEntry::load_from_raw(&ENTRY, c"/owned/restored-note-ports") }.unwrap();
    let shared = if notes {
        ClapHostShared::new_with_note_input()
    } else {
        ClapHostShared::new()
    };
    let native_shared = shared.with_native_logs().unwrap();
    let info = HostInfo::new("Mondrian", "Mondrian", "https://example.invalid", "0").unwrap();
    PluginInstance::new(
        |_| native_shared,
        |shared| super::super::clap_main_services::ClapHostMainThread::new(shared),
        &entry,
        ID,
        &info,
    )
    .unwrap()
}
fn restore(instance: &mut PluginInstance<ClapHost>, mode: u8) {
    let handle = instance.plugin_handle();
    let extension = handle.get_extension::<PluginState>().unwrap();
    let bytes = [mode];
    let mut input = &bytes[..];
    extension.load(&handle, &mut input).unwrap();
}
fn ports(
    instance: &mut PluginInstance<ClapHost>,
) -> (*const clap_host, *const clap_host_note_ports) {
    // SAFETY: this private fixture belongs to this live SDK instance; pointers
    // are borrowed until the synchronous/scoped callback returns and joins.
    let owner = unsafe { owned(instance.plugin_handle().as_raw()) };
    (owner.host, unsafe { host_ports(owner) })
}

#[test]
fn actual_abi_restored_scan_keeps_unsigned_ids_indices_and_retires_only_exact_state_cohort() {
    let mut instance = instance(true);
    restore(&mut instance, 1);
    assert!(instance.access_shared_handler(|shared| shared
        .services
        .note_ports
        .snapshot()
        .pending()));
    let binding = scan(&mut instance, offered_limits(), None, false).unwrap();
    assert_eq!(
        binding
            .contract
            .ports
            .iter()
            .map(|port| (port.native_id, port.native_index))
            .collect::<Vec<_>>(),
        [(0x8000_0017, 0), (0, 1)]
    );
    assert_eq!(
        binding.contract.ports[0].port_key,
        "clap.note-input.2147483671"
    );
    assert_eq!(
        binding.fingerprints[0].supported, 0x8000_0001,
        "unknown supported bits retained"
    );
    assert!(!instance.access_shared_handler(|shared| shared
        .services
        .note_ports
        .snapshot()
        .pending()));
    let (host, raw) = ports(&mut instance);
    assert_eq!(unsafe { (*raw).supported_dialects.unwrap()(host) }, 1);
    assert_eq!(binding.contract.limits, offered_limits());
}

#[test]
fn actual_abi_bad_ports_refuse_without_compressing_or_retiring_pending_source() {
    for mode in [2, 3, 4, 5, 6, 7, 8] {
        let mut instance = instance(true);
        restore(&mut instance, mode);
        assert!(
            scan(&mut instance, offered_limits(), None, false).is_err(),
            "mode {mode}"
        );
        assert!(instance.access_shared_handler(|shared| shared
            .services
            .note_ports
            .snapshot()
            .pending()));
        if mode == 6 || mode == 7 {
            assert_eq!(
                unsafe { owned(instance.plugin_handle().as_raw()) }.gets.get(),
                0,
                "invalid count/missing getter precedes every native get"
            );
        }
    }
}

#[test]
fn actual_restored_reordered_ports_mismatch_keeps_full_scan_cohort_pending() {
    let mut instance = instance(true);
    let original = scan(&mut instance, offered_limits(), None, false).unwrap();
    restore(&mut instance, 1);
    assert!(negotiate_restored(&mut instance, &original.contract).is_err());
    assert_eq!(
        instance.access_shared_handler(|shared| shared.services.note_ports.snapshot().flags()),
        1
    );
    assert_eq!(original.contract.ports[0].native_id, 0);
}

#[test]
fn actual_abi_names_preserve_identity_and_all_is_legal_only_after_actual_deactivation() {
    let mut instance = instance(true);
    let binding = scan(&mut instance, offered_limits(), None, false).unwrap();
    let processor = instance
        .activate(
            |_, _| (),
            clack_host::prelude::PluginAudioConfiguration {
                sample_rate: 48_000.0,
                min_frames_count: 1,
                max_frames_count: 4,
            },
        )
        .unwrap();
    instance.access_shared_handler(|shared| shared.services.active.store(true, Ordering::Release));
    let (host, raw) = ports(&mut instance);
    unsafe { (*raw).rescan.unwrap()(host, 2) };
    service_names(&mut instance, &binding).unwrap();
    assert_eq!(binding.contract.ports[0].native_id, 0);
    drop(processor); // No processing was started.
    instance.access_shared_handler(|shared| shared.services.active.store(false, Ordering::Release));
    instance.try_deactivate().unwrap(); // Native callback records legal ALL.
    assert_eq!(
        instance.access_shared_handler(|shared| shared.services.note_ports.snapshot().flags()),
        1
    );
    scan(&mut instance, offered_limits(), Some(&binding), false).unwrap();
    assert!(!instance.access_shared_handler(|shared| shared
        .services
        .note_ports
        .snapshot()
        .pending()));
}

#[test]
fn actual_raw_callbacks_refuse_wrong_thread_active_all_unknown_bits_and_keep_separate_facts() {
    let mut instance = instance(true);
    let processor = instance
        .activate(
            |_, _| (),
            clack_host::prelude::PluginAudioConfiguration {
                sample_rate: 48_000.0,
                min_frames_count: 1,
                max_frames_count: 4,
            },
        )
        .unwrap();
    instance.access_shared_handler(|shared| shared.services.active.store(true, Ordering::Release));
    let (host, raw) = ports(&mut instance);
    unsafe { (*raw).rescan.unwrap()(host, 1 | (1 << 31)) };
    let callback = unsafe { (*raw).rescan.unwrap() };
    let query = unsafe { (*raw).supported_dialects.unwrap() };
    let address = host as usize;
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                // SAFETY: this same live wrapper remains pinned until joined. The raw
                // seam must reject on immutable Shared before any main/Cell borrow.
                let host = address as *const clap_host;
                assert_eq!(unsafe { query(host) }, 0);
                unsafe { callback(host, 2) };
            })
            .join()
            .unwrap()
    });
    instance.access_shared_handler(|shared| {
        let state = &shared.services.note_ports;
        assert_eq!(state.observed_flags.load(Ordering::Acquire), 3 | (1 << 31));
        let failures = state.failures.load(Ordering::Acquire);
        assert_ne!(failures & (1 << NotePortFailure::WrongThread as u8), 0);
        assert_ne!(failures & (1 << NotePortFailure::ActiveAll as u8), 0);
        assert_ne!(failures & (1 << NotePortFailure::UnknownFlags as u8), 0);
        assert!(state.check().is_err());
        assert!(state.retire_scanned(state.snapshot()).is_err());
    });
    drop(processor);
    instance.access_shared_handler(|shared| shared.services.active.store(false, Ordering::Release));
    instance.try_deactivate().unwrap();
}

#[test]
fn actual_active_names_with_changed_native_ids_cannot_replace_binding_or_retire_source() {
    let mut instance = instance(true);
    let binding = scan(&mut instance, offered_limits(), None, false).unwrap();
    let processor = instance
        .activate(
            |_, _| (),
            clack_host::prelude::PluginAudioConfiguration {
                sample_rate: 48_000.0,
                min_frames_count: 1,
                max_frames_count: 4,
            },
        )
        .unwrap();
    instance.access_shared_handler(|shared| shared.services.active.store(true, Ordering::Release));
    // Controlled native producer violation: NAMES advertises a swapped port
    // table while active. The test reads it through actual count/get ABI calls.
    unsafe { owned(instance.plugin_handle().as_raw()) }.mode.set(1);
    let (host, raw) = ports(&mut instance);
    unsafe { (*raw).rescan.unwrap()(host, 2) };
    assert!(service_names(&mut instance, &binding).is_err());
    assert_eq!(binding.contract.ports[0].native_id, 0);
    assert_eq!(
        instance.access_shared_handler(|shared| shared.services.note_ports.snapshot().flags()),
        2
    );
    drop(processor);
    instance.access_shared_handler(|shared| shared.services.active.store(false, Ordering::Release));
    instance.try_deactivate().unwrap();
}

#[test]
fn actual_metadata_only_instance_never_registers_host_note_capability() {
    let mut instance = instance(false);
    let (_, raw) = ports(&mut instance);
    assert!(raw.is_null());
    let metadata = super::super::clap_note_input_metadata::inspect_loaded(
        &unsafe { PluginEntry::load_from_raw(&ENTRY, c"/owned/restored-note-ports-metadata") }
            .unwrap(),
        ID.to_str().unwrap(),
        [7; 32],
        None,
    )
    .unwrap();
    assert_eq!(metadata.host_supported_dialects, 0);
    assert_eq!(
        metadata.input_execution,
        super::super::clap_note_input_metadata::ClapNoteExecution::Unsupported
    );
}
