//! Version and failure/visibility contracts for registered effect definitions.
//! Effect Extension contracts do not authorize host permissions or establish
//! runtime isolation.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Version of the admitted Effect Extension contract.
pub struct EffectExtensionApiVersion {
    /// Incompatible contract generation.
    pub major: u16,
    /// Compatible feature level required by the definition.
    pub minor: u16,
}

impl EffectExtensionApiVersion {
    /// Construct a contract version from its major and minor components.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Whether the runtime has the same major and a sufficient minor version.
    pub fn is_compatible_with(self, runtime: Self) -> bool {
        self.major == runtime.major && self.minor <= runtime.minor
    }
}

/// Effect Extension contract supported by this runtime.
pub const CURRENT_EFFECT_EXTENSION_API_VERSION: EffectExtensionApiVersion =
    EffectExtensionApiVersion::new(1, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Availability policy after a registered definition fails execution.
pub enum EffectExtensionRuntimeFailurePolicy {
    /// Report the failed instance while keeping the definition available for repair/retry.
    KeepDefinitionAvailable,
    /// Disable this registered definition generation after any runtime failure.
    /// A replacement generation has independent quarantine state.
    DisableDefinition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Visibility policy for new effect insertion, independent of execution errors.
pub enum EffectExtensionLibraryPolicy {
    /// Keep an unavailable definition visible so persisted instances can be inspected or repaired.
    KeepVisible,
    /// Hide an unavailable definition from new-insertion UI.
    HideWhenUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Version, failure and visibility contract of one effect definition.
pub struct EffectExtensionContract {
    /// Version of the effect implementation, independent of package identity.
    pub extension_version: String,
    /// Required effect contract version.
    pub api_version: EffectExtensionApiVersion,
    /// Definition-generation availability after execution failure.
    pub runtime_failure_policy: EffectExtensionRuntimeFailurePolicy,
    /// Whether unavailable definitions remain visible for new insertion.
    pub library_policy: EffectExtensionLibraryPolicy,
}

impl EffectExtensionContract {
    /// Construct a contract with the current API and conservative failure policy.
    pub fn new(extension_version: impl Into<String>) -> Self {
        Self {
            extension_version: extension_version.into(),
            api_version: CURRENT_EFFECT_EXTENSION_API_VERSION,
            runtime_failure_policy: EffectExtensionRuntimeFailurePolicy::DisableDefinition,
            library_policy: EffectExtensionLibraryPolicy::HideWhenUnavailable,
        }
    }

    /// Set the effect contract version required by this definition.
    pub fn with_api_version(mut self, api_version: EffectExtensionApiVersion) -> Self {
        self.api_version = api_version;
        self
    }

    /// Set availability behavior after execution failure.
    pub fn with_runtime_failure_policy(
        mut self,
        runtime_failure_policy: EffectExtensionRuntimeFailurePolicy,
    ) -> Self {
        self.runtime_failure_policy = runtime_failure_policy;
        self
    }

    /// Set visibility behavior for new insertion.
    pub fn with_library_policy(mut self, library_policy: EffectExtensionLibraryPolicy) -> Self {
        self.library_policy = library_policy;
        self
    }

    /// Whether the current runtime admits the required effect contract version.
    pub fn is_api_compatible(&self) -> bool {
        self.api_version.is_compatible_with(CURRENT_EFFECT_EXTENSION_API_VERSION)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Current availability of one registered effect definition generation.
pub struct EffectExtensionRuntimeStatus {
    /// Exact Definition-registry generation whose quarantine state is reported.
    pub definition_registry_revision: u64,
    /// Whether this definition generation has been quarantined.
    pub disabled: bool,
    /// Most recent recorded execution failure for this generation.
    pub last_error: Option<String>,
    /// Whether its effect contract version is supported.
    pub api_compatible: bool,
}

#[derive(Debug, Clone, Default)]
struct EffectExtensionRuntimeState {
    disabled: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectExtensionRuntimeKey {
    effect_key: String,
    definition_registry_revision: u64,
}

fn extension_runtime_registry(
) -> &'static Mutex<HashMap<EffectExtensionRuntimeKey, EffectExtensionRuntimeState>> {
    static REGISTRY: OnceLock<
        Mutex<HashMap<EffectExtensionRuntimeKey, EffectExtensionRuntimeState>>,
    > = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn runtime_key(effect_key: &str, definition_registry_revision: u64) -> EffectExtensionRuntimeKey {
    EffectExtensionRuntimeKey {
        effect_key: effect_key.to_owned(),
        definition_registry_revision,
    }
}

fn lock_runtime_registry(
) -> std::sync::MutexGuard<'static, HashMap<EffectExtensionRuntimeKey, EffectExtensionRuntimeState>>
{
    // A poisoned runtime registry must never panic the execution or admission
    // path: recover the map and continue with the last recorded quarantine
    // state rather than turning one extension failure into a host panic.
    extension_runtime_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runtime status of the currently registered Definition generation.
///
/// Replaced Definitions have independent quarantine state. A failure reported
/// by an older immutable Program therefore cannot disable the current
/// Definition that happens to reuse the same persistent effect key.
pub fn effect_extension_runtime_status(key: &str) -> Option<EffectExtensionRuntimeStatus> {
    let definition =
        crate::effect::effect_definition(&mondrian_core::effect_data::EffectType::from_key(key))?;
    let contract = definition.extension_contract()?;
    Some(runtime_status(
        definition.key(),
        definition.definition_registry_revision(),
        contract,
    ))
}

fn runtime_status(
    key: &str,
    definition_registry_revision: u64,
    contract: &EffectExtensionContract,
) -> EffectExtensionRuntimeStatus {
    let state = lock_runtime_registry()
        .get(&runtime_key(key, definition_registry_revision))
        .cloned()
        .unwrap_or_default();
    EffectExtensionRuntimeStatus {
        definition_registry_revision,
        disabled: state.disabled,
        last_error: state.last_error,
        api_compatible: contract.is_api_compatible(),
    }
}

pub(crate) fn effect_extension_is_runtime_available(
    key: &str,
    definition_registry_revision: u64,
    contract: Option<&EffectExtensionContract>,
) -> bool {
    let Some(contract) = contract else {
        return true;
    };
    if !contract.is_api_compatible() {
        return false;
    }
    !lock_runtime_registry()
        .get(&runtime_key(key, definition_registry_revision))
        .map(|state| state.disabled)
        .unwrap_or(false)
}

pub(crate) fn effect_extension_is_library_visible(
    key: &str,
    definition_registry_revision: u64,
    contract: Option<&EffectExtensionContract>,
) -> bool {
    let Some(contract) = contract else {
        return true;
    };
    if effect_extension_is_runtime_available(key, definition_registry_revision, Some(contract)) {
        return true;
    }
    contract.library_policy != EffectExtensionLibraryPolicy::HideWhenUnavailable
}

pub(crate) fn record_extension_runtime_failure(
    key: &str,
    definition_registry_revision: u64,
    contract: Option<&EffectExtensionContract>,
    reason: impl Into<String>,
) {
    let Some(contract) = contract else {
        return;
    };
    let mut registry = lock_runtime_registry();
    let state = registry.entry(runtime_key(key, definition_registry_revision)).or_default();
    state.last_error = Some(reason.into());
    if matches!(
        contract.runtime_failure_policy,
        EffectExtensionRuntimeFailurePolicy::DisableDefinition
    ) {
        state.disabled = true;
    }
}

#[cfg(test)]
fn reset_extension_runtime_state(key: &str, definition_registry_revision: u64) {
    lock_runtime_registry().remove(&runtime_key(key, definition_registry_revision));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_extension_can_hide_from_effect_library() {
        let key = "plugin.contract.hidden";
        let generation = 7;
        let contract = EffectExtensionContract::new("1.2.3")
            .with_api_version(EffectExtensionApiVersion::new(2, 0))
            .with_library_policy(EffectExtensionLibraryPolicy::HideWhenUnavailable);
        assert!(!effect_extension_is_runtime_available(
            key,
            generation,
            Some(&contract)
        ));
        assert!(!effect_extension_is_library_visible(
            key,
            generation,
            Some(&contract)
        ));
    }

    #[test]
    fn quarantine_is_scoped_to_the_frozen_definition_generation() {
        let key = "plugin.contract.disable_on_failure";
        let failed_generation = 11;
        let replacement_generation = 12;
        reset_extension_runtime_state(key, failed_generation);
        reset_extension_runtime_state(key, replacement_generation);
        let contract = EffectExtensionContract::new("1.0.0")
            .with_runtime_failure_policy(EffectExtensionRuntimeFailurePolicy::DisableDefinition);
        assert!(effect_extension_is_runtime_available(
            key,
            failed_generation,
            Some(&contract)
        ));

        record_extension_runtime_failure(
            key,
            failed_generation,
            Some(&contract),
            "processor panic",
        );

        let failed = runtime_status(key, failed_generation, &contract);
        assert!(failed.disabled);
        assert_eq!(failed.last_error.as_deref(), Some("processor panic"));
        assert!(!effect_extension_is_runtime_available(
            key,
            failed_generation,
            Some(&contract)
        ));
        assert!(
            effect_extension_is_runtime_available(key, replacement_generation, Some(&contract)),
            "a failed immutable Program must not quarantine a replacement Definition"
        );
    }
}
