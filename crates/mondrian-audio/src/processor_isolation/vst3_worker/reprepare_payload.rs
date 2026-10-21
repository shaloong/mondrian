//! Borrowed replacement of state in this Factory's already typed-admitted,
//! immutable VST3 metadata. This is not an external metadata admission codec;
//! native-author restoration continues to use the original typed codec.

use super::{invalid, MAX_STATE_BYTES};
use crate::AudioProcessorHostError;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata<'a> {
    #[serde(borrow)]
    schema_version: &'a RawValue,
    #[serde(borrow)]
    main_audio_input: &'a RawValue,
    #[serde(borrow)]
    audio_output_count: &'a RawValue,
    #[serde(borrow)]
    main_audio_output_count: &'a RawValue,
    #[serde(borrow)]
    event_bus_contract: &'a RawValue,
    #[serde(borrow)]
    binary_path: &'a RawValue,
    #[serde(borrow)]
    class_id: &'a RawValue,
    #[serde(default, borrow)]
    vendor: Option<&'a RawValue>,
    #[serde(borrow)]
    binary_sha256: &'a RawValue,
    #[serde(default, borrow)]
    state: Option<&'a RawValue>,
    #[serde(borrow)]
    parameters: &'a RawValue,
    #[serde(borrow)]
    parameter_ids: &'a RawValue,
}

const FIELD_NAMES: [&str; 12] = [
    "schema_version",
    "main_audio_input",
    "audio_output_count",
    "main_audio_output_count",
    "event_bus_contract",
    "binary_path",
    "class_id",
    "vendor",
    "binary_sha256",
    "state",
    "parameters",
    "parameter_ids",
];
const FIELD_NAME_BYTES: usize = {
    let mut bytes = 0;
    let mut index = 0;
    while index < FIELD_NAMES.len() {
        bytes += FIELD_NAMES[index].len();
        index += 1;
    }
    bytes
};

// serde_json 1.0.150 SliceRead borrows raw values; ignore_value uses its one
// reusable Vec as an iterative nesting stack, and escaped root keys reuse it.
// A requested-layout envelope of 16*(N+K) covers that input-sized scratch and
// cold single-layer escaped-key/Display diagnostics. K is derived from these
// TWELVE names: four bytes per name byte for the expected-field list plus 512 for
// fixed prefixes, punctuation and numeric locations. This is not allocator/SDK
// telemetry. Output and owning Factory clones are admitted separately.
pub(in crate::processor_isolation) fn reprepare_codec_bound(n: usize) -> Option<usize> {
    if n > super::super::MAX_PREPARATION_PAYLOAD_BYTES {
        return None;
    }
    n.checked_add(FIELD_NAME_BYTES.checked_mul(4)?.checked_add(512)?)?
        .checked_mul(16)
}

pub(in crate::processor_isolation) fn reprepare_payload_output_bound(
    n: usize,
    configuration_bytes: usize,
) -> Option<usize> {
    reprepare_codec_bound(n)?;
    if configuration_bytes > MAX_STATE_BYTES {
        return None;
    }
    // At most three decimal digits and a comma per byte, plus all old raw
    // fragments, twelve quoted keys/colons/commas, braces, [] and default null.
    configuration_bytes
        .checked_mul(4)?
        .checked_add(n)?
        .checked_add(FIELD_NAME_BYTES + 4 * FIELD_NAMES.len() + 8)
}

pub(in crate::processor_isolation) fn payload_with_reprepare_configuration(
    payload: &[u8],
    configuration: &[u8],
    report_peak: &mut dyn FnMut(usize),
) -> Result<Vec<u8>, AudioProcessorHostError> {
    let codec = reprepare_codec_bound(payload.len())
        .ok_or_else(|| invalid("retained VST3 metadata exceeds the raw codec bound"))?;
    reprepare_payload_output_bound(payload.len(), configuration.len())
        .ok_or_else(|| invalid("retained VST3 configuration exceeds the raw codec bound"))?;
    report_peak(codec);
    if payload.iter().copied().find(|b| !matches!(b, b' ' | b'\n' | b'\r' | b'\t')) != Some(b'{') {
        return Err(invalid("retained VST3 metadata must be a JSON object"));
    }
    let view: Metadata<'_> = serde_json::from_slice(payload)
        .map_err(|error| invalid(format!("invalid retained VST3 metadata: {error}")))?;
    let version: u32 = serde_json::from_str(view.schema_version.get())
        .map_err(|error| invalid(format!("invalid retained VST3 version: {error}")))?;
    if version != super::PAYLOAD_VERSION {
        return Err(invalid("unsupported retained VST3 worker payload version"));
    }
    let output_count: u8 = serde_json::from_str(view.audio_output_count.get())
        .map_err(|error| invalid(format!("invalid retained VST3 output count: {error}")))?;
    let main_output_count: u8 = serde_json::from_str(view.main_audio_output_count.get())
        .map_err(|error| invalid(format!("invalid retained VST3 Main output prefix: {error}")))?;
    super::validate_main_audio_output_count(output_count, main_output_count)?;
    // Original Option fields admit both absent and null. The old state is never
    // decoded/allocated; absent vendor re-emits null just like the typed codec.
    let _old_state = view.state;
    struct Replacement<'a, 'b> {
        metadata: &'a Metadata<'b>,
        state: &'a [u8],
    }
    impl Serialize for Replacement<'_, '_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeStruct;
            let v = self.metadata;
            let mut object = serializer.serialize_struct("Vst3Payload", FIELD_NAMES.len())?;
            object.serialize_field("schema_version", v.schema_version)?;
            object.serialize_field("main_audio_input", v.main_audio_input)?;
            object.serialize_field("audio_output_count", v.audio_output_count)?;
            object.serialize_field("main_audio_output_count", v.main_audio_output_count)?;
            object.serialize_field("event_bus_contract", v.event_bus_contract)?;
            object.serialize_field("binary_path", v.binary_path)?;
            object.serialize_field("class_id", v.class_id)?;
            object.serialize_field("vendor", &v.vendor)?;
            object.serialize_field("binary_sha256", v.binary_sha256)?;
            object.serialize_field("state", &self.state)?;
            object.serialize_field("parameters", v.parameters)?;
            object.serialize_field("parameter_ids", v.parameter_ids)?;
            object.end()
        }
    }
    super::super::encode_native_configuration_payload_observed(
        &Replacement { metadata: &view, state: configuration },
        &mut |capacity| report_peak(codec.saturating_add(capacity)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_payload() -> String {
        serde_json::to_string(&super::super::Vst3Payload {
            schema_version: super::super::PAYLOAD_VERSION,
            main_audio_input: super::super::Vst3MainAudioInput::Present,
            audio_output_count: 3,
            main_audio_output_count: 2,
            event_bus_contract: super::super::Vst3EventBusContractV1::empty(),
            binary_path: std::env::current_exe().unwrap(),
            class_id: "00112233445566778899aabbccddeeff".to_owned(),
            vendor: Some("vendor".to_owned()),
            binary_sha256: [7; 32],
            state: Some(vec![1, 2]),
            parameters: Vec::new(),
            parameter_ids: Vec::new(),
        })
        .unwrap()
    }

    #[test]
    fn vst3_reprepare_raw_codec_preserves_eleven_field_fragments_and_option_defaults() {
        let payload = test_payload().replace(
            "\"parameters\":[]",
            "\"parameters\":[ {\"raw\": [1e99999,-0.0,1.2300]} ]",
        );
        // Synthetic scanner stress, not typed-admitted parameter metadata.
        let before: Metadata<'_> = serde_json::from_str(&payload).unwrap();
        let mut peak = 0;
        let output =
            payload_with_reprepare_configuration(payload.as_bytes(), &[255, 0, 9], &mut |n| {
                peak = peak.max(n)
            })
            .unwrap();
        let after: Metadata<'_> = serde_json::from_slice(&output).unwrap();
        let fragments = |v: &Metadata<'_>| {
            [
                v.schema_version.get().to_owned(),
                v.main_audio_input.get().to_owned(),
                v.audio_output_count.get().to_owned(),
                v.main_audio_output_count.get().to_owned(),
                v.event_bus_contract.get().to_owned(),
                v.binary_path.get().to_owned(),
                v.class_id.get().to_owned(),
                v.vendor.unwrap().get().to_owned(),
                v.binary_sha256.get().to_owned(),
                v.parameters.get().to_owned(),
                v.parameter_ids.get().to_owned(),
            ]
        };
        assert_eq!(fragments(&before), fragments(&after));
        assert_eq!(after.state.unwrap().get(), "[255,0,9]");
        assert!(peak >= reprepare_codec_bound(payload.len()).unwrap() + output.capacity());
        assert!(
            peak <= reprepare_codec_bound(payload.len()).unwrap()
                + reprepare_payload_output_bound(payload.len(), 3).unwrap()
        );
        for optional in [
            test_payload(),
            test_payload().replace("\"vendor\":\"vendor\",", ""),
            test_payload().replace("\"vendor\":\"vendor\"", "\"vendor\":null"),
            test_payload().replace("\"vendor\":\"vendor\"", "\"vendor\":\"\""),
        ] {
            for optional in [
                optional.clone(),
                optional.replace("\"state\":[1,2],", ""),
                optional.replace("\"state\":[1,2]", "\"state\":null"),
            ] {
                let output =
                    payload_with_reprepare_configuration(optional.as_bytes(), &[], &mut |_| {})
                        .unwrap();
                let decoded = super::super::decode_payload(&output).unwrap();
                assert_eq!(decoded.state.as_deref(), Some(&[][..]));
                assert_eq!(
                    decoded.vendor,
                    super::super::decode_payload(optional.as_bytes()).unwrap().vendor
                );
            }
        }
    }

    #[test]
    fn vst3_reprepare_raw_codec_rejects_shape_duplicates_and_bounds_before_output() {
        let payload = test_payload();
        for invalid in [
            payload.replacen("{", "{\"extra\":0,", 1),
            payload.replacen("{", "{\"class_\\u0069d\":\"duplicate\",", 1),
            payload.replacen("{", "{\"v\\u0065ndor\":null,", 1),
            payload.replacen("{", "{\"vendor\":null,", 1),
            payload.replacen("{", "{\"state\":null,", 1),
            payload.replacen("{", "{\"audio_output_count\":3,", 1),
            payload.replace("\"schema_version\":5", "\"schema_version\":4"),
            payload.replace("\"schema_version\":5", "\"schema_version\":6"),
            payload.replacen("{", "{\"main_audio_output_count\":2,", 1),
            payload.replacen("{", "{\"main_audio_output_cou\\u006et\":2,", 1),
            payload.replace(
                "\"main_audio_output_count\":2",
                "\"main_audio_output_count\":0",
            ),
            payload.replace(
                "\"main_audio_output_count\":2",
                "\"main_audio_output_count\":4",
            ),
            payload.replace("\"main_audio_output_count\":2,", ""),
            payload.replace("\"audio_output_count\":3", "\"audio_output_count\":0"),
            payload.replace("\"audio_output_count\":3", "\"audio_output_count\":33"),
            payload.replace("\"audio_output_count\":3", "\"audio_output_count\":256"),
            payload.replace("\"schema_version\":5,", ""),
            "[]".to_owned(),
        ] {
            let mut peak = 0;
            assert!(
                payload_with_reprepare_configuration(invalid.as_bytes(), &[8], &mut |n| peak =
                    peak.max(n))
                .is_err()
            );
            assert_eq!(peak, reprepare_codec_bound(invalid.len()).unwrap());
        }
        for required in FIELD_NAMES.into_iter().filter(|name| !matches!(*name, "vendor" | "state"))
        {
            let mut value: serde_json::Value = serde_json::from_str(&payload).unwrap();
            value.as_object_mut().unwrap().remove(required);
            let missing = serde_json::to_vec(&value).unwrap();
            assert!(payload_with_reprepare_configuration(&missing, &[8], &mut |_| {}).is_err());
        }
        let first = payload.replacen("{", "{\"extra\":0,", 1).replace("\"schema_version\":5,", "");
        let error =
            payload_with_reprepare_configuration(first.as_bytes(), &[8], &mut |_| {}).unwrap_err();
        assert!(
            error.to_string().contains("unknown field `extra`"),
            "{error}"
        );
        let limit = super::super::super::MAX_PREPARATION_PAYLOAD_BYTES;
        assert!(reprepare_codec_bound(limit).is_some());
        assert!(reprepare_codec_bound(limit + 1).is_none());
        assert!(reprepare_codec_bound(usize::MAX).is_none());
        assert!(reprepare_payload_output_bound(payload.len(), MAX_STATE_BYTES).is_some());
        assert!(reprepare_payload_output_bound(payload.len(), MAX_STATE_BYTES + 1).is_none());
        assert!(reprepare_payload_output_bound(payload.len(), usize::MAX).is_none());
        // A valid raw object can still exceed the original concrete output cap;
        // count-first refusal must report codec custody, never truncate the seed.
        let large = payload.replace(
            "\"parameters\":[]",
            &format!(
                "\"parameters\":\"{}\"",
                "x".repeat(limit - payload.len() - 16)
            ),
        );
        let mut peak = 0;
        assert!(payload_with_reprepare_configuration(
            large.as_bytes(),
            &vec![255; MAX_STATE_BYTES],
            &mut |n| peak = peak.max(n)
        )
        .is_err());
        assert_eq!(peak, reprepare_codec_bound(large.len()).unwrap());
    }
}
