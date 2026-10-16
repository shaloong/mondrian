//! Owned SDK Note oracle. Original Gain CIDs/state/DSP are untouched.
//! Audit is transient, fixed-capacity evidence; restoring configuration never
//! replays it. A callback may fail after earlier chunks advanced: no rollback.
use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Mutex;
use std::thread::{self, ThreadId};
use std::{fs::File, io::Write, path::PathBuf};
use vst3::ComPtr;

pub(super) const COMPONENT_CID: TUID = uid(0x6E4F05A1, 0x2D8B4C63, 0x9F017B42, 0xA4C2E611);
const CONTROLLER_CID: TUID = uid(0x84D037BE, 0x29F84A11, 0xBD625C30, 0x7A15E902);
const NAME: &str = "Mondrian VST3 Note Event Oracle";
const MAX_CALLS: usize = 64;
const MAX_EVENTS: usize = 4096;
const MAIN_MAX_CALLS: usize = 1024;
const MAIN_MAX_EVENTS: usize = 8192;
const MAIN_AUDIT_HEADER_BYTES: usize = 80;
const MAIN_AUDIT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAIN_AUDIT_ENV: &str = "MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR";
const CONFIG_BYTES: usize = 12;
const ADMISSION_CONFIG_BYTES: usize = 16;
const SERVICE_CONFIG_BYTES: usize = 20;
const TRACE_ENV: &str = "MONDRIAN_VST3_EVENT_ADMISSION_TRACE";
const TRACE_BYTES: u64 = 16 * 1024;
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
        CONTROLLER_CID => ComWrapper::new(NoteController {
            config: Cell::new(Config::legacy(0)),
            handler: RefCell::new(None),
            main_thread: Cell::new(None),
        })
        .to_com_ptr::<FUnknown>(),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Config {
    fail_call: u32,
    main_only: bool,
    // None retains the original MNE1 configuration, audit and Note DSP.
    auxiliary_default: Option<bool>,
    // MNE3 input-only first-stop command: 0 stable, 1 retain, 2 flip default.
    // Normal getState preserves current component configuration, not commands.
    service_action: Option<u32>,
}
impl Config {
    const fn legacy(fail_call: u32) -> Self {
        Self {
            fail_call,
            main_only: false,
            auxiliary_default: None,
            service_action: None,
        }
    }
    fn encode(self) -> ([u8; SERVICE_CONFIG_BYTES], usize) {
        let mut bytes = [0; SERVICE_CONFIG_BYTES];
        let (magic, version, len) = if self.main_only {
            (b"MNE4", 4_u32, CONFIG_BYTES)
        } else if self.service_action.is_some() {
            (b"MNE3", 3_u32, SERVICE_CONFIG_BYTES)
        } else if self.auxiliary_default.is_some() {
            (b"MNE2", 2_u32, ADMISSION_CONFIG_BYTES)
        } else {
            (b"MNE1", 1_u32, CONFIG_BYTES)
        };
        bytes[..4].copy_from_slice(magic);
        bytes[4..8].copy_from_slice(&version.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.fail_call.to_le_bytes());
        if let Some(active) = self.auxiliary_default {
            bytes[12..16].copy_from_slice(&u32::from(active).to_le_bytes());
        }
        if let Some(action) = self.service_action {
            bytes[16..20].copy_from_slice(&action.to_le_bytes());
        }
        (bytes, len)
    }
}
unsafe fn read_config(stream: *mut IBStream) -> Option<Config> {
    let stream = ComRef::from_raw(stream)?;
    let mut bytes = [0; SERVICE_CONFIG_BYTES];
    let mut read = 0;
    if stream.read(bytes.as_mut_ptr().cast(), CONFIG_BYTES as i32, &mut read) != kResultOk
        || read != CONFIG_BYTES as i32
    {
        return None;
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let fail_call = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let main_only = &bytes[..4] == b"MNE4" && version == 4;
    if fail_call > if main_only { MAIN_MAX_CALLS } else { MAX_CALLS } as u32 {
        return None;
    }
    let (auxiliary_default, service_action) = match (&bytes[..4], version) {
        (b"MNE1", 1) | (b"MNE4", 4) => (None, None),
        (b"MNE2", 2) => {
            if stream.read(bytes[12..].as_mut_ptr().cast(), 4, &mut read) != kResultOk || read != 4
            {
                return None;
            }
            match u32::from_le_bytes(bytes[12..16].try_into().ok()?) {
                0 => (Some(false), None),
                1 => (Some(true), None),
                _ => return None,
            }
        }
        (b"MNE3", 3) => {
            if stream.read(bytes[12..].as_mut_ptr().cast(), 8, &mut read) != kResultOk || read != 8
            {
                return None;
            }
            let active = match u32::from_le_bytes(bytes[12..16].try_into().ok()?) {
                0 => false,
                1 => true,
                _ => return None,
            };
            let action = u32::from_le_bytes(bytes[16..20].try_into().ok()?);
            if action > 2 {
                return None;
            }
            (Some(active), Some(action))
        }
        _ => return None,
    };
    Some(Config {
        fail_call,
        main_only,
        auxiliary_default,
        service_action,
    })
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
// Fixture-only, explicitly enabled by the native consumer with a precreated
// path. The control thread writes a finite census/termination trace; process()
// never opens or writes a file.
struct AdmissionTrace(File);
impl AdmissionTrace {
    fn open() -> std::io::Result<Option<Self>> {
        let Some(path) = std::env::var_os(TRACE_ENV) else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        if !path.is_absolute() || !path.is_file() {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        File::options().append(true).open(path).map(|file| Some(Self(file)))
    }
    fn record(
        &mut self,
        instance: u64,
        stage: &str,
        index: i32,
        default: bool,
        active: bool,
        calls: usize,
    ) -> std::io::Result<()> {
        let line = format!(
            "MNE2 pid={} instance={instance} stage={stage} index={index} aux_default={} aux_active={} calls={calls}\n",
            std::process::id(), u8::from(default), u8::from(active)
        );
        if self
            .0
            .metadata()?
            .len()
            .checked_add(line.len() as u64)
            .is_none_or(|n| n > TRACE_BYTES)
        {
            return Err(std::io::Error::from(std::io::ErrorKind::FileTooLarge));
        }
        self.0.write_all(line.as_bytes())
    }
}

// MNE4 alone can publish complete post-join evidence. File offsets, not the
// DLL-local serial alone, separate lifetimes after module unload/reload.
struct MainAuditOutput {
    directory: PathBuf,
    control_thread: ThreadId,
}
impl MainAuditOutput {
    fn from_env() -> std::io::Result<Option<Self>> {
        let Some(path) = std::env::var_os(MAIN_AUDIT_ENV) else {
            return Ok(None);
        };
        let directory = PathBuf::from(path);
        if !directory.is_absolute() || !directory.is_dir() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        Ok(Some(Self {
            directory,
            control_thread: thread::current().id(),
        }))
    }
    fn record(
        &self,
        instance: u64,
        active_buses: u32,
        sample_rate: u64,
        max_frames: u32,
        audit: &Audit,
    ) -> std::io::Result<()> {
        if thread::current().id() != self.control_thread {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let length = MAIN_AUDIT_HEADER_BYTES
            .checked_add(
                audit
                    .call_count
                    .checked_mul(CALL_BYTES)
                    .ok_or(std::io::ErrorKind::InvalidData)?,
            )
            .and_then(|n| n.checked_add(audit.event_count.checked_mul(EVENT_BYTES)?))
            .filter(|n| *n as u64 <= MAIN_AUDIT_FILE_BYTES)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(std::io::Error::other)?;
        bytes.extend_from_slice(b"MNA4");
        for value in [4_u32, length as u32, 4, std::process::id(), 1] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&instance.to_le_bytes());
        for value in [u32::from(!audit.incomplete), active_buses] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        for value in [
            max_frames,
            audit.call_count as u32,
            audit.event_count as u32,
            MAIN_MAX_CALLS as u32,
            MAIN_MAX_EVENTS as u32,
            std::mem::size_of::<Call>() as u32,
            std::mem::size_of::<Note>() as u32,
            0,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for call in &audit.calls[..audit.call_count] {
            bytes.extend_from_slice(&call.frames.to_le_bytes());
            bytes.extend_from_slice(&call.result.to_le_bytes());
            bytes.extend_from_slice(&call.first.to_le_bytes());
            bytes.extend_from_slice(&call.count.to_le_bytes());
        }
        for note in &audit.events[..audit.event_count] {
            let mut row = [0_u8; EVENT_BYTES];
            note.encode(&mut row);
            bytes.extend_from_slice(&row);
        }
        if bytes.len() != length {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let path = self.directory.join(format!("mne4-{}.audit", std::process::id()));
        let mut file = File::options().create(true).append(true).open(path)?;
        if file
            .metadata()?
            .len()
            .checked_add(length as u64)
            .is_none_or(|n| n > MAIN_AUDIT_FILE_BYTES)
        {
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        // Never truncate/overwrite. A failed/partial write is not complete evidence.
        file.write_all(&bytes)
    }
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
    calls: Vec<Call>,
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
            calls: vec![Call::default(); MAX_CALLS],
            call_count: 0,
            events: vec![Note::default(); MAX_EVENTS],
            event_count: 0,
            incomplete: false,
            voices: [EMPTY_VOICE; 16],
        }
    }
    // Only an explicit MNE4 restore grows these same owners, on control. Build
    // both replacements before publishing either; never allocate from process.
    fn prepare_main(&mut self) -> bool {
        if self.calls.len() == MAIN_MAX_CALLS && self.events.len() == MAIN_MAX_EVENTS {
            return true;
        }
        let mut calls = Vec::new();
        let mut events = Vec::new();
        if calls.try_reserve_exact(MAIN_MAX_CALLS).is_err()
            || events.try_reserve_exact(MAIN_MAX_EVENTS).is_err()
            || calls.capacity() != MAIN_MAX_CALLS
            || events.capacity() != MAIN_MAX_EVENTS
        {
            return false;
        }
        calls.extend_from_slice(&self.calls[..self.call_count]);
        calls.resize(MAIN_MAX_CALLS, Call::default());
        events.extend_from_slice(&self.events[..self.event_count]);
        events.resize(MAIN_MAX_EVENTS, Note::default());
        self.calls = calls;
        self.events = events;
        true
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
    admission_mode: AtomicBool,
    main_only: AtomicBool,
    main_audit: Mutex<Option<MainAuditOutput>>,
    auxiliary_default: AtomicBool,
    service_mode: AtomicBool,
    stop_action: AtomicU32,
    trace: Mutex<Option<AdmissionTrace>>,
    active_buses: AtomicU32,
    active: AtomicBool,
    processing: AtomicBool,
    max_frames: AtomicU32,
    sample_rate: AtomicU64,
    audit: Mutex<Audit>,
}
impl NoteProcessor {
    fn config(&self) -> Config {
        Config {
            fail_call: self.fail_call.load(Ordering::Relaxed),
            main_only: self.main_only.load(Ordering::Relaxed),
            auxiliary_default: self
                .admission_mode
                .load(Ordering::Relaxed)
                .then(|| self.auxiliary_default.load(Ordering::Relaxed)),
            // A configuration read is not a checkpoint of a queued test command.
            service_action: self.service_mode.load(Ordering::Relaxed).then_some(0),
        }
    }
    fn trace(&self, stage: &str, index: i32, calls: usize) -> bool {
        let Ok(mut trace) = self.trace.try_lock() else {
            return false;
        };
        trace.as_mut().is_none_or(|trace| {
            trace
                .record(
                    self.instance,
                    stage,
                    index,
                    self.auxiliary_default.load(Ordering::Relaxed),
                    self.active_buses.load(Ordering::Relaxed) & 8 != 0,
                    calls,
                )
                .is_ok()
        })
    }
    fn new() -> Option<Self> {
        let instance = NEXT_INSTANCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .ok()?;
        Some(Self {
            instance,
            fail_call: AtomicU32::new(0),
            admission_mode: AtomicBool::new(false),
            main_only: AtomicBool::new(false),
            main_audit: Mutex::new(None),
            auxiliary_default: AtomicBool::new(false),
            service_mode: AtomicBool::new(false),
            stop_action: AtomicU32::new(0),
            trace: Mutex::new(None),
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
        let Ok(audit) = self.audit.try_lock() else {
            return kResultFalse;
        };
        if self.main_only.load(Ordering::Relaxed) {
            if self.processing.load(Ordering::Acquire) {
                return kResultFalse;
            }
            let Ok(output) = self.main_audit.try_lock() else {
                return kResultFalse;
            };
            if let Some(output) = output.as_ref() {
                if output
                    .record(
                        self.instance,
                        self.active_buses.load(Ordering::Relaxed),
                        self.sample_rate.load(Ordering::Relaxed),
                        self.max_frames.load(Ordering::Relaxed),
                        &audit,
                    )
                    .is_err()
                {
                    return kResultFalse;
                }
            }
        }
        if self.trace("terminate", -1, audit.call_count) {
            kResultOk
        } else {
            kResultFalse
        }
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
        out.flags = if index == 0
            || (media == MediaTypes_::kEvent as MediaType
                && index == 1
                && self.auxiliary_default.load(Ordering::Relaxed))
        {
            BusInfo_::BusFlags_::kDefaultActive as u32
        } else {
            0
        };
        if media == MediaTypes_::kEvent as MediaType && self.admission_mode.load(Ordering::Relaxed)
        {
            let enabled = {
                let Ok(trace) = self.trace.try_lock() else {
                    return kResultFalse;
                };
                trace.is_some()
            };
            if enabled {
                let Ok(audit) = self.audit.try_lock() else {
                    return kResultFalse;
                };
                if !self.trace("census", index, audit.call_count) {
                    return kResultFalse;
                }
            }
        }
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
        let Some(config) = read_config(stream) else {
            return kInvalidArgument;
        };
        let main_audit = if config.main_only {
            match MainAuditOutput::from_env() {
                Ok(output) => output,
                Err(_) => return kResultFalse,
            }
        } else {
            None
        };
        let trace = if config.auxiliary_default.is_some() {
            match AdmissionTrace::open() {
                Ok(trace) => trace,
                Err(_) => return kResultFalse,
            }
        } else {
            None
        };
        let Ok(mut audit) = self.audit.try_lock() else {
            return kResultFalse;
        };
        let Ok(mut current_trace) = self.trace.try_lock() else {
            return kResultFalse;
        };
        let Ok(mut output) = self.main_audit.try_lock() else {
            return kResultFalse;
        };
        if config.main_only {
            if !audit.prepare_main() {
                return kResultFalse;
            }
        } else if audit.call_count > MAX_CALLS || audit.event_count > MAX_EVENTS {
            return kInvalidArgument;
        }
        *output = main_audit;
        *current_trace = trace;
        audit.voices = [EMPTY_VOICE; 16];
        self.main_only.store(config.main_only, Ordering::Relaxed);
        self.fail_call.store(config.fail_call, Ordering::Relaxed);
        self.admission_mode.store(config.auxiliary_default.is_some(), Ordering::Relaxed);
        self.service_mode.store(config.service_action.is_some(), Ordering::Relaxed);
        self.stop_action.store(config.service_action.unwrap_or(0), Ordering::Relaxed);
        self.auxiliary_default
            .store(config.auxiliary_default.unwrap_or(false), Ordering::Relaxed);
        drop(current_trace);
        // Historical callback/tuple audit is neither restored nor cleared.
        if self.trace("restore", -1, audit.call_count) {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        if self.main_only.load(Ordering::Relaxed) {
            // MNE4 state is configuration only. Audit is not a DSP checkpoint.
            let (bytes, len) = self.config().encode();
            return write_bytes(stream, &bytes[..len]);
        }
        let Ok(audit) = self.audit.try_lock() else {
            return kResultFalse;
        };
        let (configuration, prefix) = self.config().encode();
        // MNE1 retains its original 12-byte config and fixed MNA1 audit offsets.
        // MNE2 adds only its explicit four-byte default-active configuration.
        let mut bytes = vec![
            0_u8;
            prefix + HEADER_BYTES - CONFIG_BYTES
                + MAX_CALLS * CALL_BYTES
                + MAX_EVENTS * EVENT_BYTES
        ];
        bytes[..prefix].copy_from_slice(&configuration[..prefix]);
        bytes[prefix..prefix + 4].copy_from_slice(b"MNA1");
        bytes[prefix + 4..prefix + 8].copy_from_slice(&1_u32.to_le_bytes());
        bytes[prefix + 8..prefix + 16].copy_from_slice(&self.instance.to_le_bytes());
        bytes[prefix + 16..prefix + 20].copy_from_slice(&(audit.call_count as u32).to_le_bytes());
        bytes[prefix + 20..prefix + 24].copy_from_slice(&(audit.event_count as u32).to_le_bytes());
        bytes[prefix + 24..prefix + 28]
            .copy_from_slice(&u32::from(!audit.incomplete).to_le_bytes());
        bytes[prefix + 28..prefix + 36]
            .copy_from_slice(&self.sample_rate.load(Ordering::Relaxed).to_le_bytes());
        bytes[prefix + 36..prefix + 40]
            .copy_from_slice(&self.max_frames.load(Ordering::Relaxed).to_le_bytes());
        let mut cursor = prefix + HEADER_BYTES - CONFIG_BYTES;
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
        let was_processing = self.processing.swap(state != 0, Ordering::AcqRel);
        if self.service_mode.load(Ordering::Relaxed) && was_processing != (state != 0) {
            // Called on the real control thread, never from process(). A joined
            // scope does not stop processing: retained service owns the first stop.
            let Ok(audit) = self.audit.try_lock() else {
                return kResultFalse;
            };
            if !self.trace(
                if state != 0 { "start" } else { "stop" },
                -1,
                audit.call_count,
            ) {
                return kResultFalse;
            }
            if state == 0 {
                let action = self.stop_action.swap(0, Ordering::AcqRel);
                if action != 0 {
                    if action == 2 {
                        self.auxiliary_default.fetch_xor(true, Ordering::Relaxed);
                    }
                    if !self.trace("consume", 1, audit.call_count) {
                        return kResultFalse;
                    }
                }
            }
        }
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let Ok(mut audit) = self.audit.try_lock() else {
            return kResultFalse;
        };
        let main_only = self.main_only.load(Ordering::Relaxed);
        let call_limit = if main_only { MAIN_MAX_CALLS } else { MAX_CALLS };
        let event_limit = if main_only {
            MAIN_MAX_EVENTS
        } else {
            MAX_EVENTS
        };
        if audit.call_count == call_limit {
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
        let admission = self.admission_mode.load(Ordering::Relaxed);
        let active = self.active_buses.load(Ordering::Relaxed);
        if !self.processing.load(Ordering::Acquire)
            || if admission {
                active & 3 != 3
            } else if main_only {
                active != 7
            } else {
                active != 15
            }
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
            if count < 0 || count as usize > event_limit - first {
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
        if admission {
            if count != 0 {
                return kInvalidArgument;
            }
            // Separate MNE2 ordinary-effect oracle; legacy MNE1 Note DSP below
            // is unchanged. This mode never claims canonical Note execution.
            for frame in 0..data.numSamples as usize {
                *outputs[0].add(frame) = *inputs[0].add(frame) * 0.5 + 0.125;
                *outputs[1].add(frame) = *inputs[1].add(frame) * 0.5 - 0.0625;
            }
            audit.calls[call_index].result = kResultOk;
            return kResultOk;
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
    config: Cell<Config>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    main_thread: Cell<Option<ThreadId>>,
}
impl Class for NoteController {
    type Interfaces = (IEditController,);
}
impl IPluginBaseTrait for NoteController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        self.main_thread.set(Some(thread::current().id()));
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        self.handler.borrow_mut().take();
        self.main_thread.set(None);
        kResultOk
    }
}
impl IEditControllerTrait for NoteController {
    unsafe fn setComponentState(&self, stream: *mut IBStream) -> tresult {
        let Some(config) = read_config(stream) else {
            return kInvalidArgument;
        };
        // Component synchronization never issues the controller-private command.
        self.config.set(config);
        kResultOk
    }
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        if self.config.get().service_action.is_some() {
            if self.main_thread.get() != Some(thread::current().id()) {
                return kInvalidArgument;
            }
            let Some(stream) = ComRef::from_raw(stream) else {
                return kInvalidArgument;
            };
            let mut bytes = [0_u8; 12];
            let mut read = 0;
            if stream.read(bytes.as_mut_ptr().cast(), 12, &mut read) != kResultOk
                || read != 12
                || &bytes[..4] != b"MNC3"
                || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != 3
            {
                return kInvalidArgument;
            }
            match u32::from_le_bytes(bytes[8..12].try_into().unwrap()) {
                0 => return kResultOk,
                1 => {
                    // No RefCell borrow crosses the foreign callback. Its real
                    // successful ACK consumes this input-only command; getState
                    // saves controller configuration, never a pending cohort.
                    let Some(handler) = self.handler.borrow().clone() else {
                        return kResultFalse;
                    };
                    return handler.restartComponent(RestartFlags_::kLatencyChanged);
                }
                _ => return kInvalidArgument,
            }
        }
        let Some(config) = read_config(stream) else {
            return kInvalidArgument;
        };
        self.config.set(config);
        kResultOk
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        if self.config.get().service_action.is_some() {
            // MNC3 has no component Event-default field and no rearming command.
            return write_bytes(stream, b"MNC3\x03\0\0\0\0\0\0\0");
        }
        let (bytes, len) = self.config.get().encode();
        write_bytes(stream, &bytes[..len])
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
    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.borrow_mut() = ComRef::from_raw(handler).map(|handler| handler.to_com_ptr());
        kResultOk
    }
    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}
