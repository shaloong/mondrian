//! Bounded native-object observations on the sole authenticated owner stream.
//! Metadata and binding acknowledgements cannot mint installed/author leases.

use crate::{
    NativeAudioObjectBindOutcome, NativeAudioObjectBindReceipt, NativeAudioObjectBindRefusal,
    NativeAudioObjectBindRequest, NativeAudioObjectObservation,
};
use mondrian_core::AudioProcessorBindingId;
use std::io::{self, Read, Write};
use std::num::{NonZeroU32, NonZeroU64};

const MAGIC: [u8; 4] = *b"MAOB";

fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}

fn read_object(reader: &mut impl Read) -> io::Result<NativeAudioObjectObservation> {
    let mut bytes = [0_u8; 12];
    reader.read_exact(&mut bytes)?;
    let pid = NonZeroU32::new(u32::from_le_bytes(
        bytes[..4].try_into().map_err(|_| invalid("native PID extent"))?,
    ))
    .ok_or_else(|| invalid("native PID is zero"))?;
    let serial = NonZeroU64::new(u64::from_le_bytes(
        bytes[4..].try_into().map_err(|_| invalid("native serial extent"))?,
    ))
    .ok_or_else(|| invalid("native serial is zero"))?;
    Ok(NativeAudioObjectObservation::new(pid, serial))
}

fn write_object(writer: &mut impl Write, object: NativeAudioObjectObservation) -> io::Result<()> {
    writer.write_all(&object.process_id().get().to_le_bytes())?;
    writer.write_all(&object.object_serial().get().to_le_bytes())
}

pub(super) fn write_bind_request(
    writer: &mut impl Write,
    request: NativeAudioObjectBindRequest,
) -> io::Result<()> {
    writer.write_all(request.binding.0.as_bytes())?;
    write_object(writer, request.expected_object)?;
    writer.flush()
}

pub(super) fn read_bind_request(
    reader: &mut impl Read,
) -> io::Result<NativeAudioObjectBindRequest> {
    let mut id = [0_u8; 16];
    reader.read_exact(&mut id)?;
    Ok(NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId(uuid::Uuid::from_bytes(id)),
        expected_object: read_object(reader)?,
    })
}

pub(super) fn write_observation_request(
    writer: &mut impl Write,
    consumption: crate::NativeAudioObservationConsumption,
) -> io::Result<()> {
    use crate::NativeAudioObservationConsumption::*;
    let tag: u32 = match consumption {
        Preserve => 0,
        Drain => 1,
        DrainAuthor(_) => 2,
        AcknowledgeAuthor(_) => 3,
        AcknowledgeReenteredAuthor(_) => 4,
    };
    writer.write_all(&tag.to_le_bytes())?;
    match consumption {
        DrainAuthor(binding) => write_bind_request(writer, binding)?,
        AcknowledgeAuthor(ack) => {
            if ack.revision == 0
                || ack.source_binding.expected_object != ack.installed_binding.expected_object
            {
                return Err(invalid(
                    "native author acknowledgement object/revision mismatch",
                ));
            }
            write_bind_request(writer, ack.source_binding)?;
            write_bind_request(writer, ack.installed_binding)?;
            writer.write_all(&ack.revision.to_le_bytes())?;
        }
        AcknowledgeReenteredAuthor(ack) => {
            super::native_audio_author_state_entry_wire::write_receipt(writer, ack.preparation)?;
            if ack.installed_binding.expected_object != ack.preparation.object() {
                return Err(invalid("reentered Ack fresh-object binding differs"));
            }
            write_bind_request(writer, ack.installed_binding)?;
        }
        Preserve | Drain => {}
    }
    writer.flush()
}

pub(super) fn read_observation_request(
    reader: &mut impl Read,
) -> io::Result<crate::NativeAudioObservationConsumption> {
    use crate::NativeAudioObservationConsumption::*;
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    match u32::from_le_bytes(bytes) {
        0 => Ok(Preserve),
        1 => Ok(Drain),
        2 => read_bind_request(reader).map(DrainAuthor),
        3 => {
            let source_binding = read_bind_request(reader)?;
            let installed_binding = read_bind_request(reader)?;
            if source_binding.expected_object != installed_binding.expected_object {
                return Err(invalid("native author acknowledgement object changed"));
            }
            let mut bytes = [0; 8];
            reader.read_exact(&mut bytes)?;
            let revision = u64::from_le_bytes(bytes);
            if revision == 0 {
                return Err(invalid("native author acknowledgement revision is zero"));
            }
            Ok(AcknowledgeAuthor(crate::NativeAudioAuthorAcknowledgement {
                source_binding,
                installed_binding,
                revision,
            }))
        }
        4 => {
            let preparation = super::native_audio_author_state_entry_wire::read_receipt(reader)?;
            let installed_binding = read_bind_request(reader)?;
            if installed_binding.expected_object != preparation.object() {
                return Err(invalid("reentered Ack fresh-object binding differs"));
            }
            Ok(AcknowledgeReenteredAuthor(
                crate::NativeAudioReenteredAuthorAcknowledgement { preparation, installed_binding },
            ))
        }
        _ => Err(invalid("native observation consumption tag")),
    }
}

fn validate_log(record: &crate::NativeAudioLogRecord) -> io::Result<()> {
    use crate::native_audio_observations::*;
    let length = usize::from(record.length);
    if length > NATIVE_AUDIO_LOG_MESSAGE_BYTES
        || usize::from(record.utf8_prefix_bytes) > length
        || record.flags & !LOG_FLAGS != 0
    {
        return Err(invalid("native diagnostic record bound"));
    }
    if record.null_message() && (length != 0 || record.truncated())
        || record.truncated() && length != NATIVE_AUDIO_LOG_MESSAGE_BYTES
    {
        return Err(invalid("native diagnostic null/truncation contract"));
    }
    let unknown = crate::NativeAudioLogSeverity::from_raw(record.raw_severity).is_none();
    if unknown != (record.flags & LOG_UNKNOWN_SEVERITY != 0) {
        return Err(invalid("native diagnostic severity contract"));
    }
    let (valid_prefix, invalid_utf8, incomplete_suffix) =
        match std::str::from_utf8(&record.bytes[..length]) {
            Ok(_) => (length, false, false),
            Err(error) if record.truncated() && error.error_len().is_none() => {
                (error.valid_up_to(), false, true)
            }
            Err(error) => (error.valid_up_to(), true, false),
        };
    if usize::from(record.utf8_prefix_bytes) != valid_prefix
        || record.invalid_utf8() != invalid_utf8
        || record.incomplete_utf8_suffix() != incomplete_suffix
    {
        return Err(invalid("native diagnostic UTF-8 facts do not match bytes"));
    }
    Ok(())
}

use super::native_configuration_wire::{
    read_observed_request as read_pending_request, write_observed_request as write_pending_request,
};

fn write_observations(
    writer: &mut impl Write,
    observations: &crate::NativeAudioObservations,
) -> io::Result<()> {
    use crate::native_audio_observations::*;
    if observations.logs.count > NATIVE_AUDIO_LOG_RECORDS
        || observations.logs.loss_reasons & !LOG_LOSS_FLAGS != 0
    {
        return Err(invalid("native diagnostic batch bound"));
    }
    if observations.owner_process_id == 0
        || observations
            .object
            .is_some_and(|object| object.process_id().get() != observations.owner_process_id)
    {
        return Err(invalid("native observation producer identity"));
    }
    writer.write_all(&observations.owner_process_id.to_le_bytes())?;
    writer.write_all(&[u8::from(observations.object.is_some())])?;
    if let Some(object) = observations.object {
        write_object(writer, object)?;
    }
    if observations.editor_cleanup.is_some() && observations.object.is_none() {
        return Err(invalid("native cleanup has no actual object"));
    }
    let cleanup: u32 = match observations.editor_cleanup {
        None => 0,
        Some(crate::NativeAudioEditorCloseStage::RemoveView) => 1,
        Some(crate::NativeAudioEditorCloseStage::ClearHostFrame) => 2,
    };
    writer.write_all(&cleanup.to_le_bytes())?;
    write_pending_request(writer, observations.pending_reprepare)?;
    super::native_audio_author_wire::write_author(
        writer,
        observations.author.as_ref(),
        observations.object,
    )?;
    super::native_audio_parameter_wire::write_parameters(
        writer,
        observations.parameter_revision,
        &observations.parameters,
        observations.object,
    )?;
    let count =
        u32::try_from(observations.logs.count).map_err(|_| invalid("native diagnostic count"))?;
    writer.write_all(&count.to_le_bytes())?;
    writer.write_all(&observations.logs.loss_reasons.to_le_bytes())?;
    let mut previous = None;
    for record in observations.logs.records() {
        validate_log(record)?;
        if previous.is_some_and(|value| value >= record.sequence) {
            return Err(invalid("native diagnostic sequence order"));
        }
        previous = Some(record.sequence);
        let mut header = [0_u8; 18];
        header[..8].copy_from_slice(&record.sequence.to_le_bytes());
        header[8..12].copy_from_slice(&record.raw_severity.to_le_bytes());
        header[12..14].copy_from_slice(&record.length.to_le_bytes());
        header[14..16].copy_from_slice(&record.utf8_prefix_bytes.to_le_bytes());
        header[16..18].copy_from_slice(&record.flags.to_le_bytes());
        writer.write_all(&header)?;
        writer.write_all(record.bytes())?;
    }
    Ok(())
}

fn read_observations(reader: &mut impl Read) -> io::Result<crate::NativeAudioObservations> {
    use crate::native_audio_observations::*;
    let mut pid = [0; 4];
    reader.read_exact(&mut pid)?;
    let owner_process_id = u32::from_le_bytes(pid);
    if owner_process_id == 0 {
        return Err(invalid("native observation producer PID is zero"));
    }
    let mut present = [0];
    reader.read_exact(&mut present)?;
    let object = match present[0] {
        0 => None,
        1 => Some(read_object(reader)?),
        _ => return Err(invalid("native object presence flag")),
    };
    if object.is_some_and(|object| object.process_id().get() != owner_process_id) {
        return Err(invalid("native object/observation owner mismatch"));
    }
    let mut cleanup = [0; 4];
    reader.read_exact(&mut cleanup)?;
    let editor_cleanup = match u32::from_le_bytes(cleanup) {
        0 => None,
        1 => Some(crate::NativeAudioEditorCloseStage::RemoveView),
        2 => Some(crate::NativeAudioEditorCloseStage::ClearHostFrame),
        _ => return Err(invalid("native cleanup stage")),
    };
    if editor_cleanup.is_some() && object.is_none() {
        return Err(invalid("native cleanup has no actual object"));
    }
    let pending_reprepare = read_pending_request(reader)?;
    let author = super::native_audio_author_wire::read_author(reader, object)?;
    let (parameter_revision, parameters) =
        super::native_audio_parameter_wire::read_parameters(reader, object)?;
    let mut extent = [0; 8];
    reader.read_exact(&mut extent)?;
    let count = usize::try_from(u32::from_le_bytes(
        extent[..4].try_into().map_err(|_| invalid("native diagnostic count extent"))?,
    ))
    .map_err(|_| invalid("native diagnostic count conversion"))?;
    let loss_reasons = u32::from_le_bytes(
        extent[4..].try_into().map_err(|_| invalid("native diagnostic loss extent"))?,
    );
    if count > NATIVE_AUDIO_LOG_RECORDS || loss_reasons & !LOG_LOSS_FLAGS != 0 {
        return Err(invalid("native diagnostic batch exceeds bound"));
    }
    let mut logs = crate::NativeAudioLogBatch { count, loss_reasons, ..Default::default() };
    let mut previous = None;
    for record in &mut logs.records[..count] {
        let mut header = [0; 18];
        reader.read_exact(&mut header)?;
        record.sequence = u64::from_le_bytes(
            header[..8]
                .try_into()
                .map_err(|_| invalid("native diagnostic sequence extent"))?,
        );
        record.raw_severity = i32::from_le_bytes(
            header[8..12]
                .try_into()
                .map_err(|_| invalid("native diagnostic severity extent"))?,
        );
        record.length = u16::from_le_bytes(
            header[12..14]
                .try_into()
                .map_err(|_| invalid("native diagnostic length extent"))?,
        );
        record.utf8_prefix_bytes = u16::from_le_bytes(
            header[14..16]
                .try_into()
                .map_err(|_| invalid("native diagnostic prefix extent"))?,
        );
        record.flags = u16::from_le_bytes(
            header[16..18]
                .try_into()
                .map_err(|_| invalid("native diagnostic flags extent"))?,
        );
        if usize::from(record.length) > NATIVE_AUDIO_LOG_MESSAGE_BYTES {
            return Err(invalid("native diagnostic payload exceeds bound"));
        }
        reader.read_exact(&mut record.bytes[..usize::from(record.length)])?;
        validate_log(record)?;
        if previous.is_some_and(|value| value >= record.sequence) {
            return Err(invalid("native diagnostic sequence order"));
        }
        previous = Some(record.sequence);
    }
    Ok(crate::NativeAudioObservations {
        owner_process_id,
        object,
        editor_cleanup,
        pending_reprepare,
        author,
        parameter_revision,
        parameters,
        logs,
    })
}

pub(super) enum ObjectReply<'a> {
    Observation(&'a crate::NativeAudioObservations),
    Binding(NativeAudioObjectBindOutcome),
    Failed(String),
}

struct BoundedObservationBytes {
    used: usize,
}
impl Write for BoundedObservationBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .used
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("native observation body extent"))?;
        if next > super::protocol::MAX_CONTROL_FRAME_BYTES {
            return Err(invalid("native observation body exceeds control frame"));
        }
        self.used = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// Cold owned deliveries may combine author/configuration and parameter facts.
// Admit their aggregate actual wire extent before publishing ANY socket byte.
// Preserve's fixed-size source snapshot remains on the original direct path.
pub(super) fn write_reply(writer: &mut impl Write, reply: ObjectReply<'_>) -> io::Result<()> {
    if let ObjectReply::Observation(observation) = &reply {
        let author_owned = observation.author.as_ref().is_some_and(|author| {
            matches!(
                author.delivery,
                crate::NativeAudioAuthorDeliveryState::Delivered(_)
            )
        });
        if author_owned || !observation.parameters.is_empty() {
            write_reply_fields(
                &mut BoundedObservationBytes { used: 0 },
                ObjectReply::Observation(observation),
            )?;
        }
    }
    write_reply_fields(writer, reply)
}

fn write_reply_fields(writer: &mut impl Write, reply: ObjectReply<'_>) -> io::Result<()> {
    writer.write_all(&MAGIC)?;
    writer.write_all(&super::protocol::PROTOCOL_VERSION.to_le_bytes())?;
    let tag: u32 = match &reply {
        ObjectReply::Observation(_) => 1,
        ObjectReply::Binding(NativeAudioObjectBindOutcome::Bound(_)) => 3,
        ObjectReply::Binding(NativeAudioObjectBindOutcome::Refused(_)) => 4,
        ObjectReply::Failed(_) => 5,
    };
    writer.write_all(&tag.to_le_bytes())?;
    match reply {
        ObjectReply::Observation(observations) => write_observations(writer, observations)?,
        ObjectReply::Binding(NativeAudioObjectBindOutcome::Bound(receipt)) => {
            writer.write_all(receipt.binding().0.as_bytes())?;
            write_object(writer, receipt.object())?;
        }
        ObjectReply::Binding(NativeAudioObjectBindOutcome::Refused(refusal)) => {
            let value: u32 = match refusal {
                NativeAudioObjectBindRefusal::UnsupportedAdapter => 1,
                NativeAudioObjectBindRefusal::ObjectChanged => 2,
                NativeAudioObjectBindRefusal::EditorStatePending => 3,
            };
            writer.write_all(&value.to_le_bytes())?;
        }
        ObjectReply::Failed(detail) => {
            let mut end = detail.len().min(super::protocol::MAX_WORKER_DETAIL_BYTES);
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            let detail = &detail.as_bytes()[..end];
            let length =
                u32::try_from(detail.len()).map_err(|_| invalid("native failure detail extent"))?;
            writer.write_all(&length.to_le_bytes())?;
            writer.write_all(detail)?;
        }
    }
    writer.flush()
}

// Decode directly into the requested result. Observation polling also occurs
// before PCM windows, so a large owned reply union or per-poll Box is unsuitable.
struct BoundedObservationReader<'a, R> {
    reader: &'a mut R,
    used: usize,
}
impl<R: Read> Read for BoundedObservationReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let remaining = super::protocol::MAX_CONTROL_FRAME_BYTES
            .checked_sub(self.used)
            .ok_or_else(|| invalid("native observation input extent"))?;
        if remaining == 0 {
            return Err(invalid("native observation input exceeds control frame"));
        }
        let extent = bytes.len().min(remaining);
        let count = self.reader.read(&mut bytes[..extent])?;
        self.used += count;
        Ok(count)
    }
}

fn read_reply<T>(
    reader: &mut impl Read,
    observation: impl FnOnce(crate::NativeAudioObservations) -> io::Result<T>,
    binding: impl FnOnce(NativeAudioObjectBindOutcome) -> io::Result<T>,
) -> io::Result<T> {
    read_reply_fields(
        &mut BoundedObservationReader { reader, used: 0 },
        observation,
        binding,
    )
}

fn read_reply_fields<T>(
    reader: &mut impl Read,
    observation: impl FnOnce(crate::NativeAudioObservations) -> io::Result<T>,
    binding: impl FnOnce(NativeAudioObjectBindOutcome) -> io::Result<T>,
) -> io::Result<T> {
    let mut header = [0_u8; 12];
    reader.read_exact(&mut header)?;
    if header[..4] != MAGIC
        || u32::from_le_bytes(
            header[4..8].try_into().map_err(|_| invalid("native reply version extent"))?,
        ) != super::protocol::PROTOCOL_VERSION
    {
        return Err(invalid("native object reply protocol mismatch"));
    }
    let tag = u32::from_le_bytes(
        header[8..12].try_into().map_err(|_| invalid("native reply tag extent"))?,
    );
    match tag {
        1 => observation(read_observations(reader)?),
        3 => {
            let request = read_bind_request(reader)?;
            let receipt =
                NativeAudioObjectBindReceipt::acknowledge(request, request.expected_object)
                    .map_err(|_| invalid("native binding receipt refused"))?;
            binding(NativeAudioObjectBindOutcome::Bound(receipt))
        }
        4 => {
            let mut bytes = [0_u8; 4];
            reader.read_exact(&mut bytes)?;
            let refusal = match u32::from_le_bytes(bytes) {
                1 => NativeAudioObjectBindRefusal::UnsupportedAdapter,
                2 => NativeAudioObjectBindRefusal::ObjectChanged,
                3 => NativeAudioObjectBindRefusal::EditorStatePending,
                _ => return Err(invalid("native binding refusal tag")),
            };
            binding(NativeAudioObjectBindOutcome::Refused(refusal))
        }
        5 => {
            let mut bytes = [0_u8; 4];
            reader.read_exact(&mut bytes)?;
            let length = usize::try_from(u32::from_le_bytes(bytes))
                .map_err(|_| invalid("native failure extent"))?;
            if length > super::protocol::MAX_WORKER_DETAIL_BYTES {
                return Err(invalid("native failure exceeds bound"));
            }
            let mut detail = vec![0_u8; length];
            reader.read_exact(&mut detail)?;
            let detail = String::from_utf8(detail).map_err(|_| invalid("native failure UTF-8"))?;
            Err(io::Error::other(detail))
        }
        _ => Err(invalid("native object reply tag")),
    }
}

pub(super) fn read_observation_reply(
    reader: &mut impl Read,
) -> io::Result<crate::NativeAudioObservations> {
    read_reply(reader, Ok, |_| {
        Err(invalid("native object observation correlation mismatch"))
    })
}

pub(super) fn read_bind_reply(
    reader: &mut impl Read,
    expected: NativeAudioObjectBindRequest,
) -> io::Result<NativeAudioObjectBindOutcome> {
    read_reply(
        reader,
        |_| Err(invalid("native binding reply correlation mismatch")),
        |outcome| match outcome {
            NativeAudioObjectBindOutcome::Bound(receipt)
                if receipt.binding() == expected.binding
                    && receipt.object() == expected.expected_object =>
            {
                Ok(outcome)
            }
            NativeAudioObjectBindOutcome::Refused(_) => Ok(outcome),
            _ => Err(invalid("native binding reply correlation mismatch")),
        },
    )
}

#[cfg(test)]
mod native_object_wire_contracts {
    use super::*;
    use std::io::Cursor;

    fn object(serial: u64) -> NativeAudioObjectObservation {
        NativeAudioObjectObservation::new(
            NonZeroU32::new(127).expect("PID"),
            NonZeroU64::new(serial).expect("load serial"),
        )
    }
    fn request(serial: u64) -> NativeAudioObjectBindRequest {
        NativeAudioObjectBindRequest {
            binding: AudioProcessorBindingId::new(),
            expected_object: object(serial),
        }
    }

    #[test]
    fn native_object_wire_correlates_exact_binding_and_native_load_without_author_permission() {
        let request = request(11);
        let mut bytes = Vec::new();
        write_bind_request(&mut bytes, request).expect("bounded exact request");
        assert_eq!(
            bytes.len(),
            28,
            "one typed ID plus actual PID and load serial"
        );
        assert_eq!(
            read_bind_request(&mut Cursor::new(bytes)).expect("current body"),
            request
        );
        let receipt = NativeAudioObjectBindReceipt::acknowledge(request, object(11))
            .expect("same actual object");
        let mut bytes = Vec::new();
        write_reply(
            &mut bytes,
            ObjectReply::Binding(NativeAudioObjectBindOutcome::Bound(receipt)),
        )
        .expect("correlated acknowledgement");
        assert_eq!(bytes.len(), 40);
        for version in [8_u32, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20] {
            let mut old = bytes.clone();
            old[4..8].copy_from_slice(&version.to_le_bytes());
            assert!(read_bind_reply(&mut Cursor::new(old), request).is_err());
        }
        assert_eq!(
            &bytes[..12],
            &[77, 65, 79, 66, 19, 0, 0, 0, 3, 0, 0, 0],
            "single current V19 schema"
        );
        assert_eq!(
            read_bind_reply(&mut Cursor::new(bytes.clone()), request).expect("same binding/object"),
            NativeAudioObjectBindOutcome::Bound(receipt)
        );
        assert!(
            read_bind_reply(
                &mut Cursor::new(bytes.clone()),
                NativeAudioObjectBindRequest { binding: AudioProcessorBindingId::new(), ..request }
            )
            .is_err(),
            "different binding cannot reuse the acknowledgement"
        );
        assert!(
            read_bind_reply(
                &mut Cursor::new(bytes),
                NativeAudioObjectBindRequest { expected_object: object(12), ..request }
            )
            .is_err(),
            "PID equality cannot hide an actual object reload"
        );
    }

    #[test]
    fn native_object_wire_typed_refusal_and_unsupported_observation_keep_next_response_aligned() {
        let expected = request(17);
        let mut bytes = Vec::new();
        write_reply(
            &mut bytes,
            ObjectReply::Binding(NativeAudioObjectBindOutcome::Refused(
                NativeAudioObjectBindRefusal::ObjectChanged,
            )),
        )
        .expect("safe refused owner");
        write_reply(
            &mut bytes,
            ObjectReply::Observation(&crate::NativeAudioObservations {
                owner_process_id: 127,
                ..Default::default()
            }),
        )
        .expect("explicit unsupported capability");
        write_reply(
            &mut bytes,
            ObjectReply::Observation(&crate::NativeAudioObservations {
                owner_process_id: 127,
                object: Some(object(18)),
                ..Default::default()
            }),
        )
        .expect("same child future current load");
        let mut bytes = Cursor::new(bytes);
        assert_eq!(
            read_bind_reply(&mut bytes, expected).expect("typed refusal"),
            NativeAudioObjectBindOutcome::Refused(NativeAudioObjectBindRefusal::ObjectChanged)
        );
        assert_eq!(
            read_observation_reply(&mut bytes)
                .expect("absence is not fake authority")
                .object,
            None
        );
        assert_eq!(
            read_observation_reply(&mut bytes).expect("no reply bytes lost").object,
            Some(object(18))
        );
    }

    #[test]
    fn native_object_wire_rejects_old_unknown_and_partial_envelopes() {
        use super::super::protocol::{CommandKind, WorkerCommand};
        let command = WorkerCommand {
            kind: CommandKind::PollNativeObservations,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        };
        let mut bytes = Vec::new();
        command.write_to(&mut bytes).expect("current header");
        assert_eq!(&bytes[4..12], &[19, 0, 0, 0, 9, 0, 0, 0]);
        for version in [5_u32, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20] {
            let mut old = bytes.clone();
            old[4..8].copy_from_slice(&version.to_le_bytes());
            assert!(
                WorkerCommand::read_from(&mut Cursor::new(old)).is_err(),
                "old schema is refused before native entry"
            );
        }
        let mut unknown = bytes.clone();
        unknown[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            WorkerCommand::read_from(&mut Cursor::new(unknown)).is_err(),
            "future/unknown kind cannot enter foreign dispatch"
        );
        let mut payload = bytes.clone();
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());
        assert!(
            WorkerCommand::read_from(&mut Cursor::new(payload)).is_err(),
            "observation has no invented DSP block payload"
        );
        for length in 0..28 {
            let request = request(19);
            let mut body = Vec::new();
            write_bind_request(&mut body, request).expect("body");
            assert!(
                read_bind_request(&mut Cursor::new(&body[..length])).is_err(),
                "incomplete owner body cannot bind"
            );
        }
        let mut zero = vec![0_u8; 28];
        zero[..16].copy_from_slice(AudioProcessorBindingId::new().0.as_bytes());
        assert!(
            read_bind_request(&mut Cursor::new(zero)).is_err(),
            "zero PID/load is not an actual observation"
        );
    }
}

#[cfg(test)]
mod native_log_wire_contracts {
    use super::*;
    use crate::native_audio_observations::*;
    use std::io::Cursor;

    fn observation() -> crate::NativeAudioObservations {
        let mut logs = crate::NativeAudioLogBatch {
            count: 2,
            loss_reasons: LOSS_FULL | LOSS_CONTENTION,
            ..Default::default()
        };
        let mut first = crate::NativeAudioLogRecord::EMPTY;
        first.sequence = 17;
        first.raw_severity = -17;
        first.length = 1;
        first.flags = LOG_UNKNOWN_SEVERITY | LOG_INVALID_UTF8;
        first.bytes[0] = 255;
        let mut second = crate::NativeAudioLogRecord::EMPTY;
        second.sequence = 18;
        second.raw_severity = 6;
        second.length = 512;
        second.utf8_prefix_bytes = 512;
        second.flags = LOG_TRUNCATED;
        second.bytes = [b'x'; 512];
        logs.records[..2].copy_from_slice(&[first, second]);
        crate::NativeAudioObservations {
            owner_process_id: 127,
            object: None,
            editor_cleanup: None,
            pending_reprepare: None,
            author: None,
            parameter_revision: 0,
            parameters: Vec::new(),
            logs,
        }
    }

    #[test]
    fn native_log_wire_current_single_response_preserves_exact_bytes_and_no_object_capability() {
        let expected = observation();
        let mut bytes = Vec::new();
        write_reply(&mut bytes, ObjectReply::Observation(&expected))
            .expect("one bounded current response");
        assert_eq!(&bytes[..12], &[77, 65, 79, 66, 19, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(
            &bytes[17..37],
            &[0; 20],
            "no cleanup stage or pending cohort"
        );
        assert_eq!(bytes[37], 0, "no author source");
        assert_eq!(&bytes[38..46], &[0; 8], "no parameter revision");
        assert_eq!(&bytes[46..50], b"NPF2", "explicit current fact-body format");
        assert_eq!(&bytes[50..54], &[0; 4], "no parameter facts");
        assert_eq!(
            &bytes[54..58],
            &2_u32.to_le_bytes(),
            "actual log count field"
        );
        assert_eq!(
            bytes.len(),
            12 + 4 + 1 + 4 + 16 + 1 + 8 + 4 + 4 + 8 + (18 + 1) + (18 + 512),
            "fields only; no Rust padding or stale version reader"
        );
        let observed =
            read_observation_reply(&mut Cursor::new(bytes)).expect("current field decoder");
        assert_eq!(observed, expected);
        assert!(observed.object.is_none());
        assert_eq!(observed.logs.records()[0].raw_severity(), -17);
        assert_eq!(observed.logs.records()[0].severity(), None);
        assert_eq!(observed.logs.records()[0].bytes(), &[255]);
        assert!(observed.logs.records()[0].text().is_err());
        assert!(observed.logs.incomplete());
    }

    #[test]
    fn native_log_wire_bounds_and_falsified_facts_are_refused_before_exposure() {
        let good = observation();
        for invalid_case in 0..9 {
            let mut bad = good.clone();
            match invalid_case {
                0 => bad.owner_process_id = 0,
                1 => bad.logs.count = NATIVE_AUDIO_LOG_RECORDS + 1,
                2 => bad.logs.loss_reasons = 8,
                3 => bad.logs.records[0].length = 513,
                4 => bad.logs.records[0].utf8_prefix_bytes = 1,
                5 => bad.logs.records[0].flags = LOG_INVALID_UTF8,
                6 => bad.logs.records[1].sequence = 17,
                7 => bad.logs.records[1].flags = LOG_TRUNCATED | LOG_NULL,
                _ => bad.logs.records[1].length = 511,
            }
            assert!(
                write_reply(&mut Vec::new(), ObjectReply::Observation(&bad)).is_err(),
                "cannot publish invalidcase{invalid_case}"
            );
        }
        let mut bytes = Vec::new();
        write_reply(&mut bytes, ObjectReply::Observation(&good)).expect("baseline");
        // Current fixed fields: envelope12 + PID4 + object1 + cleanup4 +
        // observed-request16 + author1 + parameter-revision8 + fact-format4 + parameter-count4.
        const LOG_COUNT_OFFSET: usize = 54;
        assert_eq!(
            &bytes[LOG_COUNT_OFFSET..LOG_COUNT_OFFSET + 4],
            &2_u32.to_le_bytes()
        );
        let mut oversized = bytes.clone();
        oversized[LOG_COUNT_OFFSET..LOG_COUNT_OFFSET + 4].copy_from_slice(&33_u32.to_le_bytes());
        let mut oversized = Cursor::new(oversized);
        assert!(
            read_observation_reply(&mut oversized).is_err(),
            "untrusted count cannot grow storage"
        );
        assert_eq!(
            oversized.position(),
            62,
            "count and loss header is read, but no untrusted record or storage extent is consumed"
        );
        for cut in 0..bytes.len() {
            assert!(
                read_observation_reply(&mut Cursor::new(&bytes[..cut])).is_err(),
                "partial extent{cut} cannot expose initialized fiction"
            );
        }
    }

    #[test]
    fn native_log_wire_one_consumption_policy_body_is_explicit_bounded_and_aligned() {
        use crate::NativeAudioObservationConsumption::{Drain, Preserve};
        let mut bytes = Vec::new();
        write_observation_request(&mut bytes, Preserve).expect("preserve");
        write_observation_request(&mut bytes, Drain).expect("drain");
        assert_eq!(bytes, &[0, 0, 0, 0, 1, 0, 0, 0]);
        let mut stream = Cursor::new(bytes);
        assert_eq!(
            read_observation_request(&mut stream).expect("first"),
            Preserve
        );
        assert_eq!(
            read_observation_request(&mut stream).expect("second"),
            Drain
        );
        assert!(read_observation_request(&mut Cursor::new(2_u32.to_le_bytes())).is_err());
        for cut in 0..4 {
            assert!(
                read_observation_request(&mut Cursor::new(&1_u32.to_le_bytes()[..cut])).is_err()
            );
        }
    }
}

#[cfg(test)]
mod native_preflight_snapshot_existing_wire_contracts {
    use super::*;
    #[test]
    fn native_preflight_snapshot_existing_wire_has_one_fixed_pending_field_before_diagnostics() {
        let observations =
            crate::NativeAudioObservations { owner_process_id: 127, ..Default::default() };
        let mut actual = Vec::new();
        write_reply(&mut actual, ObjectReply::Observation(&observations))
            .expect("canonical encoder");
        // Independent byte oracle, not the decoder or a mirrored field helper.
        let mut expected = Vec::new();
        expected.extend_from_slice(b"MAOB");
        expected.extend_from_slice(&19_u32.to_le_bytes());
        expected.extend_from_slice(&1_u32.to_le_bytes());
        expected.extend_from_slice(&127_u32.to_le_bytes());
        expected.push(0);
        expected.extend_from_slice(&0_u32.to_le_bytes()); // no editor cleanup
        expected.extend_from_slice(&[0_u8; 16]); // canonical absent observed request
        expected.push(0); // V8 explicitly unavailable author source
        expected.extend_from_slice(&[0_u8; 8]); // absent parameter revision
        expected.extend_from_slice(b"NPF2"); // independent current fact-body format
        expected.extend_from_slice(&[0_u8; 4]); // absent parameter count
        expected.extend_from_slice(&[0_u8; 8]); // empty logs + no sticky loss
        assert_eq!(
            expected.len(),
            62,
            "no Rust padding or variable pending payload"
        );
        assert_eq!(
            actual, expected,
            "one current schema must carry pending and cleanup in the same reply"
        );
    }
}

#[cfg(test)]
mod native_preflight_snapshot_wire_contracts {
    use super::*;
    use mondrian_core::NativeAudioReprepareRequest;
    #[test]
    fn native_preflight_snapshot_wire_preserves_unknown_vst_bits_and_clap_request_exactly() {
        use crate::{NativeObservedRequest as Observed, NativeRequestCohort};
        for (request, words) in [
            (None, [0_u32, 0, 0, 0]),
            (
                Some(Observed::RequestOnly(
                    NativeAudioReprepareRequest::ClapRestart,
                )),
                [1, 0, 2, 0],
            ),
            (
                Some(Observed::RequestOnly(
                    NativeAudioReprepareRequest::Vst3Restart { flags: 0x8000_0010 },
                )),
                [1, 0, 1, 0x8000_0010],
            ),
            (
                Some(Observed::Cohort(
                    NativeRequestCohort::new(
                        41,
                        NativeAudioReprepareRequest::Vst3Restart { flags: 0x8000_0010 },
                    )
                    .unwrap(),
                )),
                [2, 41, 1, 0x8000_0010],
            ),
        ] {
            let observations = crate::NativeAudioObservations {
                owner_process_id: 127,
                pending_reprepare: request,
                ..Default::default()
            };
            let mut bytes = Vec::new();
            write_reply(&mut bytes, ObjectReply::Observation(&observations)).unwrap();
            for (index, word) in words.into_iter().enumerate() {
                assert_eq!(&bytes[21 + 4 * index..25 + 4 * index], &word.to_le_bytes());
            }
            let decoded = read_observation_reply(&mut std::io::Cursor::new(bytes)).unwrap();
            assert_eq!(decoded.pending_reprepare, request);
        }
    }
    #[test]
    fn native_preflight_snapshot_wire_rejects_malformed_tag_zero_flags_and_reserved_payload() {
        use crate::NativeObservedRequest::RequestOnly;
        for words in [
            [1_u32, 0, 1, 0],
            [3, 0, 0, 0],
            [0, 0, 0, 1],
            [1, 0, 2, 1],
            [1, 7, 2, 0],
            [2, 0, 2, 0],
        ] {
            let mut bytes = Vec::new();
            write_reply(
                &mut bytes,
                ObjectReply::Observation(&crate::NativeAudioObservations {
                    owner_process_id: 127,
                    ..Default::default()
                }),
            )
            .unwrap();
            for (index, word) in words.into_iter().enumerate() {
                bytes[21 + 4 * index..25 + 4 * index].copy_from_slice(&word.to_le_bytes());
            }
            assert!(
                read_observation_reply(&mut std::io::Cursor::new(bytes)).is_err(),
                "malformed native evidence must not reach root admission"
            );
        }
        assert!(write_reply(
            &mut Vec::new(),
            ObjectReply::Observation(&crate::NativeAudioObservations {
                owner_process_id: 127,
                pending_reprepare: Some(RequestOnly(NativeAudioReprepareRequest::Vst3Restart {
                    flags: 0
                })),
                ..Default::default()
            })
        )
        .is_err());
    }
    #[test]
    fn native_preflight_snapshot_wire_cleanup_and_pending_retain_same_native_owner() {
        let object = NativeAudioObjectObservation::new(
            NonZeroU32::new(127).expect("PID"),
            NonZeroU64::new(51).expect("actual load"),
        );
        let observations = crate::NativeAudioObservations {
            owner_process_id: 127,
            object: Some(object),
            editor_cleanup: Some(crate::NativeAudioEditorCloseStage::ClearHostFrame),
            pending_reprepare: Some(crate::NativeObservedRequest::RequestOnly(
                NativeAudioReprepareRequest::Vst3Restart { flags: 16 },
            )),
            ..Default::default()
        };
        let mut bytes = Vec::new();
        write_reply(&mut bytes, ObjectReply::Observation(&observations))
            .expect("same owner snapshot");
        assert_eq!(
            read_observation_reply(&mut std::io::Cursor::new(bytes)).expect("single reply"),
            observations
        );
        let mut wrong = observations;
        wrong.owner_process_id = 128;
        assert!(
            write_reply(&mut Vec::new(), ObjectReply::Observation(&wrong)).is_err(),
            "cleanup/pending cannot hide mismatched object PID"
        );
    }
}
