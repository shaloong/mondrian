//! Strict v6 query-wire controls; owned DTOs are not native/DSP evidence.
use super::super::clap_note_input_contract::{
    offered_limits, ClapNoteInputContractProbe, ClapNoteProcessingOfferV6,
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
fn offer() -> ClapNoteProcessingOfferV6 {
    ClapNoteProcessingOfferV6 {
        schema_version: 6,
        input_names: Vec::new(),
        notes: ClapNoteInputContractProbe {
            schema_version: 2,
            plugin_id: "fixture".to_owned(),
            binary_sha256: [7; 32],
            state_sha256: super::super::clap_note_input_metadata::state_digest(Some(&[1])),
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 64,
            offline: true,
            contract: AudioProcessorNoteInputContract {
                completion: crate::AudioNoteCompletionPolicy::NativeEnd,
                limits: offered_limits(),
                ports: vec![
                    AudioProcessorNoteInputPort {
                        channel_count: 16,
                        native_id: 0x8000_0017,
                        native_index: 0,
                        port_key: "clap.note-input.2147483671".to_owned(),
                    },
                    AudioProcessorNoteInputPort {
                        channel_count: 16,
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
            &ClapAudioPortMetadata {
                main_audio_input: ClapMainAudioInput::Present,
                contract: AudioProcessorAuxiliaryInputContract::default(),
                names: Vec::new(),
                main_audio_output_id: 0,
                outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            },
            17,
            TailLength::Finite(5),
        )
        .unwrap(),
    }
}
fn decode(bytes: &[u8]) -> Result<ClapNoteProcessingOfferV6, AudioProcessorHostError> {
    decode_processing_offer_v6(bytes, "fixture", [7; 32], Some(&[1]), render())
}
#[test]
fn note_wire_has_complete_identity_and_refuses_legacy_request_omission() {
    let legacy = br#"{"kind":"probe_notes","plugin_id":"fixture","sample_rate":48000,"channel_layout":"Mono","max_block_frames":64,"offline":true,"state":null}"#;
    assert!(serde_json::from_slice::<DiscoveryOperation>(legacy).is_err());
    let mut current = serde_json::from_slice::<serde_json::Value>(legacy).unwrap();
    current["schema_version"] = serde_json::json!(2);
    let operation: DiscoveryOperation = serde_json::from_value(current).unwrap();
    assert!(matches!(
        &operation,
        DiscoveryOperation::ProbeNotes { schema_version: 2, .. }
    ));
    validate_discovery_schema(&operation).unwrap();
    let bytes = encode_processing_offer_v6(&offer()).unwrap();
    assert!(bytes.len() as u64 <= MAX_RESPONSE_BYTES);
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.schema_version, 6);
    assert_eq!(decoded.notes.contract.ports[0].native_id, 0x8000_0017);
    let mut value = serde_json::to_value(operation).unwrap();
    value["schema_version"] = serde_json::json!(6);
    let full = serde_json::from_value::<DiscoveryOperation>(value).unwrap();
    assert!(matches!(
        &full,
        DiscoveryOperation::ProbeNotes { schema_version: 6, .. }
    ));
    validate_discovery_schema(&full).unwrap();
}
#[test]
fn v2_wire_refuses_schema_identity_state_render_and_finite_policy_mismatch() {
    for (path, replacement) in [
        ("/schema_version", serde_json::json!(1)),
        ("/schema_version", serde_json::json!(2)),
        ("/schema_version", serde_json::json!(3)),
        ("/schema_version", serde_json::json!(4)),
        ("/schema_version", serde_json::json!(5)),
        ("/processing/schema_version", serde_json::json!(2)),
        ("/processing/schema_version", serde_json::json!(1)),
        ("/notes/schema_version", serde_json::json!(1)),
        ("/notes/schema_version", serde_json::json!(3)),
        (
            "/notes/contract/completion",
            serde_json::json!("successful_off_block"),
        ),
        (
            "/notes/contract/ports/0/channel_count",
            serde_json::json!(1),
        ),
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
    for field in ["completion", "channel_count"] {
        let mut value = serde_json::to_value(offer()).unwrap();
        let object = if field == "completion" {
            &mut value["notes"]["contract"]
        } else {
            &mut value["notes"]["contract"]["ports"][0]
        };
        object.as_object_mut().unwrap().remove(field);
        assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut request = serde_json::json!({"kind":"probe_notes", "schema_version":2,
        "plugin_id":"fixture", "sample_rate":48000, "channel_layout":"Mono",
        "max_block_frames":64, "offline":true, "state":null});
    for version in [1, 3, 4, 5, 7] {
        request["schema_version"] = serde_json::json!(version);
        let operation = serde_json::from_value::<DiscoveryOperation>(request.clone()).unwrap();
        assert!(validate_discovery_schema(&operation).is_err());
    }
}

#[test]
fn v3_processing_shape_is_mandatory_and_optional_main_retains_auxiliary_bound() {
    let mut absent = offer();
    absent.processing.main_audio_input = ClapMainAudioInput::Absent;
    let decoded = decode(&encode_processing_offer_v6(&absent).unwrap()).unwrap();
    assert_eq!(decoded.main_audio_input(), ClapMainAudioInput::Absent);
    let mut value = serde_json::to_value(absent).unwrap();
    for key in [
        "main_audio_input",
        "main_audio_output_id",
        "auxiliary_outputs",
        "schema_version",
    ] {
        let mut missing = value.clone();
        missing["processing"].as_object_mut().unwrap().remove(key);
        assert!(decode(&serde_json::to_vec(&missing).unwrap()).is_err());
    }
    let mut extra = value.clone();
    extra["processing"]["extra"] = serde_json::json!(true);
    assert!(decode(&serde_json::to_vec(&extra).unwrap()).is_err());
    value["processing"]["auxiliary_buses"] =
        serde_json::json!([{"bus_key":"clap.audio-input.33","channel_layout":"Mono"}]);
    let decoded = decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(decoded.main_audio_input(), ClapMainAudioInput::Absent);
    assert_eq!(
        decoded.auxiliary_inputs().collect::<Vec<_>>(),
        vec![("clap.audio-input.33", AudioChannelLayout::Mono)]
    );
    value["processing"]["auxiliary_buses"] = serde_json::Value::Array((0..32).map(|id| {
        serde_json::json!({"bus_key":format!("clap.audio-input.{id}"),"channel_layout":"Mono"})
    }).collect());
    let refused = decode(&serde_json::to_vec(&value).unwrap());
    assert!(refused.is_err());
    assert!(
        matches!(refused, Err(AudioProcessorHostError::InvalidContract(detail))
        if detail == "CLAP Note processing response changed its contract identity")
    );
}

fn named_offer() -> ClapNoteProcessingOfferV6 {
    let mut value = offer();
    let auxiliary = AudioProcessorAuxiliaryInputContract {
        buses: ["clap.audio-input.33", "clap.audio-input.34"]
            .into_iter()
            .map(|key| crate::AudioProcessorAuxiliaryInputBusContract {
                bus_key: key.to_owned(),
                channel_layout: AudioChannelLayout::Mono,
            })
            .collect(),
    };
    value.processing.auxiliary_buses = auxiliary_to_wire(&auxiliary);
    value.input_names = [
        ("clap.audio-input.33", "Detector"),
        ("clap.audio-input.34", "Monitor"),
    ]
    .into_iter()
    .map(|(key, name)| crate::AudioProcessorInputName {
        bus_key: key.to_owned(),
        name: name.to_owned(),
    })
    .collect();
    value
}

#[test]
fn v6_note_names_are_required_and_bound_to_the_same_auxiliary_contract() {
    let offered = named_offer();
    let expected = offered.input_names.clone();
    let bytes = encode_processing_offer_v6(&offered).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.input_names, expected);
    assert_eq!(decoded.notes.state_sha256, offered.notes.state_sha256);
    assert_eq!(decoded.notes.binary_sha256, offered.notes.binary_sha256);
    assert_eq!(
        decoded.auxiliary_inputs().collect::<Vec<_>>(),
        offered.auxiliary_inputs().collect::<Vec<_>>()
    );
    for names in [
        serde_json::Value::Null,
        serde_json::json!([expected[0], expected[0]]),
        serde_json::json!([{"bus_key":"absent", "name":"Detector"}]),
        serde_json::json!([{"bus_key":"clap.audio-input.33", "name":"\n"}]),
        serde_json::json!([{"bus_key":"clap.audio-input.33", "name":"x".repeat(1025)}]),
        serde_json::json!([{"bus_key":"clap.audio-input.33"}]),
    ] {
        let mut value = serde_json::to_value(named_offer()).unwrap();
        value["input_names"] = names;
        assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut missing = serde_json::to_value(named_offer()).unwrap();
    missing.as_object_mut().unwrap().remove("input_names");
    assert!(decode(&serde_json::to_vec(&missing).unwrap()).is_err());
    let text = String::from_utf8(bytes).unwrap();
    let duplicate = text.replacen("\"input_names\":", "\"input_names\":[],\"input_names\":", 1);
    assert!(decode(duplicate.as_bytes()).is_err());
    // A genuinely empty captured list may accompany unnamed auxiliary buses.
    let mut unnamed = named_offer();
    unnamed.input_names.clear();
    let decoded = decode(&encode_processing_offer_v6(&unnamed).unwrap()).unwrap();
    assert!(decoded.input_names.is_empty());
    assert_eq!(decoded.auxiliary_inputs().len(), 2);
}
