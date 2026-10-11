//! Owned Windows VST3 editor fixture. The processor and controller are ordinary
//! separate VST3 objects; the IPlugView shares its actual controller state.
//! Adapted from the workspace's MIT/Apache-2.0 owned VST3 sidechain fixture.
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use vst3::{uid, Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

const PLUGIN_NAME: &str = "Mondrian Owned Native Editor Fixture";
const STATE_MAGIC: [u8; 4] = *b"MEV1";
const PROBE_BASE: u32 = 100;
const PROBE_COUNT: u32 = 8;

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let usable = dst.len().saturating_sub(1);
    for (source, target) in src.bytes().take(usable).zip(dst.iter_mut()) {
        *target = source as c_char;
    }
    if let Some(last) = dst.get_mut(src.len().min(usable)) {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let usable = dst.len().saturating_sub(1);
    let mut written = 0;
    for (source, target) in src.encode_utf16().take(usable).zip(dst.iter_mut()) {
        *target = source as TChar;
        written += 1;
    }
    if let Some(last) = dst.get_mut(written) {
        *last = 0;
    }
}

#[derive(Clone, Copy)]
struct Configuration {
    gain: f64,
    balance: f64,
    mode: u32,
    opaque_nonce: u32,
}

unsafe fn read_configuration(stream: *mut IBStream) -> Option<Configuration> {
    let stream = ComRef::from_raw(stream)?;
    let mut bytes = [0_u8; 32];
    let mut read = 0;
    if stream.read(bytes.as_mut_ptr().cast(), 32, &mut read) != kResultOk
        || read != 32
        || bytes[..4] != STATE_MAGIC
        || u32::from_le_bytes(bytes[4..8].try_into().ok()?) != 1
    {
        return None;
    }
    let value = Configuration {
        gain: f64::from_le_bytes(bytes[8..16].try_into().ok()?),
        balance: f64::from_le_bytes(bytes[16..24].try_into().ok()?),
        mode: u32::from_le_bytes(bytes[24..28].try_into().ok()?),
        opaque_nonce: u32::from_le_bytes(bytes[28..32].try_into().ok()?),
    };
    (value.gain.is_finite()
        && (0.0..=1.0).contains(&value.gain)
        && value.balance.is_finite()
        && (0.0..=1.0).contains(&value.balance)
        && value.mode <= 2)
        .then_some(value)
}

unsafe fn write_configuration(stream: *mut IBStream, value: Configuration) -> tresult {
    let Some(stream) = ComRef::from_raw(stream) else {
        return kInvalidArgument;
    };
    let mut bytes = [0_u8; 32];
    bytes[..4].copy_from_slice(&STATE_MAGIC);
    bytes[4..8].copy_from_slice(&1_u32.to_le_bytes());
    bytes[8..16].copy_from_slice(&value.gain.to_le_bytes());
    bytes[16..24].copy_from_slice(&value.balance.to_le_bytes());
    bytes[24..28].copy_from_slice(&value.mode.to_le_bytes());
    bytes[28..32].copy_from_slice(&value.opaque_nonce.to_le_bytes());
    let mut written = 0;
    if stream.write(bytes.as_mut_ptr().cast(), 32, &mut written) == kResultOk && written == 32 {
        kResultOk
    } else {
        kResultFalse
    }
}

struct EditorProcessor {
    gain: AtomicU64,
    balance: AtomicU64,
    mode: AtomicU32,
    nonce: AtomicU32,
}
impl Class for EditorProcessor {
    type Interfaces = (IComponent, IAudioProcessor);
}
impl EditorProcessor {
    const CID: TUID = uid(0xC48B1B72, 0x4CDE4A91, 0xA2C9C4EB, 0xB7DD1E83);
    fn new() -> Self {
        Self {
            gain: AtomicU64::new(0.5_f64.to_bits()),
            balance: AtomicU64::new(0.5_f64.to_bits()),
            mode: AtomicU32::new(0),
            nonce: AtomicU32::new(0),
        }
    }
}
impl IPluginBaseTrait for EditorProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}
impl IComponentTrait for EditorProcessor {
    unsafe fn getControllerClassId(&self, cid: *mut TUID) -> tresult {
        if cid.is_null() {
            return kInvalidArgument;
        }
        *cid = EditorController::CID;
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media: MediaType, direction: BusDirection) -> i32 {
        i32::from(
            media == MediaTypes_::kAudio
                && matches!(direction, BusDirections_::kInput | BusDirections_::kOutput),
        )
    }
    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        direction: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if bus.is_null() || index != 0 || self.getBusCount(media, direction) != 1 {
            return kInvalidArgument;
        }
        let bus = &mut *bus;
        bus.mediaType = media;
        bus.direction = direction;
        bus.channelCount = 2;
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        copy_wstring("Main", &mut bus.name);
        kResultOk
    }
    unsafe fn getRoutingInfo(
        &self,
        _input: *mut RoutingInfo,
        _output: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(
        &self,
        media: MediaType,
        direction: BusDirection,
        index: i32,
        _state: TBool,
    ) -> tresult {
        if index == 0 && self.getBusCount(media, direction) == 1 {
            kResultOk
        } else {
            kInvalidArgument
        }
    }
    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        let Some(value) = read_configuration(stream) else {
            return kResultFalse;
        };
        self.gain.store(value.gain.to_bits(), Ordering::Release);
        self.balance.store(value.balance.to_bits(), Ordering::Release);
        self.mode.store(value.mode, Ordering::Release);
        self.nonce.store(value.opaque_nonce, Ordering::Release);
        kResultOk
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        write_configuration(
            stream,
            Configuration {
                gain: f64::from_bits(self.gain.load(Ordering::Acquire)),
                balance: f64::from_bits(self.balance.load(Ordering::Acquire)),
                mode: self.mode.load(Ordering::Acquire),
                opaque_nonce: self.nonce.load(Ordering::Acquire),
            },
        )
    }
}
impl IAudioProcessorTrait for EditorProcessor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        input_count: i32,
        outputs: *mut SpeakerArrangement,
        output_count: i32,
    ) -> tresult {
        if input_count == 1
            && output_count == 1
            && !inputs.is_null()
            && !outputs.is_null()
            && *inputs == SpeakerArr::kStereo
            && *outputs == SpeakerArr::kStereo
        {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getBusArrangement(
        &self,
        direction: BusDirection,
        index: i32,
        value: *mut SpeakerArrangement,
    ) -> tresult {
        if value.is_null()
            || index != 0
            || !matches!(direction, BusDirections_::kInput | BusDirections_::kOutput)
        {
            return kInvalidArgument;
        }
        *value = SpeakerArr::kStereo;
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if setup.is_null()
            || !(*setup).sampleRate.is_finite()
            || (*setup).sampleRate <= 0.0
            || (*setup).maxSamplesPerBlock <= 0
        {
            kInvalidArgument
        } else {
            kResultOk
        }
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        let data = &*data;
        if data.numSamples == 0 {
            if data.numInputs != 0
                || data.numOutputs != 0
                || data.symbolicSampleSize != SymbolicSampleSizes_::kSample32
            {
                return kInvalidArgument;
            }
            let mut next = [
                f64::from_bits(self.gain.load(Ordering::Acquire)),
                f64::from_bits(self.balance.load(Ordering::Acquire)),
            ];
            let mut seen = [false; 2];
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                if !(0..=2).contains(&changes.getParameterCount()) {
                    return kInvalidArgument;
                }
                for index in 0..changes.getParameterCount() {
                    let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else {
                        return kInvalidArgument;
                    };
                    let id = queue.getParameterId() as usize;
                    if id >= 2 || seen[id] || !(0..=128).contains(&queue.getPointCount()) {
                        return kInvalidArgument;
                    }
                    seen[id] = true;
                    for point in 0..queue.getPointCount() {
                        let mut offset = 0;
                        let mut value = 0.0;
                        if queue.getPoint(point, &mut offset, &mut value) != kResultOk
                            || offset != 0
                            || !value.is_finite()
                            || !(0.0..=1.0).contains(&value)
                        {
                            return kInvalidArgument;
                        }
                        next[id] = value;
                    }
                }
            }
            // Both values commit only after the entire zero-sample batch is
            // valid. No audio pointers, sample counter or lifecycle is touched.
            self.gain.store(next[0].to_bits(), Ordering::Release);
            self.balance.store(next[1].to_bits(), Ordering::Release);
            return kResultOk;
        }
        if data.numSamples < 0
            || data.numSamples > 65536
            || data.numInputs != 1
            || data.numOutputs != 1
            || data.inputs.is_null()
            || data.outputs.is_null()
            || data.symbolicSampleSize != SymbolicSampleSizes_::kSample32
        {
            return kInvalidArgument;
        }
        let input = &*data.inputs;
        let output = &*data.outputs;
        if input.numChannels != 2
            || output.numChannels != 2
            || input.__field0.channelBuffers32.is_null()
            || output.__field0.channelBuffers32.is_null()
        {
            return kInvalidArgument;
        }
        for channel in 0..2 {
            if (*input.__field0.channelBuffers32.add(channel)).is_null()
                || (*output.__field0.channelBuffers32.add(channel)).is_null()
            {
                return kInvalidArgument;
            }
        }
        let mut values = [
            f64::from_bits(self.gain.load(Ordering::Relaxed)),
            f64::from_bits(self.balance.load(Ordering::Relaxed)),
        ];
        let mut queues: [Option<ComRef<'_, IParamValueQueue>>; 2] = [None, None];
        if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
            for index in 0..changes.getParameterCount() {
                if let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) {
                    let id = queue.getParameterId() as usize;
                    if id < 2 {
                        if queues[id].is_some() || !(0..=128).contains(&queue.getPointCount()) {
                            return kInvalidArgument;
                        }
                        queues[id] = Some(queue);
                    }
                }
            }
        }
        let mut positions = [0_i32; 2];
        let mut last_offsets = [-1_i32; 2];
        for sample in 0..data.numSamples {
            for id in 0..2 {
                if let Some(queue) = queues[id] {
                    while positions[id] < queue.getPointCount() {
                        let mut offset = 0;
                        let mut value = 0.0;
                        if queue.getPoint(positions[id], &mut offset, &mut value) != kResultOk
                            || offset <= last_offsets[id]
                            || offset < 0
                            || offset >= data.numSamples
                            || !value.is_finite()
                            || !(0.0..=1.0).contains(&value)
                        {
                            return kInvalidArgument;
                        }
                        if offset > sample {
                            break;
                        }
                        values[id] = value;
                        positions[id] += 1;
                        last_offsets[id] = offset;
                    }
                }
            }
            for channel in 0..2 {
                let coefficient = if channel == 0 {
                    2.0 * (1.0 - values[1])
                } else {
                    2.0 * values[1]
                };
                *(*output.__field0.channelBuffers32.add(channel)).add(sample as usize) =
                    (*(*input.__field0.channelBuffers32.add(channel)).add(sample as usize) as f64
                        * values[0]
                        * coefficient) as f32;
            }
        }
        self.gain.store(values[0].to_bits(), Ordering::Relaxed);
        self.balance.store(values[1].to_bits(), Ordering::Relaxed);
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

struct ControllerState {
    gain: Cell<f64>,
    balance: Cell<f64>,
    mode: Cell<u32>,
    nonce: Cell<u32>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    frame: RefCell<Option<ComPtr<IPlugFrame>>>,
    probes: [Cell<u32>; (PROBE_COUNT - 1) as usize],
    wrong_thread: AtomicU32,
    main: std::thread::ThreadId,
}
impl ControllerState {
    fn new() -> Self {
        Self {
            gain: Cell::new(0.5),
            balance: Cell::new(0.5),
            mode: Cell::new(0),
            nonce: Cell::new(0),
            handler: RefCell::new(None),
            frame: RefCell::new(None),
            probes: std::array::from_fn(|_| Cell::new(0)),
            wrong_thread: AtomicU32::new(0),
            main: std::thread::current().id(),
        }
    }
    fn main_thread(&self) -> bool {
        let current = self.main == std::thread::current().id();
        if !current {
            // Refuse before touching controller Cell/RefCell state. The sole
            // cross-thread diagnostic itself is atomic and allocation-free.
            let _ = self.wrong_thread.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(1))
            });
        }
        current
    }
    fn bump(&self, index: usize) {
        let value = &self.probes[index];
        value.set(value.get().saturating_add(1));
    }
    fn configuration(&self) -> Configuration {
        Configuration {
            gain: self.gain.get(),
            balance: self.balance.get(),
            mode: self.mode.get(),
            opaque_nonce: self.nonce.get(),
        }
    }
    fn restore(&self, value: Configuration) {
        self.gain.set(value.gain);
        self.balance.set(value.balance);
        self.mode.set(value.mode);
        self.nonce.set(value.opaque_nonce);
    }
    unsafe fn edit(&self, grouped: bool) -> tresult {
        if !self.main_thread() {
            return kResultFalse;
        }
        let handler = self.handler.borrow().clone();
        let Some(handler) = handler else {
            return kResultFalse;
        };
        let handler2 = handler.cast::<IComponentHandler2>();
        if grouped && handler2.as_ref().is_none_or(|value| value.startGroupEdit() != kResultOk) {
            return kResultFalse;
        }
        for (id, value) in if grouped {
            &[(0_u32, 0.75), (1, 0.25)][..]
        } else {
            &[(0_u32, 0.75)][..]
        } {
            if handler.beginEdit(*id) != kResultOk {
                return kResultFalse;
            }
            if *id == 0 {
                self.gain.set(*value);
            } else {
                self.balance.set(*value);
            }
            if handler.performEdit(*id, *value) != kResultOk || handler.endEdit(*id) != kResultOk {
                return kResultFalse;
            }
        }
        if grouped {
            self.nonce.set(self.nonce.get().saturating_add(1));
            let Some(handler2) = handler2 else {
                return kResultFalse;
            };
            if handler2.setDirty(1) != kResultOk || handler2.finishGroupEdit() != kResultOk {
                return kResultFalse;
            }
        }
        self.bump(6);
        kResultOk
    }
}

impl ControllerState {
    // UI-thread-only standard notification; metadata is deliberately unchanged.
    // This proves observation/retirement semantics, not a latency/PDC change.
    unsafe fn request_latency_reread(&self) -> tresult {
        if !self.main_thread() {
            return kResultFalse;
        }
        let handler = self.handler.borrow().clone();
        let Some(handler) = handler else {
            return kResultFalse;
        };
        handler.restartComponent(RestartFlags_::kLatencyChanged as i32)
    }
}

struct EditorController {
    state: Rc<ControllerState>,
}
impl Class for EditorController {
    type Interfaces = (IEditController,);
}
impl EditorController {
    const CID: TUID = uid(0x48CE5E91, 0x6FEC4D4A, 0xBDA302D1, 0xD70C2DE9);
    fn new() -> Self {
        Self { state: Rc::new(ControllerState::new()) }
    }
}
impl IPluginBaseTrait for EditorController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        if self.state.main_thread() {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn terminate(&self) -> tresult {
        if !self.state.main_thread() {
            return kResultFalse;
        }
        self.state.handler.borrow_mut().take();
        kResultOk
    }
}
impl IEditControllerTrait for EditorController {
    unsafe fn setComponentState(&self, stream: *mut IBStream) -> tresult {
        self.setState(stream)
    }
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        if !self.state.main_thread() {
            return kResultFalse;
        }
        let Some(value) = read_configuration(stream) else {
            return kResultFalse;
        };
        self.state.restore(value);
        kResultOk
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        if !self.state.main_thread() {
            return kResultFalse;
        }
        let result = write_configuration(stream, self.state.configuration());
        if result != kResultOk {
            return result;
        }
        let handler = self.state.handler.borrow().clone();
        if self.state.nonce.get() != 0 {
            if let Some(handler2) = handler.and_then(|handler| handler.cast::<IComponentHandler2>())
            {
                // Native dirty=false is a legitimate getState callback. The
                // host must retain it without inventing a saved Project state.
                if handler2.setDirty(0) != kResultOk {
                    return kResultFalse;
                }
            }
        }
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        3 + PROBE_COUNT as i32
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if info.is_null() || index < 0 || index >= self.getParameterCount() {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.unitId = 0;
        info.stepCount = 0;
        info.defaultNormalizedValue = 0.0;
        let label = match index {
            0 => "Gain",
            1 => "Balance",
            2 => "Fixture failure scenario",
            3 => "Attach count",
            4 => "Remove attempts",
            5 => "View focus calls",
            6 => "Native focus messages",
            7 => "Resize calls",
            8 => "Native messages",
            9 => "Completed native edits",
            _ => "Wrong thread calls",
        };
        info.id = if index < 3 {
            index as u32
        } else {
            PROBE_BASE + index as u32 - 3
        };
        info.flags = if index < 2 {
            ParameterInfo_::ParameterFlags_::kCanAutomate
        } else if index == 2 {
            info.stepCount = 2;
            ParameterInfo_::ParameterFlags_::kNoFlags
        } else {
            ParameterInfo_::ParameterFlags_::kIsReadOnly
        };
        if index < 2 {
            info.defaultNormalizedValue = 0.5;
        }
        copy_wstring(label, &mut info.title);
        copy_wstring(label, &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, output: *mut String128) -> tresult {
        if output.is_null() || id > 2 && !(PROBE_BASE..PROBE_BASE + PROBE_COUNT).contains(&id) {
            return kInvalidArgument;
        }
        copy_wstring(&value.to_string(), &mut *output);
        kResultOk
    }
    unsafe fn getParamValueByString(
        &self,
        _id: u32,
        _text: *mut TChar,
        _value: *mut f64,
    ) -> tresult {
        kNotImplemented
    }
    unsafe fn normalizedParamToPlain(&self, id: u32, value: f64) -> f64 {
        if id == 2 {
            value * 2.0
        } else {
            value
        }
    }
    unsafe fn plainParamToNormalized(&self, id: u32, value: f64) -> f64 {
        if id == 2 {
            value / 2.0
        } else {
            value
        }
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        if !self.state.main_thread() {
            return 0.0;
        }
        match id {
            0 => self.state.gain.get(),
            1 => self.state.balance.get(),
            2 => f64::from(self.state.mode.get()) / 2.0,
            _ if id == PROBE_BASE + PROBE_COUNT - 1 => {
                f64::from(self.state.wrong_thread.load(Ordering::Relaxed).min(1024)) / 1024.0
            }
            _ if (PROBE_BASE..PROBE_BASE + PROBE_COUNT - 1).contains(&id) => {
                f64::from(self.state.probes[(id - PROBE_BASE) as usize].get().min(1024)) / 1024.0
            }
            _ => 0.0,
        }
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if !self.state.main_thread() || !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return kInvalidArgument;
        }
        match id {
            0 => self.state.gain.set(value),
            1 => self.state.balance.set(value),
            2 => self.state.mode.set((value * 2.0).round() as u32),
            _ => return kInvalidArgument,
        }
        kResultOk
    }
    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        if !self.state.main_thread() {
            return kResultFalse;
        }
        *self.state.handler.borrow_mut() =
            ComRef::from_raw(handler).map(|value| value.to_com_ptr());
        kResultOk
    }
    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        if !self.state.main_thread()
            || name.is_null()
            || CStr::from_ptr(name).to_bytes() != b"editor"
        {
            return ptr::null_mut();
        }
        ComWrapper::new(EditorView::new(Rc::clone(&self.state)))
            .to_com_ptr::<IPlugView>()
            .map_or(ptr::null_mut(), |value| value.into_raw())
    }
}

struct EditorView {
    state: Rc<ControllerState>,
    width: Cell<i32>,
    height: Cell<i32>,
    removed_once: Cell<bool>,
    cleared_once: Cell<bool>,
    #[cfg(target_os = "windows")]
    window: Cell<winapi::shared::windef::HWND>,
}
impl Class for EditorView {
    type Interfaces = (IPlugView,);
}
impl EditorView {
    fn new(state: Rc<ControllerState>) -> Self {
        Self {
            state,
            width: Cell::new(320),
            height: Cell::new(160),
            removed_once: Cell::new(false),
            cleared_once: Cell::new(false),
            #[cfg(target_os = "windows")]
            window: Cell::new(ptr::null_mut()),
        }
    }
}
impl IPlugViewTrait for EditorView {
    unsafe fn isPlatformTypeSupported(&self, platform: FIDString) -> tresult {
        if self.state.main_thread()
            && !platform.is_null()
            && CStr::from_ptr(platform).to_bytes() == b"HWND"
            && cfg!(target_os = "windows")
        {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn attached(&self, parent: *mut c_void, platform: FIDString) -> tresult {
        if parent.is_null() || self.isPlatformTypeSupported(platform) != kResultOk {
            return kInvalidArgument;
        }
        #[cfg(target_os = "windows")]
        {
            if !self.window.get().is_null() {
                return kResultFalse;
            }
            match native_window::create(
                parent.cast(),
                self.width.get(),
                self.height.get(),
                &self.state,
            ) {
                Some(hwnd) => {
                    self.window.set(hwnd);
                    self.state.bump(0);
                    kResultOk
                }
                None => kResultFalse,
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            kResultFalse
        }
    }
    unsafe fn removed(&self) -> tresult {
        if !self.state.main_thread() {
            return kResultFalse;
        }
        self.state.bump(1);
        if self.state.mode.get() == 1 && !self.removed_once.replace(true) {
            return kResultFalse;
        }
        #[cfg(target_os = "windows")]
        {
            let hwnd = self.window.get();
            if hwnd.is_null() {
                return kResultFalse;
            }
            if winapi::um::winuser::DestroyWindow(hwnd) == 0 {
                return kResultFalse;
            }
            self.window.set(ptr::null_mut());
            native_window::unregister_unused_class();
        }
        kResultOk
    }
    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        if self.state.main_thread() {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn onKeyDown(&self, key: char16, keyCode: int16, _modifiers: int16) -> tresult {
        if !self.state.main_thread() {
            return kResultFalse;
        }
        if key == b'G' as char16 || keyCode == b'G' as int16 {
            self.state.edit(true)
        } else if key == b' ' as char16 || keyCode == 32 {
            self.state.edit(false)
        } else {
            kResultFalse
        }
    }
    unsafe fn onKeyUp(&self, _key: char16, _keyCode: int16, _modifiers: int16) -> tresult {
        if self.state.main_thread() {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getSize(&self, output: *mut ViewRect) -> tresult {
        if !self.state.main_thread() || output.is_null() {
            return kInvalidArgument;
        }
        *output = ViewRect {
            left: 0,
            top: 0,
            right: self.width.get(),
            bottom: self.height.get(),
        };
        kResultOk
    }
    unsafe fn onSize(&self, input: *mut ViewRect) -> tresult {
        if !self.state.main_thread() || input.is_null() {
            return kInvalidArgument;
        }
        let Some(width) = (*input).right.checked_sub((*input).left) else {
            return kInvalidArgument;
        };
        let Some(height) = (*input).bottom.checked_sub((*input).top) else {
            return kInvalidArgument;
        };
        if !(200..=800).contains(&width) || !(100..=400).contains(&height) {
            return kInvalidArgument;
        }
        #[cfg(target_os = "windows")]
        {
            if !self.window.get().is_null()
                && winapi::um::winuser::SetWindowPos(
                    self.window.get(),
                    ptr::null_mut(),
                    0,
                    0,
                    width,
                    height,
                    winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOACTIVATE,
                ) == 0
            {
                return kResultFalse;
            }
        }
        self.width.set(width);
        self.height.set(height);
        self.state.bump(4);
        kResultOk
    }
    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        if !self.state.main_thread() {
            return kResultFalse;
        }
        self.state.bump(2);
        kResultOk
    }
    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        if !self.state.main_thread() {
            return kResultFalse;
        }
        if frame.is_null() && self.state.mode.get() == 2 && !self.cleared_once.replace(true) {
            return kResultFalse;
        }
        *self.state.frame.borrow_mut() = ComRef::from_raw(frame).map(|value| value.to_com_ptr());
        kResultOk
    }
    unsafe fn canResize(&self) -> tresult {
        if self.state.main_thread() {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        if !self.state.main_thread() || rect.is_null() {
            return kInvalidArgument;
        }
        let Some(width) = (*rect).right.checked_sub((*rect).left) else {
            return kInvalidArgument;
        };
        let Some(height) = (*rect).bottom.checked_sub((*rect).top) else {
            return kInvalidArgument;
        };
        let Some(right) = (*rect).left.checked_add(width.clamp(200, 800)) else {
            return kInvalidArgument;
        };
        let Some(bottom) = (*rect).top.checked_add(height.clamp(100, 400)) else {
            return kInvalidArgument;
        };
        (*rect).right = right;
        (*rect).bottom = bottom;
        kResultOk
    }
}

#[cfg(target_os = "windows")]
mod native_window {
    use super::*;
    use winapi::shared::{
        minwindef::{LPARAM, LRESULT, UINT, WPARAM},
        windef::HWND,
    };
    use winapi::um::{libloaderapi::GetModuleHandleExW, winuser::*};
    const CLASS: &str = "MondrianOwnedVst3EditorFixture";
    const TITLE: &str = "Mondrian VST3 fixture view";
    struct WindowContext {
        state: Rc<ControllerState>,
        phase: Rc<Cell<u8>>,
    }
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }
    unsafe fn module() -> Option<winapi::shared::minwindef::HMODULE> {
        let mut instance = ptr::null_mut();
        // FROM_ADDRESS | UNCHANGED_REFCOUNT: the caller's actual loaded Plugin
        // owns this DLL through removal and class teardown, not the EXE module.
        if GetModuleHandleExW(
            0x0000_0004 | 0x0000_0002,
            procedure as *const u16,
            &mut instance,
        ) == 0
        {
            None
        } else {
            Some(instance)
        }
    }
    pub(super) unsafe fn unregister_unused_class() {
        if let Some(instance) = module() {
            let _ = UnregisterClassW(wide(CLASS).as_ptr(), instance);
        }
    }
    unsafe extern "system" fn procedure(
        hwnd: HWND,
        message: UINT,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            let Some(create) = (lparam as *const CREATESTRUCTW).as_ref() else {
                return 0;
            };
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            let Some(context) = (create.lpCreateParams as *const WindowContext).as_ref() else {
                return 0;
            };
            context.phase.set(1);
        }
        let context_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowContext;
        if let Some(context) = context_ptr.as_ref() {
            let state = &context.state;
            if !state.main_thread() {
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }
            match message {
                WM_SETFOCUS | WM_KILLFOCUS => {
                    state.bump(3);
                }
                WM_LBUTTONDOWN => {
                    state.bump(5);
                    SetFocus(hwnd);
                    let _ = state.edit(false);
                    return 0;
                }
                WM_KEYDOWN if wparam == b'G' as usize || wparam == 32 => {
                    state.bump(5);
                    let _ = state.edit(wparam == b'G' as usize);
                    return 0;
                }
                WM_KEYDOWN if wparam == b'R' as usize => {
                    state.bump(5);
                    let _ = state.request_latency_reread();
                    return 0;
                }
                WM_NCDESTROY => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    context.phase.set(2);
                    let result = DefWindowProcW(hwnd, message, wparam, lparam);
                    drop(Box::from_raw(context_ptr));
                    return result;
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
    pub(super) unsafe fn create(
        parent: HWND,
        width: i32,
        height: i32,
        state: &Rc<ControllerState>,
    ) -> Option<HWND> {
        if IsWindow(parent) == 0 {
            return None;
        }
        let class_name = wide(CLASS);
        let title = wide(TITLE);
        let instance = module()?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(procedure),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: ptr::null_mut(),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: ptr::null_mut(),
        };
        // The name is process-local; another view of this exact fixture may have registered it.
        if RegisterClassExW(&class) == 0 {
            let mut existing: WNDCLASSEXW = std::mem::zeroed();
            existing.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            if GetClassInfoExW(instance, class_name.as_ptr(), &mut existing) == 0
                || existing.lpfnWndProc.map(|value| value as usize)
                    != Some(procedure as *const () as usize)
            {
                return None;
            }
        }
        let phase = Rc::new(Cell::new(0));
        let context = Box::into_raw(Box::new(WindowContext {
            state: Rc::clone(state),
            phase: Rc::clone(&phase),
        }));
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            0,
            0,
            width,
            height,
            parent,
            ptr::null_mut(),
            instance,
            context.cast(),
        );
        if hwnd.is_null() {
            if phase.get() == 0 {
                drop(Box::from_raw(context));
            }
            // Do not leave a class procedure pointing into an unloadable DLL.
            // Windows refuses unregister while another real view remains.
            unregister_unused_class();
            return None;
        }
        Some(hwnd)
    }
}

struct Factory;
impl Class for Factory {
    type Interfaces = (IPluginFactory,);
}
impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, output: *mut PFactoryInfo) -> tresult {
        if output.is_null() {
            return kInvalidArgument;
        }
        let output = &mut *output;
        copy_cstring("Mondrian owned qualification", &mut output.vendor);
        copy_cstring("", &mut output.url);
        copy_cstring("", &mut output.email);
        output.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        2
    }
    unsafe fn getClassInfo(&self, index: i32, output: *mut PClassInfo) -> tresult {
        if output.is_null() {
            return kInvalidArgument;
        }
        let output = &mut *output;
        let (cid, category) = match index {
            0 => (EditorProcessor::CID, "Audio Module Class"),
            1 => (EditorController::CID, "Component Controller Class"),
            _ => return kInvalidArgument,
        };
        output.cid = cid;
        output.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring(category, &mut output.category);
        copy_cstring(PLUGIN_NAME, &mut output.name);
        kResultOk
    }
    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        output: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || output.is_null() {
            return kInvalidArgument;
        }
        *output = ptr::null_mut();
        let instance = match *(cid as *const TUID) {
            EditorProcessor::CID => {
                ComWrapper::new(EditorProcessor::new()).to_com_ptr::<FUnknown>()
            }
            EditorController::CID => {
                ComWrapper::new(EditorController::new()).to_com_ptr::<FUnknown>()
            }
            _ => None,
        };
        let Some(instance) = instance else {
            return kInvalidArgument;
        };
        let value = instance.as_ptr();
        ((*(*value).vtbl).queryInterface)(value, iid as *mut TUID, output)
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
#[no_mangle]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map_or(ptr::null_mut(), |value| value.into_raw())
}
