//! Unexecuted ABI control source: real exported tables, callbacks and Box Drop.
//! The small fake host supplies only thread-check and native buffer/event lists;
//! it defines no Mondrian scheduler, Session, author model or note lowering.

use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

pub(super) static DROPPED: AtomicUsize = AtomicUsize::new(0);
// Test-driver serialization only, never reached by plugin process/reset/flush.
static SERIAL: Mutex<()> = Mutex::new(());

struct HostContext {
    main_thread: std::thread::ThreadId,
    audio: AtomicBool,
    expose_thread_check: bool,
}

unsafe fn context<'a>(host: *const clap_host) -> &'a HostContext {
    unsafe { &*((*host).host_data.cast::<HostContext>()) }
}
unsafe extern "C" fn is_main(host: *const clap_host) -> bool {
    std::thread::current().id() == unsafe { context(host) }.main_thread
}
unsafe extern "C" fn is_audio(host: *const clap_host) -> bool {
    unsafe { context(host) }.audio.load(Ordering::Acquire)
}
static CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(is_main),
    is_audio_thread: Some(is_audio),
};
unsafe extern "C" fn host_extension(host: *const clap_host, id: *const c_char) -> *const c_void {
    if !id.is_null()
        && unsafe { CStr::from_ptr(id) } == CLAP_EXT_THREAD_CHECK
        && unsafe { context(host) }.expose_thread_check
    {
        (&CHECK as *const clap_host_thread_check).cast()
    } else {
        ptr::null()
    }
}

fn test_host(context: &HostContext) -> clap_host {
    clap_host {
        clap_version: clap_sys::version::CLAP_VERSION,
        host_data: (context as *const HostContext).cast_mut().cast(),
        name: c"owned ABI control".as_ptr(),
        vendor: c"Mondrian".as_ptr(),
        url: c"".as_ptr(),
        version: c"0.0.0".as_ptr(),
        get_extension: Some(host_extension),
        request_restart: None,
        request_process: None,
        request_callback: None,
    }
}

// Raw exposed-provenance addresses cross only the joined factory thread. The
// host/context stay on the caller's live stack through init/destroy, and the
// official thread-safe factory constructs the actual owned Box. No plugin/DSP
// reference is concurrently shared. All later callbacks run on actual main.
fn create_elsewhere(host: &clap_host) -> *const clap_plugin {
    let host_address = (host as *const clap_host).expose_provenance();
    let (plugin_address, factory_thread) = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let host = ptr::with_exposed_provenance::<clap_host>(host_address);
                let plugin = unsafe {
                    FACTORY.create_plugin.expect("real factory create")(
                        &FACTORY,
                        host,
                        c"org.mondrian.fixture.note-gate".as_ptr(),
                    )
                };
                assert!(!plugin.is_null());
                (plugin.expose_provenance(), std::thread::current().id())
            })
            .join()
            .expect("joined factory")
    });
    assert_ne!(factory_thread, std::thread::current().id());
    ptr::with_exposed_provenance(plugin_address)
}

#[test]
fn factory_other_thread_then_main_init_and_destroy_reclaims_actual_box() {
    let _serial = SERIAL.lock().expect("ABI controls serialized");
    let context = HostContext {
        main_thread: std::thread::current().id(),
        audio: AtomicBool::new(false),
        expose_thread_check: true,
    };
    let host = test_host(&context);
    let before = DROPPED.load(Ordering::Acquire);
    let plugin = create_elsewhere(&host);
    assert!(unsafe { (*plugin).init.expect("real init")(plugin) });
    unsafe { (*plugin).destroy.expect("real destroy")(plugin) };
    assert_eq!(DROPPED.load(Ordering::Acquire), before + 1);
}

#[test]
fn failed_main_init_without_optional_thread_check_still_reclaims_actual_box() {
    let _serial = SERIAL.lock().expect("ABI controls serialized");
    let context = HostContext {
        main_thread: std::thread::current().id(),
        audio: AtomicBool::new(false),
        expose_thread_check: false,
    };
    let host = test_host(&context);
    let before = DROPPED.load(Ordering::Acquire);
    let plugin = create_elsewhere(&host);
    assert!(!unsafe { (*plugin).init.expect("real init")(plugin) });
    unsafe { (*plugin).destroy.expect("real destroy after failed init")(plugin) };
    assert_eq!(DROPPED.load(Ordering::Acquire), before + 1);
}

#[test]
fn main_with_audio_role_still_admits_inactive_main_lifecycle() {
    let _serial = SERIAL.lock().expect("ABI controls serialized");
    let context = HostContext {
        main_thread: std::thread::current().id(),
        audio: AtomicBool::new(true),
        expose_thread_check: true,
    };
    let host = test_host(&context);
    let before = DROPPED.load(Ordering::Acquire);
    let plugin = create_elsewhere(&host);
    assert!(unsafe { (*plugin).init.expect("real init")(plugin) });
    let notes = unsafe {
        (*plugin).get_extension.expect("real extension")(plugin, CLAP_EXT_NOTE_PORTS.as_ptr())
    }
    .cast::<clap_plugin_note_ports>();
    assert!(!notes.is_null());
    assert_eq!(
        unsafe { (*notes).count.expect("real note count")(plugin, true) },
        2
    );
    assert!(unsafe { (*plugin).activate.expect("real activate")(plugin, 48_000.0, 1, 6) });
    // No process/reset overlaps these real main lifecycle calls. Reporting both
    // roles is permitted; it does not manufacture concurrent DSP ownership.
    unsafe { (*plugin).deactivate.expect("real deactivate")(plugin) };
    unsafe { (*plugin).destroy.expect("real destroy")(plugin) };
    assert_eq!(DROPPED.load(Ordering::Acquire), before + 1);
}

struct EventList {
    pointers: [*const clap_event_header; 2],
}
unsafe extern "C" fn input_size(_: *const clap_input_events) -> u32 {
    2
}
unsafe extern "C" fn input_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    let list = unsafe { &*((*list).ctx.cast::<EventList>()) };
    list.pointers.get(index as usize).copied().unwrap_or(ptr::null())
}
unsafe extern "C" fn unexpected_output(
    _: *const clap_output_events,
    _: *const clap_event_header,
) -> bool {
    false // Held note produces no End yet; any callback makes process fail.
}

fn run_constant_native_case(in_place: bool) -> [f32; 6] {
    let context = HostContext {
        main_thread: std::thread::current().id(),
        audio: AtomicBool::new(false),
        expose_thread_check: true,
    };
    let host = test_host(&context);
    let plugin = unsafe {
        FACTORY.create_plugin.expect("real create")(
            &FACTORY,
            &host,
            c"org.mondrian.fixture.note-gate".as_ptr(),
        )
    };
    assert!(!plugin.is_null());
    assert!(unsafe { (*plugin).init.expect("real init")(plugin) });
    let extension = unsafe {
        (*plugin).get_extension.expect("real extension")(plugin, CLAP_EXT_AUDIO_PORTS.as_ptr())
    }
    .cast::<clap_plugin_audio_ports>();
    assert!(!extension.is_null());
    let mut info = clap_audio_port_info {
        id: 0,
        name: [0; 256],
        flags: 0,
        channel_count: 0,
        port_type: ptr::null(),
        in_place_pair: CLAP_INVALID_ID,
    };
    assert!(unsafe { (*extension).get.expect("real input metadata")(plugin, 0, true, &mut info) });
    assert_eq!(info.id, INPUT_ID);
    assert_eq!(info.channel_count, 1);
    assert_eq!(info.in_place_pair, OUTPUT_ID);
    assert!(unsafe {
        (*extension).get.expect("real output metadata")(plugin, 0, false, &mut info)
    });
    assert_eq!(info.id, OUTPUT_ID);
    assert_eq!(info.in_place_pair, INPUT_ID);
    assert!(unsafe { (*plugin).activate.expect("real activate")(plugin, 48_000.0, 1, 6) });

    let on = clap_event_note {
        header: clap_event_header {
            size: size_of::<clap_event_note>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_NOTE_ON as u16,
            flags: 0,
        },
        port_index: 0,
        channel: 0,
        key: 60,
        note_id: 10,
        velocity: 0.0,
    };
    let gain = clap_event_param_value {
        header: clap_event_header {
            size: size_of::<clap_event_param_value>() as u32,
            time: 2,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_PARAM_VALUE as u16,
            flags: 0,
        },
        param_id: GAIN,
        cookie: ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value: 0.5,
    };
    let mut event_list = EventList { pointers: [&on.header, &gain.header] };
    let events = clap_input_events {
        ctx: (&mut event_list as *mut EventList).cast(),
        size: Some(input_size),
        get: Some(input_get),
    };
    let output_events = clap_output_events {
        ctx: ptr::null_mut(),
        try_push: Some(unexpected_output),
    };
    let mut input = [2.0_f32; 6];
    let mut output = [f32::NAN; 6];
    let mut input_channels = [input.as_mut_ptr()];
    let mut output_channels = [if in_place {
        input.as_mut_ptr()
    } else {
        output.as_mut_ptr()
    }];
    let input_buffer = clap_audio_buffer {
        data32: input_channels.as_mut_ptr(),
        data64: ptr::null_mut(),
        channel_count: 1,
        latency: 0,
        constant_mask: 1,
    };
    let mut output_buffer = clap_audio_buffer {
        data32: output_channels.as_mut_ptr(),
        data64: ptr::null_mut(),
        channel_count: 1,
        latency: 0,
        constant_mask: 1,
    };
    let block = clap_process {
        steady_time: -1,
        frames_count: 6,
        transport: ptr::null(),
        audio_inputs: &input_buffer,
        audio_outputs: &mut output_buffer,
        audio_inputs_count: 1,
        audio_outputs_count: 1,
        in_events: &events,
        out_events: &output_events,
    };
    context.audio.store(true, Ordering::Release);
    assert!(unsafe { (*plugin).start_processing.expect("real start")(plugin) });
    assert_eq!(
        unsafe { (*plugin).process.expect("real process")(plugin, &block) },
        CLAP_PROCESS_CONTINUE
    );
    unsafe { (*plugin).stop_processing.expect("real stop")(plugin) };
    context.audio.store(false, Ordering::Release);
    assert_eq!(output_buffer.constant_mask, 0);
    unsafe { (*plugin).deactivate.expect("real deactivate")(plugin) };
    unsafe { (*plugin).destroy.expect("real destroy")(plugin) };
    if in_place {
        input
    } else {
        output
    }
}

#[test]
fn constant_in_place_uses_original_scalar_and_matches_disjoint_actual_callbacks() {
    let _serial = SERIAL.lock().expect("ABI controls serialized");
    let before = DROPPED.load(Ordering::Acquire);
    // Independent dyadic oracle: x=2, held port0 zero-velocity On => w=1/32;
    // gain=1 for samples0..1, gain=1/2 from sample2. No host/helper expression
    // supplies the expected native PCM, and real process runs both buffer forms.
    let expected: [f32; 6] = [
        1.0 / 16.0,
        1.0 / 16.0,
        1.0 / 32.0,
        1.0 / 32.0,
        1.0 / 32.0,
        1.0 / 32.0,
    ];
    assert_eq!(run_constant_native_case(true), expected);
    assert_eq!(run_constant_native_case(false), expected);
    assert_eq!(DROPPED.load(Ordering::Acquire), before + 2);
}
