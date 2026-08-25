use crate::{
    AudioProcessingMode, AudioProcessorAuxiliaryInputBusContract,
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorHostError,
    AudioProcessorTail,
};
use bytemuck::{Pod, Zeroable};
use mondrian_core::{AudioChannelLayout, ParameterId};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub(super) const PROTOCOL_VERSION: u32 = 1;
pub(super) const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;
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
pub(super) struct PrepareWire {
    pub schema_version: u32,
    pub nonce: String,
    pub payload: Vec<u8>,
    pub execution_contract: ExecutionContractWire,
    pub auxiliary_buses: Vec<AuxiliaryBusWire>,
    pub parameter_ids: Vec<ParameterId>,
    pub sample_rate: u32,
    pub channel_layout: AudioChannelLayout,
    pub max_block_frames: u64,
    pub realtime: bool,
    pub shared_path: String,
    pub shared_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct ReadyWire {
    pub schema_version: u32,
    pub result: ReadyResultWire,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum ReadyResultWire {
    Ready {
        execution_contract: ExecutionContractWire,
        auxiliary_buses: Vec<AuxiliaryBusWire>,
    },
    Rejected {
        detail: String,
    },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkerCommand {
    pub kind: CommandKind,
    pub start_sample: i64,
    pub frames: u32,
    pub event_count: u32,
}

impl WorkerCommand {
    pub(super) fn write_to(self, writer: &mut impl Write) -> io::Result<()> {
        let kind = match self.kind {
            CommandKind::EnterState => 1_u32,
            CommandKind::Process => 2,
            CommandKind::Shutdown => 3,
        };
        let mut bytes = [0_u8; 32];
        bytes[0..4].copy_from_slice(&COMMAND_MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        bytes[8..12].copy_from_slice(&kind.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.frames.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.start_sample.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.event_count.to_le_bytes());
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
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown audio Worker command",
                ));
            }
        };
        Ok(Self {
            kind,
            frames: u32::from_le_bytes(bytes[12..16].try_into().map_err(invalid_slice)?),
            start_sample: i64::from_le_bytes(bytes[16..24].try_into().map_err(invalid_slice)?),
            event_count: u32::from_le_bytes(bytes[24..28].try_into().map_err(invalid_slice)?),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkerResponse {
    Completed,
    Failed(String),
}

impl WorkerResponse {
    pub(super) fn write_to(self, writer: &mut impl Write) -> io::Result<()> {
        let (status, detail) = match self {
            Self::Completed => (0_u32, String::new()),
            Self::Failed(detail) => (1_u32, bounded_detail(detail)),
        };
        let detail = detail.as_bytes();
        let detail_len = u32::try_from(detail.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Worker detail too large"))?;
        writer.write_all(&RESPONSE_MAGIC.to_le_bytes())?;
        writer.write_all(&PROTOCOL_VERSION.to_le_bytes())?;
        writer.write_all(&status.to_le_bytes())?;
        writer.write_all(&detail_len.to_le_bytes())?;
        writer.write_all(detail)?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SharedBlockLayout {
    pub main_offset: usize,
    pub main_samples: usize,
    pub auxiliary_offset: usize,
    pub auxiliary_samples: usize,
    pub events_offset: usize,
    pub event_capacity: usize,
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
            events_offset,
            event_capacity,
            total_bytes,
        })
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
