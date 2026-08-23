use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectPluginApiVersion {
    pub major: u16,
    pub minor: u16,
}

impl EffectPluginApiVersion {
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    pub fn is_compatible_with(self, runtime: Self) -> bool {
        self.major == runtime.major && self.minor <= runtime.minor
    }
}

pub const CURRENT_EFFECT_PLUGIN_API_VERSION: EffectPluginApiVersion =
    EffectPluginApiVersion::new(1, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectPluginRuntimeFailurePolicy {
    /// Report the failed instance while keeping the definition available for repair/retry.
    KeepDefinitionAvailable,
    /// Disable the definition for the rest of the process after any runtime failure.
    DisableDefinition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectPluginLibraryPolicy {
    /// Keep an unavailable definition visible so persisted instances can be inspected or repaired.
    KeepVisible,
    /// Hide an unavailable definition from new-insertion UI.
    HideWhenUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectPluginContract {
    pub plugin_version: String,
    pub api_version: EffectPluginApiVersion,
    pub runtime_failure_policy: EffectPluginRuntimeFailurePolicy,
    pub library_policy: EffectPluginLibraryPolicy,
}

impl EffectPluginContract {
    pub fn new(plugin_version: impl Into<String>) -> Self {
        Self {
            plugin_version: plugin_version.into(),
            api_version: CURRENT_EFFECT_PLUGIN_API_VERSION,
            runtime_failure_policy: EffectPluginRuntimeFailurePolicy::DisableDefinition,
            library_policy: EffectPluginLibraryPolicy::HideWhenUnavailable,
        }
    }

    pub fn with_api_version(mut self, api_version: EffectPluginApiVersion) -> Self {
        self.api_version = api_version;
        self
    }

    pub fn with_runtime_failure_policy(
        mut self,
        runtime_failure_policy: EffectPluginRuntimeFailurePolicy,
    ) -> Self {
        self.runtime_failure_policy = runtime_failure_policy;
        self
    }

    pub fn with_library_policy(mut self, library_policy: EffectPluginLibraryPolicy) -> Self {
        self.library_policy = library_policy;
        self
    }

    pub fn is_api_compatible(&self) -> bool {
        self.api_version.is_compatible_with(CURRENT_EFFECT_PLUGIN_API_VERSION)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectPluginRuntimeStatus {
    /// Exact Definition-registry generation whose quarantine state is reported.
    pub definition_registry_revision: u64,
    pub disabled: bool,
    pub last_error: Option<String>,
    pub api_compatible: bool,
}

#[derive(Debug, Clone, Default)]
struct EffectPluginRuntimeState {
    disabled: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectPluginRuntimeKey {
    effect_key: String,
    definition_registry_revision: u64,
}

fn plugin_runtime_registry(
) -> &'static Mutex<HashMap<EffectPluginRuntimeKey, EffectPluginRuntimeState>> {
    static REGISTRY: OnceLock<Mutex<HashMap<EffectPluginRuntimeKey, EffectPluginRuntimeState>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn runtime_key(effect_key: &str, definition_registry_revision: u64) -> EffectPluginRuntimeKey {
    EffectPluginRuntimeKey {
        effect_key: effect_key.to_owned(),
        definition_registry_revision,
    }
}

fn lock_runtime_registry(
) -> std::sync::MutexGuard<'static, HashMap<EffectPluginRuntimeKey, EffectPluginRuntimeState>> {
    // A poisoned runtime registry must never panic the execution or admission
    // path: recover the map and continue with the last recorded quarantine
    // state rather than turning one plugin failure into a host panic.
    plugin_runtime_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runtime status of the currently registered Definition generation.
///
/// Replaced Definitions have independent quarantine state. A failure reported
/// by an older immutable Program therefore cannot disable the current
/// Definition that happens to reuse the same persistent effect key.
pub fn effect_plugin_runtime_status(key: &str) -> Option<EffectPluginRuntimeStatus> {
    let definition =
        crate::effect::effect_definition(&mondrian_core::effect_data::EffectType::from_key(key))?;
    let contract = definition.plugin_contract()?;
    Some(runtime_status(
        definition.key(),
        definition.definition_registry_revision(),
        contract,
    ))
}

fn runtime_status(
    key: &str,
    definition_registry_revision: u64,
    contract: &EffectPluginContract,
) -> EffectPluginRuntimeStatus {
    let state = lock_runtime_registry()
        .get(&runtime_key(key, definition_registry_revision))
        .cloned()
        .unwrap_or_default();
    EffectPluginRuntimeStatus {
        definition_registry_revision,
        disabled: state.disabled,
        last_error: state.last_error,
        api_compatible: contract.is_api_compatible(),
    }
}

pub(crate) fn effect_plugin_is_runtime_available(
    key: &str,
    definition_registry_revision: u64,
    contract: Option<&EffectPluginContract>,
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

pub(crate) fn effect_plugin_is_library_visible(
    key: &str,
    definition_registry_revision: u64,
    contract: Option<&EffectPluginContract>,
) -> bool {
    let Some(contract) = contract else {
        return true;
    };
    if effect_plugin_is_runtime_available(key, definition_registry_revision, Some(contract)) {
        return true;
    }
    contract.library_policy != EffectPluginLibraryPolicy::HideWhenUnavailable
}

pub(crate) fn record_plugin_runtime_failure(
    key: &str,
    definition_registry_revision: u64,
    contract: Option<&EffectPluginContract>,
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
        EffectPluginRuntimeFailurePolicy::DisableDefinition
    ) {
        state.disabled = true;
    }
}

#[cfg(test)]
fn reset_plugin_runtime_state(key: &str, definition_registry_revision: u64) {
    lock_runtime_registry().remove(&runtime_key(key, definition_registry_revision));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_plugin_can_hide_from_effect_library() {
        let key = "plugin.contract.hidden";
        let generation = 7;
        let contract = EffectPluginContract::new("1.2.3")
            .with_api_version(EffectPluginApiVersion::new(2, 0))
            .with_library_policy(EffectPluginLibraryPolicy::HideWhenUnavailable);
        assert!(!effect_plugin_is_runtime_available(
            key,
            generation,
            Some(&contract)
        ));
        assert!(!effect_plugin_is_library_visible(
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
        reset_plugin_runtime_state(key, failed_generation);
        reset_plugin_runtime_state(key, replacement_generation);
        let contract = EffectPluginContract::new("1.0.0")
            .with_runtime_failure_policy(EffectPluginRuntimeFailurePolicy::DisableDefinition);
        assert!(effect_plugin_is_runtime_available(
            key,
            failed_generation,
            Some(&contract)
        ));

        record_plugin_runtime_failure(key, failed_generation, Some(&contract), "processor panic");

        let failed = runtime_status(key, failed_generation, &contract);
        assert!(failed.disabled);
        assert_eq!(failed.last_error.as_deref(), Some("processor panic"));
        assert!(!effect_plugin_is_runtime_available(
            key,
            failed_generation,
            Some(&contract)
        ));
        assert!(
            effect_plugin_is_runtime_available(key, replacement_generation, Some(&contract)),
            "a failed immutable Program must not quarantine a replacement Definition"
        );
    }
}
