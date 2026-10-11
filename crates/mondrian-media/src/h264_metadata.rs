//! Bounded H.264 SPS range evidence (H.264 Annex E), independent of file names.
//!
//! FFmpeg preserves an absent video_full_range_flag as AVColorRange::Unspecified.
//! The syntax instead infers zero. Only complete, agreeing SPS RBSPs authorize
//! that inference; unrelated codecs and malformed configurations stay unknown.

use mondrian_core::DecodedVideoRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SequenceRange {
    pub range: DecodedVideoRange,
    pub defaulted: bool,
}

pub(crate) fn sequence_range(bytes: &[u8]) -> Option<SequenceRange> {
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    let mut sets = Vec::new();
    if bytes.first() == Some(&1) {
        if bytes.len() < 7
            || bytes[4] & 0xfc != 0xfc
            || bytes[4] & 3 == 2
            || bytes[5] & 0xe0 != 0xe0
        {
            return None;
        }
        let mut position = 6;
        for _ in 0..bytes[5] & 31 {
            let length =
                u16::from_be_bytes(bytes.get(position..position + 2)?.try_into().ok()?) as usize;
            position += 2;
            sets.push(bytes.get(position..position.checked_add(length)?)?);
            position += length;
        }
        let count = *bytes.get(position)?;
        position += 1;
        for _ in 0..count {
            let length =
                u16::from_be_bytes(bytes.get(position..position + 2)?.try_into().ok()?) as usize;
            position += 2;
            let pps = bytes.get(position..position.checked_add(length)?)?;
            if pps.first().is_none_or(|header| header & 0x9f != 8) {
                return None;
            }
            position += length;
        }
        // High-profile avcC extensions are not needed for range; FFmpeg owns
        // complete configuration admission. Every declared SPS above is read.
    } else {
        let starts = bytes
            .windows(3)
            .enumerate()
            .filter_map(|(index, prefix)| (prefix == [0, 0, 1]).then_some(index))
            .take(65)
            .collect::<Vec<_>>();
        if starts.len() > 64 {
            return None;
        }
        if starts.first().is_none_or(|&start| bytes[..start].iter().any(|&byte| byte != 0)) {
            return None;
        }
        for (index, &start) in starts.iter().enumerate() {
            let end = starts.get(index + 1).copied().unwrap_or(bytes.len());
            let mut nal = &bytes[start + 3..end];
            while nal.last() == Some(&0) {
                nal = &nal[..nal.len() - 1];
            }
            if nal.first().is_some_and(|header| header & 31 == 7) {
                sets.push(nal);
            }
        }
    }
    if sets.is_empty() || sets.len() > 31 {
        return None;
    }
    let mut selected = None;
    for nal in sets {
        let next = sps_range(nal)?;
        if let Some(previous) = selected {
            let previous: SequenceRange = previous;
            if previous.range != next.range {
                return None;
            }
            selected = Some(SequenceRange {
                range: next.range,
                defaulted: previous.defaulted && next.defaulted,
            });
        } else {
            selected = Some(next);
        }
    }
    selected
}

struct Bits {
    bytes: Vec<u8>,
    position: usize,
}
impl Bits {
    fn new(nal: &[u8]) -> Option<Self> {
        if nal.first().is_none_or(|header| header & 0x9f != 7 || header & 0x60 == 0)
            || nal.len() > 65535
        {
            return None;
        }
        let mut bytes = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        let data = &nal[1..];
        for (index, &byte) in data.iter().enumerate() {
            if zeros >= 2 && byte == 3 {
                if data.get(index + 1).is_none_or(|&next| next > 3) {
                    return None;
                }
                zeros = 0;
                continue;
            }
            if zeros >= 2 && byte <= 2 {
                return None;
            }
            bytes.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        Some(Self { bytes, position: 0 })
    }
    fn read(&mut self, count: usize) -> Option<u32> {
        if count > 32 || self.position.checked_add(count)? > self.bytes.len() * 8 {
            return None;
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1)
                | u32::from((self.bytes[self.position / 8] >> (7 - self.position % 8)) & 1);
            self.position += 1;
        }
        Some(value)
    }
    fn flag(&mut self) -> Option<bool> {
        Some(self.read(1)? != 0)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            if zeros > 30 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.read(zeros)?)
    }
    fn se(&mut self) -> Option<i32> {
        let code = self.ue()?;
        Some(if code & 1 == 0 {
            -(code as i32 / 2)
        } else {
            (code as i32 + 1) / 2
        })
    }
    fn trailing(&mut self) -> Option<()> {
        if !self.flag()? {
            return None;
        }
        if self.bytes.len() * 8 - self.position > 7 {
            return None;
        }
        while self.position < self.bytes.len() * 8 {
            if self.flag()? {
                return None;
            }
        }
        Some(())
    }
}

fn hrd(bits: &mut Bits) -> Option<()> {
    let count = bits.ue()?;
    if count > 31 {
        return None;
    }
    bits.read(8)?;
    for _ in 0..=count {
        bits.ue()?;
        bits.ue()?;
        bits.flag()?;
    }
    bits.read(20)?;
    Some(())
}

fn sps_range(nal: &[u8]) -> Option<SequenceRange> {
    let mut bits = Bits::new(nal)?;
    let profile = bits.read(8)?;
    if bits.read(8)? & 3 != 0 {
        return None;
    }
    bits.read(8)?;
    if bits.ue()? > 31 {
        return None;
    }
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma = bits.ue()?;
        if chroma > 3 {
            return None;
        }
        if chroma == 3 {
            bits.flag()?;
        }
        if bits.ue()? > 6 || bits.ue()? > 6 {
            return None;
        }
        bits.flag()?;
        if bits.flag()? {
            for index in 0..if chroma == 3 { 12 } else { 8 } {
                if bits.flag()? {
                    let mut last = 8i32;
                    let mut next = 8i32;
                    for _ in 0..if index < 6 { 16 } else { 64 } {
                        if next != 0 {
                            let delta = bits.se()?;
                            if !(-128..=127).contains(&delta) {
                                return None;
                            }
                            next = (last + delta + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    } else if !matches!(profile, 66 | 77 | 88) {
        return None;
    }
    if bits.ue()? > 12 {
        return None;
    }
    match bits.ue()? {
        0 => {
            if bits.ue()? > 12 {
                return None;
            }
        }
        1 => {
            bits.flag()?;
            bits.se()?;
            bits.se()?;
            let count = bits.ue()?;
            if count > 255 {
                return None;
            }
            for _ in 0..count {
                bits.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    bits.ue()?;
    bits.flag()?;
    bits.ue()?;
    bits.ue()?;
    if !bits.flag()? {
        bits.flag()?;
    }
    bits.flag()?;
    if bits.flag()? {
        for _ in 0..4 {
            bits.ue()?;
        }
    }
    let mut range = SequenceRange { range: DecodedVideoRange::Limited, defaulted: true };
    if bits.flag()? {
        if bits.flag()? && bits.read(8)? == 255 {
            bits.read(16)?;
            bits.read(16)?;
        }
        if bits.flag()? {
            bits.flag()?;
        }
        if bits.flag()? {
            bits.read(3)?;
            range = SequenceRange {
                range: if bits.flag()? {
                    DecodedVideoRange::Full
                } else {
                    DecodedVideoRange::Limited
                },
                defaulted: false,
            };
            if bits.flag()? {
                bits.read(8)?;
                bits.read(8)?;
                bits.read(8)?;
            }
        }
        if bits.flag()? {
            bits.ue()?;
            bits.ue()?;
        }
        if bits.flag()? {
            bits.read(32)?;
            bits.read(32)?;
            bits.flag()?;
        }
        let nal_hrd = bits.flag()?;
        if nal_hrd {
            hrd(&mut bits)?;
        }
        let vcl_hrd = bits.flag()?;
        if vcl_hrd {
            hrd(&mut bits)?;
        }
        if nal_hrd || vcl_hrd {
            bits.flag()?;
        }
        bits.flag()?;
        if bits.flag()? {
            bits.flag()?;
            for _ in 0..6 {
                bits.ue()?;
            }
        }
    }
    bits.trailing()?;
    Some(range)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Actual SPS from the reported 2048x858 H.264 High clip, no name or CICP.
    const SPS: &[u8] = &[
        0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x20, 0x01, 0xb7, 0xe4, 0xc0, 0x44, 0x00, 0x00,
        0x0f, 0xa4, 0x00, 0x02, 0xee, 0x00, 0x3c, 0x60, 0xc6, 0x58,
    ];
    fn avcc(sets: &[&[u8]]) -> Vec<u8> {
        let mut bytes = vec![1, 100, 0, 40, 255, 224 | sets.len() as u8];
        for set in sets {
            bytes.extend_from_slice(&(set.len() as u16).to_be_bytes());
            bytes.extend_from_slice(set);
        }
        bytes.extend_from_slice(&[1, 0, 1, 0x68]);
        bytes
    }
    fn signal_sps(full: bool) -> Vec<u8> {
        let rbsp = Bits::new(SPS).unwrap().bytes;
        let mut bits = (0..rbsp.len() * 8)
            .map(|index| (rbsp[index / 8] >> (7 - index % 8)) & 1)
            .collect::<Vec<_>>();
        // Actual SPS video_signal_type_present_flag is RBSP bit 91.
        bits.splice(91..92, [1, 1, 0, 1, u8::from(full), 0]);
        while bits.last() == Some(&0) {
            bits.pop();
        }
        while bits.len() % 8 != 0 {
            bits.push(0);
        }
        let mut nal = vec![0x67];
        let mut zeros = 0;
        for octet in bits.chunks_exact(8) {
            let byte = octet.iter().fold(0, |value, bit| (value << 1) | bit);
            if zeros >= 2 && byte <= 3 {
                nal.push(3);
                zeros = 0;
            }
            nal.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        nal
    }
    #[test]
    fn explicit_full_and_limited_remain_distinct_and_conflicts_are_unknown() {
        let full = signal_sps(true);
        let limited = signal_sps(false);
        assert_eq!(
            sequence_range(&avcc(&[&full])),
            Some(SequenceRange { range: DecodedVideoRange::Full, defaulted: false })
        );
        assert_eq!(
            sequence_range(&avcc(&[&limited])),
            Some(SequenceRange {
                range: DecodedVideoRange::Limited,
                defaulted: false
            })
        );
        assert_eq!(sequence_range(&avcc(&[SPS, &full])), None);
        assert_eq!(sequence_range(&avcc(&[&full, SPS])), None);
        let mut invalid = full.clone();
        invalid[0] |= 0x80;
        assert_eq!(sequence_range(&avcc(&[&invalid])), None);
        assert_eq!(sequence_range(&[0, 0, 1, 0x68, 0x80]), None);
    }
    #[test]
    fn codec_default_requires_complete_sps_and_agreeing_sets() {
        assert_eq!(
            sequence_range(&avcc(&[SPS])),
            Some(SequenceRange { range: DecodedVideoRange::Limited, defaulted: true })
        );
        for end in 0..SPS.len() {
            assert_eq!(sequence_range(&avcc(&[&SPS[..end]])), None, "prefix {end}");
        }
        assert_eq!(sequence_range(&avcc(&[])), None);
        let mut annex_b = vec![0, 0, 0, 1];
        annex_b.extend_from_slice(SPS);
        assert_eq!(sequence_range(&annex_b), sequence_range(&avcc(&[SPS])));
    }
}
