//! Stateful VST3 Gain qualification fixture, adapted from the MIT/Apache-2.0
//! `vst3-rs` Gain example pinned by the Mondrian workspace.
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::{c_char, c_void, CString};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{ptr, slice};

use vst3::{uid, Class, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

mod event_oracle;

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

const PLUGIN_NAME: &str = "Mondrian Stateful Gain Fixture";
const STATE_MAGIC: [u8; 4] = *b"MSG1";
const STATE_VERSION: u32 = 1;

unsafe fn read_gain_state(stream: *mut IBStream) -> Option<f64> {
    let stream = ComRef::from_raw(stream)?;
    let mut bytes = [0_u8; 16];
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
    (gain.is_finite() && (0.0..=1.0).contains(&gain)).then_some(gain)
}

unsafe fn write_gain_state(stream: *mut IBStream, gain: f64) -> tresult {
    let Some(stream) = ComRef::from_raw(stream) else {
        return kInvalidArgument;
    };
    let mut bytes = [0_u8; 16];
    bytes[..4].copy_from_slice(&STATE_MAGIC);
    bytes[4..8].copy_from_slice(&STATE_VERSION.to_le_bytes());
    bytes[8..16].copy_from_slice(&gain.to_le_bytes());
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
    gain: AtomicU64,
}

impl Class for GainProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl GainProcessor {
    const CID: TUID = uid(0xAE4C8B19, 0x10234F81, 0x9CA26D31, 0x5F8702A1);

    fn new() -> GainProcessor {
        GainProcessor { gain: AtomicU64::new(1.0f64.to_bits()) }
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
                BusDirections_::kInput => 1,
                BusDirections_::kOutput => 1,
                _ => 0,
            },
            MediaTypes_::kEvent => 0,
            _ => 0,
        }
    }

    unsafe fn getBusInfo(
        &self,
        mediaType: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        match mediaType as MediaTypes {
            MediaTypes_::kAudio => match dir as BusDirections {
                BusDirections_::kInput => match index {
                    0 => {
                        let bus = &mut *bus;

                        bus.mediaType = MediaTypes_::kAudio as MediaType;
                        bus.direction = BusDirections_::kInput as BusDirection;
                        bus.channelCount = 2;
                        copy_wstring("Input", &mut bus.name);
                        bus.busType = BusTypes_::kMain as BusType;
                        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;

                        kResultOk
                    }
                    _ => kInvalidArgument,
                },
                BusDirections_::kOutput => match index {
                    0 => {
                        let bus = &mut *bus;

                        bus.mediaType = MediaTypes_::kAudio as MediaType;
                        bus.direction = BusDirections_::kOutput as BusDirection;
                        bus.channelCount = 2;
                        copy_wstring("Output", &mut bus.name);
                        bus.busType = BusTypes_::kMain as BusType;
                        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32 as uint32;

                        kResultOk
                    }
                    _ => kInvalidArgument,
                },
                _ => kInvalidArgument,
            },
            MediaTypes_::kEvent => kInvalidArgument,
            _ => kInvalidArgument,
        }
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
        _media_type: MediaType,
        _dir: BusDirection,
        _index: i32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some(gain) = read_gain_state(state) else {
            return kResultFalse;
        };
        self.gain.store(gain.to_bits(), Ordering::Release);
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_gain_state(state, f64::from_bits(self.gain.load(Ordering::Acquire)))
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
        if num_ins != 1 || num_outs != 1 {
            return kResultFalse;
        }

        if *inputs != SpeakerArr::kStereo || *outputs != SpeakerArr::kStereo {
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
                if index == 0 {
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

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let process_data = &*data;

        let num_samples = process_data.numSamples as usize;

        if process_data.numInputs != 1 || process_data.numOutputs != 1 {
            return kResultOk;
        }

        let input_buses =
            slice::from_raw_parts(process_data.inputs, process_data.numInputs as usize);
        let output_buses =
            slice::from_raw_parts(process_data.outputs, process_data.numOutputs as usize);

        if input_buses[0].numChannels != 2 || output_buses[0].numChannels != 2 {
            return kResultOk;
        }

        let input_channels = slice::from_raw_parts(
            input_buses[0].__field0.channelBuffers32,
            input_buses[0].numChannels as usize,
        );
        let output_channels = slice::from_raw_parts(
            output_buses[0].__field0.channelBuffers32,
            output_buses[0].numChannels as usize,
        );

        let input_l = slice::from_raw_parts(input_channels[0], num_samples);
        let input_r = slice::from_raw_parts(input_channels[1], num_samples);
        let output_l = slice::from_raw_parts_mut(output_channels[0], num_samples);
        let output_r = slice::from_raw_parts_mut(output_channels[1], num_samples);

        // SDK queue values define linear segments, with the previous block's
        // final value at implicit sample -1. Process every point in one pass;
        // no curve implementation or per-process allocation belongs here.
        let mut gain = f64::from_bits(self.gain.load(Ordering::Relaxed));
        let mut previous_offset = -1_i32;
        let mut write_frame = |index: usize, value: f64| {
            output_l[index] = value as f32 * input_l[index];
            output_r[index] = value as f32 * input_r[index];
        };
        if let Some(param_changes) = ComRef::from_raw(process_data.inputParameterChanges) {
            for param_index in 0..param_changes.getParameterCount() {
                let Some(queue) = ComRef::from_raw(param_changes.getParameterData(param_index))
                else {
                    continue;
                };
                if queue.getParameterId() != 0 {
                    continue;
                }
                for point_index in 0..queue.getPointCount() {
                    let mut offset = 0;
                    let mut value = 0.0;
                    if queue.getPoint(point_index, &mut offset, &mut value) != kResultOk
                        || offset <= previous_offset
                        || offset < 0
                        || offset >= process_data.numSamples
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
        for sample in (previous_offset + 1) as usize..num_samples {
            write_frame(sample, gain);
        }
        self.gain.store(gain.to_bits(), Ordering::Relaxed);

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

// A separate, always-Float64 class. The original Gain class and its state/PCM
// behavior remain unchanged. Mode-specific signed half-ulp offsets witness
// native input/output conversion and the actual setup/process mode.
struct Float64GainProcessor {
    inner: GainProcessor,
    process_mode: std::sync::atomic::AtomicI32,
}

impl Float64GainProcessor {
    const CID: TUID = uid(0xB64C8B19, 0x10234F81, 0x9CA26D31, 0x5F8702A1);
    fn new() -> Self {
        Self {
            inner: GainProcessor::new(),
            process_mode: std::sync::atomic::AtomicI32::new(-1),
        }
    }
}

impl Class for Float64GainProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}
impl IPluginBaseTrait for Float64GainProcessor {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        self.inner.initialize(context)
    }
    unsafe fn terminate(&self) -> tresult {
        self.inner.terminate()
    }
}
impl IComponentTrait for Float64GainProcessor {
    unsafe fn getControllerClassId(&self, id: *mut TUID) -> tresult {
        self.inner.getControllerClassId(id)
    }
    unsafe fn setIoMode(&self, mode: IoMode) -> tresult {
        self.inner.setIoMode(mode)
    }
    unsafe fn getBusCount(&self, media: MediaType, direction: BusDirection) -> i32 {
        self.inner.getBusCount(media, direction)
    }
    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        direction: BusDirection,
        index: i32,
        info: *mut BusInfo,
    ) -> tresult {
        self.inner.getBusInfo(media, direction, index, info)
    }
    unsafe fn getRoutingInfo(&self, input: *mut RoutingInfo, output: *mut RoutingInfo) -> tresult {
        self.inner.getRoutingInfo(input, output)
    }
    unsafe fn activateBus(
        &self,
        media: MediaType,
        direction: BusDirection,
        index: i32,
        active: TBool,
    ) -> tresult {
        self.inner.activateBus(media, direction, index, active)
    }
    unsafe fn setActive(&self, active: TBool) -> tresult {
        self.inner.setActive(active)
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        self.inner.setState(state)
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        self.inner.getState(state)
    }
}
impl IAudioProcessorTrait for Float64GainProcessor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        input_count: i32,
        outputs: *mut SpeakerArrangement,
        output_count: i32,
    ) -> tresult {
        self.inner.setBusArrangements(inputs, input_count, outputs, output_count)
    }
    unsafe fn getBusArrangement(
        &self,
        direction: BusDirection,
        index: i32,
        arrangement: *mut SpeakerArrangement,
    ) -> tresult {
        self.inner.getBusArrangement(direction, index, arrangement)
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        match size as SymbolicSampleSizes {
            SymbolicSampleSizes_::kSample32 => kResultFalse,
            SymbolicSampleSizes_::kSample64 => kResultOk,
            _ => kInvalidArgument,
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        let Some(setup) = setup.as_ref() else {
            return kInvalidArgument;
        };
        if setup.symbolicSampleSize != SymbolicSampleSizes_::kSample64
            || !matches!(
                setup.processMode,
                ProcessModes_::kRealtime | ProcessModes_::kOffline
            )
            || setup.sampleRate != 48_000.0
            || setup.maxSamplesPerBlock != 4
        {
            return kInvalidArgument;
        }
        self.process_mode.store(setup.processMode, Ordering::Relaxed);
        kResultOk
    }
    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        self.inner.setProcessing(state)
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let Some(data) = data.as_ref() else {
            return kInvalidArgument;
        };
        if data.symbolicSampleSize != SymbolicSampleSizes_::kSample64
            || data.processMode != self.process_mode.load(Ordering::Relaxed)
            || !(0..=4).contains(&data.numSamples)
        {
            return kInvalidArgument;
        }
        let mut gain = f64::from_bits(self.inner.gain.load(Ordering::Relaxed));
        // Legal control-scoped zero-sample delivery has no audio buffers.
        if data.numSamples == 0 {
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                for index in 0..changes.getParameterCount() {
                    let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else {
                        return kInvalidArgument;
                    };
                    if queue.getParameterId() != 0 {
                        return kInvalidArgument;
                    }
                    for point in 0..queue.getPointCount() {
                        let mut offset = -1;
                        if queue.getPoint(point, &mut offset, &mut gain) != kResultOk
                            || offset != 0
                            || !gain.is_finite()
                            || !(0.0..=1.0).contains(&gain)
                        {
                            return kInvalidArgument;
                        }
                    }
                }
            }
            self.inner.gain.store(gain.to_bits(), Ordering::Relaxed);
            return kResultOk;
        }
        if data.numInputs != 1
            || data.numOutputs != 1
            || data.inputs.is_null()
            || data.outputs.is_null()
        {
            return kInvalidArgument;
        }
        let input = &*data.inputs;
        let output = &*data.outputs;
        if input.numChannels != 2
            || output.numChannels != 2
            || input.__field0.channelBuffers64.is_null()
            || output.__field0.channelBuffers64.is_null()
        {
            return kInvalidArgument;
        }
        let input_channels = slice::from_raw_parts(input.__field0.channelBuffers64, 2);
        let output_channels = slice::from_raw_parts(output.__field0.channelBuffers64, 2);
        if input_channels.iter().any(|p| p.is_null()) || output_channels.iter().any(|p| p.is_null())
        {
            return kInvalidArgument;
        }
        let frames = data.numSamples as usize;
        let input_l = slice::from_raw_parts(input_channels[0], frames);
        let input_r = slice::from_raw_parts(input_channels[1], frames);
        let output_l = slice::from_raw_parts_mut(output_channels[0], frames);
        let output_r = slice::from_raw_parts_mut(output_channels[1], frames);
        let bias: f64 = match data.processMode {
            ProcessModes_::kRealtime => 1.0 / 16_777_216.0,
            ProcessModes_::kOffline => 3.0 / 16_777_216.0,
            _ => return kInvalidArgument,
        };
        let transform = |sample: f64, gain: f64| {
            let shifted = if sample == 0.0 {
                sample
            } else {
                sample + bias.copysign(sample)
            };
            shifted * gain
        };
        let mut write_frame = |index: usize, value: f64| {
            output_l[index] = transform(input_l[index], value);
            output_r[index] = transform(input_r[index], value);
        };
        let mut previous_offset = -1_i32;
        if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
            for index in 0..changes.getParameterCount() {
                let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else {
                    return kInvalidArgument;
                };
                if queue.getParameterId() != 0 {
                    return kInvalidArgument;
                }
                for point in 0..queue.getPointCount() {
                    let mut offset = 0;
                    let mut value = 0.0;
                    if queue.getPoint(point, &mut offset, &mut value) != kResultOk
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
            }
        }
        for sample in (previous_offset + 1) as usize..frames {
            write_frame(sample, gain);
        }
        self.inner.gain.store(gain.to_bits(), Ordering::Relaxed);
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}
impl IProcessContextRequirementsTrait for Float64GainProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

struct GainController {
    gain: Cell<f64>,
}

impl Class for GainController {
    type Interfaces = (IEditController,);
}

impl GainController {
    const CID: TUID = uid(0xD52B40E7, 0x306A4A33, 0xB807F270, 0x1B4903C8);

    fn new() -> GainController {
        GainController { gain: Cell::new(1.0) }
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
        let Some(gain) = read_gain_state(state) else {
            return kResultFalse;
        };
        self.gain.set(gain);
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some(gain) = read_gain_state(state) else {
            return kResultFalse;
        };
        self.gain.set(gain);
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_gain_state(state, self.gain.get())
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
                info.defaultNormalizedValue = 1.0;
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
        5
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
            2 | 3 => event_oracle::class_info(index - 2, info),
            4 => {
                let info = &mut *info;
                info.cid = Float64GainProcessor::CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
                copy_cstring("Audio Module Class", &mut info.category);
                copy_cstring("Mondrian Float64 Gain Qualification", &mut info.name);
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
                Some(ComWrapper::new(GainProcessor::new()).to_com_ptr::<FUnknown>().unwrap())
            }
            GainController::CID => {
                Some(ComWrapper::new(GainController::new()).to_com_ptr::<FUnknown>().unwrap())
            }
            Float64GainProcessor::CID => {
                Some(ComWrapper::new(Float64GainProcessor::new()).to_com_ptr::<FUnknown>().unwrap())
            }
            cid => event_oracle::create_instance(cid),
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
