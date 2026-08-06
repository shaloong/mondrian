//! Versioned, bounded IPC contract for the Preview demux worker.
//!
//! The helper process owns every `AVFormatContext` operation. The parent
//! receives only a validated video stream contract and owned compressed
//! packets, so an unresponsive demuxer can be terminated without abandoning a
//! thread or an in-flight Frame Work Broker lease.

use super::demux_protocol_ffi::{
    decode_chroma_location, decode_color_primaries, decode_color_range, decode_color_space,
    decode_color_trc, decode_field_order, decode_side_data_type,
};
use super::MediaFileFingerprint;
use ffmpeg::codec::packet::{Mut as PacketMut, Ref as PacketRef};
use ffmpeg_next as ffmpeg;
use mondrian_core::{MediaFileChangeStamp, MediaFileObjectIdentity};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::ptr;
use std::slice;

const PROTOCOL_MAGIC: [u8; 8] = *b"MDPDMX04";
const REQUEST_MAGIC: [u8; 8] = *b"MDPDMXR4";
pub(super) const PROTOCOL_VERSION: u32 = 4;
const BUILD_IDENTITY: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

const MESSAGE_OPEN_PHASE: u8 = 1;
const MESSAGE_STREAM: u8 = 2;
const MESSAGE_SEEK_COMPLETE: u8 = 3;
const MESSAGE_PACKET: u8 = 4;
const MESSAGE_END: u8 = 5;
const MESSAGE_CLOSED: u8 = 6;
const MESSAGE_ERROR: u8 = 7;

const OPEN_PHASE_INPUT_OPEN: u8 = 1;
const OPEN_PHASE_STREAM_INFO: u8 = 2;

const COMMAND_SEEK: u8 = 1;
const COMMAND_READ: u8 = 2;
const COMMAND_CLOSE: u8 = 3;

const MAX_CODEC_NAME_BYTES: usize = 128;
const MAX_EXTRADATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;
const MAX_SIDE_DATA_ENTRIES: usize = 64;
const MAX_SIDE_DATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_SIDE_DATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 64 * 1024;
const MAX_BUILD_IDENTITY_BYTES: usize = 256;
pub(super) const MAX_KEYFRAME_ANCHORS: usize = 262_144;

#[derive(Debug)]
pub(super) struct DemuxWorkerRequest {
    pub nonce: [u8; 16],
    pub path: PathBuf,
    pub source_revision: MediaFileFingerprint,
    pub video_stream_index: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DemuxOpenPhase {
    InputOpen,
    StreamInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DemuxWorkerCommand {
    Seek {
        command_id: u64,
        min_ts: i64,
        target_ts: i64,
        max_ts: i64,
        flags: i32,
    },
    Read {
        command_id: u64,
    },
    Close {
        command_id: u64,
    },
}

impl DemuxWorkerCommand {
    pub(super) const fn command_id(self) -> u64 {
        match self {
            Self::Seek { command_id, .. }
            | Self::Read { command_id }
            | Self::Close { command_id } => command_id,
        }
    }
}

pub(super) enum DemuxProtocolMessage {
    OpenPhase(DemuxOpenPhase),
    Stream(DemuxStreamContract),
    SeekComplete { command_id: u64 },
    Packet(DemuxPacket),
    End { command_id: u64 },
    Closed { command_id: u64 },
    Error { command_id: u64, message: String },
}

pub(super) struct DemuxStreamContract {
    pub parameters: ffmpeg::codec::Parameters,
    pub stream_index: usize,
    pub time_base: ffmpeg::Rational,
    pub start_pts: i64,
    pub frame_rate: ffmpeg::Rational,
    pub keyframe_pts: Vec<i64>,
    pub keyframe_index_truncated: bool,
}

pub(super) struct DemuxPacket {
    pub command_id: u64,
    pub packet: ffmpeg::Packet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WireSideData {
    kind: i32,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WireCodecParameters {
    codec_id: i32,
    codec_name: String,
    codec_tag: u32,
    extradata: Vec<u8>,
    coded_side_data: Vec<WireSideData>,
    format: i32,
    bit_rate: i64,
    bits_per_coded_sample: i32,
    bits_per_raw_sample: i32,
    profile: i32,
    level: i32,
    width: i32,
    height: i32,
    sample_aspect_ratio: (i32, i32),
    frame_rate: (i32, i32),
    field_order: i32,
    color_range: i32,
    color_primaries: i32,
    color_trc: i32,
    color_space: i32,
    chroma_location: i32,
    video_delay: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WirePacket {
    pts: i64,
    dts: i64,
    stream_index: i32,
    flags: i32,
    duration: i64,
    position: i64,
    time_base: (i32, i32),
    side_data: Vec<WireSideData>,
    bytes: Vec<u8>,
}

pub(super) fn write_worker_request(
    writer: &mut impl Write,
    nonce: [u8; 16],
    path: &Path,
    source_revision: MediaFileFingerprint,
    video_stream_index: Option<u32>,
) -> io::Result<()> {
    validate_media_file_revision(source_revision)?;
    writer.write_all(&REQUEST_MAGIC)?;
    write_runtime_contract(writer, nonce)?;
    let (encoding, path_bytes) = encode_native_path(path.as_os_str())?;
    writer.write_all(&[encoding])?;
    write_bounded_bytes(writer, &path_bytes, MAX_PATH_BYTES, "media path")?;
    write_optional_u64(writer, source_revision.len)?;
    write_optional_u64(writer, source_revision.modified_secs)?;
    write_optional_u32(writer, source_revision.modified_nanos)?;
    write_media_file_object_identity(writer, source_revision.object_identity)?;
    write_media_file_change_stamp(writer, source_revision.change_stamp)?;
    write_optional_u32(writer, video_stream_index)
}

pub(super) fn read_worker_request(reader: &mut impl Read) -> io::Result<DemuxWorkerRequest> {
    let mut magic = [0_u8; REQUEST_MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if magic != REQUEST_MAGIC {
        return Err(invalid_data("Preview demux worker request magic mismatch"));
    }
    let nonce = read_runtime_contract(reader)?;
    let mut encoding = [0_u8; 1];
    reader.read_exact(&mut encoding)?;
    let path = decode_native_path(
        encoding[0],
        read_bounded_bytes(reader, MAX_PATH_BYTES, "media path")?,
    )?;
    let source_revision = MediaFileFingerprint {
        len: read_optional_u64(reader, "source byte length")?,
        modified_secs: read_optional_u64(reader, "source modified seconds")?,
        modified_nanos: read_optional_u32(reader, "source modified nanoseconds")?,
        object_identity: read_media_file_object_identity(reader)?,
        change_stamp: read_media_file_change_stamp(reader)?,
    };
    if source_revision.modified_nanos.is_some_and(|nanos| nanos >= 1_000_000_000) {
        return Err(invalid_data(
            "source modified nanoseconds must be below one second",
        ));
    }
    validate_media_file_revision(source_revision)?;
    let video_stream_index = read_optional_u32(reader, "video stream index")?;
    Ok(DemuxWorkerRequest { nonce, path, source_revision, video_stream_index })
}

fn validate_media_file_revision(source_revision: MediaFileFingerprint) -> io::Result<()> {
    if !source_revision.authorizes_reuse() {
        return Err(invalid_data(
            "Preview demux request requires complete filesystem revision evidence",
        ));
    }
    if let Some(MediaFileChangeStamp::Unix { nanoseconds, .. }) = source_revision.change_stamp
        && !(0..1_000_000_000).contains(&nanoseconds)
    {
        return Err(invalid_data(
            "Unix media change nanoseconds must be below one second",
        ));
    }
    Ok(())
}

fn write_media_file_object_identity(
    writer: &mut impl Write,
    identity: Option<MediaFileObjectIdentity>,
) -> io::Result<()> {
    match identity {
        None => writer.write_all(&[0]),
        Some(MediaFileObjectIdentity::Windows { volume_serial_number, file_id }) => {
            writer.write_all(&[1])?;
            write_u64(writer, volume_serial_number)?;
            writer.write_all(&file_id)
        }
        Some(MediaFileObjectIdentity::Unix { device, inode }) => {
            writer.write_all(&[2])?;
            write_u64(writer, device)?;
            write_u64(writer, inode)
        }
    }
}

fn read_media_file_object_identity(
    reader: &mut impl Read,
) -> io::Result<Option<MediaFileObjectIdentity>> {
    let mut kind = [0_u8; 1];
    reader.read_exact(&mut kind)?;
    match kind[0] {
        0 => Ok(None),
        1 => {
            let volume_serial_number = read_u64(reader)?;
            let mut file_id = [0_u8; 16];
            reader.read_exact(&mut file_id)?;
            Ok(Some(MediaFileObjectIdentity::Windows {
                volume_serial_number,
                file_id,
            }))
        }
        2 => Ok(Some(MediaFileObjectIdentity::Unix {
            device: read_u64(reader)?,
            inode: read_u64(reader)?,
        })),
        value => Err(invalid_data(format!(
            "unknown media file object identity kind {value}"
        ))),
    }
}

fn write_media_file_change_stamp(
    writer: &mut impl Write,
    stamp: Option<MediaFileChangeStamp>,
) -> io::Result<()> {
    match stamp {
        None => writer.write_all(&[0]),
        Some(MediaFileChangeStamp::WindowsFileTime(value)) => {
            writer.write_all(&[1])?;
            write_i64(writer, value)
        }
        Some(MediaFileChangeStamp::Unix { seconds, nanoseconds }) => {
            writer.write_all(&[2])?;
            write_i64(writer, seconds)?;
            write_i64(writer, nanoseconds)
        }
    }
}

fn read_media_file_change_stamp(
    reader: &mut impl Read,
) -> io::Result<Option<MediaFileChangeStamp>> {
    let mut kind = [0_u8; 1];
    reader.read_exact(&mut kind)?;
    match kind[0] {
        0 => Ok(None),
        1 => Ok(Some(MediaFileChangeStamp::WindowsFileTime(read_i64(
            reader,
        )?))),
        2 => Ok(Some(MediaFileChangeStamp::Unix {
            seconds: read_i64(reader)?,
            nanoseconds: read_i64(reader)?,
        })),
        value => Err(invalid_data(format!(
            "unknown media file change stamp kind {value}"
        ))),
    }
}

pub(super) fn write_worker_command(
    writer: &mut impl Write,
    command: DemuxWorkerCommand,
) -> io::Result<()> {
    match command {
        DemuxWorkerCommand::Seek { command_id, min_ts, target_ts, max_ts, flags } => {
            validate_command_id(command_id)?;
            validate_seek_command(min_ts, target_ts, max_ts, flags)?;
            writer.write_all(&[COMMAND_SEEK])?;
            write_u64(writer, command_id)?;
            write_i64(writer, min_ts)?;
            write_i64(writer, target_ts)?;
            write_i64(writer, max_ts)?;
            write_i32(writer, flags)
        }
        DemuxWorkerCommand::Read { command_id } => {
            validate_command_id(command_id)?;
            writer.write_all(&[COMMAND_READ])?;
            write_u64(writer, command_id)
        }
        DemuxWorkerCommand::Close { command_id } => {
            validate_command_id(command_id)?;
            writer.write_all(&[COMMAND_CLOSE])?;
            write_u64(writer, command_id)
        }
    }
}

pub(super) fn read_worker_command(reader: &mut impl Read) -> io::Result<DemuxWorkerCommand> {
    let mut kind = [0_u8; 1];
    reader.read_exact(&mut kind)?;
    let command_id = read_u64(reader)?;
    validate_command_id(command_id)?;
    match kind[0] {
        COMMAND_SEEK => {
            let min_ts = read_i64(reader)?;
            let target_ts = read_i64(reader)?;
            let max_ts = read_i64(reader)?;
            let flags = read_i32(reader)?;
            validate_seek_command(min_ts, target_ts, max_ts, flags)?;
            Ok(DemuxWorkerCommand::Seek { command_id, min_ts, target_ts, max_ts, flags })
        }
        COMMAND_READ => Ok(DemuxWorkerCommand::Read { command_id }),
        COMMAND_CLOSE => Ok(DemuxWorkerCommand::Close { command_id }),
        other => Err(invalid_data(format!(
            "unknown Preview demux worker command {other}"
        ))),
    }
}

pub(super) fn write_protocol_preamble(writer: &mut impl Write, nonce: [u8; 16]) -> io::Result<()> {
    writer.write_all(&PROTOCOL_MAGIC)?;
    write_runtime_contract(writer, nonce)
}

pub(super) fn read_protocol_preamble(
    reader: &mut impl Read,
    expected_nonce: [u8; 16],
) -> io::Result<()> {
    let mut magic = [0_u8; PROTOCOL_MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if magic != PROTOCOL_MAGIC {
        return Err(invalid_data("Preview demux worker protocol magic mismatch"));
    }
    let observed_nonce = read_runtime_contract(reader)?;
    if observed_nonce != expected_nonce {
        return Err(invalid_data("Preview demux worker launch nonce mismatch"));
    }
    Ok(())
}

fn write_runtime_contract(writer: &mut impl Write, nonce: [u8; 16]) -> io::Result<()> {
    write_u32(writer, PROTOCOL_VERSION)?;
    for version in runtime_library_versions() {
        write_u32(writer, version)?;
    }
    writer.write_all(&[usize::BITS as u8, u8::from(cfg!(target_endian = "little"))])?;
    write_bounded_bytes(
        writer,
        BUILD_IDENTITY.as_bytes(),
        MAX_BUILD_IDENTITY_BYTES,
        "build identity",
    )?;
    writer.write_all(&nonce)
}

fn read_runtime_contract(reader: &mut impl Read) -> io::Result<[u8; 16]> {
    let version = read_u32(reader)?;
    if version != PROTOCOL_VERSION {
        return Err(invalid_data(format!(
            "unsupported Preview demux protocol version {version}; expected {PROTOCOL_VERSION}"
        )));
    }
    let observed_versions = [read_u32(reader)?, read_u32(reader)?, read_u32(reader)?];
    let expected_versions = runtime_library_versions();
    if observed_versions != expected_versions {
        return Err(invalid_data(format!(
            "FFmpeg ABI mismatch: worker {observed_versions:?}, local {expected_versions:?}"
        )));
    }
    let mut platform = [0_u8; 2];
    reader.read_exact(&mut platform)?;
    if platform != [usize::BITS as u8, u8::from(cfg!(target_endian = "little"))] {
        return Err(invalid_data("Preview demux worker platform ABI mismatch"));
    }
    let build_identity = String::from_utf8(read_bounded_bytes(
        reader,
        MAX_BUILD_IDENTITY_BYTES,
        "build identity",
    )?)
    .map_err(|_| invalid_data("Preview demux build identity is not UTF-8"))?;
    if build_identity != BUILD_IDENTITY {
        return Err(invalid_data(format!(
            "Preview demux build mismatch: worker '{build_identity}', local '{BUILD_IDENTITY}'"
        )));
    }
    let mut nonce = [0_u8; 16];
    reader.read_exact(&mut nonce)?;
    Ok(nonce)
}

fn runtime_library_versions() -> [u32; 3] {
    // SAFETY: the version functions are process-global, argument-free runtime
    // queries and do not return borrowed state.
    unsafe {
        [
            ffmpeg::ffi::avcodec_version(),
            ffmpeg::ffi::avformat_version(),
            ffmpeg::ffi::avutil_version(),
        ]
    }
}

pub(super) fn write_stream_message(
    writer: &mut impl Write,
    parameters: &ffmpeg::codec::Parameters,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    start_pts: i64,
    frame_rate: ffmpeg::Rational,
    keyframe_pts: &[i64],
    keyframe_index_truncated: bool,
) -> io::Result<()> {
    writer.write_all(&[MESSAGE_STREAM])?;
    let wire = WireCodecParameters::from_ffmpeg(parameters)?;
    wire.write_to(writer)?;
    write_u32(writer, checked_u32(stream_index, "stream index")?)?;
    write_i32(writer, time_base.numerator())?;
    write_i32(writer, time_base.denominator())?;
    write_i64(writer, start_pts)?;
    write_i32(writer, frame_rate.numerator())?;
    write_i32(writer, frame_rate.denominator())?;
    write_keyframe_anchors(writer, keyframe_pts, keyframe_index_truncated)
}

pub(super) fn write_open_phase_message(
    writer: &mut impl Write,
    phase: DemuxOpenPhase,
) -> io::Result<()> {
    writer.write_all(&[
        MESSAGE_OPEN_PHASE,
        match phase {
            DemuxOpenPhase::InputOpen => OPEN_PHASE_INPUT_OPEN,
            DemuxOpenPhase::StreamInfo => OPEN_PHASE_STREAM_INFO,
        },
    ])
}

pub(super) fn write_seek_complete_message(
    writer: &mut impl Write,
    command_id: u64,
) -> io::Result<()> {
    validate_command_id(command_id)?;
    writer.write_all(&[MESSAGE_SEEK_COMPLETE])?;
    write_u64(writer, command_id)
}

pub(super) fn write_packet_message(
    writer: &mut impl Write,
    command_id: u64,
    packet: &ffmpeg::Packet,
) -> io::Result<()> {
    validate_command_id(command_id)?;
    writer.write_all(&[MESSAGE_PACKET])?;
    write_u64(writer, command_id)?;
    WirePacket::from_ffmpeg(packet)?.write_to(writer)
}

pub(super) fn write_end_message(writer: &mut impl Write, command_id: u64) -> io::Result<()> {
    validate_command_id(command_id)?;
    writer.write_all(&[MESSAGE_END])?;
    write_u64(writer, command_id)
}

pub(super) fn write_closed_message(writer: &mut impl Write, command_id: u64) -> io::Result<()> {
    validate_command_id(command_id)?;
    writer.write_all(&[MESSAGE_CLOSED])?;
    write_u64(writer, command_id)
}

pub(super) fn write_error_message(
    writer: &mut impl Write,
    command_id: u64,
    error: &str,
) -> io::Result<()> {
    writer.write_all(&[MESSAGE_ERROR])?;
    write_u64(writer, command_id)?;
    let mut end = error.len().min(MAX_ERROR_BYTES);
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    write_bounded_bytes(
        writer,
        &error.as_bytes()[..end],
        MAX_ERROR_BYTES,
        "worker error",
    )
}

pub(super) fn read_message(reader: &mut impl Read) -> io::Result<DemuxProtocolMessage> {
    let mut kind = [0_u8; 1];
    reader.read_exact(&mut kind)?;
    match kind[0] {
        MESSAGE_OPEN_PHASE => {
            let mut phase = [0_u8; 1];
            reader.read_exact(&mut phase)?;
            match phase[0] {
                OPEN_PHASE_INPUT_OPEN => {
                    Ok(DemuxProtocolMessage::OpenPhase(DemuxOpenPhase::InputOpen))
                }
                OPEN_PHASE_STREAM_INFO => {
                    Ok(DemuxProtocolMessage::OpenPhase(DemuxOpenPhase::StreamInfo))
                }
                other => Err(invalid_data(format!(
                    "unknown Preview demux open phase {other}"
                ))),
            }
        }
        MESSAGE_STREAM => {
            let parameters = WireCodecParameters::read_from(reader)?.into_ffmpeg()?;
            let stream_index = read_u32(reader)? as usize;
            let time_base = read_rational(reader, "stream time base")?;
            let start_pts = read_i64(reader)?;
            let frame_rate = read_rational(reader, "stream frame rate")?;
            let (keyframe_pts, keyframe_index_truncated) = read_keyframe_anchors(reader)?;
            Ok(DemuxProtocolMessage::Stream(DemuxStreamContract {
                parameters,
                stream_index,
                time_base,
                start_pts,
                frame_rate,
                keyframe_pts,
                keyframe_index_truncated,
            }))
        }
        MESSAGE_SEEK_COMPLETE => {
            let command_id = read_u64(reader)?;
            validate_command_id(command_id)?;
            Ok(DemuxProtocolMessage::SeekComplete { command_id })
        }
        MESSAGE_PACKET => Ok(DemuxProtocolMessage::Packet(DemuxPacket {
            command_id: {
                let command_id = read_u64(reader)?;
                validate_command_id(command_id)?;
                command_id
            },
            packet: WirePacket::read_from(reader)?.into_ffmpeg()?,
        })),
        MESSAGE_END => {
            let command_id = read_u64(reader)?;
            validate_command_id(command_id)?;
            Ok(DemuxProtocolMessage::End { command_id })
        }
        MESSAGE_CLOSED => {
            let command_id = read_u64(reader)?;
            validate_command_id(command_id)?;
            Ok(DemuxProtocolMessage::Closed { command_id })
        }
        MESSAGE_ERROR => {
            let command_id = read_u64(reader)?;
            let bytes = read_bounded_bytes(reader, MAX_ERROR_BYTES, "worker error")?;
            let message = String::from_utf8(bytes)
                .map_err(|_| invalid_data("Preview demux worker error is not UTF-8"))?;
            Ok(DemuxProtocolMessage::Error { command_id, message })
        }
        other => Err(invalid_data(format!(
            "unknown Preview demux protocol message {other}"
        ))),
    }
}

fn validate_command_id(command_id: u64) -> io::Result<()> {
    if command_id == 0 {
        return Err(invalid_data(
            "Preview demux command identifiers must be non-zero",
        ));
    }
    Ok(())
}

fn validate_seek_command(min_ts: i64, target_ts: i64, max_ts: i64, flags: i32) -> io::Result<()> {
    if min_ts > target_ts || target_ts > max_ts {
        return Err(invalid_data(
            "Preview demux seek window must satisfy min <= target <= max",
        ));
    }
    if flags != ffmpeg::ffi::AVSEEK_FLAG_BACKWARD && flags != ffmpeg::ffi::AVSEEK_FLAG_ANY {
        return Err(invalid_data(format!(
            "unsupported Preview demux seek flags {flags}"
        )));
    }
    Ok(())
}

fn write_keyframe_anchors(
    writer: &mut impl Write,
    keyframe_pts: &[i64],
    truncated: bool,
) -> io::Result<()> {
    if keyframe_pts.len() > MAX_KEYFRAME_ANCHORS {
        return Err(invalid_data(format!(
            "Preview demux keyframe index exceeds {MAX_KEYFRAME_ANCHORS} anchors"
        )));
    }
    if keyframe_pts.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_data(
            "Preview demux keyframe anchors must be strictly increasing",
        ));
    }
    write_u32(
        writer,
        checked_u32(keyframe_pts.len(), "keyframe anchor count")?,
    )?;
    writer.write_all(&[u8::from(truncated)])?;
    for &pts in keyframe_pts {
        write_i64(writer, pts)?;
    }
    Ok(())
}

fn read_keyframe_anchors(reader: &mut impl Read) -> io::Result<(Vec<i64>, bool)> {
    let count = read_u32(reader)? as usize;
    if count > MAX_KEYFRAME_ANCHORS {
        return Err(invalid_data(format!(
            "Preview demux keyframe index exceeds {MAX_KEYFRAME_ANCHORS} anchors"
        )));
    }
    let mut truncated = [0_u8; 1];
    reader.read_exact(&mut truncated)?;
    let truncated = match truncated[0] {
        0 => false,
        1 => true,
        other => {
            return Err(invalid_data(format!(
                "invalid Preview demux keyframe truncation flag {other}"
            )))
        }
    };
    let mut keyframe_pts = Vec::with_capacity(count);
    for _ in 0..count {
        let pts = read_i64(reader)?;
        if keyframe_pts.last().is_some_and(|previous| *previous >= pts) {
            return Err(invalid_data(
                "Preview demux keyframe anchors must be strictly increasing",
            ));
        }
        keyframe_pts.push(pts);
    }
    Ok((keyframe_pts, truncated))
}

impl WireCodecParameters {
    fn from_ffmpeg(parameters: &ffmpeg::codec::Parameters) -> io::Result<Self> {
        // SAFETY: `Parameters` owns a live AVCodecParameters for this borrow.
        let raw = unsafe { &*parameters.as_ptr() };
        if raw.codec_type != ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO {
            return Err(invalid_data(
                "Preview demux worker selected a non-video stream",
            ));
        }
        let codec_name = parameters.id().name().to_owned();
        if codec_name.len() > MAX_CODEC_NAME_BYTES {
            return Err(invalid_data(
                "codec name exceeds Preview demux protocol limit",
            ));
        }
        let extradata = copy_raw_bytes(
            raw.extradata,
            checked_nonnegative_usize(raw.extradata_size, "codec extradata")?,
            MAX_EXTRADATA_BYTES,
            "codec extradata",
        )?;
        let coded_side_data = copy_side_data(raw.coded_side_data, raw.nb_coded_side_data)?;
        Ok(Self {
            codec_id: raw.codec_id as i32,
            codec_name,
            codec_tag: raw.codec_tag,
            extradata,
            coded_side_data,
            format: raw.format,
            bit_rate: raw.bit_rate,
            bits_per_coded_sample: raw.bits_per_coded_sample,
            bits_per_raw_sample: raw.bits_per_raw_sample,
            profile: raw.profile,
            level: raw.level,
            width: raw.width,
            height: raw.height,
            sample_aspect_ratio: (raw.sample_aspect_ratio.num, raw.sample_aspect_ratio.den),
            frame_rate: (raw.framerate.num, raw.framerate.den),
            field_order: raw.field_order as i32,
            color_range: raw.color_range as i32,
            color_primaries: raw.color_primaries as i32,
            color_trc: raw.color_trc as i32,
            color_space: raw.color_space as i32,
            chroma_location: raw.chroma_location as i32,
            video_delay: raw.video_delay,
        })
    }

    fn into_ffmpeg(self) -> io::Result<ffmpeg::codec::Parameters> {
        let codec = ffmpeg::codec::decoder::find_by_name(&self.codec_name).ok_or_else(|| {
            invalid_data(format!(
                "Preview demux worker codec '{}' is unavailable in the parent runtime",
                self.codec_name
            ))
        })?;
        let resolved_codec_id: ffmpeg::ffi::AVCodecID = codec.id().into();
        if resolved_codec_id as i32 != self.codec_id {
            return Err(invalid_data(format!(
                "codec identity mismatch: numeric {}, descriptor '{}' resolves to {}",
                self.codec_id, self.codec_name, resolved_codec_id as i32
            )));
        }
        let mut parameters = ffmpeg::codec::Parameters::new();
        // SAFETY: `parameters` owns a freshly allocated AVCodecParameters. All
        // pointer fields are populated through FFmpeg allocators and therefore
        // remain owned by avcodec_parameters_free on every later path.
        unsafe {
            let raw = &mut *parameters.as_mut_ptr();
            raw.codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
            raw.codec_id = resolved_codec_id;
            raw.codec_tag = self.codec_tag;
            if !self.extradata.is_empty() {
                let allocation = self
                    .extradata
                    .len()
                    .checked_add(ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize)
                    .ok_or_else(|| invalid_data("codec extradata allocation overflow"))?;
                let data = ffmpeg::ffi::av_mallocz(allocation).cast::<u8>();
                if data.is_null() {
                    return Err(io::Error::other("allocate codec extradata"));
                }
                ptr::copy_nonoverlapping(self.extradata.as_ptr(), data, self.extradata.len());
                raw.extradata = data;
                raw.extradata_size = checked_i32(self.extradata.len(), "codec extradata")?;
            }
            install_side_data(
                &mut raw.coded_side_data,
                &mut raw.nb_coded_side_data,
                &self.coded_side_data,
            )?;
            raw.format = self.format;
            raw.bit_rate = self.bit_rate;
            raw.bits_per_coded_sample = self.bits_per_coded_sample;
            raw.bits_per_raw_sample = self.bits_per_raw_sample;
            raw.profile = self.profile;
            raw.level = self.level;
            raw.width = self.width;
            raw.height = self.height;
            raw.sample_aspect_ratio =
                rational_raw(self.sample_aspect_ratio, "sample aspect ratio")?;
            raw.framerate = rational_raw(self.frame_rate, "codec frame rate")?;
            raw.field_order = decode_field_order(self.field_order)?;
            raw.color_range = decode_color_range(self.color_range)?;
            raw.color_primaries = decode_color_primaries(self.color_primaries)?;
            raw.color_trc = decode_color_trc(self.color_trc)?;
            raw.color_space = decode_color_space(self.color_space)?;
            raw.chroma_location = decode_chroma_location(self.chroma_location)?;
            raw.video_delay = self.video_delay;
        }
        Ok(parameters)
    }

    fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        write_i32(writer, self.codec_id)?;
        write_bounded_bytes(
            writer,
            self.codec_name.as_bytes(),
            MAX_CODEC_NAME_BYTES,
            "codec name",
        )?;
        write_u32(writer, self.codec_tag)?;
        write_bounded_bytes(
            writer,
            &self.extradata,
            MAX_EXTRADATA_BYTES,
            "codec extradata",
        )?;
        write_side_data(writer, &self.coded_side_data)?;
        for value in [
            self.format,
            self.bits_per_coded_sample,
            self.bits_per_raw_sample,
            self.profile,
            self.level,
            self.width,
            self.height,
            self.sample_aspect_ratio.0,
            self.sample_aspect_ratio.1,
            self.frame_rate.0,
            self.frame_rate.1,
            self.field_order,
            self.color_range,
            self.color_primaries,
            self.color_trc,
            self.color_space,
            self.chroma_location,
            self.video_delay,
        ] {
            write_i32(writer, value)?;
        }
        write_i64(writer, self.bit_rate)
    }

    fn read_from(reader: &mut impl Read) -> io::Result<Self> {
        let codec_id = read_i32(reader)?;
        let codec_name = String::from_utf8(read_bounded_bytes(
            reader,
            MAX_CODEC_NAME_BYTES,
            "codec name",
        )?)
        .map_err(|_| invalid_data("codec name is not UTF-8"))?;
        let codec_tag = read_u32(reader)?;
        let extradata = read_bounded_bytes(reader, MAX_EXTRADATA_BYTES, "codec extradata")?;
        let coded_side_data = read_side_data(reader)?;
        let format = read_i32(reader)?;
        let bits_per_coded_sample = read_i32(reader)?;
        let bits_per_raw_sample = read_i32(reader)?;
        let profile = read_i32(reader)?;
        let level = read_i32(reader)?;
        let width = read_i32(reader)?;
        let height = read_i32(reader)?;
        let sample_aspect_ratio = (read_i32(reader)?, read_i32(reader)?);
        let frame_rate = (read_i32(reader)?, read_i32(reader)?);
        let field_order = read_i32(reader)?;
        let color_range = read_i32(reader)?;
        let color_primaries = read_i32(reader)?;
        let color_trc = read_i32(reader)?;
        let color_space = read_i32(reader)?;
        let chroma_location = read_i32(reader)?;
        let video_delay = read_i32(reader)?;
        let bit_rate = read_i64(reader)?;
        Ok(Self {
            codec_id,
            codec_name,
            codec_tag,
            extradata,
            coded_side_data,
            format,
            bit_rate,
            bits_per_coded_sample,
            bits_per_raw_sample,
            profile,
            level,
            width,
            height,
            sample_aspect_ratio,
            frame_rate,
            field_order,
            color_range,
            color_primaries,
            color_trc,
            color_space,
            chroma_location,
            video_delay,
        })
    }
}

impl WirePacket {
    fn from_ffmpeg(packet: &ffmpeg::Packet) -> io::Result<Self> {
        // SAFETY: Packet owns a live AVPacket for this borrow.
        let raw = unsafe { &*packet.as_ptr() };
        if raw.flags & ffmpeg::ffi::AV_PKT_FLAG_TRUSTED != 0 {
            return Err(invalid_data(
                "process-local AV_PKT_FLAG_TRUSTED packet cannot cross the demux boundary",
            ));
        }
        let bytes = packet.data().unwrap_or_default().to_vec();
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(invalid_data(
                "compressed packet exceeds Preview demux limit",
            ));
        }
        Ok(Self {
            pts: raw.pts,
            dts: raw.dts,
            stream_index: raw.stream_index,
            flags: raw.flags,
            duration: raw.duration,
            position: raw.pos,
            time_base: (raw.time_base.num, raw.time_base.den),
            side_data: copy_side_data(raw.side_data, raw.side_data_elems)?,
            bytes,
        })
    }

    fn into_ffmpeg(self) -> io::Result<ffmpeg::Packet> {
        if self.flags & ffmpeg::ffi::AV_PKT_FLAG_TRUSTED != 0 {
            return Err(invalid_data(
                "untrusted demux IPC cannot construct AV_PKT_FLAG_TRUSTED packets",
            ));
        }
        let packet_size = checked_i32(self.bytes.len(), "compressed packet")?;
        let mut packet = ffmpeg::Packet::empty();
        // SAFETY: packet owns a freshly allocated AVPacket. av_new_packet
        // allocates the FFmpeg-owned payload and required input padding.
        let allocation_result =
            unsafe { ffmpeg::ffi::av_new_packet(packet.as_mut_ptr(), packet_size) };
        if allocation_result < 0 {
            return Err(io::Error::other(format!(
                "allocate compressed packet: {}",
                ffmpeg::Error::from(allocation_result)
            )));
        }
        if !self.bytes.is_empty() {
            // SAFETY: av_new_packet allocated exactly self.bytes.len() writable
            // payload bytes owned by packet.
            unsafe {
                ptr::copy_nonoverlapping(
                    self.bytes.as_ptr(),
                    (*packet.as_mut_ptr()).data,
                    self.bytes.len(),
                );
            }
        }
        // SAFETY: Packet owns the AVPacket and its side-data array. FFmpeg's
        // packet allocator owns every installed side-data allocation.
        unsafe {
            let raw = &mut *packet.as_mut_ptr();
            raw.pts = self.pts;
            raw.dts = self.dts;
            raw.stream_index = self.stream_index;
            raw.flags = self.flags;
            raw.duration = self.duration;
            raw.pos = self.position;
            raw.time_base = rational_raw(self.time_base, "packet time base")?;
            install_side_data(
                &mut raw.side_data,
                &mut raw.side_data_elems,
                &self.side_data,
            )?;
        }
        Ok(packet)
    }

    fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        write_i64(writer, self.pts)?;
        write_i64(writer, self.dts)?;
        write_i32(writer, self.stream_index)?;
        write_i32(writer, self.flags)?;
        write_i64(writer, self.duration)?;
        write_i64(writer, self.position)?;
        write_i32(writer, self.time_base.0)?;
        write_i32(writer, self.time_base.1)?;
        write_side_data(writer, &self.side_data)?;
        write_bounded_bytes(writer, &self.bytes, MAX_PACKET_BYTES, "compressed packet")
    }

    fn read_from(reader: &mut impl Read) -> io::Result<Self> {
        Ok(Self {
            pts: read_i64(reader)?,
            dts: read_i64(reader)?,
            stream_index: read_i32(reader)?,
            flags: read_i32(reader)?,
            duration: read_i64(reader)?,
            position: read_i64(reader)?,
            time_base: (read_i32(reader)?, read_i32(reader)?),
            side_data: read_side_data(reader)?,
            bytes: read_bounded_bytes(reader, MAX_PACKET_BYTES, "compressed packet")?,
        })
    }
}

fn copy_side_data(
    pointer: *const ffmpeg::ffi::AVPacketSideData,
    count: i32,
) -> io::Result<Vec<WireSideData>> {
    let count = checked_nonnegative_usize(count, "side-data entry count")?;
    if count > MAX_SIDE_DATA_ENTRIES {
        return Err(invalid_data("too many Preview demux side-data entries"));
    }
    if count > 0 && pointer.is_null() {
        return Err(invalid_data("null Preview demux side-data array"));
    }
    let entries = if count == 0 {
        &[][..]
    } else {
        // SAFETY: FFmpeg owns `count` live entries for the containing object.
        unsafe { slice::from_raw_parts(pointer, count) }
    };
    let mut total = 0_usize;
    entries
        .iter()
        .map(|entry| {
            total = total
                .checked_add(entry.size)
                .ok_or_else(|| invalid_data("Preview demux side-data size overflow"))?;
            if total > MAX_TOTAL_SIDE_DATA_BYTES {
                return Err(invalid_data(
                    "Preview demux side data exceeds aggregate limit",
                ));
            }
            Ok(WireSideData {
                kind: entry.type_ as i32,
                bytes: copy_raw_bytes(entry.data, entry.size, MAX_SIDE_DATA_BYTES, "side data")?,
            })
        })
        .collect()
}

unsafe fn install_side_data(
    target: *mut *mut ffmpeg::ffi::AVPacketSideData,
    count: *mut i32,
    entries: &[WireSideData],
) -> io::Result<()> {
    for entry in entries {
        let kind = decode_side_data_type(entry.kind)?;
        // SAFETY: target/count belong to a fresh FFmpeg-owned packet or codec
        // parameter object. The returned allocation is owned by that object.
        let allocated = unsafe {
            ffmpeg::ffi::av_packet_side_data_new(target, count, kind, entry.bytes.len(), 0)
        };
        if allocated.is_null() {
            return Err(io::Error::other("allocate Preview demux side data"));
        }
        if !entry.bytes.is_empty() {
            // SAFETY: FFmpeg allocated exactly entry.bytes.len() bytes.
            unsafe {
                ptr::copy_nonoverlapping(
                    entry.bytes.as_ptr(),
                    (*allocated).data,
                    entry.bytes.len(),
                );
            }
        }
    }
    Ok(())
}

fn write_side_data(writer: &mut impl Write, entries: &[WireSideData]) -> io::Result<()> {
    if entries.len() > MAX_SIDE_DATA_ENTRIES {
        return Err(invalid_data("too many Preview demux side-data entries"));
    }
    write_u32(writer, entries.len() as u32)?;
    let mut total = 0_usize;
    for entry in entries {
        total = total
            .checked_add(entry.bytes.len())
            .ok_or_else(|| invalid_data("Preview demux side-data size overflow"))?;
        if total > MAX_TOTAL_SIDE_DATA_BYTES {
            return Err(invalid_data(
                "Preview demux side data exceeds aggregate limit",
            ));
        }
        decode_side_data_type(entry.kind)?;
        write_i32(writer, entry.kind)?;
        write_bounded_bytes(writer, &entry.bytes, MAX_SIDE_DATA_BYTES, "side data")?;
    }
    Ok(())
}

fn read_side_data(reader: &mut impl Read) -> io::Result<Vec<WireSideData>> {
    let count = read_u32(reader)? as usize;
    if count > MAX_SIDE_DATA_ENTRIES {
        return Err(invalid_data("too many Preview demux side-data entries"));
    }
    let mut total = 0_usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = read_i32(reader)?;
        decode_side_data_type(kind)?;
        let bytes = read_bounded_bytes(reader, MAX_SIDE_DATA_BYTES, "side data")?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| invalid_data("Preview demux side-data size overflow"))?;
        if total > MAX_TOTAL_SIDE_DATA_BYTES {
            return Err(invalid_data(
                "Preview demux side data exceeds aggregate limit",
            ));
        }
        entries.push(WireSideData { kind, bytes });
    }
    Ok(entries)
}

fn copy_raw_bytes(
    pointer: *const u8,
    length: usize,
    limit: usize,
    label: &str,
) -> io::Result<Vec<u8>> {
    if length > limit {
        return Err(invalid_data(format!("{label} exceeds {limit} bytes")));
    }
    if length == 0 {
        return Ok(Vec::new());
    }
    if pointer.is_null() {
        return Err(invalid_data(format!("{label} has a null data pointer")));
    }
    // SAFETY: the FFmpeg owner guarantees `length` readable bytes.
    Ok(unsafe { slice::from_raw_parts(pointer, length) }.to_vec())
}

#[cfg(windows)]
fn encode_native_path(path: &OsStr) -> io::Result<(u8, Vec<u8>)> {
    use std::os::windows::ffi::OsStrExt as _;

    let wide = path.encode_wide().collect::<Vec<_>>();
    let byte_length = wide
        .len()
        .checked_mul(2)
        .ok_or_else(|| invalid_data("media path length overflow"))?;
    if byte_length > MAX_PATH_BYTES {
        return Err(invalid_data("media path exceeds protocol limit"));
    }
    let mut bytes = Vec::with_capacity(byte_length);
    for unit in wide {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Ok((1, bytes))
}

#[cfg(windows)]
fn decode_native_path(encoding: u8, bytes: Vec<u8>) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;

    if encoding != 1 || !bytes.len().is_multiple_of(2) {
        return Err(invalid_data("invalid Windows media path encoding"));
    }
    let wide = bytes
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(OsString::from_wide(&wide)))
}

#[cfg(unix)]
fn encode_native_path(path: &OsStr) -> io::Result<(u8, Vec<u8>)> {
    use std::os::unix::ffi::OsStrExt as _;

    let bytes = path.as_bytes();
    if bytes.len() > MAX_PATH_BYTES {
        return Err(invalid_data("media path exceeds protocol limit"));
    }
    Ok((2, bytes.to_vec()))
}

#[cfg(unix)]
fn decode_native_path(encoding: u8, bytes: Vec<u8>) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;

    if encoding != 2 {
        return Err(invalid_data("invalid Unix media path encoding"));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(not(any(windows, unix)))]
fn encode_native_path(path: &OsStr) -> io::Result<(u8, Vec<u8>)> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid_data("media path is not Unicode on this platform"))?;
    Ok((3, value.as_bytes().to_vec()))
}

#[cfg(not(any(windows, unix)))]
fn decode_native_path(encoding: u8, bytes: Vec<u8>) -> io::Result<PathBuf> {
    if encoding != 3 {
        return Err(invalid_data("invalid media path encoding"));
    }
    let value = String::from_utf8(bytes).map_err(|_| invalid_data("media path is not UTF-8"))?;
    Ok(PathBuf::from(value))
}

fn write_bounded_bytes(
    writer: &mut impl Write,
    bytes: &[u8],
    limit: usize,
    label: &str,
) -> io::Result<()> {
    if bytes.len() > limit {
        return Err(invalid_data(format!("{label} exceeds {limit} bytes")));
    }
    write_u32(writer, checked_u32(bytes.len(), label)?)?;
    writer.write_all(bytes)
}

fn read_bounded_bytes(reader: &mut impl Read, limit: usize, label: &str) -> io::Result<Vec<u8>> {
    let length = read_u32(reader)? as usize;
    if length > limit {
        return Err(invalid_data(format!("{label} exceeds {limit} bytes")));
    }
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_rational(reader: &mut impl Read, label: &str) -> io::Result<ffmpeg::Rational> {
    let numerator = read_i32(reader)?;
    let denominator = read_i32(reader)?;
    if denominator <= 0 {
        return Err(invalid_data(format!(
            "{label} denominator must be positive"
        )));
    }
    Ok(ffmpeg::Rational(numerator, denominator))
}

fn rational_raw(value: (i32, i32), label: &str) -> io::Result<ffmpeg::ffi::AVRational> {
    if value.1 < 0 {
        return Err(invalid_data(format!(
            "{label} denominator cannot be negative"
        )));
    }
    Ok(ffmpeg::ffi::AVRational { num: value.0, den: value.1 })
}

fn checked_nonnegative_usize(value: i32, label: &str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| invalid_data(format!("{label} cannot be negative")))
}

fn checked_u32(value: usize, label: &str) -> io::Result<u32> {
    u32::try_from(value).map_err(|_| invalid_data(format!("{label} exceeds u32")))
}

fn checked_i32(value: usize, label: &str) -> io::Result<i32> {
    i32::try_from(value).map_err(|_| invalid_data(format!("{label} exceeds i32")))
}

fn write_u32(writer: &mut impl Write, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_optional_u32(writer: &mut impl Write, value: Option<u32>) -> io::Result<()> {
    writer.write_all(&[u8::from(value.is_some())])?;
    if let Some(value) = value {
        write_u32(writer, value)?;
    }
    Ok(())
}

fn write_optional_u64(writer: &mut impl Write, value: Option<u64>) -> io::Result<()> {
    writer.write_all(&[u8::from(value.is_some())])?;
    if let Some(value) = value {
        write_u64(writer, value)?;
    }
    Ok(())
}

fn write_u64(writer: &mut impl Write, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_i32(writer: &mut impl Write, value: i32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_i64(writer: &mut impl Write, value: i64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_optional_u32(reader: &mut impl Read, label: &str) -> io::Result<Option<u32>> {
    match read_presence(reader, label)? {
        false => Ok(None),
        true => read_u32(reader).map(Some),
    }
}

fn read_optional_u64(reader: &mut impl Read, label: &str) -> io::Result<Option<u64>> {
    match read_presence(reader, label)? {
        false => Ok(None),
        true => read_u64(reader).map(Some),
    }
}

fn read_presence(reader: &mut impl Read, label: &str) -> io::Result<bool> {
    let mut present = [0_u8; 1];
    reader.read_exact(&mut present)?;
    match present[0] {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(invalid_data(format!(
            "invalid {label} presence marker {other}"
        ))),
    }
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_i32(reader: &mut impl Read) -> io::Result<i32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

fn read_i64(reader: &mut impl Read) -> io::Result<i64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(i64::from_le_bytes(bytes))
}

pub(super) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const TEST_NONCE: [u8; 16] = [0x5a; 16];

    #[test]
    fn protocol_rejects_oversized_message_before_allocation() {
        let mut bytes = Vec::new();
        write_protocol_preamble(&mut bytes, TEST_NONCE).expect("preamble");
        bytes.push(MESSAGE_ERROR);
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&((MAX_ERROR_BYTES as u32) + 1).to_le_bytes());

        let mut reader = Cursor::new(bytes);
        read_protocol_preamble(&mut reader, TEST_NONCE).expect("read preamble");
        let error = match read_message(&mut reader) {
            Ok(_) => panic!("oversized error must fail"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn packet_round_trip_preserves_timing_flags_payload_and_side_data() {
        let mut packet = ffmpeg::Packet::copy(&[1, 2, 3, 4, 5]);
        packet.set_pts(Some(101));
        packet.set_dts(Some(99));
        packet.set_stream(7);
        packet.set_flags(ffmpeg::codec::packet::Flags::KEY);
        packet.set_duration(2);
        packet.set_position(404);
        packet.set_time_base(ffmpeg::Rational(1, 25));
        // SAFETY: packet owns the side-data allocation until drop.
        unsafe {
            let data = ffmpeg::ffi::av_packet_new_side_data(
                packet.as_mut_ptr(),
                ffmpeg::ffi::AVPacketSideDataType::AV_PKT_DATA_CONTENT_LIGHT_LEVEL,
                3,
            );
            assert!(!data.is_null());
            ptr::copy_nonoverlapping([9_u8, 8, 7].as_ptr(), data, 3);
        }

        let mut bytes = Vec::new();
        write_protocol_preamble(&mut bytes, TEST_NONCE).expect("preamble");
        write_packet_message(&mut bytes, 7, &packet).expect("packet message");

        let mut reader = Cursor::new(bytes);
        read_protocol_preamble(&mut reader, TEST_NONCE).expect("read preamble");
        let DemuxProtocolMessage::Packet(decoded) = read_message(&mut reader).expect("read packet")
        else {
            panic!("expected packet");
        };
        assert_eq!(decoded.command_id, 7);
        assert_eq!(decoded.packet.data(), packet.data());
        assert_eq!(decoded.packet.pts(), Some(101));
        assert_eq!(decoded.packet.dts(), Some(99));
        assert_eq!(decoded.packet.stream(), 7);
        assert!(decoded.packet.is_key());
        assert_eq!(decoded.packet.duration(), 2);
        assert_eq!(decoded.packet.position(), 404);
        assert_eq!(decoded.packet.time_base(), ffmpeg::Rational(1, 25));
        let side_data = decoded.packet.side_data().collect::<Vec<_>>();
        assert_eq!(side_data.len(), 1);
        assert_eq!(side_data[0].data(), &[9, 8, 7]);
    }

    #[test]
    fn protocol_version_is_fail_closed() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&PROTOCOL_MAGIC);
        bytes.extend_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
        let error = read_protocol_preamble(&mut Cursor::new(bytes), TEST_NONCE)
            .expect_err("future protocol must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn worker_request_round_trip_preserves_native_path_and_nonce() {
        let path = Path::new("fixtures/媒体/clip.mov");
        let source_revision = MediaFileFingerprint {
            len: Some(123_456),
            modified_secs: Some(987),
            modified_nanos: Some(654_321),
            object_identity: Some(MediaFileObjectIdentity::Windows {
                volume_serial_number: 42,
                file_id: [7; 16],
            }),
            change_stamp: Some(MediaFileChangeStamp::WindowsFileTime(987_654_321)),
        };
        let mut bytes = Vec::new();
        write_worker_request(&mut bytes, TEST_NONCE, path, source_revision, Some(7))
            .expect("write request");

        let request = read_worker_request(&mut Cursor::new(bytes)).expect("read request");
        assert_eq!(request.nonce, TEST_NONCE);
        assert_eq!(request.path, path);
        assert_eq!(request.source_revision, source_revision);
        assert_eq!(request.video_stream_index, Some(7));
    }

    #[test]
    fn worker_commands_round_trip_with_strict_seek_window() {
        let commands = [
            DemuxWorkerCommand::Seek {
                command_id: 1,
                min_ts: 10,
                target_ts: 20,
                max_ts: 30,
                flags: ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
            },
            DemuxWorkerCommand::Read { command_id: 2 },
            DemuxWorkerCommand::Close { command_id: 3 },
        ];
        let mut bytes = Vec::new();
        for command in commands {
            write_worker_command(&mut bytes, command).expect("write command");
        }
        let mut reader = Cursor::new(bytes);
        for expected in commands {
            assert_eq!(
                read_worker_command(&mut reader).expect("read command"),
                expected
            );
        }
    }

    #[test]
    fn open_phases_round_trip_without_command_identity() {
        let mut bytes = Vec::new();
        write_open_phase_message(&mut bytes, DemuxOpenPhase::InputOpen).expect("input open");
        write_open_phase_message(&mut bytes, DemuxOpenPhase::StreamInfo).expect("stream info");
        let mut reader = Cursor::new(bytes);
        assert!(matches!(
            read_message(&mut reader).expect("input-open phase"),
            DemuxProtocolMessage::OpenPhase(DemuxOpenPhase::InputOpen)
        ));
        assert!(matches!(
            read_message(&mut reader).expect("stream-info phase"),
            DemuxProtocolMessage::OpenPhase(DemuxOpenPhase::StreamInfo)
        ));
    }

    #[test]
    fn stream_contract_round_trip_preserves_keyframe_anchors() {
        let parameters = ffmpeg::codec::Parameters::new();
        // SAFETY: the parameters object is live and exclusively owned here.
        unsafe {
            (*parameters.as_ptr().cast_mut()).codec_type =
                ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
            (*parameters.as_ptr().cast_mut()).codec_id = ffmpeg::ffi::AVCodecID::AV_CODEC_ID_H264;
        }
        let mut bytes = Vec::new();
        write_stream_message(
            &mut bytes,
            &parameters,
            2,
            ffmpeg::Rational(1, 90_000),
            42,
            ffmpeg::Rational(30_000, 1_001),
            &[42, 3_045, 6_048],
            true,
        )
        .expect("write stream");
        let DemuxProtocolMessage::Stream(stream) =
            read_message(&mut Cursor::new(bytes)).expect("read stream")
        else {
            panic!("expected stream");
        };
        assert_eq!(stream.keyframe_pts, [42, 3_045, 6_048]);
        assert!(stream.keyframe_index_truncated);
    }
}
