//! VST3 sidechain qualification fixture, adapted from the MIT/Apache-2.0
//! `vst3-rs` Gain example pinned by the Mondrian workspace.
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr, CString};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::{ptr, slice};

use vst3::{uid, Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};
use std::thread::{self, ThreadId};

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c_string = CString::new(src).unwrap_or_else(|_| CString::default());
    let bytes = c_string.as_bytes_with_nul();

    for (src, dst) in bytes.iter().zip(dst.iter_mut()) {
        *dst = *src as c_char;
    }

    if bytes.len() > dst.len() {
        if let Some(last) = dst.last_mut() {
            *last = 0;
        }
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (src, dst) in src.encode_utf16().zip(dst.iter_mut()) {
        *dst = src as TChar;
        len += 1;
    }

    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

unsafe fn len_wstring(string: *const TChar) -> usize {
    let mut len = 0;

    while *string.offset(len) != 0 {
        len += 1;
    }

    len as usize
}

const PLUGIN_NAME: &str = "Mondrian Sidechain Fixture";
const STATE_MAGIC: [u8; 4] = *b"MSS1";
const STATE_VERSION: u32 = 1;

unsafe fn read_gain_state(stream: *mut IBStream) -> Option<(f64, u32)> {
    let stream = ComRef::from_raw(stream)?;
    let mut bytes = [0_u8; 20];
    let mut read = 0_i32;
    if stream.read(
        bytes.as_mut_ptr().cast::<c_void>(),
        bytes.len() as i32,
        &mut read,
    ) != kResultOk
        || read as usize != bytes.len()
        || bytes[..4] != STATE_MAGIC
        || u32::from_le_bytes(bytes[4..8].try_into().ok()?) != STATE_VERSION
    {
        return None;
    }
    let gain = f64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let mode = u32::from_le_bytes(bytes[16..20].try_into().ok()?);
    (gain.is_finite() && (0.0..=1.0).contains(&gain) && mode <= 7).then_some((gain, mode))
}

unsafe fn write_gain_state(stream: *mut IBStream, gain: f64, mode: u32) -> tresult {
    let Some(stream) = ComRef::from_raw(stream) else {
        return kInvalidArgument;
    };
    let mut bytes = [0_u8; 20];
    bytes[..4].copy_from_slice(&STATE_MAGIC);
    bytes[4..8].copy_from_slice(&STATE_VERSION.to_le_bytes());
    bytes[8..16].copy_from_slice(&gain.to_le_bytes());
    bytes[16..20].copy_from_slice(&mode.to_le_bytes());
    let mut written = 0_i32;
    if stream.write(
        bytes.as_mut_ptr().cast::<c_void>(),
        bytes.len() as i32,
        &mut written,
    ) == kResultOk
        && written as usize == bytes.len()
    {
        kResultOk
    } else {
        kResultFalse
    }
}

// An independent, deliberately finite DLL DSP oracle. No SDK projection helper
// or floating-point seconds/tempo calculation is used to construct expectations.
fn musical_context_tag(data: &ProcessData, prepared_rate: u64) -> Option<f32> {
    if data.processContext.is_null() {
        return None;
    }
    // SAFETY: process() receives the host's live ProcessData/ProcessContext for
    // this callback only. The reference is neither retained nor mutated.
    let context = unsafe { &*data.processContext };
    let rate = if prepared_rate == 44_100_f64.to_bits() {
        0
    } else if prepared_rate == 48_000_f64.to_bits() {
        1
    } else {
        return None;
    };
    let row = match context.projectTimeSamples {
        -17 => 0,
        -13 => 1,
        -9 => 2,
        sample if sample == i64::MAX - 32 => 0,
        sample if sample == i64::MAX - 28 => 1,
        sample if sample == i64::MAX - 24 => 2,
        _ => return None,
    };
    // Each complete caller window is split by the host into 4 + 4 + 1 frames.
    if data.numSamples != if row == 2 { 1 } else { 4 }
        || context.sampleRate.to_bits() != prepared_rate
        || context.systemTime != 0
        || context.continousTimeSamples != 0
        || context.barPositionMusic.to_bits() != 0
        || context.cycleStartMusic.to_bits() != 0
        || context.cycleEndMusic.to_bits() != 0
        || context.timeSigNumerator != 0
        || context.timeSigDenominator != 0
        || context.chord.keyNote != 0
        || context.chord.rootNote != 0
        || context.chord.chordMask != 0
        || context.smpteOffsetSubframes != 0
        || context.frameRate.framesPerSecond != 0
        || context.frameRate.flags != 0
        || context.samplesToNextClock != 0
        || context.projectTimeSamples.checked_add(i64::from(data.numSamples)).is_none()
    {
        return None;
    }
    // Correctly rounded independent literal bits for 1, 5, 9 divided by
    // (3 * rate / 4). Both signed origins use zero=(origin-1)/rate seconds.
    const QUARTER_NOTES: [[u64; 3]; 2] = [
        [
            0x3eff_b3f6_5f17_ec9e,
            0x3f23_d079_fb6e_f3e3,
            0x3f31_d53a_957d_7519,
        ],
        [
            0x3efd_208a_5a91_2e32,
            0x3f22_3456_789a_bcdf,
            0x3f30_624d_d2f1_a9fc,
        ],
    ];
    let tag = (1 + row + rate * 4) as f32 / 16.0;
    if context.state == 0 && context.projectTimeMusic.to_bits() == 0 && context.tempo.to_bits() == 0
    {
        Some(-tag)
    } else if context.state
        == (ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid
            | ProcessContext_::StatesAndFlags_::kTempoValid) as u32
        && context.projectTimeMusic.to_bits() == QUARTER_NOTES[rate][row]
        && context.tempo.to_bits() == 0x4054_0000_0000_0000
    // Exactly 80 BPM.
    {
        Some(tag)
    } else {
        None
    }
}

struct GainProcessor {
    mode: AtomicU32,
    active_buses: AtomicU32,
    delay: [AtomicU32; 14],
    configured_latency: AtomicUsize,
    connection: RefCell<Option<ComPtr<IConnectionPoint>>>,
    control_thread: Cell<Option<ThreadId>>,
    position: AtomicUsize,
    gain: AtomicU64,
    sample_rate: AtomicU64,
    // Mode7-only, non-restorable diagnostic count of actual process attempts.
    musical_attempts: AtomicU64,
}

impl Class for GainProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements, IConnectionPoint);
}

impl GainProcessor {
    const CID: TUID = uid(0x7486AF22, 0x234A480B, 0x9DF13173, 0xF12EB620);

    fn new() -> GainProcessor {
        GainProcessor {
            gain: AtomicU64::new(0.5f64.to_bits()),
            sample_rate: AtomicU64::new(0),
            musical_attempts: AtomicU64::new(0),
            mode: AtomicU32::new(0),
            active_buses: AtomicU32::new(0),
            delay: std::array::from_fn(|_| AtomicU32::new(0)),
            position: AtomicUsize::new(0),
            configured_latency: AtomicUsize::new(3),
            connection: RefCell::new(None),
            control_thread: Cell::new(None),
        }
    }
}

impl IPluginBaseTrait for GainProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        self.control_thread.set(Some(thread::current().id()));
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        self.connection.borrow_mut().take();
        self.control_thread.set(None);
        kResultOk
    }
}

impl IConnectionPointTrait for GainProcessor {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) { return kInvalidArgument; }
        let Some(other) = ComRef::from_raw(other) else { return kInvalidArgument; };
        let mut connection = self.connection.borrow_mut();
        if connection.is_some() { return kResultFalse; }
        *connection = Some(other.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, other: *mut IConnectionPoint) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) { return kInvalidArgument; }
        let mut connection = self.connection.borrow_mut();
        if !connection.as_ref().is_some_and(|peer| peer.as_ptr() == other) { return kInvalidArgument; }
        connection.take();
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) { return kInvalidArgument; }
        let Some(message) = ComRef::from_raw(message) else { return kInvalidArgument; };
        let id = message.getMessageID();
        if id.is_null() { return kResultFalse; }
        let id = CStr::from_ptr(id);
        let Some(attributes) = ComRef::from_raw(message.getAttributes()) else { return kInvalidArgument; };
        if id.to_bytes() == b"mondrian.fixture.profile" {
            let profile = self.mode.load(Ordering::Acquire);
            if !matches!(profile, 5 | 6) { return kResultFalse; }
            // A main/control query observes the component configuration that an
            // earlier real DSP call recorded. The controller does not create it.
            return attributes.setInt(c"profile".as_ptr(), i64::from(profile));
        }
        if id.to_bytes() != b"mondrian.fixture.quality" { return kResultFalse; }
        let mut quality = 0.0;
        if attributes.getFloat(c"quality".as_ptr(), &mut quality) != kResultOk
            || self.mode.load(Ordering::Relaxed) != 4
            || !quality.is_finite() || !(0.0..=1.0).contains(&quality)
        { return kInvalidArgument; }
        // Retain the currently active DSP latency until the host legally deactivates
        // and reactivates. A host that ignores the request still receives valid PCM.
        self.gain.store(quality.to_bits(), Ordering::Release);
        kResultOk
    }
}

impl IComponentTrait for GainProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        *class_id = GainController::CID;
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, mediaType: MediaType, dir: BusDirection) -> i32 {
        match mediaType as BusDirections {
            MediaTypes_::kAudio => match dir as BusDirections {
                BusDirections_::kInput => {
                    if self.mode.load(Ordering::Relaxed) == 2 {
                        2
                    } else {
                        3
                    }
                }
                BusDirections_::kOutput => 1,
                _ => 0,
            },
            MediaTypes_::kEvent => 0,
            _ => 0,
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        direction: BusDirection,
        index: i32,
        output: *mut BusInfo,
    ) -> tresult {
        let count = self.getBusCount(media_type, direction);
        if output.is_null() || index < 0 || index >= count {
            return kInvalidArgument;
        }
        let bus = &mut *output;
        bus.mediaType = media_type;
        bus.direction = direction;
        bus.channelCount = 2;
        let name = match (direction, index, self.mode.load(Ordering::Relaxed)) {
            (BusDirections_::kInput, 0, _) => "Main",
            (BusDirections_::kInput, _, 2) => "   ",
            (BusDirections_::kInput, _, 1) | (BusDirections_::kInput, 1, _) => "压缩探测 🙂",
            (BusDirections_::kInput, _, _) => "备用探测",
            _ => "Audio",
        };
        copy_wstring(name, &mut bus.name);
        bus.busType = if index == 0 {
            BusTypes_::kMain
        } else {
            BusTypes_::kAux
        } as BusType;
        bus.flags = if index == 0 {
            BusInfo_::BusFlags_::kDefaultActive as u32
        } else {
            0
        };
        kResultOk
    }

    unsafe fn getRoutingInfo(
        &self,
        _in_info: *mut RoutingInfo,
        _out_info: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        media_type: MediaType,
        direction: BusDirection,
        index: i32,
        state: TBool,
    ) -> tresult {
        if index < 0 || index >= self.getBusCount(media_type, direction) {
            return kInvalidArgument;
        }
        let bit = 1
            << (index
                + if direction == BusDirections_::kOutput {
                    8
                } else {
                    0
                });
        if state != 0 {
            self.active_buses.fetch_or(bit, Ordering::Relaxed);
        } else {
            self.active_buses.fetch_and(!bit, Ordering::Relaxed);
        }
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        let mode = self.mode.load(Ordering::Acquire);
        if matches!(mode, 4 | 5 | 6) && state != 0 {
            let gain = f64::from_bits(self.gain.load(Ordering::Acquire));
            let latency = if mode == 6 || (mode == 4 && gain >= 0.75) { 7 } else { 3 };
            self.configured_latency.store(latency, Ordering::Release);
            for sample in &self.delay { sample.store(0, Ordering::Relaxed); }
            self.position.store(0, Ordering::Relaxed);
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some((gain, mode)) = read_gain_state(state) else {
            return kResultFalse;
        };
        self.gain.store(gain.to_bits(), Ordering::Release);
        self.mode.store(mode, Ordering::Release);
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let mode = self.mode.load(Ordering::Acquire);
        let result = write_gain_state(
            state,
            f64::from_bits(self.gain.load(Ordering::Acquire)),
            mode,
        );
        if mode != 7 || result != kResultOk {
            return result;
        }
        let Some(stream) = ComRef::from_raw(state) else {
            return kInvalidArgument;
        };
        // Cold observation only. setState reads the original 20B configuration;
        // it deliberately does not restore this audit suffix or DSP history.
        let mut audit = [0_u8; 12];
        audit[..4].copy_from_slice(b"MSA7");
        audit[4..].copy_from_slice(&self.musical_attempts.load(Ordering::Acquire).to_le_bytes());
        let mut written = 0;
        if stream.write(audit.as_mut_ptr().cast(), 12, &mut written) == kResultOk && written == 12 {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IAudioProcessorTrait for GainProcessor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        let count = self.getBusCount(MediaTypes_::kAudio, BusDirections_::kInput);
        if num_ins != count || num_outs != 1 || inputs.is_null() || outputs.is_null() {
            return kResultFalse;
        }
        if slice::from_raw_parts(inputs, count as usize)
            .iter()
            .any(|value| *value != SpeakerArr::kStereo)
            || *outputs != SpeakerArr::kStereo
        {
            return kResultFalse;
        }

        kResultTrue
    }

    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        match dir as BusDirections {
            BusDirections_::kInput => {
                if index >= 0 && index < self.getBusCount(MediaTypes_::kAudio, dir) {
                    *arr = SpeakerArr::kStereo;
                    kResultOk
                } else {
                    kInvalidArgument
                }
            }
            BusDirections_::kOutput => {
                if index == 0 {
                    *arr = SpeakerArr::kStereo;
                    kResultOk
                } else {
                    kInvalidArgument
                }
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn canProcessSampleSize(&self, symbolic_sample_size: i32) -> tresult {
        match symbolic_sample_size as SymbolicSampleSizes {
            SymbolicSampleSizes_::kSample32 => kResultOk,
            SymbolicSampleSizes_::kSample64 => kNotImplemented,
            _ => kInvalidArgument,
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        self.configured_latency.load(Ordering::Acquire) as u32
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if setup.is_null() { return kInvalidArgument; }
        let rate = (*setup).sampleRate;
        if !rate.is_finite() || rate <= 0.0 { return kInvalidArgument; }
        self.sample_rate.store(rate.to_bits(), Ordering::Relaxed);
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        let data = &*data;
        let mode = self.mode.load(Ordering::Relaxed);
        let musical_tag = if mode == 7 {
            // A VST3 processor instance is not processed concurrently with itself.
            // Count attempts before any context/parameter/PCM validation, without
            // allocation, a lock, or any main-thread SDK call from DSP.
            let Some(attempts) = self.musical_attempts.load(Ordering::Relaxed).checked_add(1) else {
                return kResultFalse;
            };
            self.musical_attempts.store(attempts, Ordering::Release);
            let Some(tag) = musical_context_tag(data, self.sample_rate.load(Ordering::Relaxed)) else {
                return kInvalidArgument;
            };
            Some(tag)
        } else {
            None
        };
        let count = if mode == 2 { 2 } else { 3 };
        let timeline_start = if mode == 3 {
            if data.processContext.is_null() { return kInvalidArgument; }
            let context = &*data.processContext;
            if context.sampleRate != f64::from_bits(self.sample_rate.load(Ordering::Relaxed))
                || context.state != 0 || context.systemTime != 0 || context.continousTimeSamples != 0
                || context.projectTimeMusic != 0.0 || context.barPositionMusic != 0.0
                || context.cycleStartMusic != 0.0 || context.cycleEndMusic != 0.0
                || context.tempo != 0.0 || context.timeSigNumerator != 0 || context.timeSigDenominator != 0
                || context.samplesToNextClock != 0 || context.smpteOffsetSubframes != 0
                || context.projectTimeSamples.checked_add(i64::from(data.numSamples)).is_none()
            { return kInvalidArgument; }
            Some(context.projectTimeSamples)
        } else { None };
        let mask = ((1 << count) - 1) | (1 << 8);
        if data.numSamples < 0
            || data.numSamples > 65536
            || data.numInputs != count
            || data.numOutputs != 1
            || self.active_buses.load(Ordering::Relaxed) & mask != mask
            || data.symbolicSampleSize != SymbolicSampleSizes_::kSample32
            || data.inputs.is_null()
            || data.outputs.is_null()
        {
            return kInvalidArgument;
        }
        let inputs = slice::from_raw_parts(data.inputs, count as usize);
        let outputs = slice::from_raw_parts(data.outputs, 1);
        if inputs
            .iter()
            .chain(outputs)
            .any(|bus| bus.numChannels != 2 || bus.__field0.channelBuffers32.is_null())
        {
            return kInvalidArgument;
        }
        if inputs.iter().chain(outputs).any(|bus| {
            (0..2).any(|channel| (*bus.__field0.channelBuffers32.add(channel)).is_null())
        }) {
            return kInvalidArgument;
        }
        let mut gain = f64::from_bits(self.gain.load(Ordering::Relaxed));
        let mut position = self.position.load(Ordering::Relaxed);
        let mut write_frame = |frame: usize, gain: f64| {
            for channel in 0..2 {
                let mut value = 0.0;
                for (index, bus) in inputs.iter().enumerate() {
                    let pointer = *bus.__field0.channelBuffers32.add(channel);
                    let weight = match (mode, index) {
                        (_, 0) => 2.0,
                        (1, 1) => 5.0,
                        (1, _) => 3.0,
                        (_, 1) => 3.0,
                        _ => 5.0,
                    };
                    value += weight * *pointer.add(frame);
                }
                let pointer = *outputs[0].__field0.channelBuffers32.add(channel);
                let slot = position * 2 + channel;
                *pointer.add(frame) = f32::from_bits(
                    self.delay[slot].swap({
                    let tag = timeline_start.map_or(0.0, |start| ((start + frame as i64) % 251) as f32 / 1024.0);
                    let gain = if timeline_start.is_some() { gain as f32 + tag } else { gain as f32 };
                    (gain * value).to_bits()
                }, Ordering::Relaxed),
                );
                if let Some(tag) = musical_tag {
                    // Outside the original delay: the final partial native chunk
                    // must remain observable without an extra flush callback.
                    *pointer.add(frame) += if channel == 0 { tag } else { -tag };
                }
            }
            position = (position + 1) % self.configured_latency.load(Ordering::Relaxed);
        };
        let mut previous_offset = -1_i32;
        if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
            for index in 0..changes.getParameterCount() {
                let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else {
                    continue;
                };
                if queue.getParameterId() != 0 {
                    continue;
                }
                for index in 0..queue.getPointCount() {
                    let mut offset = 0;
                    let mut value = 0.0;
                    if queue.getPoint(index, &mut offset, &mut value) != kResultOk
                        || offset <= previous_offset
                        || offset < 0
                        || offset >= data.numSamples
                        || !value.is_finite()
                        || !(0.0..=1.0).contains(&value)
                    {
                        return kInvalidArgument;
                    }
                    for sample in (previous_offset + 1)..=offset {
                        let ratio = f64::from(sample - previous_offset)
                            / f64::from(offset - previous_offset);
                        write_frame(sample as usize, gain + (value - gain) * ratio);
                    }
                    previous_offset = offset;
                    gain = value;
                }
                break;
            }
        }
        for frame in (previous_offset + 1) as usize..data.numSamples as usize {
            write_frame(frame, gain);
        }
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
        self.position.store(position, Ordering::Relaxed);
        if mode == 5 && data.numSamples > 0 {
            // Configuration changes after actual DSP. Active delay remains three
            // until the host's later deactivate/reactivate. No main-thread SDK
            // interface or restartComponent is called from process().
            self.mode.store(6, Ordering::Release);
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        self.configured_latency.load(Ordering::Acquire) as u32
    }
}

impl IProcessContextRequirementsTrait for GainProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        // Requirements are instance-static and queried before state restore.
        // Asking for a field never makes an unavailable host fact valid.
        use IProcessContextRequirements_::Flags_ as R;
        (R::kNeedSystemTime | R::kNeedContinousTimeSamples | R::kNeedProjectTimeMusic
            | R::kNeedBarPositionMusic | R::kNeedCycleMusic | R::kNeedSamplesToNextClock
            | R::kNeedTempo | R::kNeedTimeSignature | R::kNeedChord | R::kNeedFrameRate
            | R::kNeedTransportState) as u32
    }
}

struct GainController {
    mode: Cell<u32>,
    gain: Cell<f64>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    host: RefCell<Option<ComPtr<IHostApplication>>>,
    connection: RefCell<Option<ComPtr<IConnectionPoint>>>,
    control_thread: Cell<Option<ThreadId>>,
}

impl Class for GainController {
    type Interfaces = (IEditController, IConnectionPoint);
}

impl GainController {
    const CID: TUID = uid(0x681E57F2, 0x2DB047A8, 0xB94A35C1, 0xA070213D);

    fn new() -> GainController {
        GainController {
            gain: Cell::new(0.5), mode: Cell::new(0),
            handler: RefCell::new(None), host: RefCell::new(None),
            connection: RefCell::new(None), control_thread: Cell::new(None),
        }
    }
}

impl IPluginBaseTrait for GainController {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        let Some(host) = ComRef::from_raw(context).and_then(|host| host.cast::<IHostApplication>()) else {
            return kInvalidArgument;
        };
        *self.host.borrow_mut() = Some(host);
        self.control_thread.set(Some(thread::current().id()));
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        self.connection.borrow_mut().take();
        self.handler.borrow_mut().take();
        self.host.borrow_mut().take();
        self.control_thread.set(None);
        kResultOk
    }
}

impl IConnectionPointTrait for GainController {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) { return kInvalidArgument; }
        let Some(other) = ComRef::from_raw(other) else { return kInvalidArgument; };
        let mut connection = self.connection.borrow_mut();
        if connection.is_some() { return kResultFalse; }
        *connection = Some(other.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, other: *mut IConnectionPoint) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) { return kInvalidArgument; }
        let mut connection = self.connection.borrow_mut();
        if !connection.as_ref().is_some_and(|peer| peer.as_ptr() == other) { return kInvalidArgument; }
        connection.take();
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult { kResultFalse }
}

impl IEditControllerTrait for GainController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        let Some((gain, mode)) = read_gain_state(state) else {
            return kResultFalse;
        };
        self.gain.set(gain);
        self.mode.set(mode);
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some((gain, mode)) = read_gain_state(state) else {
            return kResultFalse;
        };
        self.gain.set(gain);
        self.mode.set(mode);
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_gain_state(state, self.gain.get(), self.mode.get())
    }

    unsafe fn getParameterCount(&self) -> i32 {
        1
    }

    unsafe fn getParameterInfo(&self, param_index: i32, info: *mut ParameterInfo) -> tresult {
        match param_index {
            0 => {
                let info = &mut *info;

                info.id = 0;
                copy_wstring("Gain", &mut info.title);
                copy_wstring("Gain", &mut info.shortTitle);
                copy_wstring("", &mut info.units);
                info.stepCount = 0;
                info.defaultNormalizedValue = 0.5;
                info.unitId = 0;
                info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate;

                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getParamStringByValue(
        &self,
        id: u32,
        value_normalized: f64,
        string: *mut String128,
    ) -> tresult {
        let slice = unsafe { &mut *string };

        match id {
            0 => {
                let display = value_normalized.to_string();
                copy_wstring(&display, slice);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getParamValueByString(
        &self,
        id: u32,
        string: *mut TChar,
        value_normalized: *mut f64,
    ) -> tresult {
        match id {
            0 => {
                let len = len_wstring(string as *const TChar);
                if let Ok(string) =
                    String::from_utf16(slice::from_raw_parts(string as *const u16, len))
                {
                    if let Ok(value) = f64::from_str(&string) {
                        *value_normalized = value;
                        return kResultOk;
                    }
                }
                kInvalidArgument
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn normalizedParamToPlain(&self, id: u32, value_normalized: f64) -> f64 {
        match id {
            0 => value_normalized,
            _ => 0.0,
        }
    }

    unsafe fn plainParamToNormalized(&self, id: u32, plain_value: f64) -> f64 {
        match id {
            0 => plain_value,
            _ => 0.0,
        }
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        match id {
            0 => self.gain.get(),
            _ => 0.0,
        }
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        match id {
            0 => {
                if !value.is_finite() || !(0.0..=1.0).contains(&value) { return kInvalidArgument; }
                if self.mode.get() == 5 {
                    if self.control_thread.get() != Some(thread::current().id()) { return kInvalidArgument; }
                    let Some(host) = self.host.borrow().clone() else { return kResultFalse; };
                    let Some(peer) = self.connection.borrow().clone() else { return kResultFalse; };
                    let Some(handler) = self.handler.borrow().clone() else { return kResultFalse; };
                    let mut cid = IMessage_iid;
                    let mut iid = IMessage_iid;
                    let mut raw = ptr::null_mut();
                    let result = host.createInstance(&mut cid, &mut iid, &mut raw);
                    if result != kResultOk && result != kResultTrue { return result; }
                    let Some(message) = ComPtr::<IMessage>::from_raw(raw.cast()) else { return kResultFalse; };
                    message.setMessageID(c"mondrian.fixture.profile".as_ptr());
                    let delivered = peer.notify(message.as_ptr());
                    if delivered != kResultOk && delivered != kResultTrue { return delivered; }
                    let Some(attributes) = ComRef::from_raw(message.getAttributes()) else { return kResultFalse; };
                    let mut profile = 0_i64;
                    if attributes.getInt(c"profile".as_ptr(), &mut profile) != kResultOk
                        || !matches!(profile, 5 | 6) { return kResultFalse; }
                    if profile == 6 {
                        // This is evidence from the component, not a promise that
                        // setParamNormalized mutates its activation configuration.
                        self.mode.set(6);
                        self.gain.set(value);
                        return handler.restartComponent(RestartFlags_::kLatencyChanged);
                    }
                }
                if self.mode.get() == 4 && (self.gain.get() >= 0.75) != (value >= 0.75) {
                    if self.control_thread.get() != Some(thread::current().id()) { return kInvalidArgument; }
                    // Clone owning references before calling foreign code so callback
                    // reentry cannot encounter a live RefCell borrow or dangling pointer.
                    let Some(host) = self.host.borrow().clone() else { return kResultFalse; };
                    let Some(peer) = self.connection.borrow().clone() else { return kResultFalse; };
                    let Some(handler) = self.handler.borrow().clone() else { return kResultFalse; };
                    let mut cid = IMessage_iid;
                    let mut iid = IMessage_iid;
                    let mut raw = ptr::null_mut();
                    let result = host.createInstance(&mut cid, &mut iid, &mut raw);
                    if result != kResultOk && result != kResultTrue { return result; }
                    let Some(message) = ComPtr::<IMessage>::from_raw(raw.cast()) else { return kResultFalse; };
                    message.setMessageID(c"mondrian.fixture.quality".as_ptr());
                    let Some(attributes) = ComRef::from_raw(message.getAttributes()) else { return kResultFalse; };
                    if attributes.setFloat(c"quality".as_ptr(), value) != kResultOk { return kResultFalse; }
                    let delivered = peer.notify(message.as_ptr());
                    if delivered != kResultOk && delivered != kResultTrue { return delivered; }
                    self.gain.set(value);
                    // SDK kLatencyChanged requires deactivate/reactivate, after which
                    // getLatencySamples reports the new value. Never send this from process.
                    return handler.restartComponent(RestartFlags_::kLatencyChanged);
                }
                self.gain.set(value);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.borrow_mut() = ComRef::from_raw(handler).map(|handler| handler.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}

struct Factory {}

impl Class for Factory {
    type Interfaces = (IPluginFactory,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = &mut *info;

        copy_cstring("Vendor", &mut info.vendor);
        copy_cstring("https://example.com", &mut info.url);
        copy_cstring("someone@example.com", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;

        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        2
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        match index {
            0 => {
                let info = &mut *info;
                info.cid = GainProcessor::CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
                copy_cstring("Audio Module Class", &mut info.category);
                copy_cstring(PLUGIN_NAME, &mut info.name);

                kResultOk
            }
            1 => {
                let info = &mut *info;
                info.cid = GainController::CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
                copy_cstring("Component Controller Class", &mut info.category);
                copy_cstring(PLUGIN_NAME, &mut info.name);

                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let instance = match *(cid as *const TUID) {
            GainProcessor::CID => ComWrapper::new(GainProcessor::new()).to_com_ptr::<FUnknown>(),
            GainController::CID => ComWrapper::new(GainController::new()).to_com_ptr::<FUnknown>(),
            _ => None,
        };

        if let Some(instance) = instance {
            let ptr = instance.as_ptr();
            ((*(*ptr).vtbl).queryInterface)(ptr, iid as *mut TUID, obj)
        } else {
            kInvalidArgument
        }
    }
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[no_mangle]
extern "system" fn bundleEntry(_bundle_ref: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[no_mangle]
extern "system" fn bundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleExit() -> bool {
    true
}

#[no_mangle]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory {}).to_com_ptr::<IPluginFactory>().unwrap().into_raw()
}
