//! One current named state-entry body on the existing authenticated stream.

use super::protocol::PROTOCOL_VERSION;
use crate::native_audio_author_state_entry::*;
use crate::{AudioProcessorExecutionContract, AudioProcessorTail, NativeConfigurationFormat};
use std::io::{self, Read, Write};
use std::num::NonZeroU64;

const MAGIC: [u8; 4] = *b"MASE";
const MAX_STATE: usize = crate::NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn read32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
fn read64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn read_extent(reader: &mut impl Read) -> io::Result<usize> {
    usize::try_from(read64(reader)?)
        .map_err(|_| invalid("state-entry extent exceeds address space"))
}
fn write_header(writer: &mut impl Write, status: u32) -> io::Result<()> {
    writer.write_all(&MAGIC)?;
    writer.write_all(&PROTOCOL_VERSION.to_le_bytes())?;
    writer.write_all(&status.to_le_bytes())
}
fn read_header(reader: &mut impl Read) -> io::Result<u32> {
    let mut magic = [0; 4];
    reader.read_exact(&mut magic)?;
    if magic != MAGIC || read32(reader)? != PROTOCOL_VERSION {
        return Err(invalid("state-entry current schema/magic mismatch"));
    }
    read32(reader)
}

pub(super) fn write_source(
    writer: &mut impl Write,
    source: NativeAudioAuthorStateEntrySource,
) -> io::Result<()> {
    let (tag, revision): (u32, u64) = match source {
        NativeAudioAuthorStateEntrySource::SelectedTerminal { revision } => (1, revision.get()),
        NativeAudioAuthorStateEntrySource::CanonicalPeer { revision } => (2, revision),
    };
    writer.write_all(&tag.to_le_bytes())?;
    writer.write_all(&revision.to_le_bytes())
}
pub(super) fn read_source(reader: &mut impl Read) -> io::Result<NativeAudioAuthorStateEntrySource> {
    let tag = read32(reader)?;
    let revision = read64(reader)?;
    Ok(match tag {
        1 => NativeAudioAuthorStateEntrySource::SelectedTerminal {
            revision: NonZeroU64::new(revision)
                .ok_or_else(|| invalid("state-entry terminal revision is zero"))?,
        },
        2 => NativeAudioAuthorStateEntrySource::CanonicalPeer { revision },
        _ => return Err(invalid("state-entry source kind")),
    })
}

pub(super) struct OwnedStateEntry {
    binding: crate::NativeAudioObjectBindRequest,
    source: NativeAudioAuthorStateEntrySource,
    format: NativeConfigurationFormat,
    configuration: Vec<u8>,
}

impl OwnedStateEntry {
    pub(super) fn owned_layout(&self) -> crate::NativeHostBufferLayout {
        crate::NativeHostBufferLayout {
            bytes: self.configuration.capacity(),
            alignment: align_of::<u8>(),
        }
    }
    pub(super) fn borrow(&self) -> NativeAudioAuthorStateEntryRequest<'_> {
        NativeAudioAuthorStateEntryRequest {
            binding: self.binding,
            source: self.source,
            format: self.format,
            configuration: &self.configuration,
        }
    }
}

pub(super) fn write_request(
    writer: &mut impl Write,
    request: &NativeAudioAuthorStateEntryRequest<'_>,
) -> io::Result<()> {
    if request.configuration.is_empty() || request.configuration.len() > MAX_STATE {
        return Err(invalid("state-entry configuration exceeds bounded codec"));
    }
    write_header(writer, 0)?;
    super::native_audio_object_wire::write_bind_request(writer, request.binding)?;
    write_source(writer, request.source)?;
    let format: u32 = match request.format {
        NativeConfigurationFormat::Vst3ComponentAndController => 1,
        NativeConfigurationFormat::ClapState => 2,
    };
    writer.write_all(&format.to_le_bytes())?;
    writer.write_all(&(request.configuration.len() as u64).to_le_bytes())?;
    writer.write_all(request.configuration)?;
    writer.flush()
}

pub(super) fn read_request(reader: &mut impl Read) -> io::Result<OwnedStateEntry> {
    if read_header(reader)? != 0 {
        return Err(invalid("state-entry request status"));
    }
    let binding = super::native_audio_object_wire::read_bind_request(reader)?;
    let source = read_source(reader)?;
    let format = match read32(reader)? {
        1 => NativeConfigurationFormat::Vst3ComponentAndController,
        2 => NativeConfigurationFormat::ClapState,
        _ => return Err(invalid("state-entry configuration format")),
    };
    let length = read_extent(reader)?;
    if length == 0 || length > MAX_STATE {
        return Err(invalid("state-entry request extent"));
    }
    let mut configuration = Vec::new();
    configuration
        .try_reserve_exact(length)
        .map_err(|_| invalid("state-entry request allocation"))?;
    if configuration.capacity() > MAX_STATE {
        return Err(invalid("state-entry request capacity"));
    }
    configuration.resize(length, 0);
    reader.read_exact(&mut configuration)?;
    Ok(OwnedStateEntry { binding, source, format, configuration })
}

pub(super) fn validate_receipt(receipt: NativeAudioAuthorStateEntryReceipt) -> io::Result<()> {
    if receipt.source_binding.expected_object.process_id() != receipt.object.process_id()
        || receipt.object.object_serial() <= receipt.source_binding.expected_object.object_serial()
        || receipt.configuration_bytes == 0
        || receipt.configuration_bytes > MAX_STATE
        || !receipt.contract.requires_state_entry()
    {
        return Err(invalid(
            "state-entry actual incarnation/configuration/contract",
        ));
    }
    Ok(())
}

pub(super) fn write_receipt(
    writer: &mut impl Write,
    receipt: NativeAudioAuthorStateEntryReceipt,
) -> io::Result<()> {
    validate_receipt(receipt)?;
    super::native_audio_object_wire::write_bind_request(writer, receipt.source_binding)?;
    write_source(writer, receipt.source)?;
    // Reuse the exact existing object codec by its typed binding field.
    super::native_audio_object_wire::write_bind_request(
        writer,
        crate::NativeAudioObjectBindRequest {
            binding: receipt.source_binding.binding,
            expected_object: receipt.object,
        },
    )?;
    writer.write_all(&(receipt.configuration_bytes as u64).to_le_bytes())?;
    writer.write_all(&receipt.configuration_sha256)?;
    let contract = receipt.contract;
    writer.write_all(&(contract.algorithmic_latency_frames() as u64).to_le_bytes())?;
    let (tail, frames): (u32, u64) = match contract.tail() {
        AudioProcessorTail::None => (0, 0),
        AudioProcessorTail::Finite(frames) => (1, frames as u64),
        AudioProcessorTail::Infinite => (2, 0),
    };
    writer.write_all(&tail.to_le_bytes())?;
    writer.write_all(&frames.to_le_bytes())?;
    let flags: u32 = u32::from(contract.requires_state_entry())
        | (u32::from(contract.realtime_capable()) << 1)
        | (u32::from(contract.offline_capable()) << 2);
    writer.write_all(&flags.to_le_bytes())?;
    writer.write_all(&(contract.session_scratch_bytes() as u64).to_le_bytes())
}

pub(super) fn read_receipt(
    reader: &mut impl Read,
) -> io::Result<NativeAudioAuthorStateEntryReceipt> {
    let source_binding = super::native_audio_object_wire::read_bind_request(reader)?;
    let source = read_source(reader)?;
    let fresh = super::native_audio_object_wire::read_bind_request(reader)?;
    if fresh.binding != source_binding.binding {
        return Err(invalid("state-entry reused object field ID"));
    }
    let configuration_bytes = read_extent(reader)?;
    let mut configuration_sha256 = [0; 32];
    reader.read_exact(&mut configuration_sha256)?;
    let latency = read_extent(reader)?;
    let tail_tag = read32(reader)?;
    let frames = read_extent(reader)?;
    let tail = match (tail_tag, frames) {
        (0, 0) => AudioProcessorTail::None,
        (1, frames) if frames != 0 => AudioProcessorTail::Finite(frames),
        (2, 0) => AudioProcessorTail::Infinite,
        _ => return Err(invalid("state-entry observed tail")),
    };
    let flags = read32(reader)?;
    if flags & !7 != 0 || flags & 1 == 0 {
        return Err(invalid("state-entry observed flags"));
    }
    let scratch = read_extent(reader)?;
    let contract = AudioProcessorExecutionContract::new(
        latency,
        tail,
        true,
        flags & 2 != 0,
        flags & 4 != 0,
        scratch,
    )
    .map_err(|_| invalid("state-entry observed contract"))?;
    let receipt = NativeAudioAuthorStateEntryReceipt {
        source_binding,
        source,
        object: fresh.expected_object,
        configuration_sha256,
        configuration_bytes,
        contract,
    };
    validate_receipt(receipt)?;
    Ok(receipt)
}

pub(super) fn write_reply(
    writer: &mut impl Write,
    outcome: NativeAudioAuthorStateEntryOutcome,
) -> io::Result<()> {
    use NativeAudioAuthorStateEntryRefusal::*;
    match outcome {
        NativeAudioAuthorStateEntryOutcome::Prepared(receipt) => {
            write_header(writer, 1)?;
            write_receipt(writer, receipt)?;
        }
        NativeAudioAuthorStateEntryOutcome::Refused(reason) => {
            write_header(writer, 2)?;
            let tag: u32 = match reason {
                UnsupportedAdapter => 1,
                OwnerChanged => 2,
                SourcePending => 3,
                DeliveryChanged => 4,
                EditorCleanupPending => 5,
                ConfigurationRejected => 6,
                ContractChanged => 7,
            };
            writer.write_all(&tag.to_le_bytes())?;
        }
    }
    writer.flush()
}
pub(super) fn read_reply(reader: &mut impl Read) -> io::Result<NativeAudioAuthorStateEntryOutcome> {
    use NativeAudioAuthorStateEntryRefusal::*;
    Ok(match read_header(reader)? {
        1 => NativeAudioAuthorStateEntryOutcome::Prepared(read_receipt(reader)?),
        2 => NativeAudioAuthorStateEntryOutcome::Refused(match read32(reader)? {
            1 => UnsupportedAdapter,
            2 => OwnerChanged,
            3 => SourcePending,
            4 => DeliveryChanged,
            5 => EditorCleanupPending,
            6 => ConfigurationRejected,
            7 => ContractChanged,
            _ => return Err(invalid("state-entry refusal tag")),
        }),
        3 => {
            return Err(io::Error::other(
                "native state-entry adapter failed; exact child retired",
            ))
        }
        _ => return Err(invalid("state-entry reply status")),
    })
}

pub(super) fn write_failed_reply(writer: &mut impl Write) -> io::Result<()> {
    write_header(writer, 3)?;
    writer.flush()
}

#[cfg(test)]
mod state_entry_contract_tests {
    use super::*;
    use std::io::Cursor;
    use std::num::NonZeroU32;
    fn object(serial: u64) -> crate::NativeAudioObjectObservation {
        crate::NativeAudioObjectObservation::new(
            NonZeroU32::new(127).expect("PID"),
            NonZeroU64::new(serial).expect("actual load serial"),
        )
    }
    fn binding(serial: u64) -> crate::NativeAudioObjectBindRequest {
        crate::NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: object(serial),
        }
    }
    fn receipt() -> NativeAudioAuthorStateEntryReceipt {
        NativeAudioAuthorStateEntryReceipt {
            source_binding: binding(11),
            source: NativeAudioAuthorStateEntrySource::SelectedTerminal {
                revision: NonZeroU64::new(19).expect("delivered source revision"),
            },
            object: object(12),
            configuration_sha256: [37; 32],
            configuration_bytes: 92,
            contract: AudioProcessorExecutionContract::new(
                0,
                AudioProcessorTail::None,
                true,
                true,
                true,
                0,
            )
            .expect("actual native contract"),
        }
    }
    #[test]
    fn native_state_entry_wire_preserves_exact_owner_source_config_and_fresh_receipt() {
        let binding = binding(11);
        let bytes = [71; 92];
        let request = NativeAudioAuthorStateEntryRequest {
            binding,
            source: NativeAudioAuthorStateEntrySource::CanonicalPeer { revision: 0 },
            format: NativeConfigurationFormat::Vst3ComponentAndController,
            configuration: &bytes,
        };
        let mut encoded = Vec::new();
        write_request(&mut encoded, &request).expect("single current codec");
        let decoded =
            read_request(&mut Cursor::new(encoded)).expect("bounded owned current request");
        assert_eq!(decoded.borrow().binding, binding);
        assert_eq!(decoded.borrow().source, request.source);
        assert_eq!(decoded.borrow().configuration, &bytes);
        assert_eq!(
            decoded.owned_layout().bytes,
            decoded.configuration.capacity()
        );
        let expected = receipt();
        let mut reply = Vec::new();
        write_reply(
            &mut reply,
            NativeAudioAuthorStateEntryOutcome::Prepared(expected),
        )
        .expect("receipt");
        assert_eq!(
            read_reply(&mut Cursor::new(reply)).expect("reply"),
            NativeAudioAuthorStateEntryOutcome::Prepared(expected)
        );
    }
    #[test]
    fn native_state_entry_wire_rejects_old_schema_zero_source_and_oversized_input_before_payload() {
        let mut header = Vec::new();
        write_header(&mut header, 0).expect("header");
        header[4..8].copy_from_slice(&(PROTOCOL_VERSION - 1).to_le_bytes());
        assert!(read_request(&mut Cursor::new(header)).is_err());
        let mut source = Vec::new();
        source.extend_from_slice(&1u32.to_le_bytes());
        source.extend_from_slice(&0u64.to_le_bytes());
        assert!(read_source(&mut Cursor::new(source)).is_err());
        let mut input = Vec::new();
        write_header(&mut input, 0).expect("header");
        super::super::native_audio_object_wire::write_bind_request(&mut input, binding(11))
            .expect("binding");
        write_source(
            &mut input,
            NativeAudioAuthorStateEntrySource::CanonicalPeer { revision: 0 },
        )
        .expect("source");
        input.extend_from_slice(&1u32.to_le_bytes());
        input.extend_from_slice(&((MAX_STATE as u64) + 1).to_le_bytes());
        assert!(
            read_request(&mut Cursor::new(input)).is_err(),
            "bounded extent rejects without reading any payload"
        );
    }
    #[test]
    fn native_state_entry_wire_rejects_same_or_older_serial_other_child_and_stateless_receipt() {
        let original = receipt();
        for serial in [10, 11] {
            assert!(validate_receipt(NativeAudioAuthorStateEntryReceipt {
                object: object(serial),
                ..original
            })
            .is_err());
        }
        let other = crate::NativeAudioObjectObservation::new(
            NonZeroU32::new(128).expect("other PID"),
            NonZeroU64::new(12).expect("serial"),
        );
        assert!(
            validate_receipt(NativeAudioAuthorStateEntryReceipt { object: other, ..original })
                .is_err()
        );
        let stateless =
            AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, false, true, true, 0)
                .expect("stateless");
        assert!(validate_receipt(NativeAudioAuthorStateEntryReceipt {
            contract: stateless,
            ..original
        })
        .is_err());
    }
    #[test]
    fn native_state_entry_wire_roundtrips_each_typed_refusal_and_failed_child_never_prepared() {
        use NativeAudioAuthorStateEntryRefusal::*;
        for reason in [
            UnsupportedAdapter,
            OwnerChanged,
            SourcePending,
            DeliveryChanged,
            EditorCleanupPending,
            ConfigurationRejected,
            ContractChanged,
        ] {
            let mut bytes = Vec::new();
            let expected = NativeAudioAuthorStateEntryOutcome::Refused(reason);
            write_reply(&mut bytes, expected).expect("refusal");
            assert_eq!(
                read_reply(&mut Cursor::new(bytes)).expect("typed reply"),
                expected
            );
        }
        let mut bytes = Vec::new();
        write_failed_reply(&mut bytes).expect("failed child response");
        assert!(read_reply(&mut Cursor::new(bytes)).is_err());
    }
    #[test]
    fn native_state_entry_reentered_ack_uses_only_exact_old_prefix_and_fresh_binding() {
        let preparation = receipt();
        let installed_binding = crate::NativeAudioObjectBindRequest {
            expected_object: preparation.object(),
            binding: mondrian_core::AudioProcessorBindingId::new(),
        };
        let ack =
            crate::NativeAudioReenteredAuthorAcknowledgement { preparation, installed_binding };
        let mut bytes = Vec::new();
        super::super::native_audio_object_wire::write_observation_request(
            &mut bytes,
            crate::NativeAudioObservationConsumption::AcknowledgeReenteredAuthor(ack),
        )
        .expect("one Poll9 consumption codec");
        assert_eq!(
            super::super::native_audio_object_wire::read_observation_request(&mut Cursor::new(
                bytes
            ))
            .expect("current Ack"),
            crate::NativeAudioObservationConsumption::AcknowledgeReenteredAuthor(ack)
        );
        let mut bytes = Vec::new();
        assert!(
            super::super::native_audio_object_wire::write_observation_request(
                &mut bytes,
                crate::NativeAudioObservationConsumption::AcknowledgeReenteredAuthor(
                    crate::NativeAudioReenteredAuthorAcknowledgement {
                        installed_binding: binding(11),
                        ..ack
                    }
                )
            )
            .is_err(),
            "old object cannot acknowledge fresh state entry"
        );
    }
}
