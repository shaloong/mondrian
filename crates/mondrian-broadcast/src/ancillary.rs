use mondrian_core::{DisplayTimecodeError, SmpteCountingMode, SmpteTimecodeReference};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COMPONENT_ADF: [u16; 3] = [0x000, 0x3ff, 0x3ff];
const MAX_USER_DATA_WORDS: usize = 255;
const MAX_PACKETS_PER_FRAME: usize = 64;
const MAX_WORDS_PER_FRAME: usize = 16_384;

/// Physical ancillary-data region selected by a delivery Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AncillarySpace {
    /// Vertical ancillary space.
    Vanc,
    /// Horizontal ancillary space.
    Hanc,
}

/// Picture field or progressive-frame association.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AncillaryField {
    /// One progressive frame.
    Progressive,
    /// First interlaced field.
    Field1,
    /// Second interlaced field.
    Field2,
}

/// Exact packet placement within one video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AncillaryPlacement {
    /// VANC or HANC.
    pub space: AncillarySpace,
    /// Progressive frame or interlaced field.
    pub field: AncillaryField,
    /// One-based video line number.
    pub line: u16,
    /// Zero-based ten-bit word offset in the selected ancillary interval.
    pub horizontal_offset: u16,
}

impl AncillaryPlacement {
    /// Construct a non-zero line placement.
    pub fn new(
        space: AncillarySpace,
        field: AncillaryField,
        line: u16,
        horizontal_offset: u16,
    ) -> Result<Self, AncillaryPacketError> {
        if line == 0 {
            return Err(AncillaryPacketError::InvalidLine);
        }
        Ok(Self { space, field, line, horizontal_offset })
    }
}

/// Provenance and ownership of one ancillary packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AncillaryOrigin {
    /// Preserved without semantic rewriting from a qualified source.
    Preserved,
    /// Explicitly authored data.
    Authored,
    /// Derived from canonical Timeline or Program Output state.
    Derived,
}

/// Depth of semantic validation proved for one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AncillaryValidationLevel {
    /// ST 291 packet structure, parity, and checksum only.
    Packet,
    /// The registered payload transport was also structurally validated.
    Transport,
    /// The semantic payload was generated from typed author state.
    Semantic,
}

/// One SMPTE ST 291-1 Type 2 packet, excluding physical placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct St291Type2Packet {
    did: u8,
    sdid: u8,
    user_data_words: Vec<u16>,
}

impl St291Type2Packet {
    /// Construct a Type 2 packet from already formed ten-bit UDW values.
    pub fn new(did: u8, sdid: u8, user_data_words: Vec<u16>) -> Result<Self, AncillaryPacketError> {
        if did & 0x80 != 0 {
            return Err(AncillaryPacketError::Type2Did { did });
        }
        if sdid == 0 {
            return Err(AncillaryPacketError::ReservedSdid);
        }
        if user_data_words.len() > MAX_USER_DATA_WORDS {
            return Err(AncillaryPacketError::PayloadTooLong { actual: user_data_words.len() });
        }
        for (index, word) in user_data_words.iter().copied().enumerate() {
            validate_ten_bit_payload_word(word, index)?;
        }
        Ok(Self { did, sdid, user_data_words })
    }

    /// Construct a packet whose application payload is eight-bit data with
    /// SMPTE even parity and inverse parity added to every UDW.
    pub fn from_8bit_payload(
        did: u8,
        sdid: u8,
        payload: &[u8],
    ) -> Result<Self, AncillaryPacketError> {
        Self::new(
            did,
            sdid,
            payload.iter().copied().map(parity_word).collect(),
        )
    }

    /// Primary registered data identifier.
    pub const fn did(&self) -> u8 {
        self.did
    }

    /// Secondary registered data identifier.
    pub const fn sdid(&self) -> u8 {
        self.sdid
    }

    /// Exact application-owned ten-bit UDW values.
    pub fn user_data_words(&self) -> &[u16] {
        &self.user_data_words
    }

    /// Recover an eight-bit application payload, proving UDW parity first.
    pub fn payload_bytes(&self) -> Result<Vec<u8>, AncillaryPacketError> {
        self.user_data_words
            .iter()
            .copied()
            .enumerate()
            .map(|(index, word)| decode_parity_word(word, index))
            .collect()
    }

    /// Encode the complete component-interface packet including its three-word
    /// ADF and ST 291 checksum.
    pub fn component_words(&self) -> Vec<u16> {
        let mut words = Vec::with_capacity(self.encoded_word_count());
        words.extend_from_slice(&COMPONENT_ADF);
        words.push(parity_word(self.did));
        words.push(parity_word(self.sdid));
        words.push(parity_word(self.user_data_words.len() as u8));
        words.extend_from_slice(&self.user_data_words);
        words.push(checksum_word(&words[3..]));
        words
    }

    /// Decode and validate one complete component-interface packet.
    pub fn decode_component_words(words: &[u16]) -> Result<Self, AncillaryPacketError> {
        if words.len() < 7 {
            return Err(AncillaryPacketError::TruncatedPacket);
        }
        if words[..3] != COMPONENT_ADF {
            return Err(AncillaryPacketError::InvalidAdf);
        }
        let did = decode_parity_word(words[3], 0)?;
        let sdid = decode_parity_word(words[4], 1)?;
        let count = usize::from(decode_parity_word(words[5], 2)?);
        let expected = 7usize.checked_add(count).ok_or(AncillaryPacketError::ExtentOverflow)?;
        if words.len() != expected {
            return Err(AncillaryPacketError::DataCount { expected, actual: words.len() });
        }
        let payload = &words[6..6 + count];
        for (index, word) in payload.iter().copied().enumerate() {
            validate_ten_bit_payload_word(word, index)?;
        }
        let expected_checksum = checksum_word(&words[3..6 + count]);
        let actual_checksum = words[6 + count];
        if actual_checksum != expected_checksum {
            return Err(AncillaryPacketError::Checksum {
                expected: expected_checksum,
                actual: actual_checksum,
            });
        }
        Self::new(did, sdid, payload.to_vec())
    }

    /// Complete word count including ADF and checksum.
    pub fn encoded_word_count(&self) -> usize {
        7 + self.user_data_words.len()
    }

    /// Deterministic digest of the canonical component words.
    pub fn sha256(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for word in self.component_words() {
            hasher.update(word.to_be_bytes());
        }
        hasher.finalize().into()
    }
}

/// One canonical packet with placement and evidence ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AncillaryPacket {
    /// Exact physical placement requested from an output Adapter.
    pub placement: AncillaryPlacement,
    /// ST 291 packet syntax and payload.
    pub packet: St291Type2Packet,
    /// Packet provenance.
    pub origin: AncillaryOrigin,
    /// Highest validation level proved by the producer.
    pub validation: AncillaryValidationLevel,
}

/// Bounded canonical packet inventory associated with one absolute frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AncillaryFrame {
    frame_index: u64,
    packets: Vec<AncillaryPacket>,
    sha256: [u8; 32],
}

impl AncillaryFrame {
    /// Validate, collision-check, sort, and fingerprint one frame inventory.
    pub fn new(
        frame_index: u64,
        mut packets: Vec<AncillaryPacket>,
    ) -> Result<Self, AncillaryPacketError> {
        if packets.len() > MAX_PACKETS_PER_FRAME {
            return Err(AncillaryPacketError::TooManyPackets { actual: packets.len() });
        }
        packets.sort_by_key(|packet| {
            (
                packet.placement,
                packet.packet.did(),
                packet.packet.sdid(),
                packet.packet.sha256(),
            )
        });
        let total_words = packets.iter().try_fold(0usize, |total, packet| {
            total
                .checked_add(packet.packet.encoded_word_count())
                .ok_or(AncillaryPacketError::ExtentOverflow)
        })?;
        if total_words > MAX_WORDS_PER_FRAME {
            return Err(AncillaryPacketError::FrameCapacity { actual: total_words });
        }
        for pair in packets.windows(2) {
            if placements_overlap(&pair[0], &pair[1])? {
                return Err(AncillaryPacketError::PlacementCollision);
            }
        }
        let sha256 = ancillary_frame_digest(frame_index, &packets);
        Ok(Self { frame_index, packets, sha256 })
    }

    /// Construct an empty inventory for a frame with no authored ANC.
    pub fn empty(frame_index: u64) -> Self {
        Self {
            frame_index,
            packets: Vec::new(),
            sha256: ancillary_frame_digest(frame_index, &[]),
        }
    }

    /// Absolute, non-wrapping Program Output frame coordinate.
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Deterministically ordered packet inventory.
    pub fn packets(&self) -> &[AncillaryPacket] {
        &self.packets
    }

    /// Digest over frame identity, placement, packet bytes, and ownership.
    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }
}

/// ATC payload type carried in distributed binary bits 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtcPayloadType {
    /// ST 12-1 linear time-code codeword.
    Ltc,
    /// First vertical-interval time-code codeword.
    Vitc1,
    /// Second vertical-interval time-code codeword.
    Vitc2,
}

/// Typed SMPTE ST 12-2 ancillary time-code payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcTimecode {
    /// ATC payload family.
    pub payload_type: AtcPayloadType,
    /// Conventional wrapped time-address label.
    pub label: String,
    /// Exact 16 ten-bit UDW values.
    pub user_data_words: [u16; 16],
}

impl AtcTimecode {
    /// Derive a progressive ATC_LTC codeword from the canonical SMPTE reference.
    /// User binary groups are zero and locally generated validity is asserted.
    pub fn from_reference(
        reference: SmpteTimecodeReference,
        media_frame: i64,
    ) -> Result<Self, AncillaryPacketError> {
        let timecode = reference.timecode_at_media_frame(media_frame)?;
        if timecode.negative {
            return Err(AncillaryPacketError::NegativeAtcTimecode);
        }
        let drop_frame = reference.mode() == SmpteCountingMode::DropFrame;
        let nibbles = [
            timecode.frames % 10,
            0,
            (timecode.frames / 10) | if drop_frame { 0b0100 } else { 0 },
            0,
            timecode.seconds % 10,
            0,
            timecode.seconds / 10,
            0,
            timecode.minutes % 10,
            0,
            timecode.minutes / 10,
            0,
            timecode.hours % 10,
            0,
            timecode.hours / 10,
            0,
        ];
        let user_data_words = nibbles.map(|nibble| parity_word(nibble << 4));
        Ok(Self {
            payload_type: AtcPayloadType::Ltc,
            label: timecode.label(),
            user_data_words,
        })
    }

    /// Lower the typed codeword into its registered ST 291 packet.
    pub fn packet(&self) -> Result<St291Type2Packet, AncillaryPacketError> {
        St291Type2Packet::new(0x60, 0x60, self.user_data_words.to_vec())
    }
}

/// Optional coded-frame bar geometry carried with AFD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarData {
    /// Top bar is described by `first`.
    pub top: bool,
    /// Bottom bar is described by `second`.
    pub bottom: bool,
    /// Left bar is described by `first`.
    pub left: bool,
    /// Right bar is described by `second`.
    pub right: bool,
    /// First 16-bit bar coordinate.
    pub first: u16,
    /// Second 16-bit bar coordinate.
    pub second: u16,
}

/// Typed SMPTE ST 2016-3 AFD/bar payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveFormatDescription {
    payload: [u8; 8],
}

impl ActiveFormatDescription {
    /// Construct AFD/bar data from final post-geometry author intent.
    pub fn new(
        afd_code: u8,
        aspect_ratio_16_by_9: bool,
        bars: Option<BarData>,
    ) -> Result<Self, AncillaryPacketError> {
        if afd_code > 0x0f {
            return Err(AncillaryPacketError::InvalidAfdCode { code: afd_code });
        }
        let mut payload = [0u8; 8];
        payload[0] = (afd_code << 3) | (u8::from(aspect_ratio_16_by_9) << 2);
        if let Some(bars) = bars {
            let horizontal = bars.top || bars.bottom;
            let vertical = bars.left || bars.right;
            if horizontal == vertical {
                return Err(AncillaryPacketError::InvalidBarFlags);
            }
            payload[3] = (u8::from(bars.top) << 7)
                | (u8::from(bars.bottom) << 6)
                | (u8::from(bars.left) << 5)
                | (u8::from(bars.right) << 4);
            payload[4..6].copy_from_slice(&bars.first.to_be_bytes());
            payload[6..8].copy_from_slice(&bars.second.to_be_bytes());
        }
        Ok(Self { payload })
    }

    /// Exact eight-byte application payload.
    pub const fn payload(&self) -> &[u8; 8] {
        &self.payload
    }

    /// Lower into the registered ST 2016-3 DID/SDID packet.
    pub fn packet(&self) -> Result<St291Type2Packet, AncillaryPacketError> {
        St291Type2Packet::from_8bit_payload(0x41, 0x05, &self.payload)
    }
}

/// Transport-validated SMPTE ST 334-2 Caption Distribution Packet.
///
/// This type intentionally does not claim CEA-608/708 semantic validation or
/// text authoring. Those require a qualified caption engine and golden vectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionDistributionPacket {
    bytes: Vec<u8>,
    frame_rate_code: u8,
}

impl CaptionDistributionPacket {
    /// Validate identifier, declared length, and modulo-256 checksum.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, AncillaryPacketError> {
        if bytes.len() < 7 || bytes.len() > MAX_USER_DATA_WORDS {
            return Err(AncillaryPacketError::InvalidCaptionLength { actual: bytes.len() });
        }
        if bytes[..2] != [0x96, 0x69] {
            return Err(AncillaryPacketError::InvalidCaptionIdentifier);
        }
        if usize::from(bytes[2]) != bytes.len() {
            return Err(AncillaryPacketError::InvalidCaptionLength { actual: bytes.len() });
        }
        if bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0 {
            return Err(AncillaryPacketError::InvalidCaptionChecksum);
        }
        let frame_rate_code = bytes[3] >> 4;
        if frame_rate_code == 0 {
            return Err(AncillaryPacketError::InvalidCaptionFrameRateCode);
        }
        Ok(Self { bytes, frame_rate_code })
    }

    /// Exact validated CDP bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// ST 334-2 frame-rate code retained for delivery-profile matching.
    pub const fn frame_rate_code(&self) -> u8 {
        self.frame_rate_code
    }

    /// Lower into the ST 334-1 caption VANC DID/SDID packet.
    pub fn packet(&self) -> Result<St291Type2Packet, AncillaryPacketError> {
        St291Type2Packet::from_8bit_payload(0x61, 0x01, &self.bytes)
    }
}

fn parity_word(value: u8) -> u16 {
    let parity = u16::from(!value.count_ones().is_multiple_of(2));
    u16::from(value) | (parity << 8) | ((parity ^ 1) << 9)
}

fn decode_parity_word(word: u16, index: usize) -> Result<u8, AncillaryPacketError> {
    if word > 0x3ff {
        return Err(AncillaryPacketError::NotTenBit { index, word });
    }
    let value = word as u8;
    let expected = parity_word(value);
    if word != expected {
        return Err(AncillaryPacketError::Parity { index, word });
    }
    Ok(value)
}

fn checksum_word(words: &[u16]) -> u16 {
    let value = words.iter().fold(0u16, |sum, word| sum.wrapping_add(*word)) & 0x01ff;
    let inverse = ((value >> 8) & 1) ^ 1;
    value | (inverse << 9)
}

fn validate_ten_bit_payload_word(word: u16, index: usize) -> Result<(), AncillaryPacketError> {
    if word > 0x3ff {
        return Err(AncillaryPacketError::NotTenBit { index, word });
    }
    if word <= 0x003 || word >= 0x3fc {
        return Err(AncillaryPacketError::ProtectedCode { index, word });
    }
    Ok(())
}

fn placements_overlap(
    left: &AncillaryPacket,
    right: &AncillaryPacket,
) -> Result<bool, AncillaryPacketError> {
    if (
        left.placement.space,
        left.placement.field,
        left.placement.line,
    ) != (
        right.placement.space,
        right.placement.field,
        right.placement.line,
    ) {
        return Ok(false);
    }
    let left_start = usize::from(left.placement.horizontal_offset);
    let right_start = usize::from(right.placement.horizontal_offset);
    let left_end = left_start
        .checked_add(left.packet.encoded_word_count())
        .ok_or(AncillaryPacketError::ExtentOverflow)?;
    let right_end = right_start
        .checked_add(right.packet.encoded_word_count())
        .ok_or(AncillaryPacketError::ExtentOverflow)?;
    Ok(left_start < right_end && right_start < left_end)
}

fn ancillary_frame_digest(frame_index: u64, packets: &[AncillaryPacket]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(frame_index.to_be_bytes());
    for packet in packets {
        hasher.update([packet.placement.space as u8, packet.placement.field as u8]);
        hasher.update(packet.placement.line.to_be_bytes());
        hasher.update(packet.placement.horizontal_offset.to_be_bytes());
        hasher.update([packet.origin as u8, packet.validation as u8]);
        hasher.update(packet.packet.sha256());
    }
    hasher.finalize().into()
}

/// Stable ancillary syntax, placement, and payload failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AncillaryPacketError {
    /// Physical line zero is not meaningful.
    #[error("ancillary placement line must be non-zero")]
    InvalidLine,
    /// A Type 2 DID must have its most significant bit clear.
    #[error("DID 0x{did:02x} is not a Type 2 ancillary identifier")]
    Type2Did { did: u8 },
    /// SDID zero is reserved.
    #[error("SDID zero is reserved")]
    ReservedSdid,
    /// One packet exceeded the ST 291 UDW count.
    #[error("ancillary packet has {actual} UDW values; maximum is 255")]
    PayloadTooLong { actual: usize },
    /// One word was outside its ten-bit carrier.
    #[error("ancillary word {index} value 0x{word:x} is not ten-bit")]
    NotTenBit { index: usize, word: u16 },
    /// One UDW used an ST 291 protected code.
    #[error("ancillary UDW {index} uses protected code 0x{word:03x}")]
    ProtectedCode { index: usize, word: u16 },
    /// Packet bytes did not contain a complete minimum packet.
    #[error("ancillary packet is truncated")]
    TruncatedPacket,
    /// Component ADF was not the canonical three-word marker.
    #[error("ancillary component ADF is invalid")]
    InvalidAdf,
    /// A parity-protected word was corrupt.
    #[error("ancillary word {index} has invalid parity: 0x{word:03x}")]
    Parity { index: usize, word: u16 },
    /// DC and actual complete packet size diverged.
    #[error("ancillary packet length is {actual} words; expected {expected}")]
    DataCount { expected: usize, actual: usize },
    /// Checksum did not match the ST 291 nine-bit sum.
    #[error("ancillary checksum 0x{actual:03x} does not match 0x{expected:03x}")]
    Checksum { expected: u16, actual: u16 },
    /// Checked extent arithmetic overflowed.
    #[error("ancillary extent arithmetic overflow")]
    ExtentOverflow,
    /// One frame exceeded its bounded packet count.
    #[error("ancillary frame has {actual} packets; maximum is 64")]
    TooManyPackets { actual: usize },
    /// One frame exceeded the conservative canonical word budget.
    #[error("ancillary frame requires {actual} words; maximum is 16384")]
    FrameCapacity { actual: usize },
    /// Two packets occupied overlapping words.
    #[error("ancillary packet placements collide")]
    PlacementCollision,
    /// Negative display time addresses cannot enter the unsigned ATC address.
    #[error("negative timecode cannot be encoded as ATC")]
    NegativeAtcTimecode,
    /// Core SMPTE timecode derivation failed.
    #[error(transparent)]
    Timecode(#[from] DisplayTimecodeError),
    /// AFD code exceeded its four-bit field.
    #[error("AFD code {code} exceeds four bits")]
    InvalidAfdCode { code: u8 },
    /// Bar flags selected neither or both horizontal and vertical geometry.
    #[error("AFD bar flags must select exactly one horizontal or vertical axis")]
    InvalidBarFlags,
    /// CDP size was truncated, excessive, or contradicted its declared size.
    #[error("caption distribution packet has invalid length {actual}")]
    InvalidCaptionLength { actual: usize },
    /// CDP identifier was not 0x9669.
    #[error("caption distribution packet identifier is invalid")]
    InvalidCaptionIdentifier,
    /// CDP checksum did not sum to zero modulo 256.
    #[error("caption distribution packet checksum is invalid")]
    InvalidCaptionChecksum,
    /// CDP omitted a registered frame-rate code.
    #[error("caption distribution packet frame-rate code is invalid")]
    InvalidCaptionFrameRateCode,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::Rational;

    #[test]
    fn st291_round_trip_rejects_every_structural_corruption() {
        let packet = St291Type2Packet::from_8bit_payload(0x41, 0x05, &[0x2c, 0, 0, 0, 0, 0, 0, 0])
            .expect("packet");
        let words = packet.component_words();
        assert_eq!(St291Type2Packet::decode_component_words(&words), Ok(packet));

        for index in 3..words.len() {
            let mut corrupt = words.clone();
            corrupt[index] ^= 1;
            assert!(
                St291Type2Packet::decode_component_words(&corrupt).is_err(),
                "index={index}"
            );
        }
    }

    #[test]
    fn st291_empty_and_max_payloads_are_bounded() {
        let empty = St291Type2Packet::from_8bit_payload(0x60, 0x60, &[]).expect("empty");
        assert_eq!(empty.component_words().len(), 7);
        let max = St291Type2Packet::from_8bit_payload(0x60, 0x60, &[0x55; 255]).expect("maximum");
        assert_eq!(max.component_words().len(), 262);
        assert!(matches!(
            St291Type2Packet::from_8bit_payload(0x60, 0x60, &[0x55; 256]),
            Err(AncillaryPacketError::PayloadTooLong { .. })
        ));
    }

    #[test]
    fn atc_uses_core_drop_frame_math_and_registered_identity() {
        let reference =
            SmpteTimecodeReference::new(Rational::FPS_2997, SmpteCountingMode::DropFrame, 0)
                .expect("reference");
        let atc = AtcTimecode::from_reference(reference, 1_800).expect("ATC");
        assert_eq!(atc.label, "00:01:00;02");
        let packet = atc.packet().expect("packet");
        assert_eq!((packet.did(), packet.sdid()), (0x60, 0x60));
        let payload = packet.payload_bytes().expect("parity");
        assert_eq!(payload[0] >> 4, 2);
        assert_eq!(payload[2] >> 4, 0b0100);
        assert_eq!(payload[8] >> 4, 1);
    }

    #[test]
    fn afd_payload_matches_registered_layout() {
        let afd = ActiveFormatDescription::new(
            8,
            true,
            Some(BarData {
                top: true,
                bottom: true,
                left: false,
                right: false,
                first: 42,
                second: 1037,
            }),
        )
        .expect("AFD");
        assert_eq!(afd.payload(), &[0x44, 0, 0, 0xc0, 0, 42, 4, 13]);
        assert_eq!((afd.packet().expect("packet").did(), 0x05), (0x41, 0x05));
    }

    #[test]
    fn frame_inventory_is_canonical_and_collision_safe() {
        let placement =
            AncillaryPlacement::new(AncillarySpace::Vanc, AncillaryField::Progressive, 9, 0)
                .expect("placement");
        let packet = AncillaryPacket {
            placement,
            packet: AtcTimecode::from_reference(
                SmpteTimecodeReference::new(Rational::FPS_25, SmpteCountingMode::NonDropFrame, 0)
                    .expect("reference"),
                0,
            )
            .expect("ATC")
            .packet()
            .expect("packet"),
            origin: AncillaryOrigin::Derived,
            validation: AncillaryValidationLevel::Semantic,
        };
        assert!(matches!(
            AncillaryFrame::new(0, vec![packet.clone(), packet]),
            Err(AncillaryPacketError::PlacementCollision)
        ));
    }

    #[test]
    fn cdp_transport_rejects_bad_length_and_checksum() {
        let mut cdp = vec![0x96, 0x69, 7, 0x40, 0, 0, 0];
        let checksum = 0u8.wrapping_sub(cdp.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
        cdp[6] = checksum;
        assert!(CaptionDistributionPacket::from_bytes(cdp.clone()).is_ok());
        cdp[2] = 8;
        assert!(matches!(
            CaptionDistributionPacket::from_bytes(cdp),
            Err(AncillaryPacketError::InvalidCaptionLength { .. })
        ));
    }
}
