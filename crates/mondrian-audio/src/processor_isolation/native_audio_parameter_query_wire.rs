//! One explicit selected-ID query on the existing serial native-owner connection.
//! The child validates the exact binding against its actual object before and
//! after capture; no target, UI ticket or copied author values enter this wire.

use crate::{NativeAudioObjectBindRequest, NativeAudioParameterQueryResult};
use mondrian_core::{
    NativeAudioParameterObservation, NativeAudioParameterObservationOrigin,
    NativeAudioParameterQueryRefusal as Refusal,
};
use std::io::{self, Read, Write};

const MAGIC: [u8; 4] = *b"MAPQ";
fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}
fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
pub(super) fn write_request(
    writer: &mut impl Write,
    binding: NativeAudioObjectBindRequest,
    id: u32,
) -> io::Result<()> {
    super::native_audio_object_wire::write_bind_request(writer, binding)?;
    writer.write_all(&id.to_le_bytes())?;
    writer.flush()
}
pub(super) fn read_request(
    reader: &mut impl Read,
) -> io::Result<(NativeAudioObjectBindRequest, u32)> {
    let binding = super::native_audio_object_wire::read_bind_request(reader)?;
    Ok((binding, read_u32(reader)?))
}

pub(super) enum QueryReply {
    Outcome(NativeAudioParameterQueryResult),
    Failed,
}

fn validate_single(
    revision: u64,
    facts: &Vec<mondrian_core::NativeAudioParameterFact>,
) -> io::Result<()> {
    if facts.len() != 1 {
        return Err(invalid("native parameter query requires one fact"));
    }
    NativeAudioParameterObservation::validate_facts(facts)
        .map_err(|_| invalid("native parameter query storage"))?;
    let fact = &facts[0];
    if fact.revision != revision
        || fact.event_flags != 0
        || fact.origin != (NativeAudioParameterObservationOrigin::MainQuery { rescan_flags: 0 })
    {
        return Err(invalid("native parameter query origin/revision"));
    }
    Ok(())
}

pub(super) fn write_reply(
    writer: &mut impl Write,
    reply: QueryReply,
    object: crate::NativeAudioObjectObservation,
) -> io::Result<()> {
    // Validate all owned facts before exposing the reply header. The single
    // original fact shape bounds the body below the existing control frame.
    if let QueryReply::Outcome(Ok((revision, facts))) = &reply {
        validate_single(*revision, facts)?;
    }
    writer.write_all(&MAGIC)?;
    writer.write_all(&super::protocol::PROTOCOL_VERSION.to_le_bytes())?;
    match reply {
        QueryReply::Outcome(Ok((revision, facts))) => {
            writer.write_all(&1_u32.to_le_bytes())?;
            super::native_audio_parameter_wire::write_parameters(
                writer,
                revision,
                &facts,
                Some(object),
            )?;
        }
        QueryReply::Outcome(Err(refusal)) => {
            writer.write_all(&2_u32.to_le_bytes())?;
            let tag: u32 = match refusal {
                Refusal::UnsupportedAdapter => 1,
                Refusal::Unavailable => 2,
                Refusal::OwnerChanged => 3,
                Refusal::Busy => 4,
                Refusal::ParameterUnavailable => 5,
            };
            writer.write_all(&tag.to_le_bytes())?;
        }
        QueryReply::Failed => writer.write_all(&3_u32.to_le_bytes())?,
    }
    writer.flush()
}

pub(super) fn read_reply(
    reader: &mut impl Read,
    binding: NativeAudioObjectBindRequest,
    native_id: u32,
) -> io::Result<QueryReply> {
    let mut header = [0; 8];
    reader.read_exact(&mut header)?;
    if header[..4] != MAGIC
        || u32::from_le_bytes(
            header[4..].try_into().map_err(|_| invalid("native parameter query header"))?,
        ) != super::protocol::PROTOCOL_VERSION
    {
        return Err(invalid("native parameter query version"));
    }
    match read_u32(reader)? {
        1 => {
            let (revision, facts) = super::native_audio_parameter_wire::read_single_parameter(
                reader,
                binding.expected_object,
            )?;
            validate_single(revision, &facts)?;
            // No echoed ticket is needed on this exclusive serial RPC. A native
            // result still must identify exactly the requested canonical ID.
            if facts[0]
                .parameter
                .as_str()
                .strip_prefix("vst3.param.")
                .and_then(|id| id.parse::<u32>().ok())
                != Some(native_id)
            {
                return Err(invalid("native parameter query ID mismatch"));
            }
            Ok(QueryReply::Outcome(Ok((revision, facts))))
        }
        2 => Ok(QueryReply::Outcome(Err(match read_u32(reader)? {
            1 => Refusal::UnsupportedAdapter,
            2 => Refusal::Unavailable,
            3 => Refusal::OwnerChanged,
            4 => Refusal::Busy,
            5 => Refusal::ParameterUnavailable,
            _ => return Err(invalid("native parameter query refusal")),
        }))),
        3 => Ok(QueryReply::Failed),
        _ => Err(invalid("native parameter query reply tag")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> NativeAudioObjectBindRequest {
        NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: crate::NativeAudioObjectObservation::new(
                std::num::NonZeroU32::new(42).unwrap(),
                std::num::NonZeroU64::new(7).unwrap(),
            ),
        }
    }
    fn fact(id: u32) -> mondrian_core::NativeAudioParameterFact {
        mondrian_core::NativeAudioParameterFact {
            revision: 9,
            parameter: mondrian_core::ParameterId::new(format!("vst3.param.{id}")).unwrap(),
            event_flags: 0,
            value_bits: Some(0.0_f64.to_bits()),
            author_value_bits: Some(0.0_f64.to_bits()),
            origin: NativeAudioParameterObservationOrigin::MainQuery { rescan_flags: 0 },
            hidden: Some(false),
            periodic: None,
            name: Some("Actual caption".into()),
            module: None,
            info_flags: Some(1),
            display: Some("0.0".into()),
        }
    }
    #[test]
    fn explicit_query_wire_uses_original_binding_and_accepts_native_id_zero() {
        let expected = binding();
        let mut bytes = Vec::new();
        write_request(&mut bytes, expected, 0).unwrap();
        assert_eq!(
            bytes.len(),
            16 + 4 + 8 + 4,
            "no UI ticket or parameter table"
        );
        assert_eq!(read_request(&mut bytes.as_slice()).unwrap(), (expected, 0));
        let mut current = Vec::new();
        write_reply(
            &mut current,
            QueryReply::Outcome(Ok((9, vec![fact(0)]))),
            expected.expected_object,
        )
        .unwrap();
        let QueryReply::Outcome(Ok((revision, facts))) =
            read_reply(&mut current.as_slice(), expected, 0).unwrap()
        else {
            panic!("one fact");
        };
        assert_eq!(revision, 9);
        assert_eq!(facts, [fact(0)]);
        assert!(
            read_reply(&mut current.as_slice(), expected, 4095).is_err(),
            "wrong selected ID"
        );
        let mut absent = fact(0);
        absent.value_bits = None;
        absent.display = None;
        current.clear();
        write_reply(
            &mut current,
            QueryReply::Outcome(Ok((9, vec![absent.clone()]))),
            expected.expected_object,
        )
        .unwrap();
        let QueryReply::Outcome(Ok((_, facts))) =
            read_reply(&mut current.as_slice(), expected, 0).unwrap()
        else {
            panic!("actual unavailable plain");
        };
        assert_eq!(facts, [absent]);
    }
    #[test]
    fn explicit_query_wire_refuses_invalid_bind_truncation_old_tags_and_non_single_count_before_allocating(
    ) {
        let expected = binding();
        let mut request = Vec::new();
        write_request(&mut request, expected, 4095).unwrap();
        for end in 0..request.len() {
            assert!(read_request(&mut &request[..end]).is_err());
        }
        for range in [16..20, 20..28] {
            let mut invalid = request.clone();
            invalid[range].fill(0);
            assert!(
                read_request(&mut invalid.as_slice()).is_err(),
                "zero PID or object cannot become an owner"
            );
        }
        let mut reply = Vec::new();
        write_reply(
            &mut reply,
            QueryReply::Outcome(Ok((9, vec![fact(4095)]))),
            expected.expected_object,
        )
        .unwrap();
        for end in 0..reply.len() {
            assert!(read_reply(&mut &reply[..end], expected, 4095).is_err());
        }
        for count in [0_u32, 2, 1152, u32::MAX] {
            let mut invalid = reply[..28].to_vec();
            invalid[24..28].copy_from_slice(&count.to_le_bytes());
            let mut reader = std::io::Cursor::new(invalid);
            assert!(read_reply(&mut reader, expected, 4095).is_err());
            assert_eq!(
                reader.position(),
                28,
                "count rejects before fact/string allocation or reads"
            );
        }
        let mut invalid = reply.clone();
        invalid[4..8].copy_from_slice(&18_u32.to_le_bytes());
        assert!(read_reply(&mut invalid.as_slice(), expected, 4095).is_err());
        for tag in [0_u32, 4, 5, u32::MAX] {
            let mut invalid = reply.clone();
            invalid[8..12].copy_from_slice(&tag.to_le_bytes());
            assert!(read_reply(&mut invalid.as_slice(), expected, 4095).is_err());
        }
        let mut untouched = Vec::new();
        assert!(write_reply(
            &mut untouched,
            QueryReply::Outcome(Ok((9, vec![]))),
            expected.expected_object
        )
        .is_err());
        assert!(untouched.is_empty(), "preflight before publishing a header");
    }
    #[test]
    fn explicit_query_single_fact_accepts_actual_overallocated_capacity_and_fixed_failure() {
        let expected = binding();
        let mut facts = Vec::with_capacity(7);
        facts.push(fact(0));
        let mut bytes = Vec::new();
        write_reply(
            &mut bytes,
            QueryReply::Outcome(Ok((9, facts))),
            expected.expected_object,
        )
        .unwrap();
        assert!(
            matches!(read_reply(&mut bytes.as_slice(), expected, 0).unwrap(), QueryReply::Outcome(Ok((9, facts))) if facts.len() == 1)
        );
        bytes.clear();
        write_reply(&mut bytes, QueryReply::Failed, expected.expected_object).unwrap();
        assert_eq!(
            bytes.len(),
            12,
            "fixed failure has no allocated detail payload"
        );
        assert!(matches!(
            read_reply(&mut bytes.as_slice(), expected, 0).unwrap(),
            QueryReply::Failed
        ));
    }
    #[test]
    fn explicit_query_refusals_and_fixed_command_preserve_existing_storage_bounds() {
        let expected = binding();
        for refusal in [
            Refusal::UnsupportedAdapter,
            Refusal::Unavailable,
            Refusal::OwnerChanged,
            Refusal::Busy,
            Refusal::ParameterUnavailable,
        ] {
            let mut bytes = Vec::new();
            write_reply(
                &mut bytes,
                QueryReply::Outcome(Err(refusal)),
                expected.expected_object,
            )
            .unwrap();
            assert!(
                matches!(read_reply(&mut bytes.as_slice(), expected, 0).unwrap(), QueryReply::Outcome(Err(actual)) if actual == refusal)
            );
        }
        let command =
            super::super::native_worker_command::NativeCommand::QueryParameter(expected, 4095);
        assert_eq!(
            command.owned_input_layout().unwrap(),
            crate::NativeHostBufferLayout { bytes: 0, alignment: 1 }
        );
        let facts = vec![fact(4095)];
        let bytes = NativeAudioParameterObservation::validate_facts(&facts).unwrap();
        assert_eq!(
            bytes,
            std::alloc::Layout::array::<mondrian_core::NativeAudioParameterFact>(1)
                .unwrap()
                .size()
                + facts[0].string_capacity_bytes().unwrap()
        );
        assert!(bytes < mondrian_core::NATIVE_AUDIO_PARAMETER_FACT_BYTES);
        assert_eq!(mondrian_core::NATIVE_AUDIO_PARAMETER_FACTS, 1152);
        assert_eq!(mondrian_core::NATIVE_AUDIO_PARAMETER_FACT_BYTES, 256 * 1024);
    }
}
