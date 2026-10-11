//! Bounded named zero-PCM control frames, separate from realtime block transport.

use super::native_configuration_control::*;
use super::protocol::{MAX_WORKER_DETAIL_BYTES, PROTOCOL_VERSION};
use crate::{AudioProcessorExecutionContract, AudioProcessorTail};
use mondrian_core::NativeAudioReprepareRequest;
use std::io::{self, Read, Write};
use std::num::NonZeroU64;

const MAGIC: u32 = u32::from_le_bytes(*b"MANC");
const MAX_LANES: usize = 1024;
const MAX_STATE: usize = 128 * 1024;
const MAX_BUFFER_GRANT: usize = 1024 * 1024;
const REQUEST_SIZE: usize = 40;
const READY_SIZE: usize = 112;

fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}
fn u32_at(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(|| invalid("short u32 field"))?
            .try_into()
            .map_err(|_| invalid("short u32 field"))?,
    ))
}
fn u64_at(bytes: &[u8], offset: usize) -> io::Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or_else(|| invalid("short u64 field"))?
            .try_into()
            .map_err(|_| invalid("short u64 field"))?,
    ))
}
fn usize_at(bytes: &[u8], offset: usize) -> io::Result<usize> {
    usize::try_from(u64_at(bytes, offset)?)
        .map_err(|_| invalid("control extent exceeds address space"))
}
fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_cohort(bytes: &mut [u8], offset: usize, cohort: NativeRequestCohort) -> io::Result<()> {
    let (kind, flags) = match cohort.request {
        NativeAudioReprepareRequest::Vst3Restart { flags } if flags != 0 => (1, flags),
        NativeAudioReprepareRequest::ClapRestart => (2, 0),
        _ => return Err(invalid("empty VST3 cohort")),
    };
    if cohort.serial == 0 {
        return Err(invalid("empty native cohort serial"));
    }
    put_u32(bytes, offset, cohort.serial);
    put_u32(bytes, offset + 4, kind);
    put_u32(bytes, offset + 8, flags);
    Ok(())
}
fn get_cohort(bytes: &[u8], offset: usize) -> io::Result<NativeRequestCohort> {
    let serial = u32_at(bytes, offset)?;
    let request = match (u32_at(bytes, offset + 4)?, u32_at(bytes, offset + 8)?) {
        (1, flags) if flags != 0 => NativeAudioReprepareRequest::Vst3Restart { flags },
        (2, 0) => NativeAudioReprepareRequest::ClapRestart,
        _ => return Err(invalid("invalid native cohort ABI/flags")),
    };
    NativeRequestCohort::new(serial, request).map_err(|_| invalid("invalid native cohort serial"))
}

fn write_header(writer: &mut impl Write, status: u32, extent: usize) -> io::Result<()> {
    let mut header = [0; 16];
    put_u32(&mut header, 0, MAGIC);
    put_u32(&mut header, 4, PROTOCOL_VERSION);
    put_u32(&mut header, 8, status);
    put_u32(
        &mut header,
        12,
        u32::try_from(extent).map_err(|_| invalid("control body too large"))?,
    );
    writer.write_all(&header)
}
fn read_header(reader: &mut impl Read) -> io::Result<(u32, usize)> {
    let mut header = [0; 16];
    reader.read_exact(&mut header)?;
    if u32_at(&header, 0)? != MAGIC || u32_at(&header, 4)? != PROTOCOL_VERSION {
        return Err(invalid("invalid native control reply header"));
    }
    Ok((u32_at(&header, 8)?, u32_at(&header, 12)? as usize))
}

pub(super) fn write_cohort_reply(
    writer: &mut impl Write,
    cohort: Option<NativeRequestCohort>,
) -> io::Result<()> {
    if let Some(cohort) = cohort {
        let mut bytes = [0; 12];
        put_cohort(&mut bytes, 0, cohort)?;
        write_header(writer, 1, bytes.len())?;
        writer.write_all(&bytes)?;
    } else {
        write_header(writer, 0, 0)?;
    }
    writer.flush()
}
pub(super) fn read_cohort_reply(reader: &mut impl Read) -> io::Result<Option<NativeRequestCohort>> {
    match read_header(reader)? {
        (0, 0) => Ok(None),
        (1, 12) => {
            let mut bytes = [0; 12];
            reader.read_exact(&mut bytes)?;
            Ok(Some(get_cohort(&bytes, 0)?))
        }
        _ => Err(invalid("invalid native cohort reply extent")),
    }
}

pub(super) struct OwnedControlRequest {
    cohort: NativeRequestCohort,
    token: NonZeroU64,
    values: Vec<f64>,
    state_limit: usize,
    buffer_grant: usize,
}
impl OwnedControlRequest {
    pub(super) fn value_bytes(&self) -> io::Result<usize> {
        self.values
            .capacity()
            .checked_mul(size_of::<f64>())
            .ok_or_else(|| invalid("request capacity overflow"))
    }
    pub(super) fn borrow(&self) -> io::Result<NativeConfigurationControlRequest<'_>> {
        let buffers = self.value_bytes()?;
        Ok(NativeConfigurationControlRequest {
            expected_cohort: self.cohort,
            control_token: self.token,
            canonical_constant_values: &self.values,
            configuration_limit_bytes: self.state_limit,
            control_buffer_grant_bytes: self
                .buffer_grant
                .checked_sub(buffers)
                .ok_or_else(|| invalid("request values exceed control grant"))?,
        })
    }
}
pub(super) fn validate_request(request: &NativeConfigurationControlRequest<'_>) -> io::Result<()> {
    NativeRequestCohort::new(
        request.expected_cohort.serial,
        request.expected_cohort.request,
    )
    .map_err(|_| invalid("invalid native control cohort"))?;
    let value_bytes = request
        .canonical_constant_values
        .len()
        .checked_mul(size_of::<f64>())
        .ok_or_else(|| invalid("request value extent overflow"))?;
    if request.canonical_constant_values.len() > MAX_LANES
        || request.canonical_constant_values.iter().any(|value| !value.is_finite())
        || request.configuration_limit_bytes == 0
        || request.configuration_limit_bytes > MAX_STATE
        || request.control_buffer_grant_bytes < value_bytes
        || request.control_buffer_grant_bytes > MAX_BUFFER_GRANT
    {
        return Err(invalid(
            "native control request exceeds admitted wire bounds",
        ));
    }
    Ok(())
}
pub(super) fn write_request(
    writer: &mut impl Write,
    request: &NativeConfigurationControlRequest<'_>,
) -> io::Result<()> {
    validate_request(request)?;
    let mut bytes = [0; REQUEST_SIZE];
    put_u64(&mut bytes, 0, request.control_token.get());
    put_cohort(&mut bytes, 8, request.expected_cohort)?;
    put_u32(
        &mut bytes,
        20,
        request.canonical_constant_values.len() as u32,
    );
    put_u64(&mut bytes, 24, request.configuration_limit_bytes as u64);
    put_u64(&mut bytes, 32, request.control_buffer_grant_bytes as u64);
    writer.write_all(&bytes)?;
    for value in request.canonical_constant_values {
        writer.write_all(&value.to_le_bytes())?;
    }
    writer.flush()
}
pub(super) fn read_request(
    reader: &mut impl Read,
    admitted_lanes: usize,
) -> io::Result<OwnedControlRequest> {
    let mut bytes = [0; REQUEST_SIZE];
    reader.read_exact(&mut bytes)?;
    let cohort = get_cohort(&bytes, 8)?;
    let token =
        NonZeroU64::new(u64_at(&bytes, 0)?).ok_or_else(|| invalid("empty native control token"))?;
    let lanes = u32_at(&bytes, 20)? as usize;
    let state_limit = usize_at(&bytes, 24)?;
    let buffer_grant = usize_at(&bytes, 32)?;
    let value_bytes = lanes
        .checked_mul(size_of::<f64>())
        .ok_or_else(|| invalid("control values overflow"))?;
    if lanes != admitted_lanes
        || lanes > MAX_LANES
        || state_limit == 0
        || state_limit > MAX_STATE
        || buffer_grant < value_bytes
        || buffer_grant > MAX_BUFFER_GRANT
    {
        return Err(invalid(
            "native control request bounds rejected before allocation",
        ));
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(lanes)
        .map_err(|_| invalid("control values allocation failed"))?;
    if values
        .capacity()
        .checked_mul(size_of::<f64>())
        .is_none_or(|actual| actual > buffer_grant)
    {
        return Err(invalid("control values actual capacity exceeds grant"));
    }
    for _ in 0..lanes {
        let mut value = [0; 8];
        reader.read_exact(&mut value)?;
        let value = f64::from_le_bytes(value);
        if !value.is_finite() {
            return Err(invalid("nonfinite native control scalar"));
        }
        values.push(value);
    }
    Ok(OwnedControlRequest { cohort, token, values, state_limit, buffer_grant })
}

fn refusal_tag(refusal: NativeConfigurationRefusal) -> u32 {
    match refusal {
        NativeConfigurationRefusal::UnsupportedAdapter => 1,
        NativeConfigurationRefusal::SerialExhausted => 2,
        NativeConfigurationRefusal::UnsupportedRequest => 3,
        NativeConfigurationRefusal::UnsupportedConstantProfile => 4,
        NativeConfigurationRefusal::MetadataChanged => 5,
        NativeConfigurationRefusal::ConfigurationUnavailable => 6,
        NativeConfigurationRefusal::ConfigurationTooLarge => 7,
        NativeConfigurationRefusal::BufferGrantExceeded => 8,
        NativeConfigurationRefusal::AwaitingInstallation => 9,
        NativeConfigurationRefusal::HostAllocationUnavailable => 10,
    }
}
fn refusal_from_tag(tag: u32) -> io::Result<NativeConfigurationRefusal> {
    Ok(match tag {
        1 => NativeConfigurationRefusal::UnsupportedAdapter,
        2 => NativeConfigurationRefusal::SerialExhausted,
        3 => NativeConfigurationRefusal::UnsupportedRequest,
        4 => NativeConfigurationRefusal::UnsupportedConstantProfile,
        5 => NativeConfigurationRefusal::MetadataChanged,
        6 => NativeConfigurationRefusal::ConfigurationUnavailable,
        7 => NativeConfigurationRefusal::ConfigurationTooLarge,
        8 => NativeConfigurationRefusal::BufferGrantExceeded,
        9 => NativeConfigurationRefusal::AwaitingInstallation,
        10 => NativeConfigurationRefusal::HostAllocationUnavailable,
        _ => return Err(invalid("unknown native configuration refusal")),
    })
}

pub(super) enum ControlReply {
    Outcome(NativeConfigurationControlOutcome),
    Failed(String),
}
pub(super) fn write_reply(writer: &mut impl Write, reply: ControlReply) -> io::Result<()> {
    match reply {
        ControlReply::Outcome(NativeConfigurationControlOutcome::ReadyForInstall(candidate)) => {
            if candidate.configuration.len() > MAX_STATE
                || candidate.buffers.configuration_capacity_bytes
                    != candidate.configuration.capacity()
            {
                return Err(invalid(
                    "native configuration exceeds wire bound or contradicts its capacity receipt",
                ));
            }
            let mut bytes = [0; READY_SIZE];
            put_u64(&mut bytes, 0, candidate.control_token.get());
            put_cohort(&mut bytes, 8, candidate.serviced_cohort)?;
            put_u32(
                &mut bytes,
                20,
                match candidate.format {
                    NativeConfigurationFormat::Vst3ComponentAndController => 1,
                    NativeConfigurationFormat::ClapState => 2,
                },
            );
            let contract = candidate.observed_contract;
            put_u64(&mut bytes, 24, contract.algorithmic_latency_frames() as u64);
            let (kind, frames) = match contract.tail() {
                AudioProcessorTail::None => (0, 0),
                AudioProcessorTail::Finite(frames) => (1, frames as u64),
                AudioProcessorTail::Infinite => (2, 0),
            };
            put_u32(&mut bytes, 32, kind);
            put_u64(&mut bytes, 40, frames);
            put_u32(
                &mut bytes,
                48,
                u32::from(contract.requires_state_entry())
                    | (u32::from(contract.realtime_capable()) << 1)
                    | (u32::from(contract.offline_capable()) << 2),
            );
            put_u64(&mut bytes, 56, contract.session_scratch_bytes() as u64);
            put_u64(
                &mut bytes,
                64,
                candidate.buffers.request_value_capacity_bytes as u64,
            );
            put_u64(
                &mut bytes,
                72,
                candidate.buffers.capture_payload_bytes as u64,
            );
            put_u64(
                &mut bytes,
                80,
                candidate.buffers.configuration_capacity_bytes as u64,
            );
            put_u64(&mut bytes, 88, candidate.configuration.len() as u64);
            write_header(writer, 0, READY_SIZE + candidate.configuration.len())?;
            writer.write_all(&bytes)?;
            writer.write_all(&candidate.configuration)?;
        }
        ControlReply::Outcome(NativeConfigurationControlOutcome::StillPending(cohort)) => {
            if let Some(cohort) = cohort {
                let mut bytes = [0; 12];
                put_cohort(&mut bytes, 0, cohort)?;
                write_header(writer, 1, bytes.len())?;
                writer.write_all(&bytes)?;
            } else {
                write_header(writer, 1, 0)?;
            }
        }
        ControlReply::Outcome(NativeConfigurationControlOutcome::CapabilityRefused(refusal)) => {
            write_header(writer, 2, 4)?;
            writer.write_all(&refusal_tag(refusal).to_le_bytes())?;
        }
        ControlReply::Failed(mut detail) => {
            let mut length = detail.len().min(MAX_WORKER_DETAIL_BYTES);
            while !detail.is_char_boundary(length) {
                length -= 1;
            }
            detail.truncate(length);
            write_header(writer, 3, length)?;
            writer.write_all(detail.as_bytes())?;
        }
    }
    writer.flush()
}
pub(super) fn read_reply(
    reader: &mut impl Read,
    request: &NativeConfigurationControlRequest<'_>,
) -> io::Result<ControlReply> {
    let (status, extent) = read_header(reader)?;
    match (status, extent) {
        (0, extent)
            if extent >= READY_SIZE && extent <= READY_SIZE + request.configuration_limit_bytes =>
        {
            let mut bytes = [0; READY_SIZE];
            reader.read_exact(&mut bytes)?;
            if bytes[36..40] != [0; 4] || bytes[52..56] != [0; 4] || bytes[96..] != [0; 16] {
                return Err(invalid("noncanonical native control reserved fields"));
            }
            let token = NonZeroU64::new(u64_at(&bytes, 0)?)
                .ok_or_else(|| invalid("empty candidate token"))?;
            let cohort = get_cohort(&bytes, 8)?;
            if token != request.control_token || cohort != request.expected_cohort {
                return Err(invalid("native candidate identity mismatch"));
            }
            let format = match (u32_at(&bytes, 20)?, cohort.request) {
                (1, NativeAudioReprepareRequest::Vst3Restart { .. }) => {
                    NativeConfigurationFormat::Vst3ComponentAndController
                }
                (2, NativeAudioReprepareRequest::ClapRestart) => {
                    NativeConfigurationFormat::ClapState
                }
                _ => return Err(invalid("native candidate configuration codec mismatch")),
            };
            let tail = match (u32_at(&bytes, 32)?, usize_at(&bytes, 40)?) {
                (0, 0) => AudioProcessorTail::None,
                (1, frames) if frames != 0 => AudioProcessorTail::Finite(frames),
                (2, 0) => AudioProcessorTail::Infinite,
                _ => return Err(invalid("invalid candidate tail")),
            };
            let flags = u32_at(&bytes, 48)?;
            if flags & !7 != 0 {
                return Err(invalid("unknown candidate capability flags"));
            }
            let observed_contract = AudioProcessorExecutionContract::new(
                usize_at(&bytes, 24)?,
                tail,
                flags & 1 != 0,
                flags & 2 != 0,
                flags & 4 != 0,
                usize_at(&bytes, 56)?,
            )
            .map_err(|_| invalid("invalid observed candidate contract"))?;
            let buffers = NativeControlBufferReceipt {
                request_value_capacity_bytes: usize_at(&bytes, 64)?,
                capture_payload_bytes: usize_at(&bytes, 72)?,
                configuration_capacity_bytes: usize_at(&bytes, 80)?,
            };
            let used = usize_at(&bytes, 88)?;
            if used != extent - READY_SIZE
                || used > buffers.configuration_capacity_bytes
                || buffers
                    .peak_bytes()
                    .is_none_or(|peak| peak > request.control_buffer_grant_bytes)
            {
                return Err(invalid(
                    "native candidate buffer extent exceeds admitted grant",
                ));
            }
            // Allocate only after all fixed facts and body extents have been validated.
            let mut configuration = Vec::new();
            configuration
                .try_reserve_exact(used)
                .map_err(|_| invalid("parent configuration allocation failed"))?;
            if configuration.capacity() > request.configuration_limit_bytes {
                return Err(invalid(
                    "parent configuration actual capacity exceeds extent",
                ));
            }
            let parent_configuration_capacity_bytes = configuration.capacity();
            configuration.resize(used, 0);
            reader.read_exact(&mut configuration)?;
            Ok(ControlReply::Outcome(
                NativeConfigurationControlOutcome::ReadyForInstall(NativeConfigurationCandidate {
                    serviced_cohort: cohort,
                    control_token: token,
                    observed_contract,
                    format,
                    configuration,
                    buffers,
                    parent_configuration_capacity_bytes,
                }),
            ))
        }
        (1, 0) => Ok(ControlReply::Outcome(
            NativeConfigurationControlOutcome::StillPending(None),
        )),
        (1, 12) => {
            let mut bytes = [0; 12];
            reader.read_exact(&mut bytes)?;
            Ok(ControlReply::Outcome(
                NativeConfigurationControlOutcome::StillPending(Some(get_cohort(&bytes, 0)?)),
            ))
        }
        (2, 4) => {
            let mut bytes = [0; 4];
            reader.read_exact(&mut bytes)?;
            Ok(ControlReply::Outcome(
                NativeConfigurationControlOutcome::CapabilityRefused(refusal_from_tag(u32_at(
                    &bytes, 0,
                )?)?),
            ))
        }
        (3, extent) if extent <= MAX_WORKER_DETAIL_BYTES => {
            let mut bytes = vec![0; extent];
            reader.read_exact(&mut bytes)?;
            Ok(ControlReply::Failed(
                String::from_utf8(bytes).map_err(|_| invalid("invalid native failure detail"))?,
            ))
        }
        _ => Err(invalid(
            "native configuration response rejected before allocation",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_configuration_wire_refuses_invalid_lengths_before_allocation() {
        let cohort =
            NativeRequestCohort::new(1, NativeAudioReprepareRequest::ClapRestart).expect("cohort");
        let request = NativeConfigurationControlRequest {
            expected_cohort: cohort,
            control_token: NonZeroU64::new(17).expect("token"),
            canonical_constant_values: &[0.5],
            configuration_limit_bytes: 16,
            control_buffer_grant_bytes: 32,
        };
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request).expect("request");
        assert_eq!(bytes.len(), REQUEST_SIZE + 8);
        assert_eq!(
            read_request(&mut bytes.as_slice(), 1)
                .expect("valid request")
                .value_bytes()
                .expect("bytes"),
            8
        );
        put_u32(&mut bytes, 20, u32::MAX);
        assert!(read_request(&mut bytes.as_slice(), 1).is_err());
        let mut oversized = Vec::new();
        write_header(&mut oversized, 0, READY_SIZE + 17).expect("header");
        assert!(read_reply(&mut oversized.as_slice(), &request).is_err());
    }

    #[test]
    fn native_configuration_wire_keeps_exact_tokens_codecs_and_separate_parent_capacity() {
        let cohort =
            NativeRequestCohort::new(1, NativeAudioReprepareRequest::ClapRestart).expect("cohort");
        let request = NativeConfigurationControlRequest {
            expected_cohort: cohort,
            control_token: NonZeroU64::new(17).expect("token"),
            canonical_constant_values: &[0.5],
            configuration_limit_bytes: 16,
            control_buffer_grant_bytes: 32,
        };
        let mut configuration = Vec::with_capacity(16);
        configuration.extend_from_slice(&[7, 1]);
        let candidate = NativeConfigurationCandidate {
            serviced_cohort: cohort,
            control_token: request.control_token,
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
            configuration,
            buffers: NativeControlBufferReceipt {
                request_value_capacity_bytes: 8,
                capture_payload_bytes: 0,
                configuration_capacity_bytes: 16,
            },
            parent_configuration_capacity_bytes: 0,
        };
        let mut bytes = Vec::new();
        write_reply(
            &mut bytes,
            ControlReply::Outcome(NativeConfigurationControlOutcome::ReadyForInstall(
                candidate,
            )),
        )
        .expect("encode");
        let ControlReply::Outcome(NativeConfigurationControlOutcome::ReadyForInstall(decoded)) =
            read_reply(&mut bytes.as_slice(), &request).expect("decode")
        else {
            panic!("ready candidate");
        };
        assert_eq!(decoded.configuration, [7, 1]);
        assert_eq!(decoded.buffers.configuration_capacity_bytes, 16);
        assert_eq!(
            decoded.parent_configuration_capacity_bytes,
            decoded.configuration.capacity()
        );
        put_u64(&mut bytes, 16, 18);
        assert!(
            read_reply(&mut bytes.as_slice(), &request).is_err(),
            "another service token must not install"
        );
    }
}
