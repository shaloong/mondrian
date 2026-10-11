use crate::{
    AudioProcessingMode, AudioProcessorAuxiliaryInputBusContract,
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorHostError,
    AudioProcessorTail,
};
use bytemuck::{Pod, Zeroable};
use mondrian_core::{AudioChannelLayout, ParameterId};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub(super) const PROTOCOL_VERSION: u32 = 8;
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
    pub native_lane: super::native_worker_lane_wire::NativeWorkerLaneWire,
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
        input_names: Vec<crate::AudioProcessorInputName>,
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
}

impl WorkerCommand {
    pub(super) fn write_to(self, writer: &mut impl Write) -> io::Result<()> {
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
        };
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
    CompletedWithPending(mondrian_core::NativeAudioReprepareRequest),
    DeferredForConfiguration(mondrian_core::NativeAudioReprepareRequest),
    Failed(String),
}

impl WorkerResponse {
    pub(super) fn write_to(self, writer: &mut impl Write) -> io::Result<()> {
        let (status, payload) = match &self {
            Self::Completed => (0_u32, None),
            Self::Failed(_) => (1, None),
            Self::CompletedWithPending(request) | Self::DeferredForConfiguration(request) => {
                let (kind, flags) = match request {
                    mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags }
                        if *flags != 0 =>
                    {
                        (1_u32, *flags)
                    }
                    mondrian_core::NativeAudioReprepareRequest::ClapRestart => (2, 0),
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid pending VST3 flags",
                        ))
                    }
                };
                let mut bytes = [0_u8; 8];
                bytes[..4].copy_from_slice(&kind.to_le_bytes());
                bytes[4..].copy_from_slice(&flags.to_le_bytes());
                (
                    if matches!(self, Self::DeferredForConfiguration(_)) {
                        3
                    } else {
                        2
                    },
                    Some(bytes),
                )
            }
        };
        let detail = match self {
            Self::Failed(detail) => Some(bounded_detail(detail)),
            _ => None,
        };
        let length = payload.as_ref().map_or_else(
            || detail.as_ref().map_or(0, String::len),
            |bytes| bytes.len(),
        );
        let response_len = u32::try_from(length)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Worker detail too large"))?;
        let mut header = [0_u8; 16];
        header[0..4].copy_from_slice(&RESPONSE_MAGIC.to_le_bytes());
        header[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        header[8..12].copy_from_slice(&status.to_le_bytes());
        header[12..16].copy_from_slice(&response_len.to_le_bytes());
        writer.write_all(&header)?;
        if let Some(bytes) = payload {
            writer.write_all(&bytes)?;
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
        if status == 2 || status == 3 {
            if detail_len != 8 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid pending response extent",
                ));
            }
            let mut bytes = [0_u8; 8];
            reader.read_exact(&mut bytes)?;
            let kind = u32::from_le_bytes(bytes[..4].try_into().map_err(invalid_slice)?);
            let flags = u32::from_le_bytes(bytes[4..].try_into().map_err(invalid_slice)?);
            let request = match (kind, flags) {
                (1, flags) if flags != 0 => {
                    mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags }
                }
                (2, 0) => mondrian_core::NativeAudioReprepareRequest::ClapRestart,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid pending response kind/flags",
                    ))
                }
            };
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

    // Independent protocol-v5 wire bytes: MAPR, version=5, success=0, detail=0.
    const COMPLETED_WIRE: [u8; 16] = [77, 65, 80, 82, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    #[test]
    fn native_response_wire_completed_has_one_nonempty_header_write() -> io::Result<()> {
        let mut writer = RecordedWriter::new(usize::MAX);
        WorkerResponse::Completed.write_to(&mut writer)?;
        assert_eq!(writer.written(), COMPLETED_WIRE);
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
            77, 65, 80, 82, 8, 0, 0, 0, 1, 0, 0, 0, 6, 0, 0, 0, 98, 97, 100, 45, 206, 187,
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
            77, 65, 80, 82, 8, 0, 0, 0, 1, 0, 0, 0, 6, 0, 0, 0, 98, 97, 100, 45, 206, 187,
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
        for request in [
            NativeAudioReprepareRequest::ClapRestart,
            NativeAudioReprepareRequest::Vst3Restart { flags: 0x8000_0008 },
        ] {
            let response = WorkerResponse::CompletedWithPending(request);
            let mut encoded = Vec::new();
            response.clone().write_to(&mut encoded).expect("encode native notice");
            assert_eq!(encoded.len(), 24, "fixed header + fixed eight-byte notice");
            assert_eq!(&encoded[8..12], &2_u32.to_le_bytes());
            assert_eq!(&encoded[12..16], &8_u32.to_le_bytes());
            assert_eq!(
                WorkerResponse::read_from(&mut encoded.as_slice()).expect("decode notice"),
                response
            );
        }
        let mut encoded = Vec::new();
        let query = WorkerCommand {
            kind: CommandKind::PollPending,
            start_sample: 0,
            frames: 0,
            event_count: 0,
        };
        query.write_to(&mut encoded).expect("encode query");
        assert_eq!(
            WorkerCommand::read_from(&mut encoded.as_slice()).expect("decode query"),
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
        let mut canonical = Vec::new();
        WorkerResponse::CompletedWithPending(NativeAudioReprepareRequest::ClapRestart)
            .write_to(&mut canonical)
            .expect("canonical notice");
        for (offset, value) in [(12, 0_u32), (16, 9), (20, 1)] {
            let mut invalid = canonical.clone();
            invalid[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(WorkerResponse::read_from(&mut invalid.as_slice()).is_err());
        }
        assert!(
            WorkerResponse::CompletedWithPending(NativeAudioReprepareRequest::Vst3Restart {
                flags: 0
            })
            .write_to(&mut Vec::new())
            .is_err()
        );
    }
}
