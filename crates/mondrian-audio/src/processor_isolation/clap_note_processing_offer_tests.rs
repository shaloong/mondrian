//! Original raw ABI control. No DSP/real-time/third-party qualification.
#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use clap_sys::*;
use mondrian_core::AudioChannelLayout;
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr};
use std::ptr;
use std::rc::Rc;

const ID: &CStr = c"org.mondrian.owned.coherent-note-offer";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Good,
    ZeroAudioInput,
    NamedAuxiliary,
    LoadFailure,
    RenderFailure,
    BadParameter,
    BadAudio,
    BadNote,
    ActivateFailure,
    RestartLatency,
    CallbackTail,
    ActiveAllLatency,
    UnknownTail,
    WrongThreadTail,
    ChangedParameter,
    ChangedAudio,
    ChangedNote,
    DestroyRestart,
    DestroyUnknownNote,
    DestroyNames,
    DestroyFlush,
    DestroyWrongThread,
    ParameterRescan,
    ParameterFlush,
}
struct Audit {
    trace: Cell<[u8; 128]>,
    length: Cell<usize>,
    violations: Cell<u32>,
    creates: Cell<u32>,
    destroys: Cell<u32>,
    activates: Cell<u32>,
    deactivates: Cell<u32>,
    starts: Cell<u32>,
    processes: Cell<u32>,
    main_callbacks: Cell<u32>,
    mask: Cell<u32>,
}
impl Default for Audit {
    fn default() -> Self {
        Self {
            trace: Cell::new([0; 128]),
            length: Cell::new(0),
            violations: Cell::new(0),
            creates: Cell::new(0),
            destroys: Cell::new(0),
            activates: Cell::new(0),
            deactivates: Cell::new(0),
            starts: Cell::new(0),
            processes: Cell::new(0),
            main_callbacks: Cell::new(0),
            mask: Cell::new(0),
        }
    }
}
impl Audit {
    fn record(&self, stage: u8) {
        let index = self.length.get();
        if index < 128 {
            let mut trace = self.trace.get();
            trace[index] = stage;
            self.trace.set(trace);
            self.length.set(index + 1);
        } else {
            self.violations.set(self.violations.get() | 8);
        }
    }
    fn stages(&self) -> Vec<u8> {
        self.trace.get()[..self.length.get()].to_vec()
    }
}
thread_local! {
    static NEXT: RefCell<Option<(Mode, Rc<Audit>)>> = const { RefCell::new(None) };
}
struct Owned {
    plugin: clap_plugin,
    host: *const clap_host,
    check: Cell<*const clap_host_thread_check>,
    cached_notes: Cell<*const clap_host_note_ports>,
    cached_params: Cell<*const clap_host_params>,
    notes: Cell<bool>,
    restored: Cell<u8>,
    offline: Cell<bool>,
    active: Cell<bool>,
    changed: Cell<bool>,
    mode: Mode,
    audit: Rc<Audit>,
}
unsafe fn owner<'a>(plugin: *const clap_plugin) -> &'a Owned {
    // SAFETY: only the SDK's matching live callback owner enters this fixture.
    unsafe { &*((*plugin).plugin_data.cast::<Owned>()) }
}
unsafe fn host_extension<T>(owner: &Owned, id: &CStr) -> *const T {
    unsafe { (*owner.host).get_extension.unwrap()(owner.host, id.as_ptr()) }.cast()
}
unsafe fn role(owner: &Owned, stage: u8, audio: bool, active: bool) {
    owner.audit.record(stage);
    let check = owner.check.get();
    let valid = !check.is_null()
        && unsafe {
            if audio {
                (*check).is_audio_thread.unwrap()(owner.host)
            } else {
                (*check).is_main_thread.unwrap()(owner.host)
            }
        };
    if !valid {
        owner
            .audit
            .violations
            .set(owner.audit.violations.get() | if audio { 2 } else { 1 });
    }
    if owner.active.get() != active {
        owner.audit.violations.set(owner.audit.violations.get() | 4);
    }
}
unsafe fn rescan(owner: &Owned, flags: u32) {
    let ports = owner.cached_notes.get();
    if !ports.is_null() {
        unsafe { (*ports).rescan.unwrap()(owner.host, flags) };
    }
}
unsafe extern "C" fn init(plugin: *const clap_plugin) -> bool {
    let owner = unsafe { owner(plugin) };
    owner.check.set(unsafe { host_extension(owner, CLAP_EXT_THREAD_CHECK) });
    let notes: *const clap_host_note_ports = unsafe { host_extension(owner, CLAP_EXT_NOTE_PORTS) };
    owner.cached_notes.set(notes);
    owner.cached_params.set(unsafe { host_extension(owner, CLAP_EXT_PARAMS) });
    let mask = if notes.is_null() {
        0
    } else {
        unsafe { (*notes).supported_dialects.unwrap()(owner.host) }
    };
    owner.audit.mask.set(mask);
    owner.notes.set(mask == 1);
    unsafe { role(owner, 1, false, false) };
    true
}
unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 11, false, false) };
    match owner.mode {
        Mode::DestroyUnknownNote => unsafe { rescan(owner, 0x8000_0000) },
        Mode::DestroyNames => unsafe { rescan(owner, 2) },
        Mode::DestroyFlush => {
            let params = owner.cached_params.get();
            if !params.is_null() {
                unsafe { (*params).request_flush.unwrap()(owner.host) };
            }
        }
        Mode::DestroyWrongThread => {
            let host = owner.host as usize;
            let callback = unsafe { (*owner.cached_notes.get()).rescan.unwrap() };
            std::thread::scope(|scope| {
                scope.spawn(move || unsafe { callback(host as *const clap_host, 2) });
            });
        }
        _ => {}
    }
    if owner.mode == Mode::DestroyRestart {
        unsafe { (*owner.host).request_restart.unwrap()(owner.host) };
    }
    owner.audit.destroys.set(owner.audit.destroys.get() + 1);
    unsafe { drop(Box::from_raw((*plugin).plugin_data.cast::<Owned>())) };
}
unsafe extern "C" fn activate(plugin: *const clap_plugin, rate: f64, min: u32, max: u32) -> bool {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 7, false, false) };
    owner.audit.activates.set(owner.audit.activates.get() + 1);
    if rate != 48_000.0 || min != 1 || max != 64 {
        owner.audit.violations.set(owner.audit.violations.get() | 16);
    }
    if owner.mode == Mode::ActivateFailure {
        return false;
    }
    owner.active.set(true);
    true
}
unsafe extern "C" fn deactivate(plugin: *const clap_plugin) {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 10, false, true) };
    owner.audit.deactivates.set(owner.audit.deactivates.get() + 1);
    owner.active.set(false);
    owner.changed.set(true);
    unsafe { rescan(owner, 1) }; // Real inactive ALL must be retired only after the exact full scan.
}
unsafe extern "C" fn start(plugin: *const clap_plugin) -> bool {
    let owner = unsafe { owner(plugin) };
    owner.audit.starts.set(owner.audit.starts.get() + 1);
    false
}
unsafe extern "C" fn stop(_: *const clap_plugin) {}
unsafe extern "C" fn reset(_: *const clap_plugin) {}
unsafe extern "C" fn process(
    plugin: *const clap_plugin,
    _: *const clap_process,
) -> clap_process_status {
    let owner = unsafe { owner(plugin) };
    owner.audit.processes.set(owner.audit.processes.get() + 1);
    CLAP_PROCESS_ERROR
}
unsafe extern "C" fn main_callback(plugin: *const clap_plugin) {
    let owner = unsafe { owner(plugin) };
    owner.audit.main_callbacks.set(owner.audit.main_callbacks.get() + 1);
    if owner.mode == Mode::CallbackTail {
        unsafe { (*owner.host).request_callback.unwrap()(owner.host) };
    }
}
unsafe extern "C" fn load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 2, false, false) };
    if owner.mode == Mode::LoadFailure {
        return false;
    }
    let mut byte = 0;
    if unsafe { (*stream).read.unwrap()(stream, (&mut byte as *mut u8).cast(), 1) } != 1 {
        return false;
    }
    owner.restored.set(byte);
    unsafe { rescan(owner, 1) };
    true
}
unsafe extern "C" fn hard_realtime(_: *const clap_plugin) -> bool {
    false
}
unsafe extern "C" fn render(plugin: *const clap_plugin, mode: clap_plugin_render_mode) -> bool {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 3, false, false) };
    owner.offline.set(mode == CLAP_RENDER_OFFLINE as clap_plugin_render_mode);
    owner.mode != Mode::RenderFailure
}
unsafe extern "C" fn parameter_count(plugin: *const clap_plugin) -> u32 {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 4, false, false) };
    1
}
unsafe extern "C" fn parameter_info(
    plugin: *const clap_plugin,
    index: u32,
    out: *mut clap_param_info,
) -> bool {
    if index != 0 || out.is_null() {
        return false;
    }
    let owner = unsafe { owner(plugin) };
    let max = if owner.changed.get() && owner.mode == Mode::ChangedParameter {
        4.0
    } else if owner.notes.get() {
        2.0
    } else {
        1.0
    };
    let mut info = clap_param_info {
        id: 7,
        flags: CLAP_PARAM_IS_AUTOMATABLE as u32,
        cookie: ptr::null_mut(),
        name: [0; 256],
        module: [0; 1024],
        min_value: if owner.mode == Mode::BadParameter {
            3.0
        } else {
            0.0
        },
        max_value: max,
        default_value: 0.25,
    };
    info.name[0] = b'P' as c_char;
    unsafe { out.write(info) };
    true
}
unsafe extern "C" fn parameter_value(plugin: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    if id != 7 || out.is_null() {
        return false;
    }
    let owner = unsafe { owner(plugin) };
    let value = if owner.notes.get() {
        0.5 + f64::from(owner.restored.get())
    } else {
        0.25
    };
    unsafe { out.write(value) };
    true
}
unsafe extern "C" fn audio_count(plugin: *const clap_plugin, input: bool) -> u32 {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 5, false, false) };
    if owner.mode == Mode::BadAudio || (input && owner.mode == Mode::ZeroAudioInput) {
        0
    } else if input && owner.notes.get() {
        2
    } else {
        1
    }
}
unsafe extern "C" fn audio_info(
    plugin: *const clap_plugin,
    index: u32,
    input: bool,
    out: *mut clap_audio_port_info,
) -> bool {
    if out.is_null() || index > 1 || !input && index != 0 {
        return false;
    }
    let owner = unsafe { owner(plugin) };
    let id = if index == 1 {
        if owner.changed.get() && owner.mode == Mode::ChangedAudio {
            34
        } else {
            33
        }
    } else if input {
        1
    } else {
        2
    };
    let mut info = clap_audio_port_info {
        id,
        name: [0; 256],
        flags: if index == 0 {
            CLAP_AUDIO_PORT_IS_MAIN as u32
        } else {
            0
        },
        channel_count: 2,
        port_type: CLAP_PORT_STEREO.as_ptr(),
        in_place_pair: CLAP_INVALID_ID,
    };
    if input && index == 1 && owner.mode == Mode::NamedAuxiliary {
        let name: &[u8] = if owner.changed.get() {
            b"After deactivate"
        } else if owner.restored.get() == 1 && owner.offline.get() {
            b"Restored offline aux"
        } else {
            b"Unconfigured aux"
        };
        for (destination, source) in info.name.iter_mut().zip(name) {
            *destination = *source as c_char;
        }
    }
    unsafe { out.write(info) };
    true
}
unsafe extern "C" fn note_count(plugin: *const clap_plugin, input: bool) -> u32 {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 6, false, false) };
    if input {
        2
    } else {
        0
    }
}
unsafe extern "C" fn note_info(
    plugin: *const clap_plugin,
    index: u32,
    input: bool,
    out: *mut clap_note_port_info,
) -> bool {
    if !input || index >= 2 || out.is_null() {
        return false;
    }
    let owner = unsafe { owner(plugin) };
    let high = if owner.restored.get() == 1 {
        index == 0
    } else {
        index == 1
    };
    let id = if owner.changed.get() && owner.mode == Mode::ChangedNote && high {
        43
    } else if high {
        0x8000_0017
    } else {
        0
    };
    unsafe {
        out.write(clap_note_port_info {
            id,
            supported_dialects: 1,
            preferred_dialect: if owner.mode == Mode::BadNote {
                0x8000_0000
            } else {
                1
            },
            name: [0; 256],
        })
    };
    true
}
unsafe extern "C" fn latency(plugin: *const clap_plugin) -> u32 {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 8, false, true) };
    match owner.mode {
        Mode::RestartLatency => unsafe { (*owner.host).request_restart.unwrap()(owner.host) },
        Mode::ActiveAllLatency => unsafe { rescan(owner, 1) },
        Mode::ParameterRescan | Mode::ParameterFlush => {
            let params: *const clap_host_params = unsafe { host_extension(owner, CLAP_EXT_PARAMS) };
            if !params.is_null() {
                if owner.mode == Mode::ParameterRescan {
                    unsafe {
                        (*params).rescan.unwrap()(owner.host, CLAP_PARAM_RESCAN_VALUES as u32)
                    };
                } else {
                    unsafe { (*params).request_flush.unwrap()(owner.host) };
                }
            }
        }
        _ => {}
    }
    if owner.notes.get() {
        31 + u32::from(owner.offline.get()) + u32::from(owner.restored.get())
    } else {
        17
    }
}
unsafe extern "C" fn tail(plugin: *const clap_plugin) -> u32 {
    let owner = unsafe { owner(plugin) };
    unsafe { role(owner, 9, true, true) };
    match owner.mode {
        Mode::CallbackTail => unsafe { (*owner.host).request_callback.unwrap()(owner.host) },
        Mode::UnknownTail => unsafe { rescan(owner, 0x8000_0002) },
        Mode::WrongThreadTail => {
            // Only the immutable raw/shared SDK observer runs on the joined
            // foreign thread; this fixture's non-Sync Cell owner stays here.
            let host = owner.host as usize;
            let ports: *const clap_host_note_ports =
                unsafe { host_extension(owner, CLAP_EXT_NOTE_PORTS) };
            let callback = unsafe { (*ports).rescan.unwrap() };
            std::thread::scope(|scope| {
                scope.spawn(move || unsafe { callback(host as *const clap_host, 2) });
            });
        }
        _ => {}
    }
    if owner.notes.get() {
        7 + u32::from(owner.offline.get()) + u32::from(owner.restored.get())
    } else {
        5
    }
}
static STATE: clap_plugin_state = clap_plugin_state { save: None, load: Some(load) };
static RENDER: clap_plugin_render = clap_plugin_render {
    has_hard_realtime_requirement: Some(hard_realtime),
    set: Some(render),
};
static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(parameter_count),
    get_info: Some(parameter_info),
    get_value: Some(parameter_value),
    value_to_text: None,
    text_to_value: None,
    flush: None,
};
static AUDIO: clap_plugin_audio_ports =
    clap_plugin_audio_ports { count: Some(audio_count), get: Some(audio_info) };
static NOTES: clap_plugin_note_ports =
    clap_plugin_note_ports { count: Some(note_count), get: Some(note_info) };
static LATENCY: clap_plugin_latency = clap_plugin_latency { get: Some(latency) };
static TAIL: clap_plugin_tail = clap_plugin_tail { get: Some(tail) };
unsafe extern "C" fn extension(_: *const clap_plugin, id: *const c_char) -> *const c_void {
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_STATE {
        (&STATE as *const clap_plugin_state).cast()
    } else if id == CLAP_EXT_RENDER {
        (&RENDER as *const clap_plugin_render).cast()
    } else if id == CLAP_EXT_PARAMS {
        (&PARAMS as *const clap_plugin_params).cast()
    } else if id == CLAP_EXT_AUDIO_PORTS {
        (&AUDIO as *const clap_plugin_audio_ports).cast()
    } else if id == CLAP_EXT_NOTE_PORTS {
        (&NOTES as *const clap_plugin_note_ports).cast()
    } else if id == CLAP_EXT_LATENCY {
        (&LATENCY as *const clap_plugin_latency).cast()
    } else if id == CLAP_EXT_TAIL {
        (&TAIL as *const clap_plugin_tail).cast()
    } else {
        ptr::null()
    }
}
static DESCRIPTOR: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: clap_sys::version::CLAP_VERSION,
    id: ID.as_ptr(),
    name: c"Owned coherent Notes offer".as_ptr(),
    vendor: c"Mondrian".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"0".as_ptr(),
    description: c"Metadata ABI controls only".as_ptr(),
    features: ptr::null(),
};
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
    let Some((mode, audit)) = NEXT.with(|next| next.borrow_mut().take()) else {
        return ptr::null();
    };
    audit.creates.set(audit.creates.get() + 1);
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
            on_main_thread: Some(main_callback),
        },
        host,
        check: Cell::new(ptr::null()),
        cached_notes: Cell::new(ptr::null()),
        cached_params: Cell::new(ptr::null()),
        notes: Cell::new(false),
        restored: Cell::new(0),
        offline: Cell::new(false),
        active: Cell::new(false),
        changed: Cell::new(false),
        mode,
        audit,
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
fn render_contract() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: crate::AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: usize::MAX,
        public_output_lookahead_budget_frames: usize::MAX,
        compensation_delay_scratch_budget_bytes: usize::MAX,
    }
}
fn configure(mode: Mode) -> (PluginEntry, Rc<Audit>) {
    let audit = Rc::new(Audit::default());
    NEXT.with(|next| {
        assert!(next.borrow().is_none());
        *next.borrow_mut() = Some((mode, audit.clone()));
    });
    // SAFETY: static original callback tables and entry stay live through the test.
    let entry =
        unsafe { PluginEntry::load_from_raw(&ENTRY, c"/owned/coherent-note-offer") }.unwrap();
    (entry, audit)
}
fn captured(
    mode: Mode,
) -> (
    Result<ClapNoteProcessingOfferV6, AudioProcessorHostError>,
    Rc<Audit>,
) {
    let (entry, audit) = configure(mode);
    (
        probe_loaded_processing_v6(
            &entry,
            ID.to_str().unwrap(),
            [23; 32],
            Some(&[1]),
            render_contract(),
        ),
        audit,
    )
}
fn cleanup(audit: &Audit, activates: u32, deactivates: u32) {
    assert_eq!(audit.creates.get(), 1);
    assert_eq!(audit.destroys.get(), 1);
    assert_eq!(audit.activates.get(), activates);
    assert_eq!(audit.deactivates.get(), deactivates);
    assert_eq!(audit.starts.get(), 0);
    assert_eq!(audit.processes.get(), 0);
    assert_eq!(audit.violations.get(), 0);
}
#[test]
fn actual_abi_v2_captures_one_restored_rendered_notes_environment_and_inactive_revalidation() {
    let (result, audit) = captured(Mode::Good);
    let mut offer = result.unwrap();
    cleanup(&audit, 1, 1);
    assert_eq!(audit.mask.get(), 1);
    assert_eq!(offer.schema_version, 6);
    assert!(offer.input_names.is_empty());
    // Public consumers access existing domain facts without private wire access.
    assert_eq!(offer.parameters()[0].current_value, 1.5);
    assert_eq!(offer.parameters()[0].max_value, 2.0);
    assert_eq!(
        offer.auxiliary_inputs().collect::<Vec<_>>(),
        [("clap.audio-input.33", AudioChannelLayout::Stereo)]
    );
    assert_eq!(offer.latency_frames(), 33);
    assert_eq!(offer.tail(), crate::AudioProcessorTail::Finite(9));
    assert_eq!(offer.processing_mode(), crate::AudioProcessingMode::Offline);
    offer.notes.offline = false;
    assert_eq!(
        offer.processing_mode(),
        crate::AudioProcessingMode::Offline,
        "public identity metadata cannot change captured processing facts"
    );
    offer.notes.offline = true;
    assert_eq!(
        offer.notes.state_sha256,
        super::super::clap_note_input_metadata::state_digest(Some(&[1]))
    );
    assert_eq!(offer.notes.binary_sha256, [23; 32]);
    assert_eq!(offer.notes.contract.limits, offered_limits());
    assert_eq!(
        offer
            .notes
            .contract
            .ports
            .iter()
            .map(|port| (port.native_index, port.native_id, port.port_key.as_str()))
            .collect::<Vec<_>>(),
        [
            (0, 0x8000_0017, "clap.note-input.2147483671"),
            (1, 0, "clap.note-input.0")
        ]
    );
    let value = serde_json::to_value(offer).unwrap();
    let processing = &value["processing"];
    assert_eq!(processing["latency_frames"], 33);
    assert_eq!(processing["tail"]["Finite"], 9);
    assert_eq!(processing["parameters"][0]["max_value"], 2.0);
    assert_eq!(processing["parameters"][0]["current_value"], 1.5);
    assert_eq!(
        processing["auxiliary_buses"][0]["bus_key"],
        "clap.audio-input.33"
    );
    let stages = audit.stages();
    let position = |stage| stages.iter().position(|value| *value == stage).unwrap();
    for (a, b) in [
        (1, 2),
        (2, 3),
        (3, 4),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 8),
        (8, 9),
        (9, 10),
        (10, 11),
    ] {
        assert!(position(a) < position(b));
    }
    let deactivate = position(10);
    assert!(stages[deactivate + 1..].contains(&4));
    assert!(stages[deactivate + 1..].contains(&5));
    assert!(stages[deactivate + 1..].contains(&6));
}
#[test]
fn actual_abi_ordinary_probe_and_v1_remain_distinct_without_borrowing_an_ordinary_offer() {
    let (entry, audit) = configure(Mode::Good);
    let ordinary = super::super::clap_discovery::probe_loaded(
        &entry,
        ID.to_str().unwrap(),
        render_contract(),
        Some(&[1]),
        false,
    )
    .unwrap();
    cleanup(&audit, 1, 1);
    assert_eq!(audit.mask.get(), 0);
    let value = serde_json::to_value(ordinary).unwrap();
    assert_eq!(value["latency_frames"], 17);
    assert_eq!(value["tail"]["Finite"], 5);
    assert_eq!(value["parameters"][0]["max_value"], 1.0);
    assert_eq!(value["parameters"][0]["current_value"], 0.25);
    assert_eq!(value["auxiliary_buses"], serde_json::json!([]));
    let (entry, audit) = configure(Mode::Good);
    let notes = probe_loaded(
        &entry,
        ID.to_str().unwrap(),
        [23; 32],
        Some(&[1]),
        render_contract(),
    )
    .unwrap();
    cleanup(&audit, 0, 0);
    assert_eq!(audit.mask.get(), 1);
    assert_eq!(notes.schema_version, 2);
    assert_eq!(notes.contract.ports[0].native_id, 0x8000_0017);
}
#[test]
fn actual_abi_v2_early_failures_destroy_inactive_once_without_activation_or_dsp() {
    for mode in [
        Mode::LoadFailure,
        Mode::RenderFailure,
        Mode::BadParameter,
        Mode::BadAudio,
        Mode::BadNote,
    ] {
        let (result, audit) = captured(mode);
        assert!(result.is_err(), "{mode:?}");
        cleanup(&audit, 0, 0);
    }
    let (result, audit) = captured(Mode::ActivateFailure);
    assert!(result.is_err());
    cleanup(&audit, 1, 0);
}
#[test]
fn actual_abi_v2_unresolved_or_changed_sources_refuse_and_cleanup_once() {
    for mode in [
        Mode::RestartLatency,
        Mode::CallbackTail,
        Mode::ActiveAllLatency,
        Mode::UnknownTail,
        Mode::WrongThreadTail,
        Mode::ChangedParameter,
        Mode::ChangedAudio,
        Mode::ChangedNote,
        Mode::DestroyRestart,
    ] {
        let (result, audit) = captured(mode);
        assert!(result.is_err(), "{mode:?}");
        cleanup(&audit, 1, 1);
        if mode == Mode::CallbackTail {
            assert!(audit.main_callbacks.get() >= 16);
        }
    }
}
#[cfg(windows)]
#[test]
fn actual_abi_v2_refuses_existing_parameter_rescan_and_flush_sources() {
    for mode in [Mode::ParameterRescan, Mode::ParameterFlush] {
        let (result, audit) = captured(mode);
        assert!(result.is_err(), "{mode:?}");
        cleanup(&audit, 1, 1);
    }
}

#[test]
fn v2_readonly_guard_preserves_existing_parameter_publication_and_pending_source() {
    for (all, author, open) in [(1, 0, 0), (1, 1, 0), (0, 0, 1)] {
        let shared = ClapHostShared::new_with_note_input();
        shared.parameter_source.publish_output_count(all, author, open);
        let before = shared.parameter_source.snapshot();
        assert!(check_processing_control(&shared).is_err());
        assert_eq!(shared.parameter_source.snapshot(), before);
    }
    let shared = ClapHostShared::new_with_note_input();
    let publication = shared.parameter_source.begin_output().unwrap();
    let before = shared.parameter_source.snapshot();
    assert!(before.publishing_output);
    assert!(check_processing_control(&shared).is_err());
    assert_eq!(shared.parameter_source.snapshot(), before);
    drop(publication);
    shared
        .parameter_source
        .refuse(super::super::clap_parameter_source::ClapParameterSourceFailure::UnknownFlags);
    let before = shared.parameter_source.snapshot();
    assert_ne!(before.incomplete, 0);
    assert!(check_processing_control(&shared).is_err());
    assert_eq!(shared.parameter_source.snapshot(), before);
}

#[test]
fn actual_abi_v2_destroy_cached_note_observations_refuse_after_native_release() {
    for mode in [
        Mode::DestroyUnknownNote,
        Mode::DestroyNames,
        Mode::DestroyWrongThread,
    ] {
        let (result, audit) = captured(mode);
        assert!(
            result.is_err(),
            "{mode:?}: no destroyed callback evidence may disappear"
        );
        cleanup(&audit, 1, 1);
        assert_eq!(audit.stages().last(), Some(&11));
    }
}

#[cfg(windows)]
#[test]
fn actual_abi_v2_destroy_cached_legal_main_flush_refuses_after_native_release() {
    let (result, audit) = captured(Mode::DestroyFlush);
    assert!(
        result.is_err(),
        "legal main/inactive flush is still unserviced after destroy"
    );
    cleanup(&audit, 1, 1);
    assert_eq!(audit.stages().last(), Some(&11));
}

#[test]
fn actual_abi_v3_zero_audio_inputs_keep_same_instance_notes_and_inactive_audio_revalidation() {
    let (result, audit) = captured(Mode::ZeroAudioInput);
    let offer = result.unwrap();
    cleanup(&audit, 1, 1);
    assert_eq!(offer.schema_version, 6);
    assert!(offer.input_names.is_empty());
    assert_eq!(
        offer.main_audio_input(),
        super::super::ClapMainAudioInput::Absent
    );
    assert_eq!(offer.auxiliary_inputs().len(), 0);
    assert_eq!(offer.notes.contract.ports.len(), 2);
    assert_eq!(offer.parameters()[0].current_value, 1.5);
    assert_eq!(offer.latency_frames(), 33);
    assert_eq!(offer.tail(), crate::AudioProcessorTail::Finite(9));
    let stages = audit.stages();
    let deactivate = stages.iter().position(|stage| *stage == 10).unwrap();
    assert!(stages[deactivate + 1..].contains(&5));
    let empty_aux = crate::AudioProcessorAuxiliaryInputContract::default();
    for (admitted, observed) in [
        (
            super::super::ClapMainAudioInput::Absent,
            super::super::ClapMainAudioInput::Present,
        ),
        (
            super::super::ClapMainAudioInput::Present,
            super::super::ClapMainAudioInput::Absent,
        ),
    ] {
        let processing = super::super::clap_discovery::processing_result_for_notes(
            "fixture",
            render_contract(),
            Vec::new(),
            &super::super::clap_discovery::ClapAudioPortMetadata {
                main_audio_input: admitted,
                contract: empty_aux.clone(),
                names: Vec::new(),
                main_audio_output_id: 0,
                outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            },
            0,
            clack_extensions::tail::TailLength::Finite(0),
        )
        .unwrap();
        let audio = super::super::clap_discovery::ClapAudioPortMetadata {
            main_audio_input: observed,
            contract: empty_aux.clone(),
            names: Vec::new(),
            main_audio_output_id: 0,
            outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
        };
        assert!(
            super::super::clap_discovery::revalidate_processing_metadata_for_notes(
                &processing,
                &[],
                &audio
            )
            .is_err()
        );
    }
}

#[test]
fn actual_abi_v6_names_round_trip_from_the_same_restored_inactive_snapshot() {
    let (result, audit) = captured(Mode::NamedAuxiliary);
    let offer = result.unwrap();
    cleanup(&audit, 1, 1);
    let expected = vec![crate::AudioProcessorInputName {
        bus_key: "clap.audio-input.33".to_owned(),
        name: "Restored offline aux".to_owned(),
    }];
    assert_eq!(offer.input_names, expected);
    assert_eq!(
        offer.notes.state_sha256,
        super::super::clap_note_input_metadata::state_digest(Some(&[1]))
    );
    assert_eq!(offer.notes.binary_sha256, [23; 32]);
    assert_eq!(
        offer.auxiliary_inputs().collect::<Vec<_>>(),
        [("clap.audio-input.33", AudioChannelLayout::Stereo)]
    );
    // Only text changes at deactivate. Keep the initial name, like NormalProbe4;
    // do not strengthen shape revalidation into cross-activation name identity.
    let bytes = serde_json::to_vec(&offer).unwrap();
    let restored: ClapNoteProcessingOfferV6 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(restored.input_names, expected);
    assert_eq!(restored.notes, offer.notes);
    assert_eq!(
        restored.auxiliary_inputs().collect::<Vec<_>>(),
        offer.auxiliary_inputs().collect::<Vec<_>>()
    );
}
