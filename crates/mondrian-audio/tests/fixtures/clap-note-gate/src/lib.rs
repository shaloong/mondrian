//! Original owned CLAP NoteGate oracle; raw official ABI, no host model.
#![deny(unsafe_op_in_unsafe_fn)]
#[cfg(not(target_has_atomic = "64"))]
compile_error!("this owned fixture requires native 64-bit atomics");

#[cfg(test)]
mod abi_tests;
mod gate;

use clap_sys::*;
use gate::{Address, End, Gate, RELEASE};
use std::cell::UnsafeCell;
use std::ffi::{c_char, c_void, CStr};
use std::fmt::{self, Write as _};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};

const GAIN: u32 = 17;
const SCENARIO: u32 = 19;
const EXACT_OFFS: u32 = 23;
const PROCESS_CALLS: u32 = 29;
const MAX_EVENTS: usize = 128;
const MAX_FRAMES: u32 = 8192;
const NOTE_IDS: [u32; 2] = [0, 0x8000_0017];
const INPUT_ID: u32 = 0x101;
const OUTPUT_ID: u32 = 0x201;

struct FeaturePointers([*const c_char; 3]);
// SAFETY: this one immutable static contains only static C strings and null.
unsafe impl Sync for FeaturePointers {}
static FEATURES: FeaturePointers =
    FeaturePointers([c"audio-effect".as_ptr(), c"mono".as_ptr(), ptr::null()]);
static DESCRIPTOR: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: clap_sys::version::CLAP_VERSION,
    id: c"org.mondrian.fixture.note-gate".as_ptr(),
    name: c"Mondrian owned NoteGate PCM oracle".as_ptr(),
    vendor: c"Mondrian".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"0.0.0".as_ptr(),
    description: c"Fixed mono PCM gate; independent concrete note pairing oracle".as_ptr(),
    features: FEATURES.0.as_ptr(),
};

// The sole immutable native audio topology; no host mode or mutable rescan state.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    PcmInput,
    ZeroInput,
    ThreeOutputs,
}
impl Variant {
    const fn input_count(self) -> u32 {
        match self {
            Self::PcmInput => 1,
            Self::ZeroInput | Self::ThreeOutputs => 0,
        }
    }
    const fn output_count(self) -> u32 {
        match self {
            Self::ThreeOutputs => 3,
            Self::PcmInput | Self::ZeroInput => 1,
        }
    }
    const fn parameter_count(self) -> u32 {
        if matches!(self, Self::ThreeOutputs) {
            4
        } else {
            3
        }
    }
    const fn descriptor(self) -> &'static clap_plugin_descriptor {
        match self {
            Self::PcmInput => &DESCRIPTOR,
            Self::ZeroInput => &ZERO_INPUT_DESCRIPTOR,
            Self::ThreeOutputs => &THREE_OUTPUTS_DESCRIPTOR,
        }
    }
}
static ZERO_INPUT_FEATURES: FeaturePointers =
    FeaturePointers([c"instrument".as_ptr(), c"mono".as_ptr(), ptr::null()]);
static ZERO_INPUT_DESCRIPTOR: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: clap_sys::version::CLAP_VERSION,
    id: c"org.mondrian.fixture.note-gate.zero-input".as_ptr(),
    name: c"Mondrian owned zero-input NoteGate oracle".as_ptr(),
    vendor: c"Mondrian".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"0.0.0".as_ptr(),
    description: c"Fixed mono output, no audio inputs; original independent NoteGate".as_ptr(),
    features: ZERO_INPUT_FEATURES.0.as_ptr(),
};

static THREE_OUTPUTS_DESCRIPTOR: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: clap_sys::version::CLAP_VERSION,
    id: c"org.mondrian.fixture.note-gate.zero-input.three-outputs".as_ptr(),
    name: c"Mondrian owned three-output NoteGate oracle".as_ptr(),
    vendor: c"Mondrian".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"0.0.0".as_ptr(),
    description: c"Fixed three mono outputs, one independent Gate; no audio inputs".as_ptr(),
    features: ZERO_INPUT_FEATURES.0.as_ptr(),
};

struct Dsp {
    gate: Gate,
    max_frames: u32,
}

struct Instance {
    plugin: clap_plugin,
    host: *const clap_host,
    variant: Variant,
    thread_check: AtomicPtr<clap_host_thread_check>,
    gain: AtomicU64,
    scenario: AtomicU32,
    exact_offs: AtomicU32,
    // New-ID diagnostic only: actual Process callback invocations (including
    // refused calls), since the last accepted activate/reset. Not MNG1 state,
    // not an authored parameter or an emitted Source event.
    process_calls: AtomicU32,
    fault: AtomicBool,
    active: AtomicBool,
    processing: AtomicBool,
    // CLAP gives activate/deactivate exclusive main ownership and exclusive
    // process/reset audio ownership. Other callbacks read atomic scalars only.
    dsp: UnsafeCell<Dsp>,
}

// SAFETY at every native entry: the caller supplies its live plugin/host and
// valid advertised ABI pointers; plugin_data is the one Box owned by create.
// Pointer validation cannot prove foreign memory readable. Every nullable
// function/pointer, scalar count and exact event extent is checked before use.
unsafe fn owner<'a>(plugin: *const clap_plugin) -> Option<&'a Instance> {
    if plugin.is_null() {
        return None;
    }
    let data = unsafe { (*plugin).plugin_data.cast::<Instance>() };
    if data.is_null() {
        None
    } else {
        Some(unsafe { &*data })
    }
}

unsafe fn role(owner: &Instance, audio: bool) -> bool {
    let check = owner.thread_check.load(Ordering::Acquire);
    if check.is_null() {
        return false;
    }
    let (Some(main), Some(dsp)) = (unsafe { (*check).is_main_thread }, unsafe {
        (*check).is_audio_thread
    }) else {
        return false;
    };
    let is_audio = unsafe { dsp(owner.host) };
    if audio {
        is_audio
    } else {
        // The official thread-check contract permits main to carry the audio
        // role as well. Exclusivity comes from the CLAP lifecycle, not these
        // observational predicates being mutually exclusive.
        unsafe { main(owner.host) }
    }
}

unsafe extern "C" fn init(plugin: *const clap_plugin) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    let Some(get) = (unsafe { (*owner.host).get_extension }) else {
        return false;
    };
    let check =
        unsafe { get(owner.host, CLAP_EXT_THREAD_CHECK.as_ptr()) }.cast::<clap_host_thread_check>();
    owner.thread_check.store(check.cast_mut(), Ordering::Release);
    unsafe { role(owner, false) }
}

unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return;
    };
    let check = owner.thread_check.load(Ordering::Acquire);
    let has_main_check = !check.is_null()
        && unsafe { (*check).is_main_thread }.is_some()
        && unsafe { (*check).is_audio_thread }.is_some();
    if owner.active.load(Ordering::Acquire)
        || owner.processing.load(Ordering::Acquire)
        || (has_main_check && !unsafe { role(owner, false) })
    {
        // Preserve custody on a lifecycle contract violation, not cross-thread
        // free a live DSP object. The caller must correct/retire the instance.
        owner.fault.store(true, Ordering::Release);
        return;
    }
    // Factory create is thread-safe; it does not identify the main thread.
    // destroy is always a main-thread callback under the official ABI. If init
    // failed before optional ThreadCheck was available, retain that caller
    // precondition and still free the inactive Box. An available actual role
    // service is enforced; no factory-thread identity or guessed OS ID is used.
    unsafe { drop(Box::from_raw((*plugin).plugin_data.cast::<Instance>())) };
}

#[cfg(test)]
impl Drop for Instance {
    fn drop(&mut self) {
        abi_tests::DROPPED.fetch_add(1, Ordering::AcqRel);
    }
}

unsafe extern "C" fn activate(plugin: *const clap_plugin, rate: f64, min: u32, max: u32) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if !unsafe { role(owner, false) }
        || owner.active.load(Ordering::Acquire)
        || !rate.is_finite()
        || rate <= 0.0
        || min == 0
        || min > max
        || max > MAX_FRAMES
    {
        return false;
    }
    // SAFETY: inactive instance, exclusive activate on main, no DSP borrow.
    unsafe {
        *owner.dsp.get() = Dsp { gate: Gate::new(), max_frames: max };
    }
    owner.exact_offs.store(0, Ordering::Release);
    owner.process_calls.store(0, Ordering::Release);
    owner.active.store(true, Ordering::Release);
    true
}

unsafe extern "C" fn deactivate(plugin: *const clap_plugin) {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return;
    };
    if !unsafe { role(owner, false) } || owner.processing.load(Ordering::Acquire) {
        owner.fault.store(true, Ordering::Release);
        return;
    }
    owner.active.store(false, Ordering::Release);
}

unsafe extern "C" fn start(plugin: *const clap_plugin) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if !unsafe { role(owner, true) }
        || !owner.active.load(Ordering::Acquire)
        || owner.fault.load(Ordering::Acquire)
    {
        return false;
    }
    !owner.processing.swap(true, Ordering::AcqRel)
}

unsafe extern "C" fn stop(plugin: *const clap_plugin) {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return;
    };
    if !unsafe { role(owner, true) } {
        owner.fault.store(true, Ordering::Release);
        return;
    }
    owner.processing.store(false, Ordering::Release);
}

unsafe extern "C" fn reset(plugin: *const clap_plugin) {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return;
    };
    if !unsafe { role(owner, true) } || !owner.active.load(Ordering::Acquire) {
        owner.fault.store(true, Ordering::Release);
        return;
    }
    // SAFETY: the exclusive native audio reset cannot overlap process or main
    // lifecycle. It cancels old native voices and pending Ends, not parameters.
    unsafe {
        (*owner.dsp.get()).gate = Gate::new();
    }
    owner.exact_offs.store(0, Ordering::Release);
    owner.process_calls.store(0, Ordering::Release);
    owner.fault.store(false, Ordering::Release);
}

#[derive(Clone, Copy)]
enum Input {
    Empty,
    Param {
        time: u32,
        id: u32,
        value: f64,
    },
    On {
        time: u32,
        address: Address,
        velocity: f64,
    },
    Off {
        time: u32,
        address: Address,
        velocity: f64,
    },
}
impl Input {
    fn time(self) -> u32 {
        match self {
            Self::Empty => 0,
            Self::Param { time, .. } | Self::On { time, .. } | Self::Off { time, .. } => time,
        }
    }
}

fn parameter_valid(id: u32, value: f64) -> bool {
    value.is_finite()
        && match id {
            GAIN => (0.0..=1.0).contains(&value),
            SCENARIO => (0.0..=7.0).contains(&value) && value.fract() == 0.0,
            _ => false,
        }
}

fn set_parameter(owner: &Instance, id: u32, value: f64) -> bool {
    if !parameter_valid(id, value) {
        return false;
    }
    match id {
        GAIN => owner.gain.store(value.to_bits(), Ordering::Release),
        SCENARIO => owner.scenario.store(value as u32, Ordering::Release),
        _ => return false,
    }
    true
}

unsafe fn decode(
    list: *const clap_input_events,
    frames: u32,
    storage: &mut [Input; MAX_EVENTS],
) -> Option<usize> {
    if list.is_null() {
        return None;
    }
    let (Some(size), Some(get)) = (unsafe { (*list).size }, unsafe { (*list).get }) else {
        return None;
    };
    let count = unsafe { size(list) } as usize;
    if count > MAX_EVENTS {
        return None;
    }
    let mut previous = 0;
    for (index, slot) in storage.iter_mut().take(count).enumerate() {
        let event = unsafe { get(list, index as u32) };
        if event.is_null() {
            return None;
        }
        let header = unsafe { event.read_unaligned() };
        if header.time >= frames
            || (index != 0 && header.time < previous)
            || header.space_id != CLAP_CORE_EVENT_SPACE_ID
            || header.flags & !3 != 0
        {
            return None;
        }
        previous = header.time;
        *slot = if header.type_ == CLAP_EVENT_PARAM_VALUE as u16 {
            if header.size as usize != size_of::<clap_event_param_value>() {
                return None;
            }
            let value = unsafe { event.cast::<clap_event_param_value>().read_unaligned() };
            if !value.cookie.is_null()
                || value.note_id != -1
                || value.port_index != -1
                || value.channel != -1
                || value.key != -1
                || !parameter_valid(value.param_id, value.value)
            {
                return None;
            }
            Input::Param {
                time: header.time,
                id: value.param_id,
                value: value.value,
            }
        } else if header.type_ == CLAP_EVENT_NOTE_ON as u16
            || header.type_ == CLAP_EVENT_NOTE_OFF as u16
        {
            if header.size as usize != size_of::<clap_event_note>() {
                return None;
            }
            let note = unsafe { event.cast::<clap_event_note>().read_unaligned() };
            let address = Address {
                port: note.port_index,
                channel: note.channel,
                key: note.key,
                id: note.note_id,
            };
            if !address.valid()
                || !note.velocity.is_finite()
                || !(0.0..=1.0).contains(&note.velocity)
            {
                return None;
            }
            if header.type_ == CLAP_EVENT_NOTE_ON as u16 {
                Input::On {
                    time: header.time,
                    address,
                    velocity: note.velocity,
                }
            } else {
                Input::Off {
                    time: header.time,
                    address,
                    velocity: note.velocity,
                }
            }
        } else {
            return None;
        };
    }
    Some(count)
}

unsafe fn push_parameter(list: *const clap_output_events, frame: u32, value: f64) -> bool {
    let Some(push) = (unsafe { (*list).try_push }) else {
        return false;
    };
    let event = clap_event_param_value {
        header: clap_event_header {
            size: size_of::<clap_event_param_value>() as u32,
            time: frame,
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
        value,
    };
    unsafe { push(list, &event.header) }
}

unsafe fn emit_end(list: *const clap_output_events, frame: u32, gain: f64, end: End) -> bool {
    if list.is_null() || !unsafe { push_parameter(list, frame, gain) } {
        return false;
    }
    let Some(push) = (unsafe { (*list).try_push }) else {
        return false;
    };
    let mut address = end.address;
    match end.scenario {
        3 => address.port ^= 1, // Concrete ID, wrong original input port.
        4 => address.id = -2,   // Illegal concrete/wildcard scalar.
        5 => address.id = -1,   // Standard-valid wildcard; first host subset refuses.
        6 => {
            address.port = -1;
            address.channel = -1;
            address.key = -1;
        }
        7 => address.key ^= 1, // Concrete ID, wrong original key.
        _ => {}
    }
    let event = clap_event_note {
        header: clap_event_header {
            size: size_of::<clap_event_note>() as u32,
            time: u32::MAX,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_NOTE_END as u16,
            flags: 0,
        },
        note_id: address.id,
        port_index: address.port,
        channel: address.channel,
        key: address.key,
        velocity: 0.0,
    };
    if !unsafe { push(list, &event.header) } {
        return false;
    }
    if end.scenario == 2 && !unsafe { push(list, &event.header) } {
        return false;
    }
    // The real next parameter timestamp remains frame, even after ignored MAX
    // NoteEnd and even when another End follows at this same frame.
    unsafe { push_parameter(list, frame, gain) }
}

unsafe fn process_inner(owner: &Instance, process: *const clap_process) -> bool {
    if process.is_null()
        || !unsafe { role(owner, true) }
        || !owner.processing.load(Ordering::Acquire)
        || owner.fault.load(Ordering::Acquire)
    {
        return false;
    }
    let block = unsafe { &*process };
    // Exclusive process ownership permits reading the activated bound. No Gate
    // mutation or PCM write is allowed until every native port is checked.
    let max_frames = unsafe { (*owner.dsp.get()).max_frames };
    let frames = block.frames_count;
    if frames == 0
        || frames > max_frames
        || block.audio_inputs_count != owner.variant.input_count()
        || block.audio_outputs_count != owner.variant.output_count()
        || (owner.variant == Variant::PcmInput && block.audio_inputs.is_null())
        || block.audio_outputs.is_null()
        || block.out_events.is_null()
        || unsafe { (*block.out_events).try_push }.is_none()
    {
        return false;
    }
    // Count0 permits NULL or a non-readable non-NULL array. Neither is read.
    let input = if owner.variant == Variant::PcmInput {
        Some(unsafe { block.audio_inputs.read() })
    } else {
        None
    };
    if input.as_ref().is_some_and(|port| {
        port.channel_count != 1 || port.data32.is_null() || !port.data64.is_null()
    }) {
        return false;
    }
    let (input, constant_input) = match input {
        Some(port) => (unsafe { port.data32.read() }, port.constant_mask & 1 != 0),
        None => (ptr::null_mut(), false),
    };
    if owner.variant == Variant::PcmInput && input.is_null() {
        return false;
    }
    let Some(bytes) = (frames as usize).checked_mul(size_of::<f32>()) else {
        return false;
    };
    let Some(in_end) = (input as usize).checked_add(bytes) else {
        return false;
    };
    let output_count = owner.variant.output_count() as usize;
    let mut outputs = [ptr::null_mut::<f32>(); 3];
    let mut ranges = [(0_usize, 0_usize); 3];
    for index in 0..output_count {
        let port = unsafe { block.audio_outputs.add(index).read() };
        if port.channel_count != 1 || port.data32.is_null() || !port.data64.is_null() {
            return false;
        }
        let output = unsafe { port.data32.read() };
        if output.is_null() || !(output as usize).is_multiple_of(align_of::<f32>()) {
            return false;
        }
        let Some(out_end) = (output as usize).checked_add(bytes) else {
            return false;
        };
        if owner.variant == Variant::PcmInput
            && input != output
            && (input as usize) < out_end
            && (output as usize) < in_end
        {
            return false; // Original mono input supports disjoint or exact in-place.
        }
        for &(start, end) in &ranges[..index] {
            if (output as usize) < end && start < out_end {
                return false; // Distinct native outputs may never alias.
            }
        }
        outputs[index] = output;
        ranges[index] = (output as usize, out_end);
    }
    let mut staged = [Input::Empty; MAX_EVENTS];
    let Some(count) = (unsafe { decode(block.in_events, frames, &mut staged) }) else {
        return false;
    };
    // Cache exact in-place input before any output mutation. Both instruments
    // use carrier1 and the SAME independent native Gate, not three voices.
    let constant_sample = if owner.variant != Variant::PcmInput {
        Some(1.0)
    } else if constant_input {
        Some(unsafe { input.read() })
    } else {
        None
    };
    if constant_sample.is_some_and(|value| !value.is_finite()) {
        return false;
    }
    for index in 0..output_count {
        unsafe { (*block.audio_outputs.add(index)).constant_mask = 0 };
    }
    // SAFETY: the one actual exclusive process operation owns the one Gate.
    let dsp = unsafe { &mut *owner.dsp.get() };
    let mut next = 0;
    for frame in 0..frames {
        while next < count && staged[next].time() == frame {
            let accepted = match staged[next] {
                Input::Param { id, value, .. } => set_parameter(owner, id, value),
                Input::On { address, velocity, .. } => dsp.gate.on(
                    address,
                    velocity,
                    owner.scenario.load(Ordering::Acquire) as u8,
                ),
                Input::Off { address, velocity, .. } => dsp.gate.off(address, velocity),
                Input::Empty => false,
            };
            if !accepted {
                return false;
            }
            owner.exact_offs.store(dsp.gate.exact_offs, Ordering::Release);
            next += 1;
        }
        let input = match constant_sample {
            Some(value) => value,
            None => unsafe { input.add(frame as usize).read() },
        };
        if !input.is_finite() {
            return false;
        }
        let gain = f64::from_bits(owner.gain.load(Ordering::Acquire));
        let (coefficient, ends) = dsp.gate.sample();
        let value = (f64::from(input) * gain * coefficient) as f32;
        if !value.is_finite() {
            return false;
        }
        // Three exactly representable dyadic outputs from one sample advance.
        // Old variants still write only their original unscaled single output.
        unsafe { outputs[0].add(frame as usize).write(value) };
        if owner.variant == Variant::ThreeOutputs {
            unsafe {
                outputs[1].add(frame as usize).write(-value * 0.5);
                outputs[2].add(frame as usize).write(value * 0.25);
            }
        }
        for end in ends.into_iter().flatten() {
            if !unsafe { emit_end(block.out_events, frame, gain, end) } {
                return false;
            }
        }
    }
    next == count
}

unsafe extern "C" fn process(
    plugin: *const clap_plugin,
    block: *const clap_process,
) -> clap_process_status {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return CLAP_PROCESS_ERROR;
    };
    if owner.variant == Variant::ThreeOutputs {
        // CLAP makes Process exclusive; the only other writer is exclusive
        // activate/reset. Atomic storage permits concurrent read-only get_value.
        let Some(next) = owner.process_calls.load(Ordering::Acquire).checked_add(1) else {
            owner.fault.store(true, Ordering::Release);
            return CLAP_PROCESS_ERROR;
        };
        owner.process_calls.store(next, Ordering::Release);
    }
    if unsafe { process_inner(owner, block) } {
        CLAP_PROCESS_CONTINUE
    } else {
        owner.fault.store(true, Ordering::Release);
        // No error formatting, logging, allocation or recovery wait on DSP.
        CLAP_PROCESS_ERROR
    }
}

unsafe extern "C" fn on_main(_: *const clap_plugin) {}

unsafe extern "C" fn audio_count(plugin: *const clap_plugin, input: bool) -> u32 {
    match unsafe { owner(plugin) } {
        Some(owner) if unsafe { role(owner, false) } => {
            if input {
                owner.variant.input_count()
            } else {
                owner.variant.output_count()
            }
        }
        _ => 0,
    }
}
unsafe extern "C" fn audio_get(
    plugin: *const clap_plugin,
    index: u32,
    input: bool,
    out: *mut clap_audio_port_info,
) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if index
        >= if input {
            owner.variant.input_count()
        } else {
            owner.variant.output_count()
        }
        || out.is_null()
        || !unsafe { role(owner, false) }
        || (input && owner.variant != Variant::PcmInput)
    {
        return false;
    }
    let mut port = clap_audio_port_info {
        id: if owner.variant == Variant::ThreeOutputs {
            index + 1
        } else if input {
            INPUT_ID
        } else {
            OUTPUT_ID
        },
        name: [0; 256],
        flags: if index == 0 {
            CLAP_AUDIO_PORT_IS_MAIN as u32
        } else {
            0
        },
        channel_count: 1,
        port_type: CLAP_PORT_MONO.as_ptr(),
        in_place_pair: if owner.variant != Variant::PcmInput {
            CLAP_INVALID_ID
        } else if input {
            OUTPUT_ID
        } else {
            INPUT_ID
        },
    };
    name(
        &mut port.name,
        if owner.variant == Variant::ThreeOutputs {
            match index {
                0 => b"mono main",
                1 => b"mono auxiliary 1",
                _ => b"mono auxiliary 2",
            }
        } else if input {
            b"mono input"
        } else {
            b"mono output"
        },
    );
    unsafe {
        out.write(port);
    }
    true
}

unsafe extern "C" fn note_count(plugin: *const clap_plugin, input: bool) -> u32 {
    match unsafe { owner(plugin) } {
        Some(owner) if input && unsafe { role(owner, false) } => 2,
        _ => 0,
    }
}
unsafe extern "C" fn note_get(
    plugin: *const clap_plugin,
    index: u32,
    input: bool,
    out: *mut clap_note_port_info,
) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if !input || index >= 2 || out.is_null() || !unsafe { role(owner, false) } {
        return false;
    }
    let mut port = clap_note_port_info {
        id: NOTE_IDS[index as usize],
        supported_dialects: CLAP_NOTE_DIALECT_CLAP as u32,
        preferred_dialect: CLAP_NOTE_DIALECT_CLAP as u32,
        name: [0; 256],
    };
    name(
        &mut port.name,
        if index == 0 { b"gate x1" } else { b"gate x2" },
    );
    unsafe {
        out.write(port);
    }
    true
}

fn name<const N: usize>(out: &mut [c_char; N], text: &[u8]) {
    for (slot, byte) in out.iter_mut().take(N.saturating_sub(1)).zip(text) {
        *slot = *byte as c_char;
    }
}

unsafe extern "C" fn parameter_count(plugin: *const clap_plugin) -> u32 {
    match unsafe { owner(plugin) } {
        Some(owner) if unsafe { role(owner, false) } => owner.variant.parameter_count(),
        _ => 0,
    }
}
unsafe extern "C" fn parameter_info(
    plugin: *const clap_plugin,
    index: u32,
    out: *mut clap_param_info,
) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if index >= owner.variant.parameter_count() || out.is_null() || !unsafe { role(owner, false) } {
        return false;
    }
    let (id, flags, title, max, default) = match index {
        0 => (
            GAIN,
            CLAP_PARAM_IS_AUTOMATABLE,
            b"Gain".as_slice(),
            1.0,
            1.0,
        ),
        1 => (
            SCENARIO,
            CLAP_PARAM_IS_AUTOMATABLE | CLAP_PARAM_IS_STEPPED,
            b"End scenario".as_slice(),
            7.0,
            0.0,
        ),
        2 => (
            EXACT_OFFS,
            CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_STEPPED,
            b"Exact Off count".as_slice(),
            f64::from(u32::MAX),
            0.0,
        ),
        _ => (
            PROCESS_CALLS,
            CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_STEPPED,
            b"Actual Process callback count".as_slice(),
            f64::from(u32::MAX),
            0.0,
        ),
    };
    let mut info = clap_param_info {
        id,
        flags: flags as u32,
        cookie: ptr::null_mut(),
        name: [0; 256],
        module: [0; 1024],
        min_value: 0.0,
        max_value: max,
        default_value: default,
    };
    name(&mut info.name, title);
    unsafe {
        out.write(info);
    }
    true
}

unsafe extern "C" fn parameter_value(plugin: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if out.is_null() || !unsafe { role(owner, false) } {
        return false;
    }
    let value = match id {
        GAIN => f64::from_bits(owner.gain.load(Ordering::Acquire)),
        SCENARIO => f64::from(owner.scenario.load(Ordering::Acquire)),
        EXACT_OFFS => f64::from(owner.exact_offs.load(Ordering::Acquire)),
        PROCESS_CALLS if owner.variant == Variant::ThreeOutputs => {
            f64::from(owner.process_calls.load(Ordering::Acquire))
        }
        _ => return false,
    };
    unsafe {
        out.write(value);
    }
    true
}

struct Text<'a> {
    buffer: &'a mut [c_char],
    used: usize,
}
impl fmt::Write for Text<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.used + text.len() >= self.buffer.len() {
            return Err(fmt::Error);
        }
        for byte in text.bytes() {
            self.buffer[self.used] = byte as c_char;
            self.used += 1;
        }
        self.buffer[self.used] = 0;
        Ok(())
    }
}

fn display_valid(variant: Variant, id: u32, value: f64) -> bool {
    parameter_valid(id, value)
        || ((id == EXACT_OFFS || (id == PROCESS_CALLS && variant == Variant::ThreeOutputs))
            && value.is_finite()
            && (0.0..=f64::from(u32::MAX)).contains(&value)
            && value.fract() == 0.0)
}
unsafe extern "C" fn value_to_text(
    plugin: *const clap_plugin,
    id: u32,
    value: f64,
    buffer: *mut c_char,
    capacity: u32,
) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if buffer.is_null()
        || capacity == 0
        || !display_valid(owner.variant, id, value)
        || !unsafe { role(owner, false) }
    {
        return false;
    }
    let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, capacity.min(128) as usize) };
    buffer[0] = 0;
    write!(&mut Text { buffer, used: 0 }, "{value}").is_ok()
}
unsafe extern "C" fn text_to_value(
    plugin: *const clap_plugin,
    id: u32,
    text: *const c_char,
    out: *mut f64,
) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if text.is_null() || out.is_null() || !unsafe { role(owner, false) } {
        return false;
    }
    let Ok(text) = (unsafe { CStr::from_ptr(text) }).to_str() else {
        return false;
    };
    let Ok(value) = text.parse::<f64>() else {
        return false;
    };
    if !display_valid(owner.variant, id, value) {
        return false;
    }
    unsafe {
        out.write(value);
    }
    true
}

unsafe extern "C" fn flush(
    plugin: *const clap_plugin,
    list: *const clap_input_events,
    _: *const clap_output_events,
) {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return;
    };
    if !unsafe { role(owner, owner.active.load(Ordering::Acquire)) } {
        owner.fault.store(true, Ordering::Release);
        return;
    }
    let mut storage = [Input::Empty; MAX_EVENTS];
    let Some(count) = (unsafe { decode(list, 1, &mut storage) }) else {
        owner.fault.store(true, Ordering::Release);
        return;
    };
    for event in storage.into_iter().take(count) {
        if let Input::Param { id, value, .. } = event
            && set_parameter(owner, id, value)
        {
            continue;
        }
        // flush has no return status; latch refusal for the next real process.
        owner.fault.store(true, Ordering::Release);
        return;
    }
}

unsafe extern "C" fn save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if stream.is_null() || !unsafe { role(owner, false) } {
        return false;
    }
    let Some(write) = (unsafe { (*stream).write }) else {
        return false;
    };
    let mut bytes = [0_u8; 16];
    bytes[..4].copy_from_slice(b"MNG1");
    bytes[4..12].copy_from_slice(&owner.gain.load(Ordering::Acquire).to_le_bytes());
    bytes[12..].copy_from_slice(&owner.scenario.load(Ordering::Acquire).to_le_bytes());
    let mut offset = 0;
    // Fixed 16-byte state: accept partial writes; zero/negative/over-report fail.
    while offset < bytes.len() {
        let count = unsafe {
            write(
                stream,
                bytes[offset..].as_ptr().cast(),
                (16 - offset) as u64,
            )
        };
        if count <= 0 || count as usize > 16 - offset {
            return false;
        }
        offset += count as usize;
    }
    true
}

unsafe extern "C" fn load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    let Some(owner) = (unsafe { owner(plugin) }) else {
        return false;
    };
    if stream.is_null() || owner.active.load(Ordering::Acquire) || !unsafe { role(owner, false) } {
        return false;
    }
    let Some(read) = (unsafe { (*stream).read }) else {
        return false;
    };
    let mut bytes = [0_u8; 16];
    let mut offset = 0;
    while offset < bytes.len() {
        let count = unsafe {
            read(
                stream,
                bytes[offset..].as_mut_ptr().cast(),
                (16 - offset) as u64,
            )
        };
        if count <= 0 || count as usize > 16 - offset {
            return false;
        }
        offset += count as usize;
    }
    if &bytes[..4] != b"MNG1" {
        return false;
    }
    let gain = f64::from_le_bytes([
        bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
    ]);
    let scenario = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    if !parameter_valid(GAIN, gain) || scenario > 7 {
        return false;
    }
    owner.gain.store(gain.to_bits(), Ordering::Release);
    owner.scenario.store(scenario, Ordering::Release);
    true
}

unsafe extern "C" fn latency(_: *const clap_plugin) -> u32 {
    0
}
unsafe extern "C" fn tail(_: *const clap_plugin) -> u32 {
    u32::from(RELEASE)
}
static AUDIO: clap_plugin_audio_ports =
    clap_plugin_audio_ports { count: Some(audio_count), get: Some(audio_get) };
static NOTES: clap_plugin_note_ports =
    clap_plugin_note_ports { count: Some(note_count), get: Some(note_get) };
static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(parameter_count),
    get_info: Some(parameter_info),
    get_value: Some(parameter_value),
    value_to_text: Some(value_to_text),
    text_to_value: Some(text_to_value),
    flush: Some(flush),
};
static STATE: clap_plugin_state = clap_plugin_state { save: Some(save), load: Some(load) };
static LATENCY: clap_plugin_latency = clap_plugin_latency { get: Some(latency) };
static TAIL: clap_plugin_tail = clap_plugin_tail { get: Some(tail) };

unsafe extern "C" fn extension(_: *const clap_plugin, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return ptr::null();
    }
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_AUDIO_PORTS {
        (&AUDIO as *const clap_plugin_audio_ports).cast()
    } else if id == CLAP_EXT_NOTE_PORTS {
        (&NOTES as *const clap_plugin_note_ports).cast()
    } else if id == CLAP_EXT_PARAMS {
        (&PARAMS as *const clap_plugin_params).cast()
    } else if id == CLAP_EXT_STATE {
        (&STATE as *const clap_plugin_state).cast()
    } else if id == CLAP_EXT_LATENCY {
        (&LATENCY as *const clap_plugin_latency).cast()
    } else if id == CLAP_EXT_TAIL {
        (&TAIL as *const clap_plugin_tail).cast()
    } else {
        ptr::null()
    }
}

unsafe extern "C" fn descriptor_count(_: *const clap_plugin_factory) -> u32 {
    3
}
unsafe extern "C" fn descriptor(
    _: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    match index {
        0 => &DESCRIPTOR,
        1 => &ZERO_INPUT_DESCRIPTOR,
        2 => &THREE_OUTPUTS_DESCRIPTOR,
        _ => ptr::null(),
    }
}
unsafe extern "C" fn create(
    _: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    if host.is_null() || id.is_null() || unsafe { (*host).clap_version.major } != 1 {
        return ptr::null();
    }
    let variant = match unsafe { CStr::from_ptr(id) } {
        id if id == c"org.mondrian.fixture.note-gate" => Variant::PcmInput,
        id if id == c"org.mondrian.fixture.note-gate.zero-input" => Variant::ZeroInput,
        id if id == c"org.mondrian.fixture.note-gate.zero-input.three-outputs" => {
            Variant::ThreeOutputs
        }
        _ => return ptr::null(),
    };
    // The only heap allocation in this original fixture is lifecycle creation;
    // destroy consumes it once after exclusive deactivate. No RT logging path.
    let mut owner = Box::new(Instance {
        plugin: clap_plugin {
            desc: variant.descriptor(),
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
        variant,
        thread_check: AtomicPtr::new(ptr::null_mut()),
        gain: AtomicU64::new(1.0_f64.to_bits()),
        scenario: AtomicU32::new(0),
        exact_offs: AtomicU32::new(0),
        process_calls: AtomicU32::new(0),
        fault: AtomicBool::new(false),
        active: AtomicBool::new(false),
        processing: AtomicBool::new(false),
        dsp: UnsafeCell::new(Dsp { gate: Gate::new(), max_frames: 0 }),
    });
    owner.plugin.plugin_data = (&mut *owner as *mut Instance).cast();
    let plugin = &owner.plugin as *const clap_plugin;
    let _owned = Box::into_raw(owner);
    plugin
}
static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(descriptor_count),
    get_plugin_descriptor: Some(descriptor),
    create_plugin: Some(create),
};
unsafe extern "C" fn entry_init(_: *const c_char) -> bool {
    true
}
unsafe extern "C" fn entry_deinit() {}
unsafe extern "C" fn factory(id: *const c_char) -> *const c_void {
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
    get_factory: Some(factory),
};
