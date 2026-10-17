//! Startup-only carrier: JSON metadata followed by the original opaque bytes.
//!
//! The two body lengths share the existing control-frame bound. These are wire
//! and individual allocation ceilings, not a complete finite startup peak grant:
//! parent metadata owners overlap during preflight; child metadata bytes overlap
//! decoded metadata until parsing finishes, then raw bytes overlap adapter decode.
//! The retained factory payload and existing Session/seed grants keep their own
//! unchanged ownership and accounting. No plugin state is interpreted here.

use super::{
    PrepareWire, MAX_CONTROL_FRAME_BYTES, MAX_PREPARATION_PAYLOAD_BYTES, PROTOCOL_VERSION,
};
use std::io::{self, Read, Write};

fn invalid(detail: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}

fn raw_extent(metadata: &PrepareWire) -> io::Result<usize> {
    let bytes = usize::try_from(metadata.payload_bytes)
        .map_err(|_| invalid("preparation payload length exceeds the host address space"))?;
    if bytes > MAX_PREPARATION_PAYLOAD_BYTES {
        return Err(invalid(format!(
            "audio Worker preparation payload exceeds the protocol bound (raw={bytes}, raw_limit={MAX_PREPARATION_PAYLOAD_BYTES})"
        )));
    }
    Ok(bytes)
}

fn combined_extent(metadata_bytes: usize, raw_bytes: usize) -> io::Result<()> {
    if metadata_bytes
        .checked_add(raw_bytes)
        .is_none_or(|bytes| bytes > MAX_CONTROL_FRAME_BYTES)
    {
        return Err(invalid(format!(
            "audio Worker preparation body exceeds the protocol bound (metadata={metadata_bytes}, raw={raw_bytes}, total_limit={MAX_CONTROL_FRAME_BYTES})"
        )));
    }
    Ok(())
}

// Vec capacity, including any allocator over-reservation, must fit this one
// allocation's explicit ceiling. Logical length is checked separately above.
fn reserve(bytes: usize, capacity_limit: usize) -> io::Result<Vec<u8>> {
    let mut storage = Vec::new();
    storage.try_reserve_exact(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            "startup buffer allocation refused",
        )
    })?;
    if storage.capacity() > capacity_limit {
        return Err(invalid("startup buffer actual capacity exceeds its bound"));
    }
    Ok(storage)
}

/// Encoded before spawn. Only metadata is owned; the exact retained adapter
/// payload is borrowed and cannot change between measurement and transmission.
pub(in crate::processor_isolation) struct EncodedPrepare<'a> {
    metadata: Vec<u8>,
    payload: &'a [u8],
}

impl<'a> EncodedPrepare<'a> {
    pub(in crate::processor_isolation) fn new(
        metadata: &PrepareWire,
        payload: &'a [u8],
    ) -> io::Result<Self> {
        if metadata.schema_version != PROTOCOL_VERSION {
            return Err(invalid(
                "parent preparation envelope has another protocol version",
            ));
        }
        let raw_bytes = raw_extent(metadata)?;
        if raw_bytes != payload.len() {
            return Err(invalid(
                "preparation payload length differs from its metadata",
            ));
        }
        let metadata_limit = MAX_CONTROL_FRAME_BYTES
            .checked_sub(raw_bytes)
            .ok_or_else(|| invalid("preparation body extent overflowed"))?;
        struct Count {
            bytes: usize,
            limit: usize,
        }
        impl Write for Count {
            fn write(&mut self, input: &[u8]) -> io::Result<usize> {
                self.bytes =
                    self.bytes.checked_add(input.len()).filter(|n| *n <= self.limit).ok_or_else(
                        || invalid("preparation metadata exceeds its remaining body bound"),
                    )?;
                Ok(input.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count { bytes: 0, limit: metadata_limit };
        serde_json::to_writer(&mut count, metadata).map_err(|_| {
            invalid(format!(
                "audio Worker preparation metadata exceeds or cannot encode within the protocol bound (raw={raw_bytes}, metadata_limit={metadata_limit}, total_limit={MAX_CONTROL_FRAME_BYTES})"
            ))
        })?;
        combined_extent(count.bytes, raw_bytes)?;
        let mut bytes = reserve(count.bytes, metadata_limit)?;
        struct Exact<'a> {
            bytes: &'a mut Vec<u8>,
            limit: usize,
        }
        impl Write for Exact<'_> {
            fn write(&mut self, input: &[u8]) -> io::Result<usize> {
                self.bytes
                    .len()
                    .checked_add(input.len())
                    .filter(|n| *n <= self.limit)
                    .ok_or_else(|| invalid("preparation metadata changed its measured extent"))?;
                self.bytes.extend_from_slice(input);
                Ok(input.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        // Both passes use the same concrete serializer and immutable metadata.
        // The bounded writer cannot grow beyond the fallibly reserved capacity.
        serde_json::to_writer(Exact { bytes: &mut bytes, limit: count.bytes }, metadata)
            .map_err(|_| invalid("preparation metadata encoding exceeded its measured extent"))?;
        if bytes.len() != count.bytes {
            return Err(invalid(
                "preparation metadata changed between serialization passes",
            ));
        }
        Ok(Self { metadata: bytes, payload })
    }

    /// Consuming the preflight releases encoded metadata immediately after this
    /// write, before the caller allocates/decodes Ready on the same connection.
    pub(in crate::processor_isolation) fn write_to(
        self,
        writer: &mut impl Write,
    ) -> io::Result<()> {
        let length = u32::try_from(self.metadata.len())
            .map_err(|_| invalid("startup metadata length is not wire-representable"))?;
        writer.write_all(&length.to_le_bytes())?;
        writer.write_all(&self.metadata)?;
        writer.write_all(self.payload)?;
        writer.flush()
    }
}

// This entire validation phase completes before any raw-payload allocation or
// read. The metadata byte owner is released before reserving the raw owner.
fn read_metadata(reader: &mut impl Read, nonce: &str) -> io::Result<(PrepareWire, usize)> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| invalid("startup metadata length exceeds the host address space"))?;
    if length == 0 || length > MAX_CONTROL_FRAME_BYTES {
        return Err(invalid(
            "startup metadata length exceeds the protocol bound",
        ));
    }
    let mut body = reserve(length, MAX_CONTROL_FRAME_BYTES)?;
    body.resize(length, 0);
    reader.read_exact(&mut body)?;
    let metadata: PrepareWire = serde_json::from_slice(&body)
        .map_err(|_| invalid("invalid current preparation metadata"))?;
    drop(body);
    if metadata.schema_version != PROTOCOL_VERSION || metadata.nonce != nonce {
        return Err(invalid(
            "parent preparation envelope failed version or nonce validation",
        ));
    }
    let raw_bytes = raw_extent(&metadata)?;
    combined_extent(length, raw_bytes)?;
    Ok((metadata, raw_bytes))
}

pub(in crate::processor_isolation) fn read_prepare_frame(
    reader: &mut impl Read,
    nonce: &str,
) -> io::Result<(PrepareWire, Vec<u8>)> {
    let (metadata, raw_bytes) = read_metadata(reader, nonce)?;
    let mut payload = reserve(raw_bytes, MAX_PREPARATION_PAYLOAD_BYTES)?;
    payload.resize(raw_bytes, 0);
    reader.read_exact(&mut payload)?;
    Ok((metadata, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processor_isolation::native_worker_lane_wire::NativeWorkerLaneWire;
    use crate::processor_isolation::protocol::{
        read_json_frame, write_json_frame, ExecutionContractWire, ReadyResultWire, ReadyWire,
        TailWire, WorkerResponse,
    };
    use crate::AudioProcessorNoteInputContract;
    use mondrian_core::AudioChannelLayout;
    use std::io::Cursor;

    fn metadata(raw_bytes: usize) -> PrepareWire {
        PrepareWire {
            musical_time_reference: None,
            schema_version: 18,
            native_lane: NativeWorkerLaneWire::Inline {},
            nonce: "same-startup-nonce".into(),
            payload_bytes: raw_bytes as u64,
            execution_contract: ExecutionContractWire {
                algorithmic_latency_frames: 0,
                tail: TailWire::None,
                requires_state_entry: true,
                realtime_capable: true,
                offline_capable: true,
                plugin_session_scratch_bytes: 0,
            },
            auxiliary_buses: Vec::new(),
            auxiliary_outputs: Vec::new(),
            note_input_contract: AudioProcessorNoteInputContract::default(),
            parameter_ids: Vec::new(),
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 256,
            realtime: false,
            shared_path: "exact shared path".into(),
            shared_bytes: 2048,
        }
    }

    fn header_bytes(metadata: &PrepareWire) -> Vec<u8> {
        let body = serde_json::to_vec(metadata).unwrap();
        let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&body);
        bytes
    }

    fn padded_metadata(raw_bytes: usize, target_metadata_bytes: usize) -> PrepareWire {
        let mut result = metadata(raw_bytes);
        let length = serde_json::to_vec(&result).unwrap().len();
        result
            .shared_path
            .push_str(&"x".repeat(target_metadata_bytes.checked_sub(length).unwrap()));
        assert_eq!(
            serde_json::to_vec(&result).unwrap().len(),
            target_metadata_bytes
        );
        result
    }

    #[test]
    fn startup_raw_payload_preserves_non_json_and_adapter_json_without_consuming_following_frames()
    {
        let payloads = [
            (0..=255).collect::<Vec<u8>>(),
            // The generic transport preserves concrete adapter JSON too, without
            // interpreting state/class identity or claiming SDK compatibility.
            br#"{"class_id":"ordinary-vst3-codec-case","state":[0,128,255]}"#.to_vec(),
        ];
        for payload in payloads {
            let metadata = metadata(payload.len());
            let mut stream = Vec::new();
            EncodedPrepare::new(&metadata, &payload).unwrap().write_to(&mut stream).unwrap();
            let body_length = u32::from_le_bytes(stream[..4].try_into().unwrap()) as usize;
            assert_eq!(body_length, serde_json::to_vec(&metadata).unwrap().len());
            assert_eq!(&stream[4 + body_length..], payload);
            let ready = ReadyWire {
                schema_version: 18,
                result: ReadyResultWire::Rejected { detail: "alignment-only".into() },
            };
            write_json_frame(&mut stream, &ready).unwrap();
            WorkerResponse::Completed.write_to(&mut stream).unwrap();
            let mut stream = Cursor::new(stream);
            let (decoded, raw) = read_prepare_frame(&mut stream, &metadata.nonce).unwrap();
            assert_eq!(decoded, metadata);
            assert_eq!(raw, payload);
            assert_eq!(read_json_frame::<ReadyWire>(&mut stream).unwrap(), ready);
            assert_eq!(
                WorkerResponse::read_from(&mut stream).unwrap(),
                WorkerResponse::Completed
            );
            assert_eq!(stream.position() as usize, stream.get_ref().len());
        }
    }

    #[test]
    fn startup_raw_empty_and_half_megabyte_payloads_use_one_bounded_raw_owner() {
        for length in [0, 512 * 1024] {
            let payload = (0..length).map(|i| i as u8).collect::<Vec<_>>();
            let metadata = metadata(payload.len());
            let mut bytes = Vec::new();
            EncodedPrepare::new(&metadata, &payload).unwrap().write_to(&mut bytes).unwrap();
            assert_eq!(
                bytes.len(),
                4 + serde_json::to_vec(&metadata).unwrap().len() + length
            );
            let (decoded, raw) =
                read_prepare_frame(&mut bytes.as_slice(), &metadata.nonce).unwrap();
            assert_eq!(decoded, metadata);
            assert_eq!(raw, payload);
            assert!(raw.capacity() <= 512 * 1024);
            if length == 0 {
                assert_eq!(raw.capacity(), 0);
            }
        }
    }

    #[test]
    fn startup_raw_combined_body_includes_exact_metadata_and_excludes_only_four_byte_prefix() {
        let payload = vec![255; 512 * 1024];
        let metadata = padded_metadata(payload.len(), 512 * 1024);
        let mut bytes = Vec::new();
        EncodedPrepare::new(&metadata, &payload).unwrap().write_to(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 4 + 1024 * 1024);
        let (decoded, raw) = read_prepare_frame(&mut bytes.as_slice(), &metadata.nonce).unwrap();
        assert_eq!(decoded, metadata);
        assert_eq!(raw, payload);
        let oversized = padded_metadata(payload.len(), 512 * 1024 + 1);
        assert!(EncodedPrepare::new(&oversized, &payload).is_err());
        // Only a metadata header is present. The same decoder refuses the sum
        // before it can allocate or read the announced raw-payload phase.
        let bytes = header_bytes(&oversized);
        let mut input = Cursor::new(bytes);
        let error = read_prepare_frame(&mut input, &metadata.nonce).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("preparation body exceeds the protocol bound"));
        assert_eq!(input.position() as usize, input.get_ref().len());
    }

    #[test]
    fn startup_raw_rejects_mismatched_or_excessive_raw_extents_before_encoding_or_reading() {
        let payload = [7, 9];
        assert!(EncodedPrepare::new(&metadata(1), &payload).is_err());
        for length in [512 * 1024 + 1, usize::MAX] {
            let metadata = metadata(length);
            assert!(EncodedPrepare::new(&metadata, &payload).is_err());
            let bytes = header_bytes(&metadata);
            let mut input = Cursor::new(bytes);
            let error = read_prepare_frame(&mut input, &metadata.nonce).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(input.position() as usize, input.get_ref().len());
        }
        assert!(combined_extent(usize::MAX, 1).is_err());
    }

    #[test]
    fn startup_raw_validates_current_version_nonce_and_mandatory_shape_before_payload_phase() {
        let good = metadata(256);
        for version in [8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 19] {
            let mut old = good.clone();
            old.schema_version = version;
            assert!(EncodedPrepare::new(&old, &[0; 256]).is_err());
            let mut input = Cursor::new(header_bytes(&old));
            assert_eq!(
                read_prepare_frame(&mut input, &good.nonce).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(input.position() as usize, input.get_ref().len());
        }
        let mut input = Cursor::new(header_bytes(&good));
        assert_eq!(
            read_prepare_frame(&mut input, "different-authenticated-nonce")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(input.position() as usize, input.get_ref().len());
        for legacy_payload in [false, true] {
            let mut value = serde_json::to_value(&good).unwrap();
            if legacy_payload {
                value["payload"] = serde_json::json!([1, 2, 3]);
            } else {
                value.as_object_mut().unwrap().remove("payload_bytes");
            }
            let body = serde_json::to_vec(&value).unwrap();
            let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
            bytes.extend_from_slice(&body);
            let mut input = Cursor::new(bytes);
            assert_eq!(
                read_prepare_frame(&mut input, &good.nonce).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(input.position() as usize, input.get_ref().len());
        }
    }

    #[test]
    fn startup_raw_invalid_metadata_prefix_refuses_before_any_body_read() {
        for length in [0_u32, 1024 * 1024 + 1, u32::MAX] {
            let mut input = Cursor::new(length.to_le_bytes());
            assert_eq!(
                read_prepare_frame(&mut input, "unused").unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(input.position(), 4);
        }
    }

    #[test]
    fn startup_raw_truncation_cannot_finish_the_envelope() {
        let payload = [0, 127, 128, 255];
        let metadata = metadata(payload.len());
        let mut bytes = Vec::new();
        EncodedPrepare::new(&metadata, &payload).unwrap().write_to(&mut bytes).unwrap();
        let metadata_end = bytes.len() - payload.len();
        for length in [0, 1, 3, 4, metadata_end - 1, metadata_end, bytes.len() - 1] {
            let mut input = &bytes[..length];
            assert_eq!(
                read_prepare_frame(&mut input, &metadata.nonce).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
    }

    #[test]
    fn startup_and_ready_require_explicit_nullable_music_and_preserve_exact_reference() {
        use mondrian_core::{MusicalTimeReference, TimelineTime};
        let reference = |zero| {
            MusicalTimeReference::new(
                TimelineTime::new(zero, 7).unwrap(),
                TimelineTime::new(3, 4).unwrap(),
            )
            .unwrap()
        };
        for music in [None, Some(reference(-19)), Some(reference(23))] {
            let mut prepare = metadata(3);
            prepare.musical_time_reference = music;
            let mut bytes = Vec::new();
            EncodedPrepare::new(&prepare, &[0, 128, 255])
                .unwrap()
                .write_to(&mut bytes)
                .unwrap();
            let (actual, payload) =
                read_prepare_frame(&mut bytes.as_slice(), &prepare.nonce).unwrap();
            assert_eq!(actual.musical_time_reference, music);
            assert_eq!(payload, [0, 128, 255]);
            let ready = ReadyWire {
                schema_version: 18,
                result: ReadyResultWire::Ready {
                    musical_time_reference: music,
                    execution_contract: prepare.execution_contract,
                    auxiliary_buses: Vec::new(),
                    auxiliary_outputs: Vec::new(),
                    note_input_contract: AudioProcessorNoteInputContract::default(),
                    input_names: Vec::new(),
                },
            };
            let mut value = serde_json::to_value(&ready).unwrap();
            assert_eq!(
                serde_json::from_value::<ReadyWire>(value.clone()).unwrap(),
                ready
            );
            for outer in [false, true] {
                let mut unknown = value.clone();
                if outer {
                    unknown["unknown_music"] = serde_json::json!(null);
                } else {
                    unknown["result"]["unknown_music"] = serde_json::json!(null);
                }
                assert!(serde_json::from_value::<ReadyWire>(unknown).is_err());
            }
            value["result"].as_object_mut().unwrap().remove("musical_time_reference");
            assert!(serde_json::from_value::<ReadyWire>(value).is_err());
        }
        let good = metadata(3);
        for malformed in 0..3 {
            let mut value = serde_json::to_value(&good).unwrap();
            match malformed {
                0 => {
                    value.as_object_mut().unwrap().remove("musical_time_reference");
                }
                1 => {
                    value["musical_time_reference"] = serde_json::json!({
                        "quarter_note_zero": {"numerator":0,"denominator":1},
                        "quarter_note_duration": {"numerator":0,"denominator":1}
                    });
                }
                _ => {
                    value["unknown_music"] = serde_json::json!(null);
                }
            }
            let body = serde_json::to_vec(&value).unwrap();
            let metadata_end = 4 + body.len();
            let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
            bytes.extend(body);
            bytes.extend([0, 128, 255]);
            let mut input = Cursor::new(bytes);
            assert_eq!(
                read_prepare_frame(&mut input, &good.nonce).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                input.position() as usize,
                metadata_end,
                "raw payload must remain unread"
            );
        }
    }
}
