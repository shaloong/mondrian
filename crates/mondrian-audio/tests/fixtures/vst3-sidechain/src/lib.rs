//! VST3 sidechain qualification fixture, adapted from the MIT/Apache-2.0
//! `vst3-rs` Gain example pinned by the Mondrian workspace.
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::{c_char, c_void, CString};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::{ptr, slice};

use vst3::{uid, Class, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

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
    (gain.is_finite() && (0.0..=1.0).contains(&gain) && mode <= 3).then_some((gain, mode))
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

struct GainProcessor {
    mode: AtomicU32,
    active_buses: AtomicU32,
    delay: [AtomicU32; 6],
    position: AtomicUsize,
    gain: AtomicU64,
    sample_rate: AtomicU64,
}

impl Class for GainProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl GainProcessor {
    const CID: TUID = uid(0x7486AF22, 0x234A480B, 0x9DF13173, 0xF12EB620);

    fn new() -> GainProcessor {
        GainProcessor {
            gain: AtomicU64::new(0.5f64.to_bits()),
            sample_rate: AtomicU64::new(0),
            mode: AtomicU32::new(0),
            active_buses: AtomicU32::new(0),
            delay: std::array::from_fn(|_| AtomicU32::new(0)),
            position: AtomicUsize::new(0),
        }
    }
}

impl IPluginBaseTrait for GainProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
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

    unsafe fn setActive(&self, _state: TBool) -> tresult {
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
        write_gain_state(
            state,
            f64::from_bits(self.gain.load(Ordering::Acquire)),
            self.mode.load(Ordering::Acquire),
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
        3
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
            }
            position = (position + 1) % 3;
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
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        3
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
}

impl Class for GainController {
    type Interfaces = (IEditController,);
}

impl GainController {
    const CID: TUID = uid(0x681E57F2, 0x2DB047A8, 0xB94A35C1, 0xA070213D);

    fn new() -> GainController {
        GainController { gain: Cell::new(0.5), mode: Cell::new(0) }
    }
}

impl IPluginBaseTrait for GainController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
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
                self.gain.set(value);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
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
