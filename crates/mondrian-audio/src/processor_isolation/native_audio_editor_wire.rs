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
    if !request.static_parameter_values().is_empty() {
        return Err(invalid("tag 11 cannot carry static parameter values"));
    }
    write_open_prefix(writer, request)
}

fn write_open_prefix(
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

/// Tag 16 alone carries constants keyed by prepared processing lane index.
/// An old tag-11 peer must never interpret the
/// extra bytes as a following command or acknowledge an unsynchronized Open.
pub(super) fn write_projected_open_request(
    writer: &mut impl Write,
    request: &NativeAudioEditorOpenRequest,
) -> io::Result<()> {
    request.owned_layout().map_err(|_| invalid("native editor request storage"))?;
    if request.static_parameter_values().is_empty() {
        return Err(invalid(
            "projected editor open requires static parameter values",
        ));
    }
    write_open_prefix(writer, request)?;
    writer.write_all(&(request.static_parameter_values().len() as u32).to_le_bytes())?;
    for (lane, bits) in request.static_parameter_values() {
        writer.write_all(&lane.to_le_bytes())?;
        writer.write_all(&bits.to_le_bytes())?;
    }
    writer.flush()
}

pub(super) fn read_projected_open_request(
    reader: &mut impl Read,
) -> io::Result<NativeAudioEditorOpenRequest> {
    let request = read_open_request(reader)?;
    let count = read_u32(reader)? as usize;
    if count == 0 {
        return Err(invalid(
            "projected editor open requires static parameter values",
        ));
    }
    crate::native_audio_editor::static_parameter_value_layout(request.title().len(), count)
        .map_err(|_| invalid("native editor static parameter count exceeds local frontier"))?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| invalid("native editor request allocation"))?;
    crate::native_audio_editor::static_parameter_value_layout(
        request.title().len(),
        values.capacity(),
    )
    .map_err(|_| invalid("native editor actual request capacity exceeds local frontier"))?;
    for _ in 0..count {
        let lane = read_u32(reader)?;
        let mut bits = [0_u8; 8];
        reader.read_exact(&mut bits)?;
        values.push((lane, u64::from_le_bytes(bits)));
    }
    request
        .with_static_parameter_values(values)
        .map_err(|_| invalid("invalid native editor static parameter selection"))
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

#[cfg(test)]
mod tests {
    use super::super::native_worker_command::{decode, NativeCommand};
    use super::super::protocol::{CommandKind, WorkerCommand};
    use super::*;
    fn request() -> NativeAudioEditorOpenRequest {
        NativeAudioEditorOpenRequest::new(
            NativeAudioObjectBindRequest {
                binding: mondrian_core::AudioProcessorBindingId::new(),
                expected_object: crate::NativeAudioObjectObservation::new(
                    std::num::NonZeroU32::new(42).unwrap(),
                    std::num::NonZeroU64::new(7).unwrap(),
                ),
            },
            mondrian_core::NativeAudioEditorTitle::new("same native owner".to_owned()).unwrap(),
        )
    }
    #[test]
    fn projected_open_tag16_preserves_full_selection_bits_and_exact_owned_layout() {
        let values = (0..4096)
            .map(|id| {
                (
                    id,
                    if id == 0 {
                        (-0.0_f64).to_bits()
                    } else {
                        0.25_f64.to_bits()
                    },
                )
            })
            .collect();
        let expected = request().with_static_parameter_values(values).unwrap();
        let mut bytes = Vec::new();
        WorkerCommand {
            transport_activity: None,
            kind: CommandKind::EditorOpenWithStaticParameters,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut bytes)
        .unwrap();
        write_projected_open_request(&mut bytes, &expected).unwrap();
        assert_eq!(&bytes[8..12], &16_u32.to_le_bytes());
        let decoded = decode(&mut bytes.as_slice(), 4096).unwrap();
        let actual_layout = decoded.owned_input_layout().unwrap();
        let NativeCommand::EditorOpen(actual) = decoded else {
            panic!("same full-owner dispatcher")
        };
        assert_eq!(actual, expected);
        assert_eq!(actual_layout, actual.owned_layout().unwrap());
        assert_eq!(actual_layout.alignment, align_of::<(u32, u64)>());
        assert_eq!(
            actual_layout.bytes,
            4096 * size_of::<(u32, u64)>() + actual.title().len()
        );
        let mut legacy = Vec::new();
        assert!(write_open_request(&mut legacy, &expected).is_err());
        assert!(
            legacy.is_empty(),
            "never publish projected input as legacy tag11"
        );
        for offset in [12_usize, 16, 24, 28] {
            let mut invalid = bytes.clone();
            invalid[offset..offset + 4].copy_from_slice(&1_u32.to_le_bytes());
            let mut reader = std::io::Cursor::new(invalid);
            assert!(decode(&mut reader, 4096).is_err());
            assert_eq!(reader.position(), 32);
        }
        for tag in [5_u32, 19, u32::MAX] {
            let mut invalid = bytes.clone();
            invalid[8..12].copy_from_slice(&tag.to_le_bytes());
            assert!(decode(&mut invalid.as_slice(), 4096).is_err());
        }
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn projected_open_actual_tuple_alignment_is_charged_by_original_child_lane() {
        let request =
            request().with_static_parameter_values(vec![(0, 0.25_f64.to_bits())]).unwrap();
        let actual = NativeCommand::EditorOpen(request).owned_input_layout().unwrap();
        let profile = super::super::vst3_worker::native_worker_lane_profile().unwrap().unwrap();
        let mut overlap = profile.control_overlap();
        overlap.decoded_input = actual;
        profile.admit_observed_lane_owners(profile.owning_layouts(), overlap).unwrap();
        assert_eq!(profile.control_overlap().decoded_input.bytes, 1024 * 1024);
        let mut too_weak = profile.control_overlap();
        too_weak.decoded_input.alignment = 1;
        let insufficient = crate::NativeWorkerLaneResourceProfile::scoped_main_dsp(
            profile.owning_layouts(),
            too_weak,
        )
        .unwrap();
        assert!(
            insufficient
                .admit_observed_lane_owners(profile.owning_layouts(), overlap)
                .is_err(),
            "real tuple alignment cannot be underreported as wire byte alignment"
        );
    }
    #[test]
    fn projected_open_pre_refuses_count_duplicates_nonfinite_and_truncation() {
        let empty = request();
        let mut prefix = Vec::new();
        write_open_request(&mut prefix, &empty).unwrap();
        assert_eq!(read_open_request(&mut prefix.as_slice()).unwrap(), empty);
        for count in [0_u32, 4097, u32::MAX] {
            let mut bytes = prefix.clone();
            bytes.extend_from_slice(&count.to_le_bytes());
            let mut reader = std::io::Cursor::new(bytes);
            assert!(read_projected_open_request(&mut reader).is_err());
            assert_eq!(
                reader.position() as usize,
                prefix.len() + 4,
                "count before row allocation/read"
            );
        }
        for values in [
            vec![(0, 0.25_f64.to_bits()), (0, 0.5_f64.to_bits())],
            vec![(0, f64::NAN.to_bits())],
            vec![(4096, 0.25_f64.to_bits())],
            vec![(u32::MAX, 0.25_f64.to_bits())],
        ] {
            assert!(request().with_static_parameter_values(values.clone()).is_err());
            let mut bytes = prefix.clone();
            bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for (id, bits) in values {
                bytes.extend_from_slice(&id.to_le_bytes());
                bytes.extend_from_slice(&bits.to_le_bytes());
            }
            assert!(read_projected_open_request(&mut bytes.as_slice()).is_err());
        }
        let value = request().with_static_parameter_values(vec![(0, 0.25_f64.to_bits())]).unwrap();
        let mut bytes = Vec::new();
        write_projected_open_request(&mut bytes, &value).unwrap();
        for end in 0..bytes.len() {
            assert!(read_projected_open_request(&mut &bytes[..end]).is_err());
        }
        let mut overallocated = Vec::with_capacity(4097);
        overallocated.push((0, 0.25_f64.to_bits()));
        assert!(
            request().with_static_parameter_values(overallocated).is_err(),
            "actual capacity, not length"
        );
        assert!(crate::native_audio_editor::static_parameter_value_layout(0, usize::MAX).is_err());
        assert!(request()
            .with_static_parameter_values(vec![(4096, 0.25_f64.to_bits())])
            .is_err());
        assert!(request()
            .with_static_parameter_values(vec![(u32::MAX, 0.25_f64.to_bits())])
            .is_err());
        assert_eq!(
            request().owned_layout().unwrap(),
            crate::NativeHostBufferLayout { bytes: empty.title().len(), alignment: 1 }
        );
    }
}
