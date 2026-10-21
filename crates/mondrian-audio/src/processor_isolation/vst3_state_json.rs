//! Private JSON spelling for VST3 state bytes. The owning model remains
//! Option<Vec<u8>>; this module only formats or validates that field.

use base64::{display::Base64Display, engine::general_purpose::STANDARD, Engine as _};
use serde::{de, Deserializer, Serialize, Serializer};
use std::fmt;

use super::vst3_worker::MAX_STATE_BYTES;

const MAX_ENCODED_BYTES: usize = MAX_STATE_BYTES.div_ceil(3) * 4;

pub(super) fn encoded_len(raw: usize) -> Option<usize> {
    (raw <= MAX_STATE_BYTES).then_some(())?;
    raw.checked_add(2)?.checked_div(3)?.checked_mul(4)
}

// Check the alphabet, required padding, unused tail bits and exact raw length
// before allocating the sole decoded owner. No decode/re-encode String is used.
fn decoded_len(encoded: &str) -> Result<usize, &'static str> {
    let bytes = encoded.as_bytes();
    if bytes.len() > MAX_ENCODED_BYTES || !bytes.len().is_multiple_of(4) {
        return Err("VST3 state base64 length exceeds its bound or lacks padding");
    }
    if bytes.is_empty() {
        return Ok(0);
    }
    let padding = if bytes.ends_with(b"==") {
        2
    } else {
        usize::from(bytes.ends_with(b"="))
    };
    let data = &bytes[..bytes.len() - padding];
    let mut last = 0;
    for byte in data {
        last = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("VST3 state requires canonical padded base64"),
        };
    }
    if (padding == 2 && last & 15 != 0) || (padding == 1 && last & 3 != 0) {
        return Err("VST3 state base64 has nonzero unused bits");
    }
    let raw = bytes.len() / 4 * 3 - padding;
    if raw > MAX_STATE_BYTES {
        return Err("VST3 state exceeds the decoded state limit");
    }
    Ok(raw)
}

// A borrowed serialization view, never a second state owner. serde_json's
// collect_str streams Base64Display chunks into both existing bounded passes.
pub(super) struct StateBytes<'a>(pub(super) &'a [u8]);

impl Serialize for StateBytes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        encoded_len(self.0.len()).ok_or_else(|| {
            <S::Error as serde::ser::Error>::custom("VST3 state exceeds the decoded state limit")
        })?;
        serializer.collect_str(&Base64Display::new(self.0, &STANDARD))
    }
}

pub(super) fn serialize<S: Serializer>(
    state: &Option<Vec<u8>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match state {
        Some(bytes) => serializer.serialize_some(&StateBytes(bytes)),
        None => serializer.serialize_none(),
    }
}

struct StateVisitor;
impl<'de> de::Visitor<'de> for StateVisitor {
    type Value = Vec<u8>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded canonical padded base64 state")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        let length = decoded_len(value).map_err(E::custom)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(E::custom)?;
        bytes.resize(length, 0);
        let written = STANDARD.decode_slice(value, &mut bytes).map_err(E::custom)?;
        if written != length {
            return Err(E::custom("VST3 state decoded length changed"));
        }
        Ok(bytes)
    }
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<u8>>, D::Error> {
    struct OptionalState;
    impl<'de> de::Visitor<'de> for OptionalState {
        type Value = Option<Vec<u8>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("null or bounded canonical padded base64 state")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            deserializer.deserialize_str(StateVisitor).map(Some)
        }
    }
    deserializer.deserialize_option(OptionalState)
}

// Borrowed reprepare discards the old seed without allocating/decoding its raw
// bytes. JSON escape scratch, if any, remains within its existing codec bound.
pub(super) fn validate_json(value: &serde_json::value::RawValue) -> Result<(), serde_json::Error> {
    struct Spelling;
    impl<'de> de::Visitor<'de> for Spelling {
        type Value = ();
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("bounded canonical padded base64 state")
        }
        fn visit_str<E: de::Error>(self, value: &str) -> Result<(), E> {
            decoded_len(value).map(|_| ()).map_err(E::custom)
        }
    }
    serde_json::Deserializer::from_str(value.get()).deserialize_str(Spelling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct State {
        #[serde(default, with = "super")]
        state: Option<Vec<u8>>,
    }

    #[test]
    fn vst3_state_base64_preserves_options_and_all_tail_lengths() {
        for (raw, wire) in [
            (vec![], ""),
            (vec![255], "/w=="),
            (vec![255, 0], "/wA="),
            (vec![255, 0, 9], "/wAJ"),
        ] {
            let value = State { state: Some(raw) };
            let encoded = serde_json::to_string(&value).unwrap();
            assert_eq!(encoded, format!(r#"{{"state":"{wire}"}}"#));
            assert_eq!(serde_json::from_str::<State>(&encoded).unwrap(), value);
        }
        assert_eq!(serde_json::from_str::<State>("{}").unwrap().state, None);
        assert_eq!(
            serde_json::from_str::<State>(r#"{"state":null}"#).unwrap().state,
            None
        );
        assert_eq!(
            serde_json::to_string(&State { state: None }).unwrap(),
            r#"{"state":null}"#
        );
    }

    #[test]
    fn vst3_state_base64_refuses_noncanonical_and_overlimit_before_decode() {
        for invalid in [
            "A", "AA", "AAA", "A===", "====", "AA=A", "AA==AA==", "AB==", "AAB=", "AA--", "AA__",
            "AA\\n", "éAA",
        ] {
            assert!(decoded_len(invalid).is_err(), "{invalid}");
            let value = serde_json::json!({"state": invalid});
            assert!(serde_json::from_value::<State>(value).is_err(), "{invalid}");
        }
        for invalid in [
            serde_json::json!([]),
            serde_json::json!([0]),
            serde_json::json!(false),
            serde_json::json!({"bytes":"AA=="}),
        ] {
            assert!(serde_json::from_value::<State>(serde_json::json!({"state":invalid})).is_err());
        }
        let maximum = STANDARD.encode(vec![255; MAX_STATE_BYTES]);
        assert_eq!(decoded_len(&maximum), Ok(MAX_STATE_BYTES));
        // These have the same maximum encoded length: padding still distinguishes
        // the permitted raw limit from one extra decoded byte.
        let too_many = STANDARD.encode(vec![255; MAX_STATE_BYTES + 1]);
        assert_eq!(maximum.len(), too_many.len());
        assert!(decoded_len(&too_many).is_err());
        assert!(serde_json::from_value::<State>(serde_json::json!({"state":too_many})).is_err());
        assert_eq!(encoded_len(MAX_STATE_BYTES), Some(174_764));
        assert_eq!(encoded_len(MAX_STATE_BYTES + 1), None);
        assert_eq!(encoded_len(usize::MAX), None);
        assert!(serde_json::to_vec(&State { state: Some(vec![0; MAX_STATE_BYTES + 1]) }).is_err());
    }
}
