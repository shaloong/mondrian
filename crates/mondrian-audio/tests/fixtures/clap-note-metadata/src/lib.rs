//! Original facts-only ABI oracle using the existing official generated types.
//! Instances are Box-owned until main-thread destroy. No DSP or audio ports.
#![deny(unsafe_op_in_unsafe_fn)]

use clap_sys::*;
use std::ffi::{c_char, c_void, CStr};
use std::ptr;

const fn descriptor(id: &CStr) -> clap_plugin_descriptor {
    clap_plugin_descriptor {
        clap_version: clap_sys::version::CLAP_VERSION,
        id: id.as_ptr(),
        name: c"Mondrian owned NotePort metadata oracle".as_ptr(),
        vendor: c"Mondrian".as_ptr(),
        url: c"".as_ptr(),
        manual_url: c"".as_ptr(),
        support_url: c"".as_ptr(),
        version: c"0.0.0".as_ptr(),
        description: c"Inactive metadata only; never a synthesizer".as_ptr(),
        features: ptr::null(),
    }
}

static DESCRIPTORS: [clap_plugin_descriptor; 16] = [
    descriptor(c"org.mondrian.fixture.note-metadata.0"),
    descriptor(c"org.mondrian.fixture.note-metadata.1"),
    descriptor(c"org.mondrian.fixture.note-metadata.2"),
    descriptor(c"org.mondrian.fixture.note-metadata.3"),
    descriptor(c"org.mondrian.fixture.note-metadata.4"),
    descriptor(c"org.mondrian.fixture.note-metadata.5"),
    descriptor(c"org.mondrian.fixture.note-metadata.6"),
    descriptor(c"org.mondrian.fixture.note-metadata.7"),
    descriptor(c"org.mondrian.fixture.note-metadata.8"),
    descriptor(c"org.mondrian.fixture.note-metadata.9"),
    descriptor(c"org.mondrian.fixture.note-metadata.10"),
    descriptor(c"org.mondrian.fixture.note-metadata.11"),
    descriptor(c"org.mondrian.fixture.note-metadata.12"),
    descriptor(c"org.mondrian.fixture.note-metadata.13"),
    descriptor(c"org.mondrian.fixture.note-metadata.14"),
    descriptor(c"org.mondrian.fixture.note-metadata.15"),
];

struct Instance {
    plugin: clap_plugin,
    host: *const clap_host,
    mode: u8,
    state: u8,
}

// SAFETY at native boundaries: CLAP requires live matching plugin/host pointers;
// create owns one Box and destroy consumes it once. No plugin-data pointer is
// shared between instances, and all metadata/state operations use main role.
unsafe fn instance<'a>(plugin: *const clap_plugin) -> &'a Instance {
    unsafe { &*((*plugin).plugin_data.cast::<Instance>()) }
}

unsafe fn main_role(plugin: *const clap_plugin) -> bool {
    let owner = unsafe { instance(plugin) };
    let host = owner.host;
    let Some(get) = (unsafe { (*host).get_extension }) else {
        return false;
    };
    let check =
        unsafe { get(host, CLAP_EXT_THREAD_CHECK.as_ptr()).cast::<clap_host_thread_check>() };
    if check.is_null() {
        return false;
    }
    let Some(main) = (unsafe { (*check).is_main_thread }) else {
        return false;
    };
    let Some(audio) = (unsafe { (*check).is_audio_thread }) else {
        return false;
    };
    unsafe { main(host) && !audio(host) }
}

unsafe fn restart(plugin: *const clap_plugin) {
    let host = unsafe { instance(plugin).host };
    if let Some(request) = unsafe { (*host).request_restart } {
        unsafe { request(host) };
    }
}

unsafe extern "C" fn init(plugin: *const clap_plugin) -> bool {
    if !unsafe { main_role(plugin) } {
        return false;
    }
    let owner = unsafe { instance(plugin) };
    let Some(get) = (unsafe { (*owner.host).get_extension }) else {
        return false;
    };
    // The actual production host must not advertise note support merely because
    // this operation can inspect plugin ports.
    if !unsafe { get(owner.host, CLAP_EXT_NOTE_PORTS.as_ptr()) }.is_null() {
        return false;
    }
    if owner.mode == 11 {
        unsafe { restart(plugin) };
    }
    if owner.mode == 14 {
        let Some(request) = (unsafe { (*owner.host).request_callback }) else {
            return false;
        };
        unsafe { request(owner.host) };
    }
    true
}

unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    // SAFETY: this exact pointer was set by create and destroy is called once.
    unsafe { drop(Box::from_raw((*plugin).plugin_data.cast::<Instance>())) };
}

unsafe extern "C" fn activate(_: *const clap_plugin, _: f64, _: u32, _: u32) -> bool {
    false
}
unsafe extern "C" fn deactivate(_: *const clap_plugin) {}
unsafe extern "C" fn start(_: *const clap_plugin) -> bool {
    false
}
unsafe extern "C" fn stop(_: *const clap_plugin) {}
unsafe extern "C" fn reset(_: *const clap_plugin) {}
unsafe extern "C" fn process(_: *const clap_plugin, _: *const clap_process) -> clap_process_status {
    CLAP_PROCESS_ERROR
}
unsafe extern "C" fn on_main(plugin: *const clap_plugin) {
    if unsafe { instance(plugin).mode } == 14 {
        unsafe { restart(plugin) };
    }
}

unsafe extern "C" fn count(plugin: *const clap_plugin, input: bool) -> u32 {
    if !unsafe { main_role(plugin) } || !input {
        return u32::MAX;
    }
    let owner = unsafe { instance(plugin) };
    match owner.mode {
        2 | 15 => 0,
        8 => 33,
        12 => {
            unsafe { restart(plugin) };
            2
        }
        _ => 2,
    }
}

unsafe extern "C" fn get(
    plugin: *const clap_plugin,
    index: u32,
    input: bool,
    out: *mut clap_note_port_info,
) -> bool {
    if out.is_null() || !input || index >= 2 || !unsafe { main_role(plugin) } {
        return false;
    }
    let owner = unsafe { instance(plugin) };
    if owner.mode == 8 {
        // Any get before the excessive count is refused is a host defect.
        std::process::abort();
    }
    if owner.mode == 5 {
        return false;
    }
    if owner.mode == 13 {
        unsafe { restart(plugin) };
    }
    if owner.mode == 9 && index == 1 {
        // Deliberately write only id. The host must not leak prior port bytes.
        unsafe {
            (*out).id = 0;
        }
        return true;
    }
    let high = if owner.state == 1 {
        index == 1
    } else {
        index == 0
    };
    let mut value = clap_note_port_info {
        id: if high { u32::MAX - 1 } else { 0 },
        name: [0; 256],
        supported_dialects: if high { 0x8000_0003 } else { 1 },
        preferred_dialect: if high { 0x8000_0000 } else { 1 },
    };
    if owner.mode == 6 {
        value.id = 7;
    }
    if owner.mode == 7 {
        value.id = u32::MAX;
    }
    if owner.mode == 10 {
        value.name = [b'x' as c_char; 256];
    } else {
        for (slot, byte) in value.name.iter_mut().zip(b"owned raw note input") {
            *slot = *byte as c_char;
        }
    }
    unsafe {
        out.write(value);
    }
    true
}

static PORTS: clap_plugin_note_ports =
    clap_plugin_note_ports { count: Some(count), get: Some(get) };
static NO_COUNT: clap_plugin_note_ports = clap_plugin_note_ports { count: None, get: Some(get) };
static NO_GET: clap_plugin_note_ports = clap_plugin_note_ports { count: Some(count), get: None };

unsafe extern "C" fn load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    if stream.is_null() || !unsafe { main_role(plugin) } {
        return false;
    }
    let Some(read) = (unsafe { (*stream).read }) else {
        return false;
    };
    let mut state = 0u8;
    let size = unsafe { read(stream, (&mut state as *mut u8).cast(), 1) };
    if !(0..=1).contains(&size) || state > 2 {
        return false;
    }
    // Only the inactive main thread may mutate this instance's native state.
    unsafe {
        (*(*plugin).plugin_data.cast::<Instance>()).state = state;
    }
    if state == 2 {
        unsafe { restart(plugin) };
    }
    true
}

unsafe extern "C" fn save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    if stream.is_null() || !unsafe { main_role(plugin) } {
        return false;
    }
    let Some(write) = (unsafe { (*stream).write }) else {
        return false;
    };
    let state = unsafe { instance(plugin).state };
    unsafe { write(stream, (&state as *const u8).cast(), 1) == 1 }
}

static STATE: clap_plugin_state = clap_plugin_state { save: Some(save), load: Some(load) };

unsafe extern "C" fn extension(plugin: *const clap_plugin, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return ptr::null();
    }
    let id = unsafe { CStr::from_ptr(id) };
    let owner = unsafe { instance(plugin) };
    if id == CLAP_EXT_STATE {
        return (&STATE as *const clap_plugin_state).cast();
    }
    if id != CLAP_EXT_NOTE_PORTS || owner.mode == 1 {
        return ptr::null();
    }
    let ports = match owner.mode {
        3 => &NO_COUNT,
        4 | 15 => &NO_GET,
        _ => &PORTS,
    };
    (ports as *const clap_plugin_note_ports).cast()
}

unsafe extern "C" fn descriptor_count(_: *const clap_plugin_factory) -> u32 {
    DESCRIPTORS.len() as u32
}
unsafe extern "C" fn get_descriptor(
    _: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    DESCRIPTORS.get(index as usize).map_or(ptr::null(), |row| row)
}

unsafe extern "C" fn create(
    _: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    if host.is_null() || id.is_null() {
        return ptr::null();
    }
    let id = unsafe { CStr::from_ptr(id) };
    let Some(mode) = DESCRIPTORS.iter().position(|row| unsafe { CStr::from_ptr(row.id) } == id)
    else {
        return ptr::null();
    };
    let mut owner = Box::new(Instance {
        plugin: clap_plugin {
            desc: &DESCRIPTORS[mode],
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
        mode: mode as u8,
        state: 0,
    });
    owner.plugin.plugin_data = (&mut *owner as *mut Instance).cast();
    let plugin = &owner.plugin as *const clap_plugin;
    let _owned = Box::into_raw(owner);
    plugin
}

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(descriptor_count),
    get_plugin_descriptor: Some(get_descriptor),
    create_plugin: Some(create),
};

unsafe extern "C" fn entry_init(_: *const c_char) -> bool {
    true
}
unsafe extern "C" fn entry_deinit() {}
unsafe extern "C" fn get_factory(id: *const c_char) -> *const c_void {
    if !id.is_null() && unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        (&FACTORY as *const clap_plugin_factory).cast()
    } else {
        ptr::null()
    }
}

#[unsafe(no_mangle)]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: clap_sys::version::CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(get_factory),
};
