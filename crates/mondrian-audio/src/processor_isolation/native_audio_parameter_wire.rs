//! Parameter presentation fields in the sole current Poll9 reply.

use mondrian_core::{
    NativeAudioParameterFact as Fact, NativeAudioParameterObservationOrigin as Origin,
};
use std::io::{self, Read, Write};

fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}
fn u32_read(reader: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    reader.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn u64_read(reader: &mut impl Read) -> io::Result<u64> {
    let mut b = [0; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn flag(reader: &mut impl Read) -> io::Result<bool> {
    let mut b = [0];
    reader.read_exact(&mut b)?;
    match b[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid("parameter presence flag")),
    }
}
fn text_write(writer: &mut impl Write, text: Option<&str>) -> io::Result<()> {
    writer.write_all(&[u8::from(text.is_some())])?;
    if let Some(text) = text {
        writer.write_all(
            &u32::try_from(text.len())
                .map_err(|_| invalid("parameter string extent"))?
                .to_le_bytes(),
        )?;
        writer.write_all(text.as_bytes())?;
    }
    Ok(())
}
fn text_read(
    reader: &mut impl Read,
    limit: usize,
    storage: &mut usize,
) -> io::Result<Option<String>> {
    if !flag(reader)? {
        return Ok(None);
    }
    let length =
        usize::try_from(u32_read(reader)?).map_err(|_| invalid("parameter string extent"))?;
    if length > limit {
        return Err(invalid("parameter string bound"));
    }
    *storage = storage
        .checked_add(length)
        .ok_or_else(|| invalid("parameter storage overflow"))?;
    if *storage > mondrian_core::NATIVE_AUDIO_PARAMETER_FACT_BYTES {
        return Err(invalid("parameter storage bound"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some(
        String::from_utf8(bytes).map_err(|_| invalid("parameter string UTF-8"))?,
    ))
}

pub(super) fn write_parameters(
    writer: &mut impl Write,
    revision: u64,
    facts: &Vec<Fact>,
    object: Option<crate::NativeAudioObjectObservation>,
) -> io::Result<()> {
    mondrian_core::NativeAudioParameterObservation::validate_facts(facts)
        .map_err(|_| invalid("parameter observation extent"))?;
    if (!facts.is_empty() && object.is_none()) || facts.iter().any(|fact| fact.revision > revision)
    {
        return Err(invalid("parameter source/object identity"));
    }
    writer.write_all(&revision.to_le_bytes())?;
    writer.write_all(
        &u32::try_from(facts.len())
            .map_err(|_| invalid("parameter count"))?
            .to_le_bytes(),
    )?;
    for fact in facts {
        writer.write_all(&fact.revision.to_le_bytes())?;
        text_write(writer, Some(fact.parameter.as_str()))?;
        writer.write_all(&fact.event_flags.to_le_bytes())?;
        writer.write_all(&[u8::from(fact.value_bits.is_some())])?;
        if let Some(bits) = fact.value_bits {
            writer.write_all(&bits.to_le_bytes())?;
        }
        match fact.origin {
            Origin::Process { start_sample, sample_rate, frames, offset } => {
                writer.write_all(&[0])?;
                writer.write_all(&start_sample.to_le_bytes())?;
                for value in [sample_rate, frames, offset] {
                    writer.write_all(&value.to_le_bytes())?;
                }
            }
            Origin::InactiveFlush { raw_offset } => {
                writer.write_all(&[1])?;
                writer.write_all(&raw_offset.to_le_bytes())?;
            }
            Origin::ActiveFlush { raw_offset } => {
                writer.write_all(&[2])?;
                writer.write_all(&raw_offset.to_le_bytes())?;
            }
            Origin::MainQuery { rescan_flags } => {
                writer.write_all(&[3])?;
                writer.write_all(&rescan_flags.to_le_bytes())?;
            }
        }
        for value in [fact.hidden, fact.periodic] {
            writer.write_all(&[match value {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            }])?;
        }
        text_write(writer, fact.name.as_deref())?;
        text_write(writer, fact.module.as_deref())?;
        writer.write_all(&[u8::from(fact.info_flags.is_some())])?;
        if let Some(flags) = fact.info_flags {
            writer.write_all(&flags.to_le_bytes())?;
        }
        text_write(writer, fact.display.as_deref())?;
    }
    Ok(())
}

pub(super) fn read_parameters(
    reader: &mut impl Read,
    object: Option<crate::NativeAudioObjectObservation>,
) -> io::Result<(u64, Vec<Fact>)> {
    let revision = u64_read(reader)?;
    let count = usize::try_from(u32_read(reader)?).map_err(|_| invalid("parameter count"))?;
    if count > mondrian_core::NATIVE_AUDIO_PARAMETER_FACTS || (count != 0 && object.is_none()) {
        return Err(invalid("parameter count/object"));
    }
    let mut storage = std::alloc::Layout::array::<Fact>(count)
        .map_err(|_| invalid("parameter vector storage"))?
        .size();
    if storage > mondrian_core::NATIVE_AUDIO_PARAMETER_FACT_BYTES {
        return Err(invalid("parameter vector bound"));
    }
    let mut facts = Vec::with_capacity(count);
    for _ in 0..count {
        let event_revision = u64_read(reader)?;
        if event_revision > revision {
            return Err(invalid("parameter source revision"));
        }
        let parameter = mondrian_core::ParameterId::new(
            text_read(reader, 255, &mut storage)?
                .ok_or_else(|| invalid("parameter identity absent"))?,
        )
        .map_err(|_| invalid("parameter schema ID"))?;
        let event_flags = u32_read(reader)?;
        let value_bits = if flag(reader)? {
            Some(u64_read(reader)?)
        } else {
            None
        };
        let mut tag = [0];
        reader.read_exact(&mut tag)?;
        let origin = match tag[0] {
            0 => {
                let mut start = [0; 8];
                reader.read_exact(&mut start)?;
                Origin::Process {
                    start_sample: i64::from_le_bytes(start),
                    sample_rate: u32_read(reader)?,
                    frames: u32_read(reader)?,
                    offset: u32_read(reader)?,
                }
            }
            1 => Origin::InactiveFlush { raw_offset: u32_read(reader)? },
            2 => Origin::ActiveFlush { raw_offset: u32_read(reader)? },
            3 => Origin::MainQuery { rescan_flags: u32_read(reader)? },
            _ => return Err(invalid("parameter origin tag")),
        };
        let mut presentation = [None; 2];
        for value in &mut presentation {
            let mut tag = [0];
            reader.read_exact(&mut tag)?;
            *value = match tag[0] {
                0 => None,
                1 => Some(false),
                2 => Some(true),
                _ => return Err(invalid("parameter presentation bool")),
            };
        }
        let [hidden, periodic] = presentation;
        let name = text_read(reader, 255, &mut storage)?;
        let module = text_read(reader, 1023, &mut storage)?;
        let info_flags = if flag(reader)? {
            Some(u32_read(reader)?)
        } else {
            None
        };
        let display = text_read(reader, 1023, &mut storage)?;
        facts.push(Fact {
            revision: event_revision,
            parameter,
            event_flags,
            value_bits,
            origin,
            hidden,
            periodic,
            name,
            module,
            info_flags,
            display,
        });
    }
    mondrian_core::NativeAudioParameterObservation::validate_facts(&facts)
        .map_err(|_| invalid("parameter decoded storage/shape"))?;
    Ok((revision, facts))
}

#[cfg(test)]
mod parameter_wire_contracts {
    use super::*;
    fn object() -> crate::NativeAudioObjectObservation {
        crate::NativeAudioObjectObservation::new(
            std::num::NonZeroU32::new(42).expect("pid"),
            std::num::NonZeroU64::new(7).expect("actual object"),
        )
    }
    fn fact() -> Fact {
        Fact {
            revision: 9,
            parameter: mondrian_core::ParameterId::new("clap.param.17").expect("id"),
            event_flags: 2,
            value_bits: Some(0.75_f64.to_bits()),
            origin: Origin::Process {
                start_sample: -7,
                sample_rate: 48000,
                frames: 4,
                offset: 3,
            },
            hidden: Some(false),
            periodic: Some(true),
            name: Some("Gain".into()),
            module: None,
            info_flags: Some(4),
            display: Some("-2.5 dB".into()),
        }
    }
    #[test]
    fn native_parameter_wire_independent_bytes_keep_signed_time_flags_and_info() {
        let actual = fact();
        let mut bytes = Vec::new();
        write_parameters(&mut bytes, 9, &vec![actual.clone()], Some(object()))
            .expect("write current body");
        let mut expected = Vec::new();
        expected.extend(9_u64.to_le_bytes());
        expected.extend(1_u32.to_le_bytes());
        expected.extend(9_u64.to_le_bytes());
        expected.push(1);
        expected.extend(13_u32.to_le_bytes());
        expected.extend(b"clap.param.17");
        expected.extend(2_u32.to_le_bytes());
        expected.push(1);
        expected.extend(0.75_f64.to_bits().to_le_bytes());
        expected.push(0);
        expected.extend((-7_i64).to_le_bytes());
        for value in [48000_u32, 4, 3] {
            expected.extend(value.to_le_bytes());
        }
        expected.extend([1, 2]);
        expected.push(1);
        expected.extend(4_u32.to_le_bytes());
        expected.extend(b"Gain");
        expected.push(0);
        expected.push(1);
        expected.extend(4_u32.to_le_bytes());
        expected.push(1);
        expected.extend(7_u32.to_le_bytes());
        expected.extend(b"-2.5 dB");
        assert_eq!(
            bytes, expected,
            "oracle builds each current field independently"
        );
        let (revision, decoded) =
            read_parameters(&mut bytes.as_slice(), Some(object())).expect("read current body");
        assert_eq!(revision, 9);
        assert_eq!(decoded, vec![actual]);
    }
    #[test]
    fn native_parameter_wire_refuses_missing_object_over_count_and_invalid_bool() {
        let mut bytes = Vec::new();
        write_parameters(&mut bytes, 9, &vec![fact()], Some(object())).expect("write");
        assert!(read_parameters(&mut bytes.as_slice(), None).is_err());
        let mut oversized = bytes.clone();
        oversized[8..12].copy_from_slice(&1153_u32.to_le_bytes());
        assert!(read_parameters(&mut oversized.as_slice(), Some(object())).is_err());
        // 12 envelope +8 revision +1 presence +4 length +13 ID +4 flags +1 value-presence +8 value +1 origin +20 origin-fields.
        let hidden = 12 + 8 + 1 + 4 + 13 + 4 + 1 + 8 + 1 + 20;
        bytes[hidden] = 3;
        assert!(read_parameters(&mut bytes.as_slice(), Some(object())).is_err());
    }
    #[test]
    fn native_parameter_wire_refuses_nonfinite_and_impossible_process_extent() {
        let mut nonfinite = fact();
        nonfinite.value_bits = Some(f64::NAN.to_bits());
        assert!(write_parameters(&mut Vec::new(), 9, &vec![nonfinite], Some(object())).is_err());
        let mut outside = fact();
        outside.origin = Origin::Process {
            start_sample: 0,
            sample_rate: 48000,
            frames: 4,
            offset: 4,
        };
        assert!(write_parameters(&mut Vec::new(), 9, &vec![outside], Some(object())).is_err());
        let mut future = fact();
        future.revision = 10;
        assert!(write_parameters(&mut Vec::new(), 9, &vec![future], Some(object())).is_err());
    }
}
