//! VST3 initial controller-value qualification fixture, adapted from the MIT/Apache-2.0
//! `vst3-rs` Gain example pinned by the Mondrian workspace.
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr, CString};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::{ptr, slice};

use std::thread::{self, ThreadId};
use vst3::{uid, Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

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

const PLUGIN_NAME: &str = "Mondrian Initial Values Fixture";
const STATE_MAGIC: [u8; 4] = *b"MIV1";
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
    (gain.is_finite() && (0.0..=1.0).contains(&gain) && mode <= 12).then_some((gain, mode))
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

unsafe fn write_audit(stream: *mut IBStream, bytes: &[u8]) -> tresult {
    let Some(stream) = ComRef::from_raw(stream) else {
        return kInvalidArgument;
    };
    let mut written = 0;
    if stream.write(
        bytes.as_ptr().cast_mut().cast(),
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

struct GainProcessor {
    mode: AtomicU32,
    active_buses: AtomicU32,
    process_calls: AtomicUsize,
    host: RefCell<Option<ComPtr<IHostApplication>>>,
    connection: RefCell<Option<ComPtr<IConnectionPoint>>>,
    control_thread: Cell<Option<ThreadId>>,
    gain: AtomicU64,
    sample_rate: AtomicU64,
}

impl Class for GainProcessor {
    type Interfaces = (
        IComponent,
        IAudioProcessor,
        IProcessContextRequirements,
        IConnectionPoint,
    );
}

impl GainProcessor {
    const CID: TUID = uid(0x61921CF4, 0x3A2B4A68, 0xAE5AE082, 0x41D85E22);

    fn new() -> GainProcessor {
        GainProcessor {
            gain: AtomicU64::new(0.5f64.to_bits()),
            sample_rate: AtomicU64::new(0),
            mode: AtomicU32::new(0),
            active_buses: AtomicU32::new(0),
            process_calls: AtomicUsize::new(0),
            host: RefCell::new(None),

            connection: RefCell::new(None),
            control_thread: Cell::new(None),
        }
    }
}

impl IPluginBaseTrait for GainProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        let Some(host) =
            ComRef::from_raw(_context).and_then(|host| host.cast::<IHostApplication>())
        else {
            return kInvalidArgument;
        };
        *self.host.borrow_mut() = Some(host);
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
        if self.control_thread.get() != Some(thread::current().id()) {
            return kInvalidArgument;
        }
        let Some(other) = ComRef::from_raw(other) else {
            return kInvalidArgument;
        };
        let mut connection = self.connection.borrow_mut();
        if connection.is_some() {
            return kResultFalse;
        }
        *connection = Some(other.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, other: *mut IConnectionPoint) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) {
            return kInvalidArgument;
        }
        let mut connection = self.connection.borrow_mut();
        if !connection.as_ref().is_some_and(|peer| peer.as_ptr() == other) {
            return kInvalidArgument;
        }
        connection.take();
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        kResultFalse
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
        if mediaType == MediaTypes_::kAudio
            && matches!(dir, BusDirections_::kInput | BusDirections_::kOutput)
        {
            1
        } else {
            0
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        direction: BusDirection,
        index: i32,
        output: *mut BusInfo,
    ) -> tresult {
        if output.is_null() || index != 0 || self.getBusCount(media_type, direction) != 1 {
            return kInvalidArgument;
        }
        let bus = &mut *output;
        bus.mediaType = media_type;
        bus.direction = direction;
        bus.channelCount = 2;
        copy_wstring("Main", &mut bus.name);
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
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
        let _ = state;
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
        let result = write_gain_state(
            state,
            f64::from_bits(self.gain.load(Ordering::Acquire)),
            self.mode.load(Ordering::Acquire),
        );
        if result != kResultOk {
            return result;
        }
        write_audit(
            state,
            &(self.process_calls.load(Ordering::Relaxed) as u32).to_le_bytes(),
        )
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
        0
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if setup.is_null() {
            return kInvalidArgument;
        }
        let rate = (*setup).sampleRate;
        if !rate.is_finite() || rate <= 0.0 {
            return kInvalidArgument;
        }
        self.sample_rate.store(rate.to_bits(), Ordering::Relaxed);
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        if _state == 0 {
            return kResultOk;
        }
        if self.control_thread.get() != Some(thread::current().id()) {
            return kInvalidArgument;
        }
        let Some(host) = self.host.borrow().clone() else {
            return kResultFalse;
        };
        let Some(peer) = self.connection.borrow().clone() else {
            return kResultFalse;
        };
        let mut cid = IMessage_iid;
        let mut iid = IMessage_iid;
        let mut raw = ptr::null_mut();
        if host.createInstance(&mut cid, &mut iid, &mut raw) != kResultOk {
            return kResultFalse;
        }
        let Some(message) = ComPtr::<IMessage>::from_raw(raw.cast()) else {
            return kResultFalse;
        };
        message.setMessageID(c"mondrian.fixture.initial-value-arm".as_ptr());
        peer.notify(message.as_ptr())
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        let data = &*data;
        if data.numSamples == 0 {
            // A real parameter-only Process may omit audio buses entirely.
            // Keep the old one-bus form legal too; never dereference PCM here.
            if data.symbolicSampleSize != SymbolicSampleSizes_::kSample32
                || !matches!(data.numInputs, 0 | 1)
                || !matches!(data.numOutputs, 0 | 1)
            {
                return kInvalidArgument;
            }
            let mut gain = f64::from_bits(self.gain.load(Ordering::Relaxed));
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                let queues = changes.getParameterCount();
                if !(0..=1024).contains(&queues) {
                    return kInvalidArgument;
                }
                for index in 0..queues {
                    let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else {
                        return kInvalidArgument;
                    };
                    let points = queue.getPointCount();
                    if queue.getParameterId() != 0 || !(0..=4096).contains(&points) {
                        return kInvalidArgument;
                    }
                    for point in 0..points {
                        let mut offset = 0;
                        let mut value = 0.0;
                        if queue.getPoint(point, &mut offset, &mut value) != kResultOk
                            || offset != 0
                            || !value.is_finite()
                            || !(0.0..=1.0).contains(&value)
                        {
                            return kInvalidArgument;
                        }
                        gain = value;
                    }
                }
            }
            self.gain.store(gain.to_bits(), Ordering::Relaxed);
            self.process_calls.fetch_add(1, Ordering::Relaxed);
            return kResultOk;
        }
        if data.numSamples < 0
            || data.numInputs != 1
            || data.numOutputs != 1
            || data.symbolicSampleSize != SymbolicSampleSizes_::kSample32
            || data.inputs.is_null()
            || data.outputs.is_null()
        {
            return kInvalidArgument;
        }
        let input = &*data.inputs;
        let output = &*data.outputs;
        if input.numChannels != 2
            || output.numChannels != 2
            || input.__field0.channelBuffers32.is_null()
            || output.__field0.channelBuffers32.is_null()
            || self.active_buses.load(Ordering::Relaxed) != 257
        {
            return kInvalidArgument;
        }
        let mut gain = f64::from_bits(self.gain.load(Ordering::Relaxed));
        let changes = ComRef::from_raw(data.inputParameterChanges);
        let queue = changes.and_then(|changes| {
            (0..changes.getParameterCount()).find_map(|index| {
                ComRef::from_raw(changes.getParameterData(index))
                    .filter(|queue| queue.getParameterId() == 0)
            })
        });
        let mut point = 0;
        for frame in 0..data.numSamples {
            if let Some(queue) = &queue {
                while point < queue.getPointCount() {
                    let mut offset = 0;
                    let mut value = 0.0;
                    if queue.getPoint(point, &mut offset, &mut value) != kResultOk
                        || offset < frame
                        || offset >= data.numSamples
                        || !value.is_finite()
                        || !(0.0..=1.0).contains(&value)
                    {
                        return kInvalidArgument;
                    }
                    if offset != frame {
                        break;
                    }
                    gain = value;
                    point += 1;
                }
            }
            for channel in 0..2 {
                let source = *input.__field0.channelBuffers32.add(channel);
                let destination = *output.__field0.channelBuffers32.add(channel);
                if source.is_null() || destination.is_null() {
                    return kInvalidArgument;
                }
                *destination.add(frame as usize) = *source.add(frame as usize) * gain as f32;
            }
        }
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
        self.process_calls.fetch_add(1, Ordering::Relaxed);
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for GainProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

struct GainController {
    armed: Cell<bool>,
    metadata_reads: Cell<u32>,
    value_reads: Cell<u32>,
    setters: Cell<u32>,
    last_observed: Cell<f64>,
    wrong_thread: Cell<bool>,
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
    const CID: TUID = uid(0xEC17903F, 0xF1DF4C58, 0x916B130B, 0xF5711940);

    fn new() -> GainController {
        GainController {
            armed: Cell::new(false),
            metadata_reads: Cell::new(0),
            value_reads: Cell::new(0),
            setters: Cell::new(0),
            last_observed: Cell::new(0.0),
            wrong_thread: Cell::new(false),
            gain: Cell::new(0.5),
            mode: Cell::new(0),
            handler: RefCell::new(None),
            host: RefCell::new(None),
            connection: RefCell::new(None),
            control_thread: Cell::new(None),
        }
    }
}

impl IPluginBaseTrait for GainController {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        let Some(host) = ComRef::from_raw(context).and_then(|host| host.cast::<IHostApplication>())
        else {
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
        if self.control_thread.get() != Some(thread::current().id()) {
            return kInvalidArgument;
        }
        let Some(other) = ComRef::from_raw(other) else {
            return kInvalidArgument;
        };
        let mut connection = self.connection.borrow_mut();
        if connection.is_some() {
            return kResultFalse;
        }
        *connection = Some(other.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, other: *mut IConnectionPoint) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) {
            return kInvalidArgument;
        }
        let mut connection = self.connection.borrow_mut();
        if !connection.as_ref().is_some_and(|peer| peer.as_ptr() == other) {
            return kInvalidArgument;
        }
        connection.take();
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) {
            self.wrong_thread.set(true);
            return kInvalidArgument;
        }
        let Some(message) = ComRef::from_raw(_message) else {
            return kInvalidArgument;
        };
        let id = message.getMessageID();
        if id.is_null() || CStr::from_ptr(id).to_bytes() != b"mondrian.fixture.initial-value-arm" {
            return kResultFalse;
        }
        self.armed.set(true);
        kResultOk
    }
}

impl IEditControllerTrait for GainController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) {
            self.wrong_thread.set(true);
            return kInvalidArgument;
        }
        let Some((gain, mode)) = read_gain_state(state) else {
            return kResultFalse;
        };
        self.gain.set(gain);
        self.mode.set(mode);
        if mode != 0 {
            let Some(handler) = self.handler.borrow().clone() else {
                return kResultFalse;
            };
            let flags = match mode {
                2 => 12,
                3 => 16,
                _ => 4,
            };
            return handler.restartComponent(flags);
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) {
            self.wrong_thread.set(true);
            return kInvalidArgument;
        }
        let Some((gain, mode)) = read_gain_state(state) else {
            return kResultFalse;
        };
        self.gain.set(gain);
        self.mode.set(mode);
        if mode != 0 {
            let Some(handler) = self.handler.borrow().clone() else {
                return kResultFalse;
            };
            let flags = match mode {
                2 => 12,
                3 => 16,
                _ => 4,
            };
            return handler.restartComponent(flags);
        }
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) {
            self.wrong_thread.set(true);
            return kInvalidArgument;
        }
        let result = write_gain_state(state, self.gain.get(), self.mode.get());
        if result != kResultOk {
            return result;
        }
        let mut audit = [0_u8; 24];
        audit[..4].copy_from_slice(&self.metadata_reads.get().to_le_bytes());
        audit[4..8].copy_from_slice(&self.value_reads.get().to_le_bytes());
        audit[8..12].copy_from_slice(&self.setters.get().to_le_bytes());
        audit[12..20].copy_from_slice(&self.last_observed.get().to_le_bytes());
        audit[20..24].copy_from_slice(&u32::from(self.wrong_thread.get()).to_le_bytes());
        write_audit(state, &audit)
    }

    unsafe fn getParameterCount(&self) -> i32 {
        if self.control_thread.get() != Some(thread::current().id()) {
            self.wrong_thread.set(true);
            return -1;
        }
        if self.armed.get() && self.mode.get() == 10 {
            1025
        } else if self.armed.get() && self.mode.get() == 11 {
            -1
        } else if self.armed.get() && self.mode.get() == 12 {
            0
        } else {
            1
        }
    }

    unsafe fn getParameterInfo(&self, param_index: i32, info: *mut ParameterInfo) -> tresult {
        if self.control_thread.get() != Some(thread::current().id()) {
            self.wrong_thread.set(true);
            return kInvalidArgument;
        }
        self.metadata_reads.set(self.metadata_reads.get() + 1);
        if param_index != 0 || info.is_null() || (self.armed.get() && self.mode.get() == 8) {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.id = if self.armed.get() && self.mode.get() == 7 {
            99
        } else {
            0
        };
        copy_wstring("Gain", &mut info.title);
        copy_wstring("Gain", &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = 0.5;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate;
        kResultOk
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
        if self.control_thread.get() != Some(thread::current().id()) {
            self.wrong_thread.set(true);
            return f64::NAN;
        }
        self.value_reads.set(self.value_reads.get() + 1);
        if self.armed.get() {
            if let Some(handler) = self.handler.borrow().clone() {
                if self.mode.get() == 6 {
                    handler.restartComponent(4);
                }
                if self.mode.get() == 9 {
                    handler.beginEdit(0);
                    handler.performEdit(0, 0.875);
                    handler.endEdit(0);
                }
            }
        }
        let value = match (self.armed.get(), self.mode.get(), id) {
            (true, 4, _) => f64::NAN,
            (true, 5, _) => 1.25,
            (_, _, 0) => self.gain.get(),
            _ => 0.0,
        };
        self.last_observed.set(value);
        value
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        self.setters.set(self.setters.get() + 1);
        if self.control_thread.get() != Some(thread::current().id()) {
            self.wrong_thread.set(true);
            return kInvalidArgument;
        }
        if id != 0 || !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return kInvalidArgument;
        }
        self.gain.set(value);
        kResultOk
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
