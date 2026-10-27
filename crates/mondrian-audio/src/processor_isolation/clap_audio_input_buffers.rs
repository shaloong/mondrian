//! Root-granted staging of native inputs; never activation or mapping publication.
use super::{ClapAudioInputProposal, NativeConfigurationRefusal as Refusal};
use clack_host::prelude::AudioPorts;
use mondrian_core::AudioChannelLayout;
use std::mem::size_of;

/// Exact stopped proposal plus the original Root's incremental buffer grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapAudioInputBufferRequest {
    /// Observation issued by this same native object and control attempt.
    pub proposal: ClapAudioInputProposal,
    /// Must equal the already admitted native maximum block extent.
    pub max_block_frames: usize,
    /// New owned planes/SDK storage/Box, overlapping all original owners.
    pub buffer_grant_bytes: usize,
}

/// Physical staged host owners, not a new processing contract or seed authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapAudioInputBufferReceipt {
    /// Exact original request; cannot apply to another geometry or owner.
    pub request: ClapAudioInputBufferRequest,
    /// Pure bound admitted before any new allocation.
    pub reservation_bytes: usize,
    /// Actual capacities of the staged Box and all nested host audio owners.
    pub allocated_bytes: usize,
}

/// No outcome permits activation, cohort retirement, Enter or old-shape DSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClapAudioInputBufferOutcome {
    /// New planes exist on the original child but have not been installed.
    Staged(ClapAudioInputBufferReceipt),
    /// Original owner remains blocked; no implicit retry or fallback follows.
    Refused(Refusal),
}

pub(super) struct StagedClapAudioInputs {
    pub(super) input: Vec<Vec<Vec<f32>>>,
    pub(super) ports: AudioPorts,
    pub(super) receipt: ClapAudioInputBufferReceipt,
}

/// Shared pure model for CLAP's existing cold planes and staged replacement.
pub(super) fn plane_heap_reservation(
    ports: usize,
    channels: usize,
    frames: usize,
) -> Option<usize> {
    if frames == 0
        || u32::try_from(frames).is_err()
        || ports > 32
        || channels < ports
        || channels > ports.checked_mul(2)?
        || (ports == 0) != (channels == 0)
    {
        return None;
    }
    ports
        .checked_mul(size_of::<Vec<Vec<f32>>>())?
        .checked_add(channels.checked_mul(size_of::<Vec<f32>>())?)?
        .checked_add(channels.checked_mul(frames)?.checked_mul(size_of::<f32>())?)?
        .checked_add(channels.checked_mul(size_of::<*mut f32>())?)?
        .checked_add(ports.checked_mul(size_of::<clap_sys::audio_buffer::clap_audio_buffer>())?)
}

pub(crate) fn reservation(proposal: &ClapAudioInputProposal, frames: usize) -> Option<usize> {
    let channels = proposal.ports().try_fold(0_usize, |n, port| {
        if !matches!(
            port.channel_layout,
            AudioChannelLayout::Mono | AudioChannelLayout::Stereo
        ) {
            return None;
        }
        n.checked_add(port.channel_layout.channel_count())
    })?;
    plane_heap_reservation(proposal.ports().len(), channels, frames)?
        .checked_add(size_of::<StagedClapAudioInputs>())
}

/// Nested channel/PCM and SDK capacities. The caller charges the actual outer
/// Vec capacity separately, because a borrowed slice does not own that fact.
pub(super) fn nested_storage_bytes(ports: &[Vec<Vec<f32>>], sdk: &AudioPorts) -> Option<usize> {
    let mut bytes = 0_usize;
    for port in ports {
        bytes = bytes.checked_add(port.capacity().checked_mul(size_of::<Vec<f32>>())?)?;
        for channel in port {
            bytes = bytes.checked_add(channel.capacity().checked_mul(size_of::<f32>())?)?;
        }
    }
    for owner in sdk.allocated_storage().ok()? {
        bytes = bytes.checked_add(owner.allocation_layout().size())?;
    }
    Some(bytes)
}

impl StagedClapAudioInputs {
    pub(super) fn prepare(request: ClapAudioInputBufferRequest) -> Result<Box<Self>, Refusal> {
        let bound = reservation(&request.proposal, request.max_block_frames)
            .ok_or(Refusal::BufferGrantExceeded)?;
        if bound > request.buffer_grant_bytes {
            return Err(Refusal::BufferGrantExceeded);
        }
        let mut input = Vec::new();
        input
            .try_reserve_exact(request.proposal.ports().len())
            .map_err(|_| Refusal::HostAllocationUnavailable)?;
        let mut channels = 0;
        for port in request.proposal.ports() {
            let count = port.channel_layout.channel_count();
            channels += count;
            let mut planes = Vec::new();
            planes
                .try_reserve_exact(count)
                .map_err(|_| Refusal::HostAllocationUnavailable)?;
            for _ in 0..count {
                let mut pcm = Vec::new();
                pcm.try_reserve_exact(request.max_block_frames)
                    .map_err(|_| Refusal::HostAllocationUnavailable)?;
                pcm.resize(request.max_block_frames, 0.0);
                planes.push(pcm);
            }
            input.push(planes);
        }
        let ports = AudioPorts::with_capacity(channels, input.len());
        let mut staged = Box::new(Self {
            input,
            ports,
            receipt: ClapAudioInputBufferReceipt {
                request,
                reservation_bytes: bound,
                allocated_bytes: 0,
            },
        });
        let actual = size_of::<Self>()
            .checked_add(
                staged
                    .input
                    .capacity()
                    .checked_mul(size_of::<Vec<Vec<f32>>>())
                    .ok_or(Refusal::BufferGrantExceeded)?,
            )
            .and_then(|bytes| {
                bytes.checked_add(nested_storage_bytes(&staged.input, &staged.ports)?)
            })
            .ok_or(Refusal::BufferGrantExceeded)?;
        if actual > bound {
            return Err(Refusal::BufferGrantExceeded);
        }
        staged.receipt.allocated_bytes = actual;
        Ok(staged)
    }
}

/// Fixed-size native control codec; no extent from the plugin allocates memory.
pub(super) mod wire {
    use super::*;
    use crate::processor_isolation::native_configuration_wire::{
        decode_input_proposal, encode_input_proposal, refusal_from_tag, refusal_tag,
        INPUT_PROPOSAL_SIZE,
    };
    use crate::processor_isolation::protocol::PROTOCOL_VERSION;
    use std::io::{self, Read, Write};

    const REQUEST: usize = INPUT_PROPOSAL_SIZE + 16;
    const RECEIPT: usize = REQUEST + 16;
    fn invalid() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid CLAP input buffer control frame",
        )
    }
    fn number(bytes: &[u8], at: usize) -> io::Result<usize> {
        usize::try_from(u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()))
            .map_err(|_| invalid())
    }
    fn put(bytes: &mut [u8], at: usize, value: usize) {
        bytes[at..at + 8].copy_from_slice(&(value as u64).to_le_bytes());
    }
    fn header(writer: &mut impl Write, status: u32, bytes: usize) -> io::Result<()> {
        let mut h = [0; 16];
        h[..4].copy_from_slice(b"MAIB");
        h[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        h[8..12].copy_from_slice(&status.to_le_bytes());
        h[12..].copy_from_slice(&(bytes as u32).to_le_bytes());
        writer.write_all(&h)
    }
    fn read_header(reader: &mut impl Read) -> io::Result<(u32, usize)> {
        let mut h = [0; 16];
        reader.read_exact(&mut h)?;
        if &h[..4] != b"MAIB" || h[4..8] != PROTOCOL_VERSION.to_le_bytes() {
            return Err(invalid());
        }
        Ok((
            u32::from_le_bytes(h[8..12].try_into().unwrap()),
            u32::from_le_bytes(h[12..].try_into().unwrap()) as usize,
        ))
    }
    fn request_bytes(request: ClapAudioInputBufferRequest) -> io::Result<[u8; REQUEST]> {
        if reservation(&request.proposal, request.max_block_frames).is_none() {
            return Err(invalid());
        }
        let mut bytes = [0; REQUEST];
        bytes[..INPUT_PROPOSAL_SIZE].copy_from_slice(&encode_input_proposal(request.proposal)?);
        put(&mut bytes, INPUT_PROPOSAL_SIZE, request.max_block_frames);
        put(
            &mut bytes,
            INPUT_PROPOSAL_SIZE + 8,
            request.buffer_grant_bytes,
        );
        Ok(bytes)
    }
    fn parse_request(bytes: &[u8; REQUEST]) -> io::Result<ClapAudioInputBufferRequest> {
        let request = ClapAudioInputBufferRequest {
            proposal: decode_input_proposal(bytes[..INPUT_PROPOSAL_SIZE].try_into().unwrap())?,
            max_block_frames: number(bytes, INPUT_PROPOSAL_SIZE)?,
            buffer_grant_bytes: number(bytes, INPUT_PROPOSAL_SIZE + 8)?,
        };
        if reservation(&request.proposal, request.max_block_frames).is_none() {
            return Err(invalid());
        }
        Ok(request)
    }
    pub(crate) fn validate_request(request: ClapAudioInputBufferRequest) -> io::Result<()> {
        request_bytes(request).map(|_| ())
    }
    pub(crate) fn write_request(
        writer: &mut impl Write,
        request: ClapAudioInputBufferRequest,
    ) -> io::Result<()> {
        let bytes = request_bytes(request)?;
        header(writer, 0, REQUEST)?;
        writer.write_all(&bytes)?;
        writer.flush()
    }
    pub(crate) fn read_request(reader: &mut impl Read) -> io::Result<ClapAudioInputBufferRequest> {
        if read_header(reader)? != (0, REQUEST) {
            return Err(invalid());
        }
        let mut bytes = [0; REQUEST];
        reader.read_exact(&mut bytes)?;
        parse_request(&bytes)
    }
    pub(crate) fn write_reply(
        writer: &mut impl Write,
        outcome: ClapAudioInputBufferOutcome,
    ) -> io::Result<()> {
        match outcome {
            ClapAudioInputBufferOutcome::Refused(refusal) => {
                header(writer, 1, 4)?;
                writer.write_all(&refusal_tag(refusal).to_le_bytes())?;
            }
            ClapAudioInputBufferOutcome::Staged(receipt) => {
                let bound =
                    reservation(&receipt.request.proposal, receipt.request.max_block_frames)
                        .ok_or_else(invalid)?;
                if receipt.reservation_bytes != bound
                    || receipt.allocated_bytes < size_of::<StagedClapAudioInputs>()
                    || receipt.allocated_bytes > bound
                    || bound > receipt.request.buffer_grant_bytes
                {
                    return Err(invalid());
                }
                let mut bytes = [0; RECEIPT];
                bytes[..REQUEST].copy_from_slice(&request_bytes(receipt.request)?);
                put(&mut bytes, REQUEST, receipt.reservation_bytes);
                put(&mut bytes, REQUEST + 8, receipt.allocated_bytes);
                header(writer, 0, RECEIPT)?;
                writer.write_all(&bytes)?;
            }
        }
        writer.flush()
    }
    pub(crate) fn write_failed(writer: &mut impl Write, detail: &str) -> io::Result<()> {
        let mut len = detail.len().min(4096);
        while !detail.is_char_boundary(len) {
            len -= 1;
        }
        header(writer, 2, len)?;
        writer.write_all(&detail.as_bytes()[..len])?;
        writer.flush()
    }
    pub(crate) fn read_reply(
        reader: &mut impl Read,
        expected: ClapAudioInputBufferRequest,
    ) -> io::Result<ClapAudioInputBufferOutcome> {
        match read_header(reader)? {
            (2, len) if len <= 4096 => {
                let mut bytes = [0; 4096];
                reader.read_exact(&mut bytes[..len])?;
                let detail = std::str::from_utf8(&bytes[..len]).map_err(|_| invalid())?;
                Err(io::Error::other(detail.to_owned()))
            }
            (1, 4) => {
                let mut bytes = [0; 4];
                reader.read_exact(&mut bytes)?;
                Ok(ClapAudioInputBufferOutcome::Refused(refusal_from_tag(
                    u32::from_le_bytes(bytes),
                )?))
            }
            (0, RECEIPT) => {
                let mut bytes = [0; RECEIPT];
                reader.read_exact(&mut bytes)?;
                let request = parse_request(bytes[..REQUEST].try_into().unwrap())?;
                let bound =
                    reservation(&request.proposal, request.max_block_frames).ok_or_else(invalid)?;
                let reserved = number(&bytes, REQUEST)?;
                let actual = number(&bytes, REQUEST + 8)?;
                if request != expected
                    || reserved != bound
                    || actual < size_of::<StagedClapAudioInputs>()
                    || actual > bound
                    || bound > request.buffer_grant_bytes
                {
                    return Err(invalid());
                }
                Ok(ClapAudioInputBufferOutcome::Staged(
                    ClapAudioInputBufferReceipt {
                        request,
                        reservation_bytes: reserved,
                        allocated_bytes: actual,
                    },
                ))
            }
            _ => Err(invalid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClapAudioInputPort, NativeRequestCohort};
    use mondrian_core::NativeAudioReprepareRequest;
    use std::num::NonZeroU64;

    fn proposal(main: bool) -> ClapAudioInputProposal {
        let mut p = ClapAudioInputProposal::new(
            NativeRequestCohort::new(1, NativeAudioReprepareRequest::ClapRestart).unwrap(),
            NonZeroU64::new(1).unwrap(),
        )
        .unwrap();
        for (id, layout, is_main) in [
            (0, AudioChannelLayout::Stereo, main),
            (11, AudioChannelLayout::Mono, false),
            (7, AudioChannelLayout::Stereo, false),
        ] {
            p.push(ClapAudioInputPort { native_id: id, channel_layout: layout, is_main })
                .unwrap();
        }
        p
    }

    #[test]
    fn staged_planes_preserve_each_declared_width_and_preflight_the_complete_grant() {
        for main in [true, false] {
            let p = proposal(main);
            for frames in [1, 4, 64] {
                let bound = reservation(&p, frames).unwrap();
                let mut request = ClapAudioInputBufferRequest {
                    proposal: p,
                    max_block_frames: frames,
                    buffer_grant_bytes: bound - 1,
                };
                assert!(matches!(
                    StagedClapAudioInputs::prepare(request),
                    Err(Refusal::BufferGrantExceeded)
                ));
                request.buffer_grant_bytes = bound;
                let staged = StagedClapAudioInputs::prepare(request).unwrap();
                assert_eq!(
                    staged.input.iter().map(Vec::len).collect::<Vec<_>>(),
                    [2, 1, 2]
                );
                assert!(staged
                    .input
                    .iter()
                    .flatten()
                    .all(|pcm| pcm.len() == frames && pcm.iter().all(|&x| x == 0.0)));
                assert!(staged.receipt.allocated_bytes <= bound);
            }
        }
        assert!(reservation(&proposal(true), 0).is_none());
        assert!(reservation(&proposal(true), usize::MAX).is_none());
        assert!(plane_heap_reservation(2, 1, 4).is_none());
    }

    #[test]
    fn staged_inputs_remain_blocked_without_ready_configuration_or_seed_authority() {
        use super::super::native_configuration_control::NativeConfigurationPhase;
        let proposal = proposal(true);
        let phase = NativeConfigurationPhase::AudioInputsStaged(proposal);
        assert_eq!(
            phase.blocked_request(),
            Some(proposal.observed_cohort.request)
        );
        assert_eq!(phase.ready_token(), None);
        assert_eq!(phase.seed_ticket(), None);
    }

    #[test]
    fn buffer_control_correlates_whole_request_and_refuses_malformed_or_oversized_receipts() {
        let p = proposal(true);
        let request = ClapAudioInputBufferRequest {
            proposal: p,
            max_block_frames: 4,
            buffer_grant_bytes: reservation(&p, 4).unwrap(),
        };
        let mut bytes = Vec::new();
        wire::write_request(&mut bytes, request).unwrap();
        assert_eq!(wire::read_request(&mut bytes.as_slice()).unwrap(), request);
        let mut command = Vec::new();
        super::super::protocol::WorkerCommand {
            transport_activity: None,
            kind: super::super::protocol::CommandKind::StageClapAudioInputBuffers,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut command)
        .unwrap();
        command.extend_from_slice(&bytes);
        let mut stream = command.as_slice();
        let super::super::native_worker_command::NativeCommand::StageClapAudioInputBuffers(decoded) =
            super::super::native_worker_command::decode(&mut stream, 0).unwrap()
        else {
            panic!("exact new payload kind");
        };
        assert_eq!(decoded, request);
        assert!(stream.is_empty());
        let mut old = bytes.clone();
        old[4..8].copy_from_slice(&(super::super::protocol::PROTOCOL_VERSION - 1).to_le_bytes());
        assert!(wire::read_request(&mut old.as_slice()).is_err());
        for len in [0, 15, bytes.len() - 1] {
            assert!(wire::read_request(&mut &bytes[..len]).is_err());
        }
        let staged = StagedClapAudioInputs::prepare(request).unwrap();
        bytes.clear();
        wire::write_reply(
            &mut bytes,
            ClapAudioInputBufferOutcome::Staged(staged.receipt),
        )
        .unwrap();
        assert_eq!(
            wire::read_reply(&mut bytes.as_slice(), request).unwrap(),
            ClapAudioInputBufferOutcome::Staged(staged.receipt)
        );
        let mut other = request;
        other.buffer_grant_bytes += 1;
        assert!(wire::read_reply(&mut bytes.as_slice(), other).is_err());
        let end = bytes.len();
        for actual in [0_u64, u64::MAX] {
            let mut malformed = bytes.clone();
            malformed[end - 8..].copy_from_slice(&actual.to_le_bytes());
            assert!(wire::read_reply(&mut malformed.as_slice(), request).is_err());
        }
        bytes.clear();
        wire::write_reply(
            &mut bytes,
            ClapAudioInputBufferOutcome::Refused(Refusal::BufferGrantExceeded),
        )
        .unwrap();
        assert_eq!(
            wire::read_reply(&mut bytes.as_slice(), request).unwrap(),
            ClapAudioInputBufferOutcome::Refused(Refusal::BufferGrantExceeded)
        );
        bytes.clear();
        wire::write_failed(&mut bytes, "native metadata query failed").unwrap();
        assert!(wire::read_reply(&mut bytes.as_slice(), request)
            .unwrap_err()
            .to_string()
            .contains("metadata query failed"));
    }
}
