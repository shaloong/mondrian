//! Owned SDK Note oracle. Original Gain CIDs/state/DSP are untouched.
//! Audit is transient, fixed-capacity evidence; restoring configuration never
//! replays it. A callback may fail after earlier chunks advanced: no rollback.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Mutex;

pub(super) const COMPONENT_CID: TUID = uid(0x6E4F05A1, 0x2D8B4C63, 0x9F017B42, 0xA4C2E611);
const CONTROLLER_CID: TUID = uid(0x84D037BE, 0x29F84A11, 0xBD625C30, 0x7A15E902);
const NAME: &str = "Mondrian VST3 Note Event Oracle";
const MAX_CALLS: usize = 64;
const MAX_EVENTS: usize = 4096;
const CONFIG_BYTES: usize = 12;
const CALL_BYTES: usize = 16;
const EVENT_BYTES: usize = 48;
const HEADER_BYTES: usize = 56;
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

pub(super) unsafe fn class_info(index: i32, info: *mut PClassInfo) -> tresult {
    if info.is_null() {
        return kInvalidArgument;
    }
    let info = &mut *info;
    info.cid = if index == 0 {
        COMPONENT_CID
    } else {
        CONTROLLER_CID
    };
    info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
    copy_cstring(
        if index == 0 {
            "Audio Module Class"
        } else {
            "Component Controller Class"
        },
        &mut info.category,
    );
    copy_cstring(NAME, &mut info.name);
    kResultOk
}

pub(super) fn create_instance(cid: TUID) -> Option<vst3::ComPtr<FUnknown>> {
    match cid {
        COMPONENT_CID => ComWrapper::new(NoteProcessor::new()?).to_com_ptr::<FUnknown>(),
        CONTROLLER_CID => {
            ComWrapper::new(NoteController { fail_call: Cell::new(0) }).to_com_ptr::<FUnknown>()
        }
        _ => None,
    }
}

unsafe fn read_config(stream: *mut IBStream) -> Option<u32> {
    let stream = ComRef::from_raw(stream)?;
    let mut bytes = [0; CONFIG_BYTES];
    let mut read = 0;
    if stream.read(bytes.as_mut_ptr().cast(), CONFIG_BYTES as i32, &mut read) != kResultOk
        || read != CONFIG_BYTES as i32
        || bytes[..4] != *b"MNE1"
        || u32::from_le_bytes(bytes[4..8].try_into().ok()?) != 1
    {
        return None;
    }
    let fail = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    (fail <= MAX_CALLS as u32).then_some(fail)
}
unsafe fn write_bytes(stream: *mut IBStream, bytes: &[u8]) -> tresult {
    let Some(stream) = ComRef::from_raw(stream) else {
        return kInvalidArgument;
    };
    let mut written = 0;
    if stream.write(
        bytes.as_ptr().cast_mut().cast(),
        bytes.len() as i32,
        &mut written,
    ) == kResultOk
        && written == bytes.len() as i32
    {
        kResultOk
    } else {
        kResultFalse
    }
}
fn config(fail: u32) -> [u8; CONFIG_BYTES] {
    let mut bytes = [0; CONFIG_BYTES];
    bytes[..4].copy_from_slice(b"MNE1");
    bytes[4..8].copy_from_slice(&1_u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&fail.to_le_bytes());
    bytes
}

#[derive(Clone, Copy, Default)]
struct Note {
    call: u32,
    bus: i32,
    offset: i32,
    ppq: u64,
    flags: u16,
    kind: u16,
    channel: i16,
    pitch: i16,
    tuning: u32,
    velocity: u32,
    length: i32,
    id: i32,
}
impl Note {
    unsafe fn read(call: u32, event: Event) -> Option<Self> {
        let (channel, pitch, tuning, velocity, length, id) =
            match event.r#type as Event_::EventTypes {
                Event_::EventTypes_::kNoteOnEvent => {
                    let note = event.__field0.noteOn;
                    (
                        note.channel,
                        note.pitch,
                        note.tuning,
                        note.velocity,
                        note.length,
                        note.noteId,
                    )
                }
                Event_::EventTypes_::kNoteOffEvent => {
                    let note = event.__field0.noteOff;
                    (
                        note.channel,
                        note.pitch,
                        note.tuning,
                        note.velocity,
                        0,
                        note.noteId,
                    )
                }
                _ => return None,
            };
        if !(0..2).contains(&event.busIndex)
            || !(0..16).contains(&channel)
            || !(0..128).contains(&pitch)
            || id < 0
            || length < 0
            || !tuning.is_finite()
            || !velocity.is_finite()
            || !(0.0..=1.0).contains(&velocity)
            || !event.ppqPosition.is_finite()
            || event.flags & !0xc001 != 0
        {
            return None;
        }
        Some(Self {
            call,
            bus: event.busIndex,
            offset: event.sampleOffset,
            ppq: event.ppqPosition.to_bits(),
            flags: event.flags,
            kind: event.r#type,
            channel,
            pitch,
            tuning: tuning.to_bits(),
            velocity: velocity.to_bits(),
            length,
            id,
        })
    }
    fn encode(self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.call.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.bus.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.offset.to_le_bytes());
        bytes[12..20].copy_from_slice(&self.ppq.to_le_bytes());
        bytes[20..22].copy_from_slice(&self.flags.to_le_bytes());
        bytes[22..24].copy_from_slice(&self.kind.to_le_bytes());
        bytes[24..26].copy_from_slice(&self.channel.to_le_bytes());
        bytes[26..28].copy_from_slice(&self.pitch.to_le_bytes());
        bytes[28..32].copy_from_slice(&self.tuning.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.velocity.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.length.to_le_bytes());
        bytes[40..44].copy_from_slice(&self.id.to_le_bytes());
        // Four reserved bytes are always zero, not copied SDK padding.
    }
}
#[derive(Clone, Copy, Default)]
struct Call {
    frames: i32,
    result: i32,
    first: u32,
    count: u32,
}
#[derive(Clone, Copy)]
struct Voice {
    bus: i32,
    channel: i16,
    pitch: i16,
    id: i32,
    value: f32,
}
const EMPTY_VOICE: Voice = Voice { bus: 0, channel: 0, pitch: 0, id: -1, value: 0.0 };
struct Audit {
    calls: [Call; MAX_CALLS],
    call_count: usize,
    // Allocated on the component construction/control thread, never grown by process.
    events: Vec<Note>,
    event_count: usize,
    incomplete: bool,
    voices: [Voice; 16],
}
impl Audit {
    fn new() -> Self {
        Self {
            calls: [Call::default(); MAX_CALLS],
            call_count: 0,
            events: vec![Note::default(); MAX_EVENTS],
            event_count: 0,
            incomplete: false,
            voices: [EMPTY_VOICE; 16],
        }
    }
    fn apply(&mut self, note: Note) -> bool {
        let found = self
            .voices
            .iter()
            .position(|voice| voice.id == note.id && voice.bus == note.bus);
        if note.kind as Event_::EventTypes == Event_::EventTypes_::kNoteOnEvent {
            if found.is_some() {
                return false;
            }
            let Some(index) = self.voices.iter().position(|voice| voice.id < 0) else {
                return false;
            };
            self.voices[index] = Voice {
                bus: note.bus,
                channel: note.channel,
                pitch: note.pitch,
                id: note.id,
                value: f32::from_bits(note.velocity) * (note.bus + 1) as f32 * 0.5,
            };
        } else {
            let Some(index) = found else {
                return false;
            };
            let voice = self.voices[index];
            if voice.channel != note.channel || voice.pitch != note.pitch {
                return false;
            }
            self.voices[index] = EMPTY_VOICE;
        }
        true
    }
}

struct NoteProcessor {
    instance: u64,
    fail_call: AtomicU32,
    active_buses: AtomicU32,
    active: AtomicBool,
    processing: AtomicBool,
    max_frames: AtomicU32,
    sample_rate: AtomicU64,
    audit: Mutex<Audit>,
}
impl NoteProcessor {
    fn new() -> Option<Self> {
        let instance = NEXT_INSTANCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .ok()?;
        Some(Self {
            instance,
            fail_call: AtomicU32::new(0),
            active_buses: AtomicU32::new(0),
            active: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            max_frames: AtomicU32::new(0),
            sample_rate: AtomicU64::new(0),
            audit: Mutex::new(Audit::new()),
        })
    }
}

impl Class for NoteProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}
impl IPluginBaseTrait for NoteProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}
impl IComponentTrait for NoteProcessor {
    unsafe fn getControllerClassId(&self, id: *mut TUID) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        *id = CONTROLLER_CID;
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media: MediaType, direction: BusDirection) -> i32 {
        match (media as MediaTypes, direction as BusDirections) {
            (MediaTypes_::kAudio, BusDirections_::kInput | BusDirections_::kOutput) => 1,
            (MediaTypes_::kEvent, BusDirections_::kInput) => 2,
            _ => 0,
        }
    }
    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        direction: BusDirection,
        index: i32,
        out: *mut BusInfo,
    ) -> tresult {
        if out.is_null() || index < 0 || index >= self.getBusCount(media, direction) {
            return kInvalidArgument;
        }
        let out = &mut *out;
        out.mediaType = media;
        out.direction = direction;
        out.channelCount = if media == MediaTypes_::kAudio as MediaType {
            2
        } else {
            16
        };
        copy_wstring(
            if media == MediaTypes_::kAudio as MediaType {
                "Note oracle PCM"
            } else {
                "Note oracle events"
            },
            &mut out.name,
        );
        out.busType = (if index == 0 {
            BusTypes_::kMain
        } else {
            BusTypes_::kAux
        }) as BusType;
        out.flags = if index == 0 {
            BusInfo_::BusFlags_::kDefaultActive as u32
        } else {
            0
        };
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
        state: TBool,
    ) -> tresult {
        if index < 0 || index >= self.getBusCount(media, direction) {
            return kInvalidArgument;
        }
        let bit = if media == MediaTypes_::kEvent as MediaType {
            4 << index
        } else if direction == BusDirections_::kInput as BusDirection {
            1
        } else {
            2
        };
        if state != 0 {
            self.active_buses.fetch_or(bit, Ordering::Relaxed);
        } else {
            self.active_buses.fetch_and(!bit, Ordering::Relaxed);
        }
        kResultOk
    }
    unsafe fn setActive(&self, state: TBool) -> tresult {
        self.active.store(state != 0, Ordering::Release);
        kResultOk
    }
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        let Some(fail) = read_config(stream) else {
            return kInvalidArgument;
        };
        let Ok(mut audit) = self.audit.try_lock() else {
            return kResultFalse;
        };
        audit.voices = [EMPTY_VOICE; 16];
        self.fail_call.store(fail, Ordering::Relaxed);
        // Historical callback/tuple audit is neither restored nor cleared.
        kResultOk
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        let Ok(audit) = self.audit.try_lock() else {
            return kResultFalse;
        };
        let mut bytes =
            vec![0_u8; HEADER_BYTES + MAX_CALLS * CALL_BYTES + MAX_EVENTS * EVENT_BYTES];
        bytes[..CONFIG_BYTES].copy_from_slice(&config(self.fail_call.load(Ordering::Relaxed)));
        bytes[12..16].copy_from_slice(b"MNA1");
        bytes[16..20].copy_from_slice(&1_u32.to_le_bytes());
        bytes[20..28].copy_from_slice(&self.instance.to_le_bytes());
        bytes[28..32].copy_from_slice(&(audit.call_count as u32).to_le_bytes());
        bytes[32..36].copy_from_slice(&(audit.event_count as u32).to_le_bytes());
        bytes[36..40].copy_from_slice(&u32::from(!audit.incomplete).to_le_bytes());
        bytes[40..48].copy_from_slice(&self.sample_rate.load(Ordering::Relaxed).to_le_bytes());
        bytes[48..52].copy_from_slice(&self.max_frames.load(Ordering::Relaxed).to_le_bytes());
        let mut cursor = HEADER_BYTES;
        for call in &audit.calls[..audit.call_count] {
            bytes[cursor..cursor + 4].copy_from_slice(&call.frames.to_le_bytes());
            bytes[cursor + 4..cursor + 8].copy_from_slice(&call.result.to_le_bytes());
            bytes[cursor + 8..cursor + 12].copy_from_slice(&call.first.to_le_bytes());
            bytes[cursor + 12..cursor + 16].copy_from_slice(&call.count.to_le_bytes());
            cursor += CALL_BYTES;
        }
        for note in &audit.events[..audit.event_count] {
            note.encode(&mut bytes[cursor..cursor + EVENT_BYTES]);
            cursor += EVENT_BYTES;
        }
        write_bytes(stream, &bytes[..cursor])
    }
}
impl IAudioProcessorTrait for NoteProcessor {
    unsafe fn setBusArrangements(
        &self,
        input: *mut SpeakerArrangement,
        inputs: i32,
        output: *mut SpeakerArrangement,
        outputs: i32,
    ) -> tresult {
        if inputs != 1
            || outputs != 1
            || input.is_null()
            || output.is_null()
            || *input != SpeakerArr::kStereo
            || *output != SpeakerArr::kStereo
        {
            kResultFalse
        } else {
            kResultTrue
        }
    }
    unsafe fn getBusArrangement(
        &self,
        direction: BusDirection,
        index: i32,
        out: *mut SpeakerArrangement,
    ) -> tresult {
        if out.is_null()
            || index != 0
            || !matches!(
                direction as BusDirections,
                BusDirections_::kInput | BusDirections_::kOutput
            )
        {
            return kInvalidArgument;
        }
        *out = SpeakerArr::kStereo;
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if setup.is_null()
            || !(*setup).sampleRate.is_finite()
            || (*setup).sampleRate <= 0.0
            || (*setup).maxSamplesPerBlock <= 0
            || (*setup).symbolicSampleSize != SymbolicSampleSizes_::kSample32 as i32
        {
            return kInvalidArgument;
        }
        self.max_frames.store((*setup).maxSamplesPerBlock as u32, Ordering::Relaxed);
        self.sample_rate.store((*setup).sampleRate.to_bits(), Ordering::Relaxed);
        kResultOk
    }
    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        if state != 0 && !self.active.load(Ordering::Acquire) {
            return kResultFalse;
        }
        self.processing.store(state != 0, Ordering::Release);
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let Ok(mut audit) = self.audit.try_lock() else {
            return kResultFalse;
        };
        if audit.call_count == MAX_CALLS {
            audit.incomplete = true;
            return kResultFalse;
        }
        let call_index = audit.call_count;
        audit.call_count += 1;
        let first = audit.event_count;
        audit.calls[call_index] = Call {
            frames: if data.is_null() {
                -1
            } else {
                (*data).numSamples
            },
            result: kInvalidArgument,
            first: first as u32,
            count: 0,
        };
        if data.is_null() {
            return kInvalidArgument;
        }
        let data = &*data;
        if !self.processing.load(Ordering::Acquire)
            || self.active_buses.load(Ordering::Relaxed) != 15
            || data.numSamples <= 0
            || data.numSamples as u32 > self.max_frames.load(Ordering::Relaxed)
            || data.symbolicSampleSize != SymbolicSampleSizes_::kSample32 as i32
            || data.numInputs != 1
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
            || input.__field0.channelBuffers32.is_null()
            || output.__field0.channelBuffers32.is_null()
        {
            return kInvalidArgument;
        }
        let inputs = slice::from_raw_parts(input.__field0.channelBuffers32, 2);
        let outputs = slice::from_raw_parts(output.__field0.channelBuffers32, 2);
        if inputs.iter().chain(outputs).any(|pointer| pointer.is_null()) {
            return kInvalidArgument;
        }
        let count = if let Some(events) = ComRef::from_raw(data.inputEvents) {
            let count = events.getEventCount();
            if count < 0 || count as usize > MAX_EVENTS - first {
                audit.incomplete = true;
                return kInvalidArgument;
            }
            let mut last_offset = -1;
            for index in 0..count {
                let mut event: Event = std::mem::zeroed();
                if events.getEvent(index, &mut event) != kResultOk {
                    audit.incomplete = true;
                    return kInvalidArgument;
                }
                let Some(note) = Note::read((call_index + 1) as u32, event) else {
                    audit.incomplete = true;
                    return kInvalidArgument;
                };
                if note.offset < last_offset || note.offset < 0 || note.offset >= data.numSamples {
                    audit.incomplete = true;
                    return kInvalidArgument;
                }
                last_offset = note.offset;
                audit.events[first + index as usize] = note;
            }
            count as usize
        } else {
            0
        };
        audit.event_count += count;
        audit.calls[call_index].count = count as u32;
        if self.fail_call.load(Ordering::Relaxed) == (call_index + 1) as u32 {
            audit.calls[call_index].result = kResultFalse;
            return kResultFalse;
        }
        let mut next = first;
        for frame in 0..data.numSamples {
            while next < first + count && audit.events[next].offset == frame {
                let note = audit.events[next];
                if !audit.apply(note) {
                    return kInvalidArgument;
                }
                next += 1;
            }
            let value: f32 = audit.voices.iter().map(|voice| voice.value).sum();
            *outputs[0].add(frame as usize) = value;
            *outputs[1].add(frame as usize) = if value == 0.0 { 0.0 } else { -value * 0.5 };
        }
        audit.calls[call_index].result = kResultOk;
        kResultOk
    }
}
impl IProcessContextRequirementsTrait for NoteProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

struct NoteController {
    fail_call: Cell<u32>,
}
impl Class for NoteController {
    type Interfaces = (IEditController,);
}
impl IPluginBaseTrait for NoteController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}
impl IEditControllerTrait for NoteController {
    unsafe fn setComponentState(&self, stream: *mut IBStream) -> tresult {
        self.setState(stream)
    }
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        let Some(fail) = read_config(stream) else {
            return kInvalidArgument;
        };
        self.fail_call.set(fail);
        kResultOk
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        write_bytes(stream, &config(self.fail_call.get()))
    }
    unsafe fn getParameterCount(&self) -> i32 {
        0
    }
    unsafe fn getParameterInfo(&self, _index: i32, _info: *mut ParameterInfo) -> tresult {
        kInvalidArgument
    }
    unsafe fn getParamStringByValue(
        &self,
        _id: ParamID,
        _value: ParamValue,
        _string: *mut String128,
    ) -> tresult {
        kInvalidArgument
    }
    unsafe fn getParamValueByString(
        &self,
        _id: ParamID,
        _string: *mut TChar,
        _value: *mut ParamValue,
    ) -> tresult {
        kInvalidArgument
    }
    unsafe fn normalizedParamToPlain(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value
    }
    unsafe fn plainParamToNormalized(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value
    }
    unsafe fn getParamNormalized(&self, _id: ParamID) -> ParamValue {
        0.0
    }
    unsafe fn setParamNormalized(&self, _id: ParamID, _value: ParamValue) -> tresult {
        kInvalidArgument
    }
    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }
    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}
