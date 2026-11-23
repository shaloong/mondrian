//! BWF recording time and the optional iXML display grid.

use mondrian_core::{AudioSourceTimecode, Rational, SmpteCountingMode};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_CHUNKS: usize = 4096;
const MAX_IXML_BYTES: u64 = 4 * 1024 * 1024;
const BEXT_TIME_REFERENCE_OFFSET: u64 = 338;
const BEXT_FIXED_SIZE: u64 = 602;

type ClockGrid = (Rational, SmpteCountingMode);

pub(crate) fn read(path: &Path) -> Option<AudioSourceTimecode> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    read_wave(&mut file, length)
}

fn read_wave(reader: &mut (impl Read + Seek), length: u64) -> Option<AudioSourceTimecode> {
    let (form, size) = chunk_header(reader)?;
    if &read_bytes::<4>(reader)? != b"WAVE" {
        return None;
    }
    let mut extended = match &form {
        b"RIFF" => None,
        b"RF64" | b"BW64" => Some(ExtendedSizes::read(reader, length)?),
        _ => return None,
    };
    let size = match size {
        u32::MAX => extended.as_ref()?.riff,
        size => u64::from(size),
    };
    let end = size.checked_add(8)?;
    if end > length {
        return None;
    }

    let mut sample_count = None;
    let mut sample_rate = None;
    let mut grid = None;
    let mut seen_ixml = false;
    for _ in 0..MAX_CHUNKS {
        let offset = reader.stream_position().ok()?;
        if offset == end {
            return AudioSourceTimecode::from_sample_count(sample_count?, sample_rate?, grid);
        }
        let start = offset.checked_add(8)?;
        if start > end {
            return None;
        }
        let (id, size) = chunk_header(reader)?;
        let size = match size {
            u32::MAX => extended.as_mut()?.take_size(id)?,
            size => u64::from(size),
        };
        let next = chunk_end(start, size)?;
        if next > end {
            return None;
        }
        match &id {
            b"fmt " => {
                if size < 16 || sample_rate.is_some() {
                    return None;
                }
                let format = read_bytes::<8>(reader)?;
                sample_rate = Some(u32::from_le_bytes(format[4..8].try_into().ok()?));
            }
            b"bext" => {
                if size < BEXT_FIXED_SIZE || sample_count.is_some() {
                    return None;
                }
                reader.seek(SeekFrom::Current(BEXT_TIME_REFERENCE_OFFSET as i64)).ok()?;
                sample_count = Some(u64::from_le_bytes(read_bytes(reader)?));
            }
            b"iXML" => {
                grid = if !seen_ixml && size <= MAX_IXML_BYTES {
                    let mut bytes = vec![0; size as usize];
                    reader.read_exact(&mut bytes).ok()?;
                    read_ixml_grid(&bytes)
                } else {
                    None
                };
                seen_ixml = true;
            }
            _ => {}
        }
        reader.seek(SeekFrom::Start(next)).ok()?;
    }
    None
}

struct ExtendedSizes {
    riff: u64,
    data: Option<u64>,
    other: Vec<([u8; 4], u64)>,
}

impl ExtendedSizes {
    fn read(reader: &mut (impl Read + Seek), length: u64) -> Option<Self> {
        let (id, size) = chunk_header(reader)?;
        let size = u64::from(size);
        let next = chunk_end(reader.stream_position().ok()?, size)?;
        if &id != b"ds64" || size < 28 || next > length {
            return None;
        }
        let riff = u64::from_le_bytes(read_bytes(reader)?);
        let data = u64::from_le_bytes(read_bytes(reader)?);
        let _sample_count = read_bytes::<8>(reader)?;
        let entries = u32::from_le_bytes(read_bytes(reader)?);
        if entries as usize > MAX_CHUNKS || 28 + u64::from(entries) * 12 > size {
            return None;
        }
        let mut other = Vec::with_capacity(entries as usize);
        for _ in 0..entries {
            let id = read_bytes(reader)?;
            let size = u64::from_le_bytes(read_bytes(reader)?);
            other.push((id, size));
        }
        reader.seek(SeekFrom::Start(next)).ok()?;
        Some(Self { riff, data: Some(data), other })
    }

    fn take_size(&mut self, id: [u8; 4]) -> Option<u64> {
        if &id == b"data" {
            return self.data.take();
        }
        let index = self.other.iter().position(|(chunk, _)| *chunk == id)?;
        Some(self.other.remove(index).1)
    }
}

fn chunk_end(start: u64, size: u64) -> Option<u64> {
    start.checked_add(size)?.checked_add(size % 2)
}

fn chunk_header(reader: &mut impl Read) -> Option<([u8; 4], u32)> {
    Some((read_bytes(reader)?, u32::from_le_bytes(read_bytes(reader)?)))
}

fn read_bytes<const N: usize>(reader: &mut impl Read) -> Option<[u8; N]> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

fn read_ixml_grid(bytes: &[u8]) -> Option<ClockGrid> {
    let utf16;
    let xml = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let (words, remainder) = bytes[2..].as_chunks::<2>();
        if !remainder.is_empty() {
            return None;
        }
        let decode = if bytes[0] == 0xff {
            u16::from_le_bytes
        } else {
            u16::from_be_bytes
        };
        utf16 = char::decode_utf16(words.iter().copied().map(decode))
            .collect::<Result<String, _>>()
            .ok()?;
        utf16.as_str()
    } else {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        std::str::from_utf8(bytes).ok()?
    };
    let (rate, flag) = speed_fields(xml.trim_end_matches('\0'))?;
    let (num, den) = rate.split_once('/').unwrap_or((rate, "1"));
    let rate = Rational::new(num.parse().ok()?, den.parse().ok()?);
    if rate.num <= 0 || rate.den <= 0 {
        return None;
    }
    let mode = match flag {
        "DF" => SmpteCountingMode::DropFrame,
        "NDF" => SmpteCountingMode::NonDropFrame,
        _ => return None,
    };
    Some((rate.reduce(), mode))
}

/// Read the two plain scalar children of BWFXML/SPEED. Other element bodies
/// are skipped structurally. DTDs and entity expansion are unnecessary for
/// the clock values and are outside this reader's grammar.
fn speed_fields(mut xml: &str) -> Option<(&str, &str)> {
    let mut parents = Vec::new();
    let mut seen_root = false;
    let mut seen_speed = false;
    let mut rate = None;
    let mut flag = None;
    while let Some(open) = xml.find('<') {
        let (body, markup) = xml.split_at(open);
        let markup = &markup[1..];
        if parents.is_empty() && !body.trim().is_empty() {
            return None;
        }
        if let Some(comment) = markup.strip_prefix("!--") {
            xml = comment.split_once("-->")?.1;
            continue;
        }
        if let Some(instruction) = markup.strip_prefix('?') {
            xml = instruction.split_once("?>")?.1;
            continue;
        }
        if let Some(cdata) = markup.strip_prefix("![CDATA[")
            && !parents.is_empty()
        {
            xml = cdata.split_once("]]>")?.1;
            continue;
        }
        if let Some(closing) = markup.strip_prefix('/') {
            let (name, rest) = closing.split_once('>')?;
            if parents.pop()? != name.trim_end() {
                return None;
            }
            xml = rest;
            continue;
        }

        let (tag, rest) = opening_tag(markup)?;
        xml = rest;
        let name = tag.split(|ch: char| ch.is_ascii_whitespace() || ch == '/').next()?;
        if parents.is_empty() {
            if seen_root || name != "BWFXML" {
                return None;
            }
            seen_root = true;
        }
        if parents.as_slice() == ["BWFXML"] && name == "SPEED" {
            if seen_speed {
                return None;
            }
            seen_speed = true;
        }
        if parents.as_slice() == ["BWFXML", "SPEED"] {
            let field = match name {
                "TIMECODE_RATE" => Some(&mut rate),
                "TIMECODE_FLAG" => Some(&mut flag),
                _ => None,
            };
            if let Some(field) = field {
                let (value, closing) = xml.split_once('<')?;
                let (closing, rest) = closing.strip_prefix('/')?.split_once('>')?;
                if tag.ends_with('/') || closing.trim_end() != name || field.is_some() {
                    return None;
                }
                *field = Some(value.trim());
                xml = rest;
                continue;
            }
        }
        if !tag.ends_with('/') {
            parents.push(name);
        }
    }
    if parents.is_empty() && xml.trim().is_empty() {
        Some((rate?, flag?))
    } else {
        None
    }
}

fn opening_tag(markup: &str) -> Option<(&str, &str)> {
    if !markup.as_bytes().first()?.is_ascii_alphabetic() {
        return None;
    }
    let mut quote = None;
    for (index, byte) in markup.bytes().enumerate() {
        match (quote, byte) {
            (Some(active), byte) if active == byte => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some((&markup[..index], &markup[index + 1..])),
            (None, b'<') => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::TimelineTime;
    use std::io::Cursor;

    const WAVE_IXML: &[u8] = b"<?xml version=\"1.0\"?><BWFXML>+        <NOTE><![CDATA[<SPEED><TIMECODE_RATE>25</TIMECODE_RATE></SPEED>]]></NOTE>+        <SPEED><!-- clock fields --><TIMECODE_RATE>30000/1001</TIMECODE_RATE>+        <TIMECODE_FLAG>DF</TIMECODE_FLAG><TIMESTAMP_SAMPLES_SINCE_MIDNIGHT_LO>0+        </TIMESTAMP_SAMPLES_SINCE_MIDNIGHT_LO></SPEED></BWFXML>";

    fn append_chunk(file: &mut Vec<u8>, id: &[u8; 4], size: u32, bytes: &[u8]) {
        file.extend(id);
        file.extend(size.to_le_bytes());
        file.extend(bytes);
        if !bytes.len().is_multiple_of(2) {
            file.push(0);
        }
    }

    fn wave(samples: Option<u64>, ixml: Option<&[u8]>, extended: bool) -> Vec<u8> {
        let mut file = if extended { b"RF64" } else { b"RIFF" }.to_vec();
        file.extend(u32::MAX.to_le_bytes());
        file.extend(b"WAVE");
        if extended {
            let mut ds64 = vec![0u8; 40];
            ds64[8..16].copy_from_slice(&4_u64.to_le_bytes());
            ds64[16..24].copy_from_slice(&2_u64.to_le_bytes());
            ds64[24..28].copy_from_slice(&1_u32.to_le_bytes());
            ds64[28..32].copy_from_slice(b"JUNK");
            ds64[32..40].copy_from_slice(&1_u64.to_le_bytes());
            append_chunk(&mut file, b"ds64", 40, &ds64);
        }
        // 48 kHz, mono, signed 16-bit PCM.
        let format = [1, 0, 1, 0, 128, 187, 0, 0, 0, 119, 1, 0, 2, 0, 16, 0];
        append_chunk(&mut file, b"fmt ", 16, &format);
        if let Some(samples) = samples {
            let mut bext = vec![0; BEXT_FIXED_SIZE as usize];
            let start = BEXT_TIME_REFERENCE_OFFSET as usize;
            bext[start..start + 8].copy_from_slice(&samples.to_le_bytes());
            append_chunk(&mut file, b"bext", bext.len() as u32, &bext);
        }
        append_chunk(
            &mut file,
            b"JUNK",
            if extended { u32::MAX } else { 1 },
            &[1],
        );
        append_chunk(
            &mut file,
            b"data",
            if extended { u32::MAX } else { 4 },
            &[0; 4],
        );
        if let Some(ixml) = ixml {
            append_chunk(&mut file, b"iXML", ixml.len() as u32, ixml);
        }
        let length = file.len() as u64 - 8;
        if extended {
            file[20..28].copy_from_slice(&length.to_le_bytes());
        } else {
            file[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        }
        file
    }

    #[test]
    fn media_probe_captures_exact_wave_origin_and_declared_grid() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.wav");
        std::fs::write(&path, wave(Some(172_801_440), Some(WAVE_IXML), false)).unwrap();
        let info = crate::probe_media_info(&path).unwrap();
        let stream = info.primary_audio().unwrap();
        let timecode = stream.source_timecode.unwrap();
        assert_eq!(timecode.origin(), TimelineTime::new(360_003, 100).unwrap());
        let grid = timecode.grid().unwrap();
        assert_eq!(grid.frame_rate(), Rational::FPS_2997);
        assert_eq!(grid.mode(), SmpteCountingMode::DropFrame);

        let mut old = serde_json::to_value(stream).unwrap();
        old.as_object_mut().unwrap().remove("source_timecode").unwrap();
        let restored: mondrian_core::AudioStreamInfo = serde_json::from_value(old).unwrap();
        assert_eq!(restored.source_timecode, None);
    }

    #[test]
    fn wave_timecode_skips_extended_chunks_and_preserves_64_bit_sample_counts() {
        let count = u64::from(u32::MAX) + 721;
        let mut bytes = wave(Some(count), Some(WAVE_IXML), true);
        let length = bytes.len() as u64;
        let expected = TimelineTime::new(count as i64, 48_000).unwrap();
        let rf64 = read_wave(&mut Cursor::new(&bytes), length).unwrap();
        assert_eq!(rf64.origin(), expected);
        assert_eq!(rf64.grid().unwrap().frame_rate(), Rational::FPS_2997);
        bytes[..4].copy_from_slice(b"BW64");
        assert_eq!(read_wave(&mut Cursor::new(bytes), length), Some(rf64));
    }

    #[test]
    fn wave_timecode_distinguishes_midnight_from_missing_and_rejects_truncation() {
        for (samples, expected) in [(None, None), (Some(0), Some(TimelineTime::ZERO))] {
            let bytes = wave(samples, None, false);
            let length = bytes.len() as u64;
            let timecode = read_wave(&mut Cursor::new(&bytes), length);
            assert_eq!(timecode.map(AudioSourceTimecode::origin), expected);
            assert_eq!(timecode.and_then(AudioSourceTimecode::grid), None);
            assert_eq!(read_wave(&mut Cursor::new(&bytes), length - 1), None);
        }
    }

    #[test]
    fn wave_timecode_reads_utf16_and_ignores_duplicate_or_unscoped_grid_fields() {
        let xml = std::str::from_utf8(WAVE_IXML).unwrap();
        let utf16: Vec<_> = [0xff, 0xfe]
            .into_iter()
            .chain(xml.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(read_ixml_grid(&utf16), read_ixml_grid(WAVE_IXML));
        for bad in [
            xml.replace(
                "<TIMECODE_FLAG>",
                "<TIMECODE_FLAG>DF</TIMECODE_FLAG><TIMECODE_FLAG>",
            ),
            xml.replace("<SPEED><!--", "<SPEED><OTHER><!--")
                .replace("</SPEED></BWFXML>", "</OTHER></SPEED></BWFXML>"),
            xml.replace("30000/1001</", "0/0</"),
        ] {
            let bytes = wave(Some(4_800), Some(bad.as_bytes()), false);
            let length = bytes.len() as u64;
            let result = read_wave(&mut Cursor::new(bytes), length).unwrap();
            assert_eq!(result.origin(), TimelineTime::new(1, 10).unwrap());
            assert_eq!(result.grid(), None);
        }
    }
}
