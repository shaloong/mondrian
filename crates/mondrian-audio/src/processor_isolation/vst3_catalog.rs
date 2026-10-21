//! Installed VST3 catalog and exact authoring snapshots.

use super::vst3_discovery::{
    canonical_binary, probe_vst3_plugin_registration_with_state,
    probe_vst3_plugin_registration_with_state_and_settlement,
};
use super::vst3_worker::fingerprint;
use super::{
    scan_vst3_binary_descriptors, IsolatedAudioProcessorSpecResolver,
    IsolatedAudioProcessorWorkerSpec, Vst3AudioProcessorSpecResolver, Vst3PluginDescriptor,
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
        author_instance_from_probe(probe, state)
    }
}

fn author_instance_from_probe(
    probe: super::vst3_discovery::Vst3Probe,
    state: Option<Vec<u8>>,
) -> Result<AudioProcessorInstance, AudioProcessorHostError> {
    let opaque_state = state.or(probe.captured_state);
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
            .map_err(|error| invalid(format!("VST3 parameter UI metadata is invalid: {error}")))?;
        let value = descriptor.author_scalar_from_normalized(*current)?;
        if opaque_state.is_some() {
            // Native state owns the initial value until an explicit author edit.
            // from_schema supplies a default curve, so absence must be explicit.
            parameter.automation = None;
        } else {
            parameter
                .set_automation(
                    ExactAutomationCurve::new(id.clone(), value).map_err(|error| {
                        invalid(format!("VST3 current parameter value is invalid: {error}"))
                    })?,
                )
                .map_err(|error| {
                    invalid(format!("VST3 current parameter curve is invalid: {error}"))
                })?;
        }
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
        opaque_state: opaque_state.map(Into::into),
    })
}

impl IsolatedAudioProcessorSpecResolver for DiscoveredVst3AudioProcessorSpecResolver {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        mondrian_core::NativeChildSettlementRequirement::Required
    }

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
        let probe = probe_vst3_plugin_registration_with_state_and_settlement(
            &self.helper_executable,
            path,
            class_id,
            request.render_contract(),
            request.opaque_state(),
            false,
            request.native_child_settlement_registrar(),
        )?;
        if probe.registration.binary_sha256 != *expected_hash {
            return Err(unavailable("VST3 binary changed after discovery"));
        }
        Vst3AudioProcessorSpecResolver::new(self.helper_executable.clone(), [probe.registration])?
            .resolve(request)?
            .with_native_probe_input_names(probe.input_names)
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
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        mondrian_core::NativeChildSettlementRequirement::Required
    }

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

#[cfg(test)]
mod author_instance_contracts {
    use super::super::vst3_discovery::Vst3Probe;
    use super::super::vst3_processing_metadata::Vst3EventBusContractV1;
    use super::super::vst3_worker::{
        Vst3MainAudioInput, Vst3ParameterDescriptor, Vst3PluginRegistration,
    };
    use super::*;
    use crate::{AudioProcessorExecutionContract, AudioProcessorTail};
    use mondrian_core::ParameterId;

    fn probe(captured_state: Option<Vec<u8>>) -> Vst3Probe {
        let parameter =
            |id, name: &str, step_count, can_automate, read_only| Vst3ParameterDescriptor {
                id,
                name: name.to_owned(),
                default_normalized: 0.5,
                step_count,
                can_automate,
                read_only,
                flags: u32::from(can_automate) | (u32::from(read_only) << 1),
            };
        Vst3Probe {
            registration: Vst3PluginRegistration {
                class_id: "0123456789ABCDEF0123456789ABCDEF".to_owned(),
                vendor: Some("Test vendor".to_owned()),
                binary_path: std::env::current_exe().expect("test executable path"),
                binary_sha256: [3; 32],
                execution_contract: AudioProcessorExecutionContract::new(
                    0,
                    AudioProcessorTail::None,
                    true,
                    true,
                    true,
                    0,
                )
                .unwrap(),
                main_audio_input: Vst3MainAudioInput::Present,
                audio_output_count: 1,
                main_audio_output_count: 1,
                auxiliary_inputs: Default::default(),
                parameters: vec![
                    parameter(1, "Output Gain", 0, true, false),
                    parameter(2, "Nonautomatable stepped value", 4, false, false),
                    parameter(3, "Read only", 0, false, true),
                ],
                event_bus_contract: Vst3EventBusContractV1::empty(),
            },
            input_names: Vec::new(),
            current_values: vec![(1, 0.25), (2, 0.625), (3, 0.75)],
            captured_state,
        }
    }

    #[test]
    fn catalog_state_presence_preserves_full_schema_and_explicit_override_round_trip() {
        let gain = ParameterId::new("vst3.param.1").unwrap();
        let stepped = ParameterId::new("vst3.param.2").unwrap();
        for (supplied, captured) in [
            (Some(vec![9, 10]), Some(vec![7, 8])),
            (Some(Vec::new()), Some(vec![7, 8])),
            (None, Some(Vec::new())),
            (None, Some(vec![7, 8])),
            (None, None),
        ] {
            let expected_state = supplied.clone().or(captured.clone());
            let source = probe(captured);
            let expected_schemas = source.registration.parameters[..2]
                .iter()
                .map(|descriptor| descriptor.authoring_schema().unwrap())
                .collect::<Vec<_>>();
            let mut instance = author_instance_from_probe(source, supplied).unwrap();
            assert_eq!(instance.opaque_state.as_deref(), expected_state.as_deref());
            assert_eq!(instance.parameters.len(), 2, "readonly only is excluded");
            for (index, id) in [&gain, &stepped].into_iter().enumerate() {
                let parameter = &instance.parameters[id];
                assert_eq!(parameter.schema, expected_schemas[index]);
                assert_eq!(
                    parameter.ui.display_name.as_deref(),
                    Some(if index == 0 {
                        "Output Gain"
                    } else {
                        "Nonautomatable stepped value"
                    })
                );
                assert!(!parameter.ui.hidden);
                if expected_state.is_some() {
                    assert!(parameter.automation.is_none());
                } else {
                    assert_eq!(
                        parameter.automation.as_ref().unwrap().default_value,
                        if index == 0 { 0.25 } else { 3.0 },
                        "stateless fallback keeps actual current and stepped rounding"
                    );
                }
            }
            assert!(!instance.parameters[&stepped].schema.is_animatable);
            let reopened: AudioProcessorInstance =
                serde_json::from_slice(&serde_json::to_vec(&instance).unwrap()).unwrap();
            assert_eq!(
                reopened, instance,
                "None and state presence survive save/load"
            );
            instance
                .parameters
                .get_mut(&gain)
                .unwrap()
                .set_automation(ExactAutomationCurve::new(gain.clone(), 0.875).unwrap())
                .unwrap();
            let reopened: AudioProcessorInstance =
                serde_json::from_slice(&serde_json::to_vec(&instance).unwrap()).unwrap();
            assert_eq!(reopened, instance, "explicit Some survives save/load");
            assert_eq!(
                reopened.parameters[&gain].automation.as_ref().unwrap().default_value,
                0.875
            );
        }
    }

    #[test]
    fn catalog_native_state_does_not_bypass_current_value_or_schema_validation() {
        for state in [None, Some(Vec::new()), Some(vec![7, 8])] {
            for value in [f64::NAN, f64::INFINITY, -0.25, 1.25] {
                let mut source = probe(state.clone());
                source.current_values[0].1 = value;
                assert!(author_instance_from_probe(source, None).is_err());
            }
            let mut source = probe(state);
            source.registration.parameters[0].default_normalized = f64::NAN;
            assert!(author_instance_from_probe(source, None).is_err());
        }
    }
}
