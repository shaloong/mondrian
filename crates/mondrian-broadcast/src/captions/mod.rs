//! SCC V1.0 and data-only raw ST334-2 CDP imports. Imported data stays
//! Transport: decoder rendering, glyph shaping and accessibility QC are not
//! implied by byte/control/packet validation. See architecture source records.
mod cea608;
mod cea708;
#[cfg(test)]
mod tests;
use crate::{
    AncillaryField, AncillaryFrame, AncillaryOrigin, AncillaryPacket, AncillaryPlacement,
    AncillarySpace, AncillaryValidationLevel, CaptionDistributionPacket, FrozenAncillaryProgram,
};
pub use cea608::Cea608Operation;
use mondrian_core::{
    FramePosition, FrameRounding, Rational, SmpteCountingMode, SmpteDisplayTimecodeContract,
    TimelineTime,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRAMES: u64 = 100_000;
/// Original independently identifiable caption-file syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionSourceFormat {
    /// Scenarist 608 Field1 file with literal V1.0 header.
    ScenaristSccV1,
    /// Concatenated data-only ST334-2:2015 CDPs.
    RawCdpSt334_2_2015,
}
/// Original source and validator provenance, not hardware qualification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionImportReceipt {
    /// Strict importer schema/version.
    pub importer_version: u32,
    /// Original format and version.
    pub source_format: CaptionSourceFormat,
    /// Digest of every original file byte.
    pub source_sha256: [u8; 32],
    /// Exact output duration including CDP padding frames.
    pub output_frames: u64,
    /// Bitset of explicitly selected 608 channels CC1..CC4.
    pub cea608_channels: u8,
    /// Valid non-null 608 pairs, including repeated controls.
    pub cea608_pairs: u64,
    /// Complete reassembled DTVCC packets.
    pub cea708_packets: u64,
    /// Standard/extended service numbers with nonempty blocks.
    pub cea708_services: Vec<u8>,
}
/// Exact output owner supplied by Export, independent of file interpretation.
#[derive(Debug, Clone, Copy)]
pub struct CaptionImportBinding {
    /// Selected canonical Timeline start.
    pub source_start: TimelineTime,
    /// Resolved Program Output cadence.
    pub output_frame_rate: Rational,
    /// Exact selected duration; bounded to 100,000 frames per caption import.
    pub frame_count: u64,
    /// Timeline zero's timecode offset expressed exactly as time.
    pub timecode_origin: TimelineTime,
    /// Explicit progressive luma VANC placement; ST436 requires offset zero.
    pub placement: AncillaryPlacement,
}
impl CaptionImportBinding {
    fn validate(self) -> Result<(), CaptionImportError> {
        if self.frame_count == 0
            || self.frame_count > MAX_FRAMES
            || self.source_start < TimelineTime::ZERO
            || self.timecode_origin < TimelineTime::ZERO
            || self.placement.line == 0
            || self.placement.space != AncillarySpace::Vanc
            || self.placement.field != AncillaryField::Progressive
            || self.placement.horizontal_offset != 0
        {
            return Err(CaptionImportError::Binding);
        }
        rate(self.output_frame_rate)?;
        Ok(())
    }
}
/// Import SCC or concatenated raw CDP into the sole canonical program.
/// Off-grid/out-of-selection timing rejects; initialization is never trimmed.
pub fn import_caption_program(
    bytes: &[u8],
    format: CaptionSourceFormat,
    binding: CaptionImportBinding,
) -> Result<FrozenAncillaryProgram, CaptionImportError> {
    binding.validate()?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(CaptionImportError::Extent);
    }
    let cdps = match format {
        CaptionSourceFormat::ScenaristSccV1 => import_scc(bytes, binding)?,
        CaptionSourceFormat::RawCdpSt334_2_2015 => split_cdp(bytes, binding.frame_count)?,
    };
    let mut validator = CdpValidator::default();
    let mut frames = Vec::with_capacity(cdps.len());
    for (index, cdp) in cdps.into_iter().enumerate() {
        validator.push(&cdp, binding.output_frame_rate)?;
        frames.push(AncillaryFrame::new(
            index as u64,
            vec![AncillaryPacket {
                placement: binding.placement,
                packet: CaptionDistributionPacket::from_bytes(cdp)?.packet()?,
                origin: match format {
                    CaptionSourceFormat::ScenaristSccV1 => AncillaryOrigin::Derived,
                    CaptionSourceFormat::RawCdpSt334_2_2015 => AncillaryOrigin::Preserved,
                },
                validation: AncillaryValidationLevel::Transport,
            }],
        )?);
    }
    validator.dtvcc.finish()?;
    let receipt = CaptionImportReceipt {
        importer_version: 1,
        source_format: format,
        source_sha256: Sha256::digest(bytes).into(),
        output_frames: binding.frame_count,
        cea608_channels: validator.legacy.channels,
        cea608_pairs: validator.legacy.pairs,
        cea708_packets: validator.dtvcc.packets,
        cea708_services: validator.dtvcc.services.into_iter().collect(),
    };
    Ok(FrozenAncillaryProgram::new(
        binding.source_start,
        binding.output_frame_rate,
        binding.frame_count,
        frames,
    )?
    .with_caption_source(receipt))
}
pub(crate) fn validate_program_caption_source(
    program: &FrozenAncillaryProgram,
) -> Result<(), CaptionImportError> {
    let Some(source) = program.caption_source() else {
        return Ok(());
    };
    if source.importer_version != 1
        || source.source_sha256 == [0; 32]
        || source.output_frames != program.frame_count()
        || program.frame_count() > MAX_FRAMES
    {
        return Err(CaptionImportError::Binding);
    }
    let mut validator = CdpValidator::default();
    for index in 0..program.frame_count() {
        let frame = program.frame(index)?;
        let [packet] = frame.packets() else {
            return Err(CaptionImportError::Binding);
        };
        let origin = match source.source_format {
            CaptionSourceFormat::ScenaristSccV1 => AncillaryOrigin::Derived,
            CaptionSourceFormat::RawCdpSt334_2_2015 => AncillaryOrigin::Preserved,
        };
        if packet.validation != AncillaryValidationLevel::Transport
            || packet.origin != origin
            || packet.packet.did() != 0x61
            || packet.packet.sdid() != 1
        {
            return Err(CaptionImportError::Binding);
        }
        validator.push(&packet.packet.payload_bytes()?, program.output_frame_rate())?;
    }
    validator.dtvcc.finish()?;
    if source.cea608_channels != validator.legacy.channels
        || source.cea608_pairs != validator.legacy.pairs
        || source.cea708_packets != validator.dtvcc.packets
        || source.cea708_services != validator.dtvcc.services.into_iter().collect::<Vec<_>>()
    {
        return Err(CaptionImportError::Binding);
    }
    Ok(())
}
fn rate(value: Rational) -> Result<(u8, usize, usize), CaptionImportError> {
    [
        (Rational::new(25, 1), 3, 24, 2),
        (Rational::new(30000, 1001), 4, 20, 2),
        (Rational::new(30, 1), 5, 20, 2),
        (Rational::new(50, 1), 6, 12, 1),
        (Rational::new(60000, 1001), 7, 10, 1),
        (Rational::new(60, 1), 8, 10, 1),
    ]
    .into_iter()
    .find(|(r, _, _, _)| *r == value)
    .map(|(_, code, count, legacy)| (code, count, legacy))
    .ok_or(CaptionImportError::Unsupported(
        "CDP cadence outside supported 25/29.97/30/50/59.94/60 subset",
    ))
}
#[derive(Default)]
struct CdpValidator {
    last_sequence: Option<u16>,
    next_field: Option<u8>,
    legacy: cea608::Decoder,
    dtvcc: cea708::Decoder,
}
impl CdpValidator {
    fn push(&mut self, bytes: &[u8], frame_rate: Rational) -> Result<(), CaptionImportError> {
        let (code, count, legacy_count) = rate(frame_rate)?;
        CaptionDistributionPacket::from_bytes(bytes.to_vec())?;
        if bytes.len() != 13 + 3 * count
            || bytes[3] != (code << 4 | 15)
            || !matches!(bytes[4], 0x41 | 0x43)
            || bytes[7] != 0x72
            || bytes[8] != (0xe0 | count as u8)
        {
            return Err(CaptionImportError::Unsupported(
                "only complete data-only CDP sections at the exact declared cadence are supported",
            ));
        }
        let sequence = u16::from_be_bytes([bytes[5], bytes[6]]);
        if self.last_sequence.is_some_and(|last| last.wrapping_add(1) != sequence) {
            return Err(CaptionImportError::Cdp("header sequence discontinuity"));
        }
        self.last_sequence = Some(sequence);
        let footer = 9 + 3 * count;
        if bytes[footer] != 0x74 || bytes[footer + 1..footer + 3] != bytes[5..7] {
            return Err(CaptionImportError::Cdp("footer identity/sequence mismatch"));
        }
        for (index, triplet) in bytes[9..footer].chunks_exact(3).enumerate() {
            if triplet[0] & 0xf8 != 0xf8 {
                return Err(CaptionImportError::Cdp("cc_data marker bits"));
            }
            let kind = triplet[0] & 3;
            let valid = triplet[0] & 4 != 0;
            let pair = [triplet[1], triplet[2]];
            if index < legacy_count {
                if kind > 1 || (legacy_count == 2 && usize::from(kind) != index) {
                    return Err(CaptionImportError::Cdp(
                        "608 slots must precede 708 in field order",
                    ));
                }
                if legacy_count == 1 {
                    if self.next_field.is_some_and(|expected| expected != kind) {
                        return Err(CaptionImportError::Cdp(
                            "high-rate 608 field phase discontinuity",
                        ));
                    }
                    self.next_field = Some(kind ^ 1);
                }
                if valid {
                    self.legacy.push(usize::from(kind), pair)?;
                } else if pair != [0, 0] {
                    return Err(CaptionImportError::Cdp("nonzero invalid 608 padding"));
                }
            } else {
                if kind < 2 {
                    return Err(CaptionImportError::Cdp(
                        "legacy pair outside allocated 608 slots",
                    ));
                }
                if valid {
                    self.dtvcc.push(kind, pair)?;
                } else if kind != 2 || pair != [0, 0] {
                    return Err(CaptionImportError::Cdp(
                        "noncanonical invalid DTVCC padding",
                    ));
                }
            }
        }
        Ok(())
    }
}
fn split_cdp(bytes: &[u8], expected: u64) -> Result<Vec<Vec<u8>>, CaptionImportError> {
    let mut result = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let count = usize::from(*bytes.get(at + 2).ok_or(CaptionImportError::Extent)?);
        let end = at
            .checked_add(count)
            .filter(|end| *end > at && *end <= bytes.len())
            .ok_or(CaptionImportError::Extent)?;
        if result.len() as u64 >= expected {
            return Err(CaptionImportError::Binding);
        }
        result.push(bytes[at..end].to_vec());
        at = end;
    }
    if result.len() as u64 != expected {
        return Err(CaptionImportError::Binding);
    }
    Ok(result)
}
fn import_scc(
    bytes: &[u8],
    binding: CaptionImportBinding,
) -> Result<Vec<Vec<u8>>, CaptionImportError> {
    // SCC declares no field/rate. This supported row explicitly binds Field1
    // at 30000/1001 to output 60000/1001: one pair per two output frames.
    if binding.output_frame_rate != Rational::new(60000, 1001) {
        return Err(CaptionImportError::Unsupported(
            "SCC Field1 requires exact 29.97-to-59.94 output row",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| CaptionImportError::Unsupported("SCC must be UTF-8/ASCII"))?;
    let mut lines = text.strip_prefix('\u{feff}').unwrap_or(text).lines();
    if lines.next() != Some("Scenarist_SCC V1.0") {
        return Err(CaptionImportError::Unsupported("SCC header/version"));
    }
    let input_rate = Rational::new(30000, 1001);
    let mut mode = None;
    let mut slots = BTreeMap::new();
    let mut last_input = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (label, words) = line.split_once('\t').ok_or(CaptionImportError::Scc(
            "timecode must be followed by a tab",
        ))?;
        if label.len() != 11 || words.trim().is_empty() {
            return Err(CaptionImportError::Scc("invalid timed row"));
        }
        let current = match label.as_bytes()[8] {
            b';' => SmpteCountingMode::DropFrame,
            b':' => SmpteCountingMode::NonDropFrame,
            _ => return Err(CaptionImportError::Scc("timecode separator")),
        };
        if mode.is_some_and(|previous| previous != current) {
            return Err(CaptionImportError::Scc("mixed drop/non-drop counting"));
        }
        mode = Some(current);
        let clock = SmpteDisplayTimecodeContract::new(input_rate, current, 0)
            .map_err(|_| CaptionImportError::Scc("timecode contract"))?;
        let start = clock
            .parse_label(label)
            .map_err(|_| CaptionImportError::Scc("invalid/skipped drop-frame label"))?;
        for (offset, word) in words.split_ascii_whitespace().enumerate() {
            if word.len() != 4 || !word.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(CaptionImportError::Scc(
                    "expected exactly two hexadecimal bytes",
                ));
            }
            let value =
                u16::from_str_radix(word, 16).map_err(|_| CaptionImportError::Scc("hex word"))?;
            let index = start
                .checked_add(i64::try_from(offset).map_err(|_| CaptionImportError::Extent)?)
                .ok_or(CaptionImportError::Extent)?;
            if index < 0 || last_input.is_some_and(|last| index <= last) {
                return Err(CaptionImportError::Scc("overlapping/reordered timed rows"));
            }
            last_input = Some(index);
            let time = TimelineTime::from_frame_position(FramePosition::new(
                index,
                Rational::new(1001, 30000),
            ))
            .and_then(|time| time.checked_sub(binding.timecode_origin))
            .and_then(|time| time.checked_sub(binding.source_start))
            .map_err(|_| CaptionImportError::Binding)?;
            let position = time
                .to_frame_position(binding.output_frame_rate, FrameRounding::Floor)
                .map_err(|_| CaptionImportError::Binding)?;
            if time < TimelineTime::ZERO
                || TimelineTime::from_frame_position(position)
                    .map_err(|_| CaptionImportError::Binding)?
                    != time
            {
                return Err(CaptionImportError::Binding);
            }
            let output = u64::try_from(position.frame).map_err(|_| CaptionImportError::Binding)?;
            if output >= binding.frame_count
                || output % 2 != 0
                || slots.insert(output, value.to_be_bytes()).is_some()
            {
                return Err(CaptionImportError::Binding);
            }
        }
    }
    if slots.is_empty() {
        return Err(CaptionImportError::Scc("no caption data"));
    }
    let mut result = Vec::with_capacity(binding.frame_count as usize);
    for index in 0..binding.frame_count {
        let pair = slots.remove(&index).unwrap_or([0x80, 0x80]);
        result.push(make_cdp(index as u16, (index & 1) as u8, pair, &[]));
    }
    Ok(result)
}
fn make_cdp(sequence: u16, field: u8, pair: [u8; 2], digital: &[[u8; 3]]) -> Vec<u8> {
    let [hi, lo] = sequence.to_be_bytes();
    let mut bytes = vec![
        0x96,
        0x69,
        43,
        0x7f,
        0x43,
        hi,
        lo,
        0x72,
        0xea,
        0xfc | field,
        pair[0],
        pair[1],
    ];
    for index in 0..9 {
        bytes.extend_from_slice(digital.get(index).unwrap_or(&[0xfa, 0, 0]));
    }
    bytes.extend_from_slice(&[0x74, hi, lo]);
    let sum = bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    bytes.push(0u8.wrapping_sub(sum));
    bytes
}
/// Explicit malformed, unsupported or unrepresentable input.
#[derive(Debug, thiserror::Error)]
pub enum CaptionImportError {
    /// Canonical packet syntax failure.
    #[error(transparent)]
    Packet(#[from] crate::AncillaryPacketError),
    /// ST436/carriage program bound failure.
    #[error(transparent)]
    Carriage(#[from] crate::St436Error),
    /// Incomplete or excessive input.
    #[error("caption input exceeds 8 MiB / 100000 frames or is truncated")]
    Extent,
    /// Origin/duration/frame grid cannot represent input exactly.
    #[error("caption input does not fit the exact selected output frame grid")]
    Binding,
    /// Syntax without an implemented qualified mapping.
    #[error("unsupported caption syntax: {0}")]
    Unsupported(&'static str),
    /// SCC temporal/lexical failure.
    #[error("SCC: {0}")]
    Scc(&'static str),
    /// CDP section/order/continuity failure.
    #[error("CDP: {0}")]
    Cdp(&'static str),
    /// CEA608 parity/channel/control/grid failure.
    #[error("CEA-608: {0}")]
    Cea608(&'static str),
    /// CEA708 packet/service/command failure.
    #[error("CEA-708: {0}")]
    Cea708(&'static str),
}
