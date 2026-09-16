//! SMPTE ST 436 ANC frame wrapping over the sole canonical ST291 packet model.
//!
//! See SMPTE 436M-2006 sections 4.4.4 and 6, and the official BBC BMX
//! ST436Element implementation. Luma 10-bit coding preserves checksum and UDW
//! words. This exact mapping rejects nonzero horizontal offsets: ST436 carries
//! a line and ordered packets, but has no field for an arbitrary word offset.
use crate::{
    AncillaryField, AncillaryFrame, AncillaryOrigin, AncillaryPacket, AncillaryPacketError,
    AncillaryPlacement, AncillarySpace, AncillaryValidationLevel, St291Type2Packet,
};
use std::io::{Read, Write};

const MAX_ELEMENT_BYTES: usize = 65_536;
/// Standard generic-container ANC data essence-element key, track number one.
pub const ST436_ANC_ESSENCE_KEY: [u8; 16] = [
    0x06, 0x0e, 0x2b, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01, 0x17, 0x01, 0x02, 0x01,
];

/// Encode one standard ST436 element using exact 10-bit luma ANC sample coding.
pub fn encode_st436_ancillary(frame: &AncillaryFrame) -> Result<Vec<u8>, St436Error> {
    // Revalidate deserialized canonical objects rather than trusting cached SHA.
    let canonical = AncillaryFrame::new(frame.frame_index(), frame.packets().to_vec())?;
    if canonical.sha256() != frame.sha256() {
        return Err(St436Error::CanonicalMismatch);
    }
    let mut output = Vec::new();
    output.extend_from_slice(&(frame.packets().len() as u16).to_be_bytes());
    for packet in frame.packets() {
        AncillaryPlacement::new(
            packet.placement.space,
            packet.placement.field,
            packet.placement.line,
            packet.placement.horizontal_offset,
        )?;
        if packet.placement.horizontal_offset != 0 {
            return Err(St436Error::UnrepresentablePlacement);
        }
        let words = packet.packet.component_words();
        St291Type2Packet::decode_component_words(&words)?;
        let samples = &words[3..];
        let payload_bytes = samples.len().div_ceil(3) * 4;
        output.extend_from_slice(&packet.placement.line.to_be_bytes());
        let field = match packet.placement.field {
            AncillaryField::Progressive => 4,
            AncillaryField::Field1 => 2,
            AncillaryField::Field2 => 3,
        };
        output.push(
            field
                | if packet.placement.space == AncillarySpace::Hanc {
                    0x10
                } else {
                    0
                },
        );
        output.push(7); // ANC 10-bit component luma, including source checksum
        output.extend_from_slice(&(samples.len() as u16).to_be_bytes());
        output.extend_from_slice(&(payload_bytes as u32).to_be_bytes());
        output.extend_from_slice(&1u32.to_be_bytes());
        for triple in samples.chunks(3) {
            let word = (u32::from(triple[0]) << 22)
                | (u32::from(*triple.get(1).unwrap_or(&0)) << 12)
                | (u32::from(*triple.get(2).unwrap_or(&0)) << 2);
            output.extend_from_slice(&word.to_be_bytes());
        }
    }
    if output.len() > MAX_ELEMENT_BYTES {
        return Err(St436Error::Extent);
    }
    Ok(output)
}

/// Independently decode one complete ST436 element into preserved canonical ANC.
/// Eight-bit luma is accepted with regenerated parity/checksum as ST436 requires;
/// ten-bit luma retains and validates the actual source checksum and UDW words.
pub fn decode_st436_ancillary(
    frame_index: u64,
    bytes: &[u8],
) -> Result<AncillaryFrame, St436Error> {
    if bytes.len() < 2 || bytes.len() > MAX_ELEMENT_BYTES {
        return Err(St436Error::Extent);
    }
    let mut cursor = Cursor { bytes, at: 0 };
    let count = usize::from(cursor.u16()?);
    if count > 64 {
        return Err(St436Error::Extent);
    }
    let mut packets = Vec::with_capacity(count);
    for _ in 0..count {
        let line = cursor.u16()?;
        let wrapping = cursor.u8()?;
        let coding = cursor.u8()?;
        let samples = usize::from(cursor.u16()?);
        let array = cursor.u32()? as usize;
        if cursor.u32()? != 1 || samples > 259 {
            return Err(St436Error::Extent);
        }
        let (space, field) = match wrapping {
            4 => (AncillarySpace::Vanc, AncillaryField::Progressive),
            2 => (AncillarySpace::Vanc, AncillaryField::Field1),
            3 => (AncillarySpace::Vanc, AncillaryField::Field2),
            0x14 => (AncillarySpace::Hanc, AncillaryField::Progressive),
            0x12 => (AncillarySpace::Hanc, AncillaryField::Field1),
            0x13 => (AncillarySpace::Hanc, AncillaryField::Field2),
            _ => return Err(St436Error::UnsupportedCoding),
        };
        let required = match coding {
            7 if samples >= 4 => samples.div_ceil(3) * 4,
            4 if samples >= 3 => samples.div_ceil(4) * 4,
            _ => return Err(St436Error::UnsupportedCoding),
        };
        if array != required {
            return Err(St436Error::Extent);
        }
        let payload = cursor.take(array)?;
        let packet = if coding == 7 {
            let mut words = vec![0, 1023, 1023];
            for chunk in payload.chunks_exact(4) {
                let word = u32::from_be_bytes(chunk.try_into().map_err(|_| St436Error::Extent)?);
                if word & 3 != 0 {
                    return Err(St436Error::Padding);
                }
                for shift in [22, 12, 2] {
                    let sample = ((word >> shift) & 1023) as u16;
                    if words.len() < samples + 3 {
                        words.push(sample);
                    } else if sample != 0 {
                        return Err(St436Error::Padding);
                    }
                }
            }
            St291Type2Packet::decode_component_words(&words)?
        } else {
            if usize::from(payload[2]) + 3 != samples
                || payload[samples..].iter().any(|byte| *byte != 0)
            {
                return Err(St436Error::Extent);
            }
            St291Type2Packet::from_8bit_payload(payload[0], payload[1], &payload[3..samples])?
        };
        packets.push(AncillaryPacket {
            placement: AncillaryPlacement::new(space, field, line, 0)?,
            packet,
            origin: AncillaryOrigin::Preserved,
            validation: AncillaryValidationLevel::Packet,
        });
    }
    if cursor.at != bytes.len() {
        return Err(St436Error::TrailingData);
    }
    Ok(AncillaryFrame::new(frame_index, packets)?)
}

/// Verify real reimported words against a sealed canonical owner, retaining
/// provenance only after data and every representable placement agree.
pub fn reimport_st436_canonical(
    expected: &AncillaryFrame,
    bytes: &[u8],
) -> Result<AncillaryFrame, St436Error> {
    let actual = decode_st436_ancillary(expected.frame_index(), bytes)?;
    if actual.packets().len() != expected.packets().len() {
        return Err(St436Error::CanonicalMismatch);
    }
    let mut packets = Vec::new();
    for (actual, expected) in actual.packets().iter().zip(expected.packets()) {
        if actual.placement != expected.placement || actual.packet != expected.packet {
            return Err(St436Error::CanonicalMismatch);
        }
        packets.push(AncillaryPacket {
            placement: actual.placement,
            packet: actual.packet.clone(),
            origin: expected.origin,
            validation: expected.validation,
        });
    }
    let result = AncillaryFrame::new(expected.frame_index(), packets)?;
    if result.sha256() != expected.sha256() {
        return Err(St436Error::CanonicalMismatch);
    }
    Ok(result)
}

/// Write one standard KLV-wrapped ANC frame for BMX `--klv s --anc` input.
/// Return the exact number of bytes written; no private carriage fields exist.
pub fn write_st436_klv_frame(
    writer: &mut impl Write,
    frame: &AncillaryFrame,
) -> Result<usize, St436Error> {
    let bytes = encode_st436_ancillary(frame)?;
    let length = ber_length(bytes.len());
    writer.write_all(&ST436_ANC_ESSENCE_KEY)?;
    writer.write_all(&length)?;
    writer.write_all(&bytes)?;
    Ok(16 + length.len() + bytes.len())
}

/// Read the next KLV-wrapped ANC frame. A clean end is distinct from any partial
/// key, BER length or payload. The caller owns the exact edit-rate/frame index.
pub fn read_st436_klv_frame(
    reader: &mut impl Read,
    frame_index: u64,
) -> Result<Option<AncillaryFrame>, St436Error> {
    let Some((key, size, _)) = read_klv_header(reader)? else {
        return Ok(None);
    };
    if !is_anc_key(&key) || size > MAX_ELEMENT_BYTES as u64 {
        return Err(St436Error::UnsupportedCoding);
    }
    let mut bytes = vec![0; size as usize];
    reader.read_exact(&mut bytes)?;
    Ok(Some(decode_st436_ancillary(frame_index, &bytes)?))
}

/// Scan an actual MXF KLV stream and verify every ANC essence frame against its
/// ordered canonical inventory. Picture/metadata KLVs are streamed past, never
/// interpreted as ANC. Extra tracks, extra/missing frames and truncation reject.
pub fn verify_st436_mxf_ancillary(
    reader: &mut impl Read,
    expected: &[AncillaryFrame],
    maximum_bytes: u64,
) -> Result<u64, St436Error> {
    verify_mxf_frames(
        reader,
        expected.len() as u64,
        |index| expected.get(index as usize).cloned().ok_or(St436Error::CanonicalMismatch),
        maximum_bytes,
    )
}
pub(crate) fn verify_mxf_frames(
    reader: &mut impl Read,
    expected_count: u64,
    mut expected: impl FnMut(u64) -> Result<AncillaryFrame, St436Error>,
    maximum_bytes: u64,
) -> Result<u64, St436Error> {
    let mut total = 0u64;
    let mut frames = 0u64;
    let mut key_seen = None;
    while let Some((key, size, header_bytes)) = read_klv_header(reader)? {
        total = total
            .checked_add(size)
            .and_then(|n| n.checked_add(header_bytes))
            .filter(|n| *n <= maximum_bytes)
            .ok_or(St436Error::Extent)?;
        if is_anc_key(&key) {
            if key_seen.is_some_and(|previous| previous != key)
                || frames >= expected_count
                || size > MAX_ELEMENT_BYTES as u64
            {
                return Err(St436Error::CanonicalMismatch);
            }
            key_seen = Some(key);
            let mut bytes = vec![0; size as usize];
            reader.read_exact(&mut bytes)?;
            reimport_st436_canonical(&expected(frames)?, &bytes)?;
            frames += 1;
        } else {
            let copied = std::io::copy(&mut (&mut *reader).take(size), &mut std::io::sink())?;
            if copied != size {
                return Err(St436Error::Extent);
            }
        }
    }
    if frames != expected_count || frames == 0 {
        return Err(St436Error::CanonicalMismatch);
    }
    Ok(frames)
}

fn is_anc_key(key: &[u8; 16]) -> bool {
    key[..12] == ST436_ANC_ESSENCE_KEY[..12]
        && key[12] == 0x17
        && key[13] != 0
        && key[14] == 2
        && key[15] != 0
}
fn ber_length(size: usize) -> Vec<u8> {
    if size < 128 {
        return vec![size as u8];
    }
    let bytes = (size as u64).to_be_bytes();
    let start = bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
    let mut result = vec![0x80 | (8 - start) as u8];
    result.extend_from_slice(&bytes[start..]);
    result
}
fn read_klv_header(reader: &mut impl Read) -> Result<Option<([u8; 16], u64, u64)>, St436Error> {
    let mut key = [0u8; 16];
    match reader.read(&mut key[..1]) {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    reader.read_exact(&mut key[1..])?;
    if key[..4] != [6, 14, 43, 52] {
        return Err(St436Error::UnsupportedCoding);
    }
    let mut first = [0];
    reader.read_exact(&mut first)?;
    let size = if first[0] & 128 == 0 {
        u64::from(first[0])
    } else {
        let width = usize::from(first[0] & 127);
        if width == 0 || width > 8 {
            return Err(St436Error::Extent);
        }
        let mut bytes = [0u8; 8];
        reader.read_exact(&mut bytes[8 - width..])?;
        u64::from_be_bytes(bytes)
    };
    let header_bytes = 17
        + if first[0] & 128 == 0 {
            0
        } else {
            u64::from(first[0] & 127)
        };
    Ok(Some((key, size, header_bytes)))
}
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], St436Error> {
        let end = self.at.checked_add(count).ok_or(St436Error::Extent)?;
        let bytes = self.bytes.get(self.at..end).ok_or(St436Error::Extent)?;
        self.at = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8, St436Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, St436Error> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().map_err(|_| St436Error::Extent)?,
        ))
    }
    fn u32(&mut self) -> Result<u32, St436Error> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| St436Error::Extent)?,
        ))
    }
}
/// Standard carriage, source-word or bounded reimport failure.
#[derive(Debug, thiserror::Error)]
pub enum St436Error {
    /// Canonical ST291 syntax or inventory failure.
    #[error(transparent)]
    Packet(#[from] AncillaryPacketError),
    /// Input or output transport failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Truncated, overflowing or unbounded element/array.
    #[error("invalid ST436 extent")]
    Extent,
    /// Ambiguous field wrapping or unsupported chroma/error sample coding.
    #[error("unsupported ST436 wrapping or sample coding")]
    UnsupportedCoding,
    /// Arbitrary horizontal positions are not carried by ST436.
    #[error("ST436 cannot preserve this explicit horizontal offset")]
    UnrepresentablePlacement,
    /// Unused ten-bit samples or low-order padding bits are nonzero.
    #[error("noncanonical ST436 padding")]
    Padding,
    /// Unaccounted trailing bytes followed the declared inventory.
    #[error("trailing bytes after ST436 inventory")]
    TrailingData,
    /// Actual file content differs from its canonical owner.
    #[error("ST436 canonical reimport mismatch")]
    CanonicalMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(payload: &[u8]) -> AncillaryFrame {
        AncillaryFrame::new(
            9,
            vec![AncillaryPacket {
                placement: AncillaryPlacement::new(
                    AncillarySpace::Vanc,
                    AncillaryField::Progressive,
                    20,
                    0,
                )
                .expect("placement"),
                packet: St291Type2Packet::from_8bit_payload(0x61, 1, payload).expect("packet"),
                origin: AncillaryOrigin::Derived,
                validation: AncillaryValidationLevel::Semantic,
            }],
        )
        .expect("frame")
    }
    #[test]
    fn standard_ten_bit_golden_and_all_partial_inputs() {
        let expected = frame(&[]);
        let encoded = encode_st436_ancillary(&expected).expect("encode");
        assert_eq!(
            encoded,
            vec![
                0, 1, 0, 20, 4, 7, 0, 4, 0, 0, 0, 8, 0, 0, 0, 1, 0x58, 0x50, 0x18, 0, 0x98, 0x80,
                0, 0
            ]
        );
        assert_eq!(
            reimport_st436_canonical(&expected, &encoded).expect("actual").sha256(),
            expected.sha256()
        );
        for end in 0..encoded.len() {
            assert!(decode_st436_ancillary(9, &encoded[..end]).is_err());
        }
        let mut corrupt = encoded.clone();
        corrupt[23] = 1;
        assert!(decode_st436_ancillary(9, &corrupt).is_err());
        let mut extra = encoded.clone();
        extra.push(0);
        assert!(decode_st436_ancillary(9, &extra).is_err());
        let mut huge = encoded.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_st436_ancillary(9, &huge).is_err());
        let maximum = frame(&[0x55; 255]);
        assert!(reimport_st436_canonical(
            &maximum,
            &encode_st436_ancillary(&maximum).expect("max encode")
        )
        .is_ok());
    }
    #[test]
    fn real_klv_scan_rejects_missing_extra_changed_and_truncated_essence() {
        let expected = frame(&[1, 2, 3]);
        let mut stream = Vec::new();
        write_st436_klv_frame(&mut stream, &expected).expect("KLV");
        assert_eq!(
            verify_st436_mxf_ancillary(
                &mut stream.as_slice(),
                std::slice::from_ref(&expected),
                1_000_000
            )
            .expect("scan"),
            1
        );
        assert!(verify_st436_mxf_ancillary(
            &mut stream.as_slice(),
            &[frame(&[3, 2, 1])],
            1_000_000
        )
        .is_err());
        for end in 1..stream.len() {
            assert!(verify_st436_mxf_ancillary(
                &mut &stream[..end],
                std::slice::from_ref(&expected),
                1_000_000
            )
            .is_err());
        }
        let doubled = [stream.as_slice(), stream.as_slice()].concat();
        assert!(verify_st436_mxf_ancillary(
            &mut doubled.as_slice(),
            std::slice::from_ref(&expected),
            1_000_000
        )
        .is_err());
        let mut shifted = expected.packets().to_vec();
        shifted[0].placement.horizontal_offset = 1;
        assert!(
            encode_st436_ancillary(&AncillaryFrame::new(9, shifted).expect("shifted")).is_err()
        );
    }

    #[test]
    fn eight_bit_luma_reimport_regenerates_only_standard_excluded_words() {
        let bytes = [
            0, 1, 0, 20, 4, 4, 0, 3, 0, 0, 0, 4, 0, 0, 0, 1, 0x61, 1, 0, 0,
        ];
        let expected = frame(&[]);
        assert_eq!(
            reimport_st436_canonical(&expected, &bytes).expect("8-bit transport").sha256(),
            expected.sha256()
        );
        let mut corrupt = bytes;
        corrupt[19] = 1;
        assert!(decode_st436_ancillary(9, &corrupt).is_err());
        for coding in [0, 1, 2, 3, 5, 6, 8, 9, 10, 11, 12, 255] {
            let mut unsupported = bytes;
            unsupported[5] = coding;
            assert!(decode_st436_ancillary(9, &unsupported).is_err());
        }
        let mut ambiguous = bytes;
        ambiguous[4] = 1;
        assert!(decode_st436_ancillary(9, &ambiguous).is_err());
    }

    #[test]
    #[ignore = "requires official BMX 1.6 binaries via MONDRIAN_ST436_BMX_TOOL_DIR"]
    fn official_bmx_wrap_and_actual_mxf_reimport_preserve_exact_sparse_anc() {
        use std::process::Command;
        let tools = std::path::PathBuf::from(
            std::env::var_os("MONDRIAN_ST436_BMX_TOOL_DIR")
                .expect("explicit official BMX tool directory"),
        );
        let suffix = std::env::consts::EXE_SUFFIX;
        let wrapper = tools.join(format!("raw2bmx{suffix}"));
        let reader = tools.join(format!("mxf2raw{suffix}"));
        for executable in [&wrapper, &reader] {
            let version =
                Command::new(executable).arg("--version").output().expect("version command");
            assert!(version.status.success());
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&version.stdout),
                String::from_utf8_lossy(&version.stderr)
            );
            assert!(text.contains("bmx v1.6.0"), "{text}");
        }
        let work = tempfile::tempdir().expect("private input/output directory");
        let input = work.path().join("canonical.klv");
        let output = work.path().join("actual.mxf");
        let expected = [frame(&[]), AncillaryFrame::empty(10), frame(&[0x55; 255])];
        let mut file = std::fs::File::create_new(&input).expect("input");
        for item in &expected {
            write_st436_klv_frame(&mut file, item).expect("canonical KLV");
        }
        file.sync_all().expect("sync");
        drop(file);
        let wrapped = Command::new(wrapper)
            .args(["-t", "op1a", "-f", "25", "--dur", "3", "-o"])
            .arg(&output)
            .args(["--klv", "s", "--anc"])
            .arg(&input)
            .output()
            .expect("real wrapper");
        assert!(
            wrapped.status.success(),
            "{}",
            String::from_utf8_lossy(&wrapped.stderr)
        );
        let inspected = Command::new(reader)
            .args(["-i", "--check-end", "--check-complete"])
            .arg(&output)
            .output()
            .expect("real reader");
        assert!(
            inspected.status.success(),
            "{}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        let text = String::from_utf8_lossy(&inspected.stdout);
        for token in [
            "ANC_Data",
            "ANC_10_Bit_Luma",
            "VANC_Progressive_Frame",
            "is_complete     : true",
            "last_frame      : true",
        ] {
            assert!(text.contains(token), "missing {token}: {text}");
        }
        let actual = std::fs::read(output).expect("actual MXF");
        assert_eq!(
            verify_st436_mxf_ancillary(&mut actual.as_slice(), &expected, actual.len() as u64)
                .expect("independent actual words"),
            3
        );
        let mut corrupted = actual.clone();
        let location = corrupted
            .windows(16)
            .position(|key| key == ST436_ANC_ESSENCE_KEY)
            .expect("actual ANC key");
        // Corrupt the first essence key into a second track identity. This must
        // fail even while every wrapped payload and later packet is unchanged.
        corrupted[location + 15] = 2;
        assert!(verify_st436_mxf_ancillary(
            &mut corrupted.as_slice(),
            &expected,
            corrupted.len() as u64
        )
        .is_err());
    }
}
