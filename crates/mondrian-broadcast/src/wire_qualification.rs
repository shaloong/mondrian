//! Explicit validation-only correlation of canonical ANC with physical capture.
use crate::{
    AncillaryField, AncillaryFrame, AncillaryOrigin, AncillaryPacket, AncillaryPacketError,
    AncillaryPlacement, AncillarySpace, AncillaryValidationLevel, St291Type2Packet,
};
use serde::{Deserialize, Serialize};

/// An actual ST291 component packet decoded from an independent capture raster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedAncillaryPacket {
    /// Actual placement observed in the received signal.
    pub placement: AncillaryPlacement,
    /// Complete received ten-bit words, including ADF and checksum.
    pub component_words: Vec<u16>,
}

/// Explicit qualification-only packet owner; never inserted by normal playback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AncillaryWireCorrelation {
    nonce: [u8; 16],
    placement: AncillaryPlacement,
}

impl AncillaryWireCorrelation {
    /// Bind a nonzero campaign nonce and an exact progressive luma VANC location.
    pub fn new(nonce: [u8; 16], placement: AncillaryPlacement) -> Result<Self, AncillaryWireError> {
        if nonce == [0; 16]
            || placement.space != AncillarySpace::Vanc
            || placement.field != AncillaryField::Progressive
            || placement.line == 0
        {
            return Err(AncillaryWireError::InvalidCorrelation);
        }
        Ok(Self { nonce, placement })
    }

    /// Campaign identity embedded in every real output marker packet.
    pub const fn nonce(&self) -> &[u8; 16] {
        &self.nonce
    }

    /// Explicit canonical placement used by the marker owner.
    pub const fn placement(&self) -> AncillaryPlacement {
        self.placement
    }

    /// Append the real on-wire correlation packet to the canonical frame.
    pub fn frame(
        &self,
        frame_index: u64,
        mut packets: Vec<AncillaryPacket>,
    ) -> Result<AncillaryFrame, AncillaryWireError> {
        Self::new(self.nonce, self.placement)?;
        if packets
            .iter()
            .any(|packet| packet.packet.did() == 0x5f && packet.packet.sdid() == 0x7f)
        {
            return Err(AncillaryWireError::AmbiguousMarker);
        }
        let mut payload = Vec::from(&b"MDANC001"[..]);
        payload.extend_from_slice(&self.nonce);
        payload.extend_from_slice(&frame_index.to_be_bytes());
        packets.push(AncillaryPacket {
            placement: self.placement,
            packet: St291Type2Packet::from_8bit_payload(0x5f, 0x7f, &payload)?,
            origin: AncillaryOrigin::Derived,
            validation: AncillaryValidationLevel::Semantic,
        });
        Ok(AncillaryFrame::new(frame_index, packets)?)
    }

    /// Obtain frame identity from a marker actually decoded from received words.
    pub fn captured_frame_index(
        &self,
        packets: &[CapturedAncillaryPacket],
    ) -> Result<u64, AncillaryWireError> {
        Self::new(self.nonce, self.placement)?;
        if packets.len() > 64 {
            return Err(AncillaryWireError::InventoryMismatch);
        }
        let mut marker = None;
        for packet in packets {
            let decoded = St291Type2Packet::decode_component_words(&packet.component_words)?;
            if decoded.did() != 0x5f || decoded.sdid() != 0x7f {
                continue;
            }
            if marker.is_some() || packet.placement != self.placement {
                return Err(AncillaryWireError::AmbiguousMarker);
            }
            let payload = decoded.payload_bytes()?;
            if payload.len() != 32 || &payload[..8] != b"MDANC001" || payload[8..24] != self.nonce {
                return Err(AncillaryWireError::InvalidCorrelation);
            }
            let bytes: [u8; 8] =
                payload[24..].try_into().map_err(|_| AncillaryWireError::InvalidCorrelation)?;
            marker = Some(u64::from_be_bytes(bytes));
        }
        marker.ok_or(AncillaryWireError::MissingMarker)
    }

    /// Reconstruct and verify a frame from actual input words. Provenance is
    /// joined from its canonical owner only after exact placement/word equality.
    pub fn verify_capture(
        &self,
        expected: &AncillaryFrame,
        captured: &[CapturedAncillaryPacket],
    ) -> Result<AncillaryFrame, AncillaryWireError> {
        if self.captured_frame_index(captured)? != expected.frame_index()
            || captured.len() != expected.packets().len()
        {
            return Err(AncillaryWireError::InventoryMismatch);
        }
        let mut reconstructed = Vec::with_capacity(captured.len());
        for packet in captured {
            let decoded = St291Type2Packet::decode_component_words(&packet.component_words)?;
            let matching: Vec<_> = expected
                .packets()
                .iter()
                .filter(|expected| {
                    expected.placement == packet.placement && expected.packet == decoded
                })
                .collect();
            if matching.len() != 1 {
                return Err(AncillaryWireError::InventoryMismatch);
            }
            reconstructed.push(AncillaryPacket {
                placement: packet.placement,
                packet: decoded,
                origin: matching[0].origin,
                validation: matching[0].validation,
            });
        }
        let actual = AncillaryFrame::new(expected.frame_index(), reconstructed)?;
        if actual.sha256() != expected.sha256() {
            return Err(AncillaryWireError::InventoryMismatch);
        }
        Ok(actual)
    }
}

/// Fail-closed wire association or independently decoded packet mismatch.
#[derive(Debug, thiserror::Error)]
pub enum AncillaryWireError {
    /// Canonical ST291 syntax was invalid.
    #[error(transparent)]
    Packet(#[from] AncillaryPacketError),
    /// Campaign marker nonce, placement or payload was invalid.
    #[error("invalid ancillary wire correlation binding")]
    InvalidCorrelation,
    /// More than one marker or a conflicting marker placement was present.
    #[error("ambiguous ancillary wire correlation marker")]
    AmbiguousMarker,
    /// No actual received marker existed.
    #[error("independent capture omitted the frame correlation marker")]
    MissingMarker,
    /// Independent input differed in frame, packet count, placement or words.
    #[error("independent ancillary inventory differs from canonical output")]
    InventoryMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn correlation_requires_actual_received_frame_nonce_words_and_location() {
        let placement =
            AncillaryPlacement::new(AncillarySpace::Vanc, AncillaryField::Progressive, 20, 113)
                .expect("placement");
        let owner = AncillaryWireCorrelation::new([7; 16], placement).expect("owner");
        let expected = owner.frame(u64::MAX, vec![]).expect("frame");
        let wire: Vec<_> = expected
            .packets()
            .iter()
            .map(|packet| CapturedAncillaryPacket {
                placement: packet.placement,
                component_words: packet.packet.component_words(),
            })
            .collect();
        assert_eq!(
            owner.verify_capture(&expected, &wire).expect("received").sha256(),
            expected.sha256()
        );
        assert!(owner.verify_capture(&expected, &[]).is_err());
        assert!(owner
            .verify_capture(&owner.frame(0, vec![]).expect("other frame"), &wire)
            .is_err());
        assert!(AncillaryWireCorrelation::new([8; 16], placement)
            .expect("other run")
            .verify_capture(&expected, &wire)
            .is_err());
        let mut damaged = wire.clone();
        damaged[0].component_words[8] ^= 1;
        assert!(owner.verify_capture(&expected, &damaged).is_err());
        let mut shifted = wire.clone();
        shifted[0].placement.horizontal_offset += 1;
        assert!(owner.verify_capture(&expected, &shifted).is_err());
        assert!(owner.verify_capture(&expected, &[wire[0].clone(), wire[0].clone()]).is_err());
    }
}
