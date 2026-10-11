//! Bounded editor controls on the same V6 authenticated owner connection.

use crate::{
    NativeAudioEditorCloseStage, NativeAudioEditorOpenRequest, NativeAudioEditorOutcome,
    NativeAudioEditorRefusal, NativeAudioObjectBindRequest,
};
use mondrian_core::NATIVE_AUDIO_EDITOR_TITLE_BYTES;
use std::io::{self, Read, Write};

const MAGIC: [u8; 4] = *b"MAED";
fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}

pub(super) fn write_open_request(
    writer: &mut impl Write,
    request: &NativeAudioEditorOpenRequest,
) -> io::Result<()> {
    super::native_audio_object_wire::write_bind_request(writer, request.binding())?;
    let length =
        u32::try_from(request.title().len()).map_err(|_| invalid("native editor title extent"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(request.title().as_bytes())?;
    writer.flush()
}

pub(super) fn read_open_request(
    reader: &mut impl Read,
) -> io::Result<NativeAudioEditorOpenRequest> {
    let binding = super::native_audio_object_wire::read_bind_request(reader)?;
    let length = read_u32(reader)? as usize;
    if length > NATIVE_AUDIO_EDITOR_TITLE_BYTES {
        return Err(invalid("native editor title exceeds admitted bytes"));
    }
    let mut bytes = [0_u8; NATIVE_AUDIO_EDITOR_TITLE_BYTES];
    reader.read_exact(&mut bytes[..length])?;
    let title = std::str::from_utf8(&bytes[..length])
        .map_err(|_| invalid("native editor title is not UTF-8"))?;
    mondrian_core::NativeAudioEditorTitle::new(title.to_owned())
        .map(|title| NativeAudioEditorOpenRequest::new(binding, title))
        .map_err(|_| invalid("native editor title contains NUL"))
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

pub(super) enum EditorReply {
    Outcome(NativeAudioEditorOutcome),
    Failed(String),
}

pub(super) fn write_reply(writer: &mut impl Write, reply: EditorReply) -> io::Result<()> {
    writer.write_all(&MAGIC)?;
    writer.write_all(&super::protocol::PROTOCOL_VERSION.to_le_bytes())?;
    let tag: u32 = match reply {
        EditorReply::Outcome(NativeAudioEditorOutcome::Opened { .. }) => 1,
        EditorReply::Outcome(NativeAudioEditorOutcome::Closed { .. }) => 2,
        EditorReply::Outcome(NativeAudioEditorOutcome::Refused(_)) => 3,
        EditorReply::Outcome(NativeAudioEditorOutcome::ClosePending { .. }) => 4,
        EditorReply::Failed(_) => 5,
    };
    writer.write_all(&tag.to_le_bytes())?;
    match reply {
        EditorReply::Outcome(NativeAudioEditorOutcome::Opened { binding, width, height }) => {
            super::native_audio_object_wire::write_bind_request(writer, binding)?;
            writer.write_all(&width.to_le_bytes())?;
            writer.write_all(&height.to_le_bytes())?;
        }
        EditorReply::Outcome(NativeAudioEditorOutcome::Closed { binding }) => {
            super::native_audio_object_wire::write_bind_request(writer, binding)?;
        }
        EditorReply::Outcome(NativeAudioEditorOutcome::ClosePending { binding, stage }) => {
            super::native_audio_object_wire::write_bind_request(writer, binding)?;
            let stage: u32 = match stage {
                NativeAudioEditorCloseStage::RemoveView => 1,
                NativeAudioEditorCloseStage::ClearHostFrame => 2,
            };
            writer.write_all(&stage.to_le_bytes())?;
        }
        EditorReply::Outcome(NativeAudioEditorOutcome::Refused(refusal)) => {
            let value: u32 = match refusal {
                NativeAudioEditorRefusal::UnsupportedAdapter => 1,
                NativeAudioEditorRefusal::NotInstalled => 2,
                NativeAudioEditorRefusal::OwnerChanged => 3,
                NativeAudioEditorRefusal::NoEditor => 4,
                NativeAudioEditorRefusal::ConfigurationPending => 5,
                NativeAudioEditorRefusal::InvalidTitle => 6,
            };
            writer.write_all(&value.to_le_bytes())?;
        }
        EditorReply::Failed(detail) => {
            let mut length = detail.len().min(super::protocol::MAX_WORKER_DETAIL_BYTES);
            while !detail.is_char_boundary(length) {
                length -= 1;
            }
            writer.write_all(&(length as u32).to_le_bytes())?;
            writer.write_all(&detail.as_bytes()[..length])?;
        }
    }
    writer.flush()
}

pub(super) fn read_reply(
    reader: &mut impl Read,
    expected: NativeAudioObjectBindRequest,
    opening: bool,
) -> io::Result<EditorReply> {
    let mut header = [0_u8; 8];
    reader.read_exact(&mut header)?;
    if header[..4] != MAGIC
        || u32::from_le_bytes(
            header[4..8].try_into().map_err(|_| invalid("native editor version extent"))?,
        ) != super::protocol::PROTOCOL_VERSION
    {
        return Err(invalid("native editor reply version mismatch"));
    }
    let tag = read_u32(reader)?;
    let outcome = match tag {
        1 if opening => {
            let binding = super::native_audio_object_wire::read_bind_request(reader)?;
            let width = read_u32(reader)?;
            let height = read_u32(reader)?;
            if binding != expected
                || width == 0
                || height == 0
                || width > i32::MAX as u32
                || height > i32::MAX as u32
            {
                return Err(invalid("native editor open correlation or extent mismatch"));
            }
            NativeAudioEditorOutcome::Opened { binding, width, height }
        }
        2 if !opening => {
            let binding = super::native_audio_object_wire::read_bind_request(reader)?;
            if binding != expected {
                return Err(invalid("native editor close correlation mismatch"));
            }
            NativeAudioEditorOutcome::Closed { binding }
        }
        3 => NativeAudioEditorOutcome::Refused(match read_u32(reader)? {
            1 => NativeAudioEditorRefusal::UnsupportedAdapter,
            2 => NativeAudioEditorRefusal::NotInstalled,
            3 => NativeAudioEditorRefusal::OwnerChanged,
            4 => NativeAudioEditorRefusal::NoEditor,
            5 => NativeAudioEditorRefusal::ConfigurationPending,
            6 => NativeAudioEditorRefusal::InvalidTitle,
            _ => return Err(invalid("unknown native editor refusal")),
        }),
        4 => {
            let binding = super::native_audio_object_wire::read_bind_request(reader)?;
            if binding != expected {
                return Err(invalid("native editor pending close correlation mismatch"));
            }
            let stage = match read_u32(reader)? {
                1 => NativeAudioEditorCloseStage::RemoveView,
                2 => NativeAudioEditorCloseStage::ClearHostFrame,
                _ => return Err(invalid("unknown native editor pending stage")),
            };
            NativeAudioEditorOutcome::ClosePending { binding, stage }
        }
        5 => {
            let length = read_u32(reader)? as usize;
            if length > super::protocol::MAX_WORKER_DETAIL_BYTES {
                return Err(invalid("native editor failure detail exceeds budget"));
            }
            let mut bytes = [0_u8; super::protocol::MAX_WORKER_DETAIL_BYTES];
            reader.read_exact(&mut bytes[..length])?;
            let detail = std::str::from_utf8(&bytes[..length])
                .map_err(|_| invalid("native editor failure detail UTF-8"))?;
            return Ok(EditorReply::Failed(detail.to_owned()));
        }
        _ => return Err(invalid("native editor reply does not match command")),
    };
    Ok(EditorReply::Outcome(outcome))
}
