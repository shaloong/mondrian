//! Inactive CLAP input-note facts. These observations grant no event execution.

use super::clap_discovery::{invalid, unavailable};
use super::clap_worker::{ClapHost, ClapHostShared};
use crate::AudioProcessorHostError;
use clack_extensions::note_ports::PluginNotePorts;
use clack_extensions::state::PluginState;
use clack_extensions::thread_check::HostThreadCheckImpl;
use clack_host::prelude::{HostInfo, PluginEntry, PluginInstance, PluginMainThreadHandle};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::CString;

pub(super) const SCHEMA_VERSION: u32 = 1;
pub(super) const MAX_INPUTS: u32 = 32;
pub(super) const MAX_STATE_BYTES: usize = 64 * 1024;
const MAX_NAME_BYTES: usize = 255;
const KNOWN_DIALECTS: u32 = 1 | 2 | 4 | 8;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NoteInputMetadataRequest {
    pub(super) schema_version: u32,
    pub(super) plugin_id: String,
    pub(super) state: Option<Vec<u8>>,
}

/// Event processing remains unavailable even when a plugin declares ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClapNoteExecution {
    /// No input delivery or output consumer is implemented by this query.
    Unsupported,
}

/// Interpretation of a native preference, independent of host execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClapNotePreferredDialectStatus {
    /// Native preferred value is zero.
    Unspecified,
    /// A single known dialect is included in the native supported mask.
    KnownSupported,
    /// A single known dialect is missing from the native supported mask.
    KnownUnsupported,
    /// Multiple known dialect bits were supplied where one is required.
    InvalidKnownCombination,
    /// The original numeric preference contains an unknown bit.
    Unknown,
}

/// One native input descriptor. Index and display bytes are not identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClapNoteInputDescriptor {
    /// Current native ABI order, starting at zero.
    pub port_index: u32,
    /// Definition-local unsigned ID, including zero and high unsigned values.
    pub id: u32,
    /// Bytes before the first NUL in the fixed native name buffer.
    pub name_bytes: Vec<u8>,
    /// Unmodified native supported mask, including unknown future bits.
    pub supported_dialects: u32,
    /// Unmodified native preference, including invalid or future values.
    pub preferred_dialect: u32,
}

impl ClapNoteInputDescriptor {
    /// Classify the preference without discarding the original native values.
    pub fn preferred_dialect_status(&self) -> ClapNotePreferredDialectStatus {
        let value = self.preferred_dialect;
        if value == 0 {
            ClapNotePreferredDialectStatus::Unspecified
        } else if value & !KNOWN_DIALECTS != 0 {
            ClapNotePreferredDialectStatus::Unknown
        } else if !value.is_power_of_two() {
            ClapNotePreferredDialectStatus::InvalidKnownCombination
        } else if self.supported_dialects & value == value {
            ClapNotePreferredDialectStatus::KnownSupported
        } else {
            ClapNotePreferredDialectStatus::KnownUnsupported
        }
    }
}

/// Versioned facts from one inactive native instance, never author/DSP state.
///
/// Absence of this response is uninspected. `extension_present=false` is a
/// successful actual query; present with zero inputs is a separate observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClapNoteInputMetadata {
    /// This additive operation uses version 1; no absent-version default exists.
    pub schema_version: u32,
    /// Exact selected native definition ID.
    pub plugin_id: String,
    /// Binary fingerprint checked before and after isolated inspection.
    pub binary_sha256: [u8; 32],
    /// Digest of supplied opaque bytes; None means no state was supplied.
    pub state_sha256: Option<[u8; 32]>,
    /// Actual presence of PluginNotePorts, distinct from its input count.
    pub extension_present: bool,
    /// Bounded descriptors in native ABI order, with stable unsigned IDs.
    pub inputs: Vec<ClapNoteInputDescriptor>,
    /// The host does not advertise or implement note input dialects.
    pub host_supported_dialects: u32,
    /// Metadata does not admit input event delivery.
    pub input_execution: ClapNoteExecution,
    /// Output event consumption remains unsupported; outputs are not scanned.
    pub output_execution: ClapNoteExecution,
}

pub(super) fn state_digest(state: Option<&[u8]>) -> Option<[u8; 32]> {
    state.map(|bytes| Sha256::digest(bytes).into())
}

pub(super) fn validate_request(
    plugin_id: &str,
    state: Option<&[u8]>,
) -> Result<(), AudioProcessorHostError> {
    if plugin_id.is_empty() || plugin_id.len() > 256 || plugin_id.contains('\0') {
        return Err(invalid("CLAP note metadata plugin ID is invalid"));
    }
    if state.is_some_and(|bytes| bytes.len() > MAX_STATE_BYTES) {
        return Err(invalid("CLAP note metadata state exceeds the limit"));
    }
    Ok(())
}

pub(super) fn validate_response(
    metadata: &ClapNoteInputMetadata,
    plugin_id: &str,
    binary_sha256: [u8; 32],
    state_sha256: Option<[u8; 32]>,
) -> Result<(), AudioProcessorHostError> {
    if metadata.schema_version != SCHEMA_VERSION
        || metadata.plugin_id != plugin_id
        || metadata.binary_sha256 != binary_sha256
        || metadata.state_sha256 != state_sha256
    {
        return Err(invalid(
            "CLAP note metadata response changed version or identity",
        ));
    }
    if metadata.host_supported_dialects != 0
        || metadata.input_execution != ClapNoteExecution::Unsupported
        || metadata.output_execution != ClapNoteExecution::Unsupported
        || (!metadata.extension_present && !metadata.inputs.is_empty())
        || metadata.inputs.len() > MAX_INPUTS as usize
    {
        return Err(invalid(
            "CLAP note metadata response has unsupported capabilities",
        ));
    }
    let mut ids = BTreeSet::new();
    for (index, input) in metadata.inputs.iter().enumerate() {
        if input.port_index != index as u32
            || input.id == u32::MAX
            || !ids.insert(input.id)
            || input.name_bytes.len() > MAX_NAME_BYTES
            || input.name_bytes.contains(&0)
        {
            return Err(invalid("CLAP note metadata input descriptor is invalid"));
        }
    }
    Ok(())
}

fn inspect_inputs(
    handle: &PluginMainThreadHandle<'_>,
    shared: &ClapHostShared,
) -> Result<(bool, Vec<ClapNoteInputDescriptor>), AudioProcessorHostError> {
    let Some(extension) = handle.get_extension::<PluginNotePorts>() else {
        return Ok((false, Vec::new()));
    };
    let count = extension
        .count_raw(handle, true)
        .ok_or_else(|| invalid("CLAP note metadata count function is missing"))?;
    if shared.restart_requested() {
        return Err(unavailable("CLAP note metadata changed during count query"));
    }
    if count > MAX_INPUTS {
        return Err(invalid("CLAP note metadata input count exceeds the limit"));
    }
    if !extension.has_get(handle) {
        return Err(invalid("CLAP note metadata get function is missing"));
    }
    let mut inputs = Vec::with_capacity(count as usize);
    let mut ids = BTreeSet::new();
    for index in 0..count {
        let raw = extension
            .get_raw(handle, index, true)
            .ok_or_else(|| invalid("CLAP note metadata get function is missing or failed"))?;
        if shared.restart_requested() {
            return Err(unavailable("CLAP note metadata changed during port query"));
        }
        if raw.id == u32::MAX || !ids.insert(raw.id) {
            return Err(invalid(
                "CLAP note metadata input IDs are invalid or duplicated",
            ));
        }
        let end = raw
            .name
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("CLAP note metadata name is not terminated"))?;
        inputs.push(ClapNoteInputDescriptor {
            port_index: index,
            id: raw.id,
            name_bytes: raw.name[..end].iter().map(|byte| *byte as u8).collect(),
            supported_dialects: raw.supported_dialects,
            preferred_dialect: raw.preferred_dialect,
        });
    }
    Ok((true, inputs))
}

pub(super) fn inspect_loaded(
    entry: &PluginEntry,
    plugin_id: &str,
    binary_sha256: [u8; 32],
    state: Option<&[u8]>,
) -> Result<ClapNoteInputMetadata, AudioProcessorHostError> {
    validate_request(plugin_id, state)?;
    let plugin_c_id = CString::new(plugin_id)
        .map_err(|_| invalid("CLAP note metadata plugin ID contains NUL"))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    if factory
        .plugin_descriptors()
        .filter(|descriptor| descriptor.id() == Some(plugin_c_id.as_c_str()))
        .take(2)
        .count()
        != 1
    {
        return Err(unavailable(
            "CLAP note metadata plugin ID is missing or duplicated",
        ));
    }
    let info = HostInfo::new(
        "Mondrian",
        "Mondrian",
        "https://github.com/shaloong/mondrian",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| invalid(format!("invalid CLAP host identity: {error}")))?;
    let shared = ClapHostShared::new();
    let native_shared = shared.clone().with_native_logs()?;
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| native_shared,
        |shared| super::clap_main_services::ClapHostMainThread::new(shared),
        entry,
        plugin_c_id.as_c_str(),
        &info,
    )
    .map_err(|error| unavailable(format!("CLAP note metadata instance failed: {error}")))?;
    shared.service_callbacks(&mut instance)?;
    if shared.restart_requested() {
        return Err(unavailable(
            "CLAP note metadata requires reprepare after restart",
        ));
    }
    if let Some(bytes) = state {
        let handle = instance.plugin_handle();
        let extension = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| unavailable("CLAP plugin does not support state restoration"))?;
        let mut reader = bytes;
        extension.load(&handle, &mut reader).map_err(|error| {
            unavailable(format!("CLAP note metadata state restore failed: {error}"))
        })?;
        shared.service_callbacks(&mut instance)?;
    }
    if shared.restart_requested() {
        return Err(unavailable(
            "CLAP note metadata requires reprepare after state load",
        ));
    }
    if !shared.is_main_thread() || shared.is_audio_thread() {
        return Err(invalid(
            "CLAP note metadata requires the inactive main role",
        ));
    }
    // This instance has never been activated. Its main handle and host identity
    // stay on this worker's single main thread; no symbolic audio role is entered.
    let (extension_present, inputs) = inspect_inputs(&instance.plugin_handle(), &shared)?;
    shared.service_callbacks(&mut instance)?;
    if shared.restart_requested() {
        return Err(unavailable("CLAP note metadata changed during inspection"));
    }
    let metadata = ClapNoteInputMetadata {
        schema_version: SCHEMA_VERSION,
        plugin_id: plugin_id.to_owned(),
        binary_sha256,
        state_sha256: state_digest(state),
        extension_present,
        inputs,
        host_supported_dialects: 0,
        input_execution: ClapNoteExecution::Unsupported,
        output_execution: ClapNoteExecution::Unsupported,
    };
    validate_response(&metadata, plugin_id, binary_sha256, state_digest(state))?;
    // Drop destroys this inactive native instance before the worker publishes.
    drop(instance);
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(present: bool, ids: &[u32]) -> ClapNoteInputMetadata {
        ClapNoteInputMetadata {
            schema_version: 1,
            plugin_id: "fixture".to_owned(),
            binary_sha256: [7; 32],
            state_sha256: None,
            extension_present: present,
            inputs: ids
                .iter()
                .enumerate()
                .map(|(index, id)| ClapNoteInputDescriptor {
                    port_index: index as u32,
                    id: *id,
                    name_bytes: Vec::new(),
                    supported_dialects: 0x8000_0003,
                    preferred_dialect: 0x8000_0000,
                })
                .collect(),
            host_supported_dialects: 0,
            input_execution: ClapNoteExecution::Unsupported,
            output_execution: ClapNoteExecution::Unsupported,
        }
    }

    fn validate(value: &ClapNoteInputMetadata) -> Result<(), AudioProcessorHostError> {
        validate_response(value, "fixture", [7; 32], None)
    }

    #[test]
    fn note_metadata_absent_present_zero_and_unsigned_raw_facts_are_distinct() {
        let absent = result(false, &[]);
        let zero = result(true, &[]);
        assert_ne!(absent, zero);
        assert!(validate(&absent).is_ok());
        assert!(validate(&zero).is_ok());
        let value = result(true, &[u32::MAX - 1, 0]);
        assert!(validate(&value).is_ok());
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            serde_json::from_slice::<ClapNoteInputMetadata>(&bytes).unwrap(),
            value
        );
        assert_eq!(value.inputs[0].supported_dialects, 0x8000_0003);
        assert_eq!(
            value.inputs[0].preferred_dialect_status(),
            ClapNotePreferredDialectStatus::Unknown
        );
    }

    #[test]
    fn note_metadata_rejects_reserved_duplicate_index_and_absent_with_ports() {
        for value in [
            result(true, &[u32::MAX]),
            result(true, &[7, 7]),
            result(false, &[0]),
        ] {
            assert!(matches!(
                validate(&value),
                Err(AudioProcessorHostError::InvalidContract(_))
            ));
        }
        let mut reordered = result(true, &[9, 3]);
        reordered.inputs.swap(0, 1);
        assert!(validate(&reordered).is_err());
    }

    #[test]
    fn note_metadata_port_and_name_bounds_are_exact() {
        assert!(validate(&result(true, &(0..32).collect::<Vec<_>>())).is_ok());
        assert!(validate(&result(true, &(0..33).collect::<Vec<_>>())).is_err());
        let mut value = result(true, &[0]);
        value.inputs[0].name_bytes = vec![255; 255];
        assert!(validate(&value).is_ok());
        value.inputs[0].name_bytes.push(1);
        assert!(validate(&value).is_err());
        value.inputs[0].name_bytes = vec![b'a', 0, b'b'];
        assert!(validate(&value).is_err());
    }

    #[test]
    fn note_metadata_checks_all_identity_fields_and_never_advertises_input_support() {
        let value = result(true, &[0]);
        for mutate in 0..5 {
            let mut changed = value.clone();
            match mutate {
                0 => changed.schema_version = 2,
                1 => changed.plugin_id.push('x'),
                2 => changed.binary_sha256[0] ^= 1,
                3 => changed.state_sha256 = state_digest(Some(&[])),
                _ => changed.host_supported_dialects = 1,
            }
            assert!(validate(&changed).is_err());
        }
    }

    #[test]
    fn note_metadata_strict_versioned_decoder_refuses_legacy_missing_and_unknown_fields() {
        let value = result(true, &[]);
        let mut json = serde_json::to_value(value).unwrap();
        json.as_object_mut().unwrap().remove("schema_version");
        assert!(serde_json::from_value::<ClapNoteInputMetadata>(json).is_err());
        let mut json = serde_json::to_value(result(true, &[])).unwrap();
        json["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ClapNoteInputMetadata>(json).is_err());
        let mut json = serde_json::to_value(result(true, &[])).unwrap();
        json["input_execution"] = serde_json::json!("supported");
        assert!(serde_json::from_value::<ClapNoteInputMetadata>(json).is_err());
        assert!(serde_json::from_str::<ClapNoteInputMetadata>(
            r#"{"descriptors":[],"probe":null}"#
        )
        .is_err());
    }

    #[test]
    fn note_metadata_preferred_policy_preserves_each_original_numeric_value() {
        let mut input = result(true, &[0]).inputs.remove(0);
        input.supported_dialects = 1;
        for (raw, expected) in [
            (0, ClapNotePreferredDialectStatus::Unspecified),
            (1, ClapNotePreferredDialectStatus::KnownSupported),
            (2, ClapNotePreferredDialectStatus::KnownUnsupported),
            (3, ClapNotePreferredDialectStatus::InvalidKnownCombination),
            (0x8000_0000, ClapNotePreferredDialectStatus::Unknown),
        ] {
            input.preferred_dialect = raw;
            assert_eq!(input.preferred_dialect_status(), expected);
            assert_eq!(input.preferred_dialect, raw);
        }
    }

    #[test]
    fn note_metadata_state_none_empty_and_capacity_are_not_normalized() {
        assert_ne!(state_digest(None), state_digest(Some(&[])));
        assert_ne!(state_digest(Some(&[])), state_digest(Some(&[0])));
        assert!(validate_request("fixture", Some(&vec![0; MAX_STATE_BYTES])).is_ok());
        assert!(validate_request("fixture", Some(&vec![0; MAX_STATE_BYTES + 1])).is_err());
        for id in ["", "a\0b"] {
            assert!(validate_request(id, None).is_err());
        }
        assert!(validate_request(&"a".repeat(257), None).is_err());
    }
}
