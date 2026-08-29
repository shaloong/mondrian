//! DNG/CinemaDNG probe and deterministic CPU development Adapter.
//!
//! FFmpeg owns compressed TIFF/DNG packet decompression. This module owns the
//! typed DNG metadata boundary, Bayer reconstruction, camera development, and
//! declared scene-linear output. Generic RGB swscale is never used for an
//! admitted RAW source.

use crate::decoder::{DecodedVideoRange, DecodedVideoRangeContract};
use crate::preview::{
    resize_float_rgba, CameraRawDecodeIntent, DecodedRgbaFrameContract, FloatRgbaFrame,
    PreviewDecodePath, PreviewSourceColorContract,
};
use ffmpeg_next as ffmpeg;
use mondrian_core::{
    CameraRawAdapter, CameraRawCfaPattern, CameraRawDebayerQuality, CameraRawInterpretation,
    CameraRawMetadata, CameraRawWhiteBalance, ColorSpace, MondrianError, Resolution, Result,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

const MAX_DNG_BYTES: u64 = 1_073_741_824;
const MAX_IFDS: usize = 32;
const MAX_IFD_ENTRIES: usize = 4_096;
const MAX_TIFF_VALUE_COUNT: u32 = 4_096;

const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_HEIGHT: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_MAKE: u16 = 271;
const TAG_MODEL: u16 = 272;
const TAG_SUB_IFDS: u16 = 330;
const TAG_CFA_REPEAT_PATTERN_DIM: u16 = 33_421;
const TAG_CFA_PATTERN: u16 = 33_422;
const TAG_DNG_VERSION: u16 = 50_706;
const TAG_AS_SHOT_NEUTRAL: u16 = 50_728;
const TAG_COLOR_MATRIX_1: u16 = 50_721;
const TAG_COLOR_MATRIX_2: u16 = 50_722;
const TAG_CINEMADNG_FRAME_RATE: u16 = 51_044;

#[derive(Debug, Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, bytes: [u8; 2]) -> u16 {
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: [u8; 4]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }

    fn i32(self, bytes: [u8; 4]) -> i32 {
        match self {
            Self::Little => i32::from_le_bytes(bytes),
            Self::Big => i32::from_be_bytes(bytes),
        }
    }
}

#[derive(Debug, Clone)]
struct TiffEntry {
    field_type: u16,
    count: u32,
    value: [u8; 4],
}

#[derive(Debug, Clone)]
struct TiffIfd {
    entries: HashMap<u16, TiffEntry>,
}

#[derive(Debug)]
struct ParsedDng {
    endian: Endian,
    bytes: Arc<Vec<u8>>,
    raw_ifd: TiffIfd,
    inherited_ifds: Vec<TiffIfd>,
    metadata: CameraRawMetadata,
    as_shot_neutral: Option<[f64; 3]>,
    color_matrix: Option<[[f64; 3]; 3]>,
}

impl ParsedDng {
    fn entry(&self, tag: u16) -> Option<&TiffEntry> {
        self.raw_ifd
            .entries
            .get(&tag)
            .or_else(|| self.inherited_ifds.iter().find_map(|ifd| ifd.entries.get(&tag)))
    }

    fn entry_bytes<'a>(&'a self, entry: &'a TiffEntry) -> std::result::Result<&'a [u8], String> {
        if entry.count > MAX_TIFF_VALUE_COUNT {
            return Err(format!(
                "TIFF entry contains {} values; maximum is {MAX_TIFF_VALUE_COUNT}",
                entry.count
            ));
        }
        let width = match entry.field_type {
            1 | 2 | 6 | 7 => 1_u64,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            other => return Err(format!("unsupported TIFF field type {other}")),
        };
        let size = width
            .checked_mul(u64::from(entry.count))
            .ok_or_else(|| "TIFF entry byte length overflow".to_owned())?;
        let size = usize::try_from(size).map_err(|_| "TIFF entry is too large".to_owned())?;
        if size <= 4 {
            return Ok(&entry.value[..size]);
        }
        let offset = usize::try_from(self.endian.u32(entry.value))
            .map_err(|_| "TIFF entry offset exceeds address space".to_owned())?;
        self.bytes
            .get(offset..offset.saturating_add(size))
            .ok_or_else(|| "TIFF entry points outside the source file".to_owned())
    }

    fn unsigned_values(&self, entry: &TiffEntry) -> std::result::Result<Vec<u32>, String> {
        let bytes = self.entry_bytes(entry)?;
        match entry.field_type {
            1 | 7 => Ok(bytes.iter().map(|&value| u32::from(value)).collect()),
            3 => bytes
                .chunks_exact(2)
                .map(|value| Ok(u32::from(self.endian.u16([value[0], value[1]]))))
                .collect(),
            4 => bytes
                .chunks_exact(4)
                .map(|value| Ok(self.endian.u32([value[0], value[1], value[2], value[3]])))
                .collect(),
            other => Err(format!("TIFF type {other} is not an unsigned integer")),
        }
    }

    fn rational_values(&self, entry: &TiffEntry) -> std::result::Result<Vec<f64>, String> {
        let bytes = self.entry_bytes(entry)?;
        let signed = entry.field_type == 10;
        if !matches!(entry.field_type, 5 | 10) {
            return Err(format!("TIFF type {} is not rational", entry.field_type));
        }
        bytes
            .chunks_exact(8)
            .map(|value| {
                let numerator = [value[0], value[1], value[2], value[3]];
                let denominator = [value[4], value[5], value[6], value[7]];
                let (numerator, denominator) = if signed {
                    (
                        f64::from(self.endian.i32(numerator)),
                        f64::from(self.endian.i32(denominator)),
                    )
                } else {
                    (
                        f64::from(self.endian.u32(numerator)),
                        f64::from(self.endian.u32(denominator)),
                    )
                };
                if denominator == 0.0 {
                    Err("DNG rational has a zero denominator".to_owned())
                } else {
                    Ok(numerator / denominator)
                }
            })
            .collect()
    }

    fn text(&self, entry: &TiffEntry) -> std::result::Result<Option<String>, String> {
        let bytes = self.entry_bytes(entry)?;
        let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
        let text = String::from_utf8_lossy(&bytes[..end]).trim().to_owned();
        Ok((!text.is_empty()).then_some(text))
    }
}

/// Inspect a file for a closed DNG/CinemaDNG camera RAW contract.
pub fn probe_camera_raw_metadata(path: &Path) -> Result<Option<CameraRawMetadata>> {
    parse_dng(path).map(|parsed| parsed.map(|parsed| parsed.metadata))
}

fn parse_dng(path: &Path) -> Result<Option<ParsedDng>> {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dng"))
    {
        return Ok(None);
    }
    let metadata = std::fs::metadata(path).map_err(|error| MondrianError::MediaOpen {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    if metadata.len() > MAX_DNG_BYTES {
        return Err(MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: format!("DNG source exceeds the {} byte safety bound", MAX_DNG_BYTES),
        });
    }
    let bytes = Arc::new(
        std::fs::read(path).map_err(|error| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?,
    );
    let Some(header) = bytes.get(..8) else {
        return Ok(None);
    };
    let endian = match &header[..2] {
        b"II" => Endian::Little,
        b"MM" => Endian::Big,
        _ => return Ok(None),
    };
    if endian.u16([header[2], header[3]]) != 42 {
        return Ok(None);
    }
    let first_ifd = endian.u32([header[4], header[5], header[6], header[7]]);
    let ifds = parse_ifd_graph(bytes.as_slice(), endian, first_ifd)
        .map_err(|reason| MondrianError::MediaOpen { path: path.display().to_string(), reason })?;
    let is_dng = ifds.iter().any(|ifd| ifd.entries.contains_key(&TAG_DNG_VERSION));
    if !is_dng {
        return Ok(None);
    }
    let raw_index = ifds
        .iter()
        .enumerate()
        .filter(|(_, ifd)| {
            ifd.entries.contains_key(&TAG_CFA_PATTERN)
                && ifd.entries.contains_key(&TAG_CFA_REPEAT_PATTERN_DIM)
        })
        .max_by_key(|(_, ifd)| ifd_area(bytes.as_slice(), endian, ifd))
        .map(|(index, _)| index)
        .ok_or_else(|| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: "DNG source has no supported 2x2 CFA image directory".to_owned(),
        })?;
    let raw_ifd = ifds[raw_index].clone();
    let inherited_ifds = ifds
        .into_iter()
        .enumerate()
        .filter_map(|(index, ifd)| (index != raw_index).then_some(ifd))
        .collect::<Vec<_>>();
    let mut parsed = ParsedDng {
        endian,
        bytes,
        raw_ifd,
        inherited_ifds,
        metadata: CameraRawMetadata {
            adapter: CameraRawAdapter::Dng,
            cfa_pattern: CameraRawCfaPattern::Rggb,
            width: 0,
            height: 0,
            bit_depth: 0,
            compression: 0,
            camera_make: None,
            camera_model: None,
            has_color_matrix: false,
            has_as_shot_neutral: false,
        },
        as_shot_neutral: None,
        color_matrix: None,
    };
    let cfa_dimensions = parsed
        .entry(TAG_CFA_REPEAT_PATTERN_DIM)
        .ok_or_else(|| raw_error(path, "DNG CFA dimensions are missing"))
        .and_then(|entry| {
            parsed.unsigned_values(entry).map_err(|reason| raw_error(path, reason))
        })?;
    if cfa_dimensions.as_slice() != [2, 2] {
        return Err(raw_error(path, "only 2x2 DNG CFA patterns are supported"));
    }
    let cfa = parsed
        .entry(TAG_CFA_PATTERN)
        .ok_or_else(|| raw_error(path, "DNG CFA pattern is missing"))
        .and_then(|entry| {
            parsed.unsigned_values(entry).map_err(|reason| raw_error(path, reason))
        })?;
    parsed.metadata.cfa_pattern = match cfa.as_slice() {
        [0, 1, 1, 2] => CameraRawCfaPattern::Rggb,
        [2, 1, 1, 0] => CameraRawCfaPattern::Bggr,
        [1, 2, 0, 1] => CameraRawCfaPattern::Gbrg,
        [1, 0, 2, 1] => CameraRawCfaPattern::Grbg,
        _ => {
            return Err(raw_error(
                path,
                format!("unsupported DNG CFA pattern {cfa:?}"),
            ))
        }
    };
    parsed.metadata.width = first_unsigned(&parsed, TAG_IMAGE_WIDTH)
        .ok_or_else(|| raw_error(path, "DNG ImageWidth is missing"))?;
    parsed.metadata.height = first_unsigned(&parsed, TAG_IMAGE_HEIGHT)
        .ok_or_else(|| raw_error(path, "DNG ImageLength is missing"))?;
    if parsed.metadata.width == 0 || parsed.metadata.height == 0 {
        return Err(raw_error(path, "DNG image extent is empty"));
    }
    parsed.metadata.bit_depth = first_unsigned(&parsed, TAG_BITS_PER_SAMPLE)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| raw_error(path, "DNG BitsPerSample is missing or invalid"))?;
    parsed.metadata.compression = first_unsigned(&parsed, TAG_COMPRESSION)
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(1);
    parsed.metadata.camera_make = parsed
        .entry(TAG_MAKE)
        .map(|entry| parsed.text(entry))
        .transpose()
        .map_err(|reason| raw_error(path, reason))?
        .flatten();
    parsed.metadata.camera_model = parsed
        .entry(TAG_MODEL)
        .map(|entry| parsed.text(entry))
        .transpose()
        .map_err(|reason| raw_error(path, reason))?
        .flatten();
    parsed.as_shot_neutral = parsed
        .entry(TAG_AS_SHOT_NEUTRAL)
        .map(|entry| parsed.rational_values(entry))
        .transpose()
        .map_err(|reason| raw_error(path, reason))?
        .and_then(|values| values.get(..3).and_then(|values| values.try_into().ok()));
    parsed.metadata.has_as_shot_neutral = parsed.as_shot_neutral.is_some();
    let matrix_entry =
        parsed.entry(TAG_COLOR_MATRIX_1).or_else(|| parsed.entry(TAG_COLOR_MATRIX_2));
    parsed.color_matrix = matrix_entry
        .map(|entry| parsed.rational_values(entry))
        .transpose()
        .map_err(|reason| raw_error(path, reason))?
        .and_then(|values| {
            let values: [f64; 9] = values.get(..9)?.try_into().ok()?;
            Some([
                [values[0], values[1], values[2]],
                [values[3], values[4], values[5]],
                [values[6], values[7], values[8]],
            ])
        });
    parsed.metadata.has_color_matrix = parsed.color_matrix.is_some();
    if parsed
        .inherited_ifds
        .iter()
        .any(|ifd| ifd.entries.contains_key(&TAG_CINEMADNG_FRAME_RATE))
        || parsed.raw_ifd.entries.contains_key(&TAG_CINEMADNG_FRAME_RATE)
    {
        parsed.metadata.adapter = CameraRawAdapter::CinemaDng;
    }
    // The TIFF byte buffer is probe scratch, not retained execution state.
    // Release it before FFmpeg opens the source so large DNG files are not
    // duplicated in memory across metadata parsing and packet decode.
    parsed.bytes = Arc::new(Vec::new());
    parsed.raw_ifd.entries.clear();
    parsed.inherited_ifds.clear();
    Ok(Some(parsed))
}

fn raw_error(path: &Path, reason: impl Into<String>) -> MondrianError {
    MondrianError::MediaOpen {
        path: path.display().to_string(),
        reason: reason.into(),
    }
}

fn first_unsigned(parsed: &ParsedDng, tag: u16) -> Option<u32> {
    parsed
        .entry(tag)
        .and_then(|entry| parsed.unsigned_values(entry).ok()?.first().copied())
}

fn parse_ifd_graph(
    bytes: &[u8],
    endian: Endian,
    first_offset: u32,
) -> std::result::Result<Vec<TiffIfd>, String> {
    let mut pending = vec![first_offset];
    let mut visited = HashSet::new();
    let mut ifds = Vec::new();
    while let Some(offset) = pending.pop() {
        if offset == 0 || !visited.insert(offset) {
            continue;
        }
        if ifds.len() >= MAX_IFDS {
            return Err(format!(
                "TIFF contains more than {MAX_IFDS} reachable image directories"
            ));
        }
        let (ifd, next) = parse_ifd(bytes, endian, offset)?;
        if let Some(entry) = ifd.entries.get(&TAG_SUB_IFDS) {
            pending.extend(unsigned_values(bytes, endian, entry)?);
        }
        pending.push(next);
        ifds.push(ifd);
    }
    Ok(ifds)
}

fn parse_ifd(
    bytes: &[u8],
    endian: Endian,
    offset: u32,
) -> std::result::Result<(TiffIfd, u32), String> {
    let offset = usize::try_from(offset).map_err(|_| "TIFF IFD offset is too large".to_owned())?;
    let count_bytes = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| "TIFF IFD count is outside the file".to_owned())?;
    let count = usize::from(endian.u16([count_bytes[0], count_bytes[1]]));
    if count > MAX_IFD_ENTRIES {
        return Err(format!(
            "TIFF IFD contains {count} entries; maximum is {MAX_IFD_ENTRIES}"
        ));
    }
    let entries_start = offset + 2;
    let entries_end = entries_start
        .checked_add(count.saturating_mul(12))
        .ok_or_else(|| "TIFF IFD length overflow".to_owned())?;
    let entries_bytes = bytes
        .get(entries_start..entries_end)
        .ok_or_else(|| "TIFF IFD entries are outside the file".to_owned())?;
    let mut entries = HashMap::with_capacity(count);
    for entry in entries_bytes.chunks_exact(12) {
        let tag = endian.u16([entry[0], entry[1]]);
        let field_type = endian.u16([entry[2], entry[3]]);
        let count = endian.u32([entry[4], entry[5], entry[6], entry[7]]);
        entries.insert(
            tag,
            TiffEntry {
                field_type,
                count,
                value: [entry[8], entry[9], entry[10], entry[11]],
            },
        );
    }
    let next_bytes = bytes
        .get(entries_end..entries_end.saturating_add(4))
        .ok_or_else(|| "TIFF next-IFD offset is outside the file".to_owned())?;
    let next = endian.u32([next_bytes[0], next_bytes[1], next_bytes[2], next_bytes[3]]);
    Ok((TiffIfd { entries }, next))
}

fn ifd_area(bytes: &[u8], endian: Endian, ifd: &TiffIfd) -> u64 {
    let value = |tag| {
        let entry = ifd.entries.get(&tag)?;
        unsigned_values(bytes, endian, entry).ok()?.first().copied()
    };
    u64::from(value(TAG_IMAGE_WIDTH).unwrap_or(0))
        .saturating_mul(u64::from(value(TAG_IMAGE_HEIGHT).unwrap_or(0)))
}

fn entry_bytes<'a>(
    bytes: &'a [u8],
    endian: Endian,
    entry: &'a TiffEntry,
) -> std::result::Result<&'a [u8], String> {
    if entry.count > MAX_TIFF_VALUE_COUNT {
        return Err(format!(
            "TIFF entry contains {} values; maximum is {MAX_TIFF_VALUE_COUNT}",
            entry.count
        ));
    }
    let width = match entry.field_type {
        1 | 2 | 6 | 7 => 1_u64,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        other => return Err(format!("unsupported TIFF field type {other}")),
    };
    let size = width
        .checked_mul(u64::from(entry.count))
        .ok_or_else(|| "TIFF entry byte length overflow".to_owned())?;
    let size = usize::try_from(size).map_err(|_| "TIFF entry is too large".to_owned())?;
    if size <= 4 {
        return Ok(&entry.value[..size]);
    }
    let offset = usize::try_from(endian.u32(entry.value))
        .map_err(|_| "TIFF entry offset exceeds address space".to_owned())?;
    bytes
        .get(offset..offset.saturating_add(size))
        .ok_or_else(|| "TIFF entry points outside the source file".to_owned())
}

fn unsigned_values(
    bytes: &[u8],
    endian: Endian,
    entry: &TiffEntry,
) -> std::result::Result<Vec<u32>, String> {
    let bytes = entry_bytes(bytes, endian, entry)?;
    match entry.field_type {
        1 | 7 => Ok(bytes.iter().map(|&value| u32::from(value)).collect()),
        3 => Ok(bytes
            .chunks_exact(2)
            .map(|value| u32::from(endian.u16([value[0], value[1]])))
            .collect()),
        4 => Ok(bytes
            .chunks_exact(4)
            .map(|value| endian.u32([value[0], value[1], value[2], value[3]]))
            .collect()),
        other => Err(format!("TIFF type {other} is not an unsigned integer")),
    }
}

/// Decode and develop one DNG/CinemaDNG frame into scene-linear Rec.709 RGBA32F.
pub(crate) fn decode_camera_raw_frame(
    path: &Path,
    video_stream_index: Option<u32>,
    max_width: Option<u32>,
    max_height: Option<u32>,
    intent: CameraRawDecodeIntent,
    should_cancel: &dyn Fn() -> bool,
) -> Result<FloatRgbaFrame> {
    if intent.algorithm_version() != CameraRawDecodeIntent::ALGORITHM_VERSION {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "unsupported camera RAW algorithm version {}",
                intent.algorithm_version()
            ),
        });
    }
    let interpretation = intent.interpretation();
    interpretation.validate().map_err(|error| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let parsed = parse_dng(path)?.ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "camera RAW execution intent selected a non-DNG source".to_owned(),
    })?;
    if parsed.metadata.adapter != intent.adapter() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "camera RAW Adapter mismatch: probe={:?} execution={:?}",
                parsed.metadata.adapter,
                intent.adapter()
            ),
        });
    }
    if !parsed.metadata.has_color_matrix || !parsed.metadata.has_as_shot_neutral {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "DNG execution requires ColorMatrix and AsShotNeutral metadata".to_owned(),
        });
    }
    if should_cancel() {
        return Err(MondrianError::Cancelled);
    }
    let started_at = Instant::now();
    let mut input = ffmpeg::format::input(path).map_err(|error| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let stream = video_stream_index
        .and_then(|index| input.stream(usize::try_from(index).ok()?))
        .or_else(|| input.streams().best(ffmpeg::media::Type::Video))
        .ok_or_else(|| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "DNG source has no decodable picture stream".to_owned(),
        })?;
    let stream_index = stream.index();
    let context =
        ffmpeg::codec::context::Context::from_parameters(stream.parameters()).map_err(|error| {
            MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: error.to_string(),
            }
        })?;
    let mut decoder = context.decoder().video().map_err(|error| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let mut decoded = ffmpeg::util::frame::video::Video::empty();
    let mut got_frame = false;
    for (packet_stream, packet) in input.packets() {
        if should_cancel() {
            return Err(MondrianError::Cancelled);
        }
        if packet_stream.index() != stream_index {
            continue;
        }
        decoder.send_packet(&packet).map_err(|error| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: error.to_string(),
        })?;
        if decoder.receive_frame(&mut decoded).is_ok() {
            got_frame = true;
            break;
        }
    }
    if !got_frame {
        decoder.send_eof().map_err(|error| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: error.to_string(),
        })?;
        decoder
            .receive_frame(&mut decoded)
            .map_err(|error| MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!("DNG decoder produced no Bayer frame: {error}"),
            })?;
    }
    let pattern =
        cfa_pattern_from_pixel(decoded.format()).ok_or_else(|| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "DNG decoder returned non-Bayer pixel format {:?}",
                decoded.format()
            ),
        })?;
    if pattern != parsed.metadata.cfa_pattern {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "DNG CFA probe/decode mismatch: probe={:?} decoder={pattern:?}",
                parsed.metadata.cfa_pattern
            ),
        });
    }
    let mosaic = unpack_bayer(&decoded, path)?;
    let mut rgba = demosaic(
        &mosaic,
        decoded.width(),
        decoded.height(),
        pattern,
        interpretation.debayer_quality,
    );
    develop_camera_rgb(&mut rgba, &parsed, interpretation, path)?;
    let target = bounded_target_extent(
        Resolution { width: decoded.width(), height: decoded.height() },
        max_width,
        max_height,
    );
    let rgba = resize_float_rgba(
        &rgba,
        decoded.width(),
        decoded.height(),
        target.width,
        target.height,
    );
    let source = PreviewSourceColorContract::new(
        ColorSpace::LinearRec709,
        DecodedVideoRangeContract::Automatic { probed_range: DecodedVideoRange::Full },
    );
    let mut frame = FloatRgbaFrame::new(
        target.width,
        target.height,
        rgba,
        DecodedRgbaFrameContract::source_linear(source),
        PreviewDecodePath::InProcessCameraRawDng,
    );
    frame.diagnostics.elapsed_us =
        started_at.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    Ok(frame)
}

fn bounded_target_extent(
    source: Resolution,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> Resolution {
    let width_scale = max_width
        .filter(|value| *value > 0)
        .map(|value| value as f64 / source.width as f64)
        .unwrap_or(1.0);
    let height_scale = max_height
        .filter(|value| *value > 0)
        .map(|value| value as f64 / source.height as f64)
        .unwrap_or(1.0);
    let scale = width_scale.min(height_scale).min(1.0);
    Resolution {
        width: ((source.width as f64 * scale).round() as u32).max(1),
        height: ((source.height as f64 * scale).round() as u32).max(1),
    }
}

fn cfa_pattern_from_pixel(
    pixel: ffmpeg::util::format::pixel::Pixel,
) -> Option<CameraRawCfaPattern> {
    use ffmpeg::util::format::pixel::Pixel;
    match pixel {
        Pixel::BAYER_RGGB8 | Pixel::BAYER_RGGB16LE => Some(CameraRawCfaPattern::Rggb),
        Pixel::BAYER_BGGR8 | Pixel::BAYER_BGGR16LE => Some(CameraRawCfaPattern::Bggr),
        Pixel::BAYER_GBRG8 | Pixel::BAYER_GBRG16LE => Some(CameraRawCfaPattern::Gbrg),
        Pixel::BAYER_GRBG8 | Pixel::BAYER_GRBG16LE => Some(CameraRawCfaPattern::Grbg),
        _ => None,
    }
}

fn unpack_bayer(decoded: &ffmpeg::util::frame::video::Video, path: &Path) -> Result<Vec<f32>> {
    use ffmpeg::util::format::pixel::Pixel;
    let width = decoded.width() as usize;
    let height = decoded.height() as usize;
    let stride = decoded.stride(0);
    let data = decoded.data(0);
    let mut output = Vec::with_capacity(width.saturating_mul(height));
    match decoded.format() {
        Pixel::BAYER_RGGB8 | Pixel::BAYER_BGGR8 | Pixel::BAYER_GBRG8 | Pixel::BAYER_GRBG8 => {
            for y in 0..height {
                let row = data.get(y * stride..y * stride + width).ok_or_else(|| {
                    MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!("DNG Bayer row {y} is shorter than declared"),
                    }
                })?;
                output.extend(row.iter().map(|&sample| f32::from(sample) / 255.0));
            }
        }
        Pixel::BAYER_RGGB16LE
        | Pixel::BAYER_BGGR16LE
        | Pixel::BAYER_GBRG16LE
        | Pixel::BAYER_GRBG16LE => {
            for y in 0..height {
                let row = data.get(y * stride..y * stride + width * 2).ok_or_else(|| {
                    MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!("DNG Bayer16 row {y} is shorter than declared"),
                    }
                })?;
                output.extend(row.chunks_exact(2).map(|sample| {
                    f32::from(u16::from_le_bytes([sample[0], sample[1]])) / 65_535.0
                }));
            }
        }
        _ => unreachable!("Bayer pixel format was validated"),
    }
    Ok(output)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CfaColor {
    Red,
    Green,
    Blue,
}

fn cfa_color(pattern: CameraRawCfaPattern, x: usize, y: usize) -> CfaColor {
    let index = (y & 1) * 2 + (x & 1);
    let pattern = match pattern {
        CameraRawCfaPattern::Rggb => [
            CfaColor::Red,
            CfaColor::Green,
            CfaColor::Green,
            CfaColor::Blue,
        ],
        CameraRawCfaPattern::Bggr => [
            CfaColor::Blue,
            CfaColor::Green,
            CfaColor::Green,
            CfaColor::Red,
        ],
        CameraRawCfaPattern::Gbrg => [
            CfaColor::Green,
            CfaColor::Blue,
            CfaColor::Red,
            CfaColor::Green,
        ],
        CameraRawCfaPattern::Grbg => [
            CfaColor::Green,
            CfaColor::Red,
            CfaColor::Blue,
            CfaColor::Green,
        ],
    };
    pattern[index]
}

fn demosaic(
    mosaic: &[f32],
    width: u32,
    height: u32,
    pattern: CameraRawCfaPattern,
    quality: CameraRawDebayerQuality,
) -> Vec<f32> {
    let width = width as usize;
    let height = height as usize;
    let mut rgba = vec![0.0; width.saturating_mul(height).saturating_mul(4)];
    for y in 0..height {
        for x in 0..width {
            let center_color = cfa_color(pattern, x, y);
            let center = mosaic[y * width + x];
            let mut rgb = [0.0; 3];
            for (channel, wanted) in
                [CfaColor::Red, CfaColor::Green, CfaColor::Blue].into_iter().enumerate()
            {
                rgb[channel] = if wanted == center_color {
                    center
                } else if wanted == CfaColor::Green
                    && quality == CameraRawDebayerQuality::EdgeAware
                    && center_color != CfaColor::Green
                {
                    edge_aware_green(mosaic, width, height, x, y, pattern)
                } else {
                    neighboring_color_average(mosaic, width, height, x, y, pattern, wanted)
                };
            }
            let offset = (y * width + x) * 4;
            rgba[offset..offset + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }
    }
    rgba
}

fn neighboring_color_average(
    mosaic: &[f32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    pattern: CameraRawCfaPattern,
    wanted: CfaColor,
) -> f32 {
    let mut sum = 0.0;
    let mut count = 0_u32;
    for dy in -1_isize..=1 {
        for dx in -1_isize..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x.saturating_add_signed(dx).min(width.saturating_sub(1));
            let ny = y.saturating_add_signed(dy).min(height.saturating_sub(1));
            if cfa_color(pattern, nx, ny) == wanted {
                sum += mosaic[ny * width + nx];
                count += 1;
            }
        }
    }
    if count == 0 {
        mosaic[y * width + x]
    } else {
        sum / count as f32
    }
}

fn edge_aware_green(
    mosaic: &[f32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    pattern: CameraRawCfaPattern,
) -> f32 {
    let sample = |x: usize, y: usize| mosaic[y * width + x];
    let left = x.saturating_sub(1);
    let right = (x + 1).min(width.saturating_sub(1));
    let top = y.saturating_sub(1);
    let bottom = (y + 1).min(height.saturating_sub(1));
    let horizontal = if cfa_color(pattern, left, y) == CfaColor::Green
        && cfa_color(pattern, right, y) == CfaColor::Green
    {
        Some((sample(left, y), sample(right, y)))
    } else {
        None
    };
    let vertical = if cfa_color(pattern, x, top) == CfaColor::Green
        && cfa_color(pattern, x, bottom) == CfaColor::Green
    {
        Some((sample(x, top), sample(x, bottom)))
    } else {
        None
    };
    match (horizontal, vertical) {
        (Some((left, right)), Some((top, bottom))) => {
            if (left - right).abs() <= (top - bottom).abs() {
                (left + right) * 0.5
            } else {
                (top + bottom) * 0.5
            }
        }
        (Some((left, right)), None) => (left + right) * 0.5,
        (None, Some((top, bottom))) => (top + bottom) * 0.5,
        (None, None) => sample(x, y),
    }
}

fn develop_camera_rgb(
    rgba: &mut [f32],
    parsed: &ParsedDng,
    interpretation: CameraRawInterpretation,
    path: &Path,
) -> Result<()> {
    let color_matrix = parsed.color_matrix.ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "DNG ColorMatrix is unavailable".to_owned(),
    })?;
    let camera_to_xyz = invert_3x3(color_matrix).ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "DNG ColorMatrix is singular".to_owned(),
    })?;
    let neutral = match interpretation.white_balance {
        CameraRawWhiteBalance::CameraMetadata => {
            parsed.as_shot_neutral.ok_or_else(|| MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "DNG AsShotNeutral is unavailable".to_owned(),
            })?
        }
        CameraRawWhiteBalance::TemperatureTint { temperature_kelvin, tint_milli } => {
            let xyz = temperature_xyz(f64::from(temperature_kelvin));
            let mut neutral = multiply_3x3_vector(color_matrix, xyz);
            neutral[1] /= 2.0_f64.powf(f64::from(tint_milli) / 1_000.0);
            neutral
        }
    };
    if neutral.iter().any(|value| !value.is_finite() || *value <= 0.0) {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "DNG white-balance neutral is non-positive or non-finite".to_owned(),
        });
    }
    let mut gains = [1.0 / neutral[0], 1.0 / neutral[1], 1.0 / neutral[2]];
    let green = gains[1];
    for gain in &mut gains {
        *gain /= green;
    }
    let exposure = 2.0_f64.powf(f64::from(interpretation.exposure_millistops) / 1_000.0);
    for pixel in rgba.chunks_exact_mut(4) {
        let camera = [
            f64::from(pixel[0]) * gains[0],
            f64::from(pixel[1]) * gains[1],
            f64::from(pixel[2]) * gains[2],
        ];
        let xyz_d50 = multiply_3x3_vector(camera_to_xyz, camera);
        let xyz_d65 = multiply_3x3_vector(BRADFORD_D50_TO_D65, xyz_d50);
        let rgb = multiply_3x3_vector(XYZ_D65_TO_REC709, xyz_d65);
        pixel[0] = (rgb[0] * exposure) as f32;
        pixel[1] = (rgb[1] * exposure) as f32;
        pixel[2] = (rgb[2] * exposure) as f32;
    }
    Ok(())
}

const BRADFORD_D50_TO_D65: [[f64; 3]; 3] = [
    [0.955_576_6, -0.023_039_3, 0.063_163_6],
    [-0.028_289_5, 1.009_941_6, 0.021_007_7],
    [0.012_298_2, -0.020_483, 1.329_909_8],
];

const XYZ_D65_TO_REC709: [[f64; 3]; 3] = [
    [3.240_454_2, -1.537_138_5, -0.498_531_4],
    [-0.969_266, 1.876_010_8, 0.041_556],
    [0.055_643_4, -0.204_025_9, 1.057_225_2],
];

fn multiply_3x3_vector(matrix: [[f64; 3]; 3], value: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
        matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
        matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
    ]
}

fn invert_3x3(matrix: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let determinant = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    if !determinant.is_finite() || determinant.abs() < 1.0e-12 {
        return None;
    }
    let inverse = 1.0 / determinant;
    Some([
        [
            (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1]) * inverse,
            (matrix[0][2] * matrix[2][1] - matrix[0][1] * matrix[2][2]) * inverse,
            (matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1]) * inverse,
        ],
        [
            (matrix[1][2] * matrix[2][0] - matrix[1][0] * matrix[2][2]) * inverse,
            (matrix[0][0] * matrix[2][2] - matrix[0][2] * matrix[2][0]) * inverse,
            (matrix[0][2] * matrix[1][0] - matrix[0][0] * matrix[1][2]) * inverse,
        ],
        [
            (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]) * inverse,
            (matrix[0][1] * matrix[2][0] - matrix[0][0] * matrix[2][1]) * inverse,
            (matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0]) * inverse,
        ],
    ])
}

fn temperature_xyz(kelvin: f64) -> [f64; 3] {
    let temperature = kelvin.clamp(1_667.0, 25_000.0);
    let x = if temperature <= 4_000.0 {
        -0.266_123_9e9 / temperature.powi(3) - 0.234_358e6 / temperature.powi(2)
            + 0.877_695_6e3 / temperature
            + 0.179_91
    } else {
        -3.025_846_9e9 / temperature.powi(3)
            + 2.107_037_9e6 / temperature.powi(2)
            + 0.222_634_7e3 / temperature
            + 0.240_39
    };
    let y = if temperature <= 2_222.0 {
        -1.106_381_4 * x.powi(3) - 1.348_110_2 * x.powi(2) + 2.185_558_32 * x - 0.202_196_83
    } else if temperature <= 4_000.0 {
        -0.954_947_6 * x.powi(3) - 1.374_185_93 * x.powi(2) + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758 * x.powi(3) - 5.873_386_7 * x.powi(2) + 3.751_129_97 * x - 0.370_014_83
    };
    [x / y, 1.0, (1.0 - x - y) / y]
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_DNG_WIDTH: u32 = 8;
    const TEST_DNG_HEIGHT: u32 = 8;

    fn synthetic_uncompressed_dng() -> Vec<u8> {
        const ENTRY_COUNT: usize = 24;
        const IFD_OFFSET: u32 = 8;
        const IFD_BYTES: usize = 2 + ENTRY_COUNT * 12 + 4;
        let payload_base = usize::try_from(IFD_OFFSET).expect("IFD offset") + IFD_BYTES;
        let mut payload = Vec::new();
        let mut append_payload = |bytes: &[u8]| {
            if payload.len() % 2 != 0 {
                payload.push(0);
            }
            let offset = payload_base + payload.len();
            payload.extend_from_slice(bytes);
            u32::try_from(offset).expect("small synthetic DNG offset")
        };

        let make = b"Mondrian\0";
        let model = b"Synthetic DNG\0";
        let make_offset = append_payload(make);
        let model_offset = append_payload(model);
        let mut matrix = Vec::with_capacity(9 * 8);
        for value in [1_i32, 0, 0, 0, 1, 0, 0, 0, 1] {
            matrix.extend_from_slice(&value.to_le_bytes());
            matrix.extend_from_slice(&1_i32.to_le_bytes());
        }
        let matrix_offset = append_payload(&matrix);
        let mut neutral = Vec::with_capacity(3 * 8);
        for _ in 0..3 {
            neutral.extend_from_slice(&1_u32.to_le_bytes());
            neutral.extend_from_slice(&1_u32.to_le_bytes());
        }
        let neutral_offset = append_payload(&neutral);
        if payload.len() % 2 != 0 {
            payload.push(0);
        }
        let strip_offset = u32::try_from(payload_base + payload.len()).expect("pixel offset");
        let strip_bytes = TEST_DNG_WIDTH * TEST_DNG_HEIGHT * 2;

        let short = |value: u16| {
            let [a, b] = value.to_le_bytes();
            [a, b, 0, 0]
        };
        let long = |value: u32| value.to_le_bytes();
        let mut entries = vec![
            (254_u16, 4_u16, 1_u32, long(0)),
            (TAG_IMAGE_WIDTH, 4, 1, long(TEST_DNG_WIDTH)),
            (TAG_IMAGE_HEIGHT, 4, 1, long(TEST_DNG_HEIGHT)),
            (TAG_BITS_PER_SAMPLE, 3, 1, short(16)),
            (TAG_COMPRESSION, 3, 1, short(1)),
            (262, 3, 1, short(32_803)),
            (
                TAG_MAKE,
                2,
                u32::try_from(make.len()).expect("make length"),
                long(make_offset),
            ),
            (
                TAG_MODEL,
                2,
                u32::try_from(model.len()).expect("model length"),
                long(model_offset),
            ),
            (273, 4, 1, long(strip_offset)),
            (274, 3, 1, short(1)),
            (277, 3, 1, short(1)),
            (278, 4, 1, long(TEST_DNG_HEIGHT)),
            (279, 4, 1, long(strip_bytes)),
            (284, 3, 1, short(1)),
            (TAG_CFA_REPEAT_PATTERN_DIM, 3, 2, [2, 0, 2, 0]),
            (TAG_CFA_PATTERN, 1, 4, [0, 1, 1, 2]),
            (TAG_DNG_VERSION, 1, 4, [1, 4, 0, 0]),
            (50_707, 1, 4, [1, 1, 0, 0]),
            (
                50_708,
                2,
                u32::try_from(model.len()).expect("model length"),
                long(model_offset),
            ),
            (50_714, 3, 1, short(0)),
            (50_717, 4, 1, long(65_535)),
            (TAG_COLOR_MATRIX_1, 10, 9, long(matrix_offset)),
            (TAG_AS_SHOT_NEUTRAL, 5, 3, long(neutral_offset)),
            (50_778, 3, 1, short(21)),
        ];
        assert_eq!(entries.len(), ENTRY_COUNT);
        entries.sort_unstable_by_key(|entry| entry.0);

        let mut dng =
            Vec::with_capacity(usize::try_from(strip_offset + strip_bytes).expect("size"));
        dng.extend_from_slice(b"II");
        dng.extend_from_slice(&42_u16.to_le_bytes());
        dng.extend_from_slice(&IFD_OFFSET.to_le_bytes());
        dng.extend_from_slice(&u16::try_from(entries.len()).expect("entry count").to_le_bytes());
        for (tag, field_type, count, value) in entries {
            dng.extend_from_slice(&tag.to_le_bytes());
            dng.extend_from_slice(&field_type.to_le_bytes());
            dng.extend_from_slice(&count.to_le_bytes());
            dng.extend_from_slice(&value);
        }
        dng.extend_from_slice(&0_u32.to_le_bytes());
        dng.extend_from_slice(&payload);
        assert_eq!(
            dng.len(),
            usize::try_from(strip_offset).expect("strip offset")
        );
        for y in 0..TEST_DNG_HEIGHT {
            for x in 0..TEST_DNG_WIDTH {
                let sample = match cfa_color(CameraRawCfaPattern::Rggb, x as usize, y as usize) {
                    CfaColor::Red => 12_000_u16,
                    CfaColor::Green => 16_000,
                    CfaColor::Blue => 20_000,
                };
                dng.extend_from_slice(&sample.to_le_bytes());
            }
        }
        dng
    }

    #[test]
    fn matrix_inverse_round_trips_a_vector() {
        let matrix = [[0.7, 0.2, 0.1], [0.1, 0.8, 0.1], [0.0, 0.2, 0.8]];
        let inverse = invert_3x3(matrix).expect("invertible matrix");
        let source = [0.3, 0.5, 0.8];
        let round_trip = multiply_3x3_vector(inverse, multiply_3x3_vector(matrix, source));
        for (actual, expected) in round_trip.into_iter().zip(source) {
            assert!((actual - expected).abs() < 1.0e-10);
        }
    }

    #[test]
    fn debayer_uniform_cfa_preserves_uniform_channels() {
        let mosaic = vec![0.25; 16];
        let rgba = demosaic(
            &mosaic,
            4,
            4,
            CameraRawCfaPattern::Rggb,
            CameraRawDebayerQuality::EdgeAware,
        );
        assert!(rgba.chunks_exact(4).all(|pixel| {
            (pixel[0] - 0.25).abs() < f32::EPSILON
                && (pixel[1] - 0.25).abs() < f32::EPSILON
                && (pixel[2] - 0.25).abs() < f32::EPSILON
                && pixel[3] == 1.0
        }));
    }

    #[test]
    fn temperature_white_points_are_finite() {
        for temperature in [2_000.0, 3_200.0, 5_600.0, 10_000.0, 25_000.0] {
            assert!(temperature_xyz(temperature).into_iter().all(f64::is_finite));
        }
    }

    #[test]
    fn synthetic_dng_probe_and_development_are_typed_linear_and_exposure_exact() {
        ffmpeg::init().expect("initialize FFmpeg");
        let root = tempfile::tempdir().expect("DNG tempdir");
        let path = root.path().join("synthetic.dng");
        std::fs::write(&path, synthetic_uncompressed_dng()).expect("write synthetic DNG");

        let metadata = probe_camera_raw_metadata(&path)
            .expect("probe DNG")
            .expect("camera RAW metadata");
        assert_eq!(metadata.adapter, CameraRawAdapter::Dng);
        assert_eq!(metadata.cfa_pattern, CameraRawCfaPattern::Rggb);
        assert_eq!(
            (metadata.width, metadata.height),
            (TEST_DNG_WIDTH, TEST_DNG_HEIGHT)
        );
        assert_eq!(metadata.bit_depth, 16);
        assert!(metadata.has_color_matrix);
        assert!(metadata.has_as_shot_neutral);
        let probe =
            crate::info::probe_media_info(&path).expect("probe complete DNG media contract");
        let video = probe.primary_video().expect("DNG picture stream");
        assert_eq!(video.total_frames, Some(1));
        assert_eq!(
            video.pixel_format,
            mondrian_core::PixelFormat::BayerRggb16le
        );
        assert_eq!(
            video.executable_color_space(),
            Some(ColorSpace::LinearRec709)
        );
        assert_eq!(video.camera_raw.as_deref(), Some(&metadata));

        let base = decode_camera_raw_frame(
            &path,
            None,
            None,
            None,
            CameraRawDecodeIntent::new(CameraRawAdapter::Dng, CameraRawInterpretation::default())
                .expect("base RAW intent"),
            &|| false,
        )
        .expect("develop base DNG");
        let raised = decode_camera_raw_frame(
            &path,
            None,
            None,
            None,
            CameraRawDecodeIntent::new(
                CameraRawAdapter::Dng,
                CameraRawInterpretation {
                    exposure_millistops: 1_000,
                    ..CameraRawInterpretation::default()
                },
            )
            .expect("raised RAW intent"),
            &|| false,
        )
        .expect("develop raised DNG");
        assert_eq!((base.width, base.height), (TEST_DNG_WIDTH, TEST_DNG_HEIGHT));
        assert_eq!(
            base.diagnostics.path,
            PreviewDecodePath::InProcessCameraRawDng
        );
        assert_eq!(
            base.color_contract.encoding,
            crate::preview::DecodedRgbaEncoding::SourceLinearRgb
        );
        assert_eq!(
            base.color_contract.source.color_space(),
            Some(ColorSpace::LinearRec709)
        );
        assert!(base.rgba().iter().all(|sample| sample.is_finite()));
        assert!(base.rgba().iter().any(|sample| sample.abs() > 1.0e-6));
        for (base, raised) in base.rgba().chunks_exact(4).zip(raised.rgba().chunks_exact(4)) {
            for channel in 0..3 {
                assert!((raised[channel] - base[channel] * 2.0).abs() < 2.0e-5);
            }
            assert_eq!(raised[3], 1.0);
        }
    }
}
