use crate::{
    AudioNoteEndEvent, AudioNoteEvent, AudioNoteEventKind, AudioProcessingMode,
    AudioProcessorAuxiliaryInputBusContract, AudioProcessorAuxiliaryInputContract,
    AudioProcessorExecutionContract, AudioProcessorHostError, AudioProcessorNoteInputContract,
    AudioProcessorTail, AUDIO_NOTE_ENDS_PER_BLOCK, AUDIO_NOTE_EVENTS_PER_BLOCK,
};
use bytemuck::{Pod, Zeroable};
use mondrian_core::{AudioChannelLayout, ParameterId};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub(super) const PROTOCOL_VERSION: u32 = 18;
pub(super) const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;
pub(super) const MAX_PREPARATION_PAYLOAD_BYTES: usize = MAX_CONTROL_FRAME_BYTES / 2;

mod startup;
pub(super) use startup::{read_prepare_frame, EncodedPrepare};
pub(super) const MAX_WORKER_DETAIL_BYTES: usize = 4096;
const COMMAND_MAGIC: u32 = u32::from_le_bytes(*b"MAPC");
const RESPONSE_MAGIC: u32 = u32::from_le_bytes(*b"MAPR");

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(super) enum TailWire {
    None,
    Finite(u64),
    Infinite,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct ExecutionContractWire {
    pub algorithmic_latency_frames: u64,
    pub tail: TailWire,
    pub requires_state_entry: bool,
    pub realtime_capable: bool,
    pub offline_capable: bool,
    pub plugin_session_scratch_bytes: u64,
}

impl ExecutionContractWire {
    pub(super) fn capture(
        contract: AudioProcessorExecutionContract,
    ) -> Result<Self, AudioProcessorHostError> {
        Ok(Self {
            algorithmic_latency_frames: u64::try_from(contract.algorithmic_latency_frames())
                .map_err(|_| invalid_contract("processor latency is not protocol-representable"))?,
            tail: match contract.tail() {
                AudioProcessorTail::None => TailWire::None,
                AudioProcessorTail::Finite(frames) => {
                    TailWire::Finite(u64::try_from(frames).map_err(|_| {
                        invalid_contract("processor tail is not protocol-representable")
                    })?)
                }
                AudioProcessorTail::Infinite => TailWire::Infinite,
            },
            requires_state_entry: contract.requires_state_entry(),
            realtime_capable: contract.realtime_capable(),
            offline_capable: contract.offline_capable(),
            plugin_session_scratch_bytes: u64::try_from(contract.session_scratch_bytes())
                .map_err(|_| invalid_contract("processor scratch is not protocol-representable"))?,
        })
    }

    pub(super) fn realize(
        self,
    ) -> Result<AudioProcessorExecutionContract, AudioProcessorHostError> {
        let tail = match self.tail {
            TailWire::None => AudioProcessorTail::None,
            TailWire::Finite(frames) => AudioProcessorTail::Finite(
                usize::try_from(frames)
                    .map_err(|_| invalid_contract("Worker tail exceeds host address space"))?,
            ),
            TailWire::Infinite => AudioProcessorTail::Infinite,
        };
        AudioProcessorExecutionContract::new(
            usize::try_from(self.algorithmic_latency_frames)
                .map_err(|_| invalid_contract("Worker latency exceeds host address space"))?,
            tail,
            self.requires_state_entry,
            self.realtime_capable,
            self.offline_capable,
            usize::try_from(self.plugin_session_scratch_bytes)
                .map_err(|_| invalid_contract("Worker scratch exceeds host address space"))?,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct AuxiliaryBusWire {
    pub bus_key: String,
    pub channel_layout: AudioChannelLayout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareWire {
    pub schema_version: u32,
    pub native_lane: super::native_worker_lane_wire::NativeWorkerLaneWire,
    pub nonce: String,
    pub payload_bytes: u64,
    #[serde(deserialize_with = "required_musical_reference")]
    pub musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    pub execution_contract: ExecutionContractWire,
    pub auxiliary_buses: Vec<AuxiliaryBusWire>,
    pub auxiliary_outputs: Vec<AuxiliaryBusWire>,
    pub note_input_contract: AudioProcessorNoteInputContract,
    pub parameter_ids: Vec<ParameterId>,
    pub sample_rate: u32,
    pub channel_layout: AudioChannelLayout,
    pub max_block_frames: u64,
    pub realtime: bool,
    pub shared_path: String,
    pub shared_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadyWire {
    pub schema_version: u32,
    pub result: ReadyResultWire,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ReadyResultWire {
    Ready {
        #[serde(deserialize_with = "required_musical_reference")]
        musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
        execution_contract: ExecutionContractWire,
        auxiliary_buses: Vec<AuxiliaryBusWire>,
        auxiliary_outputs: Vec<AuxiliaryBusWire>,
        note_input_contract: AudioProcessorNoteInputContract,
        input_names: Vec<crate::AudioProcessorInputName>,
    },
    Rejected {
        detail: String,
    },
}

// A nullable value is required on the wire. Plain serde Option would silently
// accept a missing field, losing the prepared author clock.
fn required_musical_reference<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<mondrian_core::MusicalTimeReference>, D::Error> {
    Option::deserialize(deserializer)
}

pub(super) fn auxiliary_to_wire(
    contract: &AudioProcessorAuxiliaryInputContract,
) -> Vec<AuxiliaryBusWire> {
    contract
        .buses
        .iter()
        .map(|bus| AuxiliaryBusWire {
            bus_key: bus.bus_key.clone(),
            channel_layout: bus.channel_layout,
        })
        .collect()
}

pub(super) fn auxiliary_from_wire(
    buses: Vec<AuxiliaryBusWire>,
) -> AudioProcessorAuxiliaryInputContract {
    AudioProcessorAuxiliaryInputContract {
        buses: buses
            .into_iter()
            .map(|bus| AudioProcessorAuxiliaryInputBusContract {
                bus_key: bus.bus_key,
                channel_layout: bus.channel_layout,
            })
            .collect(),
    }
}

pub(super) fn outputs_to_wire(
    contract: &crate::AudioProcessorAuxiliaryOutputContract,
) -> Vec<AuxiliaryBusWire> {
    contract
        .buses
        .iter()
        .map(|bus| AuxiliaryBusWire {
            bus_key: bus.bus_key.clone(),
            channel_layout: bus.channel_layout,
        })
        .collect()
}

pub(super) fn outputs_from_wire(
    buses: Vec<AuxiliaryBusWire>,
) -> crate::AudioProcessorAuxiliaryOutputContract {
    crate::AudioProcessorAuxiliaryOutputContract {
        buses: buses
            .into_iter()
            .map(|bus| crate::AudioProcessorAuxiliaryOutputBusContract {
                bus_key: bus.bus_key,
                channel_layout: bus.channel_layout,
            })
            .collect(),
    }
}

pub(super) const fn mode_is_realtime(mode: AudioProcessingMode) -> bool {
    matches!(mode, AudioProcessingMode::Realtime)
}

pub(super) fn write_json_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if body.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "audio Worker control frame exceeds the protocol bound",
        ));
    }
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "control frame is too large"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()
}

pub(super) fn read_json_frame<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
) -> io::Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid control frame length"))?;
    if length > MAX_CONTROL_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "audio Worker control frame exceeds the protocol bound",
        ));
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandKind {
    EnterState,
    Process,
    Shutdown,
    PollPending,
    ServiceConfiguration,
    PollConfigurationCohort,
    StageConfigurationSeed,
    CommitConfigurationSeed,
    PollNativeObservations,
    BindNativeObject,
    EditorOpen,
    EditorClose,
    PrepareAuthorStateEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkerCommand {
    pub kind: CommandKind,
    pub start_sample: i64,
    pub frames: u32,
    pub event_count: u32,
    pub note_count: u32,
}

impl WorkerCommand {
    pub(super) fn write_to(self, writer: &mut impl Write) -> io::Result<()> {
        validate_note_count(self.kind, self.note_count)?;
        let kind = match self.kind {
            CommandKind::EnterState => 1_u32,
            CommandKind::Process => 2,
            CommandKind::Shutdown => 3,
            CommandKind::PollPending => 4,
            CommandKind::ServiceConfiguration => 5,
            CommandKind::PollConfigurationCohort => 6,
            CommandKind::StageConfigurationSeed => 7,
            CommandKind::CommitConfigurationSeed => 8,
            CommandKind::PollNativeObservations => 9,
            CommandKind::BindNativeObject => 10,
            CommandKind::EditorOpen => 11,
            CommandKind::EditorClose => 12,
            CommandKind::PrepareAuthorStateEntry => 13,
        };
        let mut bytes = [0_u8; 32];
        bytes[0..4].copy_from_slice(&COMMAND_MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        bytes[8..12].copy_from_slice(&kind.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.frames.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.start_sample.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.event_count.to_le_bytes());
        bytes[28..32].copy_from_slice(&self.note_count.to_le_bytes());
        writer.write_all(&bytes)?;
        writer.flush()
    }

    pub(super) fn read_from(reader: &mut impl Read) -> io::Result<Self> {
        let mut bytes = [0_u8; 32];
        reader.read_exact(&mut bytes)?;
        if u32::from_le_bytes(bytes[0..4].try_into().map_err(invalid_slice)?) != COMMAND_MAGIC
            || u32::from_le_bytes(bytes[4..8].try_into().map_err(invalid_slice)?)
                != PROTOCOL_VERSION
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid audio Worker command header",
            ));
        }
        let kind = match u32::from_le_bytes(bytes[8..12].try_into().map_err(invalid_slice)?) {
            1 => CommandKind::EnterState,
            2 => CommandKind::Process,
            3 => CommandKind::Shutdown,
            4 => CommandKind::PollPending,
            5 => CommandKind::ServiceConfiguration,
            6 => CommandKind::PollConfigurationCohort,
            7 => CommandKind::StageConfigurationSeed,
            8 => CommandKind::CommitConfigurationSeed,
            9 => CommandKind::PollNativeObservations,
            10 => CommandKind::BindNativeObject,
            11 => CommandKind::EditorOpen,
            12 => CommandKind::EditorClose,
            13 => CommandKind::PrepareAuthorStateEntry,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown audio Worker command",
                ));
            }
        };
        let command = Self {
            kind,
            frames: u32::from_le_bytes(bytes[12..16].try_into().map_err(invalid_slice)?),
            start_sample: i64::from_le_bytes(bytes[16..24].try_into().map_err(invalid_slice)?),
            event_count: u32::from_le_bytes(bytes[24..28].try_into().map_err(invalid_slice)?),
            note_count: u32::from_le_bytes(bytes[28..32].try_into().map_err(invalid_slice)?),
        };
        validate_note_count(command.kind, command.note_count)?;
        if matches!(
            command.kind,
            CommandKind::PollPending
                | CommandKind::ServiceConfiguration
                | CommandKind::PollConfigurationCohort
                | CommandKind::PollNativeObservations
                | CommandKind::BindNativeObject
                | CommandKind::EditorOpen
                | CommandKind::EditorClose
                | CommandKind::PrepareAuthorStateEntry
        ) && (command.start_sample != 0 || command.frames != 0 || command.event_count != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pending query has no sample/block payload",
            ));
        }
        Ok(command)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkerResponse {
    Completed,
    CompletedWithPending(crate::NativeObservedRequest),
    NotesCompleted {
        note_end_count: u32,
        pending: Option<crate::NativeObservedRequest>,
    },
    DeferredForConfiguration(crate::NativeObservedRequest),
    Failed(String),
}

impl WorkerResponse {
    pub(super) fn write_to(self, writer: &mut impl Write) -> io::Result<()> {
        let (status, payload) = match &self {
            Self::Completed => (0_u32, None),
            Self::Failed(_) => (1, None),
            Self::CompletedWithPending(request) | Self::DeferredForConfiguration(request) => {
                let mut bytes = [0_u8; 20];
                bytes[..16].copy_from_slice(
                    &super::native_configuration_wire::encode_observed_request(Some(*request))?,
                );
                (
                    if matches!(self, Self::DeferredForConfiguration(_)) {
                        3
                    } else {
                        2
                    },
                    Some((bytes, 16)),
                )
            }
            Self::NotesCompleted { note_end_count, pending } => {
                if (*note_end_count as usize) > AUDIO_NOTE_ENDS_PER_BLOCK {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Note End count exceeds protocol bound",
                    ));
                }
                let mut bytes = [0_u8; 20];
                bytes[..4].copy_from_slice(&note_end_count.to_le_bytes());
                bytes[4..].copy_from_slice(
                    &super::native_configuration_wire::encode_observed_request(*pending)?,
                );
                (4, Some((bytes, 20)))
            }
        };
        let detail = match self {
            Self::Failed(detail) => Some(bounded_detail(detail)),
            _ => None,
        };
        let length = payload.as_ref().map_or_else(
            || detail.as_ref().map_or(0, String::len),
            |(_, length)| *length,
        );
        let response_len = u32::try_from(length)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Worker detail too large"))?;
        let mut header = [0_u8; 16];
        header[0..4].copy_from_slice(&RESPONSE_MAGIC.to_le_bytes());
        header[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        header[8..12].copy_from_slice(&status.to_le_bytes());
        header[12..16].copy_from_slice(&response_len.to_le_bytes());
        writer.write_all(&header)?;
        if let Some((bytes, length)) = payload {
            writer.write_all(&bytes[..length])?;
        }
        if let Some(detail) = detail {
            writer.write_all(detail.as_bytes())?;
        }
        writer.flush()
    }

    pub(super) fn read_from(reader: &mut impl Read) -> io::Result<Self> {
        let mut header = [0_u8; 16];
        reader.read_exact(&mut header)?;
        if u32::from_le_bytes(header[0..4].try_into().map_err(invalid_slice)?) != RESPONSE_MAGIC
            || u32::from_le_bytes(header[4..8].try_into().map_err(invalid_slice)?)
                != PROTOCOL_VERSION
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid audio Worker response header",
            ));
        }
        let status = u32::from_le_bytes(header[8..12].try_into().map_err(invalid_slice)?);
        let detail_len = usize::try_from(u32::from_le_bytes(
            header[12..16].try_into().map_err(invalid_slice)?,
        ))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Worker detail length"))?;
        if detail_len > MAX_WORKER_DETAIL_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "audio Worker detail exceeds the protocol bound",
            ));
        }
        if status == 4 {
            if detail_len != 20 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid Note response extent",
                ));
            }
            let mut count = [0_u8; 4];
            reader.read_exact(&mut count)?;
            let count = u32::from_le_bytes(count);
            if (count as usize) > AUDIO_NOTE_ENDS_PER_BLOCK {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Note End count exceeds protocol bound",
                ));
            }
            let pending = super::native_configuration_wire::read_observed_request(reader)?;
            return Ok(Self::NotesCompleted { note_end_count: count, pending });
        }
        if status == 2 || status == 3 {
            if detail_len != 16 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid pending response extent",
                ));
            }
            let request = super::native_configuration_wire::read_observed_request(reader)?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "pending response has no request",
                    )
                })?;
            return Ok(if status == 3 {
                Self::DeferredForConfiguration(request)
            } else {
                Self::CompletedWithPending(request)
            });
        }
        let mut detail = vec![0_u8; detail_len];
        reader.read_exact(&mut detail)?;
        let detail = String::from_utf8(detail)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        match status {
            0 if detail.is_empty() => Ok(Self::Completed),
            1 => Ok(Self::Failed(detail)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid audio Worker response status",
            )),
        }
    }
}

fn bounded_detail(mut detail: String) -> String {
    if detail.len() <= MAX_WORKER_DETAIL_BYTES {
        return detail;
    }
    let mut boundary = MAX_WORKER_DETAIL_BYTES;
    while !detail.is_char_boundary(boundary) {
        boundary -= 1;
    }
    detail.truncate(boundary);
    detail
}

fn invalid_slice(_: std::array::TryFromSliceError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid fixed protocol field")
}

fn validate_note_count(kind: CommandKind, count: u32) -> io::Result<()> {
    if (count as usize) > AUDIO_NOTE_EVENTS_PER_BLOCK
        || (kind != CommandKind::Process && count != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Note command count",
        ));
    }
    Ok(())
}

fn invalid_contract(detail: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.to_owned())
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub(super) struct SharedParameterEvent {
    pub lane: u32,
    pub sample_offset: u32,
    pub value: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub(super) struct SharedNoteEvent {
    pub sample_offset: u32,
    pub native_id: i32,
    pub port_index: i16,
    pub channel: i16,
    pub key: i16,
    pub kind: u16,
    pub velocity: f64,
}

impl SharedNoteEvent {
    pub(super) fn capture(
        event: AudioNoteEvent,
        frames: usize,
        ports: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        if !event.valid(frames, ports) {
            return Err(invalid_contract("invalid input Note event"));
        }
        Ok(Self {
            sample_offset: event.sample_offset,
            native_id: event.native_note_id,
            port_index: i16::try_from(event.port_index)
                .map_err(|_| invalid_contract("Note input port is not protocol-representable"))?,
            channel: i16::from(event.channel),
            key: i16::from(event.key),
            kind: match event.kind {
                AudioNoteEventKind::Off => 0,
                AudioNoteEventKind::On => 1,
            },
            velocity: event.velocity,
        })
    }

    pub(super) fn realize(
        self,
        frames: usize,
        ports: usize,
    ) -> Result<AudioNoteEvent, AudioProcessorHostError> {
        let event = AudioNoteEvent {
            sample_offset: self.sample_offset,
            native_note_id: self.native_id,
            port_index: u16::try_from(self.port_index)
                .map_err(|_| invalid_contract("invalid input Note port"))?,
            channel: u8::try_from(self.channel)
                .map_err(|_| invalid_contract("invalid input Note channel"))?,
            key: u8::try_from(self.key).map_err(|_| invalid_contract("invalid input Note key"))?,
            kind: match self.kind {
                0 => AudioNoteEventKind::Off,
                1 => AudioNoteEventKind::On,
                _ => return Err(invalid_contract("invalid input Note phase")),
            },
            velocity: self.velocity,
        };
        if !event.valid(frames, ports) {
            return Err(invalid_contract("invalid input Note event"));
        }
        Ok(event)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub(super) struct SharedNoteEnd {
    pub native_id: i32,
    pub port_index: i16,
    pub channel: i16,
    pub key: i16,
    pub reserved: u16,
    pub flags: u32,
}

impl SharedNoteEnd {
    pub(super) fn capture(
        event: AudioNoteEndEvent,
        ports: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        if !event.valid(ports) {
            return Err(invalid_contract("invalid Note End address/flags"));
        }
        Ok(Self {
            native_id: event.native_note_id,
            port_index: event.port_index,
            channel: event.channel,
            key: event.key,
            reserved: 0,
            flags: event.flags,
        })
    }

    pub(super) fn realize(
        self,
        ports: usize,
    ) -> Result<AudioNoteEndEvent, AudioProcessorHostError> {
        let event = AudioNoteEndEvent {
            native_note_id: self.native_id,
            port_index: self.port_index,
            channel: self.channel,
            key: self.key,
            flags: self.flags,
        };
        if self.reserved != 0 || !event.valid(ports) {
            return Err(invalid_contract(
                "invalid copied Note End address/flags/reserved field",
            ));
        }
        Ok(event)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SharedBlockLayout {
    pub main_offset: usize,
    pub main_samples: usize,
    pub auxiliary_offset: usize,
    pub auxiliary_samples: usize,
    pub auxiliary_outputs_offset: usize,
    pub auxiliary_outputs_samples: usize,
    pub events_offset: usize,
    pub event_capacity: usize,
    pub note_events_offset: usize,
    pub note_event_capacity: usize,
    pub note_ends_offset: usize,
    pub note_end_capacity: usize,
    pub total_bytes: usize,
}

impl SharedBlockLayout {
    pub(super) fn new(
        max_block_frames: usize,
        channel_count: usize,
        auxiliary_bus_count: usize,
        parameter_lane_count: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        let main_samples = max_block_frames
            .checked_mul(channel_count)
            .ok_or_else(|| invalid_contract("isolated main-bus capacity overflowed"))?;
        let auxiliary_samples = main_samples
            .checked_mul(auxiliary_bus_count)
            .ok_or_else(|| invalid_contract("isolated auxiliary-bus capacity overflowed"))?;
        let event_capacity = max_block_frames
            .checked_mul(parameter_lane_count)
            .ok_or_else(|| invalid_contract("isolated parameter-event capacity overflowed"))?;
        let main_bytes = main_samples
            .checked_mul(size_of::<f32>())
            .ok_or_else(|| invalid_contract("isolated main-bus bytes overflowed"))?;
        let auxiliary_bytes = auxiliary_samples
            .checked_mul(size_of::<f32>())
            .ok_or_else(|| invalid_contract("isolated auxiliary-bus bytes overflowed"))?;
        let events_offset = align_up(
            main_bytes
                .checked_add(auxiliary_bytes)
                .ok_or_else(|| invalid_contract("isolated PCM bytes overflowed"))?,
            align_of::<SharedParameterEvent>(),
        )?;
        let event_bytes = event_capacity
            .checked_mul(size_of::<SharedParameterEvent>())
            .ok_or_else(|| invalid_contract("isolated parameter bytes overflowed"))?;
        let total_bytes = events_offset
            .checked_add(event_bytes)
            .ok_or_else(|| invalid_contract("isolated shared storage overflowed"))?;
        if total_bytes == 0 {
            return Err(invalid_contract("isolated shared storage cannot be empty"));
        }
        Ok(Self {
            main_offset: 0,
            main_samples,
            auxiliary_offset: main_bytes,
            auxiliary_samples,
            auxiliary_outputs_offset: events_offset,
            auxiliary_outputs_samples: 0,
            events_offset,
            event_capacity,
            note_events_offset: total_bytes,
            note_event_capacity: 0,
            note_ends_offset: total_bytes,
            note_end_capacity: 0,
            total_bytes,
        })
    }

    pub(super) fn with_auxiliary_outputs(
        mut self,
        bus_count: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        if bus_count > 31
            || self.auxiliary_outputs_samples != 0
            || self.note_event_capacity != 0
            || self.note_end_capacity != 0
        {
            return Err(invalid_contract(
                "invalid or repeated isolated output capacity",
            ));
        }
        let samples = self
            .main_samples
            .checked_mul(bus_count)
            .ok_or_else(|| invalid_contract("isolated output sample extent overflowed"))?;
        let bytes = samples
            .checked_mul(size_of::<f32>())
            .ok_or_else(|| invalid_contract("isolated output byte extent overflowed"))?;
        self.auxiliary_outputs_samples = samples;
        self.events_offset = align_up(
            self.auxiliary_outputs_offset
                .checked_add(bytes)
                .ok_or_else(|| invalid_contract("isolated output range overflowed"))?,
            align_of::<SharedParameterEvent>(),
        )?;
        self.total_bytes = self
            .events_offset
            .checked_add(
                self.event_capacity
                    .checked_mul(size_of::<SharedParameterEvent>())
                    .ok_or_else(|| invalid_contract("isolated parameter bytes overflowed"))?,
            )
            .ok_or_else(|| invalid_contract("isolated shared output storage overflowed"))?;
        self.note_events_offset = self.total_bytes;
        self.note_ends_offset = self.total_bytes;
        Ok(self)
    }

    pub(super) fn with_notes(
        mut self,
        input_capacity: usize,
        end_capacity: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        if input_capacity > AUDIO_NOTE_EVENTS_PER_BLOCK
            || end_capacity > AUDIO_NOTE_ENDS_PER_BLOCK
            || self.note_event_capacity != 0
            || self.note_end_capacity != 0
        {
            return Err(invalid_contract(
                "invalid or repeated isolated Note capacities",
            ));
        }
        if input_capacity == 0 && end_capacity == 0 {
            return Ok(self);
        }
        self.note_events_offset = align_up(self.total_bytes, align_of::<SharedNoteEvent>())?;
        let note_bytes = input_capacity
            .checked_mul(size_of::<SharedNoteEvent>())
            .ok_or_else(|| invalid_contract("isolated Note input extent overflowed"))?;
        self.note_ends_offset = align_up(
            self.note_events_offset
                .checked_add(note_bytes)
                .ok_or_else(|| invalid_contract("isolated Note input range overflowed"))?,
            align_of::<SharedNoteEnd>(),
        )?;
        let end_bytes = end_capacity
            .checked_mul(size_of::<SharedNoteEnd>())
            .ok_or_else(|| invalid_contract("isolated Note End extent overflowed"))?;
        self.total_bytes = self
            .note_ends_offset
            .checked_add(end_bytes)
            .ok_or_else(|| invalid_contract("isolated Note storage overflowed"))?;
        self.note_event_capacity = input_capacity;
        self.note_end_capacity = end_capacity;
        Ok(self)
    }
}

fn align_up(value: usize, alignment: usize) -> Result<usize, AudioProcessorHostError> {
    let mask = alignment
        .checked_sub(1)
        .ok_or_else(|| invalid_contract("invalid shared-storage alignment"))?;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or_else(|| invalid_contract("shared-storage alignment overflowed"))
}

#[cfg(test)]
mod response_write_qualification {
    use super::*;
    use std::io::Cursor;

    struct RecordedWriter {
        bytes: [u8; MAX_WORKER_DETAIL_BYTES + 16],
        length: usize,
        nonempty_writes: usize,
        flushes: usize,
        max_write: usize,
        interrupts: usize,
        write_error: Option<io::ErrorKind>,
        flush_error: Option<io::ErrorKind>,
    }

    impl RecordedWriter {
        fn new(max_write: usize) -> Self {
            Self {
                bytes: [0; MAX_WORKER_DETAIL_BYTES + 16],
                length: 0,
                nonempty_writes: 0,
                flushes: 0,
                max_write,
                interrupts: 0,
                write_error: None,
                flush_error: None,
            }
        }

        fn written(&self) -> &[u8] {
            &self.bytes[..self.length]
        }
    }

    impl Write for RecordedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.is_empty() {
                return Ok(0);
            }
            self.nonempty_writes += 1;
            if self.interrupts > 0 {
                self.interrupts -= 1;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if let Some(error) = self.write_error {
                return Err(io::Error::from(error));
            }
            let accepted = bytes.len().min(self.max_write);
            if accepted > self.bytes.len() - self.length {
                return Err(io::Error::from(io::ErrorKind::OutOfMemory));
            }
            self.bytes[self.length..self.length + accepted].copy_from_slice(&bytes[..accepted]);
            self.length += accepted;
            Ok(accepted)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            self.flush_error.map_or(Ok(()), |error| Err(io::Error::from(error)))
        }
    }

    // Independent protocol-v18 wire bytes: MAPR, version=18, success=0, detail=0.
    const COMPLETED_WIRE: [u8; 16] = [77, 65, 80, 82, 18, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    #[test]
    fn native_response_wire_completed_has_one_nonempty_header_write() -> io::Result<()> {
        let mut writer = RecordedWriter::new(usize::MAX);
        WorkerResponse::Completed.write_to(&mut writer)?;
        assert_eq!(writer.written(), COMPLETED_WIRE);
        for version in [8_u32, 9, 10, 11, 12, 13, 14, 15, 16, 17, 19] {
            let mut old = COMPLETED_WIRE;
            old[4..8].copy_from_slice(&version.to_le_bytes());
            assert!(WorkerResponse::read_from(&mut old.as_slice()).is_err());
        }
        assert_eq!(writer.nonempty_writes, 1);
        assert_eq!(writer.flushes, 1);
        assert_eq!(
            WorkerResponse::read_from(&mut Cursor::new(writer.written()))?,
            WorkerResponse::Completed
        );
        Ok(())
    }

    #[test]
    fn native_response_wire_failed_has_one_header_and_one_body_write() -> io::Result<()> {
        let mut writer = RecordedWriter::new(usize::MAX);
        WorkerResponse::Failed("bad-λ".to_owned()).write_to(&mut writer)?;
        let expected = [
            77, 65, 80, 82, 18, 0, 0, 0, 1, 0, 0, 0, 6, 0, 0, 0, 98, 97, 100, 45, 206, 187,
        ];
        assert_eq!(writer.written(), expected);
        assert_eq!(writer.nonempty_writes, 2);
        assert_eq!(writer.flushes, 1);
        assert_eq!(
            WorkerResponse::read_from(&mut Cursor::new(writer.written()))?,
            WorkerResponse::Failed("bad-λ".to_owned()),
        );
        Ok(())
    }

    #[test]
    fn native_response_wire_partial_header_and_detail_keep_exact_order() -> io::Result<()> {
        let mut writer = RecordedWriter::new(3);
        WorkerResponse::Failed("bad-λ".to_owned()).write_to(&mut writer)?;
        let expected = [
            77, 65, 80, 82, 18, 0, 0, 0, 1, 0, 0, 0, 6, 0, 0, 0, 98, 97, 100, 45, 206, 187,
        ];
        assert_eq!(writer.written(), expected);
        assert_eq!(writer.nonempty_writes, 8);
        assert_eq!(writer.flushes, 1);
        Ok(())
    }

    #[test]
    fn native_response_wire_interrupted_write_retries_without_duplicate_bytes() -> io::Result<()> {
        let mut writer = RecordedWriter::new(usize::MAX);
        writer.interrupts = 1;
        WorkerResponse::Completed.write_to(&mut writer)?;
        assert_eq!(writer.written(), COMPLETED_WIRE);
        assert_eq!(writer.nonempty_writes, 2);
        assert_eq!(writer.flushes, 1);
        Ok(())
    }

    #[test]
    fn native_response_wire_utf8_bound_is_unchanged() -> io::Result<()> {
        let mut writer = RecordedWriter::new(usize::MAX);
        WorkerResponse::Failed("界".repeat(1367)).write_to(&mut writer)?;
        // 4096 cannot divide a three-byte UTF-8 code point: retain exactly 4095.
        assert_eq!(writer.length, 16 + 4095);
        assert_eq!(&writer.written()[12..16], &[255, 15, 0, 0]);
        assert_eq!(writer.nonempty_writes, 2);
        assert_eq!(
            WorkerResponse::read_from(&mut Cursor::new(writer.written()))?,
            WorkerResponse::Failed("界".repeat(1365)),
        );
        Ok(())
    }

    #[test]
    fn native_response_wire_zero_write_refuses_without_flush() {
        let mut writer = RecordedWriter::new(0);
        let result = WorkerResponse::Completed.write_to(&mut writer);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::WriteZero));
        assert_eq!(writer.length, 0);
        assert_eq!(writer.flushes, 0);
    }

    #[test]
    fn native_response_wire_terminal_write_error_preserves_failure() {
        let mut writer = RecordedWriter::new(usize::MAX);
        writer.write_error = Some(io::ErrorKind::BrokenPipe);
        let result = WorkerResponse::Completed.write_to(&mut writer);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::BrokenPipe));
        assert_eq!(writer.length, 0);
        assert_eq!(writer.flushes, 0);
    }

    #[test]
    fn native_response_wire_flush_error_does_not_claim_completion() {
        let mut writer = RecordedWriter::new(usize::MAX);
        writer.flush_error = Some(io::ErrorKind::ConnectionReset);
        let result = WorkerResponse::Completed.write_to(&mut writer);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::ConnectionReset));
        assert_eq!(writer.written(), COMPLETED_WIRE);
        assert_eq!(writer.flushes, 1);
    }
}

#[cfg(test)]
mod pending_tests {
    use super::*;
    use mondrian_core::NativeAudioReprepareRequest;

    #[test]
    fn native_pending_wire_round_trips_fixed_payload() {
        use crate::{NativeObservedRequest as Observed, NativeRequestCohort};
        for (request, words) in [
            (
                Observed::RequestOnly(NativeAudioReprepareRequest::ClapRestart),
                [1_u32, 0, 2, 0],
            ),
            (
                Observed::RequestOnly(NativeAudioReprepareRequest::Vst3Restart {
                    flags: 0x8000_0008,
                }),
                [1, 0, 1, 0x8000_0008],
            ),
            (
                Observed::Cohort(
                    NativeRequestCohort::new(
                        7,
                        NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
                    )
                    .unwrap(),
                ),
                [2, 7, 1, 4],
            ),
            (
                Observed::Cohort(
                    NativeRequestCohort::new(u32::MAX, NativeAudioReprepareRequest::ClapRestart)
                        .unwrap(),
                ),
                [2, u32::MAX, 2, 0],
            ),
        ] {
            for response in [
                WorkerResponse::CompletedWithPending(request),
                WorkerResponse::DeferredForConfiguration(request),
            ] {
                let mut encoded = Vec::new();
                response.clone().write_to(&mut encoded).expect("encode original pending fact");
                assert_eq!(
                    encoded.len(),
                    32,
                    "fixed header plus exact sixteen-byte carrier"
                );
                assert_eq!(&encoded[..8], &[77, 65, 80, 82, 18, 0, 0, 0]);
                let status = if matches!(response, WorkerResponse::DeferredForConfiguration(_)) {
                    3_u32
                } else {
                    2
                };
                assert_eq!(&encoded[8..12], &status.to_le_bytes());
                assert_eq!(&encoded[12..16], &16_u32.to_le_bytes());
                for (index, word) in words.into_iter().enumerate() {
                    assert_eq!(
                        &encoded[16 + 4 * index..20 + 4 * index],
                        &word.to_le_bytes()
                    );
                }
                assert_eq!(
                    WorkerResponse::read_from(&mut encoded.as_slice()).unwrap(),
                    response
                );
            }
        }
        let mut encoded = Vec::new();
        let query = WorkerCommand {
            kind: CommandKind::PollPending,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        };
        query.write_to(&mut encoded).unwrap();
        assert_eq!(
            WorkerCommand::read_from(&mut encoded.as_slice()).unwrap(),
            query
        );
        encoded[12..16].copy_from_slice(&1_u32.to_le_bytes());
        assert!(
            WorkerCommand::read_from(&mut encoded.as_slice()).is_err(),
            "pending query cannot overload block fields"
        );
    }

    #[test]
    fn native_pending_wire_refuses_invalid_extent_kind_and_zero_vst_flags() {
        use crate::NativeObservedRequest::RequestOnly;
        let mut canonical = Vec::new();
        WorkerResponse::CompletedWithPending(RequestOnly(NativeAudioReprepareRequest::ClapRestart))
            .write_to(&mut canonical)
            .unwrap();
        for (offset, value) in [
            (12, 0_u32),
            (16, 9),
            (20, 1),
            (24, 9),
            (28, 1),
            (4, 17),
            (4, 19),
        ] {
            let mut invalid = canonical.clone();
            invalid[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(WorkerResponse::read_from(&mut invalid.as_slice()).is_err());
        }
        assert!(WorkerResponse::CompletedWithPending(RequestOnly(
            NativeAudioReprepareRequest::Vst3Restart { flags: 0 }
        ))
        .write_to(&mut Vec::new())
        .is_err());
    }
}

#[cfg(test)]
mod note_protocol_tests {
    use super::*;
    use mondrian_core::NativeAudioReprepareRequest;

    fn event() -> AudioNoteEvent {
        AudioNoteEvent {
            sample_offset: 1,
            kind: AudioNoteEventKind::On,
            port_index: 0,
            channel: 15,
            key: 127,
            native_note_id: i32::MAX,
            velocity: f64::from_bits(0x3fd3_3333_3333_3335),
        }
    }

    #[test]
    fn note_pod_has_exact_field_offsets_and_preserves_original_velocity_bits() {
        assert_eq!(size_of::<SharedNoteEvent>(), 24);
        assert_eq!(align_of::<SharedNoteEvent>(), 8);
        assert_eq!(std::mem::offset_of!(SharedNoteEvent, sample_offset), 0);
        assert_eq!(std::mem::offset_of!(SharedNoteEvent, native_id), 4);
        assert_eq!(std::mem::offset_of!(SharedNoteEvent, port_index), 8);
        assert_eq!(std::mem::offset_of!(SharedNoteEvent, channel), 10);
        assert_eq!(std::mem::offset_of!(SharedNoteEvent, key), 12);
        assert_eq!(std::mem::offset_of!(SharedNoteEvent, kind), 14);
        assert_eq!(std::mem::offset_of!(SharedNoteEvent, velocity), 16);
        assert_eq!(size_of::<SharedNoteEnd>(), 16);
        assert_eq!(align_of::<SharedNoteEnd>(), 4);
        assert_eq!(std::mem::offset_of!(SharedNoteEnd, reserved), 10);
        assert_eq!(std::mem::offset_of!(SharedNoteEnd, flags), 12);
        let original = event();
        let packed = SharedNoteEvent::capture(original, 2, 1).unwrap();
        let restored = packed.realize(2, 1).unwrap();
        assert_eq!(restored, original);
        assert_eq!(restored.velocity.to_bits(), 0x3fd3_3333_3333_3335);
        let zero_on = AudioNoteEvent { velocity: -0.0, ..original };
        let restored = SharedNoteEvent::capture(zero_on, 2, 1).unwrap().realize(2, 1).unwrap();
        assert_eq!(restored.kind, AudioNoteEventKind::On);
        assert_eq!(restored.velocity.to_bits(), (-0.0_f64).to_bits());
    }

    #[test]
    fn note_free_layout_retains_exact_pcm_parameter_prefix_and_notes_append() {
        let old = SharedBlockLayout::new(3, 2, 1, 2).unwrap();
        assert_eq!(
            (
                old.main_offset,
                old.main_samples,
                old.auxiliary_offset,
                old.auxiliary_samples
            ),
            (0, 6, 24, 6)
        );
        assert_eq!(
            (old.events_offset, old.event_capacity, old.total_bytes),
            (48, 6, 144)
        );
        assert_eq!(old.with_notes(0, 0).unwrap(), old);
        let notes = old.with_notes(1, 1).unwrap();
        assert_eq!(
            (
                notes.main_offset,
                notes.auxiliary_offset,
                notes.events_offset,
                notes.event_capacity
            ),
            (0, 24, 48, 6)
        );
        assert_eq!(
            (
                notes.note_events_offset,
                notes.note_ends_offset,
                notes.total_bytes
            ),
            (144, 168, 184)
        );
        let max = old.with_notes(4096, 4096).unwrap();
        assert_eq!(max.total_bytes, 144 + 4096 * 24 + 4096 * 16);
        assert!(old.with_notes(4097, 1).is_err());
        assert!(old.with_notes(1, 4097).is_err());
        assert!(notes.with_notes(1, 1).is_err());
        let mut overflow = old;
        overflow.total_bytes = usize::MAX;
        assert!(overflow.with_notes(1, 1).is_err());
    }

    #[test]
    fn command_uses_last_four_bytes_and_refuses_old_version_or_nonprocess_notes() {
        let command = WorkerCommand {
            kind: CommandKind::Process,
            start_sample: -9,
            frames: 2,
            event_count: 3,
            note_count: 4096,
        };
        let mut bytes = Vec::new();
        command.write_to(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 32);
        assert_eq!(&bytes[28..32], &4096_u32.to_le_bytes());
        assert_eq!(
            WorkerCommand::read_from(&mut bytes.as_slice()).unwrap(),
            command
        );
        for version in [8_u32, 9, 10, 11, 12, 13, 14, 15, 16, 17, 19] {
            let mut old = bytes.clone();
            old[4..8].copy_from_slice(&version.to_le_bytes());
            assert!(WorkerCommand::read_from(&mut old.as_slice()).is_err());
        }
        let mut too_many = bytes.clone();
        too_many[28..32].copy_from_slice(&4097_u32.to_le_bytes());
        assert!(WorkerCommand::read_from(&mut too_many.as_slice()).is_err());
        for kind in [
            CommandKind::EnterState,
            CommandKind::Shutdown,
            CommandKind::PollPending,
            CommandKind::StageConfigurationSeed,
        ] {
            assert!(WorkerCommand { kind, ..command }.write_to(&mut Vec::new()).is_err());
        }
        bytes[8..12].copy_from_slice(&1_u32.to_le_bytes());
        assert!(WorkerCommand::read_from(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn notes_completed_fixed_bytes_preserve_none_clap_and_raw_vst_pending() {
        use crate::{NativeObservedRequest as Observed, NativeRequestCohort};
        for (pending, words) in [
            (None, [0_u32, 0, 0, 0]),
            (
                Some(Observed::RequestOnly(
                    NativeAudioReprepareRequest::ClapRestart,
                )),
                [1, 0, 2, 0],
            ),
            (
                Some(Observed::RequestOnly(
                    NativeAudioReprepareRequest::Vst3Restart { flags: 0x8000_0008 },
                )),
                [1, 0, 1, 0x8000_0008],
            ),
            (
                Some(Observed::Cohort(
                    NativeRequestCohort::new(
                        11,
                        NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
                    )
                    .unwrap(),
                )),
                [2, 11, 1, 4],
            ),
            (
                Some(Observed::Cohort(
                    NativeRequestCohort::new(u32::MAX, NativeAudioReprepareRequest::ClapRestart)
                        .unwrap(),
                )),
                [2, u32::MAX, 2, 0],
            ),
        ] {
            let response = WorkerResponse::NotesCompleted { note_end_count: 4096, pending };
            let mut bytes = Vec::new();
            response.clone().write_to(&mut bytes).unwrap();
            assert_eq!(bytes.len(), 36);
            assert_eq!(
                &bytes[..16],
                &[77, 65, 80, 82, 18, 0, 0, 0, 4, 0, 0, 0, 20, 0, 0, 0]
            );
            assert_eq!(&bytes[16..20], &4096_u32.to_le_bytes());
            for (index, word) in words.into_iter().enumerate() {
                assert_eq!(&bytes[20 + 4 * index..24 + 4 * index], &word.to_le_bytes());
            }
            assert_eq!(
                WorkerResponse::read_from(&mut bytes.as_slice()).unwrap(),
                response
            );
        }
        let response = WorkerResponse::NotesCompleted { note_end_count: 0, pending: None };
        let mut bytes = Vec::new();
        response.clone().write_to(&mut bytes).unwrap();
        assert_eq!(
            WorkerResponse::read_from(&mut bytes.as_slice()).unwrap(),
            response
        );
    }

    #[test]
    fn notes_completed_rejects_count_extent_old_version_and_noncanonical_pending() {
        use crate::NativeObservedRequest::RequestOnly;
        let mut bytes = Vec::new();
        WorkerResponse::NotesCompleted { note_end_count: 1, pending: None }
            .write_to(&mut bytes)
            .unwrap();
        for (offset, value) in [
            (4, 8_u32),
            (4, 9),
            (4, 10),
            (4, 11),
            (4, 17),
            (4, 19),
            (12, 12),
            (16, 4097),
            (20, 3),
            (24, 1),
            (28, 1),
            (32, 1),
        ] {
            let mut invalid = bytes.clone();
            invalid[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(WorkerResponse::read_from(&mut invalid.as_slice()).is_err());
        }
        for (kind, flags) in [(1_u32, 0_u32), (2, 1), (0, 9)] {
            let mut invalid = bytes.clone();
            invalid[20..24].copy_from_slice(&1_u32.to_le_bytes());
            invalid[28..32].copy_from_slice(&kind.to_le_bytes());
            invalid[32..36].copy_from_slice(&flags.to_le_bytes());
            assert!(WorkerResponse::read_from(&mut invalid.as_slice()).is_err());
        }
        assert!(
            WorkerResponse::NotesCompleted { note_end_count: 4097, pending: None }
                .write_to(&mut Vec::new())
                .is_err()
        );
        assert!(WorkerResponse::NotesCompleted {
            note_end_count: 0,
            pending: Some(RequestOnly(NativeAudioReprepareRequest::Vst3Restart {
                flags: 0
            }))
        }
        .write_to(&mut Vec::new())
        .is_err());
    }

    #[test]
    fn shared_note_scalar_and_end_reserved_checks_refuse_invalid_data() {
        let canonical = SharedNoteEvent::capture(event(), 2, 1).unwrap();
        for invalid in [
            SharedNoteEvent { sample_offset: 2, ..canonical },
            SharedNoteEvent { native_id: -1, ..canonical },
            SharedNoteEvent { port_index: -1, ..canonical },
            SharedNoteEvent { channel: 16, ..canonical },
            SharedNoteEvent { key: 128, ..canonical },
            SharedNoteEvent { kind: 2, ..canonical },
            SharedNoteEvent { velocity: f64::NAN, ..canonical },
            SharedNoteEvent { velocity: 1.01, ..canonical },
        ] {
            assert!(invalid.realize(2, 1).is_err());
        }
        let end = AudioNoteEndEvent {
            native_note_id: i32::MAX,
            port_index: -1,
            channel: -1,
            key: -1,
            flags: 3,
        };
        let packed = SharedNoteEnd::capture(end, 1).unwrap();
        assert_eq!(packed.realize(1).unwrap(), end);
        assert!(SharedNoteEnd { reserved: 1, ..packed }.realize(1).is_err());
        assert!(SharedNoteEnd { native_id: -1, ..packed }.realize(1).is_err());
        assert!(SharedNoteEnd { port_index: 1, ..packed }.realize(1).is_err());
        assert!(SharedNoteEnd { flags: 4, ..packed }.realize(1).is_err());
    }

    #[test]
    fn auxiliary_output_regions_are_disjoint_bounded_and_included_before_events_and_notes() {
        let old = SharedBlockLayout::new(7, 2, 2, 1).unwrap();
        let layout = old.with_auxiliary_outputs(2).unwrap().with_notes(3, 2).unwrap();
        assert_eq!(layout.main_offset, 0);
        assert_eq!(layout.auxiliary_offset, old.auxiliary_offset);
        assert_eq!(layout.auxiliary_outputs_offset, old.events_offset);
        assert_eq!(layout.auxiliary_outputs_samples, 28);
        assert_eq!(
            layout.events_offset,
            old.events_offset + 28 * size_of::<f32>()
        );
        assert!(
            layout.note_events_offset
                >= layout.events_offset + layout.event_capacity * size_of::<SharedParameterEvent>()
        );
        assert!(
            layout.note_ends_offset >= layout.note_events_offset + 3 * size_of::<SharedNoteEvent>()
        );
        assert_eq!(
            layout.total_bytes,
            layout.note_ends_offset + 2 * size_of::<SharedNoteEnd>()
        );
        assert!(old.with_auxiliary_outputs(32).is_err());
        assert!(old.with_auxiliary_outputs(1).unwrap().with_auxiliary_outputs(1).is_err());
        assert!(old.with_notes(1, 1).unwrap().with_auxiliary_outputs(1).is_err());
        assert!(SharedBlockLayout::new(usize::MAX / 16, 2, 0, 0)
            .unwrap()
            .with_auxiliary_outputs(31)
            .is_err());
    }

    #[test]
    fn current_protocol_note_contract_is_mandatory_in_prepare_and_ready_json() {
        let ready = ReadyWire {
            schema_version: 18,
            result: ReadyResultWire::Ready {
                musical_time_reference: None,
                auxiliary_outputs: Vec::new(),
                execution_contract: ExecutionContractWire::capture(
                    AudioProcessorExecutionContract::new(
                        0,
                        AudioProcessorTail::None,
                        false,
                        true,
                        true,
                        0,
                    )
                    .unwrap(),
                )
                .unwrap(),
                auxiliary_buses: Vec::new(),
                input_names: Vec::new(),
                note_input_contract: AudioProcessorNoteInputContract::default(),
            },
        };
        let mut missing_outputs = serde_json::to_value(&ready).unwrap();
        missing_outputs["result"].as_object_mut().unwrap().remove("auxiliary_outputs");
        assert!(serde_json::from_value::<ReadyWire>(missing_outputs).is_err());
        let mut json = serde_json::to_value(&ready).unwrap();
        json["result"].as_object_mut().unwrap().remove("note_input_contract");
        assert!(serde_json::from_value::<ReadyWire>(json).is_err());
        assert_eq!(
            serde_json::from_value::<ReadyWire>(serde_json::to_value(&ready).unwrap()).unwrap(),
            ready
        );
        let prepare = PrepareWire {
            musical_time_reference: None,
            auxiliary_outputs: Vec::new(),
            schema_version: 18,
            native_lane: super::super::native_worker_lane_wire::NativeWorkerLaneWire::capture(None)
                .unwrap(),
            nonce: "typed-only-test".to_owned(),
            payload_bytes: 0,
            execution_contract: ExecutionContractWire::capture(
                AudioProcessorExecutionContract::new(
                    0,
                    AudioProcessorTail::None,
                    false,
                    true,
                    true,
                    0,
                )
                .unwrap(),
            )
            .unwrap(),
            auxiliary_buses: Vec::new(),
            note_input_contract: AudioProcessorNoteInputContract::default(),
            parameter_ids: Vec::new(),
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 2,
            realtime: false,
            shared_path: "not-opened".to_owned(),
            shared_bytes: 8,
        };
        let mut missing_outputs = serde_json::to_value(&prepare).unwrap();
        missing_outputs.as_object_mut().unwrap().remove("auxiliary_outputs");
        assert!(serde_json::from_value::<PrepareWire>(missing_outputs).is_err());
        let mut json = serde_json::to_value(&prepare).unwrap();
        json.as_object_mut().unwrap().remove("note_input_contract");
        assert!(serde_json::from_value::<PrepareWire>(json).is_err());
        assert_eq!(
            serde_json::from_value::<PrepareWire>(serde_json::to_value(&prepare).unwrap()).unwrap(),
            prepare
        );
    }
}
