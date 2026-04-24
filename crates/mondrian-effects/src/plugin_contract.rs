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
pub enum EffectPluginFailurePolicy {
    BypassEffect,
    DisablePluginDefinition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectPluginDegradationPolicy {
    IdentityFallback,
    HideFromEffectLibrary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectPluginContract {
    pub plugin_version: String,
    pub api_version: EffectPluginApiVersion,
    pub failure_policy: EffectPluginFailurePolicy,
    pub degradation_policy: EffectPluginDegradationPolicy,
}

impl EffectPluginContract {
    pub fn new(plugin_version: impl Into<String>) -> Self {
        Self {
            plugin_version: plugin_version.into(),
            api_version: CURRENT_EFFECT_PLUGIN_API_VERSION,
            failure_policy: EffectPluginFailurePolicy::BypassEffect,
            degradation_policy: EffectPluginDegradationPolicy::IdentityFallback,
        }
    }

    pub fn with_api_version(mut self, api_version: EffectPluginApiVersion) -> Self {
        self.api_version = api_version;
        self
    }

    pub fn with_failure_policy(mut self, failure_policy: EffectPluginFailurePolicy) -> Self {
        self.failure_policy = failure_policy;
        self
    }

    pub fn with_degradation_policy(
        mut self,
        degradation_policy: EffectPluginDegradationPolicy,
    ) -> Self {
        self.degradation_policy = degradation_policy;
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
    contract.degradation_policy != EffectPluginDegradationPolicy::HideFromEffectLibrary
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
        contract.failure_policy,
        EffectPluginFailurePolicy::DisablePluginDefinition
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
                .with_degradation_policy(EffectPluginDegradationPolicy::HideFromEffectLibrary),
        );

        let contract = plugin_contract(key).expect("plugin contract");
        assert!(!effect_plugin_is_runtime_available(key, Some(&contract)));
        assert!(!effect_plugin_is_library_visible(key, Some(&contract)));
    }

    #[test]
    fn disable_failure_policy_marks_plugin_unavailable_after_error() {
        let key = "plugin.contract.disable_on_failure";
        reset_plugin_runtime_state(key);
        register_plugin_contract(
            key,
            EffectPluginContract::new("1.0.0")
                .with_failure_policy(EffectPluginFailurePolicy::DisablePluginDefinition),
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
