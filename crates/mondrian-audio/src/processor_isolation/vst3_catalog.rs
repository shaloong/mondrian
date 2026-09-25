//! Installed VST3 catalog and exact authoring snapshots.

use super::vst3_discovery::{canonical_binary, probe_vst3_plugin_registration_with_state};
use super::vst3_worker::fingerprint;
use super::{
    probe_vst3_plugin_registration, scan_vst3_binary_descriptors,
    IsolatedAudioProcessorSpecResolver, IsolatedAudioProcessorWorkerSpec,
    Vst3AudioProcessorSpecResolver, Vst3PluginDescriptor,
};
use crate::{AudioProcessorHostError, AudioProcessorPrepareRequest, AudioRenderContract};
use mondrian_core::{AudioProcessorInstanceId, AuthoringMap, ExactAutomationCurve};
use mondrian_timeline::audio::{
    AudioProcessorDefinitionRef, AudioProcessorInstance, AudioProcessorParameter,
    AudioProcessorParameterUiMetadata,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

/// Immutable snapshot of explicitly selected VST3 binaries.
#[derive(Clone)]
pub struct DiscoveredVst3AudioProcessorSpecResolver {
    helper_executable: PathBuf,
    plugins: BTreeMap<String, (Vst3PluginDescriptor, PathBuf, [u8; 32])>,
}

impl DiscoveredVst3AudioProcessorSpecResolver {
    /// Scan each selected native binary in a supervised child.
    pub fn discover(
        helper_executable: PathBuf,
        binaries: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, AudioProcessorHostError> {
        if !helper_executable.is_absolute() {
            return Err(invalid("VST3 helper executable must be absolute"));
        }
        let mut plugins = BTreeMap::new();
        for binary in binaries {
            let (path, hash) = canonical_binary(&binary)?;
            let descriptors = scan_vst3_binary_descriptors(&helper_executable, &path)?;
            if fingerprint(&path)? != hash {
                return Err(unavailable("VST3 binary changed during discovery"));
            }
            for descriptor in descriptors {
                if plugins
                    .insert(
                        descriptor.class_id.clone(),
                        (descriptor, path.clone(), hash),
                    )
                    .is_some()
                {
                    return Err(invalid(
                        "duplicate installed VST3 class ID requires selection",
                    ));
                }
            }
        }
        Ok(Self { helper_executable, plugins })
    }

    /// Available class descriptors for an insertion picker.
    pub fn descriptors(&self) -> Vec<&Vst3PluginDescriptor> {
        self.plugins.values().map(|(descriptor, _, _)| descriptor).collect()
    }

    /// Capture an exact author instance from the selected class and state.
    pub fn create_instance(
        &self,
        class_id: &str,
        render: AudioRenderContract,
        state: Option<Vec<u8>>,
    ) -> Result<AudioProcessorInstance, AudioProcessorHostError> {
        let (_, path, expected_hash) = self
            .plugins
            .get(class_id)
            .ok_or_else(|| unavailable(format!("VST3 class {class_id} is not installed")))?;
        let probe = probe_vst3_plugin_registration_with_state(
            &self.helper_executable,
            path,
            class_id,
            render,
            state.as_deref(),
            state.is_none(),
        )?;
        if probe.registration.binary_sha256 != *expected_hash {
            return Err(unavailable("VST3 binary changed after discovery"));
        }
        let mut parameters = AuthoringMap::new();
        for (descriptor, (_, current)) in
            probe.registration.parameters.iter().zip(&probe.current_values)
        {
            if descriptor.read_only {
                continue;
            }
            let schema = descriptor.authoring_schema()?;
            let id = schema.parameter_id.clone();
            let mut parameter = AudioProcessorParameter::from_schema(schema)
                .map_err(|error| invalid(format!("VST3 parameter schema is invalid: {error}")))?;
            parameter = parameter
                .with_ui_metadata(AudioProcessorParameterUiMetadata {
                    display_name: Some(descriptor.name.clone()),
                    hidden: false,
                })
                .map_err(|error| {
                    invalid(format!("VST3 parameter UI metadata is invalid: {error}"))
                })?;
            let value = if descriptor.step_count > 0 {
                (current * f64::from(descriptor.step_count)).round()
            } else {
                *current
            };
            parameter
                .set_automation(
                    ExactAutomationCurve::new(id.clone(), value).map_err(|error| {
                        invalid(format!("VST3 current parameter value is invalid: {error}"))
                    })?,
                )
                .map_err(|error| {
                    invalid(format!("VST3 current parameter curve is invalid: {error}"))
                })?;
            parameters.insert(id, parameter);
        }
        Ok(AudioProcessorInstance {
            id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::Vst3 {
                class_id: probe.registration.class_id,
                vendor: probe.registration.vendor,
                schema_version: 1,
                binary_sha256: Some(probe.registration.binary_sha256),
            },
            bypassed: false,
            parameters,
            opaque_state: state.or(probe.captured_state).map(Into::into),
        })
    }
}

impl IsolatedAudioProcessorSpecResolver for DiscoveredVst3AudioProcessorSpecResolver {
    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
        let AudioProcessorDefinitionRef::Vst3 { class_id, .. } = request.definition() else {
            return Err(unavailable(
                "only VST3 definitions are handled by this catalog",
            ));
        };
        let (_, path, expected_hash) = self
            .plugins
            .get(class_id)
            .ok_or_else(|| unavailable(format!("VST3 class {class_id} is not installed")))?;
        let registration = probe_vst3_plugin_registration(
            &self.helper_executable,
            path,
            class_id,
            request.render_contract(),
            request.opaque_state(),
        )?;
        if registration.binary_sha256 != *expected_hash {
            return Err(unavailable("VST3 binary changed after discovery"));
        }
        Vst3AudioProcessorSpecResolver::new(self.helper_executable.clone(), [registration])?
            .resolve(request)
    }
}

/// Mutable selected-binary catalog shared by Preview and Export.
pub struct InstalledVst3AudioProcessorSpecResolver {
    catalog: RwLock<DiscoveredVst3AudioProcessorSpecResolver>,
}

impl InstalledVst3AudioProcessorSpecResolver {
    /// Create an empty catalog for one product executable.
    pub fn new(helper_executable: PathBuf) -> Result<Self, AudioProcessorHostError> {
        Ok(Self {
            catalog: RwLock::new(DiscoveredVst3AudioProcessorSpecResolver::discover(
                helper_executable,
                [],
            )?),
        })
    }

    /// Discover and atomically publish every effect class in a selected file or bundle.
    pub fn install_plugin(
        &self,
        binary: PathBuf,
    ) -> Result<Vec<Vst3PluginDescriptor>, AudioProcessorHostError> {
        let (selected, before) = canonical_binary(&binary)?;
        let helper = self
            .catalog
            .read()
            .map_err(|_| unavailable("VST3 catalog lock is poisoned"))?
            .helper_executable
            .clone();
        let discovered =
            DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [selected.clone()])?;
        if fingerprint(&selected)? != before {
            return Err(unavailable("VST3 binary changed during installation"));
        }
        let mut catalog =
            self.catalog.write().map_err(|_| unavailable("VST3 catalog lock is poisoned"))?;
        for id in discovered.plugins.keys() {
            if catalog.plugins.get(id).is_some_and(|(_, path, _)| path != &selected) {
                return Err(invalid(format!(
                    "VST3 class {id} is already supplied by another binary"
                )));
            }
        }
        catalog.plugins.retain(|_, (_, path, _)| path != &selected);
        let descriptors = discovered
            .plugins
            .values()
            .map(|(descriptor, _, _)| descriptor.clone())
            .collect();
        catalog.plugins.extend(discovered.plugins);
        Ok(descriptors)
    }

    /// Current installed effect classes for an insertion picker.
    pub fn descriptors(&self) -> Result<Vec<Vst3PluginDescriptor>, AudioProcessorHostError> {
        Ok(self
            .catalog
            .read()
            .map_err(|_| unavailable("VST3 catalog lock is poisoned"))?
            .plugins
            .values()
            .map(|(descriptor, _, _)| descriptor.clone())
            .collect())
    }

    /// Capture an authoring instance from a selected installed class.
    pub fn create_instance(
        &self,
        class_id: &str,
        render: AudioRenderContract,
        state: Option<Vec<u8>>,
    ) -> Result<AudioProcessorInstance, AudioProcessorHostError> {
        self.catalog
            .read()
            .map_err(|_| unavailable("VST3 catalog lock is poisoned"))?
            .clone()
            .create_instance(class_id, render, state)
    }
}

impl IsolatedAudioProcessorSpecResolver for InstalledVst3AudioProcessorSpecResolver {
    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
        self.catalog
            .read()
            .map_err(|_| unavailable("VST3 catalog lock is poisoned"))?
            .clone()
            .resolve(request)
    }
}

fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}

fn unavailable(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::Unavailable(detail.into())
}
