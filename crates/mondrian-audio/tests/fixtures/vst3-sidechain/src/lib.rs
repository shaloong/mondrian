//! VST3 sidechain qualification fixture, adapted from the MIT/Apache-2.0
//! `vst3-rs` Gain example pinned by the Mondrian workspace.
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr, CString};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
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
// One foreground queue with one four-byte component-profile notification. The
// host owns the allocation; this is a payload bound, not its complete resource fee.
const PROFILE_CONTEXT: u32 = 0x4d535335;
const PROFILE_BYTES: u32 = 4;

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
    (gain.is_finite() && (0.0..=1.0).contains(&gain) && mode <= 8).then_some((gain, mode))
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

// Independent canonical Scope oracle for zero=-19/7 seconds and duration=3/4.
// Literal bits were generated with Python Fraction, then one binary64 conversion:
// struct.unpack(">Q", struct.pack(">d", float((Fraction(s, r) + Fraction(19, 7))
//                                           / Fraction(3, 4))))[0].
// The admitted pairs are a finite domain, not an invocation-order/count audit.
fn canonical_musical_context(data: &ProcessData, prepared_rate: u64) -> Option<(i64, f32)> {
    if data.processContext.is_null() {
        return None;
    }
    // SAFETY: the caller owns this live context for this native callback only.
    let context = unsafe { &*data.processContext };
    let rate = if prepared_rate == 44_100_f64.to_bits() {
        0
    } else if prepared_rate == 48_000_f64.to_bits() {
        1
    } else {
        return None;
    };
    let row = match (context.projectTimeSamples, data.numSamples) {
        (-3, 4) => 0,
        (0, 4) => 1,
        (1, 2) => 2,
        (3, 4) => 3,
        (4, 2) => 4,
        (6, 4) => 5,
        (7, 1) => 6,
        (9, 4) => 7,
        (10, 1) | (10, 2) => 8,
        (12, 4) => 9,
        (13, 2) => 10,
        (15, 4) => 11,
        (16, 1) => 12,
        (19, 1) => 13,
        _ => return None,
    };
    if context.sampleRate.to_bits() != prepared_rate
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
    // Sorted starts: -3, 0, 1, 3, 4, 6, 7, 9, 10, 12, 13, 15, 16, 19.
    const QUARTER_NOTES: [[u64; 14]; 2] = [
        [
            0x400c_f39f_af02_4099,
            0x400c_f3cf_3cf3_cf3d,
            0x400c_f3df_16ee_fec9,
            0x400c_f3fe_cae5_5de1,
            0x400c_f40e_a4e0_8d6d,
            0x400c_f42e_58d6_ec85,
            0x400c_f43e_32d2_1c11,
            0x400c_f45d_e6c8_7b29,
            0x400c_f46d_c0c3_aab5,
            0x400c_f48d_74ba_09cc,
            0x400c_f49d_4eb5_3958,
            0x400c_f4bd_02ab_9870,
            0x400c_f4cc_dca6_c7fc,
            0x400c_f4fc_6a98_56a0,
        ],
        [
            0x400c_f3a3_8c24_4763,
            0x400c_f3cf_3cf3_cf3d,
            0x400c_f3dd_cd38_fc86,
            0x400c_f3fa_edc3_5717,
            0x400c_f409_7e08_845f,
            0x400c_f426_9e92_def0,
            0x400c_f435_2ed8_0c39,
            0x400c_f452_4f62_66ca,
            0x400c_f460_dfa7_9413,
            0x400c_f47e_0031_eea4,
            0x400c_f48c_9077_1bed,
            0x400c_f4a9_b101_767e,
            0x400c_f4b8_4146_a3c6,
            0x400c_f4e3_f216_2ba0,
        ],
    ];
    if context.state == 0 && context.projectTimeMusic.to_bits() == 0 && context.tempo.to_bits() == 0
    {
        Some((context.projectTimeSamples, 0.0))
    } else if context.state
        == (ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid
            | ProcessContext_::StatesAndFlags_::kTempoValid) as u32
        && context.projectTimeMusic.to_bits() == QUARTER_NOTES[rate][row]
        && context.tempo.to_bits() == 0x4054_0000_0000_0000
    {
        Some((context.projectTimeSamples, 80.0_f32 / 1024.0))
    } else {
        None
    }
}

struct GainProcessor {
    mode: AtomicU32,
    active: AtomicBool,
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
    // The factory records this object's stable COM identity without retaining a
    // self-reference. It remains valid until the component's final COM release.
    exchange_processor: AtomicPtr<IAudioProcessor>,
    // Lifecycle changes are main-only and cannot overlap process(). During DSP
    // this retained handler is borrowed read-only; no COM owner is created/dropped.
    exchange: RefCell<Option<ComPtr<IDataExchangeHandler>>>,
    profile_queue: AtomicU32,
    profile_notification_pending: AtomicBool,
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
            exchange_processor: AtomicPtr::new(ptr::null_mut()),
            exchange: RefCell::new(None),
            profile_queue: AtomicU32::new(InvalidDataExchangeQueueID),
            profile_notification_pending: AtomicBool::new(false),
            mode: AtomicU32::new(0),
            active: AtomicBool::new(false),
            active_buses: AtomicU32::new(0),
            delay: std::array::from_fn(|_| AtomicU32::new(0)),
            position: AtomicUsize::new(0),
            configured_latency: AtomicUsize::new(3),
            connection: RefCell::new(None),
            control_thread: Cell::new(None),
        }
    }

    unsafe fn open_profile_queue(&self) -> tresult {
        if self.control_thread.get() != Some(thread::current().id())
            || self.active.load(Ordering::Acquire) || self.connection.borrow().is_none()
        {
            return kInvalidArgument;
        }
        if self.profile_queue.load(Ordering::Acquire) != InvalidDataExchangeQueueID {
            return kResultOk;
        }
        let exchange = self.exchange.borrow();
        let Some(exchange) = exchange.as_ref() else { return kNoInterface; };
        let processor = self.exchange_processor.load(Ordering::Acquire);
        if processor.is_null() { return kInvalidArgument; }
        let mut queue = InvalidDataExchangeQueueID;
        let result = exchange.openQueue(processor, PROFILE_BYTES, 1, 4, PROFILE_CONTEXT, &mut queue);
        if result != kResultOk && result != kResultTrue { return result; }
        if queue == InvalidDataExchangeQueueID { return kInvalidArgument; }
        self.profile_queue.store(queue, Ordering::Release);
        kResultOk
    }

    unsafe fn close_profile_queue(&self) -> tresult {
        let queue = self.profile_queue.load(Ordering::Acquire);
        if queue == InvalidDataExchangeQueueID { return kResultOk; }
        if self.control_thread.get() != Some(thread::current().id())
            || self.active.load(Ordering::Acquire)
        { return kInvalidArgument; }
        let exchange = self.exchange.borrow();
        let Some(exchange) = exchange.as_ref() else { return kNoInterface; };
        let result = exchange.closeQueue(queue);
        if result != kResultOk && result != kResultTrue { return result; }
        self.profile_queue.store(InvalidDataExchangeQueueID, Ordering::Release);
        self.profile_notification_pending.store(false, Ordering::Release);
        kResultOk
    }

    unsafe fn send_profile_notification(&self) -> tresult {
        if !self.profile_notification_pending.load(Ordering::Acquire) { return kResultOk; }
        let exchange = self.exchange.borrow();
        let Some(exchange) = exchange.as_ref() else { return kNoInterface; };
        let queue = self.profile_queue.load(Ordering::Acquire);
        if queue == InvalidDataExchangeQueueID { return kInvalidArgument; }
        let mut block = DataExchangeBlock {
            data: ptr::null_mut(), size: 0, blockID: InvalidDataExchangeBlockID,
        };
        let result = exchange.lockBlock(queue, &mut block);
        // A temporarily unavailable block is not a blocking wait or an invented
        // notification. Keep the pending fact for a later actual DSP call.
        if result == kOutOfMemory { return kResultOk; }
        if result != kResultOk && result != kResultTrue { return result; }
        if block.blockID == InvalidDataExchangeBlockID { return kInvalidArgument; }
        if block.data.is_null() || block.size != PROFILE_BYTES {
            let _ = exchange.freeBlock(queue, block.blockID, 0);
            return kInvalidArgument;
        }
        ptr::copy_nonoverlapping(6_u32.to_le_bytes().as_ptr(), block.data.cast(), 4);
        let result = exchange.freeBlock(queue, block.blockID, 1);
        if result == kResultOk || result == kResultTrue {
            self.profile_notification_pending.store(false, Ordering::Release);
            kResultOk
        } else {
            result
        }
    }
}

impl IPluginBaseTrait for GainProcessor {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        self.control_thread.set(Some(thread::current().id()));
        *self.exchange.borrow_mut() = ComRef::from_raw(context)
            .and_then(|host| host.cast::<IDataExchangeHandler>());
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        let result = self.close_profile_queue();
        if result != kResultOk { return result; }
        self.exchange.borrow_mut().take();
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
        if state != 0 && self.active.load(Ordering::Acquire) { return kResultOk; }
        let mode = self.mode.load(Ordering::Acquire);
        let result = if state == 0 {
            // The owning host already stopped DSP. Complete this component's
            // inactive transition before closing its main-thread queue.
            self.active.store(false, Ordering::Release);
            self.close_profile_queue()
        } else if mode == 5 {
            self.open_profile_queue()
        } else { kResultOk };
        if result != kResultOk { return result; }
        if matches!(mode, 4 | 5 | 6) && state != 0 {
            let gain = f64::from_bits(self.gain.load(Ordering::Acquire));
            let latency = if mode == 6 || (mode == 4 && gain >= 0.75) { 7 } else { 3 };
            self.configured_latency.store(latency, Ordering::Release);
            for sample in &self.delay { sample.store(0, Ordering::Relaxed); }
            self.position.store(0, Ordering::Relaxed);
        }
        self.active.store(state != 0, Ordering::Release);
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
        if data.numSamples == 0 {
            // A normative parameter flush has no sample grid, audio-pointer or
            // musical-context access. Both absent I/O and the original declared
            // bus counts are valid; zero counts never require null pointers.
            let inputs = if mode == 2 { 2 } else { 3 };
            if (data.numInputs != 0 && data.numInputs != inputs)
                || (data.numOutputs != 0 && data.numOutputs != 1)
                || data.symbolicSampleSize != SymbolicSampleSizes_::kSample32
            {
                return kInvalidArgument;
            }
            let mut gain = f64::from_bits(self.gain.load(Ordering::Relaxed));
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                let count = changes.getParameterCount();
                if count < 0 {
                    return kInvalidArgument;
                }
                for index in 0..count {
                    let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else {
                        continue;
                    };
                    if queue.getParameterId() != 0 {
                        continue;
                    }
                    let count = queue.getPointCount();
                    if count < 0 {
                        return kInvalidArgument;
                    }
                    for index in 0..count {
                        let mut offset = 0;
                        let mut value = 0.0;
                        if queue.getPoint(index, &mut offset, &mut value) != kResultOk
                            || offset != 0
                            || !value.is_finite()
                            || !(0.0..=1.0).contains(&value)
                        {
                            return kInvalidArgument;
                        }
                        gain = value;
                    }
                    break;
                }
            }
            // Commit only the complete parameter input. No delay, position,
            // restart, output event or transport state advances without samples.
            self.gain.store(gain.to_bits(), Ordering::Relaxed);
            return kResultOk;
        }
        let canonical_music = if mode == 8 {
            if self.configured_latency.load(Ordering::Relaxed) != 3 {
                return kInvalidArgument;
            }
            let Some(context) =
                canonical_musical_context(data, self.sample_rate.load(Ordering::Relaxed))
            else {
                return kInvalidArgument;
            };
            Some(context)
        } else {
            None
        };
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
        } else { canonical_music.map(|(start, _)| start) };
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
                    let gain = if let Some((_, tempo)) = canonical_music { gain + tempo } else { gain };
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
            self.profile_notification_pending.store(true, Ordering::Release);
        }
        self.send_profile_notification()
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
    profile_queue_open: Cell<bool>,
}

impl Class for GainController {
    type Interfaces = (IEditController, IConnectionPoint, IDataExchangeReceiver);
}

impl GainController {
    const CID: TUID = uid(0x681E57F2, 0x2DB047A8, 0xB94A35C1, 0xA070213D);

    fn new() -> GainController {
        GainController {
            gain: Cell::new(0.5), mode: Cell::new(0),
            handler: RefCell::new(None), host: RefCell::new(None),
            connection: RefCell::new(None), control_thread: Cell::new(None),
            profile_queue_open: Cell::new(false),
        }
    }
}

impl IDataExchangeReceiverTrait for GainController {
    unsafe fn queueOpened(&self, context: u32, size: u32, background: *mut TBool) {
        if !background.is_null() { *background = 0; }
        if self.control_thread.get() == Some(thread::current().id())
            && context == PROFILE_CONTEXT && size == PROFILE_BYTES
        {
            self.profile_queue_open.set(true);
        }
    }

    unsafe fn queueClosed(&self, context: u32) {
        if self.control_thread.get() == Some(thread::current().id()) && context == PROFILE_CONTEXT {
            self.profile_queue_open.set(false);
        }
    }

    unsafe fn onDataExchangeBlocksReceived(
        &self, context: u32, count: u32, blocks: *mut DataExchangeBlock, background: TBool,
    ) {
        if self.control_thread.get() != Some(thread::current().id()) || background != 0
            || context != PROFILE_CONTEXT || !self.profile_queue_open.get()
            || count != 1 || blocks.is_null() || self.mode.get() != 5
        { return; }
        let block = &*blocks;
        if block.size != PROFILE_BYTES || block.data.is_null()
            || block.blockID == InvalidDataExchangeBlockID
            || u32::from_le_bytes(ptr::read_unaligned(block.data.cast::<[u8; 4]>())) != 6
        { return; }
        let Some(handler) = self.handler.borrow().clone() else { return; };
        // The component emitted this fact only after real DSP changed its desired
        // profile. Record it before foreign reentry so either this foreground
        // notification or the ordinary scalar-sync path requests restart once.
        self.mode.set(6);
        let _ = handler.restartComponent(RestartFlags_::kLatencyChanged);
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
        self.profile_queue_open.set(false);
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
            GainProcessor::CID => {
                let component = ComWrapper::new(GainProcessor::new());
                let processor = component.to_com_ptr::<IAudioProcessor>().unwrap();
                component.exchange_processor.store(processor.as_ptr(), Ordering::Release);
                component.to_com_ptr::<FUnknown>()
            },
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

#[cfg(test)]
mod zero_sample_contracts {
    use super::*;

    struct Queue(Vec<(i32, f64)>);
    impl Class for Queue {
        type Interfaces = (IParamValueQueue,);
    }
    impl IParamValueQueueTrait for Queue {
        unsafe fn getParameterId(&self) -> ParamID {
            0
        }
        unsafe fn getPointCount(&self) -> i32 {
            self.0.len() as i32
        }
        unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut f64) -> tresult {
            let Some(&(at, scalar)) = usize::try_from(index).ok().and_then(|i| self.0.get(i))
            else {
                return kInvalidArgument;
            };
            if offset.is_null() || value.is_null() {
                return kInvalidArgument;
            }
            *offset = at;
            *value = scalar;
            kResultOk
        }
        unsafe fn addPoint(&self, _offset: i32, _value: f64, _index: *mut i32) -> tresult {
            kNotImplemented
        }
    }
    struct Changes {
        queue: Option<ComPtr<IParamValueQueue>>,
        writes: Cell<usize>,
    }
    impl Class for Changes {
        type Interfaces = (IParameterChanges,);
    }
    impl IParameterChangesTrait for Changes {
        unsafe fn getParameterCount(&self) -> i32 {
            i32::from(self.queue.is_some())
        }
        unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
            if index == 0 {
                self.queue.as_ref().map_or(ptr::null_mut(), ComPtr::as_ptr)
            } else {
                ptr::null_mut()
            }
        }
        unsafe fn addParameterData(
            &self,
            _id: *const ParamID,
            _index: *mut i32,
        ) -> *mut IParamValueQueue {
            self.writes.set(self.writes.get() + 1);
            ptr::null_mut()
        }
    }
    struct Events(Cell<usize>);
    impl Class for Events {
        type Interfaces = (IEventList,);
    }
    impl IEventListTrait for Events {
        unsafe fn getEventCount(&self) -> i32 {
            0
        }
        unsafe fn getEvent(&self, _index: i32, _event: *mut Event) -> tresult {
            kInvalidArgument
        }
        unsafe fn addEvent(&self, _event: *mut Event) -> tresult {
            self.0.set(self.0.get() + 1);
            kResultOk
        }
    }
    fn changes(points: Vec<(i32, f64)>) -> ComPtr<IParameterChanges> {
        ComWrapper::new(Changes {
            queue: Some(ComWrapper::new(Queue(points)).to_com_ptr().unwrap()),
            writes: Cell::new(0),
        })
        .to_com_ptr()
        .unwrap()
    }
    fn data() -> ProcessData {
        ProcessData {
            processMode: ProcessModes_::kRealtime,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32,
            numSamples: 0,
            numInputs: 0,
            numOutputs: 0,
            inputs: ptr::null_mut(),
            outputs: ptr::null_mut(),
            inputParameterChanges: ptr::null_mut(),
            outputParameterChanges: ptr::null_mut(),
            inputEvents: ptr::null_mut(),
            outputEvents: ptr::null_mut(),
            processContext: ptr::null_mut(),
        }
    }
    fn dsp_state(processor: &GainProcessor) -> ([u32; 14], usize, u32, u64) {
        (
            std::array::from_fn(|i| processor.delay[i].load(Ordering::Relaxed)),
            processor.position.load(Ordering::Relaxed),
            processor.mode.load(Ordering::Relaxed),
            processor.musical_attempts.load(Ordering::Relaxed),
        )
    }

    #[test]
    fn zero_sample_process_uses_real_com_queues_without_advancing_dsp_or_emitting_output() {
        let wrapper = ComWrapper::new(GainProcessor::new());
        let processor = wrapper.to_com_ptr::<IAudioProcessor>().unwrap();
        wrapper.position.store(2, Ordering::Relaxed);
        wrapper.musical_attempts.store(17, Ordering::Relaxed);
        for (index, slot) in wrapper.delay.iter().enumerate() {
            slot.store(index as u32 + 1, Ordering::Relaxed);
        }
        let output = ComWrapper::new(Changes { queue: None, writes: Cell::new(0) });
        let output_parameters = output.to_com_ptr::<IParameterChanges>().unwrap();
        let events = ComWrapper::new(Events(Cell::new(0)));
        let output_events = events.to_com_ptr::<IEventList>().unwrap();
        for mode in 0..=8 {
            wrapper.mode.store(mode, Ordering::Relaxed);
            let before = dsp_state(&wrapper);
            let gain = wrapper.gain.load(Ordering::Relaxed);
            let mut block = data();
            block.outputParameterChanges = output_parameters.as_ptr();
            block.outputEvents = output_events.as_ptr();
            assert_eq!(unsafe { processor.process(&mut block) }, kResultOk);
            assert_eq!(wrapper.gain.load(Ordering::Relaxed), gain);
            let input = changes(vec![(0, 0.75)]);
            block.inputParameterChanges = input.as_ptr();
            assert_eq!(unsafe { processor.process(&mut block) }, kResultOk);
            assert_eq!(wrapper.gain.load(Ordering::Relaxed), 0.75_f64.to_bits());
            // Declared bus metadata remains legal with no audio samples. No
            // pointers or context are read, including non-null zero-count I/O.
            let mut untouched: [AudioBusBuffers; 3] = std::array::from_fn(|_| AudioBusBuffers {
                numChannels: 2,
                silenceFlags: 7,
                __field0: AudioBusBuffers__type0 { channelBuffers32: ptr::null_mut() },
            });
            block.inputs = untouched.as_mut_ptr();
            block.outputs = untouched.as_mut_ptr();
            assert_eq!(unsafe { processor.process(&mut block) }, kResultOk);
            block.numInputs = if mode == 2 { 2 } else { 3 };
            block.numOutputs = 1;
            assert_eq!(unsafe { processor.process(&mut block) }, kResultOk);
            assert!(untouched.iter().all(|bus| bus.silenceFlags == 7));
            assert_eq!(dsp_state(&wrapper), before);
            assert_eq!(output.writes.get(), 0);
            assert_eq!(events.0.get(), 0);
            for (offset, value) in [
                (-1, 0.25),
                (1, 0.25),
                (0, f64::NAN),
                (0, f64::INFINITY),
                (0, -0.25),
                (0, 1.25),
            ] {
                let input = changes(vec![(0, 0.25), (offset, value)]);
                block.inputParameterChanges = input.as_ptr();
                assert_eq!(unsafe { processor.process(&mut block) }, kInvalidArgument);
                assert_eq!(wrapper.gain.load(Ordering::Relaxed), 0.75_f64.to_bits());
                assert_eq!(dsp_state(&wrapper), before);
            }
            block.inputParameterChanges = ptr::null_mut();
            for count in [-1, 4] {
                block.numInputs = count;
                assert_eq!(unsafe { processor.process(&mut block) }, kInvalidArgument);
            }
        }
    }

    #[test]
    fn positive_process_preserves_bus_automation_and_delay_after_zero_sample_control() {
        let wrapper = ComWrapper::new(GainProcessor::new());
        let processor = wrapper.to_com_ptr::<IAudioProcessor>().unwrap();
        let input = changes(vec![(0, 0.25)]);
        let mut block = data();
        block.inputParameterChanges = input.as_ptr();
        assert_eq!(unsafe { processor.process(&mut block) }, kResultOk);
        let mut samples = [[0.0_f32; 4]; 6];
        samples[0] = [1.0, 2.0, 3.0, 4.0];
        samples[1] = [-1.0, -2.0, -3.0, -4.0];
        let mut pointers: [[*mut f32; 2]; 3] = std::array::from_fn(|bus| {
            [
                samples[bus * 2].as_mut_ptr(),
                samples[bus * 2 + 1].as_mut_ptr(),
            ]
        });
        let mut buses: [AudioBusBuffers; 3] = std::array::from_fn(|bus| AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 { channelBuffers32: pointers[bus].as_mut_ptr() },
        });
        let mut left = [99.0_f32; 4];
        let mut right = [99.0_f32; 4];
        let mut output_pointers = [left.as_mut_ptr(), right.as_mut_ptr()];
        let mut output = AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 { channelBuffers32: output_pointers.as_mut_ptr() },
        };
        let automated = changes(vec![(0, 0.5)]);
        block.numSamples = 4;
        block.numInputs = 3;
        block.numOutputs = 1;
        block.inputs = buses.as_mut_ptr();
        block.outputs = &mut output;
        block.inputParameterChanges = automated.as_ptr();
        wrapper.active_buses.store(0x107, Ordering::Relaxed);
        block.numInputs = 2;
        assert_eq!(unsafe { processor.process(&mut block) }, kInvalidArgument);
        block.numInputs = 3;
        assert_eq!(unsafe { processor.process(&mut block) }, kResultOk);
        assert_eq!(left, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(right, [0.0, 0.0, 0.0, -1.0]);
        assert_eq!(wrapper.gain.load(Ordering::Relaxed), 0.5_f64.to_bits());
        samples[0].copy_from_slice(&[5.0, 6.0, 7.0, 8.0]);
        samples[1].copy_from_slice(&[-5.0, -6.0, -7.0, -8.0]);
        block.inputParameterChanges = ptr::null_mut();
        assert_eq!(unsafe { processor.process(&mut block) }, kResultOk);
        assert_eq!(left, [2.0, 3.0, 4.0, 5.0]);
        assert_eq!(right, [-2.0, -3.0, -4.0, -5.0]);
    }
}


#[cfg(test)]
mod profile_notification_contracts {
    use super::*;
    use std::cell::UnsafeCell;
    use std::sync::Arc;

    // This minimal COM transport owns exactly one block. Tests join the actual
    // DSP thread before main dispatch/lifecycle work, matching the ABI ownership
    // phases; none of these Cell/UnsafeCell owners is accessed concurrently.
    struct ExchangeHost {
        owner: ThreadId,
        processor: *mut IAudioProcessor,
        receiver: ComPtr<IDataExchangeReceiver>,
        bytes: UnsafeCell<[u8; 4]>,
        open: Cell<bool>,
        locked: Cell<bool>,
        pending: Cell<bool>,
        busy: Cell<bool>,
        opens: Cell<usize>,
        closes: Cell<usize>,
        sends: Cell<usize>,
        returns: Cell<usize>,
        restarts: AtomicUsize,
        wrong_restart_thread: AtomicBool,
        dropped: Arc<AtomicBool>,
    }
    impl Class for ExchangeHost {
        type Interfaces = (IDataExchangeHandler, IComponentHandler);
    }
    impl ExchangeHost {
        unsafe fn dispatch(&self) {
            if self.pending.replace(false) {
                let mut block = DataExchangeBlock {
                    data: self.bytes.get().cast(),
                    size: PROFILE_BYTES,
                    blockID: 0,
                };
                self.receiver.onDataExchangeBlocksReceived(PROFILE_CONTEXT, 1, &mut block, 0);
            }
        }
    }
    impl Drop for ExchangeHost {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    impl IDataExchangeHandlerTrait for ExchangeHost {
        unsafe fn openQueue(
            &self,
            processor: *mut IAudioProcessor,
            size: u32,
            count: u32,
            alignment: u32,
            context: u32,
            out: *mut u32,
        ) -> tresult {
            if thread::current().id() != self.owner
                || processor != self.processor
                || size != 4
                || count != 1
                || alignment != 4
                || context != PROFILE_CONTEXT
                || out.is_null()
                || self.open.get()
            {
                return kInvalidArgument;
            }
            self.open.set(true);
            self.opens.set(self.opens.get() + 1);
            let mut background = 1;
            self.receiver.queueOpened(context, size, &mut background);
            if background != 0 {
                return kInvalidArgument;
            }
            *out = 9;
            kResultTrue
        }
        unsafe fn closeQueue(&self, queue: u32) -> tresult {
            if thread::current().id() != self.owner
                || queue != 9
                || !self.open.get()
                || self.locked.get()
            {
                return kInvalidArgument;
            }
            self.dispatch();
            self.receiver.queueClosed(PROFILE_CONTEXT);
            self.open.set(false);
            self.closes.set(self.closes.get() + 1);
            kResultTrue
        }
        unsafe fn lockBlock(&self, queue: u32, out: *mut DataExchangeBlock) -> tresult {
            if queue != 9 || !self.open.get() || out.is_null() {
                return kInvalidArgument;
            }
            if self.busy.get() || self.locked.get() || self.pending.get() {
                return kOutOfMemory;
            }
            self.locked.set(true);
            *out = DataExchangeBlock { data: self.bytes.get().cast(), size: 4, blockID: 0 };
            kResultTrue
        }
        unsafe fn freeBlock(&self, queue: u32, block: u32, send: TBool) -> tresult {
            if queue != 9 || block != 0 || !self.locked.replace(false) {
                return kInvalidArgument;
            }
            self.returns.set(self.returns.get() + 1);
            if send != 0 {
                self.pending.set(true);
                self.sends.set(self.sends.get() + 1);
            }
            kResultTrue
        }
    }
    impl IComponentHandlerTrait for ExchangeHost {
        unsafe fn beginEdit(&self, _: u32) -> tresult {
            kNotImplemented
        }
        unsafe fn performEdit(&self, _: u32, _: f64) -> tresult {
            kNotImplemented
        }
        unsafe fn endEdit(&self, _: u32) -> tresult {
            kNotImplemented
        }
        unsafe fn restartComponent(&self, flags: i32) -> tresult {
            if thread::current().id() != self.owner {
                self.wrong_restart_thread.store(true, Ordering::Release);
                return kInvalidArgument;
            }
            if flags != RestartFlags_::kLatencyChanged {
                return kInvalidArgument;
            }
            self.restarts.fetch_add(1, Ordering::AcqRel);
            kResultOk
        }
    }

    struct Fixture {
        component: ComWrapper<GainProcessor>,
        signal: ComPtr<IAudioProcessor>,
        controller: ComWrapper<GainController>,
        host: ComWrapper<ExchangeHost>,
    }
    impl Fixture {
        fn new() -> Self {
            let component = ComWrapper::new(GainProcessor::new());
            let signal = component.to_com_ptr::<IAudioProcessor>().unwrap();
            component.exchange_processor.store(signal.as_ptr(), Ordering::Release);
            let controller = ComWrapper::new(GainController::new());
            controller.control_thread.set(Some(thread::current().id()));
            controller.mode.set(5);
            let host = ComWrapper::new(ExchangeHost {
                owner: thread::current().id(),
                processor: signal.as_ptr(),
                receiver: controller.to_com_ptr::<IDataExchangeReceiver>().unwrap(),
                bytes: UnsafeCell::new([0; 4]),
                open: Cell::new(false),
                locked: Cell::new(false),
                pending: Cell::new(false),
                busy: Cell::new(false),
                opens: Cell::new(0),
                closes: Cell::new(0),
                sends: Cell::new(0),
                returns: Cell::new(0),
                restarts: AtomicUsize::new(0),
                wrong_restart_thread: AtomicBool::new(false),
                dropped: Arc::new(AtomicBool::new(false)),
            });
            unsafe {
                let base = host.to_com_ptr::<FUnknown>().unwrap();
                let control = component.to_com_ptr::<IComponent>().unwrap();
                assert_eq!(control.initialize(base.as_ptr()), kResultOk);
                let component_peer = component.to_com_ptr::<IConnectionPoint>().unwrap();
                let controller_peer = controller.to_com_ptr::<IConnectionPoint>().unwrap();
                assert_eq!(component_peer.connect(controller_peer.as_ptr()), kResultOk);
                assert_eq!(controller_peer.connect(component_peer.as_ptr()), kResultOk);
                component.mode.store(5, Ordering::Release);
                let handler = host.to_com_ptr::<IComponentHandler>().unwrap();
                let editor = controller.to_com_ptr::<IEditController>().unwrap();
                assert_eq!(editor.setComponentHandler(handler.as_ptr()), kResultOk);
                let mut inputs = [SpeakerArr::kStereo; 3];
                let mut output = SpeakerArr::kStereo;
                assert_eq!(
                    signal.setBusArrangements(inputs.as_mut_ptr(), 3, &mut output, 1),
                    kResultTrue
                );
                for bus in 0..3 {
                    assert_eq!(
                        control.activateBus(MediaTypes_::kAudio, BusDirections_::kInput, bus, 1),
                        kResultOk
                    );
                }
                assert_eq!(
                    control.activateBus(MediaTypes_::kAudio, BusDirections_::kOutput, 0, 1),
                    kResultOk
                );
                let mut setup = ProcessSetup {
                    processMode: ProcessModes_::kRealtime,
                    symbolicSampleSize: SymbolicSampleSizes_::kSample32,
                    maxSamplesPerBlock: 4,
                    sampleRate: 48000.0,
                };
                assert_eq!(signal.setupProcessing(&mut setup), kResultOk);
                assert_eq!(control.setActive(1), kResultOk);
                assert_eq!(signal.setProcessing(1), kResultOk);
            }
            Self { component, signal, controller, host }
        }
        fn process_without_host_points(&self) {
            let signal = self.signal.clone();
            // Real OS thread separation, not an onBackgroundThread flag oracle.
            thread::spawn(move || {
                let mut input = [[1.0_f32; 4]; 6];
                let mut input_pointers: [[*mut f32; 2]; 3] = std::array::from_fn(|bus| {
                    [input[bus * 2].as_mut_ptr(), input[bus * 2 + 1].as_mut_ptr()]
                });
                let mut inputs: [AudioBusBuffers; 3] = std::array::from_fn(|bus| AudioBusBuffers {
                    numChannels: 2,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: input_pointers[bus].as_mut_ptr(),
                    },
                });
                let mut samples = [[f32::NAN; 4]; 2];
                let mut output_pointers = [samples[0].as_mut_ptr(), samples[1].as_mut_ptr()];
                let mut output = AudioBusBuffers {
                    numChannels: 2,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: output_pointers.as_mut_ptr(),
                    },
                };
                let mut data = ProcessData {
                    processMode: ProcessModes_::kRealtime,
                    symbolicSampleSize: SymbolicSampleSizes_::kSample32,
                    numSamples: 4,
                    numInputs: 3,
                    numOutputs: 1,
                    inputs: inputs.as_mut_ptr(),
                    outputs: &mut output,
                    inputParameterChanges: ptr::null_mut(),
                    outputParameterChanges: ptr::null_mut(),
                    inputEvents: ptr::null_mut(),
                    outputEvents: ptr::null_mut(),
                    processContext: ptr::null_mut(),
                };
                assert_eq!(unsafe { signal.process(&mut data) }, kResultOk);
                assert!(samples.iter().flatten().all(|value| value.is_finite()));
            })
            .join()
            .unwrap();
        }
        fn retire(self) {
            let dropped = Arc::clone(&self.host.dropped);
            unsafe {
                let control = self.component.to_com_ptr::<IComponent>().unwrap();
                assert_eq!(self.signal.setProcessing(0), kResultOk);
                assert_eq!(control.setActive(0), kResultOk);
                for bus in 0..3 {
                    assert_eq!(
                        control.activateBus(MediaTypes_::kAudio, BusDirections_::kInput, bus, 0),
                        kResultOk
                    );
                }
                assert_eq!(
                    control.activateBus(MediaTypes_::kAudio, BusDirections_::kOutput, 0, 0),
                    kResultOk
                );
                let component_peer = self.component.to_com_ptr::<IConnectionPoint>().unwrap();
                let controller_peer = self.controller.to_com_ptr::<IConnectionPoint>().unwrap();
                assert_eq!(
                    component_peer.disconnect(controller_peer.as_ptr()),
                    kResultOk
                );
                assert_eq!(
                    controller_peer.disconnect(component_peer.as_ptr()),
                    kResultOk
                );
                assert_eq!(control.terminate(), kResultOk);
                assert_eq!(
                    self.controller.to_com_ptr::<IEditController>().unwrap().terminate(),
                    kResultOk
                );
            }
            assert!(!self.host.open.get());
            assert!(!self.host.locked.get());
            assert!(!self.host.pending.get());
            assert!(!self.controller.profile_queue_open.get());
            assert!(!self.host.wrong_restart_thread.load(Ordering::Acquire));
            assert_eq!(self.host.opens.get(), 1);
            assert_eq!(self.host.closes.get(), 1);
            drop(self);
            assert!(
                dropped.load(Ordering::Acquire),
                "no retained host/controller COM cycle"
            );
        }
    }

    #[test]
    fn native_owned_dsp_profile_notifies_main_once_without_host_parameter_points() {
        let fixture = Fixture::new();
        fixture.process_without_host_points();
        assert_eq!(fixture.component.mode.load(Ordering::Acquire), 6);
        assert_eq!(
            fixture.component.configured_latency.load(Ordering::Acquire),
            3
        );
        assert_eq!(
            unsafe { fixture.component.to_com_ptr::<IComponent>().unwrap().setActive(1) },
            kResultOk
        );
        assert_eq!(
            fixture.component.configured_latency.load(Ordering::Acquire),
            3,
            "repeated active does not realize the new latency without deactivation"
        );
        assert_eq!(fixture.host.opens.get(), 1);
        assert_eq!(
            fixture.component.gain.load(Ordering::Acquire),
            0.5_f64.to_bits()
        );
        assert_eq!(
            fixture.host.restarts.load(Ordering::Acquire),
            0,
            "no handler call on DSP thread"
        );
        assert_eq!(fixture.host.sends.get(), 1);
        assert_eq!(fixture.host.returns.get(), 1);
        assert_eq!(unsafe { *fixture.host.bytes.get() }, 6_u32.to_le_bytes());
        fixture.process_without_host_points();
        assert_eq!(
            fixture.host.sends.get(),
            1,
            "one notification, even before main dispatch"
        );
        unsafe {
            fixture.host.dispatch();
            fixture.host.dispatch();
        }
        assert_eq!(fixture.host.restarts.load(Ordering::Acquire), 1);
        assert_eq!(fixture.controller.mode.get(), 6);
        assert_eq!(fixture.controller.gain.get(), 0.5);
        unsafe {
            let receiver = fixture.controller.to_com_ptr::<IDataExchangeReceiver>().unwrap();
            let mut duplicate = DataExchangeBlock {
                data: fixture.host.bytes.get().cast(),
                size: 4,
                blockID: 0,
            };
            receiver.onDataExchangeBlocksReceived(PROFILE_CONTEXT, 1, &mut duplicate, 0);
            assert_eq!(
                fixture.host.restarts.load(Ordering::Acquire),
                1,
                "duplicate profile stays idempotent"
            );
        }
        unsafe {
            let editor = fixture.controller.to_com_ptr::<IEditController>().unwrap();
            assert_eq!(editor.setParamNormalized(0, 0.5), kResultOk);
            assert_eq!(
                fixture.host.restarts.load(Ordering::Acquire),
                1,
                "later scalar sync stays idempotent"
            );
            let control = fixture.component.to_com_ptr::<IComponent>().unwrap();
            assert_eq!(fixture.signal.setProcessing(0), kResultOk);
            assert_eq!(control.setActive(0), kResultOk);
            assert_eq!(control.setActive(1), kResultOk);
            assert_eq!(fixture.signal.setProcessing(1), kResultOk);
        }
        assert_eq!(
            fixture.component.configured_latency.load(Ordering::Acquire),
            7
        );
        fixture.retire();
    }

    #[test]
    fn foreground_profile_receiver_rejects_wrong_os_thread_background_shape_and_late_delivery() {
        let fixture = Fixture::new();
        let receiver = fixture.controller.to_com_ptr::<IDataExchangeReceiver>().unwrap();
        let other_thread_receiver = receiver.clone();
        thread::spawn(move || {
            let mut value = 6_u32.to_le_bytes();
            let mut block = DataExchangeBlock {
                data: value.as_mut_ptr().cast(),
                size: 4,
                blockID: 0,
            };
            unsafe {
                other_thread_receiver.onDataExchangeBlocksReceived(
                    PROFILE_CONTEXT,
                    1,
                    &mut block,
                    0,
                );
            }
        })
        .join()
        .unwrap();
        let mut value = 6_u32.to_le_bytes();
        let mut block = DataExchangeBlock {
            data: value.as_mut_ptr().cast(),
            size: 4,
            blockID: 0,
        };
        unsafe {
            receiver.onDataExchangeBlocksReceived(PROFILE_CONTEXT, 1, &mut block, 1);
            receiver.onDataExchangeBlocksReceived(PROFILE_CONTEXT + 1, 1, &mut block, 0);
            receiver.onDataExchangeBlocksReceived(PROFILE_CONTEXT, 0, &mut block, 0);
            receiver.onDataExchangeBlocksReceived(PROFILE_CONTEXT, 1, ptr::null_mut(), 0);
            block.size = 3;
            receiver.onDataExchangeBlocksReceived(PROFILE_CONTEXT, 1, &mut block, 0);
            assert_eq!(fixture.signal.setProcessing(0), kResultOk);
            assert_eq!(
                fixture.component.to_com_ptr::<IComponent>().unwrap().setActive(0),
                kResultOk
            );
            block.size = 4;
            receiver.onDataExchangeBlocksReceived(PROFILE_CONTEXT, 1, &mut block, 0);
        }
        assert_eq!(fixture.host.restarts.load(Ordering::Acquire), 0);
        assert_eq!(fixture.controller.mode.get(), 5);
        drop(receiver);
        fixture.retire();
    }

    #[test]
    fn unavailable_profile_block_keeps_real_change_pending_and_close_retires_notification() {
        let fixture = Fixture::new();
        fixture.host.busy.set(true);
        fixture.process_without_host_points();
        assert_eq!(fixture.component.mode.load(Ordering::Acquire), 6);
        assert!(fixture.component.profile_notification_pending.load(Ordering::Acquire));
        assert_eq!(fixture.host.sends.get(), 0);
        assert_eq!(fixture.host.restarts.load(Ordering::Acquire), 0);
        fixture.host.busy.set(false);
        fixture.process_without_host_points();
        assert!(!fixture.component.profile_notification_pending.load(Ordering::Acquire));
        assert_eq!(fixture.host.sends.get(), 1);
        assert_eq!(fixture.host.returns.get(), 1);
        // No explicit dispatch: legal queue close delivers the pending foreground
        // notification before queueClosed, exactly as the existing host does.
        assert_eq!(unsafe { fixture.signal.setProcessing(0) }, kResultOk);
        assert_eq!(
            unsafe { fixture.component.to_com_ptr::<IComponent>().unwrap().setActive(0) },
            kResultOk
        );
        assert_eq!(fixture.host.restarts.load(Ordering::Acquire), 1);
        assert!(!fixture.host.pending.get());
        assert!(!fixture.controller.profile_queue_open.get());
        fixture.retire();
    }
}
