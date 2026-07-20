use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock, RwLock},
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
    pub disabled: bool,
    pub last_error: Option<String>,
    pub api_compatible: bool,
}

#[derive(Debug, Clone, Default)]
struct EffectPluginRuntimeState {
    disabled: bool,
    last_error: Option<String>,
}

fn plugin_contract_registry() -> &'static RwLock<HashMap<String, EffectPluginContract>> {
    static REGISTRY: OnceLock<RwLock<HashMap<String, EffectPluginContract>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

fn plugin_runtime_registry() -> &'static Mutex<HashMap<String, EffectPluginRuntimeState>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, EffectPluginRuntimeState>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_plugin_contract(key: impl Into<String>, contract: EffectPluginContract) {
    let key = key.into();
    plugin_contract_registry()
        .write()
        .expect("plugin contract registry poisoned")
        .insert(key.clone(), contract);
    plugin_runtime_registry()
        .lock()
        .expect("plugin runtime registry poisoned")
        .entry(key)
        .or_default();
}

pub fn plugin_contract(key: &str) -> Option<EffectPluginContract> {
    plugin_contract_registry()
        .read()
        .expect("plugin contract registry poisoned")
        .get(key)
        .cloned()
}

pub fn effect_plugin_runtime_status(key: &str) -> Option<EffectPluginRuntimeStatus> {
    let contract = plugin_contract(key)?;
    let state = plugin_runtime_registry()
        .lock()
        .expect("plugin runtime registry poisoned")
        .get(key)
        .cloned()
        .unwrap_or_default();
    Some(EffectPluginRuntimeStatus {
        disabled: state.disabled,
        last_error: state.last_error,
        api_compatible: contract.is_api_compatible(),
    })
}

pub fn effect_plugin_is_runtime_available(
    key: &str,
    contract: Option<&EffectPluginContract>,
) -> bool {
    let Some(contract) = contract else {
        return true;
    };
    if !contract.is_api_compatible() {
        return false;
    }
    !plugin_runtime_registry()
        .lock()
        .expect("plugin runtime registry poisoned")
        .get(key)
        .map(|state| state.disabled)
        .unwrap_or(false)
}

pub fn effect_plugin_is_library_visible(
    key: &str,
    contract: Option<&EffectPluginContract>,
) -> bool {
    let Some(contract) = contract else {
        return true;
    };
    if effect_plugin_is_runtime_available(key, Some(contract)) {
        return true;
    }
    contract.library_policy != EffectPluginLibraryPolicy::HideWhenUnavailable
}

pub fn record_plugin_runtime_failure(
    key: &str,
    contract: Option<&EffectPluginContract>,
    reason: impl Into<String>,
) {
    let Some(contract) = contract else {
        return;
    };
    let mut registry = plugin_runtime_registry().lock().expect("plugin runtime registry poisoned");
    let state = registry.entry(key.to_string()).or_default();
    state.last_error = Some(reason.into());
    if matches!(
        contract.runtime_failure_policy,
        EffectPluginRuntimeFailurePolicy::DisableDefinition
    ) {
        state.disabled = true;
    }
}

#[cfg(test)]
pub fn reset_plugin_runtime_state(key: &str) {
    plugin_runtime_registry()
        .lock()
        .expect("plugin runtime registry poisoned")
        .remove(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_plugin_can_hide_from_effect_library() {
        let key = "plugin.contract.hidden";
        register_plugin_contract(
            key,
            EffectPluginContract::new("1.2.3")
                .with_api_version(EffectPluginApiVersion::new(2, 0))
                .with_library_policy(EffectPluginLibraryPolicy::HideWhenUnavailable),
        );

        let contract = plugin_contract(key).expect("plugin contract");
        assert!(!effect_plugin_is_runtime_available(key, Some(&contract)));
        assert!(!effect_plugin_is_library_visible(key, Some(&contract)));
    }

    #[test]
    fn disable_definition_policy_marks_plugin_unavailable_after_error() {
        let key = "plugin.contract.disable_on_failure";
        reset_plugin_runtime_state(key);
        register_plugin_contract(
            key,
            EffectPluginContract::new("1.0.0")
                .with_runtime_failure_policy(EffectPluginRuntimeFailurePolicy::DisableDefinition),
        );

        let contract = plugin_contract(key).expect("plugin contract");
        assert!(effect_plugin_is_runtime_available(key, Some(&contract)));

        record_plugin_runtime_failure(key, Some(&contract), "processor panic");

        let status = effect_plugin_runtime_status(key).expect("runtime status");
        assert!(status.disabled);
        assert_eq!(status.last_error.as_deref(), Some("processor panic"));
        assert!(!effect_plugin_is_runtime_available(key, Some(&contract)));
    }
}
