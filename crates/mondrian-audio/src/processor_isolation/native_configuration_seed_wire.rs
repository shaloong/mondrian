//! Fixed bounded non-RT seed control; a refused body is consumed before reply.

use super::native_configuration_seed::*;
use super::protocol::{MAX_WORKER_DETAIL_BYTES, PROTOCOL_VERSION};
use super::{NativeConfigurationFormat, NativeRequestCohort};
use crate::{AudioProcessorExecutionContract, AudioProcessorTail};
use mondrian_core::NativeAudioReprepareRequest;
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::num::NonZeroU64;

const MAGIC: u32 = u32::from_le_bytes(*b"MASD");
const TICKET_SIZE: usize = 104;
const STAGE_SIZE: usize = TICKET_SIZE + 8;
const MAX_SEED_BYTES: usize = 128 * 1024;

fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}
fn get32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let range = bytes.get(offset..offset + 4).ok_or_else(|| invalid("short seed field"))?;
    Ok(u32::from_le_bytes(
        range.try_into().map_err(|_| invalid("short seed field"))?,
    ))
}
fn get64(bytes: &[u8], offset: usize) -> io::Result<u64> {
    let range = bytes.get(offset..offset + 8).ok_or_else(|| invalid("short seed field"))?;
    Ok(u64::from_le_bytes(
        range.try_into().map_err(|_| invalid("short seed field"))?,
    ))
}
fn extent(bytes: &[u8], offset: usize) -> io::Result<usize> {
    usize::try_from(get64(bytes, offset)?).map_err(|_| invalid("seed extent exceeds address space"))
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_ticket(bytes: &mut [u8], ticket: NativeConfigurationSeedTicket) -> io::Result<()> {
    NativeRequestCohort::new(
        ticket.serviced_cohort.serial,
        ticket.serviced_cohort.request,
    )
    .map_err(|_| invalid("empty seed cohort"))?;
    if ticket.serviced_cohort.exhausted() || ticket.configuration_bytes > MAX_SEED_BYTES {
        return Err(invalid("seed ticket exceeds bounded service"));
    }
    put64(bytes, 0, ticket.control_token.get());
    put32(bytes, 8, ticket.serviced_cohort.serial);
    let (kind, flags) = match (ticket.serviced_cohort.request, ticket.format) {
        (
            NativeAudioReprepareRequest::Vst3Restart { flags },
            NativeConfigurationFormat::Vst3ComponentAndController,
        ) => (1, flags),
        (NativeAudioReprepareRequest::ClapRestart, NativeConfigurationFormat::ClapState) => (2, 0),
        _ => return Err(invalid("seed configuration codec differs from cohort")),
    };
    put32(bytes, 12, kind);
    put32(bytes, 16, flags);
    put32(bytes, 20, kind);
    let contract = ticket.observed_contract;
    put64(bytes, 24, contract.algorithmic_latency_frames() as u64);
    let (tail, frames) = match contract.tail() {
        AudioProcessorTail::None => (0, 0),
        AudioProcessorTail::Finite(frames) => (1, frames as u64),
        AudioProcessorTail::Infinite => (2, 0),
    };
    put32(bytes, 32, tail);
    put64(bytes, 40, frames);
    put32(
        bytes,
        48,
        u32::from(contract.requires_state_entry())
            | (u32::from(contract.realtime_capable()) << 1)
            | (u32::from(contract.offline_capable()) << 2),
    );
    put64(bytes, 56, contract.session_scratch_bytes() as u64);
    put64(bytes, 64, ticket.configuration_bytes as u64);
    bytes[72..104].copy_from_slice(&ticket.configuration_sha256);
    Ok(())
}

fn get_ticket(bytes: &[u8]) -> io::Result<NativeConfigurationSeedTicket> {
    if bytes.len() < TICKET_SIZE || bytes[36..40] != [0; 4] || bytes[52..56] != [0; 4] {
        return Err(invalid("seed reserved fields are not canonical"));
    }
    let control_token =
        NonZeroU64::new(get64(bytes, 0)?).ok_or_else(|| invalid("empty seed control token"))?;
    let (request, format) = match (get32(bytes, 12)?, get32(bytes, 16)?, get32(bytes, 20)?) {
        (1, flags, 1) if flags != 0 => (
            NativeAudioReprepareRequest::Vst3Restart { flags },
            NativeConfigurationFormat::Vst3ComponentAndController,
        ),
        (2, 0, 2) => (
            NativeAudioReprepareRequest::ClapRestart,
            NativeConfigurationFormat::ClapState,
        ),
        _ => return Err(invalid("seed cohort/codec mismatch")),
    };
    let serviced_cohort = NativeRequestCohort::new(get32(bytes, 8)?, request)
        .map_err(|_| invalid("invalid seed serial"))?;
    let configuration_bytes = extent(bytes, 64)?;
    if serviced_cohort.exhausted() || configuration_bytes > MAX_SEED_BYTES {
        return Err(invalid("seed exceeds bounded service"));
    }
    let tail = match (get32(bytes, 32)?, extent(bytes, 40)?) {
        (0, 0) => AudioProcessorTail::None,
        (1, frames) if frames != 0 => AudioProcessorTail::Finite(frames),
        (2, 0) => AudioProcessorTail::Infinite,
        _ => return Err(invalid("seed tail is not canonical")),
    };
    let flags = get32(bytes, 48)?;
    if flags & !7 != 0 {
        return Err(invalid("unknown seed capability flags"));
    }
    let observed_contract = AudioProcessorExecutionContract::new(
        extent(bytes, 24)?,
        tail,
        flags & 1 != 0,
        flags & 2 != 0,
        flags & 4 != 0,
        extent(bytes, 56)?,
    )
    .map_err(|_| invalid("invalid seed contract"))?;
    let configuration_sha256 = bytes[72..104].try_into().map_err(|_| invalid("short seed SHA"))?;
    Ok(NativeConfigurationSeedTicket {
        serviced_cohort,
        control_token,
        observed_contract,
        format,
        configuration_bytes,
        configuration_sha256,
    })
}

pub(super) fn validate_stage(request: &NativeConfigurationSeedRequest<'_>) -> io::Result<()> {
    let mut bytes = [0; TICKET_SIZE];
    put_ticket(&mut bytes, request.ticket)?;
    if request.configuration.len() != request.ticket.configuration_bytes
        || request.seed_capacity_grant_bytes > MAX_SEED_BYTES
        || <[u8; 32]>::from(Sha256::digest(request.configuration))
            != request.ticket.configuration_sha256
    {
        return Err(invalid("seed body/grant differs from candidate"));
    }
    Ok(())
}

pub(super) fn write_stage(
    writer: &mut impl Write,
    request: &NativeConfigurationSeedRequest<'_>,
) -> io::Result<()> {
    validate_stage(request)?;
    let mut header = [0; STAGE_SIZE];
    put_ticket(&mut header, request.ticket)?;
    put64(
        &mut header,
        TICKET_SIZE,
        request.seed_capacity_grant_bytes as u64,
    );
    writer.write_all(&header)?;
    writer.write_all(request.configuration)?;
    writer.flush()
}

pub(super) enum DecodedStage {
    Owned(NativeConfigurationOwnedSeed),
    Refused(NativeConfigurationSeedRefusal),
}

// A known bounded body must be drained on allocation/capacity refusal, preserving
// framing and the valid stopped child. No dynamic buffer is needed for this path.
fn drain(reader: &mut impl Read, mut bytes: usize) -> io::Result<()> {
    let mut storage = [0; 4096];
    while bytes != 0 {
        let chunk = bytes.min(storage.len());
        reader.read_exact(&mut storage[..chunk])?;
        bytes -= chunk;
    }
    Ok(())
}

pub(super) fn read_stage(reader: &mut impl Read) -> io::Result<DecodedStage> {
    let mut header = [0; STAGE_SIZE];
    reader.read_exact(&mut header)?;
    let ticket = get_ticket(&header)?;
    let grant = extent(&header, TICKET_SIZE)?;
    if grant > MAX_SEED_BYTES {
        return Err(invalid("seed grant exceeds protocol bound"));
    }
    let mut configuration = Vec::new();
    if ticket.configuration_bytes > grant
        || configuration.try_reserve_exact(ticket.configuration_bytes).is_err()
        || configuration.capacity() > grant
    {
        drop(configuration);
        drain(reader, ticket.configuration_bytes)?;
        return Ok(DecodedStage::Refused(
            NativeConfigurationSeedRefusal::BufferGrantExceeded,
        ));
    }
    configuration.resize(ticket.configuration_bytes, 0);
    reader.read_exact(&mut configuration)?;
    Ok(DecodedStage::Owned(NativeConfigurationOwnedSeed {
        ticket,
        configuration,
        seed_capacity_grant_bytes: grant,
    }))
}

pub(super) fn write_commit(
    writer: &mut impl Write,
    ticket: NativeConfigurationSeedTicket,
) -> io::Result<()> {
    let mut bytes = [0; TICKET_SIZE];
    put_ticket(&mut bytes, ticket)?;
    writer.write_all(&bytes)?;
    writer.flush()
}
pub(super) fn read_commit(reader: &mut impl Read) -> io::Result<NativeConfigurationSeedTicket> {
    let mut bytes = [0; TICKET_SIZE];
    reader.read_exact(&mut bytes)?;
    get_ticket(&bytes)
}

pub(super) enum SeedReply {
    Outcome(NativeConfigurationSeedOutcome),
    Failed(String),
}

fn refusal_tag(refusal: NativeConfigurationSeedRefusal) -> u32 {
    match refusal {
        NativeConfigurationSeedRefusal::UnsupportedAdapter => 1,
        NativeConfigurationSeedRefusal::StaleTicket => 2,
        NativeConfigurationSeedRefusal::ConfigurationMismatch => 3,
        NativeConfigurationSeedRefusal::BufferGrantExceeded => 4,
        NativeConfigurationSeedRefusal::SeedNotStaged => 5,
    }
}
fn refusal_from_tag(tag: u32) -> io::Result<NativeConfigurationSeedRefusal> {
    Ok(match tag {
        1 => NativeConfigurationSeedRefusal::UnsupportedAdapter,
        2 => NativeConfigurationSeedRefusal::StaleTicket,
        3 => NativeConfigurationSeedRefusal::ConfigurationMismatch,
        4 => NativeConfigurationSeedRefusal::BufferGrantExceeded,
        5 => NativeConfigurationSeedRefusal::SeedNotStaged,
        _ => return Err(invalid("unknown seed refusal")),
    })
}
fn write_header(writer: &mut impl Write, status: u32, length: usize) -> io::Result<()> {
    let mut bytes = [0; 16];
    put32(&mut bytes, 0, MAGIC);
    put32(&mut bytes, 4, PROTOCOL_VERSION);
    put32(&mut bytes, 8, status);
    put32(
        &mut bytes,
        12,
        u32::try_from(length).map_err(|_| invalid("seed reply exceeds u32"))?,
    );
    writer.write_all(&bytes)
}
pub(super) fn write_reply(writer: &mut impl Write, reply: SeedReply) -> io::Result<()> {
    match reply {
        SeedReply::Outcome(NativeConfigurationSeedOutcome::Accepted(receipt)) => {
            let mut bytes = [0; STAGE_SIZE];
            put_ticket(&mut bytes, receipt.ticket)?;
            put64(
                &mut bytes,
                TICKET_SIZE,
                receipt.child_seed_capacity_bytes as u64,
            );
            write_header(writer, 0, bytes.len())?;
            writer.write_all(&bytes)?;
        }
        SeedReply::Outcome(NativeConfigurationSeedOutcome::Refused(refusal)) => {
            write_header(writer, 1, 4)?;
            writer.write_all(&refusal_tag(refusal).to_le_bytes())?;
        }
        SeedReply::Failed(mut detail) => {
            let mut end = detail.len().min(MAX_WORKER_DETAIL_BYTES);
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
            write_header(writer, 2, end)?;
            writer.write_all(detail.as_bytes())?;
        }
    }
    writer.flush()
}
pub(super) fn read_reply(
    reader: &mut impl Read,
    expected: NativeConfigurationSeedTicket,
    grant: usize,
) -> io::Result<SeedReply> {
    let mut header = [0; 16];
    reader.read_exact(&mut header)?;
    if get32(&header, 0)? != MAGIC || get32(&header, 4)? != PROTOCOL_VERSION {
        return Err(invalid("invalid seed reply header"));
    }
    match (get32(&header, 8)?, get32(&header, 12)? as usize) {
        (0, STAGE_SIZE) => {
            let mut bytes = [0; STAGE_SIZE];
            reader.read_exact(&mut bytes)?;
            let ticket = get_ticket(&bytes)?;
            let capacity = extent(&bytes, TICKET_SIZE)?;
            if ticket != expected || capacity < ticket.configuration_bytes || capacity > grant {
                return Err(invalid(
                    "seed receipt differs from same-child request or grant",
                ));
            }
            Ok(SeedReply::Outcome(
                NativeConfigurationSeedOutcome::Accepted(NativeConfigurationSeedReceipt {
                    ticket,
                    child_seed_capacity_bytes: capacity,
                }),
            ))
        }
        (1, 4) => {
            let mut bytes = [0; 4];
            reader.read_exact(&mut bytes)?;
            Ok(SeedReply::Outcome(NativeConfigurationSeedOutcome::Refused(
                refusal_from_tag(get32(&bytes, 0)?)?,
            )))
        }
        (2, length) if length <= MAX_WORKER_DETAIL_BYTES => {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(length)
                .map_err(|_| invalid("seed failure allocation failed"))?;
            bytes.resize(length, 0);
            reader.read_exact(&mut bytes)?;
            Ok(SeedReply::Failed(
                String::from_utf8(bytes).map_err(|_| invalid("invalid seed failure detail"))?,
            ))
        }
        _ => Err(invalid("seed reply extent rejected before allocation")),
    }
}

#[cfg(test)]
mod seed_wire_tests {
    use super::*;

    fn seed_request(bytes: &[u8], grant: usize) -> NativeConfigurationSeedRequest<'_> {
        NativeConfigurationSeedRequest {
            ticket: NativeConfigurationSeedTicket {
                serviced_cohort: NativeRequestCohort::new(
                    1,
                    NativeAudioReprepareRequest::ClapRestart,
                )
                .expect("cohort"),
                control_token: NonZeroU64::new(17).expect("token"),
                observed_contract: AudioProcessorExecutionContract::new(
                    7,
                    AudioProcessorTail::Finite(7),
                    true,
                    true,
                    true,
                    0,
                )
                .expect("facts"),
                format: NativeConfigurationFormat::ClapState,
                configuration_bytes: bytes.len(),
                configuration_sha256: Sha256::digest(bytes).into(),
            },
            configuration: bytes,
            seed_capacity_grant_bytes: grant,
        }
    }

    #[test]
    fn native_configuration_seed_wire_bounded_refusal_drains_body_and_preserves_next_command() {
        let request = seed_request(&[7, 1, 2], 2);
        let mut encoded = Vec::new();
        write_stage(&mut encoded, &request).expect("valid below-grant body");
        encoded.extend_from_slice(b"next exact command");
        let mut input = encoded.as_slice();
        assert!(matches!(
            read_stage(&mut input).expect("typed resource refusal"),
            DecodedStage::Refused(NativeConfigurationSeedRefusal::BufferGrantExceeded)
        ));
        assert_eq!(input, b"next exact command");
        let mut valid = Vec::new();
        write_stage(&mut valid, &seed_request(&[7, 1, 2], 3)).expect("same connection retry");
        assert!(matches!(
            read_stage(&mut valid.as_slice()).expect("bounded owner"),
            DecodedStage::Owned(_)
        ));
    }

    #[test]
    fn native_configuration_seed_wire_oversized_and_reserved_metadata_are_rejected_before_body() {
        let request = seed_request(&[7, 1, 2], 3);
        let mut bytes = Vec::new();
        write_stage(&mut bytes, &request).expect("canonical fixed fields");
        let mut excessive = bytes.clone();
        put64(&mut excessive, 64, (MAX_SEED_BYTES + 1) as u64);
        let mut input = excessive.as_slice();
        assert!(read_stage(&mut input).is_err());
        assert_eq!(
            input.len(),
            request.configuration.len(),
            "body not read or allocated from invalid header"
        );
        let mut reserved = bytes;
        reserved[36] = 1;
        let mut input = reserved.as_slice();
        assert!(read_stage(&mut input).is_err());
        assert_eq!(input.len(), request.configuration.len());
    }

    #[test]
    fn native_configuration_seed_wire_reply_uses_exact_outer_prefix_and_receipt() {
        // Only the outer MASD header carries the current protocol version.
        let request = seed_request(&[7, 1, 2], 19);
        let receipt = NativeConfigurationSeedReceipt {
            ticket: request.ticket,
            child_seed_capacity_bytes: 19,
        };
        let mut bytes = Vec::new();
        write_reply(
            &mut bytes,
            SeedReply::Outcome(NativeConfigurationSeedOutcome::Accepted(receipt)),
        )
        .expect("reply");
        assert_eq!(
            &bytes[..16],
            &[77, 65, 83, 68, 19, 0, 0, 0, 0, 0, 0, 0, 112, 0, 0, 0]
        );
        assert!(
            matches!(read_reply(&mut bytes.as_slice(), request.ticket, 19).expect("same-child receipt"),
            SeedReply::Outcome(NativeConfigurationSeedOutcome::Accepted(observed)) if observed == receipt)
        );
        for version in [8_u32, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20] {
            let mut old = bytes.clone();
            put32(&mut old, 4, version);
            let mut input = old.as_slice();
            assert!(read_reply(&mut input, request.ticket, 19).is_err());
            assert_eq!(
                input.len(),
                bytes.len() - 16,
                "old outer header never consumes seed body"
            );
        }
        assert!(read_reply(&mut bytes.as_slice(), request.ticket, 18).is_err());
        let mut stale = request.ticket;
        stale.control_token = NonZeroU64::new(18).expect("another token");
        assert!(read_reply(&mut bytes.as_slice(), stale, 19).is_err());
    }
}
