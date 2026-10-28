//! Stopped shared-storage staging. No active interpretation or DSP is replaced.
use super::protocol::SharedBlockLayout;
use super::{ClapAudioInputBufferReceipt, NativeConfigurationRefusal};
use crate::{
    AudioProcessorAuxiliaryInputBusContract, AudioProcessorAuxiliaryInputContract,
    AudioProcessorHostError,
};
use memmap2::{MmapMut, MmapOptions};
use mondrian_core::AudioChannelLayout;
use std::{fmt::Write as _, fs::File, mem::size_of, path::Path};

const INPUT_KEY: &str = "clap.audio-input.";
const KEY_CAPACITY: usize = INPUT_KEY.len() + 10;

/// A new transport interpretation derived from an already staged native input set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapAudioInputMappingRequest {
    /// Actual same-child input resource receipt, not a caller-invented shape.
    pub buffers: ClapAudioInputBufferReceipt,
    /// Existing Main/output semantic layout; this stage cannot change it.
    pub channel_layout: AudioChannelLayout,
    /// Existing immutable parameter directory extent.
    pub parameter_lane_count: usize,
    /// Existing immutable auxiliary output count.
    pub auxiliary_output_count: usize,
    /// Both mappings, typed metadata and complete staging Box layouts overlap old owners.
    pub mapping_grant_bytes: usize,
}

/// Both peers retained matching staged storage; activation/install remain separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapAudioInputMappingReceipt {
    /// Exact request; cannot be reused for another child, geometry or generation.
    pub request: ClapAudioInputMappingRequest,
    /// Independently computed file/mapping extent on both peers.
    pub shared_bytes: usize,
    /// Actual known parent owners; std path/allocator/kernel internals are excluded.
    pub parent_owner_bytes: usize,
    /// Actual known child owners; no cross-process address union or early credit.
    pub child_owner_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "The bounded Copy mapping receipt preserves the exact inline cross-process accounting contract."
)]
pub enum ClapAudioInputMappingOutcome {
    /// Storage exists on both peers but remains unavailable to processing.
    Staged(ClapAudioInputMappingReceipt),
    /// Existing processing stays blocked; no automatic fallback or retry.
    Refused(NativeConfigurationRefusal),
}

enum MappingFile {
    Parent(tempfile::NamedTempFile),
    Child { _file: File },
}

pub(super) struct StagedClapInputMapping {
    pub(super) shared: MmapMut,
    file: MappingFile,
    pub(super) layout: SharedBlockLayout,
    pub(super) auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    pub(super) request: ClapAudioInputMappingRequest,
}

fn invalid(detail: &'static str) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.to_owned())
}

pub(crate) fn geometry(
    request: ClapAudioInputMappingRequest,
) -> Result<SharedBlockLayout, AudioProcessorHostError> {
    let b = request.buffers;
    let buffer_bound = super::clap_audio_input_buffers::reservation(
        &b.request.proposal,
        b.request.max_block_frames,
    )
    .ok_or_else(|| invalid("invalid staged input geometry"))?;
    if b.reservation_bytes != buffer_bound
        || b.allocated_bytes == 0
        || b.allocated_bytes > buffer_bound
        || b.request.buffer_grant_bytes < buffer_bound
        || !matches!(
            request.channel_layout,
            AudioChannelLayout::Mono | AudioChannelLayout::Stereo
        )
        || request.parameter_lane_count > 4096
        || request.auxiliary_output_count > 31
        || b.request
            .proposal
            .ports()
            .any(|p| p.is_main && p.channel_layout != request.channel_layout)
    {
        return Err(invalid("staged input mapping changes its admitted context"));
    }
    let channels = b
        .request
        .proposal
        .ports()
        .filter(|p| !p.is_main)
        .try_fold(0_usize, |n, p| {
            n.checked_add(p.channel_layout.channel_count())
        })
        .ok_or_else(|| invalid("input mapping channel extent overflowed"))?;
    let samples = channels
        .checked_mul(b.request.max_block_frames)
        .ok_or_else(|| invalid("input mapping sample extent overflowed"))?;
    SharedBlockLayout::with_auxiliary_samples(
        b.request.max_block_frames,
        request.channel_layout.channel_count(),
        samples,
        request.parameter_lane_count,
    )?
    .with_auxiliary_outputs(request.auxiliary_output_count)?
    .with_notes(0, 0)
}

/// One conservative peer bound; Root admits two peers before either allocates.
pub(crate) fn role_reservation(request: ClapAudioInputMappingRequest) -> Option<usize> {
    let layout = geometry(request).ok()?;
    let ports = request.buffers.request.proposal.ports().filter(|p| !p.is_main).count();
    size_of::<StagedClapInputMapping>()
        .checked_add(layout.total_bytes)?
        .checked_add(ports.checked_mul(size_of::<AudioProcessorAuxiliaryInputBusContract>())?)?
        .checked_add(ports.checked_mul(KEY_CAPACITY)?)
}

pub(crate) fn reservation(request: ClapAudioInputMappingRequest) -> Option<usize> {
    role_reservation(request)?.checked_mul(2)
}

pub(crate) fn shared_bytes(request: ClapAudioInputMappingRequest) -> Option<usize> {
    Some(geometry(request).ok()?.total_bytes)
}

fn inputs(
    request: ClapAudioInputMappingRequest,
) -> Result<AudioProcessorAuxiliaryInputContract, AudioProcessorHostError> {
    let count = request.buffers.request.proposal.ports().filter(|p| !p.is_main).count();
    let mut buses = Vec::new();
    buses
        .try_reserve_exact(count)
        .map_err(|_| invalid("input mapping metadata allocation failed"))?;
    if buses.capacity() > count {
        return Err(invalid(
            "input mapping metadata capacity exceeded admission",
        ));
    }
    for port in request.buffers.request.proposal.ports().filter(|p| !p.is_main) {
        let mut key = String::new();
        key.try_reserve_exact(KEY_CAPACITY)
            .map_err(|_| invalid("input mapping key allocation failed"))?;
        if key.capacity() > KEY_CAPACITY {
            return Err(invalid("input mapping key capacity exceeded admission"));
        }
        key.push_str(INPUT_KEY);
        write!(&mut key, "{}", port.native_id)
            .map_err(|_| invalid("input mapping key encoding failed"))?;
        buses.push(AudioProcessorAuxiliaryInputBusContract {
            bus_key: key,
            channel_layout: port.channel_layout,
        });
    }
    let inputs = AudioProcessorAuxiliaryInputContract { buses };
    inputs.validate(request.channel_layout)?;
    Ok(inputs)
}

// Staged bytes are not audio yet. A four-byte initialization challenge verifies
// that the actual mappings share one backing file. The parent clears it before
// any later promotion; neither peer processes this staging storage.
fn challenge(request: ClapAudioInputMappingRequest) -> u32 {
    let token = request.buffers.request.proposal.control_token.get();
    (token as u32)
        ^ ((token >> 32) as u32)
        ^ request.buffers.request.proposal.observed_cohort.serial
        ^ 0x4d41_494d
}

impl StagedClapInputMapping {
    pub(super) fn into_parent(
        self: Box<Self>,
    ) -> (
        MmapMut,
        tempfile::NamedTempFile,
        SharedBlockLayout,
        AudioProcessorAuxiliaryInputContract,
    ) {
        let Self { shared, file, layout, auxiliary_inputs, .. } = *self;
        let MappingFile::Parent(file) = file else {
            unreachable!("parent constructor retains parent file")
        };
        (shared, file, layout, auxiliary_inputs)
    }

    pub(super) fn into_child(
        self: Box<Self>,
    ) -> (
        MmapMut,
        File,
        SharedBlockLayout,
        AudioProcessorAuxiliaryInputContract,
    ) {
        let Self { shared, file, layout, auxiliary_inputs, .. } = *self;
        let MappingFile::Child { _file: file } = file else {
            unreachable!("child constructor retains child file")
        };
        (shared, file, layout, auxiliary_inputs)
    }

    pub(super) fn known_bytes(&self) -> Option<usize> {
        let mut n = size_of::<Self>().checked_add(self.shared.len())?.checked_add(
            self.auxiliary_inputs
                .buses
                .capacity()
                .checked_mul(size_of::<AudioProcessorAuxiliaryInputBusContract>())?,
        )?;
        for bus in &self.auxiliary_inputs.buses {
            n = n.checked_add(bus.bus_key.capacity())?;
        }
        Some(n)
    }
    pub(super) fn path(&self) -> Option<&Path> {
        match &self.file {
            MappingFile::Parent(file) => Some(file.path()),
            MappingFile::Child { .. } => None,
        }
    }
    pub(super) fn create_parent(
        request: ClapAudioInputMappingRequest,
        directory: &Path,
    ) -> Result<Box<Self>, AudioProcessorHostError> {
        let bound =
            reservation(request).ok_or_else(|| invalid("input mapping owner bound unavailable"))?;
        if bound > request.mapping_grant_bytes {
            return Err(invalid("input mapping grant is insufficient"));
        }
        let layout = geometry(request)?;
        let auxiliary_inputs = inputs(request)?;
        let file = tempfile::NamedTempFile::new_in(directory)
            .map_err(|_| invalid("input mapping file creation failed"))?;
        file.as_file()
            .set_len(layout.total_bytes as u64)
            .map_err(|_| invalid("input mapping file sizing failed"))?;
        // SAFETY: this unique fixed-size file is retained by the owner and never
        // resized. Staged access follows the sole control-channel handoff.
        let mut shared =
            unsafe { MmapOptions::new().len(layout.total_bytes).map_mut(file.as_file()) }
                .map_err(|_| invalid("input mapping parent map failed"))?;
        shared[..4].copy_from_slice(&challenge(request).to_le_bytes());
        let owner = Box::new(Self {
            shared,
            file: MappingFile::Parent(file),
            layout,
            auxiliary_inputs,
            request,
        });
        if owner.known_bytes().is_none_or(|n| n > bound / 2) {
            return Err(invalid("input mapping parent owners exceeded admission"));
        }
        Ok(owner)
    }
    pub(super) fn open_child(
        request: ClapAudioInputMappingRequest,
        path: &Path,
        directory: &Path,
    ) -> Result<Box<Self>, AudioProcessorHostError> {
        let bound =
            reservation(request).ok_or_else(|| invalid("input mapping owner bound unavailable"))?;
        if bound > request.mapping_grant_bytes {
            return Err(invalid("input mapping grant is insufficient"));
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| invalid("input mapping path resolution failed"))?;
        let directory = directory
            .canonicalize()
            .map_err(|_| invalid("input mapping directory resolution failed"))?;
        if canonical.parent() != Some(directory.as_path()) {
            return Err(invalid(
                "input mapping escaped its original process directory",
            ));
        }
        let layout = geometry(request)?;
        let file = File::options()
            .read(true)
            .write(true)
            .open(&canonical)
            .map_err(|_| invalid("input mapping child open failed"))?;
        if file
            .metadata()
            .map_err(|_| invalid("input mapping metadata query failed"))?
            .len()
            != layout.total_bytes as u64
        {
            return Err(invalid(
                "input mapping file extent differs from typed geometry",
            ));
        }
        let auxiliary_inputs = inputs(request)?;
        // SAFETY: same fixed-size parent-owned file, exact extent checked above;
        // no active processing view exists and peers hand access over via IPC.
        let mut shared = unsafe { MmapOptions::new().len(layout.total_bytes).map_mut(&file) }
            .map_err(|_| invalid("input mapping child map failed"))?;
        if shared[..4] != challenge(request).to_le_bytes() {
            return Err(invalid("input mapping backing challenge mismatch"));
        }
        shared[..4].copy_from_slice(&(!challenge(request)).to_le_bytes());
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        let owner = Box::new(Self {
            shared,
            file: MappingFile::Child { _file: file },
            layout,
            auxiliary_inputs,
            request,
        });
        if owner.known_bytes().is_none_or(|n| n > bound / 2) {
            return Err(invalid("input mapping child owners exceeded admission"));
        }
        Ok(owner)
    }
    pub(super) fn verify_child_backing(&mut self) -> Result<(), AudioProcessorHostError> {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        if self.shared[..4] != (!challenge(self.request)).to_le_bytes() {
            return Err(invalid(
                "input mapping acknowledgement has no matching shared backing",
            ));
        }
        self.shared[..4].fill(0);
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

pub(super) mod wire {
    use super::super::clap_audio_input_buffers::wire::{decode_receipt, encode_receipt, RECEIPT};
    use super::super::native_configuration_wire::{refusal_from_tag, refusal_tag};
    use super::super::protocol::PROTOCOL_VERSION;
    use super::*;
    use std::io::{self, Read, Write};
    pub(super) const MAX_PATH: usize = 4096;
    const META: usize = RECEIPT + 24;
    const REQUEST: usize = META + 16;
    const REPLY: usize = META + 24;

    pub(crate) struct OwnedRequest {
        pub request: ClapAudioInputMappingRequest,
        pub path: String,
        pub parent_bytes: usize,
    }
    fn invalid() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid input mapping control frame",
        )
    }
    fn put(bytes: &mut [u8], at: usize, value: usize) {
        bytes[at..at + 8].copy_from_slice(&(value as u64).to_le_bytes());
    }
    fn get(bytes: &[u8], at: usize) -> io::Result<usize> {
        usize::try_from(u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()))
            .map_err(|_| invalid())
    }
    fn header(writer: &mut impl Write, status: u32, size: usize) -> io::Result<()> {
        let mut bytes = [0; 16];
        bytes[..4].copy_from_slice(b"MAIM");
        bytes[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        bytes[8..12].copy_from_slice(&status.to_le_bytes());
        bytes[12..].copy_from_slice(&(size as u32).to_le_bytes());
        writer.write_all(&bytes)
    }
    fn read_header(reader: &mut impl Read) -> io::Result<(u32, usize)> {
        let mut bytes = [0; 16];
        reader.read_exact(&mut bytes)?;
        if &bytes[..4] != b"MAIM" || bytes[4..8] != PROTOCOL_VERSION.to_le_bytes() {
            return Err(invalid());
        }
        Ok((
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            u32::from_le_bytes(bytes[12..].try_into().unwrap()) as usize,
        ))
    }
    fn meta(request: ClapAudioInputMappingRequest) -> io::Result<[u8; META]> {
        if reservation(request).is_none_or(|n| n > request.mapping_grant_bytes) {
            return Err(invalid());
        }
        let mut bytes = [0; META];
        bytes[..RECEIPT].copy_from_slice(&encode_receipt(request.buffers)?);
        let tag: u32 = if request.channel_layout == AudioChannelLayout::Mono {
            1
        } else {
            2
        };
        bytes[RECEIPT..RECEIPT + 4].copy_from_slice(&tag.to_le_bytes());
        bytes[RECEIPT + 4..RECEIPT + 8]
            .copy_from_slice(&(request.parameter_lane_count as u32).to_le_bytes());
        bytes[RECEIPT + 8..RECEIPT + 12]
            .copy_from_slice(&(request.auxiliary_output_count as u32).to_le_bytes());
        put(&mut bytes, RECEIPT + 16, request.mapping_grant_bytes);
        Ok(bytes)
    }
    fn parse(bytes: &[u8; META]) -> io::Result<ClapAudioInputMappingRequest> {
        if bytes[RECEIPT + 12..RECEIPT + 16] != [0; 4] {
            return Err(invalid());
        }
        let tag = u32::from_le_bytes(bytes[RECEIPT..RECEIPT + 4].try_into().unwrap());
        let request = ClapAudioInputMappingRequest {
            buffers: decode_receipt(bytes[..RECEIPT].try_into().unwrap())?,
            channel_layout: match tag {
                1 => AudioChannelLayout::Mono,
                2 => AudioChannelLayout::Stereo,
                _ => return Err(invalid()),
            },
            parameter_lane_count: u32::from_le_bytes(
                bytes[RECEIPT + 4..RECEIPT + 8].try_into().unwrap(),
            ) as usize,
            auxiliary_output_count: u32::from_le_bytes(
                bytes[RECEIPT + 8..RECEIPT + 12].try_into().unwrap(),
            ) as usize,
            mapping_grant_bytes: get(bytes, RECEIPT + 16)?,
        };
        meta(request)?;
        Ok(request)
    }
    pub(crate) fn validate_request(
        request: ClapAudioInputMappingRequest,
        path: &str,
        parent: usize,
    ) -> io::Result<()> {
        meta(request)?;
        let bound = role_reservation(request).ok_or_else(invalid)?;
        if path.is_empty()
            || path.len() > MAX_PATH
            || path.as_bytes().contains(&0)
            || parent
                < geometry(request).map_err(|_| invalid())?.total_bytes
                    + size_of::<StagedClapInputMapping>()
            || parent > bound
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn write_request(
        writer: &mut impl Write,
        request: ClapAudioInputMappingRequest,
        path: &str,
        parent: usize,
    ) -> io::Result<()> {
        validate_request(request, path, parent)?;
        let mut bytes = [0; REQUEST];
        bytes[..META].copy_from_slice(&meta(request)?);
        put(&mut bytes, META, parent);
        bytes[META + 8..META + 12].copy_from_slice(&(path.len() as u32).to_le_bytes());
        header(writer, 0, REQUEST + path.len())?;
        writer.write_all(&bytes)?;
        writer.write_all(path.as_bytes())?;
        writer.flush()
    }
    pub(crate) fn read_request(reader: &mut impl Read) -> io::Result<OwnedRequest> {
        let (status, extent) = read_header(reader)?;
        if status != 0 || extent <= REQUEST || extent > REQUEST + MAX_PATH {
            return Err(invalid());
        }
        let mut bytes = [0; REQUEST];
        reader.read_exact(&mut bytes)?;
        if bytes[META + 12..] != [0; 4] {
            return Err(invalid());
        }
        let count = u32::from_le_bytes(bytes[META + 8..META + 12].try_into().unwrap()) as usize;
        if count != extent - REQUEST {
            return Err(invalid());
        }
        let request = parse(bytes[..META].try_into().unwrap())?;
        let parent_bytes = get(&bytes, META)?;
        let mut raw = [0; MAX_PATH];
        reader.read_exact(&mut raw[..count])?;
        let path = std::str::from_utf8(&raw[..count]).map_err(|_| invalid())?;
        validate_request(request, path, parent_bytes)?;
        let mut owned = String::new();
        owned.try_reserve_exact(count).map_err(|_| invalid())?;
        if owned.capacity() > MAX_PATH {
            return Err(invalid());
        }
        owned.push_str(path);
        Ok(OwnedRequest { request, path: owned, parent_bytes })
    }
    fn validate_receipt(receipt: ClapAudioInputMappingReceipt) -> io::Result<()> {
        meta(receipt.request)?;
        let layout = geometry(receipt.request).map_err(|_| invalid())?;
        let bound = role_reservation(receipt.request).ok_or_else(invalid)?;
        let minimum = layout
            .total_bytes
            .checked_add(size_of::<StagedClapInputMapping>())
            .ok_or_else(invalid)?;
        if receipt.shared_bytes != layout.total_bytes
            || receipt.parent_owner_bytes < minimum
            || receipt.parent_owner_bytes > bound
            || receipt.child_owner_bytes < minimum
            || receipt.child_owner_bytes > bound
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn write_reply(
        writer: &mut impl Write,
        outcome: ClapAudioInputMappingOutcome,
    ) -> io::Result<()> {
        match outcome {
            ClapAudioInputMappingOutcome::Refused(refusal) => {
                header(writer, 1, 4)?;
                writer.write_all(&refusal_tag(refusal).to_le_bytes())?;
            }
            ClapAudioInputMappingOutcome::Staged(receipt) => {
                validate_receipt(receipt)?;
                let mut bytes = [0; REPLY];
                bytes[..META].copy_from_slice(&meta(receipt.request)?);
                put(&mut bytes, META, receipt.shared_bytes);
                put(&mut bytes, META + 8, receipt.parent_owner_bytes);
                put(&mut bytes, META + 16, receipt.child_owner_bytes);
                header(writer, 0, REPLY)?;
                writer.write_all(&bytes)?;
            }
        }
        writer.flush()
    }
    pub(crate) fn write_failed(writer: &mut impl Write, detail: &str) -> io::Result<()> {
        let mut n = detail.len().min(4096);
        while !detail.is_char_boundary(n) {
            n -= 1;
        }
        header(writer, 2, n)?;
        writer.write_all(&detail.as_bytes()[..n])?;
        writer.flush()
    }
    pub(crate) fn read_reply(
        reader: &mut impl Read,
        expected: ClapAudioInputMappingRequest,
        parent: usize,
    ) -> io::Result<ClapAudioInputMappingOutcome> {
        match read_header(reader)? {
            (1, 4) => {
                let mut raw = [0; 4];
                reader.read_exact(&mut raw)?;
                Ok(ClapAudioInputMappingOutcome::Refused(refusal_from_tag(
                    u32::from_le_bytes(raw),
                )?))
            }
            (2, n) if n <= 4096 => {
                let mut raw = [0; 4096];
                reader.read_exact(&mut raw[..n])?;
                Err(io::Error::other(
                    std::str::from_utf8(&raw[..n]).map_err(|_| invalid())?.to_owned(),
                ))
            }
            (0, REPLY) => {
                let mut raw = [0; REPLY];
                reader.read_exact(&mut raw)?;
                let receipt = ClapAudioInputMappingReceipt {
                    request: parse(raw[..META].try_into().unwrap())?,
                    shared_bytes: get(&raw, META)?,
                    parent_owner_bytes: get(&raw, META + 8)?,
                    child_owner_bytes: get(&raw, META + 16)?,
                };
                validate_receipt(receipt)?;
                if receipt.request != expected || receipt.parent_owner_bytes != parent {
                    return Err(invalid());
                }
                Ok(ClapAudioInputMappingOutcome::Staged(receipt))
            }
            _ => Err(invalid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        ClapAudioInputBufferRequest, ClapAudioInputPort, ClapAudioInputProposal,
        NativeRequestCohort,
    };
    use super::*;
    use std::num::NonZeroU64;

    fn request(frames: usize, mono: bool) -> ClapAudioInputMappingRequest {
        let layout = if mono {
            AudioChannelLayout::Mono
        } else {
            AudioChannelLayout::Stereo
        };
        let mut proposal = ClapAudioInputProposal::new(
            NativeRequestCohort::new(3, mondrian_core::NativeAudioReprepareRequest::ClapRestart)
                .unwrap(),
            NonZeroU64::new(9).unwrap(),
        )
        .unwrap();
        proposal
            .push(ClapAudioInputPort {
                native_id: 0,
                channel_layout: layout,
                is_main: true,
            })
            .unwrap();
        if !mono {
            proposal
                .push(ClapAudioInputPort {
                    native_id: 11,
                    channel_layout: AudioChannelLayout::Mono,
                    is_main: false,
                })
                .unwrap();
            proposal
                .push(ClapAudioInputPort {
                    native_id: 7,
                    channel_layout: AudioChannelLayout::Stereo,
                    is_main: false,
                })
                .unwrap();
        }
        let bytes = super::super::clap_audio_input_buffers::reservation(&proposal, frames).unwrap();
        let buffers = super::super::clap_audio_input_buffers::StagedClapAudioInputs::prepare(
            ClapAudioInputBufferRequest {
                proposal,
                max_block_frames: frames,
                buffer_grant_bytes: bytes,
            },
        )
        .unwrap()
        .receipt;
        let mut request = ClapAudioInputMappingRequest {
            buffers,
            channel_layout: layout,
            parameter_lane_count: 0,
            auxiliary_output_count: 0,
            mapping_grant_bytes: 0,
        };
        request.mapping_grant_bytes = reservation(request).unwrap();
        request
    }

    #[test]
    fn both_actual_mappings_share_one_exact_backing_and_keep_typed_inputs() {
        for (frames, mono) in [(1, true), (4, false), (64, false)] {
            let directory = tempfile::tempdir().unwrap();
            let request = request(frames, mono);
            let mut parent =
                StagedClapInputMapping::create_parent(request, directory.path()).unwrap();
            assert!(
                parent.verify_child_backing().is_err(),
                "a parent-only map is not a receipt"
            );
            let child = StagedClapInputMapping::open_child(
                request,
                parent.path().unwrap(),
                directory.path(),
            )
            .unwrap();
            assert_eq!(parent.layout, child.layout);
            assert_eq!(parent.auxiliary_inputs, child.auxiliary_inputs);
            assert!(
                parent.known_bytes().unwrap() + child.known_bytes().unwrap()
                    <= request.mapping_grant_bytes
            );
            if mono {
                assert_eq!(parent.layout.main_samples, 1);
            } else {
                assert_eq!(
                    child
                        .auxiliary_inputs
                        .buses
                        .iter()
                        .map(|b| (&*b.bus_key, b.channel_layout))
                        .collect::<Vec<_>>(),
                    [
                        ("clap.audio-input.11", AudioChannelLayout::Mono),
                        ("clap.audio-input.7", AudioChannelLayout::Stereo)
                    ]
                );
                assert_eq!(child.layout.auxiliary_samples, frames * 3);
            }
            parent.verify_child_backing().unwrap();
            assert!(parent.shared.iter().all(|&b| b == 0));
            assert!(
                child.shared.iter().all(|&b| b == 0),
                "cleared bytes are observed through the second actual map"
            );
        }
    }

    #[test]
    fn mapping_preflights_grant_context_extent_directory_and_backing() {
        let directory = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut req = request(4, false);
        req.mapping_grant_bytes -= 1;
        assert!(StagedClapInputMapping::create_parent(req, directory.path()).is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        req.mapping_grant_bytes += 1;
        let parent = StagedClapInputMapping::create_parent(req, directory.path()).unwrap();
        assert!(
            StagedClapInputMapping::open_child(req, parent.path().unwrap(), other.path()).is_err()
        );
        let wrong = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        wrong.as_file().set_len(parent.shared.len() as u64 - 1).unwrap();
        assert!(StagedClapInputMapping::open_child(req, wrong.path(), directory.path()).is_err());
        wrong.as_file().set_len(parent.shared.len() as u64).unwrap();
        assert!(StagedClapInputMapping::open_child(req, wrong.path(), directory.path()).is_err());
        req.channel_layout = AudioChannelLayout::Mono;
        assert!(geometry(req).is_err());
    }

    #[test]
    fn mapping_control_is_bounded_correlated_and_uses_its_named_decoder() {
        use super::super::{
            native_worker_command::{self, NativeCommand},
            protocol::{CommandKind, WorkerCommand},
        };
        let req = request(4, false);
        let parent = role_reservation(req).unwrap();
        let mut bytes = Vec::new();
        wire::write_request(&mut bytes, req, "original/new", parent).unwrap();
        let decoded = wire::read_request(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.request, req);
        assert_eq!(decoded.parent_bytes, parent);
        assert_eq!(decoded.path, "original/new");
        let mut command = Vec::new();
        WorkerCommand {
            transport_activity: None,
            kind: CommandKind::StageClapAudioInputMapping,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut command)
        .unwrap();
        command.extend_from_slice(&bytes);
        let NativeCommand::StageClapAudioInputMapping(owned) =
            native_worker_command::decode(&mut command.as_slice(), 0).unwrap()
        else {
            panic!("mapping payload must never enter a Plain command");
        };
        assert_eq!(owned.request, req);
        for end in 0..bytes.len() {
            assert!(wire::read_request(&mut &bytes[..end]).is_err());
        }
        for extent in [0u32, u32::MAX] {
            let mut invalid = bytes.clone();
            invalid[12..16].copy_from_slice(&extent.to_le_bytes());
            assert!(wire::read_request(&mut invalid.as_slice()).is_err());
        }
        assert!(wire::write_request(&mut Vec::new(), req, &"x".repeat(4097), parent).is_err());
        assert!(wire::write_request(&mut Vec::new(), req, "a\0b", parent).is_err());
        let receipt = ClapAudioInputMappingReceipt {
            request: req,
            shared_bytes: geometry(req).unwrap().total_bytes,
            parent_owner_bytes: parent,
            child_owner_bytes: parent,
        };
        bytes.clear();
        wire::write_reply(&mut bytes, ClapAudioInputMappingOutcome::Staged(receipt)).unwrap();
        assert_eq!(
            wire::read_reply(&mut bytes.as_slice(), req, parent).unwrap(),
            ClapAudioInputMappingOutcome::Staged(receipt)
        );
        let mut stale = req;
        stale.parameter_lane_count = 1;
        assert!(wire::read_reply(&mut bytes.as_slice(), stale, parent).is_err());
        assert!(wire::read_reply(&mut bytes.as_slice(), req, parent - 1).is_err());
        let mut bad = receipt;
        bad.child_owner_bytes += 1;
        assert!(
            wire::write_reply(&mut Vec::new(), ClapAudioInputMappingOutcome::Staged(bad)).is_err()
        );
    }
}
