//! Facts-only owned ABI gate; no note dispatch, instrument or PCM qualification.
use super::*;

const PREFIX: &str = "org.mondrian.fixture.note-metadata.";

#[test]
#[ignore = "fresh normal product helper and rebuilt original note-metadata fixture"]
fn installed_clap_input_note_metadata_facts_and_refusals() {
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(PathBuf::from)
        .expect("set current normal Mondrian helper");
    let plugin = std::env::var_os("MONDRIAN_CLAP_NOTE_METADATA_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("set fresh note-metadata fixture DLL");
    assert!(helper.is_absolute() && plugin.is_absolute());
    let fingerprint = super::super::clap_discovery::fingerprint_clap_binary(&plugin).unwrap();
    let descriptors = scan_clap_library_descriptors(&helper, &plugin).unwrap();
    assert_eq!(descriptors.len(), 16);
    assert_eq!(
        descriptors.iter().map(|row| row.plugin_id.clone()).collect::<Vec<_>>(),
        (0..16).map(|mode| format!("{PREFIX}{mode}")).collect::<Vec<_>>()
    );
    let id = format!("{PREFIX}0");
    for _ in 0..3 {
        let observed = probe_clap_note_input_metadata(&helper, &plugin, &id, None).unwrap();
        assert_eq!(observed.binary_sha256, fingerprint);
        assert_eq!(observed.state_sha256, None);
        assert!(observed.extension_present);
        assert_eq!(observed.host_supported_dialects, 0);
        assert_eq!(observed.input_execution, ClapNoteExecution::Unsupported);
        assert_eq!(observed.output_execution, ClapNoteExecution::Unsupported);
        assert_eq!(
            observed.inputs.iter().map(|port| port.id).collect::<Vec<_>>(),
            vec![u32::MAX - 1, 0]
        );
        assert_eq!(observed.inputs[0].supported_dialects, 0x8000_0003);
        assert_eq!(observed.inputs[0].preferred_dialect, 0x8000_0000);
        assert_eq!(
            observed.inputs[0].preferred_dialect_status(),
            ClapNotePreferredDialectStatus::Unknown
        );
        assert_eq!(
            observed.inputs[1].preferred_dialect_status(),
            ClapNotePreferredDialectStatus::KnownSupported
        );
    }
    let changed = probe_clap_note_input_metadata(&helper, &plugin, &id, Some(&[1])).unwrap();
    assert_eq!(
        changed.inputs.iter().map(|port| port.id).collect::<Vec<_>>(),
        vec![0, u32::MAX - 1]
    );
    assert_eq!(
        changed.state_sha256,
        super::super::clap_note_input_metadata::state_digest(Some(&[1]))
    );
    let empty = probe_clap_note_input_metadata(&helper, &plugin, &id, Some(&[])).unwrap();
    assert_eq!(
        empty.state_sha256,
        super::super::clap_note_input_metadata::state_digest(Some(&[]))
    );
    assert!(empty.state_sha256.is_some());
    let absent =
        probe_clap_note_input_metadata(&helper, &plugin, &format!("{PREFIX}1"), None).unwrap();
    assert!(!absent.extension_present && absent.inputs.is_empty());
    let zero =
        probe_clap_note_input_metadata(&helper, &plugin, &format!("{PREFIX}2"), None).unwrap();
    assert!(zero.extension_present && zero.inputs.is_empty());
    let partial =
        probe_clap_note_input_metadata(&helper, &plugin, &format!("{PREFIX}9"), None).unwrap();
    assert_eq!(partial.inputs[0].supported_dialects, 0x8000_0003);
    assert_eq!(partial.inputs[1].id, 0);
    assert_eq!(partial.inputs[1].supported_dialects, 0);
    assert_eq!(partial.inputs[1].preferred_dialect, 0);
    assert!(partial.inputs[1].name_bytes.is_empty());
    for mode in [3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15] {
        let error =
            probe_clap_note_input_metadata(&helper, &plugin, &format!("{PREFIX}{mode}"), None)
                .expect_err(
                    "malformed or pending-restart native metadata cannot appear empty/successful",
                );
        assert!(
            matches!(error, AudioProcessorHostError::Unavailable(_)),
            "mode {mode}: {error:?}"
        );
    }
    assert!(matches!(
        probe_clap_note_input_metadata(&helper, &plugin, &id, Some(&[2])),
        Err(AudioProcessorHostError::Unavailable(_))
    ));
    // This fixture deliberately has no audio ports. Metadata success must not
    // make the legacy effect probe admit it into the PCM execution path.
    assert!(
        probe_clap_plugin_registration(&helper, &plugin, &id, test_render_contract(), None)
            .is_err()
    );
    assert_eq!(
        scan_clap_library_descriptors(&helper, &plugin).unwrap(),
        descriptors
    );
    assert_eq!(
        super::super::clap_discovery::fingerprint_clap_binary(&plugin).unwrap(),
        fingerprint
    );
}
