//! Strict v2 query-wire controls; owned DTOs are not native/DSP evidence.
use super::super::clap_note_input_contract::{
    offered_limits, ClapNoteInputContractProbe, ClapNoteProcessingOfferV2,
};
use super::*;
use crate::{AudioProcessorNoteInputContract, AudioProcessorNoteInputPort};
fn render() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: usize::MAX,
        public_output_lookahead_budget_frames: usize::MAX,
        compensation_delay_scratch_budget_bytes: usize::MAX,
    }
}
fn offer() -> ClapNoteProcessingOfferV2 {
    ClapNoteProcessingOfferV2 {
        schema_version: 2,
        notes: ClapNoteInputContractProbe {
            schema_version: 1,
            plugin_id: "fixture".to_owned(),
            binary_sha256: [7; 32],
            state_sha256: super::super::clap_note_input_metadata::state_digest(Some(&[1])),
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 64,
            offline: true,
            contract: AudioProcessorNoteInputContract {
                limits: offered_limits(),
                ports: vec![
                    AudioProcessorNoteInputPort {
                        native_id: 0x8000_0017,
                        native_index: 0,
                        port_key: "clap.note-input.2147483671".to_owned(),
                    },
                    AudioProcessorNoteInputPort {
                        native_id: 0,
                        native_index: 1,
                        port_key: "clap.note-input.0".to_owned(),
                    },
                ],
            },
        },
        processing: processing_result_for_notes(
            "fixture",
            render(),
            Vec::new(),
            AudioProcessorAuxiliaryInputContract::default(),
            17,
            TailLength::Finite(5),
        )
        .unwrap(),
    }
}
fn decode(bytes: &[u8]) -> Result<ClapNoteProcessingOfferV2, AudioProcessorHostError> {
    decode_processing_offer_v2(bytes, "fixture", [7; 32], Some(&[1]), render())
}
#[test]
fn v2_wire_has_complete_identity_and_legacy_request_omission_remains_byte_exact() {
    let legacy = br#"{"kind":"probe_notes","plugin_id":"fixture","sample_rate":48000,"channel_layout":"Mono","max_block_frames":64,"offline":true,"state":null}"#;
    let operation: DiscoveryOperation = serde_json::from_slice(legacy).unwrap();
    assert!(matches!(
        &operation,
        DiscoveryOperation::ProbeNotes { schema_version: 1, .. }
    ));
    assert_eq!(serde_json::to_vec(&operation).unwrap(), legacy);
    let bytes = encode_processing_offer_v2(&offer()).unwrap();
    assert!(bytes.len() as u64 <= MAX_RESPONSE_BYTES);
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.schema_version, 2);
    assert_eq!(decoded.notes.contract.ports[0].native_id, 0x8000_0017);
    let mut value = serde_json::to_value(operation).unwrap();
    value["schema_version"] = serde_json::json!(2);
    assert!(matches!(
        serde_json::from_value::<DiscoveryOperation>(value).unwrap(),
        DiscoveryOperation::ProbeNotes { schema_version: 2, .. }
    ));
}
#[test]
fn v2_wire_refuses_schema_identity_state_render_and_finite_policy_mismatch() {
    for (path, replacement) in [
        ("/schema_version", serde_json::json!(1)),
        ("/notes/schema_version", serde_json::json!(2)),
        ("/notes/plugin_id", serde_json::json!("other")),
        ("/notes/binary_sha256/0", serde_json::json!(8)),
        ("/notes/state_sha256", serde_json::Value::Null),
        ("/notes/sample_rate", serde_json::json!(44_100)),
        ("/notes/channel_layout", serde_json::json!({"Discrete":1})),
        ("/notes/max_block_frames", serde_json::json!(65)),
        ("/notes/offline", serde_json::json!(false)),
        (
            "/notes/contract/ports/0/port_key",
            serde_json::json!("clap.note-input.-2147483625"),
        ),
        ("/notes/contract/limits/live_voices", serde_json::json!(8)),
        ("/processing/plugin_id", serde_json::json!("other")),
        ("/processing/offline", serde_json::json!(false)),
        ("/processing/captured_state", serde_json::json!([1])),
        ("/processing/tail", serde_json::json!({"Finite":0})),
    ] {
        let mut value = serde_json::to_value(offer()).unwrap();
        *value.pointer_mut(path).unwrap() = replacement;
        assert!(
            decode(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{path}"
        );
    }
    let v1 = serde_json::to_vec(&offer().notes).unwrap();
    assert!(decode(&v1).is_err());
    let oversized = vec![b' '; MAX_RESPONSE_BYTES as usize + 1];
    assert!(decode(&oversized).is_err());
    let mut value = serde_json::to_value(offer()).unwrap();
    value.as_object_mut().unwrap().remove("processing");
    assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut value = serde_json::to_value(offer()).unwrap();
    value["extra"] = serde_json::json!(true);
    assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
}
