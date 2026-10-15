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

// Own only the real native lifecycle. It supplies no voice/note/PCM model and
// unwinds stop/deactivate/destroy while the actual borrowed host stays alive.
struct LivePlugin<'a> {
    plugin: *const clap_plugin,
    context: &'a HostContext,
    _host: &'a clap_host,
}
impl<'a> LivePlugin<'a> {
    fn new(context: &'a HostContext, host: &'a clap_host, id: &CStr) -> Self {
        let plugin = unsafe { FACTORY.create_plugin.unwrap()(&FACTORY, host, id.as_ptr()) };
        assert!(!plugin.is_null());
        let result = Self { plugin, context, _host: host };
        assert!(unsafe { (*plugin).init.unwrap()(plugin) });
        result
    }
    fn start(&self) {
        assert!(unsafe { (*self.plugin).activate.unwrap()(self.plugin, 48_000.0, 1, 6) });
        self.context.audio.store(true, Ordering::Release);
        assert!(unsafe { (*self.plugin).start_processing.unwrap()(self.plugin) });
    }
    fn stop(&self) {
        unsafe { (*self.plugin).stop_processing.unwrap()(self.plugin) };
        self.context.audio.store(false, Ordering::Release);
    }
    fn deactivate(&self) {
        // Actual inactive transition, not a host-side role/active annotation.
        unsafe { (*self.plugin).deactivate.unwrap()(self.plugin) };
    }
    fn exact_offs(&self) -> f64 {
        let mut value = f64::NAN;
        assert!(unsafe { parameter_value(self.plugin, EXACT_OFFS, &mut value) });
        value
    }
}
impl Drop for LivePlugin<'_> {
    fn drop(&mut self) {
        // The native Instance is still live here. These facts only choose safe
        // lifecycle cleanup; no expected PCM or pairing result is read from it.
        let owner = unsafe { owner(self.plugin) }.unwrap();
        if owner.processing.load(Ordering::Acquire) {
            self.context.audio.store(true, Ordering::Release);
            unsafe { (*self.plugin).stop_processing.unwrap()(self.plugin) };
        }
        self.context.audio.store(false, Ordering::Release);
        if owner.active.load(Ordering::Acquire) {
            unsafe { (*self.plugin).deactivate.unwrap()(self.plugin) };
        }
        unsafe { (*self.plugin).destroy.unwrap()(self.plugin) };
    }
}
fn native_context() -> HostContext {
    HostContext {
        main_thread: std::thread::current().id(),
        audio: AtomicBool::new(false),
        expose_thread_check: true,
    }
}
struct NativeInputs<'a> {
    pointers: &'a [*const clap_event_header],
}
unsafe extern "C" fn native_size(list: *const clap_input_events) -> u32 {
    unsafe { &*((*list).ctx.cast::<NativeInputs<'_>>()) }.pointers.len() as u32
}
unsafe extern "C" fn native_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    unsafe { &*((*list).ctx.cast::<NativeInputs<'_>>()) }
        .pointers
        .get(index as usize)
        .copied()
        .unwrap_or(ptr::null())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeOutput {
    Gain { time: u32, bits: u64 },
    End { time: u32, address: Address },
}
struct NativeOutputs {
    records: Vec<NativeOutput>,
}
unsafe extern "C" fn capture_native(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    if event.is_null() {
        return false;
    }
    let output = unsafe { &mut *((*list).ctx.cast::<NativeOutputs>()) };
    if output.records.len() == output.records.capacity() {
        return false; // Bounded preallocation; never allocate in this callback.
    }
    let header = unsafe { event.read_unaligned() };
    if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
        return false;
    }
    let record = if header.type_ == CLAP_EVENT_PARAM_VALUE as u16
        && header.size as usize == size_of::<clap_event_param_value>()
    {
        let event = unsafe { event.cast::<clap_event_param_value>().read_unaligned() };
        if event.param_id != GAIN || !event.value.is_finite() {
            return false;
        }
        NativeOutput::Gain { time: header.time, bits: event.value.to_bits() }
    } else if header.type_ == CLAP_EVENT_NOTE_END as u16
        && header.size as usize == size_of::<clap_event_note>()
    {
        let event = unsafe { event.cast::<clap_event_note>().read_unaligned() };
        NativeOutput::End {
            time: header.time,
            address: Address {
                port: event.port_index,
                channel: event.channel,
                key: event.key,
                id: event.note_id,
            },
        }
    } else {
        return false;
    };
    output.records.push(record);
    true
}
fn note(kind: u16, time: u32, port: i16) -> clap_event_note {
    clap_event_note {
        header: clap_event_header {
            size: size_of::<clap_event_note>() as u32,
            time,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: kind,
            flags: 0,
        },
        note_id: 10,
        port_index: port,
        channel: 0,
        key: 60,
        velocity: 0.0,
    }
}
fn parameter(id: u32, time: u32, value: f64) -> clap_event_param_value {
    clap_event_param_value {
        header: clap_event_header {
            size: size_of::<clap_event_param_value>() as u32,
            time,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_PARAM_VALUE as u16,
            flags: 0,
        },
        param_id: id,
        cookie: ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value,
    }
}
#[derive(Debug, Clone, Copy)]
enum Shape {
    Valid,
    ExtraInput,
    NoOutput,
    TwoOutputs,
    NullOutputArray,
    StereoOutput,
    NullOutputData,
    NullOutputChannel,
    DoubleOutput,
    ZeroFrames,
    ExcessFrames,
}
fn call_zero(
    plugin: &LivePlugin<'_>,
    pointers: &[*const clap_event_header],
    output: &mut [f32],
    nonnull_empty_input: bool,
    shape: Shape,
) -> (clap_process_status, u64, Vec<NativeOutput>) {
    let mut input_events = NativeInputs { pointers };
    let input_events = clap_input_events {
        ctx: (&mut input_events as *mut NativeInputs<'_>).cast(),
        size: Some(native_size),
        get: Some(native_get),
    };
    let mut capture = NativeOutputs { records: Vec::with_capacity(16) };
    let output_events = clap_output_events {
        ctx: (&mut capture as *mut NativeOutputs).cast(),
        try_push: Some(capture_native),
    };
    let mut channels = [if matches!(shape, Shape::NullOutputChannel) {
        ptr::null_mut()
    } else {
        output.as_mut_ptr()
    }];
    let mut buffer = clap_audio_buffer {
        data32: channels.as_mut_ptr(),
        data64: ptr::null_mut(),
        channel_count: 1,
        latency: 0,
        constant_mask: 1,
    };
    let mut block = clap_process {
        steady_time: -1,
        frames_count: output.len() as u32,
        transport: ptr::null(),
        // The nonnull pointer is aligned but has no readable element. Count0
        // makes dereferencing it illegal; a fake hidden input cannot pass.
        audio_inputs: if nonnull_empty_input {
            ptr::NonNull::<clap_audio_buffer>::dangling().as_ptr()
        } else {
            ptr::null()
        },
        audio_outputs: &mut buffer,
        audio_inputs_count: 0,
        audio_outputs_count: 1,
        in_events: &input_events,
        out_events: &output_events,
    };
    match shape {
        Shape::Valid => {}
        Shape::ExtraInput => block.audio_inputs_count = 1,
        Shape::NoOutput => block.audio_outputs_count = 0,
        Shape::TwoOutputs => block.audio_outputs_count = 2,
        Shape::NullOutputArray => block.audio_outputs = ptr::null_mut(),
        Shape::StereoOutput => buffer.channel_count = 2,
        Shape::NullOutputData => buffer.data32 = ptr::null_mut(),
        Shape::NullOutputChannel => {}
        Shape::DoubleOutput => buffer.data64 = ptr::NonNull::<*mut f64>::dangling().as_ptr(),
        Shape::ZeroFrames => block.frames_count = 0,
        Shape::ExcessFrames => block.frames_count = 7,
    }
    let status = unsafe { (*plugin.plugin).process.unwrap()(plugin.plugin, &block) };
    (status, buffer.constant_mask, capture.records)
}
fn check_zero_ports(plugin: &LivePlugin<'_>) {
    let audio = unsafe {
        (*plugin.plugin).get_extension.unwrap()(plugin.plugin, CLAP_EXT_AUDIO_PORTS.as_ptr())
    }
    .cast::<clap_plugin_audio_ports>();
    assert!(!audio.is_null());
    assert_eq!(unsafe { (*audio).count.unwrap()(plugin.plugin, true) }, 0);
    assert_eq!(unsafe { (*audio).count.unwrap()(plugin.plugin, false) }, 1);
    let mut info = clap_audio_port_info {
        id: CLAP_INVALID_ID,
        name: [0; 256],
        flags: 0,
        channel_count: 0,
        port_type: ptr::null(),
        in_place_pair: INPUT_ID,
    };
    assert!(!unsafe { (*audio).get.unwrap()(plugin.plugin, 0, true, &mut info) });
    assert_eq!(
        info.id, CLAP_INVALID_ID,
        "nonexistent input is never written"
    );
    assert!(unsafe { (*audio).get.unwrap()(plugin.plugin, 0, false, &mut info) });
    assert_eq!(info.id, OUTPUT_ID);
    assert_eq!(info.flags, CLAP_AUDIO_PORT_IS_MAIN as u32);
    assert_eq!(info.channel_count, 1);
    assert_eq!(unsafe { CStr::from_ptr(info.port_type) }, CLAP_PORT_MONO);
    assert_eq!(info.in_place_pair, CLAP_INVALID_ID);
}

#[test]
fn zero_input_variant_constant_carrier_literal_pcm_and_note_end_across_partitions() {
    let _serial = SERIAL.lock().unwrap();
    let before = DROPPED.load(Ordering::Acquire);
    assert_eq!(unsafe { FACTORY.get_plugin_count.unwrap()(&FACTORY) }, 3);
    assert_eq!(
        unsafe { FACTORY.get_plugin_descriptor.unwrap()(&FACTORY, 0) },
        &DESCRIPTOR as *const clap_plugin_descriptor
    );
    assert_eq!(
        unsafe { FACTORY.get_plugin_descriptor.unwrap()(&FACTORY, 1) },
        &ZERO_INPUT_DESCRIPTOR as *const clap_plugin_descriptor
    );
    assert_eq!(
        unsafe { FACTORY.get_plugin_descriptor.unwrap()(&FACTORY, 2) },
        &THREE_OUTPUTS_DESCRIPTOR as *const clap_plugin_descriptor
    );
    assert!(unsafe { FACTORY.get_plugin_descriptor.unwrap()(&FACTORY, 3) }.is_null());
    // Independent literals: carrier1, port0 zero-velocity Held weight1/32,
    // gain1/2 and the original Off at2, then four release samples through5.
    let expected = [
        1.0_f32 / 32.0,
        1.0 / 32.0,
        1.0 / 64.0,
        3.0 / 256.0,
        1.0 / 128.0,
        1.0 / 256.0,
    ];
    for nonnull in [false, true] {
        for partition in [&[6_usize][..], &[2, 4][..], &[1, 2, 3][..]] {
            let context = native_context();
            let host = test_host(&context);
            let plugin = LivePlugin::new(
                &context,
                &host,
                c"org.mondrian.fixture.note-gate.zero-input",
            );
            check_zero_ports(&plugin);
            plugin.start();
            let mut result = Vec::new();
            let mut recorded = Vec::new();
            let mut start = 0;
            for &frames in partition {
                let on = note(CLAP_EVENT_NOTE_ON as u16, 0, 0);
                let gain = parameter(GAIN, 2_u32.saturating_sub(start), 0.5);
                let off = note(CLAP_EVENT_NOTE_OFF as u16, 2_u32.saturating_sub(start), 0);
                let mut events = Vec::new();
                if start == 0 {
                    events.push(&on.header as *const clap_event_header);
                }
                if start <= 2 && start + frames as u32 > 2 {
                    events.push(&gain.header);
                    events.push(&off.header);
                }
                let mut output = vec![99.0_f32; frames];
                let (status, mask, records) =
                    call_zero(&plugin, &events, &mut output, nonnull, Shape::Valid);
                assert_eq!(status, CLAP_PROCESS_CONTINUE);
                assert_eq!(mask, 0);
                for record in records {
                    recorded.push(match record {
                        NativeOutput::Gain { time, bits } => {
                            NativeOutput::Gain { time: time + start, bits }
                        }
                        end => end,
                    });
                }
                result.extend(output);
                start += frames as u32;
            }
            assert_eq!(result, expected);
            assert_eq!(
                recorded,
                vec![
                    NativeOutput::Gain { time: 5, bits: 0.5_f64.to_bits() },
                    NativeOutput::End {
                        time: u32::MAX,
                        address: Address { port: 0, channel: 0, key: 60, id: 10 }
                    },
                    NativeOutput::Gain { time: 5, bits: 0.5_f64.to_bits() },
                ]
            );
            plugin.stop();
            assert_eq!(plugin.exact_offs(), 1.0);
            plugin.deactivate();
            check_zero_ports(&plugin);
        }
    }
    assert_eq!(DROPPED.load(Ordering::Acquire), before + 6);
}

#[test]
fn zero_input_early_end_retains_original_off_pairing_and_rejects_changed_tuple() {
    let _serial = SERIAL.lock().unwrap();
    let before = DROPPED.load(Ordering::Acquire);
    for wrong in [false, true] {
        let context = native_context();
        let host = test_host(&context);
        let plugin = LivePlugin::new(
            &context,
            &host,
            c"org.mondrian.fixture.note-gate.zero-input",
        );
        plugin.start();
        let scenario = parameter(SCENARIO, 0, 1.0);
        let on = note(CLAP_EVENT_NOTE_ON as u16, 0, 0);
        let off = note(CLAP_EVENT_NOTE_OFF as u16, 4, if wrong { 1 } else { 0 });
        let pointers = [
            &scenario.header as *const clap_event_header,
            &on.header,
            &off.header,
        ];
        let mut output = [99.0_f32; 6];
        let (status, _, records) = call_zero(&plugin, &pointers, &mut output, false, Shape::Valid);
        assert_eq!(
            records,
            vec![
                NativeOutput::Gain { time: 1, bits: 1.0_f64.to_bits() },
                NativeOutput::End {
                    time: u32::MAX,
                    address: Address { port: 0, channel: 0, key: 60, id: 10 }
                },
                NativeOutput::Gain { time: 1, bits: 1.0_f64.to_bits() },
            ]
        );
        if wrong {
            assert_eq!(status, CLAP_PROCESS_ERROR);
            assert_eq!(output, [1.0 / 32.0, 1.0 / 32.0, 0.0, 0.0, 99.0, 99.0]);
        } else {
            assert_eq!(status, CLAP_PROCESS_CONTINUE);
            assert_eq!(output, [1.0 / 32.0, 1.0 / 32.0, 0.0, 0.0, 0.0, 0.0]);
        }
        plugin.stop();
        assert_eq!(plugin.exact_offs(), if wrong { 0.0 } else { 1.0 });
    }
    assert_eq!(DROPPED.load(Ordering::Acquire), before + 2);
}

#[test]
fn zero_input_rejects_wrong_native_shape_extent_and_event_before_pcm_or_off_credit() {
    let _serial = SERIAL.lock().unwrap();
    let before = DROPPED.load(Ordering::Acquire);
    let shapes = [
        Shape::ExtraInput,
        Shape::NoOutput,
        Shape::TwoOutputs,
        Shape::NullOutputArray,
        Shape::StereoOutput,
        Shape::NullOutputData,
        Shape::NullOutputChannel,
        Shape::DoubleOutput,
        Shape::ZeroFrames,
        Shape::ExcessFrames,
        Shape::Valid,
    ];
    for shape in shapes {
        let context = native_context();
        let host = test_host(&context);
        let plugin = LivePlugin::new(
            &context,
            &host,
            c"org.mondrian.fixture.note-gate.zero-input",
        );
        plugin.start();
        let mut on = note(CLAP_EVENT_NOTE_ON as u16, 0, 0);
        if matches!(shape, Shape::Valid) {
            on.header.size = 0;
        }
        let off = note(CLAP_EVENT_NOTE_OFF as u16, 2, 0);
        let pointers = [&on.header as *const clap_event_header, &off.header];
        let mut output = [99.0_f32; 6];
        let (status, mask, records) = call_zero(&plugin, &pointers, &mut output, true, shape);
        assert_eq!(status, CLAP_PROCESS_ERROR, "shape={shape:?}");
        assert_eq!(output, [99.0; 6], "shape={shape:?}");
        assert_eq!(mask, 1);
        assert!(records.is_empty());
        plugin.stop();
        assert_eq!(plugin.exact_offs(), 0.0);
    }
    assert_eq!(DROPPED.load(Ordering::Acquire), before + shapes.len());
}

// The new ID shares the original Gate and event sink, not a second oracle.
// Expected PCM below is independently written as literal dyadic sequences.
#[derive(Debug, Clone, Copy)]
enum ThreeShape {
    Valid,
    ExtraInput,
    NoOutput,
    OneOutput,
    FourOutputs,
    NullArray,
    LastStereo,
    LastNullData,
    LastNullChannel,
    LastDoubleData,
    ExactAlias,
    PartialAlias,
    Misaligned,
    ZeroFrames,
    ExcessFrames,
}

unsafe extern "C" fn reject_native_end(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    if event.is_null() || unsafe { event.read_unaligned() }.type_ == CLAP_EVENT_NOTE_END as u16 {
        return false;
    }
    unsafe { capture_native(list, event) }
}

fn call_three(
    plugin: &LivePlugin<'_>,
    pointers: &[*const clap_event_header],
    output: &mut [[f32; 7]; 3],
    frames: u32,
    nonnull_empty_input: bool,
    shape: ThreeShape,
    refuse_end: bool,
) -> (clap_process_status, [u64; 3], Vec<NativeOutput>) {
    let mut inputs = NativeInputs { pointers };
    let inputs = clap_input_events {
        ctx: (&mut inputs as *mut NativeInputs<'_>).cast(),
        size: Some(native_size),
        get: Some(native_get),
    };
    let mut capture = NativeOutputs { records: Vec::with_capacity(16) };
    let callback: unsafe extern "C" fn(
        *const clap_output_events,
        *const clap_event_header,
    ) -> bool = if refuse_end {
        reject_native_end
    } else {
        capture_native
    };
    let events = clap_output_events {
        ctx: (&mut capture as *mut NativeOutputs).cast(),
        try_push: Some(callback),
    };
    let mut channels = output.each_mut().map(|samples| [samples.as_mut_ptr()]);
    match shape {
        ThreeShape::ExactAlias => channels[2][0] = channels[0][0],
        ThreeShape::PartialAlias => channels[2][0] = unsafe { channels[0][0].add(1) },
        ThreeShape::LastNullChannel => channels[2][0] = ptr::null_mut(),
        ThreeShape::Misaligned => {
            channels[2][0] = unsafe { channels[2][0].cast::<u8>().add(1).cast::<f32>() };
        }
        _ => {}
    }
    let mut buffers = channels.each_mut().map(|channel| clap_audio_buffer {
        data32: channel.as_mut_ptr(),
        data64: ptr::null_mut(),
        channel_count: 1,
        latency: 0,
        constant_mask: 1,
    });
    match shape {
        ThreeShape::LastStereo => buffers[2].channel_count = 2,
        ThreeShape::LastNullData => buffers[2].data32 = ptr::null_mut(),
        ThreeShape::LastDoubleData => {
            buffers[2].data64 = ptr::NonNull::<*mut f64>::dangling().as_ptr();
        }
        _ => {}
    }
    let mut block = clap_process {
        steady_time: -1,
        frames_count: frames,
        transport: ptr::null(),
        // Both zero-count pointer representations must remain unread.
        audio_inputs: if nonnull_empty_input {
            ptr::NonNull::<clap_audio_buffer>::dangling().as_ptr()
        } else {
            ptr::null()
        },
        audio_outputs: buffers.as_mut_ptr(),
        audio_inputs_count: 0,
        audio_outputs_count: 3,
        in_events: &inputs,
        out_events: &events,
    };
    match shape {
        ThreeShape::ExtraInput => block.audio_inputs_count = 1,
        ThreeShape::NoOutput => block.audio_outputs_count = 0,
        ThreeShape::OneOutput => block.audio_outputs_count = 1,
        ThreeShape::FourOutputs => block.audio_outputs_count = 4,
        ThreeShape::NullArray => block.audio_outputs = ptr::null_mut(),
        ThreeShape::ZeroFrames => block.frames_count = 0,
        ThreeShape::ExcessFrames => block.frames_count = 7,
        _ => {}
    }
    let status = unsafe { (*plugin.plugin).process.unwrap()(plugin.plugin, &block) };
    (
        status,
        buffers.map(|port| port.constant_mask),
        capture.records,
    )
}

fn process_calls(plugin: &LivePlugin<'_>) -> f64 {
    let params =
        unsafe { (*plugin.plugin).get_extension.unwrap()(plugin.plugin, CLAP_EXT_PARAMS.as_ptr()) }
            .cast::<clap_plugin_params>();
    assert!(!params.is_null());
    let mut count = f64::NAN;
    assert!(unsafe { (*params).get_value.unwrap()(plugin.plugin, PROCESS_CALLS, &mut count) });
    count
}

fn check_three_ports(plugin: &LivePlugin<'_>) {
    let audio = unsafe {
        (*plugin.plugin).get_extension.unwrap()(plugin.plugin, CLAP_EXT_AUDIO_PORTS.as_ptr())
    }
    .cast::<clap_plugin_audio_ports>();
    assert!(!audio.is_null());
    assert_eq!(unsafe { (*audio).count.unwrap()(plugin.plugin, true) }, 0);
    assert_eq!(unsafe { (*audio).count.unwrap()(plugin.plugin, false) }, 3);
    for index in 0..3 {
        let mut port: clap_audio_port_info = unsafe { std::mem::zeroed() };
        assert!(unsafe { (*audio).get.unwrap()(plugin.plugin, index, false, &mut port) });
        assert_eq!(port.id, index + 1);
        assert_eq!(
            port.flags,
            if index == 0 {
                CLAP_AUDIO_PORT_IS_MAIN as u32
            } else {
                0
            }
        );
        assert_eq!(port.channel_count, 1);
        assert_eq!(unsafe { CStr::from_ptr(port.port_type) }, CLAP_PORT_MONO);
        assert_eq!(port.in_place_pair, CLAP_INVALID_ID);
    }
    let mut untouched: clap_audio_port_info = unsafe { std::mem::zeroed() };
    untouched.id = 123;
    assert!(!unsafe { (*audio).get.unwrap()(plugin.plugin, 0, true, &mut untouched) });
    assert!(!unsafe { (*audio).get.unwrap()(plugin.plugin, 3, false, &mut untouched) });
    assert_eq!(untouched.id, 123);
}

#[test]
fn three_output_zero_input_literal_pcm_single_gate_and_callback_count_across_partitions() {
    let _serial = SERIAL.lock().unwrap();
    let before = DROPPED.load(Ordering::Acquire);
    // Old IDs still expose exactly their original three parameters. The new
    // observation is neither a new old-ID parameter nor a hidden setter.
    for id in [
        c"org.mondrian.fixture.note-gate",
        c"org.mondrian.fixture.note-gate.zero-input",
    ] {
        let context = native_context();
        let host = test_host(&context);
        let plugin = LivePlugin::new(&context, &host, id);
        assert_eq!(unsafe { parameter_count(plugin.plugin) }, 3);
        let mut value = 123.0;
        assert!(!unsafe { parameter_value(plugin.plugin, PROCESS_CALLS, &mut value) });
        assert_eq!(value, 123.0);
        let mut info: clap_param_info = unsafe { std::mem::zeroed() };
        assert!(!unsafe { parameter_info(plugin.plugin, 3, &mut info) });
    }
    let expected = [
        [
            1.0_f32 / 32.0,
            1.0 / 32.0,
            1.0 / 64.0,
            3.0 / 256.0,
            1.0 / 128.0,
            1.0 / 256.0,
        ],
        [
            -1.0_f32 / 64.0,
            -1.0 / 64.0,
            -1.0 / 128.0,
            -3.0 / 512.0,
            -1.0 / 256.0,
            -1.0 / 512.0,
        ],
        [
            1.0_f32 / 128.0,
            1.0 / 128.0,
            1.0 / 256.0,
            3.0 / 1024.0,
            1.0 / 512.0,
            1.0 / 1024.0,
        ],
    ];
    for nonnull in [false, true] {
        for partition in [&[6_usize][..], &[2, 4][..], &[1, 2, 3][..]] {
            let context = native_context();
            let host = test_host(&context);
            let plugin = LivePlugin::new(
                &context,
                &host,
                c"org.mondrian.fixture.note-gate.zero-input.three-outputs",
            );
            check_three_ports(&plugin);
            assert_eq!(unsafe { parameter_count(plugin.plugin) }, 4);
            let mut info: clap_param_info = unsafe { std::mem::zeroed() };
            assert!(unsafe { parameter_info(plugin.plugin, 3, &mut info) });
            assert_eq!(info.id, 29);
            assert_eq!(
                info.flags,
                (CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_STEPPED) as u32
            );
            plugin.start();
            assert_eq!(process_calls(&plugin), 0.0);
            let mut silent = [[99.0_f32; 7]; 3];
            let (status, masks, records) = call_three(
                &plugin,
                &[],
                &mut silent,
                6,
                nonnull,
                ThreeShape::Valid,
                false,
            );
            assert_eq!(status, CLAP_PROCESS_CONTINUE);
            assert_eq!(masks, [0; 3]);
            assert!(records.is_empty());
            for (bus, port) in silent.iter().enumerate() {
                let zero = if bus == 1 { -0.0_f32 } else { 0.0_f32 };
                assert!(port[..6].iter().all(|value| value.to_bits() == zero.to_bits()));
                assert_eq!(port[6], 99.0);
            }
            assert_eq!(process_calls(&plugin), 1.0);
            unsafe { (*plugin.plugin).reset.unwrap()(plugin.plugin) };
            assert_eq!(process_calls(&plugin), 0.0);
            let mut result: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::new());
            let mut recorded = Vec::new();
            let mut start = 0;
            for &frames in partition {
                let on = note(CLAP_EVENT_NOTE_ON as u16, 0, 0);
                let gain = parameter(GAIN, 2_u32.saturating_sub(start), 0.5);
                let off = note(CLAP_EVENT_NOTE_OFF as u16, 2_u32.saturating_sub(start), 0);
                let mut events = Vec::new();
                if start == 0 {
                    events.push(&on.header as *const clap_event_header);
                }
                if start <= 2 && start + frames as u32 > 2 {
                    events.push(&gain.header);
                    events.push(&off.header);
                }
                let mut output = [[99.0_f32; 7]; 3];
                let (status, masks, records) = call_three(
                    &plugin,
                    &events,
                    &mut output,
                    frames as u32,
                    nonnull,
                    ThreeShape::Valid,
                    false,
                );
                assert_eq!(status, CLAP_PROCESS_CONTINUE);
                assert_eq!(masks, [0; 3]);
                for (bus, samples) in output.iter().enumerate() {
                    result[bus].extend_from_slice(&samples[..frames]);
                    assert!(samples[frames..].iter().all(|value| *value == 99.0));
                }
                recorded.extend(records.into_iter().map(|record| match record {
                    NativeOutput::Gain { time, bits } => {
                        NativeOutput::Gain { time: time + start, bits }
                    }
                    end => end,
                }));
                start += frames as u32;
            }
            for (actual, expected) in result.iter().zip(expected) {
                assert_eq!(
                    actual.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
                    expected.map(f32::to_bits)
                );
            }
            assert_eq!(
                recorded,
                vec![
                    NativeOutput::Gain { time: 5, bits: 0.5_f64.to_bits() },
                    NativeOutput::End {
                        time: u32::MAX,
                        address: Address { port: 0, channel: 0, key: 60, id: 10 }
                    },
                    NativeOutput::Gain { time: 5, bits: 0.5_f64.to_bits() },
                ]
            );
            plugin.stop();
            assert_eq!(plugin.exact_offs(), 1.0);
            assert_eq!(process_calls(&plugin), partition.len() as f64);
            plugin.deactivate();
            check_three_ports(&plugin);
        }
    }
    assert_eq!(DROPPED.load(Ordering::Acquire), before + 8);
}

#[test]
fn three_output_zero_input_refuses_all_port_shapes_aliases_and_readonly_writes_before_dsp() {
    let _serial = SERIAL.lock().unwrap();
    let before = DROPPED.load(Ordering::Acquire);
    let shapes = [
        ThreeShape::ExtraInput,
        ThreeShape::NoOutput,
        ThreeShape::OneOutput,
        ThreeShape::FourOutputs,
        ThreeShape::NullArray,
        ThreeShape::LastStereo,
        ThreeShape::LastNullData,
        ThreeShape::LastNullChannel,
        ThreeShape::LastDoubleData,
        ThreeShape::ExactAlias,
        ThreeShape::PartialAlias,
        ThreeShape::Misaligned,
        ThreeShape::ZeroFrames,
        ThreeShape::ExcessFrames,
        ThreeShape::Valid,
    ];
    for shape in shapes {
        let context = native_context();
        let host = test_host(&context);
        let plugin = LivePlugin::new(
            &context,
            &host,
            c"org.mondrian.fixture.note-gate.zero-input.three-outputs",
        );
        plugin.start();
        let on = note(CLAP_EVENT_NOTE_ON as u16, 0, 0);
        let off = note(CLAP_EVENT_NOTE_OFF as u16, 2, 0);
        let readonly = parameter(PROCESS_CALLS, 0, 9.0);
        let events = if matches!(shape, ThreeShape::Valid) {
            vec![
                &readonly.header as *const clap_event_header,
                &on.header,
                &off.header,
            ]
        } else {
            vec![&on.header as *const clap_event_header, &off.header]
        };
        let mut output = [[99.0_f32; 7]; 3];
        let (status, masks, records) =
            call_three(&plugin, &events, &mut output, 6, true, shape, false);
        assert_eq!(status, CLAP_PROCESS_ERROR, "shape={shape:?}");
        assert_eq!(output, [[99.0; 7]; 3], "shape={shape:?}");
        assert_eq!(masks, [1; 3]);
        assert!(records.is_empty());
        plugin.stop();
        assert_eq!(plugin.exact_offs(), 0.0);
        // A refused callback is still one real callback; readonly writing9 did
        // not replace the statistic. No Process invocation is synthesized.
        assert_eq!(process_calls(&plugin), 1.0);
    }
    assert_eq!(DROPPED.load(Ordering::Acquire), before + shapes.len());
}

#[test]
fn three_output_zero_input_retains_original_off_and_propagates_real_end_sink_refusal() {
    let _serial = SERIAL.lock().unwrap();
    let before = DROPPED.load(Ordering::Acquire);
    for (wrong_off, refuse_end) in [(false, false), (true, false), (false, true)] {
        let context = native_context();
        let host = test_host(&context);
        let plugin = LivePlugin::new(
            &context,
            &host,
            c"org.mondrian.fixture.note-gate.zero-input.three-outputs",
        );
        plugin.start();
        let scenario = parameter(SCENARIO, 0, 1.0);
        let on = note(CLAP_EVENT_NOTE_ON as u16, 0, 0);
        let off = note(CLAP_EVENT_NOTE_OFF as u16, 4, if wrong_off { 1 } else { 0 });
        let events = [
            &scenario.header as *const clap_event_header,
            &on.header,
            &off.header,
        ];
        let mut output = [[99.0_f32; 7]; 3];
        let (status, _, records) = call_three(
            &plugin,
            &events,
            &mut output,
            6,
            false,
            ThreeShape::Valid,
            refuse_end,
        );
        let completed = if refuse_end {
            2
        } else if wrong_off {
            4
        } else {
            6
        };
        let expected = [
            [1.0_f32 / 32.0, 1.0 / 32.0, 0.0, 0.0, 0.0, 0.0],
            [-1.0_f32 / 64.0, -1.0 / 64.0, -0.0, -0.0, -0.0, -0.0],
            [1.0_f32 / 128.0, 1.0 / 128.0, 0.0, 0.0, 0.0, 0.0],
        ];
        for (actual, expected) in output.iter().zip(expected) {
            assert_eq!(
                actual[..completed].iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
                expected[..completed].iter().map(|value| value.to_bits()).collect::<Vec<_>>()
            );
            assert!(actual[completed..].iter().all(|value| *value == 99.0));
        }
        if refuse_end {
            assert_eq!(
                records,
                vec![NativeOutput::Gain { time: 1, bits: 1.0_f64.to_bits() }]
            );
        } else {
            assert_eq!(
                records,
                vec![
                    NativeOutput::Gain { time: 1, bits: 1.0_f64.to_bits() },
                    NativeOutput::End {
                        time: u32::MAX,
                        address: Address { port: 0, channel: 0, key: 60, id: 10 }
                    },
                    NativeOutput::Gain { time: 1, bits: 1.0_f64.to_bits() },
                ]
            );
        }
        assert_eq!(
            status,
            if wrong_off || refuse_end {
                CLAP_PROCESS_ERROR
            } else {
                CLAP_PROCESS_CONTINUE
            }
        );
        plugin.stop();
        assert_eq!(
            plugin.exact_offs(),
            if wrong_off || refuse_end { 0.0 } else { 1.0 }
        );
        assert_eq!(process_calls(&plugin), 1.0);
    }
    assert_eq!(DROPPED.load(Ordering::Acquire), before + 3);
}
